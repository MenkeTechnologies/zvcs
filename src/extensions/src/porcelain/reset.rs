//! `git reset` — move `HEAD` (`--soft`/`--mixed`/`--hard`) and/or unstage paths.
//!
//! Served natively via the vendored gitoxide crates so tools on PATH observe the
//! same refs and index. Every ref/index/worktree mutation is serialized through
//! [`crate::lock::RepoLock`] and matches git's staging semantics.
//!
//! ## Supported forms
//!
//! * `reset --soft [<commit>]`  — move the current branch only (no index/worktree touch).
//! * `reset --hard [<commit>]`  — move the branch, then overwrite the index and worktree
//!   from the target tree (discarding local changes to tracked files). Prints
//!   `HEAD is now at <short> <summary>` unless `--quiet`.
//! * `reset [--mixed] --quiet [<commit>]` — move the branch and reset the index to the
//!   target tree, leaving the worktree. Quiet only (see deferral below).
//! * `reset [<commit>] [--] <paths>...` — unstage the given paths back to the target
//!   tree's version (default `HEAD`), leaving the worktree. No `HEAD` move.
//!
//! ## Deferred
//!
//! Non-quiet whole-tree `--mixed` (the bare `git reset` default) additionally prints
//! the `Unstaged changes after reset:` `git diff-files` listing. That listing is not
//! reproduced here, so this form bails rather than emit mismatching output. Use
//! `--quiet`, `--soft`, `--hard`, or the path form instead. `--merge`, `--keep`,
//! `--patch`/`-p` and `--intent-to-add`/`-N` are likewise unsupported.

use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

impl ResetMode {
    fn label(self) -> &'static str {
        match self {
            ResetMode::Soft => "soft",
            ResetMode::Mixed => "mixed",
            ResetMode::Hard => "hard",
        }
    }
}

pub fn reset(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    // ---- 1. Parse flags, honoring the `--` paths separator. ----
    let mut mode: Option<ResetMode> = None;
    let mut quiet = false;
    let mut saw_dd = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut paths: Vec<String> = Vec::new();

    for a in args {
        if saw_dd {
            paths.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => saw_dd = true,
            "--soft" => mode = Some(ResetMode::Soft),
            "--mixed" => mode = Some(ResetMode::Mixed),
            "--hard" => mode = Some(ResetMode::Hard),
            "-q" | "--quiet" => quiet = true,
            "--merge" => bail!("--merge is unsupported (three-way merge reset not ported)"),
            "--keep" => bail!("--keep is unsupported (keep-local-changes reset not ported)"),
            "-p" | "--patch" => bail!("--patch is unsupported (interactive hunk selection not ported)"),
            "-N" | "--intent-to-add" => {
                bail!("--intent-to-add is unsupported (intent-to-add markers not ported)")
            }
            other if other.starts_with('-') && other != "-" => bail!("unknown option {other:?}"),
            other => positionals.push(other),
        }
    }

    // ---- 2. Split positionals into an optional <commit> and pathspecs. ----
    // With `--`, a lone token before it is the commit; everything after is a path.
    // Without `--`, git takes the first positional as <commit> iff it resolves as a
    // revision, and the remainder as pathspecs.
    let mut commit_spec: Option<&str> = None;
    if saw_dd {
        match positionals.as_slice() {
            [] => {}
            [c] => commit_spec = Some(*c),
            _ => bail!("too many revisions given before `--`"),
        }
    } else if let Some((first, rest)) = positionals.split_first() {
        if repo.rev_parse_single(*first).is_ok() {
            commit_spec = Some(*first);
            paths.extend(rest.iter().map(|s| s.to_string()));
        } else {
            paths.extend(positionals.iter().map(|s| s.to_string()));
        }
    }

    // ---- 3. Path form: unstage the given pathspecs; no HEAD move, no output. ----
    if !paths.is_empty() {
        if let Some(m @ (ResetMode::Soft | ResetMode::Hard)) = mode {
            bail!("Cannot do {} reset with paths.", m.label());
        }
        return pathspec_reset(&repo, commit_spec, &paths);
    }

    // ---- 4. Whole-tree form. ----
    let mode = mode.unwrap_or(ResetMode::Mixed);

    // Bail BEFORE any mutation for the one output we cannot reproduce.
    if mode == ResetMode::Mixed && !quiet {
        bail!(
            "non-quiet mixed reset is unsupported: its \"Unstaged changes after reset:\" \
             listing is not reproduced (use --quiet, --soft, --hard, or 'reset [<commit>] -- <paths>')"
        );
    }

    let reflog_spec = commit_spec.unwrap_or("HEAD");
    let commit = repo.rev_parse_single(reflog_spec)?.object()?.peel_to_commit()?;
    let target_commit = commit.id;
    let target_tree = commit.tree_id()?.detach();

    if mode == ResetMode::Hard && repo.workdir().is_none() {
        bail!("hard reset not allowed in a bare repository");
    }

    // Serialize the whole read-modify-write; held for the rest of the function.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Capture the pre-reset index (worktree stats + tracked set) before mutating.
    let old_index = repo.index_or_load_from_head_or_empty()?.into_owned();

    // Move the current branch (or a detached HEAD) to the target commit. `deref`
    // follows a symbolic HEAD to its referent branch, matching `git reset`.
    move_head(&repo, target_commit, reflog_spec)?;

    match mode {
        ResetMode::Soft => {}
        ResetMode::Mixed => reset_index_to_tree(&repo, &old_index, target_tree)?,
        ResetMode::Hard => {
            let should_interrupt = AtomicBool::new(false);
            reset_worktree_hard(&repo, &old_index, target_tree, &should_interrupt)?;
            if !quiet {
                let summary = commit.message()?.summary().into_owned();
                println!("HEAD is now at {} {}", commit.short_id()?, summary);
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Point `HEAD` (or the branch it references) at `target`, writing a reflog entry
/// `reset: moving to <spec>` on both refs, exactly as `git reset` does.
fn move_head(repo: &gix::Repository, target: ObjectId, spec: &str) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("reset: moving to {spec}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(target),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: true,
    })?;
    Ok(())
}

/// Reset the index to `tree` (the `--mixed` index step), preserving worktree stats
/// for entries whose id and mode are unchanged so a later status stays cheap and
/// the index isn't spuriously reported as fully modified.
fn reset_index_to_tree(repo: &gix::Repository, old: &gix::index::File, tree: ObjectId) -> Result<()> {
    let mut new_index = repo.index_from_tree(&tree)?;

    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }

    // Drop the stale cache-tree extension before persisting (see gix File::write).
    new_index.remove_tree();
    new_index.write(Default::default())?;
    Ok(())
}

/// `--hard`: overwrite the worktree and index from `tree`, discarding local changes
/// to tracked files and deleting files the reset removes. Untracked files are left
/// untouched, matching `git reset --hard`.
fn reset_worktree_hard(
    repo: &gix::Repository,
    old: &gix::index::File,
    tree: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("hard reset not allowed in a bare repository"))?
        .to_owned();

    // The full target index; checking it all out overwrites every tracked file with
    // the tree version (thus discarding worktree modifications) and back-fills fresh
    // stats onto the entries, yielding a clean index after the write.
    let mut new_index = repo.index_from_tree(&tree)?;

    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    gix::worktree::state::checkout(
        &mut new_index,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        should_interrupt,
        opts,
    )?;

    // Remove files tracked before the reset but absent from the target tree.
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

    new_index.remove_tree();
    new_index.write(Default::default())?;
    Ok(())
}

