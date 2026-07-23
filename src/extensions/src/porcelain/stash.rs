//! `git stash` — save the dirty worktree/index to `refs/stash` and restore it.
//!
//! Ported onto the vendored gitoxide. The stash object model mirrors stock git
//! exactly: a stash is a merge commit `W` whose tree is the *worktree* snapshot,
//! whose first parent is `HEAD` (the base) and whose second parent is an *index*
//! commit `I` whose tree is the staged snapshot. Entries are tracked as reflog
//! lines on `refs/stash`, newest first (`stash@{0}`).
//!
//! ### What is implemented faithfully
//!
//! * `push` (the default, plus explicit `push` / `save`, with `-m/--message`):
//!   builds `I` and `W`, appends the `refs/stash` reflog entry, and resets the
//!   tracked worktree + index back to `HEAD`. Untracked files are left in place.
//! * `list`  — one `stash@{N}: <message>` line per reflog entry, newest first.
//! * `pop` / `apply` — restore the index to `I`'s tree and the worktree to `W`'s
//!   tree; `pop` then drops the entry. Restricted to the non-conflicting case
//!   (see below).
//! * `drop` / `clear` — remove one / all entries, rewriting the reflog exactly
//!   like `git reflog delete --rewrite --updateref`.
//!
//! ### Honest boundaries (precise bail, never fake success)
//!
//! * `-u/--include-untracked`, `-a/--all`, `-p/--patch`, `-k/--keep-index`,
//!   `-S/--staged`, pathspec-limited stashing, and `--index` on apply are not
//!   backed and bail with a message naming the unsupported flag.
//! * `apply`/`pop` only handle a clean apply: the current worktree+index must be
//!   clean and `HEAD` unchanged since the stash was made (guaranteed right after
//!   a `push`). A dirty target needs a real 3-way merge, which bails explicitly.
//! * Content blobs are produced through the repo filter pipeline, so CRLF /
//!   clean filters are honored just like git.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::diff::index::ChangeRef;
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::objs::tree::EntryKind;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

pub fn stash(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    match args.first().map(String::as_str) {
        None => {
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            push(&repo, None, false)
        }
        Some("push") => {
            let (msg, quiet) = parse_push_options(&args[1..])?;
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            push(&repo, msg, quiet)
        }
        Some("save") => {
            let msg = parse_save_message(&args[1..])?;
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            push(&repo, msg, false)
        }
        Some("list") => list(&repo),
        Some("pop") => {
            let n = parse_stash_index(positional(&args[1..]))?;
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            apply_or_pop(&repo, n, true)
        }
        Some("apply") => {
            reject_apply_flags(&args[1..])?;
            let n = parse_stash_index(positional(&args[1..]))?;
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            apply_or_pop(&repo, n, false)
        }
        Some("drop") => {
            let n = parse_stash_index(positional(&args[1..]))?;
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            let dropped = drop_reflog_entry(&repo, n)?;
            println!("Dropped stash@{{{n}}} ({dropped})");
            Ok(ExitCode::SUCCESS)
        }
        Some("clear") => {
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            clear(&repo)
        }
        Some("show") => bail!("`stash show` is not ported yet"),
        Some("branch") => bail!("`stash branch` is not ported yet"),
        Some("create") | Some("store") => bail!("`stash create`/`store` plumbing is not ported yet"),
        Some(flag) if flag.starts_with('-') => {
            // Implicit push with options, e.g. `git stash -m msg` or `git stash -u`.
            let (msg, quiet) = parse_push_options(args)?;
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            push(&repo, msg, quiet)
        }
        Some(other) => bail!("{other} is not a stash command"),
    }
}

