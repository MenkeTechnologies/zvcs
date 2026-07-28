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
//!   tracked worktree + index back to `HEAD`. Untracked files are left in place
//!   unless `-u`/`-a` asks for them.
//! * Pathspec-limited pushes (`-- <pathspec>…`, `--pathspec-from-file`,
//!   `--pathspec-file-nul`), over the same engine as `ls-files`/`grep`, so
//!   `:(glob)`/`:(icase)`/`:!` behave identically. A pathspec narrows the
//!   worktree tree `W` and the set of paths reset — never the index commit `I`,
//!   which git always captures whole, so a staged change to an unmatched path
//!   rides along in the stash and stays staged afterwards.
//! * `-k/--keep-index`, which resets to `I` instead of `HEAD` and so leaves the
//!   staged state staged with its content on disk.
//! * `-S/--staged`, taking the index diff alone and leaving unstaged work.
//! * `-u/--include-untracked` and `-a/--all`, capturing untracked (and for
//!   `-a`, ignored) files into a parentless third parent and deleting them from
//!   the worktree. Refused together with `-S`, as git refuses it.
//! * `list`  — one `stash@{N}: <message>` line per reflog entry, newest first.
//! * `pop` / `apply` — restore the worktree to `W`'s tree, plus any untracked
//!   files captured in the third parent (which come back untracked, and never
//!   overwrite a file already sitting there); `pop` then drops the entry. Restricted to the non-conflicting case (see below). The staged state
//!   is restored only under `--index` (or `stash.index=true`, which is exactly
//!   that option's default): git's plain `apply` leaves everything *unstaged*,
//!   resetting the index to the stash's base. `--no-index` countermands the
//!   config. Both then run `git status` unless `-q`/`--quiet` was given, which
//!   also silences `pop`'s `Dropped …` line.
//! * `drop` / `clear` — remove one / all entries, rewriting the reflog exactly
//!   like `git reflog delete --rewrite --updateref`.
//! * `create` — build the `I`/`W` commit graph and print the `W` id without
//!   storing it or touching the worktree (`do_create_stash`).
//! * `store` — point `refs/stash` at an existing stash-like commit, appending
//!   the reflog entry (`do_store_stash`).
//! * `show` — diff the stash base tree against its worktree tree, formatted per
//!   the diff options / `stash.showStat`+`stash.showPatch` config; rendering is
//!   delegated to the `diff` porcelain (git's own `diff_tree_oid` machinery).
//! * `branch` — create+check out a branch at the stash base, apply there, drop.
//! * `create_autostash` / `apply_autostash` — the rebase/pull `--autostash`
//!   helpers. Unlike `apply`/`pop`, the re-apply here IS a real three-way merge
//!   (`merge_apply::three_way_merge`) of the stash onto the moved `HEAD`, since
//!   autostash by definition re-applies over a tree the rebase/merge advanced.
//!
//! ### Honest boundaries (precise bail, never fake success)
//!
//! * `-p/--patch` is not backed and bails. It runs the hunk selector against a
//!   scratch index (`.git/stash-index`, seeded from `HEAD` and named by
//!   `GIT_INDEX_FILE`); nothing in this port honors `GIT_INDEX_FILE`, so the
//!   selector would stage into the REAL index and corrupt the user's staged
//!   state. The selector itself (`super::add_patch`) is ready — only the
//!   scratch-index plumbing is missing.
//! * `stash show -u` does not render the untracked (`^3`) tree, so
//!   `stash.showIncludeUntracked` is still unread — `push -u` writes that tree
//!   and `apply`/`pop` restore it, but `show` does not display it.
//! * `stash.index` is honored for `apply`/`pop` (and `branch`, which git always
//!   applies with the index). git also lets it reach the `--autostash` re-apply
//!   of `merge`/`rebase`/`pull`; that path always behaves as `--no-index` here,
//!   because restoring the staged state across a *moved* `HEAD` needs a second
//!   three-way merge of the index tree that is not ported.
//! * `apply`/`pop` only handle a clean apply: the current worktree+index must be
//!   clean and `HEAD` unchanged since the stash was made (guaranteed right after
//!   a `push`). A dirty target needs a real 3-way merge, which bails explicitly.
//! * Content blobs are produced through the repo filter pipeline, so CRLF /
//!   clean filters are honored just like git.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
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
            push(&repo, &PushOpts::with_message(None))
        }
        Some("push") => {
            let opts = parse_push_options(&args[1..])?;
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            push(&repo, &opts)
        }
        Some("save") => {
            let msg = parse_save_message(&args[1..])?;
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            push(&repo, &PushOpts::with_message(msg))
        }
        Some("list") => list(&repo, &args[1..]),
        Some("pop") => {
            let opts = parse_apply_options(&repo, &args[1..])?;
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            apply_or_pop(&repo, &opts, true)
        }
        Some("apply") => {
            let opts = parse_apply_options(&repo, &args[1..])?;
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            apply_or_pop(&repo, &opts, false)
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
        Some("show") => show_stash(&repo, &args[1..]),
        Some("branch") => {
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            branch_stash(&repo, &args[1..])
        }
        Some("create") => create_stash(&repo, &args[1..]),
        Some("store") => {
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            store_stash(&repo, &args[1..])
        }
        Some(flag) if flag.starts_with('-') => {
            // Implicit push with options, e.g. `git stash -m msg` or `git stash -u`.
            let opts = parse_push_options(args)?;
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            push(&repo, &opts)
        }
        Some(other) => bail!("{other} is not a stash command"),
    }
}

