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
//! * `reset [--mixed] [<commit>]` — move the branch and reset the index to the target
//!   tree, leaving the worktree, then refresh the index against the worktree and
//!   report what is still unstaged (see below).
//! * `reset [<commit>] [--] <paths>...` — reset the given pathspecs in the index back
//!   to the target tree's version (default `HEAD`), leaving the worktree. No `HEAD`
//!   move, no `ORIG_HEAD`, but the same index refresh and report.
//!
//! ## The `Unstaged changes after reset:` report
//!
//! `cmd_reset()` (builtin/reset.c) ends a `MIXED` reset — which includes the path
//! form, since `reset_type` defaults to `MIXED` — by calling `refresh_index()` with
//! `REFRESH_IN_PORCELAIN` and the header `Unstaged changes after reset:`. That walks
//! every index entry, `lstat`s it, and prints `<status>\t<path>` for the ones that
//! disagree with the worktree, emitting the header lazily before the first hit
//! (`show_file()`, read-cache.c). Paths are written raw — `refresh_index` does not
//! quote them. Entries that only looked stale get their stat data refreshed instead.
//! Here the walk is `Repository::status()` restricted to the index↔worktree pass,
//! whose `EntryStatus` maps 1:1 onto git's `modified`/`deleted`/`typechange` formats,
//! and whose `NeedsUpdate` carries exactly the refreshed stat git would store.
//! `--quiet` and `--no-refresh` suppress the report, as does a bare repository.
//!
//! ## Deferred
//!
//! `--merge`, `--keep`, `--patch`/`-p` and `--intent-to-add`/`-N` are unsupported.

use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

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

/// `git reset -h` / the block parse-options prints on a bad option, verbatim.
const USAGE: &str = "\
usage: git reset [--mixed | --soft | --hard | --merge | --keep] [-q] [<commit>]
   or: git reset [-q] [<tree-ish>] [--] <pathspec>...
   or: git reset [-q] [--pathspec-from-file [--pathspec-file-nul]] [<tree-ish>]
   or: git reset --patch [<tree-ish>] [--] [<pathspec>...]

    -q, --[no-]quiet      be quiet, only report errors
    --no-refresh          skip refreshing the index after reset
    --refresh             opposite of --no-refresh
    --mixed               reset HEAD and index
    --soft                reset only HEAD
    --hard                reset HEAD, index and working tree
    --merge               reset HEAD, index and working tree
    --keep                reset HEAD but keep local changes
    --[no-]recurse-submodules[=<reset>]
                          control recursive updating of submodules
    -p, --[no-]patch      select hunks interactively
    --[no-]auto-advance   auto advance to the next file when selecting hunks interactively
    -U, --unified <n>     generate diffs with <n> lines context
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    -N, --[no-]intent-to-add
                          record only the fact that removed paths will be added later
    --[no-]pathspec-from-file <file>
                          read pathspec from file
    --[no-]pathspec-file-nul
                          with --pathspec-from-file, pathspec elements are separated with NUL character

";

/// `fatal: ambiguous argument ...` — `die()` from `verify_filename()` (setup.c) when a
/// leading positional is neither a revision nor an existing worktree path.
fn ambiguous_argument(arg: &str) -> ExitCode {
    eprintln!("fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.");
    eprintln!("Use '--' to separate paths from revisions, like this:");
    eprintln!("'git <command> [<revision>...] -- [<file>...]'");
    ExitCode::from(128)
}

