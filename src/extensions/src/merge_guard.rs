//! The two dirty-worktree gates `git merge` runs — ported from the two places
//! git enforces them, which are *not* the same check and do not fire together.
//!
//! 1. [`index_changes_from_head`] is merge-ort's `merge_start()` sanity check
//!    (`merge-ort.c`), which calls `repo_index_has_changes()` and refuses the
//!    whole merge when the index differs from `HEAD` *anywhere*. It is a
//!    strategy-level gate: only `ort`, `ours` and `octopus` run it, so a plain
//!    fast-forward accepts a staged change (verified against git 2.50.1).
//! 2. [`verify_two_way`] is `unpack_trees.c`'s `twoway_merge()` together with
//!    `verify_uptodate()` / `verify_absent()`, the checkout that moves the
//!    worktree from one tree to another. It is *per path*: a path both trees
//!    agree on is `keep_entry()`d without ever being looked at, which is why a
//!    locally modified file outside the merge's footprint is not a reason to
//!    refuse anything.
//!
//! Collapsing the two into one "is the worktree dirty at all" test — which is
//! what this port did before — refuses merges git performs. The most visible
//! case is `git pull` into a tree with unrelated local edits: git fast-forwards
//! and leaves the edits alone.
//!
//! The buckets in [`Clobber`] are git's `unpack_trees_error_types`, kept apart
//! because `display_error_msgs()` emits one block per type, in enum order.

use anyhow::Result;
use std::collections::{BTreeSet, HashMap, HashSet};

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::Mode;

/// A tree flattened to `path -> (id, mode)`. git's `same()` compares exactly
/// this pair, so the map doubles as the comparison unit.
type Flat = HashMap<BString, (ObjectId, Mode)>;

/// The paths that stop the worktree from moving onto a new tree, one bucket per
/// `unpack_trees_error_types` variant that a two-way merge can reach.
#[derive(Default)]
pub struct Clobber {
    /// `ERROR_WOULD_OVERWRITE` — `twoway_merge()`'s `reject_merge()`: the index
    /// entry matches neither the tree being left nor the one being entered, so
    /// the checkout would drop a staged change.
    pub would_overwrite: Vec<BString>,
    /// `ERROR_NOT_UPTODATE_FILE` — `verify_uptodate()`: the worktree file does
    /// not match the index entry the checkout is about to replace.
    pub not_uptodate: Vec<BString>,
    /// `ERROR_WOULD_LOSE_UNTRACKED_OVERWRITTEN` — `verify_absent()`: an
    /// untracked file sits where the new tree wants to write.
    pub untracked_overwritten: Vec<BString>,
    /// `ERROR_WOULD_LOSE_UNTRACKED_REMOVED` — `verify_absent()` for a path the
    /// new tree drops that the index no longer carries either.
    pub untracked_removed: Vec<BString>,
}

impl Clobber {
    /// Whether the checkout may proceed.
    pub fn is_empty(&self) -> bool {
        self.would_overwrite.is_empty()
            && self.not_uptodate.is_empty()
            && self.untracked_overwritten.is_empty()
            && self.untracked_removed.is_empty()
    }

