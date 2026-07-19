//! `git checkout` — the legacy combined verb: switch branches (or detach at a
//! commit) *and* restore paths, backed by the vendored gitoxide crates so tools
//! on PATH see the same `.git`, index, and worktree.
//!
//! Supported invocations (the common forms):
//!   * `git checkout <branch>`                 → switch to an existing local branch
//!   * `git checkout <commit-ish>`             → detach `HEAD` at a commit
//!   * `git checkout --detach <rev>`           → detach even when `<rev>` is a branch
//!   * `git checkout -b <name> [<start>]`      → create branch at `<start>` (default HEAD), switch
//!   * `git checkout -B <name> [<start>]`      → create-or-reset branch at `<start>`, switch
//!   * `git checkout [<tree-ish>] -- <path>…`  → restore paths (index+worktree from `<tree-ish>`; worktree-only from index when no tree-ish)
//!   * `git checkout <path>…`                  → restore paths from the index (bare pathspec form)
//!   * `-q`/`--quiet` suppress the transition messages
//!
//! Deviations (honest, conservative — never corrupting):
//!   * A branch/commit switch that changes the working tree requires a clean
//!     tracked worktree. Stock git also permits a switch when the dirty files do
//!     not collide with the diff between trees; that non-conflicting case is
//!     refused here (message names it) rather than risking an incorrect merge.
//!     Switches whose target tree equals the current tree (e.g. `-b` at HEAD, or
//!     two branches on the same commit) carry local changes and are never
//!     refused. Untracked files are ignored for the clean check, matching git.
//!   * The multi-line `advice.detachedHead` help block is not printed; the
//!     functional `HEAD is now at …` / `Previous HEAD position was …` lines are.
//!   * Pathspecs match literal files and directory prefixes (and `.`); general
//!     glob magic is left to the shell.
//!   * `-m`/`--merge`, `-p`/`--patch`, `--ours`/`--theirs`, `-t`/`--track`,
//!     `--orphan` are not supported and bail precisely.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

pub fn checkout(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    // --- Argument classification -------------------------------------------
    // `new_branch` is Some((name, reset_if_exists)) for -b / -B.
    let mut new_branch: Option<(String, bool)> = None;
    let mut detach = false;
    let mut quiet = false;
    let mut pre: Vec<&str> = Vec::new(); // positionals before `--`
    let mut post: Vec<&str> = Vec::new(); // pathspecs after `--`
    let mut has_dashdash = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if has_dashdash {
            post.push(a);
            i += 1;
            continue;
        }
        match a {
            "--" => has_dashdash = true,
            "-b" | "-B" => {
                let name = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("option '{a}' requires a value"))?;
                new_branch = Some((name.clone(), a == "-B"));
                i += 1;
            }
            "--detach" => detach = true,
            "-q" | "--quiet" => quiet = true,
            "-f" | "--force" => {} // accepted; a clean switch needs no forcing here
            "-m" | "--merge" => bail!("three-way merge on checkout (-m) is not supported"),
            "-p" | "--patch" => bail!("interactive patch checkout (-p) is not supported"),
            "--ours" | "--theirs" => bail!("conflict-side checkout (--ours/--theirs) is not supported"),
            "-t" | "--track" => bail!("upstream tracking setup (--track) is not supported"),
            "--orphan" => bail!("orphan branch creation (--orphan) is not supported"),
            _ if a.starts_with('-') && a.len() > 1 => bail!("unsupported flag {a:?}"),
            _ => pre.push(a),
        }
        i += 1;
    }

    // --- Dispatch -----------------------------------------------------------
    if let Some((name, reset)) = new_branch {
        if has_dashdash || !post.is_empty() {
            bail!("cannot combine branch creation (-b/-B) with path restore");
        }
        if pre.len() > 1 {
            bail!("too many start-points given for branch creation");
        }
        let start = pre.first().copied().unwrap_or("HEAD");
        return create_and_switch(&repo, &name, reset, start, quiet);
    }

    if has_dashdash {
        if post.is_empty() {
            bail!("you must specify path(s) to restore");
        }
        return match pre.len() {
            0 => restore_from_index(&repo, &post, false, quiet),
            1 => restore_from_tree(&repo, pre[0], &post, quiet),
            _ => bail!("only one <tree-ish> may precede `--`"),
        };
    }

    // No `--`, no -b/-B.
    if pre.is_empty() {
        bail!("nothing to checkout: specify a branch, commit, or path(s)");
    }

    // Single positional: prefer ref interpretation (branch → switch; else rev →
    // detach); fall back to a bare path restore from the index.
    if pre.len() == 1 {
        let spec = pre[0];
        let is_branch = repo
            .try_find_reference(format!("refs/heads/{spec}").as_str())?
            .is_some();
        if is_branch && !detach {
            return switch_to_branch(&repo, spec, quiet);
        }
        if let Ok(id) = repo.rev_parse_single(spec) {
            let commit = id.object()?.peel_to_commit()?;
            return detached_checkout(&repo, spec, commit, quiet);
        }
        // Not a ref/rev — treat as a path restore from the index (bare form).
        return restore_from_index(&repo, &pre, true, quiet);
    }

    // Multiple positionals, no `--`: if the first resolves to a tree-ish it is the
    // source and the rest are paths; otherwise all are paths from the index.
    if repo.rev_parse_single(pre[0]).is_ok() {
        return restore_from_tree(&repo, pre[0], &pre[1..], quiet);
    }
    restore_from_index(&repo, &pre, true, quiet)
}