pub fn reset(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    // ---- 1. Parse flags, honoring the `--` paths separator. ----
    let mut mode: Option<ResetMode> = None;
    let mut quiet = false;
    let mut refresh = true;
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
            "--no-quiet" => quiet = false,
            "--refresh" => refresh = true,
            "--no-refresh" => refresh = false,
            "--merge" => bail!("--merge is unsupported (three-way merge reset not ported)"),
            "--keep" => bail!("--keep is unsupported (keep-local-changes reset not ported)"),
            "-p" | "--patch" => bail!("--patch is unsupported (interactive hunk selection not ported)"),
            "-N" | "--intent-to-add" => {
                bail!("--intent-to-add is unsupported (intent-to-add markers not ported)")
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown option `{}'", &other[2..]);
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            other if other.starts_with('-') && other != "-" => {
                let sw = other.chars().nth(1).unwrap_or('-');
                eprintln!("error: unknown switch `{sw}'");
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            other => positionals.push(other),
        }
    }

    // ---- 2. Split positionals into an optional <commit> and pathspecs. ----
    // With `--`, a lone token before it is the commit; everything after is a path.
    // Without `--`, git takes the first positional as <commit> iff it resolves as a
    // revision; otherwise it must name an existing worktree path (`verify_filename()`),
    // and the remainder are pathspecs that go unverified.
    let mut commit_spec: Option<&str> = None;
    let mut unverified: Option<&str> = None;
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
            unverified = Some(*first);
            paths.extend(positionals.iter().map(|s| s.to_string()));
        }
    }

    // `check_filename()` is a bare `lstat` probe: a tracked-but-deleted path fails it
    // just like a typo'd revision does, and both die before anything is touched.
    if let Some(first) = unverified {
        if std::fs::symlink_metadata(first).is_err() {
            return Ok(ambiguous_argument(first));
        }
    }

    let with_paths = !paths.is_empty();
    if with_paths {
        if let Some(m @ (ResetMode::Soft | ResetMode::Hard)) = mode {
            eprintln!("fatal: Cannot do {} reset with paths.", m.label());
            return Ok(ExitCode::from(128));
        }
        if mode == Some(ResetMode::Mixed) {
            eprintln!("warning: --mixed with paths is deprecated; use 'git reset -- <paths>' instead.");
        }
    }

    let mode = mode.unwrap_or(ResetMode::Mixed);

    let reflog_spec = commit_spec.unwrap_or("HEAD");
    let target = match repo.rev_parse_single(reflog_spec) {
        Ok(id) => id,
        Err(_) => return Ok(ambiguous_argument(reflog_spec)),
    };
    let commit = target.object()?.peel_to_commit()?;
    let target_commit = commit.id;
    let target_tree = commit.tree_id()?.detach();

    if mode == ResetMode::Hard && repo.workdir().is_none() {
        bail!("hard reset not allowed in a bare repository");
    }

    // Serialize the whole read-modify-write; held for the rest of the function.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Capture the pre-reset index (worktree stats + tracked set) before mutating.
    let old_index = repo.index_or_load_from_head_or_empty()?.into_owned();

    // ---- 3. Path form: reset the named index entries only; no HEAD move. ----
    if with_paths {
        let mut index = pathspec_index(&repo, &old_index, target_tree, &paths)?;
        finish_mixed(&repo, &mut index, quiet, refresh)?;
        return Ok(ExitCode::SUCCESS);
    }

    // ---- 4. Whole-tree form. ----
    // `reset_refs()` records the pre-reset HEAD in ORIG_HEAD before moving HEAD, and
    // `remove_branch_state()` drops any in-progress merge/cherry-pick/revert state.
    if let Ok(prev) = repo.head_id() {
        set_orig_head(&repo, prev.detach())?;
    }
    move_head(&repo, target_commit, reflog_spec)?;
    remove_branch_state(repo.git_dir());

    match mode {
        ResetMode::Soft => {}
        ResetMode::Mixed => {
            let mut index = reset_index_to_tree(&repo, &old_index, target_tree)?;
            finish_mixed(&repo, &mut index, quiet, refresh)?;
        }
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

/// Point `ORIG_HEAD` at `id`, as `reset_refs()` does before `HEAD` moves.
fn set_orig_head(repo: &gix::Repository, id: ObjectId) -> Result<()> {
    let name: FullName = "ORIG_HEAD"
        .try_into()
        .map_err(|e| anyhow!("invalid ref name ORIG_HEAD: {e}"))?;
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

/// The state files `remove_branch_state()` (branch.c) unlinks after a whole-tree reset.
fn remove_branch_state(git_dir: &std::path::Path) {
    for name in [
        "MERGE_HEAD",
        "MERGE_RR",
        "MERGE_MSG",
        "MERGE_MODE",
        "AUTO_MERGE",
        "SQUASH_MSG",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        let _ = std::fs::remove_file(git_dir.join(name));
    }
    let _ = std::fs::remove_dir_all(git_dir.join("sequencer"));
}

/// Close out a `MIXED` reset: refresh the index against the worktree, report what is
/// still unstaged, then persist. `--quiet`, `--no-refresh` and bare repositories skip
/// the refresh, exactly as `cmd_reset()` does.
fn finish_mixed(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    quiet: bool,
    refresh: bool,
) -> Result<()> {
    if !quiet && refresh && repo.workdir().is_some() {
        refresh_index_report(repo, index)?;
    }
    // Drop the stale cache-tree extension before persisting (see gix File::write).
    index.remove_tree();
    index.write(Default::default())?;
    Ok(())
}

/// `refresh_index(..., REFRESH_IN_PORCELAIN, "Unstaged changes after reset:")`.
///
/// Prints one `<status>\t<path>` line per index entry that disagrees with the
/// worktree, under a header emitted lazily before the first line, and folds the
/// refreshed stat data of merely-stale entries back into `index`. Paths are written
/// as raw bytes because `refresh_index` does no quoting.
fn refresh_index_report(repo: &gix::Repository, index: &mut gix::index::File) -> Result<()> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change as Wt, EntryStatus};

    let mut changed: Vec<(BString, &'static str)> = Vec::new();
    let mut fresh: HashMap<BString, Stat> = HashMap::new();

    let iter = repo
        .status(gix::progress::Discard)?
        .index(gix::worktree::IndexPersistedOrInMemory::InMemory(index.clone()))
        .untracked_files(gix::status::UntrackedFiles::None)
        .index_worktree_options_mut(|opts| opts.dirwalk_options = None)
        .into_index_worktree_iter(Vec::new())?;

    for item in iter {
        if let Item::Modification { rela_path, status, .. } = item? {
            // read-cache.c picks the format string in this order: deleted, then
            // intent-to-add, then typechange, then modified; unmerged entries are
            // reported as `U` because reset does not pass REFRESH_UNMERGED.
            let code = match status {
                EntryStatus::Change(Wt::Removed) => "D",
                EntryStatus::IntentToAdd => "A",
                EntryStatus::Change(Wt::Type { .. }) => "T",
                EntryStatus::Change(Wt::Modification { .. })
                | EntryStatus::Change(Wt::SubmoduleModification(_)) => "M",
                EntryStatus::Conflict { .. } => "U",
                EntryStatus::NeedsUpdate(stat) => {
                    fresh.insert(rela_path, stat);
                    continue;
                }
            };
            changed.push((rela_path, code));
        }
    }

    // git walks the index, which is sorted by path; the status iterator is not.
    changed.sort_by(|a, b| a.0.cmp(&b.0));
    if !changed.is_empty() {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(b"Unstaged changes after reset:\n")?;
        for (path, code) in &changed {
            out.write_all(code.as_bytes())?;
            out.write_all(b"\t")?;
            out.write_all(&path[..])?;
            out.write_all(b"\n")?;
        }
        out.flush()?;
    }

    if !fresh.is_empty() {
        let backing = index.path_backing().to_owned();
        for e in index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = fresh.get(&path) {
                e.stat = *stat;
            }
        }
    }

    Ok(())
}