/// `git stash push` — snapshot tracked changes, then reset the worktree+index to HEAD.
fn push(repo: &gix::Repository, opts: &PushOpts) -> Result<ExitCode> {
    // An unborn HEAD has no base to stash against.
    if repo.head_id().is_err() {
        bail!("You do not have the initial commit yet");
    }

    // A pathspec naming nothing git tracks is an error, before any work — git
    // reports the normalized spec, magic prefix and all.
    if !opts.pathspecs.is_empty() {
        if let Some(spec) = first_unmatched_spec(repo, &opts.pathspecs)? {
            bail!("pathspec '{spec}' did not match any file(s) known to git");
        }
    }

    // Untracked-only changes are not stashed without `-u`, matching git.
    let untracked_wanted = opts.untracked != Untracked::No;
    if !repo.is_dirty()? && !untracked_wanted {
        if !opts.quiet {
            println!("No local changes to save");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let StashBuild { w_commit, stash_msg, head_tree_id, i_tree_id, affected, untracked_paths } =
        build_stash_commit(repo, opts.message.as_deref(), opts)?;

    // Nothing survived the filter: a pathspec that matched only clean paths, or
    // `-S` with an empty index diff. git distinguishes the two.
    if affected.is_empty() && untracked_paths.is_empty() {
        if opts.staged_only {
            bail!("No staged changes");
        }
        if !opts.quiet {
            println!("No local changes to save");
        }
        return Ok(ExitCode::SUCCESS);
    }

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

    // Reset the tracked worktree + index (untracked files handled separately
    // below). `--keep-index` resets to the index tree rather than HEAD, which is
    // precisely what leaves the staged changes staged and their content on disk.
    let worktree_target = if opts.keep_index { i_tree_id } else { head_tree_id };
    // The index is rebuilt wholesale from its target tree, so that tree has to
    // describe EVERY path — not just the reset ones. Starting from `I` (what the
    // index holds right now) and reverting only the affected paths is what keeps
    // a staged change to an unmatched path staged, which is what git does under
    // a pathspec. Reverting nothing is `--keep-index`.
    let index_target = if opts.keep_index {
        i_tree_id
    } else {
        revert_paths_in_tree(repo, i_tree_id, head_tree_id, &affected)?
    };
    let target_map = tree_map(repo, worktree_target)?;
    let should_interrupt = AtomicBool::new(false);
    let fresh = sync_worktree(repo, worktree_target, &affected, &target_map, &should_interrupt)?;
    let old_index = repo.open_index()?;
    write_target_index(repo, index_target, &old_index, &fresh)?;

    // The untracked files now live in the stash's third parent, so git removes
    // them from the worktree. Empty parent directories are left behind, as git
    // leaves them.
    let workdir = repo.workdir().map(std::path::Path::to_path_buf);
    if let Some(root) = workdir {
        for path in &untracked_paths {
            let abs = root.join(gix::path::from_bstr(path.as_bstr()).as_ref());
            let _ = std::fs::remove_file(&abs);
        }
    }

    if !opts.quiet {
        println!("Saved working directory and index state {stash_msg}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Pathspec matching for a push, over the same `repo.pathspec()` engine `rm`,
/// `grep` and `jump` use — so `:(glob)`, `:(icase)` and `:!` behave identically
/// here and a bare `*` keeps crossing `/` the way git's pathspecs do.
/// Of `candidates`, the paths any spec selects.
///
/// The engine is built once for the whole batch: `repo.pathspec()` borrows both
/// the repository and the index, so a per-path matcher would either re-open the
/// index on every call or need a self-referential struct.
fn select_matching(
    repo: &gix::Repository,
    specs: &[String],
    candidates: &[BString],
) -> Result<HashSet<BString>> {
    let index = repo.open_index()?;
    let patterns: Vec<BString> = specs.iter().map(|s| BString::from(s.as_str())).collect();
    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;
    let mut out = HashSet::new();
    for path in candidates {
        if ps.pattern_matching_relative_path(path.as_bstr(), Some(false)).is_some() {
            out.insert(path.clone());
        }
    }
    Ok(out)
}

/// The first spec naming nothing in the index, in git's normalized form
/// (`:(prefix:0)<path>`), or `None` when every spec matched something.
///
/// Exclusions select by removing, so they are never reported as unmatched.
fn first_unmatched_spec(repo: &gix::Repository, specs: &[String]) -> Result<Option<String>> {
    let index = repo.open_index()?;
    let backing = index.path_backing();
    for raw in specs {
        if raw.starts_with(":!") || raw.starts_with(":^") {
            continue;
        }
        let one = [BString::from(raw.as_str())];
        let mut ps = repo.pathspec(
            true,
            &one,
            false,
            &index,
            gix::worktree::stack::state::attributes::Source::IdMapping,
        )?;
        let hit = index
            .entries()
            .iter()
            .any(|e| ps.pattern_matching_relative_path(e.path_in(backing), Some(false)).is_some());
        if !hit {
            return Ok(Some(format!(":(prefix:0){raw}")));
        }
    }
    Ok(None)
}

/// Untracked files a `-u`/`-a` push takes, restricted by any pathspec.
///
/// `-a` additionally takes ignored files. Directories are walked into rather
/// than captured whole, since the third parent's tree stores blobs by path.
fn collect_untracked(repo: &gix::Repository, opts: &PushOpts) -> Result<Vec<BString>> {
    let index = repo.open_index()?;
    let patterns: Vec<BString> =
        opts.pathspecs.iter().map(|s| BString::from(s.as_str())).collect();
    let want_ignored = opts.untracked == Untracked::All;
    let options = repo
        .dirwalk_options()?
        .emit_ignored(want_ignored.then_some(gix::dir::walk::EmissionMode::Matching))
        .emit_untracked(gix::dir::walk::EmissionMode::Matching);

    let mut out: Vec<BString> = Vec::new();
    let mut collect = repo.dirwalk_iter(index, patterns, Default::default(), options)?;
    for item in &mut collect {
        let entry = item?.entry;
        match entry.status {
            gix::dir::entry::Status::Untracked => {}
            gix::dir::entry::Status::Ignored(_) if want_ignored => {}
            _ => continue,
        }
        // Only regular files land in the tree; a nested repository is left alone.
        if entry.disk_kind == Some(gix::dir::entry::Kind::Directory)
            || entry.disk_kind == Some(gix::dir::entry::Kind::Repository)
        {
            continue;
        }
        out.push(entry.rela_path.clone());
    }
    out.sort();
    Ok(out)
}

/// The stash object graph produced from the current tracked changes: the `W`
/// merge commit plus the data `push`/`create` need afterwards.
struct StashBuild {
    /// The stash (`W`) merge commit id — the value stored in `refs/stash`.
    w_commit: ObjectId,
    /// The reflog / commit message (`WIP on …` or `On <branch>: …`).
    stash_msg: String,
    /// HEAD's tree, used by `push` to reset the worktree/index afterwards.
    head_tree_id: ObjectId,
    /// The index tree `I`. `--keep-index` resets to this instead of HEAD, which
    /// is what leaves the staged changes staged.
    i_tree_id: ObjectId,
    /// Every path touched by the staged/unstaged changes, for the reset step.
    affected: HashSet<BString>,
    /// Untracked (and with `-a`, ignored) files captured into the third parent,
    /// which `push` deletes from the worktree afterwards.
    untracked_paths: Vec<BString>,
}

/// Build the stash commit graph (`I` index commit then `W` merge commit) from
/// the current tracked staged + unstaged changes, without touching `refs/stash`
/// or the worktree. Faithful port of `do_create_stash` (git builtin/stash.c),
/// shared by `push` (which then stores + resets) and `create` (which prints the
/// `W` id). The caller has already verified HEAD is born and the tree is dirty.
fn build_stash_commit(
    repo: &gix::Repository,
    message: Option<&str>,
    opts: &PushOpts,
) -> Result<StashBuild> {
    let head_id = repo.head_id()?.detach();
    let head_tree_id = repo.head_tree_id()?.detach();
    let branch = match repo.head_name()? {
        Some(name) => name.shorten().to_string(),
        None => "(no branch)".to_string(),
    };
    let head_short = repo.head_id()?.shorten_or_id().to_string();
    let subject = repo.head_commit()?.message()?.summary().to_string();

    let stash_msg = match message {
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

    // A pathspec narrows which *worktree* changes are taken and which paths are
    // reset — never the index tree above, which git always captures whole (a
    // staged change to an unmatched path still rides along in `I`, and stays
    // staged in the worktree afterwards).
    //
    // `-S` takes the staged changes alone, so the unstaged set is dropped
    // entirely and `W` ends up identical to `I`.
    if opts.staged_only {
        wt_mods.clear();
    }
    if !opts.pathspecs.is_empty() {
        let mut candidates: Vec<BString> = wt_mods.iter().map(|(p, _)| p.clone()).collect();
        candidates.extend(affected.iter().cloned());
        let keep = select_matching(repo, &opts.pathspecs, &candidates)?;
        wt_mods.retain(|(path, _)| keep.contains(path));
        affected.retain(|path| keep.contains(path));
    }

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

    // `-u`/`-a` capture the untracked files into a THIRD parent: a parentless
    // commit whose tree is just those files (`do_create_stash`'s `u_commit`).
    // They are not part of `I` or `W` — `stash pop` restores them from this
    // commit — and `push` deletes them from the worktree afterwards.
    let mut untracked_paths: Vec<BString> = Vec::new();
    let mut parents = vec![head_id, index_commit];
    if opts.untracked != Untracked::No {
        untracked_paths = collect_untracked(repo, opts)?;
        if !untracked_paths.is_empty() {
            let (mut pipeline, wt_index) = repo.filter_pipeline(None)?;
            let empty = repo.empty_tree().id().detach();
            let mut u_editor = repo.edit_tree(empty)?;
            for path in &untracked_paths {
                if let Some((id, kind, _md)) =
                    pipeline.worktree_file_to_object(path.as_bstr(), &wt_index)?
                {
                    u_editor.upsert(path.as_bstr(), kind, id)?;
                }
            }
            let u_tree_id = u_editor.write()?.detach();
            let u_msg = format!("untracked files on {branch}: {head_short} {subject}");
            // Parentless, like git's `u_commit`: the untracked tree stands alone.
            let no_parents: Vec<ObjectId> = Vec::new();
            let u_commit =
                repo.new_commit(format!("{u_msg}\n"), u_tree_id, no_parents)?.id().detach();
            parents.push(u_commit);
        } else {
            untracked_paths.clear();
        }
    }

    let w_commit = repo.new_commit(stash_msg.as_str(), w_tree_id, parents)?.id().detach();

    Ok(StashBuild { w_commit, stash_msg, head_tree_id, i_tree_id, affected, untracked_paths })
}

/// `git stash create [<message>…]` — build the stash commit graph and print the
/// `W` commit id without storing it or touching the worktree. Port of
/// `create_stash` (builtin/stash.c): the message is every remaining arg joined
/// by a space (no option parsing), and a clean tree prints nothing (exit 0).
fn create_stash(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    if repo.head_id().is_err() {
        bail!("You do not have the initial commit yet");
    }
    // `check_changes_tracked_files`: no tracked changes → nothing to create.
    if !repo.is_dirty()? {
        return Ok(ExitCode::SUCCESS);
    }
    let message = if args.is_empty() { None } else { Some(args.join(" ")) };
    let built = build_stash_commit(repo, message.as_deref(), &PushOpts::with_message(None))?;
    println!("{}", built.w_commit);
    Ok(ExitCode::SUCCESS)
}

/// `git stash store [-m <msg>] [-q] <commit>` — point `refs/stash` at an existing
/// stash-like commit, appending the reflog entry. Port of `do_store_stash`.
fn store_stash(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let (message, quiet, commit) = parse_store_options(args)?;
    let oid = match repo.rev_parse_single(commit.as_str()) {
        Ok(id) => id.detach(),
        Err(_) => {
            if !quiet {
                eprintln!("Cannot update refs/stash with {commit}");
            }
            return Ok(ExitCode::FAILURE);
        }
    };
    let stash_msg = message.unwrap_or_else(|| "Created via \"git stash store\".".to_string());
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: true,
                message: stash_msg.into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(oid),
        },
        name: "refs/stash".try_into().map_err(|e| anyhow!("invalid ref name refs/stash: {e}"))?,
        deref: false,
    })?;
    Ok(ExitCode::SUCCESS)
}

/// `git stash show [<diff-options>] [<stash>]` — show the change the stash would
/// apply. Port of `show_stash`: the diff is always `b_commit`→`w_commit` (base
/// tree vs. stashed worktree tree); the user's diff options only pick the format.
/// With no options the `stash.showStat` (default on) / `stash.showPatch` config
/// decides. Delegates the actual rendering to the `diff` porcelain, which is the
/// same machinery git's `diff_tree_oid` drives, so output is byte-identical.
fn show_stash(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut diff_flags: Vec<String> = Vec::new();
    let mut stash_spec: Option<String> = None;
    for a in args {
        match a.as_str() {
            "-u" | "--include-untracked" | "--only-untracked" => {
                // These need the untracked (`^3`) tree, which is not stored.
                bail!("showing untracked files in a stash is not ported");
            }
            s if s.starts_with('-') && s != "-" => diff_flags.push(a.clone()),
            _ => {
                if stash_spec.is_some() {
                    bail!("Too many revisions specified");
                }
                stash_spec = Some(a.clone());
            }
        }
    }

    // Resolve the stash: a `stash@{n}` / bare `N` / (default) `stash@{0}` goes
    // through the reflog; anything else is resolved as an arbitrary stash-like
    // commit, matching git's `get_stash_info`.
    let commit_id = if let Ok(n) = parse_stash_index(stash_spec.as_deref()) {
        let entries = read_stash_reflog(repo)?;
        if entries.is_empty() {
            bail!("No stash entries found.");
        }
        entries
            .get(n)
            .map(|(id, _)| *id)
            .ok_or_else(|| anyhow!("{} is not a valid reference", stash_spec.as_deref().unwrap_or("stash@{0}")))?
    } else {
        let s = stash_spec.as_deref().expect("non-index spec is Some");
        repo.rev_parse_single(s).map_err(|_| anyhow!("{s} is not a valid reference"))?.detach()
    };

    let commit = repo.find_commit(commit_id)?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    if parents.len() < 2 {
        bail!("'{commit_id}' is not a stash-like commit");
    }
    let b_commit = parents[0];

    // No diff options given → apply config defaults (git: revision_args.nr == 1).
    if diff_flags.is_empty() {
        let snap = repo.config_snapshot();
        let show_stat = snap.boolean("stash.showStat").unwrap_or(true);
        let show_patch = snap.boolean("stash.showPatch").unwrap_or(false);
        if !show_stat && !show_patch {
            return Ok(ExitCode::SUCCESS);
        }
        if show_stat {
            diff_flags.push("--stat".to_string());
        }
        if show_patch {
            diff_flags.push("-p".to_string());
        }
    }

    diff_flags.push(b_commit.to_string());
    diff_flags.push(commit_id.to_string());
    super::diff::diff(&diff_flags)
}

/// `git stash branch <branchname> [<stash>]` — create and check out `<branchname>`
/// at the stash's base commit, apply the stash there (so a stash made on a since-
/// changed branch applies cleanly), then drop it. Port of `branch_stash`.
fn branch_stash(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut positionals: Vec<&str> = Vec::new();
    for a in args {
        if a.starts_with('-') && a.as_str() != "-" {
            bail!("unsupported stash branch option '{a}'");
        }
        positionals.push(a);
    }
    let branch = match positionals.first() {
        Some(b) => (*b).to_string(),
        None => bail!("No branch name specified"),
    };
    let n = parse_stash_index(positionals.get(1).copied())?;

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
    let b_commit = parents[0];

    // `git checkout -b <branch> <b_commit>`; only apply if the checkout succeeds.
    super::checkout::checkout(&["-b".to_string(), branch, b_commit.to_string()])?;

    // The checkout moved HEAD/worktree on disk; re-open so the restore sees it.
    // `branch_stash` calls `do_apply_stash(..., 1, ...)`: the index is always
    // restored here, whatever `stash.index` says, so the staged state a stash
    // captured comes back staged on the new branch.
    let repo = gix::discover(".")?;
    restore_stash_commit(&repo, commit_id, true)?;

    // `do_apply_stash` ends by running `git status` (non-quiet).
    super::status::status(&[])?;

    // The stash came from `refs/stash`, so `is_stash_ref` holds: drop it.
    let dropped = drop_reflog_entry(&repo, n)?;
    println!("Dropped refs/stash@{{{n}}} ({dropped})");
    Ok(ExitCode::SUCCESS)
}

/// `git stash list` — newest first, `stash@{N}: <reflog message>`.
fn list(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    // `git stash list` is `git log --format="%gd: %gs" -g <log-opts> refs/stash`,
    // so delegate to the reflog machinery on `refs/stash` rather than duplicate its
    // format engine (`%H`, `%gd`, `%gs`, dates, `--pretty`, …). Only the default
    // format differs: stash uses `%gd: %gs`, injected when the caller gives none.
    // With no stash ref, git prints nothing (exit 0) — reflog would instead fatal
    // on an unknown ref, so short-circuit that here.
    if repo.try_find_reference("refs/stash")?.is_none() {
        return Ok(ExitCode::SUCCESS);
    }
    let has_format = args
        .iter()
        .any(|a| a.starts_with("--format") || a.starts_with("--pretty"));
    let mut rf: Vec<String> = vec!["show".into()];
    if !has_format {
        rf.push("--format=%gd: %gs".into());
    }
    rf.extend(args.iter().cloned());
    rf.push("refs/stash".into());
    super::reflog(&rf)
}

/// `git stash apply` / `pop` — restore `stash@{n}` onto a clean worktree+index.
///
/// Port of `do_apply_stash`'s tail: the restore, then `git status` (skipped under
/// `-q`), and for `pop` the `Dropped …` line — which `-q` silences too.
fn apply_or_pop(repo: &gix::Repository, opts: &ApplyOptions, pop: bool) -> Result<ExitCode> {
    let entries = read_stash_reflog(repo)?;
    if entries.is_empty() {
        bail!("No stash entries found.");
    }
    let n = opts.index_in_stash;
    let commit_id = entries.get(n).map(|(id, _)| *id).ok_or_else(|| anyhow!("stash@{{{n}}} is not a valid reference"))?;

    restore_stash_commit(repo, commit_id, opts.restore_index)?;

    if !opts.quiet {
        super::status::status(&[])?;
    }
    if pop {
        let dropped = drop_reflog_entry(repo, n)?;
        if !opts.quiet {
            println!("Dropped refs/stash@{{{n}}} ({dropped})");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Restore the worktree to the stash's `W` tree, for the clean-apply case only:
/// the current tree must be clean and still at the stash's base (`b_tree`). A
/// dirty/moved target needs a real 3-way merge, which is not backed. Shared by
/// `apply`/`pop` and `branch` (where the prior checkout guarantees the clean
/// base).
///
/// `restore_index` is `do_apply_stash`'s `index` argument: with it the index is
/// rebuilt from the stash's `I` (staged) tree, so what was staged when the stash
/// was made is staged again; without it the index is reset to the stash's base,
/// which is what leaves every restored change *unstaged* — git's default, and
/// the reason a plain `git stash apply` reports ` M` rather than `M ` for a path
/// that had been `git add`ed. `git stash branch` always passes `true`.
fn restore_stash_commit(repo: &gix::Repository, commit_id: ObjectId, restore_index: bool) -> Result<()> {
    let commit = repo.find_commit(commit_id)?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    if parents.len() < 2 {
        bail!("'{commit_id}' is not a stash-like commit");
    }
    // A third parent is the untracked capture written by `push -u`/`-a`; its
    // tree is restored to the worktree after the tracked paths, below.
    let u_tree = match parents.get(2) {
        Some(id) => Some(repo.find_commit(*id)?.tree_id()?.detach()),
        None => None,
    };
    if parents.len() > 3 {
        bail!("'{commit_id}' is not a stash-like commit");
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
    let index_tree = if restore_index { i_tree } else { base_tree };
    write_target_index(repo, index_tree, &old_index, &fresh)?;

    // Untracked files come back last and stay untracked — their stats are
    // deliberately dropped rather than fed to the index. git refuses to clobber
    // an existing file here, and checks every path before writing any of them so
    // a collision cannot leave the restore half-done.
    if let Some(u_tree) = u_tree {
        let u_map = tree_map(repo, u_tree)?;
        for path in u_map.keys() {
            if let Some(full) = repo.workdir_path(path.as_bstr()) {
                if full.exists() {
                    bail!("could not restore untracked file from stash: {path}");
                }
            }
        }
        let u_paths: HashSet<BString> = u_map.keys().cloned().collect();
        let _ = sync_worktree(repo, u_tree, &u_paths, &u_map, &should_interrupt)?;
    }
    Ok(())
}

/// Create an *autostash* from the current dirty worktree+index: build the
/// stash-like `W` commit (as `git stash create` does) and reset the tracked
/// worktree and index back to `HEAD`, leaving a clean tree. Returns the `W`
/// commit id. Unlike [`push`] it never touches `refs/stash` — the caller
/// (rebase/pull `--autostash`) owns the commit and re-applies it directly via
/// [`apply_autostash`]. The caller has already checked the tree is dirty.
pub fn create_autostash(repo: &gix::Repository) -> Result<ObjectId> {
    let StashBuild { w_commit, head_tree_id, affected, .. } =
        build_stash_commit(repo, Some("autostash"), &PushOpts::with_message(None))?;

    // Reset the tracked worktree + index back to HEAD (untracked files untouched),
    // exactly as `push` does after storing the stash.
    let head_map = tree_map(repo, head_tree_id)?;
    let should_interrupt = AtomicBool::new(false);
    let fresh = sync_worktree(repo, head_tree_id, &affected, &head_map, &should_interrupt)?;
    let old_index = repo.open_index()?;
    write_target_index(repo, head_tree_id, &old_index, &fresh)?;
    Ok(w_commit)
}

/// Re-apply an autostash `W` commit onto the *current* `HEAD` with a real
/// three-way merge — the case `apply`/`pop` refuse. The three sides are the
/// stash's base (`W`'s first parent tree = `HEAD` when the stash was made),
/// *ours* (the current `HEAD` tree, e.g. the just-rebased tip), and *theirs*
/// (the stashed worktree tree `W`). Prints git's `Applied autostash.` on a clean
/// apply, or the conflict notice (leaving the changes recoverable) otherwise.
/// Returns the conflicted paths (empty on a clean apply).
pub fn apply_autostash(repo: &gix::Repository, commit_id: ObjectId, quiet: bool) -> Result<Vec<BString>> {
    let commit = repo.find_commit(commit_id)?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    if parents.len() < 2 {
        bail!("'{commit_id}' is not a stash-like commit");
    }
    let base = repo.find_commit(parents[0])?.tree_id()?.detach();
    let theirs = commit.tree_id()?.detach();
    let ours = repo.head_tree_id()?.detach();
    let old_index = repo.index_or_load_from_head()?.into_owned();

    let labels = gix::merge::blob::builtin_driver::text::Labels {
        ancestor: Some(BStr::new(b"stash base")),
        current: Some(BStr::new(b"HEAD")),
        other: Some(BStr::new(b"Stashed changes")),
    };
    let should_interrupt = AtomicBool::new(false);
    let applied = crate::merge_apply::three_way_merge(
        repo, base, ours, theirs, &old_index, labels, &should_interrupt,
    )?;

    if applied.conflicts.is_empty() {
        // three_way_merge already wrote the merged content to the worktree. git's
        // autostash re-applies with `stash apply` (no `--index`), so the restored
        // changes stay UNSTAGED: reset the index to HEAD rather than persisting the
        // merged index, leaving worktree-vs-index as the user's local changes.
        let mut head_index = repo.index_from_tree(&ours)?;
        head_index.write(Default::default())?;
        if !quiet {
            // `apply_save_autostash_oid()` reports this on **stderr**, alongside
            // every other line the autostash machinery prints.
            eprintln!("Applied autostash.");
        }
    } else {
        // Keep the conflicted index (stages 1/2/3) so the user can resolve, exactly
        // as a conflicting `git stash apply` leaves it.
        let mut index = applied.index;
        index.write(Default::default())?;
        if !quiet {
            // git keeps the changes recoverable in the stash on a conflicting apply.
            eprintln!("Applying autostash resulted in conflicts.");
            eprintln!("Your changes are safe in the stash.");
            eprintln!("You can run \"git stash pop\" or \"git stash drop\" at any time.");
        }
    }
    Ok(applied.conflicts)
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

/// `base` with `paths` taken from `source` instead — the tree the index should
/// end up at when only some paths are being reset.
///
/// A path absent from `source` was newly added, so reverting it means dropping
/// it from the tree entirely.
fn revert_paths_in_tree(
    repo: &gix::Repository,
    base: ObjectId,
    source: ObjectId,
    paths: &HashSet<BString>,
) -> Result<ObjectId> {
    if paths.is_empty() {
        return Ok(base);
    }
    let source_map = tree_map(repo, source)?;
    let mut editor = repo.edit_tree(base)?;
    for path in paths {
        match source_map.get(path) {
            Some((id, mode)) => {
                editor.upsert(path.as_bstr(), entry_kind(*mode)?, *id)?;
            }
            None => {
                editor.remove(path.as_bstr())?;
            }
        }
    }
    Ok(editor.write()?.detach())
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

/// Which files beyond the tracked changes a push takes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Untracked {
    /// Tracked changes only — git's default.
    No,
    /// `-u`: untracked files as well, but not ignored ones.
    Include,
    /// `-a`: untracked *and* ignored files.
    All,
}

/// Everything `stash push` accepts, after parsing.
pub(crate) struct PushOpts {
    pub message: Option<String>,
    pub quiet: bool,
    /// `-k`: leave the index staged after the reset.
    pub keep_index: bool,
    /// `-S`: stash the staged changes only, leaving unstaged work alone.
    pub staged_only: bool,
    pub untracked: Untracked,
    /// Empty means "everything", matching git's unrestricted push.
    pub pathspecs: Vec<String>,
}

impl PushOpts {
    /// A plain push with just a message — what `save` and the bare form want.
    fn with_message(message: Option<String>) -> Self {
        PushOpts {
            message,
            quiet: false,
            keep_index: false,
            staged_only: false,
            untracked: Untracked::No,
            pathspecs: Vec::new(),
        }
    }
}

/// Parse `push` options.
///
/// Flag spellings follow `git stash push -h` on 2.55.0, including the `--no-`
/// negations git generates for every boolean. Note that `--only-untracked` is
/// *not* among them — git rejects it as an unknown option, so it is not
/// accepted here either.
fn parse_push_options(args: &[String]) -> Result<PushOpts> {
    let mut o = PushOpts::with_message(None);
    let mut from_file: Option<String> = None;
    let mut nul = false;
    let mut rest_are_paths = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if rest_are_paths {
            o.pathspecs.push(a.to_string());
            i += 1;
            continue;
        }
        match a {
            "-m" | "--message" => {
                i += 1;
                let m = args.get(i).ok_or_else(|| anyhow!("option '{a}' requires a value"))?;
                o.message = Some(m.clone());
            }
            "-q" | "--quiet" => o.quiet = true,
            "--no-quiet" => o.quiet = false,
            "-u" | "--include-untracked" => o.untracked = Untracked::Include,
            "--no-include-untracked" => o.untracked = Untracked::No,
            "-a" | "--all" => o.untracked = Untracked::All,
            "--no-all" => o.untracked = Untracked::No,
            "-k" | "--keep-index" => o.keep_index = true,
            "--no-keep-index" => o.keep_index = false,
            "-S" | "--staged" => o.staged_only = true,
            "--no-staged" => o.staged_only = false,
            // `stash -p` runs the hunk selector against a SCRATCH index
            // (`.git/stash-index`, seeded from HEAD and pointed at with
            // `GIT_INDEX_FILE`), then turns that index into the stash tree.
            // This port ignores `GIT_INDEX_FILE` everywhere, so the selector
            // would stage into the REAL index instead — silently corrupting the
            // user's staged state. Refused until that plumbing exists; the
            // selector itself is ready (`super::add_patch`).
            "-p" | "--patch" => bail!("--patch is not ported"),
            "--pathspec-file-nul" => nul = true,
            "--pathspec-from-file" => {
                i += 1;
                let f = args.get(i).ok_or_else(|| anyhow!("option '{a}' requires a value"))?;
                from_file = Some(f.clone());
            }
            "--" => rest_are_paths = true,
            other => {
                if let Some(f) = other.strip_prefix("--pathspec-from-file=") {
                    from_file = Some(f.to_string());
                } else if let Some(m) = other.strip_prefix("--message=") {
                    o.message = Some(m.to_string());
                } else if let Some(m) = other.strip_prefix("-m") {
                    o.message = Some(m.to_string());
                } else if other.starts_with('-') {
                    bail!("unsupported stash option '{other}'");
                } else {
                    o.pathspecs.push(other.to_string());
                }
            }
        }
        i += 1;
    }

    if let Some(f) = from_file {
        if !o.pathspecs.is_empty() {
            bail!("--pathspec-from-file is incompatible with pathspec arguments");
        }
        o.pathspecs = super::commit::read_pathspec_file(&f, nul)?;
    } else if nul {
        bail!("--pathspec-file-nul requires --pathspec-from-file");
    }

    // git refuses the combination outright rather than picking a winner.
    if o.staged_only && o.untracked != Untracked::No {
        bail!("Can't use --staged and --include-untracked or --all at the same time");
    }
    Ok(o)
}

/// `save` takes its message as positional words (plus the same rejected flags).
fn parse_save_message(args: &[String]) -> Result<Option<String>> {
    let mut words = Vec::new();
    for a in args {
        match a.as_str() {
            "-q" | "--quiet" => {}
            "-u" | "--include-untracked" => bail!("--include-untracked is not ported"),
            "-a" | "--all" => bail!("--all is not ported"),
            // `stash -p` runs the hunk selector against a SCRATCH index
            // (`.git/stash-index`, seeded from HEAD and pointed at with
            // `GIT_INDEX_FILE`), then turns that index into the stash tree.
            // This port ignores `GIT_INDEX_FILE` everywhere, so the selector
            // would stage into the REAL index instead — silently corrupting the
            // user's staged state. Refused until the scratch-index plumbing
            // exists; the selector itself is ready (`super::add_patch`).
            "-p" | "--patch" => bail!("--patch is not ported"),
            "-k" | "--keep-index" | "--no-keep-index" => bail!("--keep-index is not ported"),
            other if other.starts_with('-') => bail!("unsupported stash option '{other}'"),
            other => words.push(other.to_string()),
        }
    }
    Ok(if words.is_empty() { None } else { Some(words.join(" ")) })
}

/// Parse `store` options: `-m/--message <msg>`, `-q/--quiet`, and exactly one
/// positional `<commit>`. Port of `store_stash`'s option table, which requires
/// precisely one non-option argument.
fn parse_store_options(args: &[String]) -> Result<(Option<String>, bool, String)> {
    let mut message = None;
    let mut quiet = false;
    let mut positionals: Vec<String> = Vec::new();
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
            other => {
                if let Some(m) = other.strip_prefix("--message=") {
                    message = Some(m.to_string());
                } else {
                    positionals.push(other.to_string());
                }
            }
        }
        i += 1;
    }
    if positionals.len() != 1 {
        bail!("\"git stash store\" requires one <commit> argument");
    }
    Ok((message, quiet, positionals.into_iter().next().expect("exactly one")))
}

/// The resolved `git stash apply` / `git stash pop` command line.
struct ApplyOptions {
    /// Which reflog entry to restore; `stash@{0}` when no `<stash>` was given.
    index_in_stash: usize,
    /// `--index`: rebuild the index from the stash's staged (`I`) tree.
    restore_index: bool,
    /// `-q`/`--quiet`: skip the trailing `git status` and `pop`'s `Dropped …`.
    quiet: bool,
}

/// Parse the `apply`/`pop` option table (`-q`/`--quiet`, `--index`/`--no-index`,
/// at most one `<stash>`).
///
/// `stash.index` seeds `restore_index` before the loop, exactly as `git_config`
/// runs before `parse_options`, so an explicit `--no-index` on the command line
/// countermands the config and an explicit `--index` is redundant with it.
fn parse_apply_options(repo: &gix::Repository, args: &[String]) -> Result<ApplyOptions> {
    let mut restore_index = repo.config_snapshot().boolean("stash.index").unwrap_or(false);
    let mut quiet = false;
    let mut spec: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "--index" => restore_index = true,
            "--no-index" => restore_index = false,
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            // `stash -p` runs the hunk selector against a SCRATCH index
            // (`.git/stash-index`, seeded from HEAD and pointed at with
            // `GIT_INDEX_FILE`), then turns that index into the stash tree.
            // This port ignores `GIT_INDEX_FILE` everywhere, so the selector
            // would stage into the REAL index instead — silently corrupting the
            // user's staged state. Refused until the scratch-index plumbing
            // exists; the selector itself is ready (`super::add_patch`).
            "-p" | "--patch" => bail!("--patch is not ported"),
            other if other.starts_with('-') && other != "-" => {
                bail!("unsupported stash option '{other}'")
            }
            other => {
                if spec.is_some() {
                    bail!("Too many revisions specified");
                }
                spec = Some(other);
            }
        }
    }
    Ok(ApplyOptions {
        index_in_stash: parse_stash_index(spec)?,
        restore_index,
        quiet,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn store_options_parse_message_quiet_and_commit() {
        // `-m <msg>` form.
        let (msg, quiet, commit) = parse_store_options(&v(&["-m", "hello", "deadbeef"])).unwrap();
        assert_eq!(msg.as_deref(), Some("hello"));
        assert!(!quiet);
        assert_eq!(commit, "deadbeef");

        // `--message=` form plus `-q`.
        let (msg, quiet, commit) = parse_store_options(&v(&["--message=hi", "-q", "cafe"])).unwrap();
        assert_eq!(msg.as_deref(), Some("hi"));
        assert!(quiet);
        assert_eq!(commit, "cafe");

        // Bare commit, no options.
        let (msg, quiet, commit) = parse_store_options(&v(&["abc123"])).unwrap();
        assert_eq!(msg, None);
        assert!(!quiet);
        assert_eq!(commit, "abc123");
    }

    #[test]
    fn store_options_require_exactly_one_commit() {
        // git: `"git stash store" requires one <commit> argument` on 0 or >1.
        let none = parse_store_options(&v(&[])).unwrap_err().to_string();
        assert_eq!(none, "\"git stash store\" requires one <commit> argument");
        let many = parse_store_options(&v(&["a", "b"])).unwrap_err().to_string();
        assert_eq!(many, "\"git stash store\" requires one <commit> argument");
    }
}