/// Switch `HEAD` to an existing local branch `spec`, updating the worktree when
/// the target tree differs from the current one.
fn switch_to_branch(repo: &gix::Repository, spec: &str, quiet: bool) -> Result<ExitCode> {
    // Already on it → no-op, matching git's "Already on 'x'".
    if let Some(cur) = repo.head_name()? {
        if cur.shorten().to_string() == spec {
            if !quiet {
                println!("Already on '{spec}'");
            }
            return Ok(ExitCode::SUCCESS);
        }
    }

    let commit = repo.rev_parse_single(spec)?.object()?.peel_to_commit()?;
    let target_tree = commit.tree_id()?.detach();

    let head = repo.head()?;
    let old_detached = head.is_detached();
    let old_id = head.id().map(|i| i.detach());
    let old_label = head_label(&head);
    let cur_tree = repo.head_tree_id_or_empty()?.detach();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if target_tree != cur_tree {
        ensure_clean(repo)?;
        update_worktree_to_tree(repo, target_tree)?;
    }

    let branch_full: FullName = format!("refs/heads/{spec}")
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{spec}': {e}"))?;
    set_head_symbolic(repo, branch_full, &format!("checkout: moving from {old_label} to {spec}"))?;

    if !quiet {
        if old_detached {
            if let Some(id) = old_id {
                let (abbrev, summary) = describe(repo, id)?;
                println!("Previous HEAD position was {abbrev} {summary}");
            }
        }
        println!("Switched to branch '{spec}'");
    }
    Ok(ExitCode::SUCCESS)
}

