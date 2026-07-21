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
//! Every transition message (`Switched to …`, `Already on …`, `Reset branch …`,
//! `HEAD is now at …`, `Previous HEAD position was …`, `Updated N path(s) …`,
//! and the `advice.detachedHead` block) goes to **stderr**, as in stock git —
//! `git checkout` writes nothing to stdout on success.
//!
//! Deviations (honest, conservative — never corrupting):
//!   * A branch/commit switch that changes the working tree requires a clean
//!     tracked worktree. Stock git also permits a switch when the dirty files do
//!     not collide with the diff between trees; that non-conflicting case is
//!     refused here (message names it) rather than risking an incorrect merge.
//!     Switches whose target tree equals the current tree (e.g. `-b` at HEAD, or
//!     two branches on the same commit) carry local changes and are never
//!     refused. Untracked files are ignored for the clean check, matching git.
//!   * Pathspecs match literal files and directory prefixes (and `.`); general
//!     glob magic is left to the shell.
//!   * `--ours`/`--theirs` write a conflicted path's stage-2/stage-3 blob into
//!     the worktree (index left conflicted), `-t`/`--track` create-and-track,
//!     `--orphan` starts an unborn branch — all matching stock git.
//!   * `-m`/`--merge` is accepted: with a clean worktree it is byte-identical to
//!     a plain switch, and the dirty case is governed by the same conservative
//!     clean-check as every other switch here.
//!   * `-p`/`--patch` (interactive hunk selection) still bails — it needs a TTY.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};
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
    let mut track = false;
    let mut orphan: Option<String> = None;
    // Which conflict stage `--ours`/`--theirs` writes out (2 = ours, 3 = theirs);
    // the last of the two flags wins, exactly like git's `opts.writeout_stage`.
    let mut writeout_stage: Option<u8> = None;
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
            "--orphan" => {
                let Some(name) = args.get(i + 1) else {
                    // git: `error: option `orphan' requires a value`, exit 129.
                    eprintln!("error: option `orphan' requires a value");
                    return Ok(ExitCode::from(129));
                };
                orphan = Some(name.clone());
                i += 1;
            }
            "--detach" => detach = true,
            "-q" | "--quiet" => quiet = true,
            "-f" | "--force" => {} // accepted; a clean switch needs no forcing here
            "-t" | "--track" => track = true,
            "--no-track" => {} // accepted; auto-tracking is off unless -t is given
            "--ours" | "-2" => writeout_stage = Some(2),
            "--theirs" | "-3" => writeout_stage = Some(3),
            // `-m` only changes behavior when local changes must be carried across
            // the switch; with a clean worktree it is byte-identical to a plain
            // checkout, so accept it and let the shared clean-check govern the
            // dirty case exactly as every other switch here does.
            "-m" | "--merge" => {}
            "-p" | "--patch" => bail!("interactive patch checkout (-p) needs a TTY"),
            _ if a.starts_with('-') && a.len() > 1 => bail!("unsupported flag {a:?}"),
            _ => pre.push(a),
        }
        i += 1;
    }

    // --- Dispatch -----------------------------------------------------------
    // `--orphan <name> [<start>]`: start an unborn branch off `<start>`'s tree.
    if let Some(name) = orphan {
        let start = pre.first().copied().unwrap_or("HEAD");
        return orphan_checkout(&repo, &name, start, quiet);
    }

    // `--ours`/`--theirs <path>…`: write one conflict side into the worktree.
    if let Some(stage) = writeout_stage {
        let paths = if has_dashdash { &post } else { &pre };
        if paths.is_empty() {
            eprintln!("fatal: '--ours/--theirs' needs the paths to check out");
            return Ok(ExitCode::from(128));
        }
        return restore_conflict_stage(&repo, paths, stage, !has_dashdash, quiet);
    }

    if let Some((name, reset)) = new_branch {
        if has_dashdash || !post.is_empty() {
            bail!("cannot combine branch creation (-b/-B) with path restore");
        }
        if pre.len() > 1 {
            bail!("too many start-points given for branch creation");
        }
        let start = pre.first().copied().unwrap_or("HEAD");
        return create_and_switch(&repo, &name, reset, start, quiet, track);
    }

    // `-t <remote>/<branch>` with no `-b`: DWIM the local branch name from the
    // remote-tracking start-point, then create-and-track.
    if track {
        if pre.len() != 1 {
            eprintln!("fatal: missing branch name; try -b");
            return Ok(ExitCode::from(128));
        }
        match resolve_tracking(&repo, pre[0])? {
            Some(info) => {
                let Some(name) = info.dwim_name.clone() else {
                    // A local-branch start-point can't DWIM a new name.
                    eprintln!("fatal: missing branch name; try -b");
                    return Ok(ExitCode::from(128));
                };
                return create_and_switch(&repo, &name, false, pre[0], quiet, true);
            }
            None => {
                eprintln!("fatal: missing branch name; try -b");
                return Ok(ExitCode::from(128));
            }
        }
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
            return detached_checkout(&repo, spec, commit, quiet, detach);
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
                eprintln!("Already on '{spec}'");
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
        // git only reports the abandoned detached position when it actually
        // moves (checkout.c: `!old->path && old->commit != new->commit`).
        if old_detached {
            if let Some(id) = old_id.filter(|id| *id != commit.id) {
                let (abbrev, summary) = describe(repo, id)?;
                eprintln!("Previous HEAD position was {abbrev} {summary}");
            }
        }
        eprintln!("Switched to branch '{spec}'");
    }
    Ok(ExitCode::SUCCESS)
}

