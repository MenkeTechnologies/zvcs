//! `git history` — EXPERIMENTAL history rewriting (`fixup`, `reword`, `split`).
//!
//! What this module implements, byte-identically with stock git 2.55.0: the
//! whole command-line surface. That is `-h` for the command and for each
//! subcommand (usage text on stdout, exit 129), the missing/unknown subcommand
//! diagnostics, per-subcommand option parsing (`--update-refs`, `-n`/
//! `--dry-run`/`--no-dry-run`, `--reedit-message`/`--no-reedit-message`,
//! `--empty`, `--` plus trailing pathspecs for `split`), the option-value
//! validation messages, repository discovery, the single-revision check, commit
//! lookup, and `fixup`'s bare-repository rejection — each with git's exact
//! wording, stream, and exit status (129 for usage errors, 128 for `fatal:`,
//! 255 for the `error()` returns the builtin passes up once setup is past).
//!
//! All three subcommands are ported end to end:
//!
//! * `setup_revwalk()` — `--reverse --topo-order --full-history
//!   --ancestry-path=<commit> ^<commit>` over `--branches HEAD` (or `HEAD`
//!   alone under `--update-refs=head`, behind its descendance check), plus
//!   `revwalk_contains_merges()`'s refusal.
//! * `commit_tree_ext()` — the rewritten commit keeps the original's author,
//!   message body and extra headers, minus `encoding`/`gpgsig`/`gpgsig-sha256`.
//! * `fill_commit_message()` — `.git/COMMIT_EDITMSG` with the old message and a
//!   commented hint, the editor, then `strbuf_stripspace` + `cleanup_message`.
//! * `fixup`'s three-way merge (`HEAD` as base, the target's tree as ours, the
//!   index tree as theirs) and `commit_became_empty()` with
//!   `--empty=drop|keep|abort`.
//! * `split_commit()` — a scratch `$GIT_DIR/history-split.index` seeded with the
//!   target's parent tree, the interactive hunk selector over it
//!   (`add_patch::run_index`, git's `run_add_p_index`), the resulting tree, the
//!   two empty-half refusals, and the two commits the halves become.
//! * `handle_reference_updates()` — `replay_revisions()` over the descendants
//!   (sharing `replay.rs`'s `pick_regular_commit`), then the references that
//!   decorate the target itself, printed as `update <ref> <new> <old>` under
//!   `--dry-run` and committed as one reference transaction otherwise.
//!
//! Two deliberate reductions, neither observable in a commit or a reference:
//!
//! * The `Changes to be committed:` block in the editor buffer is a reduced
//!   form of `wt_status_print()` — see [`staged_status_block`]. Every line of
//!   it is a comment line and is stripped before the message is used.
//! * git orders the branch fan-out coming out of `replay_revisions()` by its
//!   `strmap` bucket layout; this port emits it in the decoration order
//!   `load_branch_decorations` produces (descending ref name, `HEAD` first),
//!   which agrees for the ref sets measured here but is not the same rule.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::merge::blob::builtin_driver::text::Labels;
use gix::merge::tree::TreatAsUnresolved;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

use super::replay::{pick_regular_commit, EmptyAction, Mode, Picked};

/// The three-line synopsis git prints for the command as a whole.
const USAGE: &str = "\
usage: git history fixup <commit> [--dry-run] [--update-refs=(branches|head)] [--reedit-message] [--empty=(drop|keep|abort)]
   or: git history reword <commit> [--dry-run] [--update-refs=(branches|head)]
   or: git history split <commit> [--dry-run] [--update-refs=(branches|head)] [--] [<pathspec>...]
";

/// `fixup`'s own `-h` text: synopsis, blank line, option list.
const USAGE_FIXUP: &str = "\
usage: git history fixup <commit> [--dry-run] [--update-refs=(branches|head)] [--reedit-message] [--empty=(drop|keep|abort)]

    --update-refs (branches|head)
                          control which refs should be updated
    -n, --[no-]dry-run    perform a dry-run without updating any refs
    --[no-]reedit-message open an editor to modify the commit message
    --empty (drop|keep|abort)
                          how to handle commits that become empty
";

/// `reword`'s own `-h` text.
const USAGE_REWORD: &str = "\
usage: git history reword <commit> [--dry-run] [--update-refs=(branches|head)]

    --update-refs (branches|head)
                          control which refs should be updated
    -n, --[no-]dry-run    perform a dry-run without updating any refs
";

/// `split`'s own `-h` text. Note the deliberately different `--update-refs`
/// description; git's own option table words it differently here.
const USAGE_SPLIT: &str = "\
usage: git history split <commit> [--dry-run] [--update-refs=(branches|head)]

    --update-refs (branches|head)
                          control ref update behavior
    -n, --[no-]dry-run    perform a dry-run without updating any refs
";

/// Which subcommand is running; selects the option table and usage text.
#[derive(Clone, Copy, PartialEq)]
enum Sub {
    Fixup,
    Reword,
    Split,
}