/// Detach `HEAD` at `commit`, updating the worktree when the target tree differs.
fn detached_checkout(
    repo: &gix::Repository,
    spec: &str,
    commit: gix::Commit<'_>,
    quiet: bool,
) -> Result<ExitCode> {
    let target_id = commit.id;
    let target_tree = commit.tree_id()?.detach();

    let head = repo.head()?;
    let old_detached = head.is_detached();
    let old_id = head.id().map(|i| i.detach());
    let old_label = head_label(&head);
    let cur_tree = repo.head_tree_id_or_empty()?.detach();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if target_tree != cur_tree {
        ensure_clean(repo)?;
        update_worktree_to_tree(repo, target_tree)?;
    }

    set_head_detached(
        repo,
        target_id,
        &format!("checkout: moving from {old_label} to {spec}"),
    )?;

    if !quiet {
        if old_detached {
            if let (Some(old), true) = (old_id, old_id != Some(target_id)) {
                let (abbrev, summary) = describe(repo, old)?;
                println!("Previous HEAD position was {abbrev} {summary}");
            }
        }
        let (abbrev, summary) = describe(repo, target_id)?;
        println!("HEAD is now at {abbrev} {summary}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Create (`-b`) or create-or-reset (`-B`) `refs/heads/<name>` at `start`, then
/// switch `HEAD` to it, updating the worktree when the tree changes.
fn create_and_switch(
    repo: &gix::Repository,
    name: &str,
    reset: bool,
    start: &str,
    quiet: bool,
) -> Result<ExitCode> {
    let full = format!("refs/heads/{name}");
    if gix::validate::reference::branch_name(BStr::new(full.as_bytes())).is_err() {
        bail!("'{name}' is not a valid branch name");
    }

    let commit = repo.rev_parse_single(start)?.object()?.peel_to_commit()?;
    let start_id = commit.id;
    let target_tree = commit.tree_id()?.detach();

    let head = repo.head()?;
    let old_detached = head.is_detached();
    let old_id = head.id().map(|i| i.detach());
    let old_label = head_label(&head);
    // Whether HEAD is already attached to the branch we're (re)creating.
    let already_on = head
        .referent_name()
        .map(|n| n.shorten().to_string() == name)
        .unwrap_or(false);
    let cur_tree = repo.head_tree_id_or_empty()?.detach();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let existed = repo.try_find_reference(full.as_str())?.is_some();
    if existed && !reset {
        bail!("a branch named '{name}' already exists");
    }

    if target_tree != cur_tree {
        ensure_clean(repo)?;
        update_worktree_to_tree(repo, target_tree)?;
    }

    let branch_full: FullName = full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{name}': {e}"))?;
    // Create fresh, or force-move an existing branch for -B.
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("branch: Created from {start}").into(),
            },
            expected: if existed {
                PreviousValue::Any
            } else {
                PreviousValue::MustNotExist
            },
            new: Target::Object(start_id),
        },
        name: branch_full.clone(),
        deref: false,
    })?;
    set_head_symbolic(repo, branch_full, &format!("checkout: moving from {old_label} to {name}"))?;

    if !quiet {
        // Reset-in-place (-B on the current branch) prints only "Reset branch".
        if existed && already_on {
            println!("Reset branch '{name}'");
        } else {
            if old_detached {
                if let Some(id) = old_id {
                    let (abbrev, summary) = describe(repo, id)?;
                    println!("Previous HEAD position was {abbrev} {summary}");
                }
            }
            if existed {
                println!("Switched to and reset branch '{name}'");
            } else {
                println!("Switched to a new branch '{name}'");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Restore `paths` in the worktree from the current index (index left unchanged;
/// only stat info is refreshed). `bare` is true for the no-`--` pathspec form,
/// which prints git's "Updated N path(s) from the index" confirmation.
fn restore_from_index(
    repo: &gix::Repository,
    paths: &[&str],
    bare: bool,
    quiet: bool,
) -> Result<ExitCode> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut index = repo.open_index()?;
    let matched = match_paths(&index, paths)?;

    let mut subset = repo.open_index()?;
    keep_only(&mut subset, &matched);
    let should_interrupt = AtomicBool::new(false);
    checkout_subset(repo, &mut subset, &should_interrupt)?;

    // Refresh stat info in the real index for the restored paths so a later
    // status stays cheap; content ids are unchanged.
    let fresh = stats_by_path(&subset);
    for path in &matched {
        if let Ok(idx) = index.entry_index_by_path(BStr::new(path)) {
            if let Some((id, mode, stat)) = fresh.get(path) {
                let e = &mut index.entries_mut()[idx];
                e.id = *id;
                e.mode = *mode;
                e.stat = *stat;
            }
        }
    }
    index.remove_tree();
    index.write(Default::default())?;

    if bare && !quiet {
        let n = matched.len();
        println!("Updated {n} path{} from the index", if n == 1 { "" } else { "s" });
    }
    Ok(ExitCode::SUCCESS)
}

/// Restore `paths` from `tree_ish` into both the index and the worktree
/// (matching stock `git checkout <tree-ish> -- <path>`).
fn restore_from_tree(
    repo: &gix::Repository,
    tree_ish: &str,
    paths: &[&str],
    _quiet: bool,
) -> Result<ExitCode> {
    let tree_id = repo.rev_parse_single(tree_ish)?.object()?.peel_to_tree()?.id;

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let src = repo.index_from_tree(&tree_id)?;
    let matched = match_paths(&src, paths)?;

    let mut subset = repo.index_from_tree(&tree_id)?;
    keep_only(&mut subset, &matched);
    let should_interrupt = AtomicBool::new(false);
    checkout_subset(repo, &mut subset, &should_interrupt)?;

    // Fold the tree's blobs (with fresh checkout stats) into the real index.
    let fresh = stats_by_path(&subset);
    let mut index = repo.open_index()?;
    let mut pushed = false;
    for path in &matched {
        let Some((id, mode, stat)) = fresh.get(path) else {
            continue;
        };
        match index.entry_index_by_path(BStr::new(path)) {
            Ok(idx) => {
                let e = &mut index.entries_mut()[idx];
                e.id = *id;
                e.mode = *mode;
                e.stat = *stat;
            }
            Err(_) => {
                index.dangerously_push_entry(
                    *stat,
                    *id,
                    gix::index::entry::Flags::empty(),
                    *mode,
                    BStr::new(path),
                );
                pushed = true;
            }
        }
    }
    if pushed {
        index.sort_entries();
    }
    index.remove_tree();
    index.write(Default::default())?;

    Ok(ExitCode::SUCCESS)
}

// --- Worktree / ref helpers ------------------------------------------------

/// Refuse any tracked-file modification before a switch that rewrites the
/// worktree (untracked files are ignored, matching git).
fn ensure_clean(repo: &gix::Repository) -> Result<()> {
    if repo.is_dirty()? {
        bail!("your local changes would be overwritten by checkout; commit or stash them first");
    }
    Ok(())
}

/// Move a clean worktree and its index from the current state to `new_tree`,
/// writing only the files that changed (added/modified checked out, removed
/// deleted). Mirrors the file-level reconciliation used by `zsync`.
fn update_worktree_to_tree(repo: &gix::Repository, new_tree: ObjectId) -> Result<()> {
    let should_interrupt = AtomicBool::new(false);

    // Current tracked state (worktree == this when clean), with real stats.
    let old = repo.index_or_load_from_head()?.into_owned();
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    // Full target index (the whole new tree) — what is finally written.
    let mut new_index = repo.index_from_tree(&new_tree)?;

    // Just the changed subset (added, or content/mode differs) — what is written
    // to disk.
    let mut subset = repo.index_from_tree(&new_tree)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        None => false,
    });

    checkout_subset(repo, &mut subset, &should_interrupt)?;

    // Delete files present in the old tree but not the new one.
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

    // Fresh stats for changed entries; reuse previous stats for unchanged ones.
    let subset_stats = stats_by_path(&subset);
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some((_, _, stat)) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }
    new_index.remove_tree();
    new_index.write(Default::default())?;
    Ok(())
}

