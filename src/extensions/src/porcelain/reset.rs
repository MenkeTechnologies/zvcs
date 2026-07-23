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
//! * `reset --merge [<commit>]` / `reset --keep [<commit>]` — git's two-tree merge
//!   (`unpack-trees.c` `oneway_merge` / `twoway_merge`): move the branch and update
//!   the index and worktree toward the target, but preserve local changes to files
//!   the reset does not touch, and abort (exit 128, `error: Entry '<p>' not
//!   uptodate. Cannot merge.` + `fatal: Could not reset index file …`, HEAD
//!   unmoved) if a file that must change has un-committed local modifications.
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
//! ## `--intent-to-add` / `-N` and `--pathspec-from-file`
//!
//! `-N` (MIXED only) is git's `update_index_from_diff()` intent-to-add path: any
//! index entry the reset would drop because it is absent from the target tree is
//! kept as an intent-to-add stub instead — mode `100644`, the empty-blob object id,
//! `CE_INTENT_TO_ADD` set — so the removed path stays tracked and re-appears in
//! `git diff`. Entries present in the target tree reset to it as usual. `-N` with a
//! non-MIXED mode dies `the option '-N' requires '--mixed'`.
//!
//! `--pathspec-from-file[=<file>]` / `--pathspec-file-nul` (git's
//! `parse_pathspec_from_file()`) read the pathspec list from a file (or stdin for
//! `-`), NUL- or newline-separated; they feed the same path form as inline
//! pathspecs and reject being combined with inline pathspecs.
//!
//! ## Deferred
//!
//! `--patch`/`-p` (interactive hunk selection) is unsupported.

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
    Merge,
    Keep,
}