impl Sub {
    /// The `-h` text for this subcommand, also printed after an unknown option.
    fn usage(self) -> &'static str {
        match self {
            Sub::Fixup => USAGE_FIXUP,
            Sub::Reword => USAGE_REWORD,
            Sub::Split => USAGE_SPLIT,
        }
    }
}

/// Options common to all three subcommands, plus the `fixup`-only ones.
struct Opts {
    dry_run: bool,
    /// `--update-refs=head` restricts updates to HEAD; `branches` is the default.
    head_only: bool,
    /// `--reedit-message` (fixup only).
    reedit_message: bool,
    /// `--empty=<action>` (fixup only).
    empty: EmptyAction,
    /// The single `<commit>` argument.
    rev: Option<String>,
    /// Trailing pathspecs (split only).
    pathspecs: Vec<String>,
}

/// git's usage-error exit status (`usage()`/`usage_with_options()`).
const EXIT_USAGE: u8 = 129;
/// git's `die()` exit status once the command is past setup.
const EXIT_DIE: u8 = 255;
/// git's `die()` exit status from setup / option-value parsing (`fatal:`).
const EXIT_FATAL: u8 = 128;

/// `git history` — rewrite history by modifying one commit and replaying its
/// descendants.
///
/// Argument handling matches stock git exactly, including which stream each
/// diagnostic goes to and the exit status. Past that, this dispatches to the
/// three rewrites; every failure they report is one of git's `error()` strings,
/// which the builtin passes up as exit 255.
pub fn history(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand name at index 0 so both calling conventions work.
    let args = match args.first() {
        Some(a) if a == "history" => &args[1..],
        _ => args,
    };

    let Some(first) = args.first() else {
        eprint!("error: need a subcommand\n{USAGE}\n");
        return Ok(ExitCode::from(EXIT_USAGE));
    };

    // `-h` anywhere in the leading position prints to stdout and still exits 129.
    // `--help-all` joins it: parse_options_step() tests that name with a
    // `strcmp()` of its own, ahead of parse_long_opt(), and renders `USAGE_FULL`
    // — the same block, because this option table has no `PARSE_OPT_HIDDEN`
    // entry. The exact compare is why `--help-a` and `--help-all=x` stay errors.
    if first == "-h" || first == "--help" || first == "--help-all" {
        println!("{USAGE}");
        std::io::stdout().flush()?;
        return Ok(ExitCode::from(EXIT_USAGE));
    }

    let sub = match first.as_str() {
        "fixup" => Sub::Fixup,
        "reword" => Sub::Reword,
        "split" => Sub::Split,
        // A dashed word never reaches the sub-command lookup: the top-level
        // table is `OPT_SUBCOMMAND`s only, so `parse_options_step()` sends it to
        // `parse_long_opt()`, finds nothing and reports `PARSE_OPT_UNKNOWN` —
        // the option named as typed, `=<value>` and all, then the block.
        other if other.len() > 1 && other.starts_with('-') => {
            return Ok(super::unknown_option(other, &format!("{USAGE}\n")));
        }
        other => {
            eprint!("error: unknown subcommand: `{other}'\n{USAGE}\n");
            return Ok(ExitCode::from(EXIT_USAGE));
        }
    };

    let opts = match parse(sub, &args[1..])? {
        Parsed::Opts(o) => o,
        Parsed::Exit(code) => return Ok(code),
    };

    // git runs its repository setup before touching the revision.
    let repo = match gix::discover(".") {
        Ok(repo) => repo,
        Err(_) => {
            eprintln!("fatal: not a git repository (or any of the parent directories): .git");
            return Ok(ExitCode::from(EXIT_FATAL));
        }
    };

    let Some(rev) = opts.rev.as_deref() else {
        eprintln!("error: command expects a single revision");
        return Ok(ExitCode::from(EXIT_DIE));
    };

    // `fixup` reads staged changes, so it needs an index and a worktree.
    if sub == Sub::Fixup && repo.worktree().is_none() {
        eprintln!("error: cannot run fixup in a bare repository");
        return Ok(ExitCode::from(EXIT_DIE));
    }

    // git resolves the revision and requires it to name a commit.
    let commit = repo
        .rev_parse_single(rev)
        .ok()
        .and_then(|id| id.object().ok())
        .and_then(|obj| obj.peel_to_commit().ok());
    if commit.is_none() {
        eprintln!("error: commit cannot be found: {rev}");
        return Ok(ExitCode::from(EXIT_DIE));
    }

    let original = commit.expect("checked above").id;
    // `if (action == REF_ACTION_DEFAULT) action = REF_ACTION_BRANCHES;`
    let action = if opts.head_only {
        RefAction::Head
    } else {
        RefAction::Branches
    };

    // Every remaining failure is git's `error()`, which the builtin returns as
    // -1 and `git` turns into exit 255.
    let outcome = match sub {
        Sub::Fixup => fixup(&repo, &opts, rev, original, action),
        Sub::Reword => reword(&repo, &opts, rev, original, action),
        Sub::Split => split(&repo, &opts, rev, original, action),
    }?;
    match outcome {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(msg) => {
            eprintln!("error: {msg}");
            Ok(ExitCode::from(EXIT_DIE))
        }
    }
}

