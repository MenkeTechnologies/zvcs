//! `git switch` — move `HEAD` to a branch, backed by the vendored gitoxide
//! ref store and worktree-state checkout.
//!
//! Supported invocations (the common forms):
//!   * `git switch <branch>`                    → attach `HEAD` to an existing
//!                                                local branch and update the
//!                                                worktree/index to its tip.
//!   * `git switch -c|--create <new> [<start>]` → create `refs/heads/<new>` at
//!                                                `<start>` (default `HEAD`),
//!                                                attach `HEAD` to it, and update
//!                                                the worktree if `<start>`
//!                                                differs from the current tip.
//!
//! Semantics matched to stock `git switch` for these cases: `Already on '<b>'`
//! when the target is the current branch (exit 0), `Switched to branch '<b>'` /
//! `Switched to a new branch '<b>'` on success, and `a branch named '<b>'
//! already exists` / `invalid reference: <b>` on the corresponding failures.
//!
//! Deferred (bailed with a precise reason, never faked): carrying local
//! modifications across a real worktree change (a clean worktree is required
//! whenever files must change), `--detach`, `-C`/`--force-create`, `--orphan`,
//! `--track`, `--merge`, `--guess`, and the `-`/`@{-N}` previous-branch shorthand.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

pub fn switch(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    // Classify arguments into the `-c` flag and positional names, giving a
    // precise reason for the known-but-unsupported flags rather than a generic
    // rejection so callers know exactly what is missing.
    let mut create = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-c" | "--create" => create = true,
            "--" => {
                for rest in &args[i + 1..] {
                    positionals.push(rest.as_str());
                }
                break;
            }
            "-" => bail!("switching to the previous branch ('-') is not supported"),
            "-C" | "--force-create" => {
                bail!("force-create (-C) is not supported; delete the branch and use -c")
            }
            "-d" | "--detach" => bail!("switching to a detached HEAD (--detach) is not supported"),
            "--orphan" => bail!("creating an orphan branch (--orphan) is not supported"),
            "--merge" | "-m" => bail!("three-way merge on switch (--merge) is not supported"),
            "-t" | "--track" | "--no-track" | "--guess" | "--no-guess" => {
                bail!("upstream tracking/guessing ({a}) is not supported")
            }
            "-f" | "--force" | "--discard-changes" => {
                bail!("discarding local changes on switch ({a}) is not supported")
            }
            _ if a.starts_with('-') => bail!("unsupported flag {a:?}"),
            _ => positionals.push(a),
        }
        i += 1;
    }

    if positionals.is_empty() {
        bail!("missing branch or commit argument");
    }

    if create {
        switch_create(&repo, &positionals)
    } else {
        switch_existing(&repo, &positionals)
    }
}

/// `git switch <branch>` — attach `HEAD` to an existing local branch.
fn switch_existing(repo: &gix::Repository, positionals: &[&str]) -> Result<ExitCode> {
    if positionals.len() > 1 {
        bail!("only one reference expected");
    }
    let branch = positionals[0];
    let full = format!("refs/heads/{branch}");
    let full_name: FullName = full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{branch}': {e}"))?;

    // Already on the requested branch → git prints and exits 0 without touching
    // the worktree.
    if repo.head_name()?.as_ref().map(|n| n.as_bstr()) == Some(full_name.as_bstr()) {
        println!("Already on '{branch}'");
        return Ok(ExitCode::SUCCESS);
    }

    // Resolve the target tip (read-only, cheap error path before any lock).
    let mut reference = match repo.try_find_reference(full.as_str())? {
        Some(r) => r,
        None => bail!("invalid reference: {branch}"),
    };
    let target = reference.into_fully_peeled_id()?.detach();

    let mut head = repo.head()?;
    let current_commit = head.try_peel_to_id()?.map(|id| id.detach());
    let from_desc = describe_head(repo)?;

    // A worktree/index rewrite is only needed when the target commit differs
    // from where HEAD currently resolves; two branches on the same commit (or an
    // unborn HEAD) still just re-point HEAD.
    let needs_worktree = current_commit != Some(target);

    // Serialize the whole read-modify-write through the repo coordinator, held
    // across the ref move and worktree update.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if needs_worktree {
        if repo.is_dirty()? {
            bail!("your local changes would be overwritten; commit or stash them (dirty-worktree switch is unsupported)");
        }
        let old = repo.index_or_load_from_head()?.into_owned();
        update_clean_worktree(repo, &old, target)?;
    }

    attach_head(repo, &full_name, &from_desc, branch)?;
    println!("Switched to branch '{branch}'");
    Ok(ExitCode::SUCCESS)
}

