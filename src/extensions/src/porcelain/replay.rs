//! `git replay` — replay a range of commits onto a new base, touching neither
//! the index nor the working tree.
//!
//! This is a port of git's libified `replay_revisions()` (`replay.c`) plus the
//! `cmd_replay()` driver (`builtin/replay.c`). The structure below follows the C
//! function-for-function: option parsing, `get_ref_information`,
//! `set_up_replay_mode`, the topological walk, `pick_regular_commit` and the
//! final reference transaction.
//!
//! ## Covered
//!
//! * `--onto=<newbase>`, including multi-branch replay: every `refs/heads/*`
//!   that decorates a replayed commit and was named as a positive revision is
//!   updated, in git's decoration order (reverse ref-name order, `HEAD` first).
//! * `--contained` — drop the "was named on the command line" filter.
//! * `--advance=<branch>` and `--revert=<branch>`, including git's revert
//!   message conventions (`Revert "<subject>"` / `Reapply "<subject>"` plus the
//!   `This reverts commit <hash>.` line) and its use of the *current* user as
//!   the author of revert commits.
//! * `--ref=<ref>` with its `refs/`-prefix and refname-format validation.
//! * `--ref-action=update|print` and the `replay.refAction` configuration
//!   variable. `print` emits `update <ref> <new> <old>` lines; `update` runs one
//!   atomic reference transaction and prints nothing.
//! * Revision ranges in the forms `<rev>`, `^<rev>` and `<a>..<b>`, walked
//!   `--topo-order` (reversed for pick mode, newest-first for revert mode) via
//!   `gix_traverse::commit::topo`, which is a port of git's
//!   `sort_in_topological_order`.
//! * Commits that become empty are dropped, matching the CLI-reachable default
//!   (`REPLAY_EMPTY_COMMIT_DROP`; git exposes no flag to change it).
//! * Exit codes: 0 clean, 1 conflicted (with no output and no ref updates, as
//!   git documents), 128 for `die`/`error` paths, 129 for the usage error.
//! * `pick_regular_commit` is `pub(super)` because `git history` replays its
//!   descendants through the same libified function git does.
//!
//! ## Not covered
//!
//! * `<a>...<b>` symmetric difference, `--` pathspec limiting, and every
//!   rev-list commit-limiting option (`-n`, `--grep`, `--since`, `--merges`, …).
//!   git passes those through `setup_revisions`; here they are refused rather
//!   than silently ignored, because ignoring them would change which commits get
//!   replayed.
//! * Replaying merge commits — git refuses these too.
//!
//! ## Known divergences
//!
//! * The author/committer headers are re-serialized from gitoxide's parsed
//!   signature rather than copied verbatim, so a commit carrying a signature
//!   git itself would not round-trip identically produces a different object id.
//! * `i18n.commitEncoding` is not consulted; new commits never carry an
//!   `encoding` header. Extra headers are otherwise preserved verbatim, minus
//!   `gpgsig`/`gpgsig-sha256`, exactly as git's `read_commit_extra_headers` does.
//! * Unresolvable revision arguments report through this port's error channel
//!   instead of git's `ambiguous argument` wording.

use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::merge::blob::builtin_driver::text::Labels;
use gix::merge::tree::TreatAsUnresolved;
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// Verbatim `git replay` usage text, printed to stderr when no mode is given.
const USAGE: &str = "\
usage: (EXPERIMENTAL!) git replay ([--contained] --onto=<newbase> | --advance=<branch> | --revert=<branch>)
       [--ref=<ref>] [--ref-action=<mode>] <revision-range>

    --[no-]contained      update all branches that point at commits in <revision-range>
    --onto <revision>     replay onto given commit
    --advance <branch>    make replay advance given branch
    --revert <branch>     revert commits onto given branch
    --ref <branch>        reference to update with result
    --ref-action <mode>   control ref update behavior (update|print)

";