/// `enum ref_action` — which references the rewrite is allowed to move.
#[derive(Clone, Copy, PartialEq)]
enum RefAction {
    Branches,
    Head,
}

/// A git `error()` return: the text goes to stderr with an `error: ` prefix and
/// the command exits 255. `Ok(Ok(()))` is success; `Err` is reserved for faults
/// that are not part of git's diagnostic vocabulary.
type Outcome = Result<std::result::Result<(), String>>;

/// `cmd_history_fixup()` from `repo_read_index()` onwards.
fn fixup(
    repo: &gix::Repository,
    opts: &Opts,
    rev: &str,
    original: ObjectId,
    action: RefAction,
) -> Outcome {
    // Resolve HEAD so its tree can be the merge base: the staged changes are the
    // diff from HEAD's tree to the index tree.
    let Ok(head_commit) = repo.head_commit() else {
        return Ok(Err("cannot look up HEAD".into()));
    };
    let Ok(head_tree) = head_commit.tree_id().map(|t| t.detach()) else {
        return Ok(Err("cannot get tree for HEAD".into()));
    };
    let Some(index_tree) = staged_tree(repo) else {
        return Ok(Err("unable to read index".into()));
    };
    // `repo_index_has_changes(repo, head_tree, NULL)`: an index that writes back
    // to HEAD's own tree has nothing staged.
    if index_tree == head_tree {
        return Ok(Err("nothing to fixup: no staged changes".into()));
    }
    let original_commit = repo.find_commit(original)?;
    let original_tree = original_commit.tree_id()?.detach();
    let parents: Vec<ObjectId> = original_commit.parent_ids().map(|p| p.detach()).collect();

    // The same three-way merge a cherry-pick does: base is HEAD, ours is the
    // target commit's tree, theirs is the index tree.
    let mut outcome = repo.merge_trees(
        head_tree,
        original_tree,
        index_tree,
        Labels {
            ancestor: Some(BStr::new("HEAD")),
            current: Some(BStr::new(rev)),
            other: Some(BStr::new("staged")),
        },
        repo.tree_merge_options()?,
    )?;
    // merge-ort writes its result tree before `merge_result.clean` is consulted,
    // so a conflicted fixup still leaves the merged trees in the odb.
    let result_tree = outcome.tree.write()?.detach();
    if outcome.has_unresolved_conflicts(TreatAsUnresolved::git()) {
        return Ok(Err("fixup would produce conflicts; aborting".into()));
    }

    // `commit_became_empty()`: the merged tree matches the target's parent.
    let parent_tree = match parents.first() {
        Some(p) => repo.find_commit(*p)?.tree_id()?.detach(),
        None => repo.object_hash().empty_tree(),
    };
    let mut rewritten = None;
    if result_tree == parent_tree {
        match opts.empty {
            EmptyAction::Drop => {
                // Dropping the target means replaying its descendants straight
                // onto its parent; a root commit has none to replay onto.
                let Some(p) = parents.first().copied() else {
                    return Ok(Err(format!(
                        "cannot drop root commit {rev}: it has no parent to replay onto"
                    )));
                };
                rewritten = Some(p);
            }
            EmptyAction::Keep => {}
            EmptyAction::Abort => {
                return Ok(Err(format!("fixup makes commit {rev} empty")));
            }
        }
    }

    let order = match setup_revwalk(repo, action, original)? {
        Ok(o) => o,
        Err(msg) => return Ok(Err(msg)),
    };

    let rewritten = match rewritten {
        Some(id) => id,
        None => match commit_tree_ext(
            repo,
            "fixup",
            original,
            &parents,
            original_tree,
            result_tree,
            opts.reedit_message,
        )? {
            Ok(id) => id,
            Err(msg) => return Ok(Err(msg)),
        },
    };

    handle_reference_updates(
        repo,
        &order,
        action,
        original,
        rewritten,
        &format!("fixup: updating {rev}"),
        opts.dry_run,
        opts.empty,
    )
}

/// `cmd_history_reword()`.
fn reword(
    repo: &gix::Repository,
    opts: &Opts,
    rev: &str,
    original: ObjectId,
    action: RefAction,
) -> Outcome {
    let order = match setup_revwalk(repo, action, original)? {
        Ok(o) => o,
        Err(msg) => return Ok(Err(msg)),
    };

    // `commit_tree_with_edited_message()`: same tree, same parents, only the
    // message goes through the editor.
    let commit = repo.find_commit(original)?;
    let tree = commit.tree_id()?.detach();
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
    let parent_tree = match parents.first() {
        Some(p) => repo.find_commit(*p)?.tree_id()?.detach(),
        None => repo.object_hash().empty_tree(),
    };
    let rewritten = match commit_tree_ext(
        repo,
        "reworded",
        original,
        &parents,
        parent_tree,
        tree,
        true,
    )? {
        Ok(id) => id,
        Err(msg) => return Ok(Err(msg)),
    };

    handle_reference_updates(
        repo,
        &order,
        action,
        original,
        rewritten,
        &format!("reword: updating {rev}"),
        opts.dry_run,
        EmptyAction::Abort,
    )
}