/// `git switch -c <new> [<start>]` — create a local branch and attach `HEAD`.
fn switch_create(repo: &gix::Repository, positionals: &[&str]) -> Result<ExitCode> {
    if positionals.len() > 2 {
        bail!("too many arguments (expected <new-branch> [<start-point>])");
    }
    let branch = positionals[0];
    let start = positionals.get(1).copied();
    let full = format!("refs/heads/{branch}");

    // Validate as a local branch name before any store access.
    if gix::validate::reference::branch_name(BStr::new(full.as_bytes())).is_err() {
        bail!("'{branch}' is not a valid branch name");
    }
    let full_name: FullName = full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{branch}': {e}"))?;

    // Current tip and the start-point the new branch is created at.
    let mut head = repo.head()?;
    let current_commit = head.try_peel_to_id()?.map(|id| id.detach());
    let start_commit = match start {
        Some(s) => repo.rev_parse_single(BStr::new(s))?.detach(),
        None => current_commit
            .ok_or_else(|| anyhow!("cannot create a branch from an unborn HEAD without a start-point"))?,
    };
    let from_desc = describe_head(repo)?;

    // A worktree rewrite is needed only when the start-point differs from the
    // current tip; `switch -c foo` at HEAD is a pure re-label that keeps any
    // local modifications intact.
    let needs_worktree = current_commit != Some(start_commit);

    // Serialize the create + attach (+ optional worktree update).
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if repo.try_find_reference(full.as_str())?.is_some() {
        bail!("a branch named '{branch}' already exists");
    }

    // Capture the old index (mirroring the clean worktree) before mutating, only
    // when a worktree rewrite is actually required.
    let old = if needs_worktree {
        if repo.is_dirty()? {
            bail!("your local changes would be overwritten; commit or stash them (dirty-worktree switch is unsupported)");
        }
        Some(repo.index_or_load_from_head()?.into_owned())
    } else {
        None
    };

    // Create the branch ref, then (if needed) move the worktree/index, then HEAD.
    repo.reference(
        full.as_str(),
        start_commit,
        PreviousValue::MustNotExist,
        format!("branch: Created from {from_desc}"),
    )?;
    if let Some(old) = old {
        update_clean_worktree(repo, &old, start_commit)?;
    }

    attach_head(repo, &full_name, &from_desc, branch)?;
    println!("Switched to a new branch '{branch}'");
    Ok(ExitCode::SUCCESS)
}

/// Point `HEAD` symbolically at `branch_ref`, writing a checkout reflog entry
/// (`checkout: moving from <from> to <to>`), exactly as git records it.
fn attach_head(
    repo: &gix::Repository,
    branch_ref: &FullName,
    from_desc: &str,
    to_short: &str,
) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("checkout: moving from {from_desc} to {to_short}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch_ref.clone()),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    Ok(())
}

/// Human description of where `HEAD` currently is, for the reflog `from` field:
/// the short branch name when attached, else the abbreviated commit, else the
/// literal `HEAD` for an unborn ref.
fn describe_head(repo: &gix::Repository) -> Result<String> {
    if let Some(name) = repo.head_name()? {
        return Ok(name.shorten().to_string());
    }
    let mut head = repo.head()?;
    match head.try_peel_to_id()? {
        Some(id) => Ok(id.shorten_or_id().to_string()),
        None => Ok("HEAD".to_string()),
    }
}

/// Move a clean worktree and its index to the tree of commit `new_commit`,
/// writing only the files that changed.
///
/// Ported from the `zsync` reconcile path: the change set is derived by
/// comparing the current index (mirroring the clean worktree) against the target
/// tree, so additions/modifications are checked out through `gix-worktree-state`
/// (correct mode/symlink/filter handling) and removals are deleted from disk.
/// The rewritten index reuses prior stats for unchanged entries and fresh stats
/// for changed ones, keeping a later status check cheap.
fn update_clean_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_commit: ObjectId,
) -> Result<()> {
    let should_interrupt = AtomicBool::new(false);

    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    let new_tree_id = repo.find_object(new_commit)?.peel_to_tree()?.id;

    // Index by path of the current entries, for change detection and stat reuse.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    // The full target index (all new-tree entries) — what is eventually written.
    let mut new_index = repo.index_from_tree(&new_tree_id)?;

    // A second copy reduced to just the changed entries (added, or content/mode
    // differs from `old`) — the subset actually checked out to the worktree.
    let mut subset = repo.index_from_tree(&new_tree_id)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        None => false,
    });

    // Write the changed files into the (clean) worktree, overwriting in place.
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    gix::worktree::state::checkout(
        &mut subset,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        &should_interrupt,
        opts,
    )?;

    // Remove files present in the old tree but not the new one.
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

    // Fill in the target index stats: changed entries get their fresh stat;
    // unchanged entries reuse the previous stat.
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode {
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