    /// `display_error_msgs()`: one `error:` block per non-empty bucket in enum
    /// order — each path on its own tab-indented line, then the bucket's advice
    /// line — followed by a single `Aborting`. `cmd` is the verb
    /// `setup_unpack_trees_porcelain()` interpolates (`merge`, `checkout`, …).
    ///
    /// `ERROR_WOULD_OVERWRITE` and `ERROR_NOT_UPTODATE_FILE` carry the same
    /// wording in git's porcelain table, so a merge blocked by both prints the
    /// block twice — reproduced here rather than folded.
    pub fn report(&self, cmd: &str) {
        let block = |paths: &[BString], headline: &str, advice: &str| {
            if paths.is_empty() {
                return;
            }
            eprintln!("error: {headline}");
            for path in paths {
                eprintln!("\t{}", quote_path(path));
            }
            eprintln!("{advice}");
        };
        // `setup_unpack_trees_porcelain()` names the command twice: once as it is
        // for the headline, and once as the *action* the advice tells you to
        // finish first — where `checkout` reads `switch branches`.
        let action = match cmd {
            "checkout" => "switch branches",
            other => other,
        };
        let overwritten =
            format!("Your local changes to the following files would be overwritten by {cmd}:");
        let commit_advice = format!("Please commit your changes or stash them before you {action}.");
        let move_advice = format!("Please move or remove them before you {action}.");
        block(&self.would_overwrite, &overwritten, &commit_advice);
        block(&self.not_uptodate, &overwritten, &commit_advice);
        block(
            &self.untracked_overwritten,
            &format!("The following untracked working tree files would be overwritten by {cmd}:"),
            &move_advice,
        );
        block(
            &self.untracked_removed,
            &format!("The following untracked working tree files would be removed by {cmd}:"),
            &move_advice,
        );
        if !self.is_empty() {
            eprintln!("Aborting");
        }
    }

    /// `add_rejected_path()` without `show_all_errors`: the plumbing message
    /// table (`unpack_plumbing_errors`), one `error:` line per path as it is
    /// rejected, and no `Aborting`. This is what a caller that reaches the
    /// checkout through `read-tree -u -m` prints — the octopus strategy's
    /// per-head merges, where `setup_unpack_trees_porcelain()` never ran.
    pub fn report_plumbing(&self) {
        let block = |paths: &[BString], msg: fn(&str) -> String| {
            for path in paths {
                eprintln!("error: {}", msg(&quote_path(path)));
            }
        };
        block(&self.would_overwrite, |p| {
            format!("Entry '{p}' would be overwritten by merge. Cannot merge.")
        });
        block(&self.not_uptodate, |p| {
            format!("Entry '{p}' not uptodate. Cannot merge.")
        });
        block(&self.untracked_overwritten, |p| {
            format!("Untracked working tree file '{p}' would be overwritten by merge.")
        });
        block(&self.untracked_removed, |p| {
            format!("Untracked working tree file '{p}' would be removed by merge.")
        });
    }
}

/// git's `repo_index_has_changes()`: the paths where the index differs from
/// `head_tree`, in path order (git's diff order), unquoted — the exact list
/// merge-ort's `merge_start()` interpolates into its refusal.
pub fn index_changes_from_head(
    repo: &gix::Repository,
    head_tree: ObjectId,
    index: &gix::index::State,
) -> Result<Vec<BString>> {
    use gix::diff::index::ChangeRef;
    use gix::status::tree_index::TrackRenames;

    let mut paths: BTreeSet<BString> = BTreeSet::new();
    repo.tree_index_status(
        &head_tree,
        index,
        None,
        TrackRenames::Disabled,
        |change, _tree_index, _worktree_index| -> Result<_, std::convert::Infallible> {
            match change {
                ChangeRef::Addition { location, .. }
                | ChangeRef::Deletion { location, .. }
                | ChangeRef::Modification { location, .. } => {
                    paths.insert(location.into_owned());
                }
                // Rename tracking is disabled above, so this never fires.
                ChangeRef::Rewrite { .. } => {}
            }
            Ok(gix::diff::index::Action::Continue(()))
        },
    )?;
    Ok(paths.into_iter().collect())
}

/// The refusal merge-ort's `merge_start()` prints before any merging happens:
/// one line, the paths space-joined behind two spaces, unquoted.
pub fn report_index_changes(paths: &[BString]) {
    let joined = paths
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!("error: Your local changes to the following files would be overwritten by merge:");
    eprintln!("  {joined}");
}