/// `cmd_history_split()`.
fn split(
    repo: &gix::Repository,
    opts: &Opts,
    rev: &str,
    original: ObjectId,
    action: RefAction,
) -> Outcome {
    let order = match setup_revwalk(repo, action, original)? {
        Ok(o) => o,
        Err(msg) => return Ok(Err(msg)),
    };
    // git checks this *after* `setup_revwalk`, so a merge in the descendants is
    // reported before a merge at the target.
    if repo.find_commit(original)?.parent_ids().count() > 1 {
        return Ok(Err("cannot split up merge commit".into()));
    }

    let rewritten = match split_commit(repo, original, &opts.pathspecs)? {
        Ok(id) => id,
        Err(msg) => return Ok(Err(msg)),
    };

    handle_reference_updates(
        repo,
        &order,
        action,
        original,
        rewritten,
        &format!("split: updating {rev}"),
        opts.dry_run,
        EmptyAction::Abort,
    )
}

/// git's `split_commit()`: turn one commit into two by letting the user pick,
/// hunk by hunk, which of its changes belong to the first.
///
/// The selection happens in a scratch index seeded with the target's *parent*
/// tree, so the hunks the user accepts accumulate into the split-out tree. Both
/// halves then go through `commit_tree_ext` with the message editor: the first
/// carries the split-out tree on the original's parents, the second carries the
/// original tree on the first. The second is what descendants are replayed onto.
fn split_commit(
    repo: &gix::Repository,
    original: ObjectId,
    pathspecs: &[String],
) -> Result<std::result::Result<ObjectId, String>> {
    let commit = repo.find_commit(original)?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
    let parent_tree = match parents.first() {
        Some(p) => repo.find_commit(*p)?.tree_id()?.detach(),
        None => repo.object_hash().empty_tree(),
    };
    let original_tree = commit.tree_id()?.detach();

    // `repo_git_path_replace(repo, &index_file, "%s", "history-split.index")`.
    let index_file = repo.git_dir().join("history-split.index");
    if let Err(e) = write_ondisk_index(repo, parent_tree, &index_file) {
        let _ = std::fs::remove_file(&index_file);
        return Ok(Err(format!("unable to populate index with tree: {e}")));
    }

    // `run_add_p_index(..., ADD_P_DISALLOW_EDIT)`.
    let selector = super::add_patch::run_index(
        repo,
        &index_file,
        &original.to_string(),
        super::add_patch::Options {
            disallow_edit: true,
            ..Default::default()
        },
        pathspecs,
    );
    let split_tree = match selector {
        Ok(_) => tree_of_index_file(repo, &index_file),
        Err(e) => {
            let _ = std::fs::remove_file(&index_file);
            return Err(e);
        }
    };
    let _ = std::fs::remove_file(&index_file);
    let Some(split_tree) = split_tree else {
        return Ok(Err("failed split tree".into()));
    };

    // Neither half may be empty, so a selection that took nothing (or took
    // everything) is refused.
    if split_tree == parent_tree {
        return Ok(Err("split commit is empty".into()));
    }
    if split_tree == original_tree {
        return Ok(Err("split commit tree matches original commit".into()));
    }

    let first = match commit_tree_ext(
        repo,
        "split-out",
        original,
        &parents,
        parent_tree,
        split_tree,
        true,
    )? {
        Ok(id) => id,
        Err(_) => return Ok(Err("failed writing first commit".into())),
    };
    let first_tree = repo.find_commit(first)?.tree_id()?.detach();
    match commit_tree_ext(
        repo,
        "split-out",
        original,
        &[first],
        first_tree,
        original_tree,
        true,
    )? {
        Ok(id) => Ok(Ok(id)),
        Err(_) => Ok(Err("failed writing second commit".into())),
    }
}

/// git's `write_ondisk_index()`: unpack `tree` into a fresh index and write it
/// to `path`, so the hunk selector has something to stage into.
pub(super) fn write_ondisk_index(
    repo: &gix::Repository,
    tree: ObjectId,
    path: &std::path::Path,
) -> Result<()> {
    let mut index = repo.index_from_tree(&tree)?;
    index.set_path(path);
    // A stale file from an interrupted run would otherwise block the lock.
    let _ = std::fs::remove_file(path);
    // A scratch index at `path` rather than `.git/index`, but git writes those
    // through the same `write_locked_index()` -> `do_write_index()` pair, which
    // reads `skip_hash` off the repository regardless of where the file lands
    // (read-cache.c:2830-2831).
    index.write(crate::config::index_write_options(repo))?;
    Ok(())
}