impl ResetMode {
    fn label(self) -> &'static str {
        match self {
            ResetMode::Soft => "soft",
            ResetMode::Mixed => "mixed",
            ResetMode::Hard => "hard",
            ResetMode::Merge => "merge",
            ResetMode::Keep => "keep",
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
    let mut intent_to_add = false;
    let mut pathspec_from_file: Option<String> = None;
    let mut pathspec_file_nul = false;
    let mut take_pff_value = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut paths: Vec<String> = Vec::new();

    for a in args {
        // `--pathspec-from-file <file>` (separate-argument form): parse-options
        // consumes the very next token as the value regardless of what it looks like.
        if take_pff_value {
            pathspec_from_file = Some(a.clone());
            take_pff_value = false;
            continue;
        }
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
            "--merge" => mode = Some(ResetMode::Merge),
            "--keep" => mode = Some(ResetMode::Keep),
            "-p" | "--patch" => bail!("--patch is unsupported (interactive hunk selection not ported)"),
            "-N" | "--intent-to-add" => intent_to_add = true,
            "--no-intent-to-add" => intent_to_add = false,
            "--pathspec-from-file" => take_pff_value = true,
            "--no-pathspec-from-file" => pathspec_from_file = None,
            "--pathspec-file-nul" => pathspec_file_nul = true,
            "--no-pathspec-file-nul" => pathspec_file_nul = false,
            s if s.starts_with("--pathspec-from-file=") => {
                pathspec_from_file = Some(s["--pathspec-from-file=".len()..].to_string());
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

    // parse-options rejects a dangling value-taking option with exit 129.
    if take_pff_value {
        eprintln!("error: option `pathspec-from-file' requires a value");
        return Ok(ExitCode::from(129));
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

    // `parse_pathspec_from_file()` (builtin/reset.c): a NUL separator needs the file
    // option; the file list and inline pathspecs are mutually exclusive; then the
    // file/stdin is split into pathspecs that join the path form.
    if pathspec_file_nul && pathspec_from_file.is_none() {
        eprintln!("fatal: the option '--pathspec-file-nul' requires '--pathspec-from-file'");
        return Ok(ExitCode::from(128));
    }
    if let Some(f) = pathspec_from_file {
        if !paths.is_empty() {
            eprintln!("fatal: '--pathspec-from-file' and pathspec arguments cannot be used together");
            return Ok(ExitCode::from(128));
        }
        let data = if f == "-" {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
            buf
        } else {
            std::fs::read(&f)?
        };
        let sep = if pathspec_file_nul { b'\0' } else { b'\n' };
        for part in data.split(|&c| c == sep) {
            let mut s = part;
            if !pathspec_file_nul && s.last() == Some(&b'\r') {
                s = &s[..s.len() - 1];
            }
            if s.is_empty() {
                continue;
            }
            paths.push(String::from_utf8_lossy(s).into_owned());
        }
    }

    let with_paths = !paths.is_empty();
    if with_paths {
        if let Some(m @ (ResetMode::Soft | ResetMode::Hard | ResetMode::Merge | ResetMode::Keep)) =
            mode
        {
            eprintln!("fatal: Cannot do {} reset with paths.", m.label());
            return Ok(ExitCode::from(128));
        }
        if mode == Some(ResetMode::Mixed) {
            eprintln!("warning: --mixed with paths is deprecated; use 'git reset -- <paths>' instead.");
        }
    }

    let mode = mode.unwrap_or(ResetMode::Mixed);

    // `-N` rides only on a MIXED reset (the with-paths guard above already fired for
    // the non-MIXED path form, so this catches the whole-tree `--soft/--hard/… -N`).
    if intent_to_add && mode != ResetMode::Mixed {
        eprintln!("fatal: the option '-N' requires '--mixed'");
        return Ok(ExitCode::from(128));
    }

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
        let mut index = pathspec_index(&repo, &old_index, target_tree, &paths, intent_to_add)?;
        finish_mixed(&repo, &mut index, quiet, refresh)?;
        return Ok(ExitCode::SUCCESS);
    }

    // ---- 4. Whole-tree form. ----
    // `--merge`/`--keep` run git's two-tree merge (`unpack-trees.c`), which may
    // abort on local changes. git does not move HEAD when it aborts, so the merge
    // is computed and applied *before* any ref is touched.
    if matches!(mode, ResetMode::Merge | ResetMode::Keep) {
        let head_tree = match repo.head_id() {
            Ok(h) => h.object()?.peel_to_commit()?.tree_id()?.detach(),
            Err(_) => gix::ObjectId::empty_tree(repo.object_hash()),
        };
        let should_interrupt = AtomicBool::new(false);
        let applied = reset_two_tree(
            &repo,
            &old_index,
            head_tree,
            target_tree,
            mode == ResetMode::Keep,
            &should_interrupt,
        )?;
        if !applied {
            // git's `reset_index` failure: `fatal:` line, exit 128, HEAD untouched.
            eprintln!("fatal: Could not reset index file to revision '{target_commit}'.");
            return Ok(ExitCode::from(128));
        }
        if let Ok(prev) = repo.head_id() {
            set_orig_head(&repo, prev.detach())?;
        }
        move_head(&repo, target_commit, reflog_spec)?;
        remove_branch_state(repo.git_dir());
        if !quiet {
            let summary = commit.message()?.summary().into_owned();
            println!("HEAD is now at {} {}", commit.short_id()?, summary);
        }
        return Ok(ExitCode::SUCCESS);
    }

    // soft/mixed/hard: `reset_refs()` records the pre-reset HEAD in ORIG_HEAD
    // before moving HEAD, and `remove_branch_state()` drops any in-progress
    // merge/cherry-pick/revert state.
    if let Ok(prev) = repo.head_id() {
        set_orig_head(&repo, prev.detach())?;
    }
    move_head(&repo, target_commit, reflog_spec)?;
    remove_branch_state(repo.git_dir());

    match mode {
        ResetMode::Soft => {}
        ResetMode::Mixed => {
            let mut index = reset_index_to_tree(&repo, &old_index, target_tree, intent_to_add)?;
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
        // Handled above, before the ref move.
        ResetMode::Merge | ResetMode::Keep => unreachable!(),
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
///
/// With `intent_to_add`, every old-index path absent from `tree` — which a mixed
/// reset would otherwise drop — is re-added as git's intent-to-add stub instead
/// (`update_index_from_diff()`, the `!is_in_reset_tree` branch): mode `100644`, the
/// empty-blob id, `CE_INTENT_TO_ADD` set, and a zeroed stat so it is never up to date.
fn reset_index_to_tree(
    repo: &gix::Repository,
    old: &gix::index::File,
    tree: ObjectId,
    intent_to_add: bool,
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

    let tree_paths: HashSet<BString> = {
        let backing = new_index.path_backing();
        new_index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
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

    if intent_to_add {
        let ita = ObjectId::empty_blob(repo.object_hash());
        let mut added: HashSet<BString> = HashSet::new();
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing).to_owned();
            if !tree_paths.contains(&path) && added.insert(path.clone()) {
                // EXTENDED must accompany INTENT_TO_ADD: the writer upgrades to index
                // V3 and emits the extended-flags word only when EXTENDED is set —
                // without it the i-t-a bit (>0xffff) is truncated away on write.
                new_index.dangerously_push_entry(
                    Stat::default(),
                    ita,
                    Flags::INTENT_TO_ADD | Flags::EXTENDED,
                    Mode::FILE,
                    BStr::new(&path),
                );
            }
        }
        new_index.sort_entries();
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

/// The per-path outcome of the two-tree merge.
enum Act {
    /// Leave the index and worktree entry untouched (preserving local changes).
    Keep,
    /// Remove the path from the index and worktree.
    Delete,
    /// Set the index and worktree to the target version.
    Update,
    /// Local changes would be lost — abort the whole reset.
    Conflict,
}

/// `oneway_merge` (`--merge`): compare the index entry `i` to the target `t`.
fn classify_merge(t: Option<&(Mode, ObjectId)>, i: Option<&(Mode, ObjectId)>) -> Act {
    match (i, t) {
        (Some(_), None) => Act::Delete,
        (None, None) => Act::Keep,
        (Some(iv), Some(tv)) if iv == tv => Act::Keep,
        (_, Some(_)) => Act::Update,
    }
}

/// `twoway_merge` (`--keep`): compare HEAD `h`, target `t` and index `i`. Keeps
/// files unchanged between HEAD and target (or already at target), updates files
/// that changed only when the index still matches HEAD, and rejects everything
/// else (staged divergence).
fn classify_keep(
    h: Option<&(Mode, ObjectId)>,
    t: Option<&(Mode, ObjectId)>,
    i: Option<&(Mode, ObjectId)>,
) -> Act {
    match i {
        Some(iv) => {
            if h == t || Some(iv) == t {
                Act::Keep
            } else if t.is_none() && h == Some(iv) {
                Act::Delete
            } else if t.is_some() && h == Some(iv) {
                Act::Update
            } else {
                Act::Conflict
            }
        }
        None => match (h, t) {
            (_, None) => Act::Keep,
            (Some(hv), Some(_)) if Some(hv) == t => Act::Keep, // staged deletion kept
            (Some(_), Some(_)) => Act::Conflict,
            (None, Some(_)) => Act::Update,
        },
    }
}

/// `--merge` / `--keep`: git's two-tree merge (`unpack-trees.c` `oneway_merge` /
/// `twoway_merge`). Updates the index and worktree toward `target_tree` while
/// preserving local changes to files the reset does not touch, and aborts —
/// writing nothing and leaving HEAD in place — if a file that must change has
/// un-committed local modifications.
fn reset_two_tree(
    repo: &gix::Repository,
    old: &gix::index::File,
    head_tree: ObjectId,
    target_tree: ObjectId,
    keep: bool,
    should_interrupt: &AtomicBool,
) -> Result<bool> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| {
            anyhow!(
                "{} reset not allowed in a bare repository",
                if keep { "keep" } else { "merge" }
            )
        })?
        .to_owned();

    let target = tree_map(repo, target_tree)?;
    let head = if keep {
        tree_map(repo, head_tree)?
    } else {
        HashMap::new()
    };
    let index = index_entry_map(old);

    let mut all: BTreeSet<BString> = BTreeSet::new();
    all.extend(index.keys().cloned());
    all.extend(target.keys().cloned());
    all.extend(head.keys().cloned());

    let mut updates: Vec<(BString, Mode, ObjectId)> = Vec::new();
    let mut deletes: Vec<BString> = Vec::new();
    // Each conflict carries git's per-entry reason: a worktree that no longer
    // matches the index is "not uptodate"; a staged divergence "would be
    // overwritten by merge" (unpack-trees.c `ERRORMSG`).
    let mut conflicts: BTreeSet<(BString, &'static str)> = BTreeSet::new();

    for path in &all {
        let i = index.get(path);
        let t = target.get(path);
        let act = if keep {
            classify_keep(head.get(path), t, i)
        } else {
            classify_merge(t, i)
        };
        match act {
            Act::Keep => {}
            Act::Delete => {
                if worktree_uptodate(repo, BStr::new(path), i.map(|(_, o)| *o)) {
                    deletes.push(path.clone());
                } else {
                    conflicts.insert((path.clone(), "not uptodate"));
                }
            }
            Act::Update => {
                let (tm, to) = *t.expect("update implies a target entry");
                let clean = match i {
                    Some((_, io)) => worktree_uptodate(repo, BStr::new(path), Some(*io)),
                    None => worktree_absent_or_matches(repo, BStr::new(path), to),
                };
                if clean {
                    updates.push((path.clone(), tm, to));
                } else {
                    conflicts.insert((path.clone(), "not uptodate"));
                }
            }
            Act::Conflict => {
                conflicts.insert((path.clone(), "would be overwritten by merge"));
            }
        }
    }

    // git's `unpack_trees` prints one `error:` line per conflicting entry, then the
    // caller (`reset_index`) prints the `fatal:` line and exits 128. Nothing is
    // written and HEAD is not moved.
    if !conflicts.is_empty() {
        for (path, reason) in &conflicts {
            eprintln!("error: Entry '{path}' {reason}. Cannot merge.");
        }
        return Ok(false);
    }

    // No conflicts: apply. Start from the old index so kept paths retain their
    // existing entry (and thus any staged content), then apply updates/deletes.
    let mut new_index = old.clone();
    let changed: HashSet<BString> = updates
        .iter()
        .map(|(p, _, _)| p.clone())
        .chain(deletes.iter().cloned())
        .collect();
    new_index.remove_entries(|_, path, _| changed.contains(path));
    for (p, mode, oid) in &updates {
        new_index.dangerously_push_entry(Stat::default(), *oid, Flags::empty(), *mode, BStr::new(p));
    }
    new_index.sort_entries();

    // Write the changed files to the worktree by checking out a filtered copy that
    // holds only the updated entries — kept files (with their local changes) are
    // never touched.
    if !updates.is_empty() {
        let upd: HashSet<BString> = updates.iter().map(|(p, _, _)| p.clone()).collect();
        let mut wt = new_index.clone();
        wt.remove_entries(|_, path, _| !upd.contains(path));
        let mut opts =
            repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
        opts.destination_is_initially_empty = false;
        opts.overwrite_existing = true;
        let odb = repo.objects.clone().into_arc()?;
        let discard_files = gix::progress::Discard;
        let discard_bytes = gix::progress::Discard;
        crate::worktree::checkout_subset(
            &mut wt,
            workdir.as_path(),
            odb,
            &discard_files,
            &discard_bytes,
            should_interrupt,
            opts,
        )?;
        // Copy the fresh stats back onto the persisted index so the just-written
        // files are not reported modified before the next refresh.
        let stat_map: HashMap<BString, Stat> = {
            let backing = wt.path_backing();
            wt.entries()
                .iter()
                .map(|e| (e.path_in(backing).to_owned(), e.stat))
                .collect()
        };
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            if let Some(stat) = stat_map.get(e.path_in(&backing)) {
                e.stat = *stat;
            }
        }
    }

    for p in &deletes {
        if let Some(full) = repo.workdir_path(BStr::new(p)) {
            let _ = std::fs::remove_file(full);
        }
    }

    new_index.remove_tree();
    new_index.write(Default::default())?;
    Ok(true)
}

/// A tree flattened to `path -> (mode, oid)`, via a throwaway index built from it.
fn tree_map(repo: &gix::Repository, tree: ObjectId) -> Result<HashMap<BString, (Mode, ObjectId)>> {
    let index = repo.index_from_tree(&tree)?;
    let backing = index.path_backing();
    Ok(index
        .entries()
        .iter()
        .map(|e| (e.path_in(backing).to_owned(), (e.mode, e.id)))
        .collect())
}

/// The stage-0 entries of `index` as `path -> (mode, oid)`.
fn index_entry_map(index: &gix::index::File) -> HashMap<BString, (Mode, ObjectId)> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .filter(|e| e.stage() == gix::index::entry::Stage::Unconflicted)
        .map(|e| (e.path_in(backing).to_owned(), (e.mode, e.id)))
        .collect()
}

/// Whether the worktree file at `path` still matches the index (`index_oid`), so
/// overwriting or removing it loses nothing. A missing file is up to date (git's
/// `verify_uptodate` returns 0 on `ENOENT`); an unreadable one is treated as
/// changed, so the reset errs on the side of aborting.
fn worktree_uptodate(repo: &gix::Repository, path: &BStr, index_oid: Option<ObjectId>) -> bool {
    let Some(full) = repo.workdir_path(path) else {
        return true;
    };
    let meta = match std::fs::symlink_metadata(&full) {
        Ok(m) => m,
        Err(_) => return true,
    };
    let Some(oid) = index_oid else {
        return true;
    };
    blob_oid(repo, &full, &meta) == Some(oid)
}

/// Whether it is safe to create `path` from the target: no worktree file exists,
/// or the one that does already matches the target content (no untracked data lost).
fn worktree_absent_or_matches(repo: &gix::Repository, path: &BStr, target_oid: ObjectId) -> bool {
    let Some(full) = repo.workdir_path(path) else {
        return true;
    };
    let meta = match std::fs::symlink_metadata(&full) {
        Ok(m) => m,
        Err(_) => return true,
    };
    blob_oid(repo, &full, &meta) == Some(target_oid)
}

/// The blob object id a worktree file would hash to (the link target for a
/// symlink), without writing it, for the up-to-date comparison.
fn blob_oid(
    repo: &gix::Repository,
    full: &std::path::Path,
    meta: &std::fs::Metadata,
) -> Option<ObjectId> {
    let data = if meta.file_type().is_symlink() {
        std::fs::read_link(full)
            .ok()?
            .into_os_string()
            .into_string()
            .ok()?
            .into_bytes()
    } else {
        std::fs::read(full).ok()?
    };
    gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &data).ok()
}

/// `git reset [<commit>] [--] <paths>` — build the index with the named entries (all
/// stages) reset to the target tree's version, or dropped if absent from the tree.
/// The worktree is untouched. A pathspec that matches nothing is not an error: git
/// only validates the *leading* positional, and that happens during setup.
///
/// With `intent_to_add`, a matched path that the reset would drop (present in the old
/// index but absent from the target tree) becomes an intent-to-add stub instead of
/// vanishing, matching `update_index_from_diff()`'s `!is_in_reset_tree` branch.
fn pathspec_index(
    repo: &gix::Repository,
    old: &gix::index::File,
    tree: ObjectId,
    paths: &[String],
    intent_to_add: bool,
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

    // Drop every stage of each selected path, then re-add the tree version if any;
    // a `-N` path missing from the tree comes back as an intent-to-add stub instead.
    index.remove_entries(|_, path, _| ops.contains(&path.to_owned()));
    let ita = ObjectId::empty_blob(repo.object_hash());
    for path in &ops {
        if let Some((stat, id, flags, mode)) = target_map.get(path) {
            index.dangerously_push_entry(*stat, *id, *flags, *mode, BStr::new(path));
        } else if intent_to_add {
            // EXTENDED must accompany INTENT_TO_ADD so the writer keeps the bit (V3).
            index.dangerously_push_entry(
                Stat::default(),
                ita,
                Flags::INTENT_TO_ADD | Flags::EXTENDED,
                Mode::FILE,
                BStr::new(path),
            );
        }
    }
    index.sort_entries();

    Ok(index)
}