/// `git stash push` — snapshot tracked changes, then reset the worktree+index to HEAD.
fn push(repo: &gix::Repository, message: Option<String>, quiet: bool) -> Result<ExitCode> {
    // An unborn HEAD has no base to stash against.
    if repo.head_id().is_err() {
        bail!("You do not have the initial commit yet");
    }

    // Untracked-only changes are not stashed without `-u`, matching git.
    if !repo.is_dirty()? {
        if !quiet {
            println!("No local changes to save");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let head_id = repo.head_id()?.detach();
    let head_tree_id = repo.head_tree_id()?.detach();
    let branch = match repo.head_name()? {
        Some(name) => name.shorten().to_string(),
        None => "(no branch)".to_string(),
    };
    let head_short = repo.head_id()?.shorten_or_id().to_string();
    let subject = repo.head_commit()?.message()?.summary().to_string();

    let stash_msg = match &message {
        Some(m) => format!("On {branch}: {m}"),
        None => format!("WIP on {branch}: {head_short} {subject}"),
    };
    let index_msg = format!("index on {branch}: {head_short} {subject}");

    // Collect staged (HEAD↔index) and unstaged (index↔worktree) tracked changes.
    // Rename tracking and the untracked dirwalk are disabled so only concrete
    // per-path additions/deletions/modifications are reported.
    let mut staged: Vec<ChangeRef<'static, 'static>> = Vec::new();
    let mut wt_mods: Vec<(BString, bool)> = Vec::new(); // (path, is_removed)
    {
        let iter = repo
            .status(gix::progress::Discard)?
            .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled)
            .index_worktree_rewrites(None)
            .untracked_files(gix::status::UntrackedFiles::None)
            .index_worktree_options_mut(|opts| opts.dirwalk_options = None)
            .into_iter(Vec::new())?;
        for item in iter {
            match item? {
                gix::status::Item::TreeIndex(change) => staged.push(change),
                gix::status::Item::IndexWorktree(iw) => {
                    use gix::status::index_worktree::Item as Iw;
                    use gix::status::plumbing::index_as_worktree::{Change as Wt, EntryStatus};
                    if let Iw::Modification { rela_path, status, .. } = iw {
                        match status {
                            EntryStatus::Change(Wt::Removed) => wt_mods.push((rela_path, true)),
                            EntryStatus::Change(Wt::Modification { .. })
                            | EntryStatus::Change(Wt::Type { .. })
                            | EntryStatus::Change(Wt::SubmoduleModification(_)) => {
                                wt_mods.push((rela_path, false));
                            }
                            EntryStatus::Conflict { .. } => {
                                bail!("cannot stash: unmerged (conflicted) entries present")
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Build the index tree `I` = HEAD tree + staged changes.
    let mut affected: HashSet<BString> = HashSet::new();
    let mut i_editor = repo.edit_tree(head_tree_id)?;
    for change in &staged {
        match change {
            ChangeRef::Addition { location, entry_mode, id, .. }
            | ChangeRef::Modification { location, entry_mode, id, .. } => {
                let path: BString = (**location).to_owned();
                let oid: ObjectId = (**id).to_owned();
                i_editor.upsert(path.as_bstr(), entry_kind(*entry_mode)?, oid)?;
                affected.insert(path);
            }
            ChangeRef::Deletion { location, .. } => {
                let path: BString = (**location).to_owned();
                i_editor.remove(path.as_bstr())?;
                affected.insert(path);
            }
            ChangeRef::Rewrite { source_location, location, entry_mode, id, .. } => {
                let source: BString = (**source_location).to_owned();
                let path: BString = (**location).to_owned();
                let oid: ObjectId = (**id).to_owned();
                i_editor.remove(source.as_bstr())?;
                i_editor.upsert(path.as_bstr(), entry_kind(*entry_mode)?, oid)?;
                affected.insert(source);
                affected.insert(path);
            }
        }
    }
    let i_tree_id = i_editor.write()?.detach();

    // Build the worktree tree `W` = `I` + unstaged worktree changes. Blobs are
    // produced through the filter pipeline so they are byte-identical to git's.
    let mut w_editor = repo.edit_tree(i_tree_id)?;
    if !wt_mods.is_empty() {
        let (mut pipeline, wt_index) = repo.filter_pipeline(None)?;
        for (path, removed) in &wt_mods {
            affected.insert(path.clone());
            if *removed {
                w_editor.remove(path.as_bstr())?;
            } else {
                match pipeline.worktree_file_to_object(path.as_bstr(), &wt_index)? {
                    Some((id, kind, _md)) => {
                        w_editor.upsert(path.as_bstr(), kind, id)?;
                    }
                    None => {
                        w_editor.remove(path.as_bstr())?;
                    }
                }
            }
        }
    }
    let w_tree_id = w_editor.write()?.detach();

    // `I` commit (parent: HEAD), then `W` merge commit (parents: HEAD, I).
    //
    // The newline asymmetry is git's, not a typo: `do_create_stash` terminates
    // the index commit's message but commits the stash message verbatim, so a
    // `W` commit body ends on the last byte of the message with no trailing LF.
    // Appending one here changes the commit id and diverges `refs/stash` plus
    // every object-listing probe from stock git. Verified byte-for-byte against
    // git 2.55.0 for the `push`, `push -m`, and `save` message forms.
    let index_commit = repo.new_commit(format!("{index_msg}\n"), i_tree_id, [head_id])?.id().detach();
    let w_commit = repo
        .new_commit(&stash_msg, w_tree_id, [head_id, index_commit])?
        .id()
        .detach();

    // Append the reflog entry and move refs/stash to the new W commit.
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: true,
                message: stash_msg.clone().into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(w_commit),
        },
        name: "refs/stash".try_into().map_err(|e| anyhow!("invalid ref name refs/stash: {e}"))?,
        deref: false,
    })?;

    // Reset the tracked worktree + index back to HEAD (untracked files untouched).
    let head_map = tree_map(repo, head_tree_id)?;
    let should_interrupt = AtomicBool::new(false);
    let fresh = sync_worktree(repo, head_tree_id, &affected, &head_map, &should_interrupt)?;
    let old_index = repo.open_index()?;
    write_target_index(repo, head_tree_id, &old_index, &fresh)?;

    if !quiet {
        println!("Saved working directory and index state {stash_msg}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git stash list` — newest first, `stash@{N}: <reflog message>`.
fn list(repo: &gix::Repository) -> Result<ExitCode> {
    for (i, (_, msg)) in read_stash_reflog(repo)?.iter().enumerate() {
        println!("stash@{{{i}}}: {msg}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git stash apply` / `pop` — restore `stash@{n}` onto a clean worktree+index.
fn apply_or_pop(repo: &gix::Repository, n: usize, pop: bool) -> Result<ExitCode> {
    let entries = read_stash_reflog(repo)?;
    if entries.is_empty() {
        bail!("No stash entries found.");
    }
    let commit_id = entries.get(n).map(|(id, _)| *id).ok_or_else(|| anyhow!("stash@{{{n}}} is not a valid reference"))?;

    let commit = repo.find_commit(commit_id)?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    if parents.len() < 2 {
        bail!("'{commit_id}' is not a stash-like commit");
    }
    if parents.len() > 2 {
        bail!("stash includes untracked files (created with -u); restoring those is not ported");
    }
    let base_tree = repo.find_commit(parents[0])?.tree_id()?.detach();
    let i_tree = repo.find_commit(parents[1])?.tree_id()?.detach();
    let w_tree = commit.tree_id()?.detach();

    // Only a non-conflicting apply is backed: the current tree must be clean and
    // still at the stash's base. A dirty/moved target needs a real 3-way merge.
    if repo.head_tree_id()?.detach() != base_tree {
        bail!("HEAD moved since the stash was created; 3-way merge apply is not ported");
    }
    if repo.is_dirty()? {
        bail!("worktree/index has local changes; only applying onto a clean tree is ported (3-way merge is not)");
    }

    // Worktree paths that differ between base and the stashed worktree tree.
    let base_map = tree_map(repo, base_tree)?;
    let w_map = tree_map(repo, w_tree)?;
    let mut affected: HashSet<BString> = HashSet::new();
    for (path, entry) in &w_map {
        if base_map.get(path) != Some(entry) {
            affected.insert(path.clone());
        }
    }
    for path in base_map.keys() {
        if !w_map.contains_key(path) {
            affected.insert(path.clone());
        }
    }

    let should_interrupt = AtomicBool::new(false);
    let fresh = sync_worktree(repo, w_tree, &affected, &w_map, &should_interrupt)?;
    let old_index = repo.open_index()?;
    write_target_index(repo, i_tree, &old_index, &fresh)?;

    if pop {
        let dropped = drop_reflog_entry(repo, n)?;
        println!("Dropped refs/stash@{{{n}}} ({dropped})");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git stash clear` — remove every entry (ref + reflog), silently if none.
fn clear(repo: &gix::Repository) -> Result<ExitCode> {
    let common = repo.common_dir();
    let _ = std::fs::remove_file(common.join("refs/stash"));
    let _ = std::fs::remove_file(common.join("logs/refs/stash"));
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Tree / index / worktree helpers
// ---------------------------------------------------------------------------

/// Convert an index entry mode to the tree entry kind used by the tree editor.
fn entry_kind(mode: Mode) -> Result<EntryKind> {
    Ok(mode
        .to_tree_entry_mode()
        .ok_or_else(|| anyhow!("index entry has an invalid mode"))?
        .into())
}

/// Flatten a tree into `path -> (blob id, mode)`.
fn tree_map(repo: &gix::Repository, tree_id: ObjectId) -> Result<HashMap<BString, (ObjectId, Mode)>> {
    let idx = repo.index_from_tree(&tree_id)?;
    let backing = idx.path_backing();
    let mut map = HashMap::with_capacity(idx.entries().len());
    for e in idx.entries() {
        map.insert(e.path_in(backing).to_owned(), (e.id, e.mode));
    }
    Ok(map)
}

/// Check out `affected` paths from `tree_id` into the worktree (overwriting),
/// deleting affected paths that don't exist in the target. Returns the fresh
/// filesystem stats produced for the written files, for index stat reuse.
fn sync_worktree(
    repo: &gix::Repository,
    tree_id: ObjectId,
    affected: &HashSet<BString>,
    target_map: &HashMap<BString, (ObjectId, Mode)>,
    should_interrupt: &AtomicBool,
) -> Result<HashMap<BString, Stat>> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    // Restrict a fresh target-tree index to just the affected, present paths.
    let mut subset = repo.index_from_tree(&tree_id)?;
    subset.remove_entries(|_, path, _| !affected.contains(&path.to_owned()));

    let mut opts = repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    crate::worktree::checkout_subset(
        &mut subset,
        workdir.as_path(),
        odb,
        &gix::progress::Discard,
        &gix::progress::Discard,
        should_interrupt,
        opts,
    )?;

    let mut fresh = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            fresh.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

    // Affected paths absent from the target tree are deletions.
    for path in affected {
        if !target_map.contains_key(path) {
            if let Some(full) = repo.workdir_path(path.as_bstr()) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    Ok(fresh)
}

/// Write the on-disk index to the state of `tree_id`, reusing `fresh` stats for
/// just-written files and the previous index stats for entries that didn't move,
/// so the next status check stays cheap.
fn write_target_index(
    repo: &gix::Repository,
    tree_id: ObjectId,
    old_index: &gix::index::File,
    fresh: &HashMap<BString, Stat>,
) -> Result<()> {
    let mut new_index = repo.index_from_tree(&tree_id)?;

    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> = HashMap::with_capacity(old_index.entries().len());
    {
        let backing = old_index.path_backing();
        for e in old_index.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = fresh.get(&path) {
                e.stat = *stat;
            } else if let Some((id, mode, stat)) = old_map.get(&path) {
                if *id == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }

    new_index.remove_tree();
    new_index.write(gix::index::write::Options::default())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Reflog helpers
// ---------------------------------------------------------------------------

/// Read `refs/stash` reflog entries newest-first as `(commit id, message)`.
fn read_stash_reflog(repo: &gix::Repository) -> Result<Vec<(ObjectId, BString)>> {
    let reference = match repo.try_find_reference("refs/stash")? {
        Some(r) => r,
        None => return Ok(Vec::new()),
    };
    let mut platform = reference.log_iter();
    let mut oldest_first: Vec<(ObjectId, BString)> = Vec::new();
    if let Some(iter) = platform.all()? {
        for line in iter {
            let line = line?;
            oldest_first.push((line.new_oid(), line.message.to_owned()));
        }
    }
    oldest_first.reverse();
    Ok(oldest_first)
}

/// Remove `stash@{n}` from the reflog, rewriting the chain and repointing the
/// ref, exactly like `git reflog delete --rewrite --updateref stash@{n}`.
/// Returns the dropped commit id.
fn drop_reflog_entry(repo: &gix::Repository, n: usize) -> Result<ObjectId> {
    let common = repo.common_dir();
    let log_path = common.join("logs/refs/stash");
    let ref_path = common.join("refs/stash");

    let data = std::fs::read(&log_path).map_err(|_| anyhow!("No stash entries found."))?;
    // Reflog lines are stored oldest-first, one per line.
    let mut lines: Vec<Vec<u8>> = data.split(|b| *b == b'\n').filter(|l| !l.is_empty()).map(<[u8]>::to_vec).collect();
    let len = lines.len();
    if n >= len {
        bail!("stash@{{{n}}} is not a valid reference");
    }
    let target = len - 1 - n; // stash@{0} is the last (newest) line

    let dropped = parse_new_oid(&lines[target])?;

    // Preserve chain consistency: the entry after the dropped one inherits the
    // dropped entry's previous oid (its new "old" side).
    if target + 1 < len {
        let prev = field_prev(&lines[target])?.to_vec();
        set_prev(&mut lines[target + 1], &prev)?;
    }
    lines.remove(target);

    if lines.is_empty() {
        let _ = std::fs::remove_file(&ref_path);
        let _ = std::fs::remove_file(&log_path);
    } else {
        let mut out = Vec::with_capacity(data.len());
        for l in &lines {
            out.extend_from_slice(l);
            out.push(b'\n');
        }
        std::fs::write(&log_path, &out)?;
        let newest = parse_new_oid(lines.last().expect("non-empty"))?;
        std::fs::write(&ref_path, format!("{newest}\n"))?;
    }

    Ok(dropped)
}

/// Byte offsets of the first two spaces in a reflog line (`<old> <new> …`).
fn split2(line: &[u8]) -> Result<(usize, usize)> {
    let s1 = line.iter().position(|b| *b == b' ').ok_or_else(|| anyhow!("malformed reflog line"))?;
    let s2 = line[s1 + 1..]
        .iter()
        .position(|b| *b == b' ')
        .map(|p| p + s1 + 1)
        .ok_or_else(|| anyhow!("malformed reflog line"))?;
    Ok((s1, s2))
}

fn parse_new_oid(line: &[u8]) -> Result<ObjectId> {
    let (s1, s2) = split2(line)?;
    ObjectId::from_hex(&line[s1 + 1..s2]).map_err(|e| anyhow!("invalid oid in reflog: {e}"))
}

fn field_prev(line: &[u8]) -> Result<&[u8]> {
    let (s1, _) = split2(line)?;
    Ok(&line[..s1])
}

fn set_prev(line: &mut Vec<u8>, prev: &[u8]) -> Result<()> {
    let (s1, _) = split2(line)?;
    line.splice(0..s1, prev.iter().copied());
    Ok(())
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

/// First non-flag argument, if any.
fn positional(args: &[String]) -> Option<&str> {
    args.iter().find(|a| !a.starts_with('-')).map(String::as_str)
}

/// Parse a `stash@{N}` / `refs/stash@{N}` / bare `N` reference to its index.
/// Missing spec defaults to `stash@{0}`.
fn parse_stash_index(spec: Option<&str>) -> Result<usize> {
    let s = match spec {
        None => return Ok(0),
        Some(s) => s.trim(),
    };
    let inner = s.strip_prefix("stash@{").or_else(|| s.strip_prefix("refs/stash@{"));
    if let Some(rest) = inner {
        let num = rest.strip_suffix('}').ok_or_else(|| anyhow!("{s} is not a valid reference"))?;
        return num.parse::<usize>().map_err(|_| anyhow!("{s} is not a valid reference"));
    }
    s.parse::<usize>().map_err(|_| anyhow!("{s} is not a valid stash reference"))
}

/// Parse `push` options, returning the optional message and the quiet flag.
/// Unsupported flags and pathspecs bail with a precise message.
fn parse_push_options(args: &[String]) -> Result<(Option<String>, bool)> {
    let mut message = None;
    let mut quiet = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-m" | "--message" => {
                i += 1;
                let m = args.get(i).ok_or_else(|| anyhow!("option '{a}' requires a value"))?;
                message = Some(m.clone());
            }
            "-q" | "--quiet" => quiet = true,
            "-u" | "--include-untracked" | "--only-untracked" => {
                bail!("--include-untracked is not ported")
            }
            "-a" | "--all" => bail!("--all is not ported"),
            "-p" | "--patch" => bail!("--patch is not ported"),
            "-k" | "--keep-index" | "--no-keep-index" => bail!("--keep-index is not ported"),
            "-S" | "--staged" => bail!("--staged is not ported"),
            "--" => {
                if i + 1 < args.len() {
                    bail!("pathspec-limited stashing is not ported");
                }
            }
            other => {
                if let Some(m) = other.strip_prefix("--message=") {
                    message = Some(m.to_string());
                } else if let Some(m) = other.strip_prefix("-m") {
                    message = Some(m.to_string());
                } else if other.starts_with('-') {
                    bail!("unsupported stash option '{other}'");
                } else {
                    bail!("pathspec-limited stashing is not ported");
                }
            }
        }
        i += 1;
    }
    Ok((message, quiet))
}

/// `save` takes its message as positional words (plus the same rejected flags).
fn parse_save_message(args: &[String]) -> Result<Option<String>> {
    let mut words = Vec::new();
    for a in args {
        match a.as_str() {
            "-q" | "--quiet" => {}
            "-u" | "--include-untracked" => bail!("--include-untracked is not ported"),
            "-a" | "--all" => bail!("--all is not ported"),
            "-p" | "--patch" => bail!("--patch is not ported"),
            "-k" | "--keep-index" | "--no-keep-index" => bail!("--keep-index is not ported"),
            other if other.starts_with('-') => bail!("unsupported stash option '{other}'"),
            other => words.push(other.to_string()),
        }
    }
    Ok(if words.is_empty() { None } else { Some(words.join(" ")) })
}

/// `apply` shares `pop`'s restrictions; `--index` needs staged-state restore.
fn reject_apply_flags(args: &[String]) -> Result<()> {
    for a in args {
        match a.as_str() {
            "--index" => bail!("`--index` (restoring the staged state separately) is not ported"),
            "-p" | "--patch" => bail!("--patch is not ported"),
            _ => {}
        }
    }
    Ok(())
}