/// `read_index_from()` + `write_in_core_index_as_tree()` over the scratch index
/// the selector staged into. `None` is git's `failed split tree`.
pub(super) fn tree_of_index_file(repo: &gix::Repository, path: &std::path::Path) -> Option<ObjectId> {
    let index = gix::index::File::at(
        path,
        repo.object_hash(),
        false,
        gix::index::decode::Options::default(),
    )
    .ok()?;
    tree_of_index(repo, &index)
}

/// git's `setup_revwalk()`: the strict descendants of `original` that the walk
/// tips reach, oldest first in topological order.
///
/// git spells this `--reverse --topo-order --full-history
/// --ancestry-path=<original> ^<original>` over `--branches HEAD` (or `HEAD`
/// alone for `--update-refs=head`). `--full-history` only matters under pathspec
/// limiting, which this walk has none of; the rest is `^<original>` (drop the
/// target and everything behind it) plus `--ancestry-path` (keep only what
/// descends from it).
fn setup_revwalk(
    repo: &gix::Repository,
    action: RefAction,
    original: ObjectId,
) -> Result<std::result::Result<Vec<ObjectId>, String>> {
    let mut tips: Vec<ObjectId> = Vec::new();
    if action == RefAction::Head {
        let Ok(head) = repo.head_commit() else {
            return Ok(Err("cannot look up HEAD".into()));
        };
        // `repo_is_descendant_of(head, {original})` — rewriting a commit HEAD
        // cannot see would leave HEAD pointing into the old history.
        if head.id != original && !is_descendant_of(repo, head.id, original)? {
            return Ok(Err(
                "rewritten commit must be an ancestor of HEAD when using --update-refs=head"
                    .into(),
            ));
        }
        tips.push(head.id);
    } else {
        // Materialise the names first: the reference iterator holds the
        // packed-refs buffer, which would block the per-ref object lookups.
        let mut names: Vec<String> = Vec::new();
        for r in repo.references()?.prefixed("refs/heads/")? {
            let r = r.map_err(|e| anyhow::anyhow!("{e}"))?;
            names.push(r.name().as_bstr().to_str_lossy().into_owned());
        }
        for name in names {
            let Ok(mut reference) = repo.find_reference(name.as_str()) else {
                continue;
            };
            if let Ok(id) = reference.peel_to_id() {
                tips.push(id.detach());
            }
        }
        if let Ok(id) = repo.head_id() {
            tips.push(id.detach());
        }
    }
    tips.sort();
    tips.dedup();

    // `^<original>` hides the target and its ancestors, so the walk output is
    // already free of everything but siblings and descendants.
    let topo = gix::traverse::commit::topo::Builder::from_iters(
        &repo.objects,
        tips,
        Some(std::iter::once(original)),
    )
    .sorting(gix::traverse::commit::topo::Sorting::TopoOrder)
    .build()?;
    let mut newest_first: Vec<ObjectId> = Vec::new();
    for info in topo {
        newest_first.push(info?.id);
    }
    // `--reverse`: parents before children, which is also the order the
    // ancestry-path filter below needs.
    newest_first.reverse();
    let order = newest_first;

    // `--ancestry-path=<original>`: keep only commits that reach `original`.
    // Walking oldest first, a commit qualifies exactly when one of its parents
    // is the target or already qualified.
    let mut descendants: HashSet<ObjectId> = HashSet::new();
    let mut kept: Vec<ObjectId> = Vec::new();
    for id in order {
        let commit = repo.find_commit(id)?;
        let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        if !parents
            .iter()
            .any(|p| *p == original || descendants.contains(p))
        {
            continue;
        }
        descendants.insert(id);
        // `revwalk_contains_merges()` reruns the same walk with
        // `--min-parents=2` and refuses if it yields anything.
        if parents.len() > 1 {
            return Ok(Err("replaying merge commits is not supported yet!".into()));
        }
        kept.push(id);
    }
    Ok(Ok(kept))
}

/// `repo_is_descendant_of(candidate, {ancestor})`.
fn is_descendant_of(
    repo: &gix::Repository,
    candidate: ObjectId,
    ancestor: ObjectId,
) -> Result<bool> {
    for info in repo.rev_walk(Some(candidate)).all()? {
        if info?.id == ancestor {
            return Ok(true);
        }
    }
    Ok(false)
}

