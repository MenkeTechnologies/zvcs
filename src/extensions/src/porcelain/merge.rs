//! `git merge` — fast-forward, `--no-ff` over a fast-forwardable history,
//! `--abort` and `--quit`.
//!
//! What is served natively via the vendored gitoxide crates:
//!
//! * A fast-forward merge: the ref being merged is a descendant of the current
//!   `HEAD` (their merge-base is `HEAD` itself). The branch `HEAD` points to is
//!   advanced (or `HEAD` itself on a detached head), and the clean worktree +
//!   index are moved to the new tree.
//! * `--no-ff` over that same fast-forwardable history. The merged tree is then
//!   exactly the tree of the ref being merged — when the merge-base *is* our
//!   own commit, the three-way merge of every path resolves to theirs — so the
//!   merge commit is written directly with no three-way machinery involved.
//! * A real merge of diverged histories (with or without `--no-ff`), via the
//!   shared three-way merge in [`crate::merge_apply`]: `Auto-merging`/`CONFLICT`
//!   reporting, a clean two-parent merge commit, or — on conflict —
//!   `MERGE_HEAD`/`MERGE_MSG` plus the conflicted index and worktree markers, then
//!   `Automatic merge failed; fix conflicts and then commit the result.` (exit 1).
//! * `--abort` / `--quit`: `--quit` drops the in-progress merge state files;
//!   `--abort` additionally restores the index and the merge-affected worktree
//!   paths to `HEAD`, as `git reset --merge` does.
//!
//! Also served, as faithful ports of git's behaviour:
//!
//! * `--squash`/`--no-squash`: fold the merge into the worktree/index without a
//!   commit or ref move, writing `SQUASH_MSG` (a port of `squash_message()`,
//!   including the `git log`-medium body).
//! * `--commit`/`--no-commit`: `--no-commit` records `MERGE_HEAD`/`MERGE_MODE`/
//!   `MERGE_MSG` and stops with `Automatic merge went well; stopped before
//!   committing as requested`, leaving `git commit` (or `--continue`) to finish.
//! * `--continue`: finalize a resolved, staged in-progress merge.
//! * `-s ours` (and `-s ort`/`octopus`): `ours` records every head as a parent
//!   but keeps our tree verbatim.
//! * `--allow-unrelated-histories`: merge with an empty base tree; without it,
//!   `fatal: refusing to merge unrelated histories` (exit 128).
//! * `--signoff`, `-F`/`--file`, `--cleanup=<mode>`, `-q`/`--quiet`,
//!   `-v`/`--verbose`, and `--no-verify` (bypassing the `pre-merge-commit` and
//!   `commit-msg` hooks).
//!
//! What is refused or deferred rather than faked:
//!
//! * `-s recursive`/`resolve`/`subtree`: distinct conflict-resolution engines
//!   that are not vendored, refused rather than aliased onto `ort`.
//! * `-X`/`--strategy-option`, `--log[=<n>]`/`--no-log`, `--autostash`: these
//!   need substrate not reachable from this file (blob-merge options threaded
//!   through `merge_apply::three_way_merge`, the `fmt-merge-msg` shortlog
//!   builder, and stash create/apply helpers respectively); left rejected.
//! * `-e`/`--edit` (interactive editor), `-S`/`--gpg-sign`,
//!   `--verify-signatures` (no signing/verification driver).
//!
//! Known fidelity gaps, stated rather than hidden: the diffstat is computed
//! with rename detection off, while `git merge` enables it, so a merge that
//! renames a file reports it as a delete plus a create instead of a `rename`
//! summary line; diffstat column widths measure Unicode scalar values rather
//! than terminal columns; `--verbose`'s extra stderr diagnostics are not
//! emitted; and a `pre-merge-commit` hook that edits the index is not reflected
//! in the committed tree (the pre-computed merge tree is committed).

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stage, Stat};
use gix::object::tree::diff::{Action, Change as TreeChange};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// The mutually exclusive top-level modes of `git merge`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Merge,
    Abort,
    Quit,
    Continue,
}

/// How the fast-forward question is answered, mirroring git's `fast_forward`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ff {
    Allow,
    Never,
    Only,
}

/// The merge strategy selected by `-s`/`--strategy`. Only the strategies the
/// vendored primitives implement byte-for-byte are represented; the remaining
/// git strategies (`recursive`, `resolve`, `subtree`) are refused rather than
/// aliased onto `ort`, since their conflict resolution genuinely differs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Strategy {
    /// The default three-way merge (git's `ort`).
    Ort,
    /// `-s ours`: record every head as a parent but keep our tree verbatim.
    Ours,
}

/// `--cleanup=<mode>` — how the commit message is stripped, a port of git's
/// `cleanup_mode` / `strbuf_stripspace`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cleanup {
    /// `whitespace` when a message is supplied without an editor (merge's default).
    Default,
    Verbatim,
    Whitespace,
    Strip,
    Scissors,
}

/// Everything the argument loop gathers for a real merge, so the merge helpers
/// take one struct rather than a growing parameter list.
struct Opts {
    ff: Ff,
    /// Whether `--no-ff` was passed explicitly (needed for the `--squash`
    /// incompatibility check, which git keys off the literal flag).
    no_ff_given: bool,
    show_stat: bool,
    /// `-m`/`--message` or `-F`/`--file` contents (the latter read eagerly).
    message: Option<String>,
    squash: bool,
    /// `--commit`/`--no-commit` as given; `None` leaves the default (`!squash`).
    commit: Option<bool>,
    /// `--commit` was given explicitly (for the `--squash` incompatibility check).
    commit_given: bool,
    signoff: bool,
    allow_unrelated: bool,
    no_verify: bool,
    quiet: bool,
    cleanup: Cleanup,
    strategy: Strategy,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            ff: Ff::Allow,
            no_ff_given: false,
            show_stat: true,
            message: None,
            squash: false,
            commit: None,
            commit_given: false,
            signoff: false,
            allow_unrelated: false,
            no_verify: false,
            quiet: false,
            cleanup: Cleanup::Default,
            strategy: Strategy::Ort,
        }
    }
}