/// `unpack_trees()` with `fn = twoway_merge` over `old_tree -> new_tree`: which
/// paths refuse to be checked out over, given `index` and the worktree on disk.
///
/// Mirrors `twoway_merge()`'s case table. For every path the two trees disagree
/// on (the others are `keep_entry()`d untouched, and never checked):
///
/// * index already at the new entry — keep it, nothing to write.
/// * index at the old entry — `merged_entry()`/`deleted_entry()`, which run
///   `verify_uptodate()`: the worktree file must still match the index.
/// * path absent from the index but present in the old tree — the deletion was
///   staged, so the checkout would undo it: rejected.
/// * path absent from both index and old tree — an addition, so
///   `verify_absent()` requires nothing untracked in the way.
/// * anything else (index matching neither side) — rejected.
///
/// A worktree file that was *deleted* without staging is not dirt here: git's
/// `verify_uptodate_1()` returns 0 on `ENOENT` and simply rewrites the file.
pub fn verify_two_way(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
    index: &gix::index::State,
) -> Result<Clobber> {
    let old = flatten(repo, old_tree)?;
    let new = flatten(repo, new_tree)?;
    let mut clobber = Clobber::default();

    // The merge's footprint: every path the two trees disagree on, in path
    // order so the reported lists come out sorted like git's.
    let touched: BTreeSet<&BString> = old
        .keys()
        .chain(new.keys())
        .filter(|p| old.get(*p) != new.get(*p))
        .collect();
    if touched.is_empty() {
        return Ok(clobber);
    }

    let (cur, conflicted) = index_map(index);
    // Paths still needing a look at the disk, deferred so the worktree is
    // scanned once, and only when the answer can matter.
    let mut uptodate: Vec<&BString> = Vec::new();
    let mut absent_overwritten: Vec<&BString> = Vec::new();
    let mut absent_removed: Vec<&BString> = Vec::new();

    for path in touched {
        let (o, n) = (old.get(path), new.get(path));
        match cur.get(path) {
            // An unmerged path cannot match any tree entry; git dies on a
            // conflicted index long before this, so treat it as a rejection.
            Some(_) if conflicted.contains(path) => clobber.would_overwrite.push(path.clone()),
            // Already at the target — `keep_entry()`, whatever the worktree holds.
            Some(c) if n == Some(c) => {}
            Some(c) if o == Some(c) => uptodate.push(path),
            Some(_) => clobber.would_overwrite.push(path.clone()),
            // Staged deletion of a path the merge changes: `twoway_merge()`
            // rejects rather than resurrect it.
            None if o.is_some() && n.is_some() => clobber.would_overwrite.push(path.clone()),
            None if n.is_some() => absent_overwritten.push(path),
            None => absent_removed.push(path),
        }
    }

    if uptodate.is_empty() && absent_overwritten.is_empty() && absent_removed.is_empty() {
        return Ok(clobber);
    }
    let want_untracked = !absent_overwritten.is_empty() || !absent_removed.is_empty();
    let worktree = scan(repo, want_untracked)?;
    for path in uptodate {
        if worktree.modified.contains(path) {
            clobber.not_uptodate.push(path.clone());
        }
    }
    for path in absent_overwritten {
        if worktree.untracked.contains(path) && is_plain_file(repo, path) {
            clobber.untracked_overwritten.push(path.clone());
        }
    }
    for path in absent_removed {
        if worktree.untracked.contains(path) && is_plain_file(repo, path) {
            clobber.untracked_removed.push(path.clone());
        }
    }
    Ok(clobber)
}

/// Whether the path in the way is a plain file rather than a directory.
///
/// `check_ok_to_remove()` rejects a file outright, but hands a directory to
/// `verify_clean_subdirectory()`, which lets through a gitlink whose submodule
/// already sits at the target commit and otherwise counts the untracked files
/// inside. That subdirectory walk is not ported: a directory in the way is left
/// to the checkout, which keeps meta-repo merges that add a submodule already
/// present on disk working, as they do under git.
fn is_plain_file(repo: &gix::Repository, path: &BString) -> bool {
    repo.workdir_path(path.as_bstr())
        .and_then(|full| full.symlink_metadata().ok())
        .is_some_and(|meta| !meta.is_dir())
}