/// git's `commit_tree_ext()`: rewrite `original` with `new_tree` and `parents`,
/// keeping its authorship, its message body and its extra headers.
///
/// The excluded headers are git's `exclude_gpgsig` list — `encoding` (the
/// message is re-encoded), `gpgsig` and `gpgsig-sha256` (the signatures would no
/// longer verify). gitoxide parses `encoding` out of the extra headers already,
/// and `encoding: None` is what leaves it off the rewritten object.
fn commit_tree_ext(
    repo: &gix::Repository,
    action: &str,
    original: ObjectId,
    parents: &[ObjectId],
    old_tree: ObjectId,
    new_tree: ObjectId,
    edit_message: bool,
) -> Result<std::result::Result<ObjectId, String>> {
    let commit = repo.find_commit(original)?;
    let author = commit.author()?.to_owned()?;

    // `find_commit_subject()` starts the body at the subject, past the blank
    // lines the header block leaves behind.
    let raw = commit.message_raw()?;
    let all: &[u8] = raw.as_ref();
    let body = &all[all.iter().position(|&b| b != b'\n').unwrap_or(all.len())..];

    let message: BString = if edit_message {
        match fill_commit_message(repo, old_tree, new_tree, body, action)? {
            Ok(m) => m,
            Err(msg) => return Ok(Err(msg)),
        }
    } else {
        BString::from(body)
    };

    let mut extra_headers: Vec<(BString, BString)> = Vec::new();
    for (key, value) in commit.decode()?.extra_headers.iter() {
        let key: &BStr = key;
        if key == BStr::new("gpgsig") || key == BStr::new("gpgsig-sha256") {
            continue;
        }
        let value: &BStr = value.as_ref();
        extra_headers.push((key.to_owned(), value.to_owned()));
    }

    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??
        .to_owned()?;

    let object = gix::objs::Commit {
        message,
        tree: new_tree,
        author,
        committer,
        encoding: None,
        parents: parents.iter().copied().collect(),
        extra_headers,
    };
    Ok(Ok(repo.write_object(&object)?.detach()))
}

/// git's `fill_commit_message()`: write `$GIT_DIR/COMMIT_EDITMSG` with the old
/// message plus a commented hint and status block, run the editor over it, then
/// strip the comments back out.
///
/// The status block is a reduced form of `wt_status_print()` — the
/// `Changes to be committed:` listing for the `old_tree`→`new_tree` diff, not
/// the branch line, the ahead/behind counts or the unstaged and untracked
/// sections. Every line of it is a comment line, so `cleanup_message` removes
/// the whole block before it can reach the commit; the reduction is visible only
/// to a human reading the buffer in the editor.
fn fill_commit_message(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
    default_message: &[u8],
    action: &str,
) -> Result<std::result::Result<BString, String>> {
    let snap = repo.config_snapshot();
    let comment = super::commit::comment_prefix(&snap);

    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(default_message);
    buf.push(b'\n');
    // `strbuf_commented_addf(out, comment_line_str, hint, action, comment_line_str)`
    // — the hint is two lines, each prefixed with the comment string and a space.
    for line in [
        format!("Please enter the commit message for the {action} changes. Lines starting"),
        format!("with '{comment}' will be ignored, and an empty message aborts the commit."),
    ] {
        buf.extend_from_slice(format!("{comment} {line}\n").as_bytes());
    }
    buf.extend_from_slice(&staged_status_block(repo, old_tree, new_tree, &comment)?);

    let path = repo.git_dir().join("COMMIT_EDITMSG");
    std::fs::write(&path, &buf)?;
    if super::commit::launch_editor(&snap, &path).is_err() {
        eprintln!("Aborting commit as launching the editor failed.");
        return Ok(Err("failed writing reworded commit".into()));
    }
    let edited = std::fs::read(&path)?;

    // `strbuf_stripspace(out, comment_line_str)` then
    // `cleanup_message(out, COMMIT_MSG_CLEANUP_ALL, 0)` — the same transform
    // twice, which is idempotent.
    let text = String::from_utf8_lossy(&edited).into_owned();
    let cleaned = super::commit::cleanup_message(
        &text,
        &comment,
        super::commit::Cleanup::Strip,
        false,
    );
    if cleaned.is_empty() {
        eprintln!("Aborting commit due to empty commit message.");
        return Ok(Err("failed writing reworded commit".into()));
    }
    Ok(Ok(BString::from(cleaned)))
}

/// The `Changes to be committed:` block `wt_status_collect_changes_trees()` +
/// `wt_status_print()` append to the editor buffer, with git's comment prefixing
/// (`#` alone before a tab, `# ` before anything else) and its label column.
fn staged_status_block(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
    comment: &str,
) -> Result<Vec<u8>> {
    let old = repo.find_tree(old_tree).ok();
    let new = repo.find_tree(new_tree).ok();
    let changes = repo.diff_tree_to_tree(old.as_ref(), new.as_ref(), gix::diff::Options::default())?;
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows: Vec<(String, String)> = Vec::new();
    for change in &changes {
        use gix::object::tree::diff::ChangeDetached as C;
        let (what, path) = match change {
            C::Addition { location, .. } => ("new file:", location),
            C::Deletion { location, .. } => ("deleted:", location),
            C::Modification { location, .. } => ("modified:", location),
            C::Rewrite { location, .. } => ("renamed:", location),
        };
        rows.push((what.to_string(), path.to_str_lossy().into_owned()));
    }
    rows.sort_by(|a, b| a.1.cmp(&b.1));

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(format!("{comment} Changes to be committed:\n").as_bytes());
    for (what, path) in rows {
        // git pads the status word into a fixed column, giving `new file:   x`.
        out.extend_from_slice(format!("{comment}\t{what:<12}{path}\n").as_bytes());
    }
    out.extend_from_slice(format!("{comment}\n").as_bytes());
    Ok(out)
}