/// Detach `HEAD` at `commit`, updating the worktree when the target tree differs.
/// `force_detach` is true for an explicit `--detach`, which suppresses the
/// `advice.detachedHead` block just as git's `opts->force_detach` does.
fn detached_checkout(
    repo: &gix::Repository,
    spec: &str,
    commit: gix::Commit<'_>,
    quiet: bool,
    force_detach: bool,
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
                eprintln!("Previous HEAD position was {abbrev} {summary}");
            }
        } else if !force_detach
            && repo.config_snapshot().boolean("advice.detachedHead") != Some(false)
        {
            // Leaving an attached HEAD without an explicit --detach: git warns.
            print_detached_head_advice(spec);
        }
        let (abbrev, summary) = describe(repo, target_id)?;
        eprintln!("HEAD is now at {abbrev} {summary}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The `advice.detachedHead` block git prints when a bare `git checkout <commit>`
/// moves off a branch, verbatim (git 2.55.0, `builtin/checkout.c`).
fn print_detached_head_advice(spec: &str) {
    eprintln!("Note: switching to '{spec}'.\n");
    eprintln!(
        "You are in 'detached HEAD' state. You can look around, make experimental\n\
         changes and commit them, and you can discard any commits you make in this\n\
         state without impacting any branches by switching back to a branch.\n\
         \n\
         If you want to create a new branch to retain commits you create, you may\n\
         do so (now or later) by using -c with the switch command. Example:\n\
         \n\
         \x20 git switch -c <new-branch-name>\n\
         \n\
         Or undo this operation with:\n\
         \n\
         \x20 git switch -\n\
         \n\
         Turn off this advice by setting config variable advice.detachedHead to false\n"
    );
}

/// Create (`-b`) or create-or-reset (`-B`) `refs/heads/<name>` at `start`, then
/// switch `HEAD` to it, updating the worktree when the tree changes.
fn create_and_switch(
    repo: &gix::Repository,
    name: &str,
    reset: bool,
    start: &str,
    quiet: bool,
    track: bool,
) -> Result<ExitCode> {
    let full = format!("refs/heads/{name}");
    if gix::validate::reference::branch_name(BStr::new(full.as_bytes())).is_err() {
        bail!("'{name}' is not a valid branch name");
    }

    // `-t`: resolve the upstream before any mutation, so a bad start-point fails
    // exactly like git — branch untouched, HEAD unmoved.
    let track_info = if track {
        match resolve_tracking(repo, start)? {
            Some(info) => Some(info),
            None => {
                eprintln!(
                    "fatal: cannot set up tracking information; starting point '{start}' is not a branch"
                );
                return Ok(ExitCode::from(128));
            }
        }
    } else {
        None
    };

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

    // `-t`: persist branch.<name>.remote / .merge (lock already held above; the
    // per-thread RepoLock is reentrant, so config.rs-style locking isn't needed).
    if let Some(info) = &track_info {
        write_tracking_config(repo, name, info)?;
    }

    if !quiet {
        // Reset-in-place (-B on the current branch) prints only "Reset branch".
        if existed && already_on {
            eprintln!("Reset branch '{name}'");
        } else {
            if old_detached {
                if let Some(id) = old_id.filter(|id| *id != start_id) {
                    let (abbrev, summary) = describe(repo, id)?;
                    eprintln!("Previous HEAD position was {abbrev} {summary}");
                }
            }
            if existed {
                eprintln!("Switched to and reset branch '{name}'");
            } else {
                eprintln!("Switched to a new branch '{name}'");
            }
        }
        // git prints the tracking confirmation to stdout, after the stderr
        // transition line, and only when not quiet.
        if let Some(info) = &track_info {
            println!("branch '{name}' set up to track '{}'.", info.display);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git checkout --orphan <name> [<start>]`: point `HEAD` at an unborn branch
/// `<name>` whose worktree/index come from `<start>`'s tree. The ref is not
/// created (git materializes it only at the first commit) and no reflog entry is
/// written, matching stock git.
fn orphan_checkout(
    repo: &gix::Repository,
    name: &str,
    start: &str,
    quiet: bool,
) -> Result<ExitCode> {
    // git resolves the start-point before anything else: a bad one aborts here.
    let commit = match repo
        .rev_parse_single(start)
        .ok()
        .and_then(|id| id.object().ok())
        .and_then(|o| o.peel_to_commit().ok())
    {
        Some(c) => c,
        None => {
            eprintln!(
                "fatal: '{start}' is not a commit and a branch '{name}' cannot be created from it"
            );
            return Ok(ExitCode::from(128));
        }
    };

    let full = format!("refs/heads/{name}");
    if gix::validate::reference::branch_name(BStr::new(full.as_bytes())).is_err() {
        eprintln!("fatal: '{name}' is not a valid branch name");
        eprintln!("hint: See 'git help check-ref-format'");
        eprintln!("hint: Disable this message with \"git config set advice.refSyntax false\"");
        return Ok(ExitCode::from(128));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if repo.try_find_reference(full.as_str())?.is_some() {
        eprintln!("fatal: a branch named '{name}' already exists");
        return Ok(ExitCode::from(128));
    }

    let target_tree = commit.tree_id()?.detach();
    let cur_tree = repo.head_tree_id_or_empty()?.detach();
    if target_tree != cur_tree {
        ensure_clean(repo)?;
        update_worktree_to_tree(repo, target_tree)?;
    }

    // Write HEAD as a plain symref to the (not-yet-existing) branch. No ref is
    // created and no reflog line is appended — git's exact orphan behavior.
    let head_path = repo.git_dir().join("HEAD");
    std::fs::write(&head_path, format!("ref: {full}\n"))?;

    if !quiet {
        eprintln!("Switched to a new branch '{name}'");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git checkout --ours|--theirs <path>…`: write one side of a conflict into the
/// worktree. `stage` is 2 (ours) or 3 (theirs). A non-conflicted path falls back
/// to its stage-0 blob; a conflicted path missing the requested side errors with
/// git's `path 'X' does not have our/their version` and exits 1. The index is
/// left untouched (a conflicted path stays conflicted).
fn restore_conflict_stage(
    repo: &gix::Repository,
    paths: &[&str],
    stage: u8,
    bare: bool,
    quiet: bool,
) -> Result<ExitCode> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let index = repo.open_index()?;
    let matched = match_paths(&index, paths)?;
    let mset: HashSet<BString> = matched.iter().cloned().collect();

    // Which stages each matched path carries (index 0..=3).
    let mut have: HashMap<BString, [bool; 4]> = HashMap::new();
    {
        let backing = index.path_backing();
        for e in index.entries() {
            let p = e.path_in(backing).to_owned();
            if mset.contains(&p) {
                let s = (e.stage_raw() as usize).min(3);
                have.entry(p).or_insert([false; 4])[s] = true;
            }
        }
    }

    let side = if stage == 2 { "our" } else { "their" };
    let mut keep: HashMap<BString, u32> = HashMap::new();
    let mut had_error = false;
    for p in &matched {
        let flags = have.get(p).copied().unwrap_or([false; 4]);
        let chosen = if flags[0] {
            Some(0u32) // not conflicted → the single indexed blob
        } else if flags[stage as usize] {
            Some(stage as u32)
        } else {
            None
        };
        match chosen {
            Some(st) => {
                keep.insert(p.clone(), st);
            }
            None => {
                let pb: &[u8] = p.as_ref();
                eprintln!(
                    "error: path '{}' does not have {side} version",
                    String::from_utf8_lossy(pb)
                );
                had_error = true;
            }
        }
    }

    if !keep.is_empty() {
        // Build a stage-0 view holding exactly the chosen entries and check it out;
        // the real index is never rewritten, so conflicts survive.
        let mut subset = repo.open_index()?;
        subset.remove_entries(|_, path, e| match keep.get(&path.to_owned()) {
            Some(&st) => e.stage_raw() != st,
            None => true,
        });
        for e in subset.entries_mut() {
            e.flags.remove(Flags::STAGE_MASK);
        }
        let should_interrupt = AtomicBool::new(false);
        checkout_subset(repo, &mut subset, &should_interrupt)?;
    }

    if had_error {
        return Ok(ExitCode::from(1));
    }
    if bare && !quiet {
        let n = keep.len();
        eprintln!(
            "Updated {n} path{} from the index",
            if n == 1 { "" } else { "s" }
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Upstream a `-t`/`--track` start-point resolves to.
struct TrackInfo {
    /// `branch.<name>.remote`: `"."` for a local start-point, else the remote name.
    remote: String,
    /// `branch.<name>.merge`, always `refs/heads/<branch>`.
    merge: String,
    /// Upstream short name shown in the "set up to track" line.
    display: String,
    /// For `-t` without `-b`: the local branch name DWIM'd from the start-point
    /// (`Some` only for a remote-tracking start; a local one can't DWIM a name).
    dwim_name: Option<String>,
}

/// Classify a `-t` start-point as a trackable branch. Returns `None` when it is
/// neither a local branch nor a remote-tracking branch of a configured remote —
/// the caller turns that into git's "is not a branch" / "missing branch name".
fn resolve_tracking(repo: &gix::Repository, start: &str) -> Result<Option<TrackInfo>> {
    if repo
        .try_find_reference(format!("refs/heads/{start}").as_str())?
        .is_some()
    {
        return Ok(Some(TrackInfo {
            remote: ".".into(),
            merge: format!("refs/heads/{start}"),
            display: start.into(),
            dwim_name: None,
        }));
    }
    if repo
        .try_find_reference(format!("refs/remotes/{start}").as_str())?
        .is_some()
    {
        // Remote names carry no '/', so the first component is the remote.
        if let Some((remote, rest)) = start.split_once('/') {
            if !rest.is_empty()
                && repo
                    .remote_names()
                    .iter()
                    .any(|n| n.to_str_lossy() == remote)
            {
                return Ok(Some(TrackInfo {
                    remote: remote.into(),
                    merge: format!("refs/heads/{rest}"),
                    display: start.into(),
                    dwim_name: Some(rest.into()),
                }));
            }
        }
    }
    Ok(None)
}

/// Persist `branch.<name>.remote` / `branch.<name>.merge` into the repo-local
/// config. The caller already holds the reentrant `RepoLock`.
fn write_tracking_config(repo: &gix::Repository, name: &str, info: &TrackInfo) -> Result<()> {
    let path = repo.common_dir().join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)?;
    file.set_raw_value_by("branch", Some(name), "remote", info.remote.as_str())?;
    file.set_raw_value_by("branch", Some(name), "merge", info.merge.as_str())?;
    let bytes = file.to_bstring();
    let tmp = path.with_extension("zvcs-tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
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
        eprintln!("Updated {n} path{} from the index", if n == 1 { "" } else { "s" });
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
    crate::worktree::checkout_subset(
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