/// Build the `--mixed` index: `tree` verbatim, but preserving worktree stats for
/// entries whose id and mode are unchanged so the following refresh does not have to
/// re-hash every file and the index isn't spuriously reported as fully modified.
fn reset_index_to_tree(
    repo: &gix::Repository,
    old: &gix::index::File,
    tree: ObjectId,
) -> Result<gix::index::File> {
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

    Ok(new_index)
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
    crate::worktree::checkout_subset(
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

/// `git reset [<commit>] [--] <paths>` — build the index with the named entries (all
/// stages) reset to the target tree's version, or dropped if absent from the tree.
/// The worktree is untouched. A pathspec that matches nothing is not an error: git
/// only validates the *leading* positional, and that happens during setup.
fn pathspec_index(
    repo: &gix::Repository,
    old: &gix::index::File,
    tree: ObjectId,
    paths: &[String],
) -> Result<gix::index::File> {
    // Pathspecs are given relative to the CWD; index paths are repo-root relative.
    let prefix = repo
        .prefix()?
        .map(|p| p.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))
        .filter(|p| !p.is_empty());
    let specs: Vec<String> = paths
        .iter()
        .map(|raw| {
            let cleaned = raw.trim_start_matches("./").trim_end_matches('/');
            let cleaned = if cleaned == "." { "" } else { cleaned };
            match (&prefix, cleaned.is_empty()) {
                (Some(p), true) => p.clone(),
                (Some(p), false) => format!("{p}/{cleaned}"),
                (None, _) => cleaned.to_string(),
            }
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

    let mut index = old.clone();

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

    let mut ops: HashSet<BString> = HashSet::new();
    for full in &specs {
        for cand in &candidates {
            if matches(BStr::new(cand), full) {
                ops.insert(cand.clone());
            }
        }
    }

    if ops.is_empty() {
        return Ok(index);
    }

    // Drop every stage of each selected path, then re-add the tree version if any.
    index.remove_entries(|_, path, _| ops.contains(&path.to_owned()));
    for path in &ops {
        if let Some((stat, id, flags, mode)) = target_map.get(path) {
            index.dangerously_push_entry(*stat, *id, *flags, *mode, BStr::new(path));
        }
    }
    index.sort_entries();

    Ok(index)
}