pub fn merge(args: &[String]) -> Result<ExitCode> {
    let mut op = Op::Merge;
    let mut opts = Opts::default();
    let mut refs: Vec<String> = Vec::new();
    // A pending `-F`/`--file` read, resolved after parsing so the diagnostic
    // order matches git (options first, file open second).
    let mut file: Option<String> = None;

    // git reads merge.ff and merge.stat as the defaults; the CLI flags below
    // override them (`--ff`/`--no-ff`/`--ff-only`, `--stat`/`--no-stat`).
    // merge.suppressDest is consulted later, in `dest_suppressed`, when the
    // default merge message's title is composed.
    if let Ok(repo) = gix::discover(".") {
        let snap = repo.config_snapshot();
        match snap.string("merge.ff").map(|v| v.to_string().to_ascii_lowercase()).as_deref() {
            Some("only") => opts.ff = Ff::Only,
            Some("false" | "no" | "off" | "0") => opts.ff = Ff::Never,
            Some(_) => opts.ff = Ff::Allow, // true/yes/on/1/valueless → allow
            None => {}
        }
        if snap.boolean("merge.stat") == Some(false) {
            opts.show_stat = false;
        }
    }

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--abort" => op = Op::Abort,
            "--quit" => op = Op::Quit,
            "--continue" => op = Op::Continue,
            "--ff" => opts.ff = Ff::Allow,
            "--no-ff" => {
                opts.ff = Ff::Never;
                opts.no_ff_given = true;
            }
            "--ff-only" => opts.ff = Ff::Only,
            "--stat" | "--summary" => opts.show_stat = true,
            "--no-stat" | "--no-summary" | "-n" => opts.show_stat = false,
            "--squash" => opts.squash = true,
            "--no-squash" => opts.squash = false,
            "--commit" => {
                opts.commit = Some(true);
                opts.commit_given = true;
            }
            "--no-commit" => opts.commit = Some(false),
            "--signoff" => opts.signoff = true,
            "--no-signoff" => opts.signoff = false,
            "--allow-unrelated-histories" => opts.allow_unrelated = true,
            "--no-allow-unrelated-histories" => opts.allow_unrelated = false,
            "--no-verify" => opts.no_verify = true,
            "--verify" => opts.no_verify = false,
            // Verbosity: git keeps a signed level; only quiet has an observable
            // effect on stdout (it silences the summary/diffstat). `--verbose`'s
            // extra diagnostics go to stderr and are not reproduced.
            "-q" | "--quiet" => opts.quiet = true,
            "-v" | "--verbose" => opts.quiet = false,
            // We never open an editor, so `--no-edit` is the natural state; `-e`
            // is deferred (see the module docs).
            "--no-edit" => {}
            "-m" | "--message" => {
                i += 1;
                match args.get(i) {
                    Some(m) => opts.message = Some(m.clone()),
                    None => {
                        eprintln!("error: option `{a}' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if a.starts_with("--message=") => opts.message = Some(a["--message=".len()..].to_string()),
            _ if a.len() > 2 && a.starts_with("-m") && !a.starts_with("--") => {
                opts.message = Some(a[2..].to_string())
            }
            "-F" | "--file" => {
                i += 1;
                match args.get(i) {
                    Some(p) => file = Some(p.clone()),
                    None => {
                        eprintln!("error: option `{a}' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if a.starts_with("--file=") => file = Some(a["--file=".len()..].to_string()),
            _ if a.len() > 2 && a.starts_with("-F") && !a.starts_with("--") => {
                file = Some(a[2..].to_string())
            }
            "--cleanup" => {
                i += 1;
                match args.get(i).and_then(|v| parse_cleanup(v)) {
                    Some(mode) => opts.cleanup = mode,
                    None => {
                        let bad = args.get(i).map(String::as_str).unwrap_or("");
                        eprintln!("fatal: Invalid cleanup mode {bad}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            _ if a.starts_with("--cleanup=") => match parse_cleanup(&a["--cleanup=".len()..]) {
                Some(mode) => opts.cleanup = mode,
                None => {
                    eprintln!("fatal: Invalid cleanup mode {}", &a["--cleanup=".len()..]);
                    return Ok(ExitCode::from(128));
                }
            },
            "-s" | "--strategy" => {
                i += 1;
                match args.get(i).map(String::as_str).map(resolve_strategy) {
                    Some(Ok(s)) => opts.strategy = s,
                    Some(Err(code)) => return Ok(code),
                    None => {
                        eprintln!("error: option `{a}' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if a.starts_with("--strategy=") => match resolve_strategy(&a["--strategy=".len()..]) {
                Ok(s) => opts.strategy = s,
                Err(code) => return Ok(code),
            },
            _ if a.len() > 2 && a.starts_with("-s") && !a.starts_with("--") => {
                match resolve_strategy(&a[2..]) {
                    Ok(s) => opts.strategy = s,
                    Err(code) => return Ok(code),
                }
            }
            _ if a.len() > 1 && a.starts_with('-') => {
                anyhow::bail!("unsupported flag {a}")
            }
            _ => refs.push(a.to_string()),
        }
        i += 1;
    }

    // `-F <path>` — read now, after option parsing. `-` and an empty value are
    // stdin, matching git's `read_from_file`/`fix_filename`.
    if let Some(path) = file {
        let data = if path == "-" || path.is_empty() {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut buf)?;
            buf
        } else {
            match std::fs::read(&path) {
                Ok(buf) => buf,
                Err(e) => {
                    eprintln!("fatal: could not open '{path}' for reading: {}", strerror(&e));
                    return Ok(ExitCode::from(128));
                }
            }
        };
        opts.message = Some(String::from_utf8_lossy(&data).into_owned());
    }

    match op {
        // git: `--abort`/`--quit`/`--continue` expect no arguments.
        Op::Abort | Op::Quit | Op::Continue if !refs.is_empty() => {
            let which = match op {
                Op::Abort => "--abort",
                Op::Quit => "--quit",
                _ => "--continue",
            };
            eprintln!("fatal: {which} expects no arguments");
            Ok(ExitCode::from(129))
        }
        Op::Abort => abort(),
        Op::Quit => quit(),
        Op::Continue => continue_merge(&opts),
        Op::Merge => {
            // git's `builtin/merge.c` incompatibility checks, keyed off the literal
            // flags. `--squash` cannot fast-forward, so it clashes with `--no-ff`,
            // and it never commits, so it clashes with `--commit`.
            if opts.squash && opts.commit_given {
                eprintln!("fatal: options '--squash' and '--commit.' cannot be used together");
                return Ok(ExitCode::from(128));
            }
            if opts.squash && opts.no_ff_given {
                eprintln!("fatal: options '--squash' and '--no-ff.' cannot be used together");
                return Ok(ExitCode::from(128));
            }
            do_merge(&refs, &opts)
        }
    }
}

// ---------------------------------------------------------------------------
// --abort / --quit
// ---------------------------------------------------------------------------

/// The state files `remove_merge_branch_state()` (branch.c) unlinks.
const MERGE_STATE_FILES: &[&str] = &["MERGE_HEAD", "MERGE_RR", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"];

/// The extra state `remove_branch_state()` unlinks on top of the merge state;
/// `git merge --abort` reaches it by running `git reset --merge`.
const BRANCH_STATE_FILES: &[&str] = &["SQUASH_MSG", "CHERRY_PICK_HEAD", "REVERT_HEAD"];

fn remove_merge_state(git_dir: &Path, and_branch_state: bool) {
    for name in MERGE_STATE_FILES {
        let _ = std::fs::remove_file(git_dir.join(name));
    }
    if and_branch_state {
        for name in BRANCH_STATE_FILES {
            let _ = std::fs::remove_file(git_dir.join(name));
        }
        let _ = std::fs::remove_dir_all(git_dir.join("sequencer"));
    }
}

/// `git merge --quit`: forget the in-progress merge, leaving index and worktree
/// exactly as they are.
fn quit() -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    remove_merge_state(repo.git_dir(), false);
    Ok(ExitCode::SUCCESS)
}

/// `git merge --abort`: `git reset --merge` plus dropping the merge state.
///
/// The reset is confined to the paths the merge touched — every path that has a
/// conflicted stage, or whose index entry disagrees with `HEAD` — so unrelated
/// local modifications and untracked files survive, as they do under git.
fn abort() -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    if !repo.git_dir().join("MERGE_HEAD").exists() {
        eprintln!("fatal: There is no merge to abort (MERGE_HEAD missing).");
        return Ok(ExitCode::from(128));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let head = repo.head()?;
    let head_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    let head_tree = repo.find_object(head_id)?.peel_to_tree()?.id;

    let old_index = repo.index_or_load_from_head()?.into_owned();
    let should_interrupt = AtomicBool::new(false);
    update_worktree(&repo, &old_index, head_tree, &should_interrupt)?;

    // git's `reset_refs()` records the pre-reset HEAD in ORIG_HEAD.
    set_orig_head(&repo, head_id)?;
    remove_merge_state(repo.git_dir(), true);

    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

fn do_merge(refs: &[String], opts: &Opts) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    if repo.git_dir().join("MERGE_HEAD").exists() {
        eprintln!("fatal: You have not concluded your merge (MERGE_HEAD exists).");
        eprintln!("Please, commit your changes before you merge.");
        return Ok(ExitCode::from(128));
    }

    if refs.is_empty() {
        // git dies here rather than defaulting to anything.
        eprintln!("fatal: No remote for the current branch.");
        return Ok(ExitCode::from(128));
    }

    // Current HEAD state. An unborn branch has no commit to fast-forward from;
    // a real merge into it would be a checkout, which is out of scope.
    let head = repo.head()?;
    if head.is_unborn() {
        anyhow::bail!("cannot merge into an unborn branch");
    }
    let local_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    // Owned branch name when attached; `None` when detached.
    let branch: Option<FullName> = head.referent_name().map(std::borrow::ToOwned::to_owned);
    // The ref to move: the attached branch, or HEAD itself when detached. Both
    // are direct (non-symbolic) refs here, so `deref` is false either way.
    let name: FullName = match &branch {
        Some(b) => b.clone(),
        None => "HEAD"
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
    };

    // Resolve every ref to merge and peel it to a commit (tags included).
    let mut targets: Vec<ObjectId> = Vec::with_capacity(refs.len());
    for spec in refs {
        let id = repo.rev_parse_single(spec.as_str())?.object()?.peel_to_commit()?.id;
        targets.push(id);
    }

    // `-s ours`: every head becomes a parent while our tree is kept verbatim.
    // Handles any number of heads and never fast-forwards.
    if opts.strategy == Strategy::Ours {
        return merge_ours(&repo, name, branch.as_ref(), local_id, &targets, refs, opts);
    }

    // More than one head, default strategy → octopus.
    if refs.len() > 1 {
        return do_octopus(&repo, refs, &targets, local_id, branch.as_ref(), name, opts);
    }

    let spec = refs[0].as_str();
    let target_id = targets[0];

    // merge-base analysis. An empty set of merge bases means unrelated histories,
    // which git refuses without `--allow-unrelated-histories`.
    let bases = repo.merge_bases_many(local_id, &[target_id])?;
    if bases.is_empty() && !opts.allow_unrelated {
        eprintln!("fatal: refusing to merge unrelated histories");
        return Ok(ExitCode::from(128));
    }
    if bases.iter().any(|b| b.detach() == target_id) {
        // Target already reachable from HEAD (or identical). git checks this
        // before it consults --no-ff, so --no-ff does not force a commit here.
        if !opts.quiet {
            println!("Already up to date.");
        }
        return Ok(ExitCode::SUCCESS);
    }
    // Fast-forwardable exactly when HEAD is one of the merge bases.
    let diverged = !bases.iter().any(|b| b.detach() == local_id);
    if diverged && opts.ff == Ff::Only {
        eprintln!("fatal: Not possible to fast-forward, aborting.");
        return Ok(ExitCode::from(128));
    }

    // From here on we mutate a ref, the index and the worktree. Serialize the
    // whole read-modify-write through the repo coordinator (a no-op if no
    // daemon is running), matching the zsync/zbump write path.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Never clobber uncommitted work.
    if repo.is_dirty()? {
        anyhow::bail!("worktree has uncommitted changes; refusing to merge");
    }

    let old_index = repo.index_or_load_from_head()?.into_owned();
    let head_tree = repo.find_object(local_id)?.peel_to_tree()?.id;
    let target_tree = repo.find_object(target_id)?.peel_to_tree()?.id;
    let should_interrupt = AtomicBool::new(false);
    let message = merge_message(&repo, spec, branch.as_ref(), opts.message.clone())?;

    // Diverged histories: a genuine three-way merge (`ort` strategy) of HEAD and
    // the target against their merge base (an empty tree for unrelated histories).
    // On a clean merge the finish step commits/squashes/records per the options;
    // on conflict we record MERGE_HEAD/MERGE_MSG and stop, exactly as git does.
    if diverged {
        // `git`'s recursive base for the three-way; the empty tree stands in for an
        // unrelated history (`--allow-unrelated-histories`), which has no base.
        let base_tree = if bases.is_empty() {
            gix::ObjectId::empty_tree(repo.object_hash())
        } else {
            let base = repo.merge_base(local_id, target_id)?.detach();
            repo.find_object(base)?.peel_to_tree()?.id
        };
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BStr::new(b"merged common ancestors")),
            current: Some(BStr::new(b"HEAD")),
            other: Some(BStr::new(spec.as_bytes())),
        };
        let applied = crate::merge_apply::three_way_merge(
            &repo,
            base_tree,
            head_tree,
            target_tree,
            &old_index,
            labels,
            &should_interrupt,
        )?;
        let mut index = applied.index;
        index.write(Default::default())?;

        if applied.conflicts.is_empty() {
            return finalize_clean(
                &repo,
                name,
                local_id,
                &[target_id],
                message,
                applied.tree_id,
                head_tree,
                opts,
                "ort",
                spec,
            );
        }

        // Conflicts: record the in-progress merge and stop with git's message.
        set_orig_head(&repo, local_id)?;
        let git_dir = repo.git_dir();
        std::fs::write(git_dir.join("MERGE_HEAD"), format!("{target_id}\n"))?;
        std::fs::write(git_dir.join("MERGE_MODE"), merge_mode(opts.ff))?;
        let mut merge_msg = message.into_bytes();
        merge_msg.extend_from_slice(b"\n# Conflicts:\n");
        for path in &applied.conflicts {
            merge_msg.extend_from_slice(b"#\t");
            merge_msg.extend_from_slice(&path[..]);
            merge_msg.push(b'\n');
        }
        std::fs::write(git_dir.join("MERGE_MSG"), &merge_msg)?;
        if !opts.quiet {
            println!("Automatic merge failed; fix conflicts and then commit the result.");
        }
        return Ok(ExitCode::from(1));
    }

    // `--no-ff` over a fast-forwardable history: the merge-base is our own commit,
    // so a three-way merge of every path resolves to theirs — the merged tree is
    // exactly the target's tree. Sync the worktree, then finish as a merge commit.
    if opts.ff == Ff::Never {
        update_worktree(&repo, &old_index, target_tree, &should_interrupt)?;
        return finalize_clean(
            &repo,
            name,
            local_id,
            &[target_id],
            message,
            target_tree,
            head_tree,
            opts,
            "ort",
            spec,
        );
    }

    // Pure fast-forward territory. `--squash` fast-forwards the *content* but does
    // not move the ref: git updates the worktree, prints the fast-forward summary,
    // then the squash notice and writes SQUASH_MSG.
    if opts.squash {
        update_worktree(&repo, &old_index, target_tree, &should_interrupt)?;
        if !opts.quiet {
            println!(
                "Updating {}..{}",
                local_id.to_hex_with_len(7),
                target_id.to_hex_with_len(7)
            );
            println!("Fast-forward");
            if opts.show_stat {
                print!("{}", diffstat(&repo, head_tree, target_tree)?);
            }
        }
        write_squash_msg(&repo, &[target_id], local_id)?;
        if !opts.quiet {
            println!("Squash commit -- not updating HEAD");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // Normal fast-forward. `--no-commit` does not stop a fast-forward (there is no
    // merge commit to stop before), matching git.
    set_orig_head(&repo, local_id)?;
    advance(&repo, name, local_id, target_id, format!("merge {spec}: Fast-forward"))?;
    update_worktree(&repo, &old_index, target_tree, &should_interrupt)?;
    if !opts.quiet {
        println!(
            "Updating {}..{}",
            local_id.to_hex_with_len(7),
            target_id.to_hex_with_len(7)
        );
        println!("Fast-forward");
        if opts.show_stat {
            print!("{}", diffstat(&repo, head_tree, target_tree)?);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The clean-merge finish shared by the diverged, `--no-ff`, and `-s ours` paths:
/// records `ORIG_HEAD`, then squashes, stops before committing, or writes the
/// merge commit, honouring `--signoff`, `--cleanup`, `--no-verify` and `--quiet`.
/// `merged_tree` is the already-computed result tree (its worktree/index are
/// assumed synced by the caller); `head_tree` feeds the diffstat.
#[allow(clippy::too_many_arguments)]
fn finalize_clean(
    repo: &gix::Repository,
    name: FullName,
    local_id: ObjectId,
    targets: &[ObjectId],
    message: String,
    merged_tree: ObjectId,
    head_tree: ObjectId,
    opts: &Opts,
    strategy_name: &str,
    spec_label: &str,
) -> Result<ExitCode> {
    set_orig_head(repo, local_id)?;
    let do_commit = opts.commit.unwrap_or(!opts.squash);

    // `--squash`: no commit, no ref move, no MERGE_HEAD — just SQUASH_MSG.
    if opts.squash {
        if !opts.quiet {
            println!("Automatic merge went well; stopped before committing as requested");
        }
        write_squash_msg(repo, targets, local_id)?;
        if !opts.quiet {
            println!("Squash commit -- not updating HEAD");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // `--no-commit`: leave the merge in progress for `git commit` to finalize.
    if !do_commit {
        let git_dir = repo.git_dir();
        let mut merge_head = String::new();
        for t in targets {
            merge_head.push_str(&format!("{t}\n"));
        }
        std::fs::write(git_dir.join("MERGE_HEAD"), merge_head)?;
        std::fs::write(git_dir.join("MERGE_MODE"), merge_mode(opts.ff))?;
        std::fs::write(git_dir.join("MERGE_MSG"), &message)?;
        if !opts.quiet {
            println!("Automatic merge went well; stopped before committing as requested");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // `pre-merge-commit` runs before the commit; a non-zero exit vetoes it. The
    // hook's own output (inherited on stderr) is the whole diagnostic, as in git.
    if !opts.no_verify && !crate::hooks::run(repo, "pre-merge-commit", &[], None)? {
        return Ok(ExitCode::from(1));
    }

    let mut msg = message;
    if opts.signoff {
        append_signoff(repo, &mut msg)?;
    }
    let comment = comment_char(repo);
    msg = cleanup_message(&msg, opts.cleanup, &comment);

    // `commit-msg` gets the message file (via MERGE_MSG) and may rewrite it.
    if !opts.no_verify {
        let msg_path = repo.git_dir().join("MERGE_MSG");
        std::fs::write(&msg_path, &msg)?;
        let arg = msg_path.to_string_lossy().into_owned();
        if !crate::hooks::run(repo, "commit-msg", &[&arg], None)? {
            return Ok(ExitCode::from(1));
        }
        msg = std::fs::read_to_string(&msg_path)?;
        let _ = std::fs::remove_file(&msg_path);
    }

    let author = repo
        .author()
        .ok_or_else(|| anyhow::anyhow!("author identity is not configured"))??;
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
    let mut parents: Vec<ObjectId> = Vec::with_capacity(targets.len() + 1);
    parents.push(local_id);
    parents.extend_from_slice(targets);
    let commit = gix::objs::Commit {
        message: msg.into(),
        tree: merged_tree,
        author: author.to_owned()?,
        committer: committer.to_owned()?,
        encoding: None,
        parents: parents.into_iter().collect(),
        extra_headers: Default::default(),
    };
    let new_id = repo.write_object(&commit)?.detach();
    advance(
        repo,
        name,
        local_id,
        new_id,
        format!("merge {spec_label}: Merge made by the '{strategy_name}' strategy."),
    )?;
    if !opts.quiet {
        println!("Merge made by the '{strategy_name}' strategy.");
        if opts.show_stat {
            print!("{}", diffstat(repo, head_tree, merged_tree)?);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `-s ours`: record every head as a parent, keep our tree verbatim. Never
/// fast-forwards; already up to date only when every head is reachable from HEAD.
fn merge_ours(
    repo: &gix::Repository,
    name: FullName,
    branch: Option<&FullName>,
    local_id: ObjectId,
    targets: &[ObjectId],
    refs: &[String],
    opts: &Opts,
) -> Result<ExitCode> {
    let mut all_reachable = true;
    for t in targets {
        let bases = repo.merge_bases_many(local_id, &[*t])?;
        if !bases.iter().any(|b| b.detach() == *t) {
            all_reachable = false;
            break;
        }
    }
    if all_reachable {
        if !opts.quiet {
            println!("Already up to date.");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    if repo.is_dirty()? {
        anyhow::bail!("worktree has uncommitted changes; refusing to merge");
    }

    let head_tree = repo.find_object(local_id)?.peel_to_tree()?.id;
    let old_index = repo.index_or_load_from_head()?.into_owned();
    let should_interrupt = AtomicBool::new(false);
    // Our tree is unchanged; sync the index (a no-op checkout).
    update_worktree(repo, &old_index, head_tree, &should_interrupt)?;

    let message = match &opts.message {
        Some(m) => {
            let mut m = m.clone();
            if !m.ends_with('\n') {
                m.push('\n');
            }
            m
        }
        None if refs.len() == 1 => merge_message(repo, refs[0].as_str(), branch, None)?,
        None => {
            let specs: Vec<&str> = refs.iter().map(String::as_str).collect();
            octopus_message(&specs)
        }
    };
    let spec_label = refs.join(" ");
    finalize_clean(
        repo,
        name,
        local_id,
        targets,
        message,
        head_tree,
        head_tree,
        opts,
        "ours",
        &spec_label,
    )
}

/// `git merge <a> <b> [<c>...]` — the octopus strategy: fold each head into the
/// result with a three-way merge, then write one commit carrying every head as a
/// parent. Any head that cannot merge cleanly fails the octopus (git does not
/// resolve conflicts under octopus), leaving the conflicted state and `MERGE_HEAD`.
fn do_octopus(
    repo: &gix::Repository,
    refs: &[String],
    targets: &[ObjectId],
    local_id: ObjectId,
    _branch: Option<&FullName>,
    name: FullName,
    opts: &Opts,
) -> Result<ExitCode> {
    // Every head, resolved by the caller; pair each with its spec for messages.
    let heads: Vec<(String, ObjectId)> = refs
        .iter()
        .cloned()
        .zip(targets.iter().copied())
        .collect();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    if repo.is_dirty()? {
        anyhow::bail!("worktree has uncommitted changes; refusing to merge");
    }

    let mut cur_index = repo.index_or_load_from_head()?.into_owned();
    let mut mrt = repo.find_object(local_id)?.peel_to_tree()?.id; // merge result tree
    // `MRC` (git's merge-result-commit list): the parents of the eventual commit.
    // It starts as HEAD but, while still a single commit, is *replaced* by a head
    // that fast-forwards it (so `merge a b` where main is an ancestor of `a` yields
    // parents `[a, b]`, not `[main, a, b]`).
    let mut mrc: Vec<ObjectId> = vec![local_id];
    let should_interrupt = AtomicBool::new(false);

    for (spec, head_id) in &heads {
        let common = if mrc.len() == 1 {
            repo.merge_base(mrc[0], *head_id)?.detach()
        } else {
            repo.merge_base(local_id, *head_id)?.detach()
        };
        if common == *head_id {
            if !opts.quiet {
                println!("Already up to date with {spec}");
            }
            continue;
        }
        let head_tree = repo.find_object(*head_id)?.peel_to_tree()?.id;

        // Fast-forward: while MRC is still a single commit and it is the merge base,
        // git advances the base line to this head rather than recording a parent.
        if mrc.len() == 1 && common == mrc[0] {
            update_worktree(repo, &cur_index, head_tree, &should_interrupt)?;
            cur_index = repo.index_from_tree(&head_tree)?;
            mrt = head_tree;
            mrc = vec![*head_id];
            continue;
        }

        let base_tree = repo.find_object(common)?.peel_to_tree()?.id;
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BStr::new(b"merged common ancestors")),
            current: Some(BStr::new(b"HEAD")),
            other: Some(BStr::new(spec.as_bytes())),
        };
        let applied = crate::merge_apply::three_way_merge(
            repo,
            base_tree,
            mrt,
            head_tree,
            &cur_index,
            labels,
            &should_interrupt,
        )?;
        cur_index = applied.index;
        cur_index.write(Default::default())?;

        if !applied.conflicts.is_empty() {
            // Octopus aborts on the first conflicting head, leaving the conflicted
            // worktree/index and MERGE_HEAD listing every head, as git does.
            let git_dir = repo.git_dir();
            let mut merge_head = String::new();
            for (_, h) in &heads {
                merge_head.push_str(&format!("{h}\n"));
            }
            std::fs::write(git_dir.join("MERGE_HEAD"), merge_head)?;
            std::fs::write(git_dir.join("MERGE_MODE"), b"")?;
            set_orig_head(repo, local_id)?;
            if !opts.quiet {
                println!("Automatic merge failed; fix conflicts and then commit the result.");
            }
            return Ok(ExitCode::from(1));
        }
        mrt = applied.tree_id;
        mrc.push(*head_id);
    }

    // Nothing merged: every head was already reachable.
    if mrc.len() == 1 && mrc[0] == local_id {
        if !opts.quiet {
            println!("Already up to date.");
        }
        return Ok(ExitCode::SUCCESS);
    }
    // Everything collapsed onto one line via fast-forward — a plain fast-forward,
    // not an octopus commit.
    if mrc.len() == 1 {
        set_orig_head(repo, local_id)?;
        advance(
            repo,
            name,
            local_id,
            mrc[0],
            format!("merge {}: Fast-forward", refs.join(" ")),
        )?;
        if !opts.quiet {
            println!("Fast-forward");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // The default octopus message, or the explicit `-m`/`-F` text.
    let specs: Vec<&str> = refs.iter().map(String::as_str).collect();
    let message = match &opts.message {
        Some(m) => {
            let mut m = m.clone();
            if !m.ends_with('\n') {
                m.push('\n');
            }
            m
        }
        None => octopus_message(&specs),
    };
    // The finish (squash / stop-before-commit / commit) is shared with the two-head
    // paths; every merged head becomes a parent (`mrc` minus HEAD).
    let extra_parents: Vec<ObjectId> = mrc.iter().copied().filter(|p| *p != local_id).collect();
    finalize_clean(
        repo,
        name,
        local_id,
        &extra_parents,
        message,
        mrt,
        mrt, // no diffstat basis distinct from the octopus tree; git prints none
        opts,
        "octopus",
        &refs.join(" "),
    )
}

/// The default octopus commit subject: `Merge branches 'a', 'b' and 'c'`.
fn octopus_message(refs: &[&str]) -> String {
    let quoted: Vec<String> = refs.iter().map(|r| format!("'{r}'")).collect();
    let joined = match quoted.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, init)) => format!("{} and {}", init.join(", "), last),
        None => String::new(),
    };
    format!("Merge branches {joined}\n")
}

// ---------------------------------------------------------------------------
// Option plumbing: strategy, cleanup, squash message, signoff, --continue
// ---------------------------------------------------------------------------

/// `MERGE_MODE`'s body: git writes `no-ff` (no trailing newline) when the merge
/// must not fast-forward, and an empty file otherwise.
fn merge_mode(ff: Ff) -> &'static [u8] {
    if ff == Ff::Never {
        b"no-ff"
    } else {
        b""
    }
}

/// Map a `--cleanup=<mode>` value to its mode, or `None` for an invalid one.
fn parse_cleanup(value: &str) -> Option<Cleanup> {
    Some(match value {
        "default" => Cleanup::Default,
        "verbatim" => Cleanup::Verbatim,
        "whitespace" => Cleanup::Whitespace,
        "strip" => Cleanup::Strip,
        "scissors" => Cleanup::Scissors,
        _ => return None,
    })
}

/// Resolve a `-s`/`--strategy` value. Only `ort` and `ours` map to a real merge
/// here; `octopus` folds onto the default path (which already selects the octopus
/// engine for multiple heads). `recursive`/`resolve`/`subtree` are genuine git
/// strategies with distinct conflict resolution that is not vendored, so they are
/// refused rather than silently aliased onto `ort`. An unknown name reproduces
/// git's `Could not find merge strategy` diagnostic.
fn resolve_strategy(name: &str) -> std::result::Result<Strategy, ExitCode> {
    match name {
        "ort" | "octopus" => Ok(Strategy::Ort),
        "ours" => Ok(Strategy::Ours),
        "recursive" | "resolve" | "subtree" => {
            eprintln!("merge: strategy '{name}' is not supported by this build (use 'ort' or 'ours')");
            Err(ExitCode::from(128))
        }
        _ => {
            eprintln!("Could not find merge strategy '{name}'.");
            eprintln!("Available strategies are: octopus ours recursive resolve subtree.");
            Err(ExitCode::from(128))
        }
    }
}

/// `core.commentChar` for cleanup, defaulting to `#` (and treating `auto` and an
/// empty value as `#`). The full multi-valued `commentChar`/`commentString`
/// interleaving lives in `fmt-merge-msg`; a single character covers the merge
/// message paths.
fn comment_char(repo: &gix::Repository) -> String {
    match repo.config_snapshot().string("core.commentChar") {
        Some(v) => {
            let s = v.to_string();
            if s.is_empty() || s == "auto" {
                "#".to_string()
            } else {
                s
            }
        }
        None => "#".to_string(),
    }
}

/// Append git's `Signed-off-by:` trailer (from the committer identity) to a merge
/// message, inserting a blank separator line when the message does not already end
/// with one. This is the common title-only case; a message that already ends in a
/// trailer block is not de-duplicated (git's `append_signoff` scans for that).
fn append_signoff(repo: &gix::Repository, msg: &mut String) -> Result<()> {
    let sig = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
    let trailer = format!(
        "Signed-off-by: {} <{}>",
        sig.name.to_str_lossy(),
        sig.email.to_str_lossy()
    );
    if !msg.ends_with('\n') {
        msg.push('\n');
    }
    if !msg.ends_with("\n\n") {
        msg.push('\n');
    }
    msg.push_str(&trailer);
    msg.push('\n');
    Ok(())
}

/// git's `cleanup_message()` / `strbuf_stripspace()` for the modes merge exposes.
fn cleanup_message(input: &str, mode: Cleanup, comment: &str) -> String {
    match mode {
        Cleanup::Verbatim => input.to_string(),
        Cleanup::Scissors => {
            let marker = format!("{comment} ------------------------ >8 ------------------------");
            stripspace(&input[..scissors_cut(input, &marker)], None)
        }
        Cleanup::Strip => stripspace(input, Some(comment)),
        // merge's default when a message is supplied without an editor is
        // `whitespace`: strip trailing whitespace and blank runs, keep comments.
        Cleanup::Whitespace | Cleanup::Default => stripspace(input, None),
    }
}

/// The byte offset of the scissors line (`# ----- >8 -----`), or the input length
/// when it is absent; everything from there on is dropped.
fn scissors_cut(input: &str, marker: &str) -> usize {
    let mut pos = 0;
    for line in input.split_inclusive('\n') {
        if line.strip_suffix('\n').unwrap_or(line) == marker {
            return pos;
        }
        pos += line.len();
    }
    input.len()
}

/// Port of `strbuf_stripspace()`: rtrim every line, drop leading/trailing blank
/// lines and collapse consecutive blank lines to one; when `comment` is set,
/// lines starting with it are removed entirely.
fn stripspace(input: &str, comment: Option<&str>) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 1);
    let mut empties = 0usize;
    let mut rest = bytes;

    while !rest.is_empty() {
        let len = match rest.iter().position(|&b| b == b'\n') {
            Some(offset) => offset + 1,
            None => rest.len(),
        };
        let (line, tail) = rest.split_at(len);
        rest = tail;

        if let Some(c) = comment {
            if line.starts_with(c.as_bytes()) {
                continue;
            }
        }

        let mut end = line.len();
        while end > 0 && line[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end == 0 {
            empties += 1;
            continue;
        }
        if empties > 0 && !out.is_empty() {
            out.push(b'\n');
        }
        empties = 0;
        out.extend_from_slice(&line[..end]);
        out.push(b'\n');
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Write `SQUASH_MSG`, a port of `squash_message()` (builtin/merge.c): the header
/// line, then, for every non-merge commit reachable from `targets` but not from
/// `head` (newest first), a `commit <id>` line and its `git log`-medium body.
fn write_squash_msg(repo: &gix::Repository, targets: &[ObjectId], head: ObjectId) -> Result<()> {
    let mut out = String::from("Squashed commit of the following:\n");
    let walk = repo
        .rev_walk(targets.iter().copied())
        .with_hidden([head])
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    for info in walk.all()? {
        let commit = info?.object()?;
        // `rev.ignore_merges = 1`: merge commits are skipped.
        if commit.parent_ids().count() > 1 {
            continue;
        }
        out.push('\n');
        out.push_str(&format!("commit {}\n", commit.id));
        let author = commit.author()?;
        out.push_str(&format!(
            "Author: {} <{}>\n",
            author.name.to_str_lossy(),
            author.email.to_str_lossy()
        ));
        let date = author.time()?.format_or_unix(gix::date::time::format::DEFAULT);
        out.push_str(&format!("Date:   {date}\n\n"));
        // medium format indents every message line by four spaces, empty ones too.
        let raw = commit.message_raw()?;
        let body = raw.to_str_lossy();
        for line in body.trim_end_matches('\n').split('\n') {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
    std::fs::write(repo.git_dir().join("SQUASH_MSG"), out)?;
    Ok(())
}

/// Build a tree object from `index` (all stage-0 entries) and return its id, the
/// standard gix editor pass over the index in path order — the tree `--continue`
/// commits.
fn index_tree(repo: &gix::Repository, index: &gix::index::File) -> Result<ObjectId> {
    let backing = index.path_backing();
    let mut editor =
        gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, repo.object_hash());
    for entry in index.entries() {
        let path = entry.path_in(backing);
        let mode = entry
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| anyhow::anyhow!("index entry `{path}` has an unrepresentable mode"))?;
        editor.upsert(path.split(|&b| b == b'/').map(|c| c.as_bstr()), mode.kind(), entry.id)?;
    }
    Ok(editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?)
}

/// `git merge --continue`: finish a merge whose conflicts have been resolved and
/// staged, writing the merge commit from the current index and clearing the
/// in-progress state, exactly as `git commit` does when `MERGE_HEAD` is present.
fn continue_merge(opts: &Opts) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    let git_dir = repo.git_dir().to_owned();
    if !git_dir.join("MERGE_HEAD").exists() {
        eprintln!("fatal: There is no merge in progress (MERGE_HEAD missing).");
        return Ok(ExitCode::from(128));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Refuse while the index still carries conflicted (stage 1/2/3) entries.
    let index = repo.open_index()?;
    if index.entries().iter().any(|e| e.stage() != Stage::Unconflicted) {
        eprintln!("error: Committing is not possible because you have unmerged files.");
        eprintln!("hint: Fix them up in the work tree, and then use 'git add/rm <file>'");
        eprintln!("hint: as appropriate to mark resolution and make a commit.");
        eprintln!("fatal: Exiting because of an unresolved conflict.");
        return Ok(ExitCode::from(128));
    }

    let head = repo.head()?;
    if head.is_unborn() {
        anyhow::bail!("cannot conclude a merge on an unborn branch");
    }
    let local_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();

    // Parents: HEAD first, then every id listed in MERGE_HEAD.
    let mut parents: Vec<ObjectId> = vec![local_id];
    let merge_head = std::fs::read_to_string(git_dir.join("MERGE_HEAD"))?;
    for line in merge_head.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        parents.push(
            ObjectId::from_hex(line.as_bytes())
                .map_err(|e| anyhow::anyhow!("invalid id in MERGE_HEAD: {e}"))?,
        );
    }

    // Message from MERGE_MSG, comment lines (the `# Conflicts:` block) stripped as
    // git's finalize cleanup does.
    let raw = std::fs::read_to_string(git_dir.join("MERGE_MSG")).unwrap_or_default();
    let comment = comment_char(&repo);
    let mut msg = cleanup_message(&raw, Cleanup::Strip, &comment);
    if opts.signoff {
        append_signoff(&repo, &mut msg)?;
    }
    if !opts.no_verify {
        let msg_path = git_dir.join("COMMIT_EDITMSG");
        std::fs::write(&msg_path, &msg)?;
        let arg = msg_path.to_string_lossy().into_owned();
        if !crate::hooks::run(&repo, "commit-msg", &[&arg], None)? {
            return Ok(ExitCode::from(1));
        }
        msg = std::fs::read_to_string(&msg_path)?;
    }
    let subject = msg.lines().next().unwrap_or("").to_string();

    let tree_id = index_tree(&repo, &index)?;
    let commit_id = repo.commit("HEAD", &msg, tree_id, parents)?;

    remove_merge_state(&git_dir, true);
    let _ = crate::hooks::run(&repo, "post-merge", &["0"], None);

    if !opts.quiet {
        let short = commit_id.shorten_or_id();
        let branch_label = match repo.head_name()? {
            Some(name) => name.shorten().to_string(),
            None => "detached HEAD".to_string(),
        };
        println!("[{branch_label} {short}] {subject}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `strerror(errno)`: the bare message, without Rust's ` (os error <n>)` tail.
fn strerror(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

/// Move `name` from `old` to `new`, writing `reflog` as the reflog message.
fn advance(
    repo: &gix::Repository,
    name: FullName,
    old: ObjectId,
    new: ObjectId,
    reflog: String,
) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: reflog.into(),
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(old)),
            new: Target::Object(new),
        },
        name,
        deref: false,
    })?;
    Ok(())
}

/// Point `ORIG_HEAD` at `id`, as git does before it moves `HEAD`.
fn set_orig_head(repo: &gix::Repository, id: ObjectId) -> Result<()> {
    let name: FullName = "ORIG_HEAD"
        .try_into()
        .map_err(|e| anyhow::anyhow!("invalid ref name ORIG_HEAD: {e}"))?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "updating ORIG_HEAD".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(id),
        },
        name,
        deref: false,
    })?;
    Ok(())
}

/// The merge commit's message.
///
/// Port of `merge_name()` (builtin/merge.c) feeding `fmt_merge_msg_title()`
/// (fmt-merge-msg.c): the ref is described by the category it resolved into,
/// and ` into <branch>` is appended unless the current branch matches a
/// `merge.suppressDest` glob (defaulting to `main`/`master`), see
/// `dest_suppressed`.
fn merge_message(
    repo: &gix::Repository,
    spec: &str,
    branch: Option<&FullName>,
    explicit: Option<String>,
) -> Result<String> {
    if let Some(mut m) = explicit {
        if !m.ends_with('\n') {
            m.push('\n');
        }
        return Ok(m);
    }

    // gix resolves a partial name through the same rule list git's `dwim_ref`
    // uses ("", tags, heads, remotes), so the full name it lands on is the
    // category git would have reported. An invalid ref name (`main~2`) is not
    // an error here, it just means no ref matched.
    let described = match repo.try_find_reference(spec) {
        Ok(Some(r)) => {
            let full = r.name().as_bstr().to_str_lossy().into_owned();
            if full.starts_with("refs/heads/") {
                format!("branch '{spec}'")
            } else if full.starts_with("refs/tags/") {
                format!("tag '{spec}'")
            } else if full.starts_with("refs/remotes/") {
                format!("remote-tracking branch '{spec}'")
            } else {
                format!("commit '{spec}'")
            }
        }
        _ => match early_part_of_branch(repo, spec) {
            Some(d) => d,
            None => format!("commit '{spec}'"),
        },
    };

    let current = match branch {
        Some(b) => b.shorten().to_str_lossy().into_owned(),
        None => "HEAD".to_string(),
    };
    let mut out = format!("Merge {described}");
    if !dest_suppressed(repo, &current) {
        out.push_str(&format!(" into {current}"));
    }
    out.push('\n');
    Ok(out)
}

/// Port of `dest_suppressed()` and the default seeding in `fmt_merge_msg()`
/// (fmt-merge-msg.c): the merge title's ` into <branch>` is dropped when the
/// current branch matches any glob in `merge.suppressDest`, tested with
/// `wildmatch(pattern, branch, WM_PATHNAME)` — case-sensitive, and `*` does not
/// cross a `/`. The variable is multi-valued and accumulates in config order;
/// an empty value clears whatever was gathered so far. When the key is never
/// set at all, the list defaults to `main` then `master`.
fn dest_suppressed(repo: &gix::Repository, branch: &str) -> bool {
    let patterns = suppress_dest_patterns(repo);
    let value = branch.as_bytes().as_bstr();
    patterns
        .iter()
        .any(|p| gix::glob::wildmatch(p.as_bstr(), value, gix::glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL))
}

/// The accumulated `merge.suppressDest` pattern list, resolving git's
/// empty-value-clears rule and its `main`/`master` default when unset.
///
/// Fidelity gap: a *valueless* `merge.suppressDest` (no `=`) makes git die with
/// `config_error_nonbool` at config-parse time; gix reports it as an empty
/// value, indistinguishable from `suppressDest=`, so here it clears the list
/// rather than aborting. This is a config-subsystem limitation shared across
/// keys, not specific to the merge logic.
fn suppress_dest_patterns(repo: &gix::Repository) -> Vec<BString> {
    match repo.config_snapshot().raw_values("merge.suppressDest") {
        Ok(values) => {
            let mut list: Vec<BString> = Vec::new();
            for v in values {
                if v.is_empty() {
                    list.clear();
                } else {
                    list.push(v);
                }
            }
            list
        }
        // `suppress_dest_pattern_seen` never set → the built-in default.
        Err(_) => vec![BString::from("main"), BString::from("master")],
    }
}

/// `merge_name()`'s second attempt: `<name>^^^` or `<name>~<number>` naming a
/// point inside an existing branch. The suffix is stripped and, if a branch by
/// the remaining name exists, that branch is what git reports — tagged
/// `(early part)` whenever the suffix actually walks back at least one commit.
fn early_part_of_branch(repo: &gix::Repository, spec: &str) -> Option<String> {
    let bytes = spec.as_bytes();
    let mut len = 0usize;
    let mut early = false;

    let carets = bytes.iter().rev().take_while(|&&b| b == b'^').count();
    if carets > 0 && carets < bytes.len() {
        len = carets;
        early = true;
    } else if carets == 0 {
        if let Some(tilde) = spec.rfind('~') {
            let digits = &bytes[tilde + 1..];
            if digits.iter().all(u8::is_ascii_digit) {
                len = 1 + digits.len();
                // "name~" means "name~1"; "name~0" walks back nothing.
                early = digits.is_empty() || digits.iter().any(|&b| b != b'0');
            }
        }
    }

    if len == 0 || len >= bytes.len() {
        return None;
    }
    let stripped = &spec[..bytes.len() - len];
    match repo.try_find_reference(format!("refs/heads/{stripped}").as_str()) {
        Ok(Some(_)) => Some(format!(
            "branch '{stripped}'{}",
            if early { " (early part)" } else { "" }
        )),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Worktree + index transition
// ---------------------------------------------------------------------------

/// Move the worktree and its index from the state captured in `old` to
/// `new_tree`, writing only the paths that changed.
///
/// Ported from the `zsync` reconcile path: the change set is derived by
/// comparing the old index against the new tree-index (file-level granularity),
/// added/modified files are checked out via `gix-worktree-state`, removed files
/// are deleted, and the new index is written reusing prior stats for unchanged
/// entries so a later status stays cheap.
///
/// A path carrying any conflicted stage in `old` is always treated as changed:
/// its worktree file holds conflict markers rather than any indexed blob, so it
/// must be rewritten even when one of its stages happens to match the new tree.
fn update_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_tree: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    // Index the current entries by path for change detection and stat reuse.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    let mut conflicted: HashSet<BString> = HashSet::new();
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing).to_owned();
            if e.stage_raw() != 0 {
                conflicted.insert(path.clone());
            }
            old_map.insert(path, (e.id, e.mode, e.stat));
        }
    }

    // Full target index (all new-tree entries) — what is finally written; a
    // reduced copy of only the changed entries is what is checked out.
    let mut new_index = repo.index_from_tree(&new_tree)?;
    let mut subset = repo.index_from_tree(&new_tree)?;
    subset.remove_entries(|_, path, entry| {
        let path = path.to_owned();
        if conflicted.contains(&path) {
            return false;
        }
        match old_map.get(&path) {
            // Present before with identical content and mode → unchanged, drop it.
            Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
            // Absent before → an addition, keep it.
            None => false,
        }
    });

    // Write the changed files into the worktree, overwriting in place.
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    crate::worktree::checkout_subset(
        &mut subset,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        should_interrupt,
        opts,
    )?;

    // Remove files present before but not in the new tree.
    let new_paths: HashSet<BString> = {
        let backing = new_index.path_backing();
        new_index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing);
            if !new_paths.contains(&path.to_owned()) {
                if let Some(full) = repo.workdir_path(path) {
                    let _ = std::fs::remove_file(full);
                }
            }
        }
    }

    // Fresh stats produced by the checkout for the changed entries.
    let mut subset_stats: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            subset_stats.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

    // Changed entries get their fresh stat; unchanged entries reuse the old one.
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode && !conflicted.contains(&path) {
                    e.stat = *stat;
                }
            }
        }
    }

    // Drop any stale cache-tree extension before persisting.
    new_index.remove_tree();
    new_index.write(Default::default())?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Diffstat and summary (diff.c)
// ---------------------------------------------------------------------------

/// One diffstat row.
struct StatRow {
    /// Quoted path, as git's `fill_print_name` produces it.
    name: String,
    /// Inserted lines, or the new blob's byte size when `binary`.
    added: u64,
    /// Deleted lines, or the old blob's byte size when `binary`.
    deleted: u64,
    binary: bool,
}

/// git's `decimal_width`.
fn decimal_width(mut n: u64) -> i64 {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

/// git's `scale_linear`: at least one column for any non-zero change.
fn scale_linear(it: i64, width: i64, max_change: i64) -> i64 {
    if it == 0 {
        return 0;
    }
    1 + (it * (width - 1) / max_change)
}

/// Display width in Unicode scalar values (git measures terminal columns; wide
/// characters are counted as 1 here, see the module note).
fn display_width(s: &str) -> i64 {
    s.chars().count() as i64
}

/// git's `quote_c_style` as applied to diff path names.
fn quote_path(path: &[u8]) -> String {
    let needs = path
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return String::from_utf8_lossy(path).into_owned();
    }
    let mut out = String::from("\"");
    for &b in path {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// The `DIFF_FORMAT_DIFFSTAT | DIFF_FORMAT_SUMMARY` block `finish()`
/// (builtin/merge.c) prints after a merge, rendered as one string.
fn diffstat(repo: &gix::Repository, old_tree: ObjectId, new_tree: ObjectId) -> Result<String> {
    let (rows, summary) = collect(repo, old_tree, new_tree)?;
    let mut out = String::new();
    emit_stats(&mut out, &rows);
    for line in &summary {
        out.push_str(&format!(" {line}\n"));
    }
    Ok(out)
}

/// Walk the tree-to-tree diff once, producing the stat rows and the summary
/// lines, both ordered by path as git's tree recursion orders them.
fn collect(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
) -> Result<(Vec<StatRow>, Vec<String>)> {
    let old = repo.find_tree(old_tree)?;
    let new = repo.find_tree(new_tree)?;
    let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;

    // Per row: path (for ordering), display name, line counts, and — when the
    // blob diff declined because a side is binary — the ids whose sizes git
    // reports instead. Sizes are looked up after the walk so the callback stays
    // infallible.
    let mut raw: Vec<(BString, String, Option<(u64, u64)>, Option<ObjectId>, Option<ObjectId>)> =
        Vec::new();
    let mut summary: Vec<(BString, String)> = Vec::new();

    let mut platform = old.changes()?;
    platform.options(|opts| {
        opts.track_rewrites(None);
    });
    let _rewrites = platform.for_each_to_obtain_tree(&new, |change| {
        let path: BString = change.location().to_owned();
        let display = quote_path(&path[..]);
        let (old_id, new_id) = match change {
            TreeChange::Addition { entry_mode, id, .. } => {
                summary.push((
                    path.clone(),
                    format!("create mode {:06o} {display}", entry_mode.value()),
                ));
                (None, Some(id.detach()))
            }
            TreeChange::Deletion { entry_mode, id, .. } => {
                summary.push((
                    path.clone(),
                    format!("delete mode {:06o} {display}", entry_mode.value()),
                ));
                (Some(id.detach()), None)
            }
            TreeChange::Modification {
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
                ..
            } => {
                if previous_entry_mode.value() != entry_mode.value() {
                    summary.push((
                        path.clone(),
                        format!(
                            "mode change {:06o} => {:06o} {display}",
                            previous_entry_mode.value(),
                            entry_mode.value()
                        ),
                    ));
                }
                (Some(previous_id.detach()), Some(id.detach()))
            }
            // Rewrites cannot occur: rename tracking is off above.
            TreeChange::Rewrite { source_id, id, .. } => (Some(source_id.detach()), Some(id.detach())),
        };

        let counts = change
            .diff(&mut resource_cache)
            .ok()
            .and_then(|mut p| p.line_counts().ok())
            .flatten()
            .map(|c| (u64::from(c.insertions), u64::from(c.removals)));
        raw.push((path, display, counts, old_id, new_id));

        resource_cache.clear_resource_cache_keep_allocation();
        Ok::<_, std::convert::Infallible>(Action::Continue(()))
    })?;
    drop(platform);

    let blob_size = |id: Option<ObjectId>| -> Result<u64> {
        match id {
            // git's `diff_filespec_size` of an invalid filespec is 0.
            None => Ok(0),
            Some(id) => Ok(repo.find_object(id)?.data.len() as u64),
        }
    };

    let mut rows: Vec<(BString, StatRow)> = Vec::with_capacity(raw.len());
    for (path, name, counts, old_id, new_id) in raw {
        let row = match counts {
            Some((added, deleted)) => StatRow { name, added, deleted, binary: false },
            None => StatRow {
                name,
                added: blob_size(new_id)?,
                deleted: blob_size(old_id)?,
                binary: true,
            },
        };
        rows.push((path, row));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));
    summary.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((
        rows.into_iter().map(|(_, r)| r).collect(),
        summary.into_iter().map(|(_, l)| l).collect(),
    ))
}

/// Port of `show_stats()` (diff.c) at merge's `stat_width = -1`, which resolves
/// to `term_columns()` — 80 whenever stdout is not a terminal and `COLUMNS` is
/// unset, as it is under the parity harness. Followed by
/// `print_stat_summary_inserts_deletes()`.
fn emit_stats(out: &mut String, files: &[StatRow]) {
    if files.is_empty() {
        return;
    }

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    let mut bin_width: i64 = 0;
    let mut number_width: i64 = 0;
    for f in files {
        max_len = max_len.max(display_width(&f.name));
        if f.binary {
            // "Bin XXX -> YYY bytes"
            bin_width = bin_width.max(14 + decimal_width(f.added) + decimal_width(f.deleted));
            // Display change counts aligned with "Bin".
            number_width = 3;
            continue;
        }
        max_change = max_change.max((f.added + f.deleted) as i64);
    }

    let mut width: i64 = 80;
    number_width = number_width.max(decimal_width(max_change as u64));

    // Guarantee 3/8*16==6 for the graph part and 5/8*16==10 for the filename.
    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    let mut graph_width = if max_change + 4 > bin_width { max_change } else { bin_width - 4 };
    let mut name_width = max_len;
    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = width * 3 / 8 - number_width - 6;
            if graph_width < 6 {
                graph_width = 6;
            }
        }
        if name_width > width - number_width - 6 - graph_width {
            name_width = width - number_width - 6 - graph_width;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    for f in files {
        // Scale the filename: elide the head, then resume at a path separator.
        let mut len = name_width;
        let mut prefix = "";
        let mut name: &str = &f.name;
        if name_width < display_width(name) {
            prefix = "...";
            len -= 3;
            if len < 0 {
                len = 0;
            }
            let mut name_len = display_width(name);
            let mut off = 0;
            while name_len > len && off < name.len() {
                let c = name[off..]
                    .chars()
                    .next()
                    .expect("off stays on a char boundary");
                off += c.len_utf8();
                name_len -= 1;
            }
            name = &name[off..];
            if let Some(slash) = name.find('/') {
                name = &name[slash..];
            }
        }
        let padding = (len - display_width(name)).max(0) as usize;
        let nw = number_width as usize;

        if f.binary {
            out.push_str(&format!(" {prefix}{name}{:padding$} | {:>nw$}", "", "Bin"));
            if f.added == 0 && f.deleted == 0 {
                out.push('\n');
            } else {
                out.push_str(&format!(" {} -> {} bytes\n", f.deleted, f.added));
            }
            continue;
        }

        let total = f.added + f.deleted;
        let mut add = f.added as i64;
        let mut del = f.deleted as i64;
        if graph_width <= max_change && max_change > 0 {
            let mut sum = scale_linear(add + del, graph_width, max_change);
            if sum < 2 && add > 0 && del > 0 {
                sum = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = sum - add;
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = sum - del;
            }
        }

        out.push_str(&format!(
            " {prefix}{name}{:padding$} | {:>nw$}{}",
            "",
            total,
            if total > 0 { " " } else { "" },
        ));
        for _ in 0..add.max(0) {
            out.push('+');
        }
        for _ in 0..del.max(0) {
            out.push('-');
        }
        out.push('\n');
    }

    // Binary rows count as changed files but contribute no insertions/deletions.
    let mut adds: u64 = 0;
    let mut dels: u64 = 0;
    for f in files {
        if !f.binary {
            adds += f.added;
            dels += f.deleted;
        }
    }

    let n = files.len();
    let mut line = format!(" {n} {} changed", if n == 1 { "file" } else { "files" });
    if adds > 0 || dels == 0 {
        line.push_str(&format!(
            ", {adds} {}",
            if adds == 1 { "insertion(+)" } else { "insertions(+)" }
        ));
    }
    if dels > 0 || adds == 0 {
        line.push_str(&format!(
            ", {dels} {}",
            if dels == 1 { "deletion(-)" } else { "deletions(-)" }
        ));
    }
    out.push_str(&line);
    out.push('\n');
}