/// `git reset [<commit>] [--] <paths>` — reset the named index entries (all stages)
/// to the target tree's version, or drop them if absent from the tree. The worktree
/// is left untouched and no `HEAD` move is performed; git prints nothing on success.
fn pathspec_reset(repo: &gix::Repository, commit_spec: Option<&str>, paths: &[String]) -> Result<ExitCode> {
    let spec = commit_spec.unwrap_or("HEAD");
    let tree = repo.rev_parse_single(spec)?.object()?.peel_to_commit()?.tree_id()?.detach();

    // Pathspecs are given relative to the CWD; index paths are repo-root relative.
    let prefix = repo
        .prefix()?
        .map(|p| p.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))
        .filter(|p| !p.is_empty());
    let specs: Vec<(String, String)> = paths
        .iter()
        .map(|raw| {
            let cleaned = raw.trim_start_matches("./").trim_end_matches('/');
            let cleaned = if cleaned == "." { "" } else { cleaned };
            let full = match (&prefix, cleaned.is_empty()) {
                (Some(p), true) => p.clone(),
                (Some(p), false) => format!("{p}/{cleaned}"),
                (None, _) => cleaned.to_string(),
            };
            (raw.clone(), full)
        })
        .collect();

    let matches = |path: &BStr, s: &str| -> bool {
        if s.is_empty() {
            return true;
        }
        let pb: &[u8] = path.as_ref();
        let sb = s.as_bytes();
        pb == sb || (pb.len() > sb.len() && pb.starts_with(sb) && pb[sb.len()] == b'/')
    };

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Desired versions for every path in the target tree.
    let target = repo.index_from_tree(&tree)?;
    let mut target_map: HashMap<BString, (Stat, ObjectId, Flags, Mode)> =
        HashMap::with_capacity(target.entries().len());
    {
        let backing = target.path_backing();
        for e in target.entries() {
            target_map.insert(e.path_in(backing).to_owned(), (e.stat, e.id, e.flags, e.mode));
        }
    }

    let mut index = repo.index_or_load_from_head_or_empty()?.into_owned();

    // Candidate paths = union of currently-tracked and target-tree paths.
    let mut candidates: BTreeSet<BString> = BTreeSet::new();
    {
        let backing = index.path_backing();
        for e in index.entries() {
            candidates.insert(e.path_in(backing).to_owned());
        }
    }
    for p in target_map.keys() {
        candidates.insert(p.clone());
    }

    // Resolve which candidate paths each pathspec selects; a spec matching nothing
    // is a fatal error, exactly like git.
    let mut ops: HashSet<BString> = HashSet::new();
    for (raw, full) in &specs {
        let mut hit = false;
        for cand in &candidates {
            if matches(BStr::new(cand), full) {
                ops.insert(cand.clone());
                hit = true;
            }
        }
        if !hit {
            bail!("pathspec '{raw}' did not match any files");
        }
    }

    if ops.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    // Drop every stage of each selected path, then re-add the tree version if any.
    index.remove_entries(|_, path, _| ops.contains(&path.to_owned()));
    for path in &ops {
        if let Some((stat, id, flags, mode)) = target_map.get(path) {
            index.dangerously_push_entry(*stat, *id, *flags, *mode, BStr::new(path));
        }
    }
    index.sort_entries();
    index.remove_tree();
    index.write(Default::default())?;

    Ok(ExitCode::SUCCESS)
}