/// git's `handle_reference_updates()`: replay every descendant onto `rewritten`,
/// then move the references that pointed at `original` itself.
///
/// `dry_run` is git's `transaction == NULL` path, where each update is printed
/// as `update <ref> <new> <old>` instead of being staged.
#[allow(clippy::too_many_arguments)]
fn handle_reference_updates(
    repo: &gix::Repository,
    order: &[ObjectId],
    action: RefAction,
    original: ObjectId,
    rewritten: ObjectId,
    reflog_msg: &str,
    dry_run: bool,
    empty: EmptyAction,
) -> Outcome {
    let detached_head = repo.head()?.is_detached();
    let decorations = super::replay::load_branch_decorations(repo, detached_head)?;

    // --- replay_revisions() ------------------------------------------------
    // `opts.onto` is the rewritten commit and neither `ref` nor `advance` is
    // set, so the per-commit decoration loop is the only source of updates.
    let merge_options = repo.tree_merge_options()?;
    let mut replayed: HashMap<ObjectId, ObjectId> = HashMap::new();
    let mut updates: Vec<(String, ObjectId, ObjectId)> = Vec::new();
    for pickme in order {
        let new_commit = match pick_regular_commit(
            repo,
            *pickme,
            &replayed,
            rewritten,
            &merge_options,
            Mode::Pick,
            empty,
        )? {
            Picked::Commit(id) => id,
            // `replay_revisions` returns 1, which `handle_reference_updates`
            // passes up as a non-zero `ret`.
            Picked::Conflict => return Ok(Err("failed replaying descendants".into())),
            Picked::BecameEmpty(id) => {
                return Ok(Err(format!("commit {id} became empty after replay")));
            }
        };
        replayed.insert(*pickme, new_commit);

        for refname in decorations.get(pickme).into_iter().flatten() {
            if refname == "HEAD" && !detached_head {
                continue;
            }
            updates.push((refname.clone(), *pickme, new_commit));
        }
    }

    // --- the references that pointed at `original` itself ------------------
    for refname in decorations.get(&original).into_iter().flatten() {
        let is_head = refname == "HEAD";
        if action == RefAction::Head && !is_head {
            continue;
        }
        // HEAD only needs its own update when detached; otherwise the branch it
        // points at is already in the list.
        if action == RefAction::Branches && is_head && !detached_head {
            continue;
        }
        updates.push((refname.clone(), original, rewritten));
    }

    if dry_run {
        let mut out: Vec<u8> = Vec::new();
        for (refname, old, new) in &updates {
            writeln!(out, "update {refname} {new} {old}")?;
        }
        std::io::stdout().lock().write_all(&out)?;
        return Ok(Ok(()));
    }

    let mut edits: Vec<RefEdit> = Vec::new();
    for (refname, old, new) in &updates {
        let Ok(name) = FullName::try_from(refname.as_str()) else {
            return Ok(Err(format!("failed to update ref '{refname}'")));
        };
        edits.push(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: reflog_msg.into(),
                },
                expected: PreviousValue::MustExistAndMatch(Target::Object(*old)),
                new: Target::Object(*new),
            },
            name,
            deref: false,
        });
    }
    if !edits.is_empty() {
        if let Err(e) = repo.edit_references(edits) {
            return Ok(Err(format!("failed to commit ref transaction: {e}")));
        }
    }
    Ok(Ok(()))
}

/// `repo_read_index()` followed by `write_in_core_index_as_tree()`: the tree the
/// current index would produce, which is HEAD's tree plus whatever is staged.
/// `None` is git's `unable to read index`.
///
/// The tree is written into the object database, as git's is — comparing it to
/// HEAD's tree id is exactly `repo_index_has_changes()`'s question, and an
/// unmerged index has no tree at all, which git also treats as a failure to
/// write one.
fn staged_tree(repo: &gix::Repository) -> Option<gix::hash::ObjectId> {
    let index = repo.index_or_empty().ok()?;
    tree_of_index(repo, &index)
}