/// git's `ref_rev_parse_rules`, used by `repo_dwim_ref` to decide whether a
/// revision expression names exactly one reference.
const REV_PARSE_RULES: [&str; 6] = [
    "{}",
    "refs/{}",
    "refs/tags/{}",
    "refs/heads/{}",
    "refs/remotes/{}",
    "refs/remotes/{}/HEAD",
];

/// `enum replay_mode`.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Mode {
    Pick,
    Revert,
}

/// `enum replay_empty_commit_action` (replay.h).
#[derive(Clone, Copy, PartialEq)]
pub(super) enum EmptyAction {
    Drop,
    Keep,
    Abort,
}

/// What [`pick_regular_commit`] produced for one commit.
pub(super) enum Picked {
    /// The replayed commit — or, when an empty commit was dropped, the base it
    /// was replayed onto, which is what git returns in that case.
    Commit(ObjectId),
    /// The three-way merge left unresolved conflicts (`!result->clean`).
    Conflict,
    /// `--empty=abort` and the commit became empty; git sets
    /// `result->clean = error("commit %s became empty after replay")`, which the
    /// caller turns into a -1 return.
    BecameEmpty(ObjectId),
}

/// git's `pick_regular_commit` (replay.c): three-way merge `pickme` onto the
/// already-replayed version of its parent, then commit the result.
///
/// `fallback` is `mapped_commit`'s default when `pickme`'s parent has not been
/// replayed — `onto` in pick mode, the previous result in revert mode.
pub(super) fn pick_regular_commit(
    repo: &gix::Repository,
    pickme: ObjectId,
    replayed: &HashMap<ObjectId, ObjectId>,
    fallback: ObjectId,
    merge_options: &gix::merge::tree::Options,
    mode: Mode,
    empty: EmptyAction,
) -> Result<Picked> {
    let commit = repo.find_commit(pickme)?;
    let base = commit.parent_ids().next().map(|id| id.detach());
    let base_tree = match base {
        Some(b) => repo.find_commit(b)?.tree_id()?.detach(),
        None => repo.object_hash().empty_tree(),
    };

    let replayed_base = base
        .and_then(|b| replayed.get(&b).copied())
        .unwrap_or(fallback);
    let replayed_base_tree = repo.find_commit(replayed_base)?.tree_id()?.detach();
    let pickme_tree = commit.tree_id()?.detach();

    // Labels only surface inside conflict markers, which a conflicted replay
    // never writes; they are set for parity with `pick_regular_commit`.
    let ours_label = replayed_base.attach(repo).shorten_or_id().to_string();
    let pickme_label = pickme.attach(repo).shorten_or_id().to_string();
    let parent_label = format!("parent of {pickme_label}");
    let (ancestor_tree, our_tree, their_tree, labels) = match mode {
        Mode::Pick => (
            base_tree,
            replayed_base_tree,
            pickme_tree,
            Labels {
                ancestor: Some(if base.is_some() {
                    BStr::new(parent_label.as_str())
                } else {
                    BStr::new("empty tree")
                }),
                current: Some(BStr::new(ours_label.as_str())),
                other: Some(BStr::new(pickme_label.as_str())),
            },
        ),
        Mode::Revert => (
            pickme_tree,
            replayed_base_tree,
            base_tree,
            Labels {
                ancestor: Some(BStr::new(pickme_label.as_str())),
                current: Some(BStr::new(ours_label.as_str())),
                other: Some(BStr::new(parent_label.as_str())),
            },
        ),
    };

    let mut outcome =
        repo.merge_trees(ancestor_tree, our_tree, their_tree, labels, merge_options.clone())?;
    // `merge_incore_nonrecursive()` produces `result->tree` whether or not the
    // merge came out clean — `pick_regular_commit` only inspects `result->clean`
    // afterwards — so the tree objects land in the odb either way. Writing after
    // the conflict check instead left a conflicted replay with the marked blob in
    // the store but none of the trees pointing at it.
    let tree_id = outcome.tree.write()?.detach();
    if outcome.has_unresolved_conflicts(TreatAsUnresolved::git()) {
        return Ok(Picked::Conflict);
    }

    if tree_id == replayed_base_tree && pickme_tree != base_tree {
        match empty {
            EmptyAction::Drop => return Ok(Picked::Commit(replayed_base)),
            EmptyAction::Keep => {}
            EmptyAction::Abort => return Ok(Picked::BecameEmpty(pickme)),
        }
    }

    Ok(Picked::Commit(create_commit(
        repo,
        &commit,
        tree_id,
        replayed_base,
        mode,
    )?))
}