/// Check out the entries currently held in `index` into the worktree, overwriting
/// existing files (filters, mode and symlink handling applied by gitoxide).
fn checkout_subset(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    gix::worktree::state::checkout(
        index,
        workdir.as_path(),
        odb,
        &gix::progress::Discard,
        &gix::progress::Discard,
        should_interrupt,
        opts,
    )?;
    Ok(())
}

/// Set `HEAD` to point symbolically at `branch` (attached), logging the move.
fn set_head_symbolic(repo: &gix::Repository, branch: FullName, message: &str) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    Ok(())
}

/// Detach `HEAD` at object `id`, logging the move.
fn set_head_detached(repo: &gix::Repository, id: ObjectId, message: &str) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(id),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    Ok(())
}

/// Human label for the current `HEAD` used in reflog "moving from …" messages:
/// the short branch name, else the abbreviated detached hash, else "(unborn)".
fn head_label(head: &gix::Head<'_>) -> String {
    if let Some(name) = head.referent_name() {
        name.shorten().to_string()
    } else if let Some(id) = head.id() {
        id.shorten_or_id().to_string()
    } else {
        "(unborn)".to_string()
    }
}

/// Abbreviated hash + commit summary for `HEAD is now at …` / `Previous HEAD …`.
fn describe(repo: &gix::Repository, id: ObjectId) -> Result<(String, String)> {
    let abbrev = id.attach(repo).shorten_or_id().to_string();
    let commit = repo.find_object(id)?.peel_to_commit()?;
    let summary = commit.message()?.summary().into_owned().to_string();
    Ok((abbrev, summary))
}

// --- Pathspec / index helpers ----------------------------------------------

/// Collect the index entries (by path) matching every pathspec in `specs`.
/// Each spec must match at least one entry, else git's "did not match" error.
fn match_paths(index: &gix::index::File, specs: &[&str]) -> Result<Vec<BString>> {
    let mut matched: Vec<BString> = Vec::new();
    let mut seen: HashSet<BString> = HashSet::new();
    let mut hit = vec![false; specs.len()];

    let backing = index.path_backing();
    for e in index.entries() {
        let path = e.path_in(backing);
        let bytes: &[u8] = path.as_ref();
        for (si, spec) in specs.iter().enumerate() {
            if spec_matches(bytes, spec) {
                hit[si] = true;
                let owned = path.to_owned();
                if seen.insert(owned.clone()) {
                    matched.push(owned);
                }
            }
        }
    }

    if let Some(si) = hit.iter().position(|h| !h) {
        bail!(
            "pathspec '{}' did not match any file(s) known to git",
            specs[si]
        );
    }
    Ok(matched)
}

/// A pathspec matches a file path when it is `.`/empty (all), equals the path,
/// or is a directory prefix of it.
fn spec_matches(path: &[u8], spec: &str) -> bool {
    let s = spec.as_bytes();
    if s.is_empty() || spec == "." {
        return true;
    }
    if path == s {
        return true;
    }
    path.len() > s.len() && path.starts_with(s) && path[s.len()] == b'/'
}

/// Reduce `index` to only the entries whose path is in `keep`.
fn keep_only(index: &mut gix::index::File, keep: &[BString]) {
    index.remove_entries(|_, path, _| !keep.iter().any(|k| BStr::new(k) == path));
}

/// Map path → (id, mode, stat) for every entry of `index` (post-checkout stats).
fn stats_by_path(index: &gix::index::File) -> HashMap<BString, (ObjectId, Mode, Stat)> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode, e.stat)))
        .collect()
}