/// `write_in_core_index_as_tree()` for any index, wherever it was read from.
///
/// `None` when the index holds a conflict — git has no tree for one either — or
/// when an entry's mode has no tree equivalent.
fn tree_of_index(
    repo: &gix::Repository,
    index: &gix::index::File,
) -> Option<gix::hash::ObjectId> {
    let backing = index.path_backing();
    let mut editor = gix::objs::tree::Editor::new(
        gix::objs::Tree::empty(),
        &repo.objects,
        repo.object_hash(),
    );
    for entry in index.entries() {
        if entry.stage() != gix::index::entry::Stage::Unconflicted {
            return None;
        }
        let mode = entry.mode.to_tree_entry_mode()?;
        editor
            .upsert(
                entry
                    .path_in(backing)
                    .split(|&b| b == b'/')
                    .map(gix::bstr::ByteSlice::as_bstr),
                mode.kind(),
                entry.id,
            )
            .ok()?;
    }
    editor
        .write(|tree| repo.write_object(tree).map(|id| id.detach()))
        .ok()
}

/// Outcome of option parsing: either the options, or an exit status to return
/// after git's diagnostic has already been written.
enum Parsed {
    Opts(Box<Opts>),
    Exit(ExitCode),
}

/// Parse one subcommand's arguments with git's option table for that subcommand.
///
/// The first non-option argument is `<commit>`. For `split`, any further
/// arguments (with or without a preceding `--`) are pathspecs; for `fixup` and
/// `reword` a second positional is a usage error, reported by the caller via
/// the single-revision check.
fn parse(sub: Sub, args: &[String]) -> Result<Parsed> {
    let mut opts = Opts {
        dry_run: false,
        head_only: false,
        reedit_message: false,
        empty: EmptyAction::Drop,
        rev: None,
        pathspecs: Vec::new(),
    };
    let mut positionals: Vec<String> = Vec::new();
    let mut no_more_opts = false;

    // Report an unknown option exactly as git's parse-options does: the offending
    // name without its leading dashes, then the subcommand's full usage.
    let unknown = |name: &str| -> Parsed {
        eprint!("error: unknown option `{name}'\n{}\n", sub.usage());
        Parsed::Exit(ExitCode::from(EXIT_USAGE))
    };

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            positionals.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            i += 1;
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, so the test happens before the `=` split below. This
        // table has no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the
        // same block `-h` prints.
        if a == "--help-all" {
            println!("{}", sub.usage());
            std::io::stdout().flush()?;
            return Ok(Parsed::Exit(ExitCode::from(EXIT_USAGE)));
        }

        let (name, value) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (a, None),
        };

        // Pull `--opt <v>` when `--opt=<v>` was not used.
        let take = |i: &mut usize| -> Option<String> {
            match value {
                Some(v) => Some(v.to_string()),
                None => {
                    *i += 1;
                    args.get(*i).cloned()
                }
            }
        };

        match name {
            "-h" | "--help" => {
                println!("{}", sub.usage());
                std::io::stdout().flush()?;
                return Ok(Parsed::Exit(ExitCode::from(EXIT_USAGE)));
            }
            "-n" | "--dry-run" => opts.dry_run = true,
            "--no-dry-run" => opts.dry_run = false,
            "--update-refs" => {
                let Some(v) = take(&mut i) else {
                    eprint!("error: option `update-refs' requires a value\n{}\n", sub.usage());
                    return Ok(Parsed::Exit(ExitCode::from(EXIT_USAGE)));
                };
                match v.as_str() {
                    "branches" => opts.head_only = false,
                    "head" => opts.head_only = true,
                    _ => {
                        eprintln!("error: update-refs expects one of 'branches' or 'head'");
                        return Ok(Parsed::Exit(ExitCode::from(EXIT_USAGE)));
                    }
                }
            }
            "--reedit-message" if sub == Sub::Fixup => opts.reedit_message = true,
            "--no-reedit-message" if sub == Sub::Fixup => opts.reedit_message = false,
            "--empty" if sub == Sub::Fixup => {
                let Some(v) = take(&mut i) else {
                    eprint!("error: option `empty' requires a value\n{}\n", sub.usage());
                    return Ok(Parsed::Exit(ExitCode::from(EXIT_USAGE)));
                };
                opts.empty = match v.as_str() {
                    "drop" => EmptyAction::Drop,
                    "keep" => EmptyAction::Keep,
                    "abort" => EmptyAction::Abort,
                    other => {
                        eprintln!(
                            "fatal: unrecognized '--empty=' action '{other}'; \
                             valid values are \"drop\", \"keep\", and \"abort\"."
                        );
                        return Ok(Parsed::Exit(ExitCode::from(EXIT_FATAL)));
                    }
                };
            }
            _ => {
                // git strips the leading dashes but keeps any `=<value>` suffix,
                // so report the whole argument, not the split-off name.
                return Ok(unknown(a.trim_start_matches('-')));
            }
        }
        i += 1;
    }

    let mut positionals = positionals.into_iter();
    opts.rev = positionals.next();
    let rest: Vec<String> = positionals.collect();
    match sub {
        // `split` takes trailing pathspecs; the other two take nothing further,
        // and a second positional trips the single-revision check.
        Sub::Split => opts.pathspecs = rest,
        _ if !rest.is_empty() => opts.rev = None,
        _ => {}
    }

    Ok(Parsed::Opts(Box::new(opts)))
}