/// Flatten a tree into `path -> (id, mode)` through its index representation,
/// which already expands nested trees into slash-separated paths.
fn flatten(repo: &gix::Repository, tree: ObjectId) -> Result<Flat> {
    let index = repo.index_from_tree(&tree)?;
    let backing = index.path_backing();
    Ok(index
        .entries()
        .iter()
        .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode)))
        .collect())
}

/// The index in the same shape as a flattened tree, plus the paths carrying
/// conflict stages (which no tree entry can match).
fn index_map(index: &gix::index::State) -> (Flat, HashSet<BString>) {
    let backing = index.path_backing();
    let mut flat = Flat::with_capacity(index.entries().len());
    let mut conflicted = HashSet::new();
    for e in index.entries() {
        let path = e.path_in(backing).to_owned();
        if e.stage_raw() != 0 {
            conflicted.insert(path.clone());
            continue;
        }
        flat.insert(path, (e.id, e.mode));
    }
    (flat, conflicted)
}

/// The two facts the gate needs from the worktree, from a single status pass.
struct Worktree {
    /// Tracked paths whose worktree content differs from the index — git's
    /// `ie_match_stat()` mismatch. A path deleted from the worktree is left out:
    /// `verify_uptodate_1()` accepts `ENOENT` and rewrites the file.
    modified: HashSet<BString>,
    /// Untracked (and not ignored) worktree files. Empty unless the caller asked
    /// for them, since collecting them means walking the directory tree.
    untracked: HashSet<BString>,
}

fn scan(repo: &gix::Repository, want_untracked: bool) -> Result<Worktree> {
    use gix::status::index_worktree::Item as IndexWorktreeItem;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    let mut state = Worktree {
        modified: HashSet::new(),
        untracked: HashSet::new(),
    };
    let untracked_files = if want_untracked {
        // One entry per file: the default collapses an untracked directory into
        // a single record, which would hide `dir/file` from the lookup below.
        gix::status::UntrackedFiles::Files
    } else {
        gix::status::UntrackedFiles::None
    };
    let patterns: Vec<BString> = Vec::new();
    let iter = repo
        .status(gix::progress::Discard)?
        .untracked_files(untracked_files)
        .into_iter(patterns)?;
    for item in iter {
        let gix::status::Item::IndexWorktree(iw) = item? else {
            continue;
        };
        match iw {
            IndexWorktreeItem::Modification {
                rela_path, status, ..
            } => match status {
                // A file gone from disk is rewritten, not a refusal.
                EntryStatus::Change(Change::Removed) => {}
                // Racily clean: the content still matches.
                EntryStatus::NeedsUpdate(_) => {}
                // A submodule is never in the way: `verify_uptodate_1()` hands a
                // gitlink to `check_submodule_move_head()` — a no-op unless the
                // command recurses into submodules — and returns 0 without ever
                // consulting `ie_match_stat`'s verdict. Dirty content inside the
                // submodule, or a submodule `HEAD` that has moved away from the
                // recorded gitlink, therefore does not stop a checkout or a
                // merge that changes that gitlink: the superproject's index is
                // updated and the submodule worktree is left exactly as it is.
                EntryStatus::Change(Change::SubmoduleModification(_)) => {}
                EntryStatus::Change(_) | EntryStatus::Conflict { .. } | EntryStatus::IntentToAdd => {
                    state.modified.insert(rela_path);
                }
            },
            IndexWorktreeItem::DirectoryContents { entry, .. } => {
                if matches!(entry.status, gix::dir::entry::Status::Untracked) {
                    state.untracked.insert(entry.rela_path);
                }
            }
            IndexWorktreeItem::Rewrite { .. } => {}
        }
    }
    Ok(state)
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(path: &BString) -> String {
    let bytes = path.as_slice();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        // All bytes are printable ASCII here, so this is lossless.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
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