/// `enum ref_action_mode`.
#[derive(Clone, Copy, PartialEq)]
enum RefAction {
    Update,
    Print,
}

/// One entry of git's `rev_cmdline_info`: the expression as written (with any
/// leading `^` stripped), whether it is a `BOTTOM` (negative) tip, and the
/// commit it resolves to.
struct RevArg {
    name: String,
    negative: bool,
    oid: ObjectId,
}

/// One queued `struct replay_ref_update`.
struct Update {
    refname: String,
    old: ObjectId,
    new: ObjectId,
}

/// `git replay ([--contained] --onto=<newbase> | --advance=<branch> | --revert=<branch>) [--ref=<ref>] [--ref-action=<mode>] <revision-range>`.
pub fn replay(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand being present at index 0 so both calling
    // conventions behave the same.
    let args = match args.first() {
        Some(a) if a == "replay" => &args[1..],
        _ => args,
    };

    // --- parse_options ---------------------------------------------------
    let mut contained = false;
    let mut onto_name: Option<String> = None;
    let mut advance_name: Option<String> = None;
    let mut revert_name: Option<String> = None;
    let mut ref_name: Option<String> = None;
    let mut ref_action: Option<String> = None;
    let mut rev_exprs: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (a, None),
        };
        let inline = inline.as_deref();
        match name {
            "--contained" => contained = true,
            "--no-contained" => contained = false,
            "--onto" => onto_name = Some(value_of(args, &mut i, inline, name)?),
            "--advance" => advance_name = Some(value_of(args, &mut i, inline, name)?),
            "--revert" => revert_name = Some(value_of(args, &mut i, inline, name)?),
            "--ref" => ref_name = Some(value_of(args, &mut i, inline, name)?),
            "--ref-action" => ref_action = Some(value_of(args, &mut i, inline, name)?),
            // parse_options_step()'s `internal_help`: the block on stdout at
            // 129, ahead of every unported-flag refusal below.
            // `--help-all` reaches the same renderer with USAGE_FULL, which this
            // table renders identically: it has no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help-all" => return Ok(super::show_usage(USAGE)),
            "--" => bail!(
                "unsupported flag \"--\" (pathspec-limited replay is not ported; \
                 ported: --contained, --onto, --advance, --revert, --ref, --ref-action)"
            ),
            s if s.starts_with('-') && s.len() > 1 => bail!(
                "unsupported flag {s:?} (rev-list commit-limiting options are not ported; \
                 ported: --contained, --onto, --advance, --revert, --ref, --ref-action)"
            ),
            s => rev_exprs.push(s.to_string()),
        }
        i += 1;
    }

    // --- mode validation, in git's order ---------------------------------
    if onto_name.is_none() && advance_name.is_none() && revert_name.is_none() {
        eprintln!("error: exactly one of --onto, --advance, or --revert is required");
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
    let set = [
        (onto_name.is_some(), "--onto"),
        (advance_name.is_some(), "--advance"),
        (revert_name.is_some(), "--revert"),
    ];
    let chosen: Vec<&str> = set.iter().filter(|(on, _)| *on).map(|(_, n)| *n).collect();
    if chosen.len() > 1 {
        return fatal(&format!(
            "options '{}' and '{}' cannot be used together",
            chosen[0], chosen[1]
        ));
    }
    for (flag, label) in [
        (advance_name.is_some(), "--advance"),
        (revert_name.is_some(), "--revert"),
        (ref_name.is_some(), "--ref"),
    ] {
        if flag && contained {
            return fatal(&format!(
                "options '{label}' and '--contained' cannot be used together"
            ));
        }
    }

    let repo = gix::discover(".")?;

    // --- get_ref_action_mode ---------------------------------------------
    let configured = repo
        .config_snapshot()
        .string("replay.refAction")
        .map(|v| v.to_str_lossy().into_owned());
    let ref_mode = match (&ref_action, &configured) {
        (Some(v), _) => match parse_ref_action(v) {
            Some(m) => m,
            None => return fatal(&format!("invalid --ref-action value: '{v}'")),
        },
        (None, Some(v)) => match parse_ref_action(v) {
            Some(m) => m,
            None => return fatal(&format!("invalid replay.refAction value: '{v}'")),
        },
        (None, None) => RefAction::Update,
    };

    let mode = if revert_name.is_some() {
        Mode::Revert
    } else {
        Mode::Pick
    };

    // The replay writes objects and (in update mode) references; hold the
    // coordinator lock across the whole read-modify-write, like `cherry-pick`.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // --- setup_revisions --------------------------------------------------
    let mut revs: Vec<RevArg> = Vec::new();
    for expr in &rev_exprs {
        // Every rejection here is `setup_revisions()`'s, not this port's, so the
        // whole operand goes to the shared diagnosis rather than the endpoint
        // that failed: an absent full-length hex resolves (see
        // [`crate::objname`]) and dies as `bad object <hex>` on its own or as
        // `Invalid revision range <a>..<b>` inside a range, while a name that
        // resolves to nothing keeps the `ambiguous argument` advice. Reporting
        // the gitoxide error instead leaked this port's own source paths.
        let arg = |name: &str, negative: bool| -> Result<RevArg> {
            Ok(RevArg {
                name: name.to_string(),
                negative,
                oid: peel_to_commit(&repo, name)?,
            })
        };
        // `handle_dotdot()` is the first thing `handle_revision_arg_1()` tries,
        // so a range git rejects dies here — ahead of the symmetric-difference
        // refusal below, which is this port's gap and not git's answer. Without
        // this, `git replay --onto HEAD <tag-of-a-tree>...HEAD` reported the gap
        // where git reports `error: object … is a tree, not a commit` and
        // `fatal: Invalid symmetric difference expression …` at 128.
        match crate::objname::dotdot(&repo, expr) {
            crate::objname::Dotdot::Missing { .. } => {
                eprint!("{}", super::log::bad_revision_message_in(&repo, expr));
                return Ok(ExitCode::from(128));
            }
            // A range whose endpoints are all good is the one shape that really
            // is unported; anything `handle_dotdot()` did not accept falls
            // through to the per-endpoint resolution below, which reports it the
            // way `setup_revisions()` does.
            crate::objname::Dotdot::Ok { .. }
                if crate::objname::split_range(expr).is_some_and(|r| r.symmetric) =>
            {
                bail!(
                    "unsupported revision range {expr:?} \
                     (symmetric difference `...` is not ported)"
                )
            }
            _ => {}
        }
        let resolved = if let Some((left, right)) = expr.split_once("..") {
            // git substitutes `HEAD` for an omitted side of the range.
            let left = if left.is_empty() { "HEAD" } else { left };
            let right = if right.is_empty() { "HEAD" } else { right };
            arg(left, true).and_then(|a| Ok(vec![a, arg(right, false)?]))
        } else if let Some(bare) = expr.strip_prefix('^') {
            arg(bare, true).map(|a| vec![a])
        } else {
            arg(expr, false).map(|a| vec![a])
        };
        match resolved {
            Ok(args) => revs.extend(args),
            Err(_) => {
                eprint!("{}", super::log::bad_revision_message_in(&repo, expr));
                return Ok(ExitCode::from(128));
            }
        }
    }

    // --- get_ref_information ---------------------------------------------
    let mut positive_refexprs = 0usize;
    let mut positive_refs: Vec<String> = Vec::new();
    for e in &revs {
        if e.negative {
            continue;
        }
        positive_refexprs += 1;
        if let Some(full) = dwim_ref(&repo, &e.name) {
            if !positive_refs.contains(&full) {
                positive_refs.push(full);
            }
        }
    }

    // --- set_up_replay_mode ------------------------------------------------
    let detached_head = repo.head()?.is_detached();
    if positive_refexprs == 0 {
        return fatal("need some commits to replay");
    }

    // `update_refs` exists only in --onto mode; the branch modes update exactly
    // one reference and never consult decorations.
    let mut update_refs: Option<Vec<String>> = None;
    let onto: ObjectId;
    // The fully-qualified branch that --advance/--revert updates.
    let mut branch_full: Option<String> = None;

    if let Some(spec) = &onto_name {
        onto = match try_peel_to_commit(&repo, spec, "--onto") {
            Ok(id) => id,
            Err(e) => return fatal(&e),
        };
        update_refs = Some(std::mem::take(&mut positive_refs));
    } else {
        let (raw, label) = match (&advance_name, &revert_name) {
            (Some(v), _) => (v, "--advance"),
            (_, Some(v)) => (v, "--revert"),
            _ => unreachable!("a mode was validated above"),
        };
        let Some(full) = dwim_ref(&repo, raw) else {
            return fatal(&format!("argument to {label} must be a reference"));
        };
        onto = match try_peel_to_commit(&repo, &full, label) {
            Ok(id) => id,
            Err(e) => return fatal(&e),
        };
        if positive_refexprs > 1 {
            return fatal(&format!(
                "'{label}' cannot be used with multiple revision ranges \
                 because the ordering would be ill-defined"
            ));
        }
        branch_full = Some(full);
    }

    // --- the single reference `--ref`/`--advance`/`--revert` updates -------
    let single_ref: Option<String>;
    let mut single_old = ObjectId::null(repo.object_hash());
    if let Some(r) = &ref_name {
        if update_refs.as_ref().is_some_and(|u| u.len() > 1) {
            return error("'--ref' cannot be used with multiple revision ranges");
        }
        if FullName::try_from(r.as_str()).is_err() || !r.starts_with("refs/") {
            return error(&format!("'{r}' is not a valid refname"));
        }
        if let Ok(Some(mut existing)) = repo.try_find_reference(r.as_str()) {
            if let Ok(id) = existing.peel_to_id() {
                single_old = id.detach();
            }
        }
        single_ref = Some(r.clone());
    } else {
        single_ref = branch_full.clone();
        if single_ref.is_some() {
            single_old = onto;
        }
    }

    // --- prepare_revision_walk --------------------------------------------
    let tips: Vec<ObjectId> = revs.iter().filter(|e| !e.negative).map(|e| e.oid).collect();
    let ends: Vec<ObjectId> = revs.iter().filter(|e| e.negative).map(|e| e.oid).collect();
    let topo = gix::traverse::commit::topo::Builder::from_iters(&repo.objects, tips, Some(ends))
        .sorting(gix::traverse::commit::topo::Sorting::TopoOrder)
        .build()?;
    let mut order: Vec<ObjectId> = Vec::new();
    for info in topo {
        order.push(info?.id);
    }
    // Pick needs oldest-first so each commit builds on its replayed parent;
    // revert keeps newest-first, peeling changes off the top like `git revert`.
    if mode == Mode::Pick {
        order.reverse();
    }

    // --- the replay loop ---------------------------------------------------
    let decorations = if single_ref.is_none() {
        load_branch_decorations(&repo, detached_head)?
    } else {
        HashMap::new()
    };

    let merge_options = repo.tree_merge_options()?;
    let mut replayed: HashMap<ObjectId, ObjectId> = HashMap::new();
    let mut last_commit = onto;
    let mut updates: Vec<Update> = Vec::new();
    let mut conflicted = false;

    for pickme in order {
        if repo.find_commit(pickme)?.parent_ids().count() > 1 {
            return fatal("replaying merge commits is not supported yet!");
        }

        // In revert mode each commit stacks on the previous result; in pick mode
        // it stacks on its already-replayed parent, or `onto` if it has none.
        let fallback = if mode == Mode::Revert { last_commit } else { onto };
        // The CLI exposes no `--empty`, so the default `drop` is the only value
        // reachable here.
        let new_commit = match pick_regular_commit(
            &repo,
            pickme,
            &replayed,
            fallback,
            &merge_options,
            mode,
            EmptyAction::Drop,
        )? {
            Picked::Commit(id) => id,
            Picked::Conflict => {
                conflicted = true;
                break;
            }
            // Unreachable with `EmptyAction::Drop`.
            Picked::BecameEmpty(id) => {
                bail!("commit {id} became empty after replay");
            }
        };

        replayed.insert(pickme, new_commit);
        last_commit = new_commit;

        if single_ref.is_some() {
            continue;
        }
        for refname in decorations.get(&pickme).into_iter().flatten() {
            if refname.as_str() == "HEAD" && !detached_head {
                continue;
            }
            if !contained
                && !update_refs
                    .as_ref()
                    .is_some_and(|u| u.iter().any(|r| r == refname))
            {
                continue;
            }
            updates.push(Update {
                refname: refname.clone(),
                old: pickme,
                new: new_commit,
            });
        }
    }

    if conflicted {
        // git documents that a conflicted replay writes nothing at all and
        // leaves every reference alone; only the status reports it.
        return Ok(ExitCode::FAILURE);
    }

    if let Some(r) = &single_ref {
        updates.push(Update {
            refname: r.clone(),
            old: single_old,
            new: last_commit,
        });
    }

    // --- reflog message ----------------------------------------------------
    let reflog_msg = if let Some(v) = &revert_name {
        format!("replay --revert {v}")
    } else if let Some(v) = &advance_name {
        format!("replay --advance {v}")
    } else {
        format!("replay --onto {onto}")
    };

    match ref_mode {
        RefAction::Print => {
            let mut out: Vec<u8> = Vec::new();
            for u in &updates {
                writeln!(out, "update {} {} {}", u.refname, u.new, u.old)?;
            }
            std::io::stdout().lock().write_all(&out)?;
        }
        RefAction::Update => {
            let mut edits: Vec<RefEdit> = Vec::new();
            for u in &updates {
                // git's transaction turns a no-op update into nothing at all —
                // no ref write and, importantly, no reflog entry.
                if u.old == u.new {
                    continue;
                }
                let name = FullName::try_from(u.refname.as_str())
                    .map_err(|e| anyhow!("invalid ref name {:?}: {e}", u.refname))?;
                let expected = if u.old.is_null() {
                    PreviousValue::MustNotExist
                } else {
                    PreviousValue::MustExistAndMatch(Target::Object(u.old))
                };
                edits.push(RefEdit {
                    change: Change::Update {
                        log: LogChange {
                            mode: RefLog::AndReference,
                            force_create_reflog: false,
                            message: reflog_msg.clone().into(),
                        },
                        expected,
                        new: Target::Object(u.new),
                    },
                    name,
                    deref: false,
                });
            }
            if !edits.is_empty() {
                repo.edit_references(edits)?;
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// The value of `--opt=<v>` or, when it was written `--opt <v>`, the argument
/// that follows — advancing the cursor past it.
///
/// A missing value is `get_arg()`'s refusal: ``error: option `<name>' requires a
/// value`` on stderr and exit 129, with no usage block. It used to unwind as an
/// ordinary error and come out as `zvcs: replay: option `--onto` requires a
/// value` at exit 1 — the port's voice for something that is not a port gap.
fn value_of(args: &[String], i: &mut usize, inline: Option<&str>, name: &str) -> Result<String> {
    match inline {
        Some(v) => Ok(v.to_string()),
        None => {
            *i += 1;
            Ok(super::value_at(args, *i, name)?.to_string())
        }
    }
}

/// `parse_ref_action_mode`.
fn parse_ref_action(v: &str) -> Option<RefAction> {
    match v {
        "update" => Some(RefAction::Update),
        "print" => Some(RefAction::Print),
        _ => None,
    }
}

/// git's `die`: the message on stderr with a `fatal:` prefix, exit 128.
fn fatal(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// git's `error` on a path `cmd_replay` turns into `exit(128)`.
fn error(msg: &str) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(128))
}

/// Resolve a revision expression to the commit it names, for the revision walk.
fn peel_to_commit(repo: &gix::Repository, spec: &str) -> Result<ObjectId> {
    Ok(repo.rev_parse_single(spec)?.object()?.peel_to_commit()?.id)
}

/// `peel_committish`, whose three failure modes carry distinct `die` messages.
fn try_peel_to_commit(repo: &gix::Repository, name: &str, mode: &str) -> Result<ObjectId, String> {
    // `repo_get_oid()` succeeds for a full-length hex without asking the object
    // database (see [`crate::objname`]), so `parse_object_or_die()` is the next
    // thing to run and it dies first, ahead of either message below. It names
    // the operand as written — verified against stock 2.55.0, which echoes an
    // uppercase `--onto` argument back uppercase rather than folding it.
    if crate::objname::resolves_but_absent(repo, name) {
        return Err(format!("unable to parse object: {name}"));
    }
    // Resolution and lookup have unrelated error types, so each is reduced to
    // the same `die` message rather than joined through `?`.
    let object = repo
        .rev_parse_single(name)
        .map_err(|_| ())
        .and_then(|id| id.object().map_err(|_| ()))
        .map_err(|()| format!("'{name}' is not a valid commit-ish for {mode}"))?;
    object
        .peel_to_commit()
        .map(|c| c.id)
        .map_err(|_| format!("'{name}' does not point to a commit for {mode}"))
}

/// git's `repo_dwim_ref`: the fully-qualified name of the reference `name`
/// designates, but only when exactly one of the rev-parse rules matches.
fn dwim_ref(repo: &gix::Repository, name: &str) -> Option<String> {
    let mut first: Option<String> = None;
    let mut matches = 0usize;
    for rule in REV_PARSE_RULES {
        let candidate = rule.replace("{}", name);
        // `expand_ref()` calls `refs_resolve_ref_unsafe()` on the *expanded*
        // name, so each rule is an exact lookup — the bare `{}` rule asks for a
        // ref literally named `main`, which is how `HEAD` and `FETCH_HEAD`
        // resolve and why a branch does not. gix's lookups are partial-name
        // lookups that re-run this same expansion internally, so `main` resolved
        // under the `{}` rule *and* under `refs/heads/{}` — two matches where git
        // counts one, and `repo_dwim_ref(...) == 1` is what gates
        // `set_up_branch_mode()`. Every `--advance <branch>` therefore died with
        // "argument to --advance must be a reference". Requiring the resolved
        // name to be the candidate itself restores the exact-lookup semantics.
        let exact = repo
            .try_find_reference(candidate.as_str())
            .ok()
            .flatten()
            .is_some_and(|r| r.name().as_bstr() == candidate.as_bytes());
        if exact {
            matches += 1;
            if first.is_none() {
                first = Some(candidate);
            }
        }
    }
    if matches == 1 {
        first
    } else {
        None
    }
}

/// git's `load_branch_decorations` plus the decoration-list ordering its
/// `add_name_decoration` produces: references are visited in ascending name
/// order and *prepended*, so each commit's list runs in descending name order,
/// with `HEAD` — visited last — at the front.
pub(super) fn load_branch_decorations(
    repo: &gix::Repository,
    detached_head: bool,
) -> Result<HashMap<ObjectId, Vec<String>>> {
    // Materialise names first: the reference iterator holds the packed-refs
    // buffer, which would block the per-ref object lookups below.
    let mut names: Vec<String> = Vec::new();
    for r in repo.references()?.all()? {
        let r = r.map_err(|e| anyhow!("{e}"))?;
        let name = r.name().as_bstr().to_str_lossy().into_owned();
        if name.starts_with("refs/heads/") {
            names.push(name);
        }
    }
    names.sort();

    let mut map: HashMap<ObjectId, Vec<String>> = HashMap::new();
    for name in names {
        let Ok(mut reference) = repo.find_reference(name.as_str()) else {
            continue;
        };
        let Ok(id) = reference.peel_to_id() else {
            continue;
        };
        map.entry(id.detach()).or_default().insert(0, name);
    }
    if detached_head {
        if let Ok(id) = repo.head_id() {
            map.entry(id.detach()).or_default().insert(0, "HEAD".into());
        }
    }
    Ok(map)
}

/// `create_commit`: the replayed commit for `based_on`, carrying its tree, its
/// extra headers (minus the signatures) and — for a pick — its author and
/// message; a revert instead gets the current user as author and git's
/// generated revert message.
pub(super) fn create_commit(
    repo: &gix::Repository,
    based_on: &gix::Commit<'_>,
    tree: ObjectId,
    parent: ObjectId,
    mode: Mode,
) -> Result<ObjectId> {
    // `read_commit_extra_headers(based_on, exclude_gpgsig)` — gitoxide has
    // already split off the standard fields (`tree`, `parent`, `author`,
    // `committer`, `encoding`), so only the signature exclusions remain.
    let mut extra_headers: Vec<(BString, BString)> = Vec::new();
    for (key, value) in based_on.decode()?.extra_headers.iter() {
        let key: &BStr = key;
        if key == BStr::new("gpgsig") || key == BStr::new("gpgsig-sha256") {
            continue;
        }
        let value: &BStr = value.as_ref();
        extra_headers.push((key.to_owned(), value.to_owned()));
    }

    // git's `find_commit_subject` starts the message at the subject, skipping
    // the blank lines that follow the header block.
    let raw = based_on.message_raw()?;
    let all: &[u8] = raw.as_ref();
    let body = &all[all.iter().position(|&b| b != b'\n').unwrap_or(all.len())..];

    let committer = repo
        .committer()
        .ok_or_else(|| anyhow!("committer identity is not configured"))??
        .to_owned()?;

    let (message, author) = match mode {
        Mode::Pick => (BString::from(body), based_on.author()?.to_owned()?),
        Mode::Revert => {
            let author = repo
                .author()
                .ok_or_else(|| anyhow!("author identity is not configured"))??
                .to_owned()?;
            (revert_message(body, based_on.id), author)
        }
    };

    let commit = gix::objs::Commit {
        message,
        tree,
        author,
        committer,
        encoding: None,
        parents: std::iter::once(parent).collect(),
        extra_headers,
    };
    Ok(repo.write_object(&commit)?.detach())
}

/// `sequencer_format_revert_message` with `use_commit_reference == false`, for a
/// non-merge commit (git refuses to replay merges before reaching this point).
///
/// The subject is the first line of `body`. A subject already of the form
/// `Revert "<x>"` — but not `Revert "Revert "<x>""` — flips to `Reapply`, which
/// is how repeated reverts read in stock git.
fn revert_message(body: &[u8], commit: ObjectId) -> BString {
    let subject = match body.iter().position(|&b| b == b'\n') {
        Some(nl) => &body[..nl],
        None => body,
    };

    let mut out: Vec<u8> = Vec::new();
    match subject.strip_prefix(b"Revert \"".as_slice()) {
        Some(orig) if !orig.starts_with(b"Revert \"") => {
            out.extend_from_slice(b"Reapply \"");
            out.extend_from_slice(orig);
            out.push(b'\n');
        }
        _ => {
            out.extend_from_slice(b"Revert \"");
            out.extend_from_slice(subject);
            out.extend_from_slice(b"\"\n");
        }
    }
    out.extend_from_slice(b"\nThis reverts commit ");
    out.extend_from_slice(commit.to_string().as_bytes());
    out.extend_from_slice(b".\n");
    out.into()
}
