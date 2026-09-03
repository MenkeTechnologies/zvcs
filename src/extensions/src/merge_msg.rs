//! git's merge informational messages — the single renderer behind every merge
//! verb.
//!
//! merge-ort collects its `Auto-merging` / `CONFLICT (…)` / `warning:` lines
//! through one function, `path_msg()` (merge-ort.c:792), and flushes them
//! through one other, `merge_display_update_messages()` (merge-ort.c:4815).
//! Every caller — `git merge`, `cherry-pick`, `revert`, `rebase`, `stash
//! apply`, `merge-tree`, `merge-recursive` and `merge-subtree` — therefore sees
//! byte-identical text for the same conflict. This module is that one function
//! pair; before it existed the port had **four** independent renderers
//! (`merge_apply`, `merge_tree`, `merge_recursive`, `merge_subtree`, the last
//! two byte-identical clones of each other) which had already drifted into
//! opposite bugs: the porcelain one named `add/add` from the missing-base rule
//! but knew none of the tree-conflict classes, while `merge-tree`'s knew the
//! classes but named `add/add` from the change kinds and got rename/add wrong.
//!
//! # Strictness
//!
//! The one thing the callers genuinely differ on is what to do with a conflict
//! class whose git text is not reconstructible here:
//!
//! * [`Strictness::Refuse`] — `merge-tree`'s contract. Its stdout is a
//!   machine-readable record consumed by scripts, and it writes nothing to the
//!   worktree, so refusing costs nothing and an invented `CONFLICT (…)` line
//!   would be a lie in a data format.
//! * [`Strictness::Approximate`] — `git merge`'s contract. By the time the
//!   messages are rendered the merge has already happened: merge-ort's
//!   `merge_switch_to_result()` checks the result out *before* calling
//!   `merge_display_update_messages()` (merge-ort.c:4964). Dying at that point
//!   would abandon a half-applied merge, which is strictly worse than printing
//!   an approximate line, so unrenderable classes fall back to the plain
//!   `CONFLICT (content|add/add): Merge conflict in <path>` shape.
//!
//! # Ordering
//!
//! `path_msg()` keys every message on a *primary path* (its strmap key) and
//! `merge_display_update_messages()` sorts those keys with `string_list_sort()`
//! before printing, preserving insertion order among messages sharing a key
//! (merge-ort.c:4837-4847). [`render`] reproduces that with a stable sort on
//! [`Message::paths`]`[0]`.

use anyhow::{bail, Result};

use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::tree_with_rewrites::Change;
use gix::hash::ObjectId;
use gix::merge::tree::{Conflict, Resolution, ResolutionFailure, TreatAsUnresolved};

/// One informational message, in both the human and the `-z` shape.
///
/// `paths` are git's `logical_conflict_info.paths` for this message: the first
/// entry is the *primary* path (git's strmap key, used to sort the messages),
/// and any further entries follow in git's `path_msg()` argument order (e.g.
/// the source then destination of a rename). `ctype` is git's stable short
/// conflict type from `type_short_descriptions[]` (merge-ort.c:596-635), which
/// `merge-tree -z` prints as its own field and which is **not** always the
/// prefix of `text` — `CONFLICT_DISTINCT_MODES` is spelled `CONFLICT (distinct
/// modes)` there while its message reads `CONFLICT (distinct types)`. `text` is
/// the free-form line, carrying its own trailing newline exactly as git emits
/// it via `puts()`.
pub struct Message {
    pub paths: Vec<BString>,
    pub ctype: &'static str,
    pub text: String,
}

/// What to do with a conflict class whose git text is not ported.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Strictness {
    /// Fail before a byte is written — `merge-tree`.
    Refuse,
    /// Fall back to the plain content-conflict line — `git merge` and the other
    /// verbs that have already moved the worktree by the time this runs.
    Approximate,
}

/// How to reach **operand 1**'s tree, for the message classes whose text names a
/// side.
///
/// git derives `opt->branch1`/`branch2` positionally from its command line and
/// keeps the trees it resolved from them; `gix-merge` normalizes *ours* and
/// *theirs* independently of operand order (see `Conflict::changes_in_resolution`),
/// so the side a path belongs to has to be recovered from tree membership. One
/// tree is enough: a path that operand 1 does not carry is operand 2's.
pub enum Operand1<'a> {
    /// Peel this operand spec on demand — `merge-tree`, whose operands are
    /// revision specs it must not re-resolve unless a message actually needs it.
    Spec(&'a str),
    /// A tree the caller already resolved, *after* any `-Xsubtree` shift, so
    /// membership is tested against the tree that was really merged.
    Tree(ObjectId),
}

/// Render `conflicts` into git's message list, sorted the way
/// `merge_display_update_messages()` prints them.
///
/// `label1`/`label2` are `opt->branch1`/`opt->branch2`: the two operands exactly
/// as spelled on the command line. `unresolved` is the caller's
/// [`TreatAsUnresolved`] policy, used for the same decision merge-ort makes with
/// `clean_merge`: whether a content merge earns its `CONFLICT (…)` line.
pub fn render<'r, 's>(
    repo: &'r gix::Repository,
    conflicts: &[Conflict],
    label1: &'s str,
    label2: &'s str,
    operand1: Operand1<'s>,
    unresolved: TreatAsUnresolved,
    strictness: Strictness,
) -> Result<Vec<Message>> {
    let mut operands = Operands::new(label1, label2, operand1);
    let mut out: Vec<Message> = Vec::new();
    for conflict in conflicts {
        match render_one(repo, conflict, unresolved, &mut operands)? {
            Some(msgs) => out.extend(msgs),
            None => match strictness {
                Strictness::Refuse => bail!(
                    "conflict at {} is a class whose git message text is not ported (retry with --no-messages or --quiet)",
                    conflict.changes_in_resolution().0.location()
                ),
                Strictness::Approximate => out.extend(approximate(conflict, unresolved)),
            },
        }
    }
    out.sort_by(|a, b| a.paths[0].cmp(&b.paths[0]));
    Ok(out)
}

/// The messages for one conflict, or `None` when the class is not ported.
fn render_one<'r, 's>(
    repo: &'r gix::Repository,
    conflict: &Conflict,
    unresolved: TreatAsUnresolved,
    operands: &mut Operands<'r, 's>,
) -> Result<Option<Vec<Message>>> {
    let (ours, theirs) = conflict.changes_in_resolution();
    let mut out = Vec::new();
    match &conflict.resolution {
        // The content-merge family. Both variants end in merge-ort's
        // `handle_content_merge()` (merge-ort.c:2160), which is also where the
        // binary warning and the `Auto-merging` line come from.
        Ok(Resolution::OursModifiedTheirsModifiedThenBlobContentMerge { .. })
        | Ok(Resolution::OursModifiedTheirsRenamedAndChangedThenRename {
            merged_blob: Some(_),
            ..
        }) => {
            let path = conflict_location(conflict);
            let Some(stages) = Stages::of(conflict) else {
                return Ok(None);
            };
            let conflicted = conflict.is_unresolved(unresolved);

            if stages.our_mode.is_link() && stages.their_mode.is_link() {
                // `handle_content_merge()`'s `S_ISLNK` arm (merge-ort.c:2291)
                // resolves symlinks without `ll_merge()`, so neither the binary
                // warning nor `Auto-merging` is emitted — only the notice.
            } else if stages.our_mode.is_commit() && stages.their_mode.is_commit() {
                // The `S_ISGITLINK` arm (merge-ort.c:2280) hands off to
                // `merge_submodule()`, which prints its own diagnostics — those
                // are not ported, so a *conflicting* gitlink merge is refused
                // rather than reported with half its output.
                if conflicted {
                    return Ok(None);
                }
            } else if stages.our_mode.is_blob() && stages.their_mode.is_blob() {
                // `merge_3way()` emits the binary warning from inside
                // `ll_merge()`'s return check (merge-ort.c:2154-2158), i.e.
                // *before* the `Auto-merging` line its caller adds afterwards.
                //
                // Only the shape where all three of git's `pathnames[]` are
                // equal is rendered: that is the one whose labels are
                // `opt->branch1`/`branch2` verbatim (merge-ort.c:2137-2140). The
                // renamed shapes use `<ancestor|branch>:<path>` triples
                // (merge-ort.c:2142-2144) built from an ancestor label this
                // module is not given, so they emit nothing rather than a line
                // with the wrong operands in it.
                if conflicted
                    && ours.location() == theirs.location()
                    && !matches!(ours, Change::Rewrite { .. })
                    && !matches!(theirs, Change::Rewrite { .. })
                    && stages.any_is_binary(repo)?
                {
                    out.push(Message {
                        paths: vec![path.clone()],
                        ctype: "CONFLICT (binary)",
                        text: format!(
                            "warning: Cannot merge binary files: {path} ({label1} vs. {label2})\n",
                            label1 = operands.label1,
                            label2 = operands.label2
                        ),
                    });
                }
                if stages.needs_content_merge() {
                    out.push(Message {
                        paths: vec![path.clone()],
                        ctype: "Auto-merging",
                        text: format!("Auto-merging {path}\n"),
                    });
                }
            } else {
                // Mixed types cannot reach `handle_content_merge()` at all — it
                // asserts both sides share an `S_IFMT` (merge-ort.c:2199).
                return Ok(None);
            }

            if conflicted {
                out.push(Message {
                    paths: vec![path.clone()],
                    ctype: "CONFLICT (contents)",
                    text: format!(
                        "CONFLICT ({reason}): Merge conflict in {path}\n",
                        reason = stages.content_reason()
                    ),
                });
            }
        }

        // The same class with no content merge at all: one side renamed the
        // blob, the other's modification turned out to be the only change, so
        // merge-ort's trivial-oid shortcut (merge-ort.c:2233-2236) picked a side
        // outright and `process_entry()` found nothing to report. Stock is
        // silent and exits 0 — measured on a fixture where `side` renames
        // `old.txt` to `new.txt` and `main` appends to it — so this is a
        // *rendered* class whose rendering is the empty list, not an
        // unrenderable one.
        Ok(Resolution::OursModifiedTheirsRenamedAndChangedThenRename { merged_blob: None, .. }) => {}

        // Modify/delete (merge-ort.c:4404-4410). `changes_in_resolution()`
        // orients `ours` to the modified side and `theirs` to the deleted side;
        // git names the two by which operand still carries the file, which is
        // exactly the tree that retains `path` as a non-tree entry.
        Err(ResolutionFailure::OursModifiedTheirsDeleted) => {
            let path = ours.location().to_owned();
            let (modify_branch, delete_branch) = operands.split_at(repo, path.as_bstr())?;
            out.push(modify_delete(&path, delete_branch, modify_branch));
        }

        // Rename/delete (merge-ort.c:3206-3211): `theirs` is the rename (a
        // rewrite carrying source and destination), `ours` the deletion. The
        // renaming operand is the one whose tree holds the new name.
        Err(ResolutionFailure::OursDeletedTheirsRenamed) => {
            let src = theirs.source_location().to_owned();
            let dst = theirs.location().to_owned();
            let (rename_branch, delete_branch) = operands.split_at(repo, dst.as_bstr())?;
            out.push(Message {
                // git's primary path is the new name, followed by the old one.
                paths: vec![dst.clone(), src.clone()],
                ctype: "CONFLICT (rename/delete)",
                text: format!(
                    "CONFLICT (rename/delete): {src} renamed to {dst} in {rename_branch}, but deleted in {delete_branch}.\n"
                ),
            });
            // A rename that also *changed* the blob leaves a modify/delete on top
            // of the rename/delete: `process_entry()` reaches its `filemask == 3
            // || 5` arm with `path_conflict` set, and only skips the notice when
            // the content is byte-identical to the base (merge-ort.c:4396-4402).
            if rewrite_changed_content(theirs) {
                out.push(modify_delete(&dst, delete_branch, rename_branch));
            }
        }

        // Rename/rename(1to2) (merge-ort.c:3060-3066): both operands renamed the
        // same source to distinct destinations. git prints them positionally as
        // `to <d1> in <branch1> and to <d2> in <branch2>`, so `d1` is whichever
        // destination lives in operand 1's tree.
        Err(ResolutionFailure::OursRenamedTheirsRenamedDifferently { merged_blob }) => {
            let src = ours.source_location().to_owned();
            let our_dst = ours.location().to_owned();
            let their_dst = theirs.location().to_owned();
            let (dst1, dst2) = if operands.holds(repo, our_dst.as_bstr())? {
                (our_dst, their_dst)
            } else {
                (their_dst, our_dst)
            };
            // git content-merges the two destinations against the shared base
            // *before* reporting the rename (merge-ort.c:3011), naming the merge
            // after the **source** path. The stage entries recorded for this
            // class already hold that merged blob on both sides, so the
            // trivial-oid test has to read the two changes instead.
            if merged_blob.is_some()
                && Stages::of_changes(ours, theirs).is_some_and(|s| s.needs_content_merge())
            {
                out.push(Message {
                    paths: vec![src.clone()],
                    ctype: "Auto-merging",
                    text: format!("Auto-merging {src}\n"),
                });
            }
            out.push(Message {
                // git's paths are the shared source, then both destinations.
                paths: vec![src.clone(), dst1.clone(), dst2.clone()],
                ctype: "CONFLICT (rename/rename)",
                text: format!(
                    "CONFLICT (rename/rename): {src} renamed to {dst1} in {label1} and to {dst2} in {label2}.\n",
                    label1 = operands.label1,
                    label2 = operands.label2
                ),
            });
        }

        // One side put a *directory* where the other has a file, and git moved
        // the file aside (merge-ort.c:4170-4174). `gix-merge` files this under
        // the same failure as a plain type clash, so the directory side is what
        // tells the two apart.
        Err(ResolutionFailure::OursAddedTheirsAddedTypeMismatch { their_unique_location })
            if change_mode(ours).is_tree() || change_mode(theirs).is_tree() =>
        {
            let old_path = ours.location().to_owned();
            let new_path = their_unique_location.clone();
            // `df_file_index` picks the side that is *not* the directory, and
            // names that operand (merge-ort.c:4165-4166).
            let (file_branch, _) = operands.split_at(repo, old_path.as_bstr())?;
            out.push(Message {
                paths: vec![new_path.clone(), old_path.clone()],
                ctype: "CONFLICT (file/directory)",
                text: format!(
                    "CONFLICT (file/directory): directory in the way of {old_path} from {file_branch}; moving it to {new_path} instead.\n"
                ),
            });
        }

        // Two different non-directory types on the same path — file vs symlink,
        // symlink vs submodule, … — which git resolves by renaming whichever
        // sides are not regular files (merge-ort.c:4238-4269). `Unknown` is
        // `gix-merge`'s catch-all and lands here too whenever it carries the
        // same shape, which is how a symlink/submodule clash is reported.
        Err(
            failure @ (ResolutionFailure::OursAddedTheirsAddedTypeMismatch { .. } | ResolutionFailure::Unknown),
        ) if is_type_clash(ours, theirs) => {
            let path = ours.location().to_owned();
            // `rename_a`/`rename_b` are set from `S_ISREG` alone: the regular
            // file is the side that moves, so exactly "one" of them is renamed
            // whenever either side is a regular file, and "both" otherwise.
            let which = if change_mode(ours).is_blob() || change_mode(theirs).is_blob() {
                "one"
            } else {
                "both"
            };
            // git names the shared path first and the path the moved side was
            // given second (`path, rename_a ? a_path : b_path`,
            // merge-ort.c:4163-4166) — a second field `merge-tree -z` prints.
            let mut paths = vec![path.clone()];
            if let ResolutionFailure::OursAddedTheirsAddedTypeMismatch { their_unique_location } = failure {
                paths.push(their_unique_location.clone());
            }
            out.push(Message {
                paths,
                ctype: "CONFLICT (distinct modes)",
                text: format!(
                    "CONFLICT (distinct types): {path} had different types on each side; renamed {which} of them so each can be recorded somewhere.\n"
                ),
            });
        }

        // The other half of a directory/file clash: the blob one side kept
        // modifying was moved aside, and what remains at the original name is a
        // modify/delete against the moved path (merge-ort.c:4404-4410 again,
        // reached for the relocated entry).
        Err(ResolutionFailure::OursModifiedTheirsDirectoryThenOursRenamed {
            renamed_unique_path_to_modified_blob,
        }) => {
            let original = ours.location().to_owned();
            let path = renamed_unique_path_to_modified_blob.clone();
            // The operand that still has a *blob* at the original name is the one
            // that modified it; the other turned it into a directory.
            let (modify_branch, delete_branch) = operands.split_at(repo, original.as_bstr())?;
            // The move itself is announced first (merge-ort.c:4170-4174) — the
            // same `CONFLICT (file/directory)` line the add/add type clash emits,
            // reached here through the other door: a path that was a blob in the
            // base, edited on one side and replaced by a directory on the other.
            // `gix-merge` performs the rename and names the destination, so only
            // the notice was missing; without it stock's two-line report came out
            // as one line and read as a bare modify/delete on a path the user
            // never named.
            out.push(Message {
                paths: vec![path.clone(), original.clone()],
                ctype: "CONFLICT (file/directory)",
                text: format!(
                    "CONFLICT (file/directory): directory in the way of {original} from {modify_branch}; moving it to {path} instead.\n"
                ),
            });
            out.push(modify_delete(&path, delete_branch, modify_branch));
        }

        // One side renamed a directory and the other put a change inside the old
        // name, so merge-ort moved the change along (`apply_directory_rename_and_ort`,
        // merge-ort.c:2797-2839). Under `merge.directoryRenames=conflict` — git's
        // default — the move is only *suggested* and the path stays unmerged; under
        // `true` the same sentence is printed as a `Path updated:` hint and the merge
        // stays clean. Both come from the same `path_msg()` pair, keyed on the new
        // path with the old one following it.
        Err(ResolutionFailure::DirectoryRenameSuggested { final_location }) => {
            out.push(directory_rename_message(
                repo, operands, ours, theirs, final_location, true,
            )?);
        }
        Ok(Resolution::SourceLocationAffectedByRename { final_location }) => {
            out.push(directory_rename_message(
                repo, operands, ours, theirs, final_location, false,
            )?);
        }

        _ => return Ok(None),
    }
    Ok(Some(out))
}

/// merge-ort's directory-rename notice (merge-ort.c:2797-2839), in both the
/// `CONFLICT (file location)` and the `Path updated:` spelling.
///
/// The two changes arrive without a fixed order, so the directory rename is
/// picked out by being the tree-mode [`Change::Rewrite`] — the only shape
/// `rewrite_location_with_renamed_directory()` follows — and the other change is
/// the one that was carried along.
fn directory_rename_message<'r, 's>(
    repo: &'r gix::Repository,
    operands: &mut Operands<'r, 's>,
    ours: &Change,
    theirs: &Change,
    final_location: &BString,
    suggested: bool,
) -> Result<Message> {
    let moved = if change_mode(ours).is_tree() { theirs } else { ours };
    let new_path = final_location.clone();
    let old_path = moved.location().to_owned();
    // `branch_with_new_path` is the side that carries the change, and it is the
    // only side whose tree still holds a file at the change's own path.
    let (branch_with_new_path, branch_with_dir_rename) = operands.split_at(repo, old_path.as_bstr())?;
    // `pair->status == 'A'`: an addition names only where it landed, while a
    // rename names where it came from as well.
    let text = match (moved, suggested) {
        (Change::Addition { .. }, true) => format!(
            "CONFLICT (file location): {old_path} added in {branch_with_new_path} inside a directory that was renamed in {branch_with_dir_rename}, suggesting it should perhaps be moved to {new_path}.\n"
        ),
        (Change::Addition { .. }, false) => format!(
            "Path updated: {old_path} added in {branch_with_new_path} inside a directory that was renamed in {branch_with_dir_rename}; moving it to {new_path}.\n"
        ),
        (_, true) => {
            let source = moved.source_location();
            format!(
                "CONFLICT (file location): {source} renamed to {old_path} in {branch_with_new_path}, inside a directory that was renamed in {branch_with_dir_rename}, suggesting it should perhaps be moved to {new_path}.\n"
            )
        }
        (_, false) => {
            let source = moved.source_location();
            format!(
                "Path updated: {source} renamed to {old_path} in {branch_with_new_path}, inside a directory that was renamed in {branch_with_dir_rename}; moving it to {new_path}.\n"
            )
        }
    };
    Ok(Message {
        paths: vec![new_path, old_path],
        ctype: if suggested {
            "CONFLICT (directory rename suggested)"
        } else {
            "Path updated due to directory rename"
        },
        text,
    })
}

/// merge-ort's modify/delete notice (merge-ort.c:4406-4410), shared by the
/// plain modify/delete arm and the two conflict classes that end in one.
fn modify_delete(path: &BString, delete_branch: &str, modify_branch: &str) -> Message {
    Message {
        paths: vec![path.clone()],
        ctype: "CONFLICT (modify/delete)",
        text: format!(
            "CONFLICT (modify/delete): {path} deleted in {delete_branch} and modified in {modify_branch}.  Version {modify_branch} of {path} left in tree.\n"
        ),
    }
}

/// The fallback for [`Strictness::Approximate`]: merge-ort's plain content
/// notice, named by the destination path and classified by the missing base
/// stage. It is what every unrenderable class degrades to, and is deliberately
/// the *whole* of what the porcelain used to print for every conflict.
fn approximate(conflict: &Conflict, unresolved: TreatAsUnresolved) -> Vec<Message> {
    let path = conflict_location(conflict);
    let mut out = Vec::new();
    if conflict.content_merge().is_some() {
        out.push(Message {
            paths: vec![path.clone()],
            ctype: "Auto-merging",
            text: format!("Auto-merging {path}\n"),
        });
    }
    if conflict.is_unresolved(unresolved) {
        let reason = if conflict.entries()[0].is_none() {
            "add/add"
        } else {
            "content"
        };
        out.push(Message {
            paths: vec![path.clone()],
            ctype: "CONFLICT (contents)",
            text: format!("CONFLICT ({reason}): Merge conflict in {path}\n"),
        });
    }
    out
}

/// The path merge-ort names a conflict by: where the merged content actually
/// ended up, not where it started.
///
/// merge-ort's `path_msg()` calls are keyed on the *destination*. When one side
/// renames a path and the other modifies it, `handle_content_merge()` runs
/// against the rename's new name and reports it — stock 2.55.0 on a fixture
/// where `HEAD` renames `old.txt` to `new.txt` and `side` edits `old.txt` prints
/// `Auto-merging new.txt` / `CONFLICT (content): Merge conflict in new.txt`, and
/// leaves the three stages under `new.txt`.
///
/// `gix-merge` records the same thing but spread across three places, and
/// `changes_in_resolution().0` is not it: for that fixture the resolution is
/// `OursModifiedTheirsRenamedAndChangedThenRename`, whose `ours` is the plain
/// modification at the *old* name and whose `theirs` is the
/// [`Change::Rewrite`](gix::diff::tree_with_rewrites::Change::Rewrite) carrying
/// the new one. Reading `.0.location()` therefore named the pre-rename path on
/// every rename conflict. The destination is, in order of authority:
///
///   1. `final_location`, when the resolution carries one — the directory-rename
///      case, where the blob lands somewhere neither side spelled;
///   2. the `Rewrite` side's `location`, which is documented as "the location
///      after the rename or copy operation";
///   3. `ours.location()`, when no rename is involved at all.
pub fn conflict_location(conflict: &Conflict) -> BString {
    if let Ok(
        Resolution::SourceLocationAffectedByRename { final_location }
        | Resolution::OursModifiedTheirsRenamedAndChangedThenRename {
            final_location: Some(final_location),
            ..
        },
    ) = &conflict.resolution
    {
        return final_location.clone();
    }
    let (ours, theirs) = conflict.changes_in_resolution();
    for change in [ours, theirs] {
        if matches!(change, Change::Rewrite { .. }) {
            return change.location().to_owned();
        }
    }
    ours.location().to_owned()
}

/// merge-ort's `ci->stages[]` for one path: the base, *our* and *their*
/// versions, with the modes and ids the content merge is decided from.
struct Stages {
    base: Option<ObjectId>,
    our_id: ObjectId,
    our_mode: gix::object::tree::EntryMode,
    their_id: ObjectId,
    their_mode: gix::object::tree::EntryMode,
    /// `ci->filemask == 6`: no ancestor stage, i.e. both sides added the path.
    no_base: bool,
    /// Whether the merged result is a gitlink, which renames the conflict.
    merged_is_gitlink: bool,
}

impl Stages {
    /// Read the stage triple a conflict recorded for the index. `gix-merge`
    /// fills these with exactly what merge-ort would put in `ci->stages[]`, so
    /// they are the faithful input to merge-ort's decisions; a conflict that
    /// recorded no side entries has no content merge to describe.
    fn of(conflict: &Conflict) -> Option<Self> {
        let entries = conflict.entries();
        let (ours, theirs) = (entries[1]?, entries[2]?);
        let (our_change, their_change) = conflict.changes_in_resolution();
        Some(Stages {
            base: entries[0].map(|e| e.id),
            our_id: ours.id,
            our_mode: ours.mode,
            their_id: theirs.id,
            their_mode: theirs.mode,
            no_base: entries[0].is_none(),
            merged_is_gitlink: change_mode(our_change).is_commit()
                && change_mode(their_change).is_commit(),
        })
    }

    /// The same triple read from the two changes instead of the recorded index
    /// entries, for the classes whose entries already hold a *merged* blob on
    /// both sides rather than the two originals.
    fn of_changes(ours: &Change, theirs: &Change) -> Option<Self> {
        let base = match (ours, theirs) {
            (Change::Rewrite { source_id, .. }, _) => Some(*source_id),
            (Change::Modification { previous_id, .. }, _) => Some(*previous_id),
            (_, Change::Rewrite { source_id, .. }) => Some(*source_id),
            (_, Change::Modification { previous_id, .. }) => Some(*previous_id),
            _ => None,
        };
        Some(Stages {
            base,
            our_id: change_id(ours),
            our_mode: change_mode(ours),
            their_id: change_id(theirs),
            their_mode: change_mode(theirs),
            no_base: base.is_none(),
            merged_is_gitlink: change_mode(ours).is_commit() && change_mode(theirs).is_commit(),
        })
    }

    /// merge-ort's trivial-oid shortcut (merge-ort.c:2233-2236): when *ours*
    /// equals *theirs*, or either side equals the base, the result is picked
    /// outright and `ll_merge()` never runs — so no `Auto-merging` line is
    /// emitted. This is what keeps stock silent when one side only flipped the
    /// executable bit while the other rewrote the content.
    ///
    /// The line itself lives in the `S_ISREG` arm only (merge-ort.c:2278), so a
    /// symlink or gitlink merge never reaches it either.
    fn needs_content_merge(&self) -> bool {
        if !(self.our_mode.is_blob() && self.their_mode.is_blob()) {
            return false;
        }
        if self.our_id == self.their_id {
            return false;
        }
        match self.base {
            Some(base) => self.our_id != base && self.their_id != base,
            None => true,
        }
    }

    /// merge-ort's `reason` for the content notice (merge-ort.c:4354-4358).
    fn content_reason(&self) -> &'static str {
        if self.merged_is_gitlink {
            "submodule"
        } else if self.no_base {
            "add/add"
        } else {
            "content"
        }
    }

    /// git's binary-merge trigger: `merge_3way()` emits its `warning:` line when
    /// `ll_merge()` returns `LL_MERGE_BINARY_CONFLICT`, which happens when any of
    /// the base/ours/theirs blobs is binary.
    fn any_is_binary(&self, repo: &gix::Repository) -> Result<bool> {
        for id in self.base.iter().chain([&self.our_id, &self.their_id]) {
            if is_binary(repo, id)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Whether a rewrite carried a content change as well as a rename, which is what
/// separates a bare rename/delete from one that also earns a modify/delete.
fn rewrite_changed_content(change: &Change) -> bool {
    matches!(change, Change::Rewrite { source_id, id, .. } if source_id != id)
}

/// Whether the two changes are two different *non-directory* types at one path,
/// merge-ort's `filemask >= 6 && S_IFMT differs` case (merge-ort.c:4216-4218).
fn is_type_clash(ours: &Change, theirs: &Change) -> bool {
    let (a, b) = (change_mode(ours), change_mode(theirs));
    !a.is_tree()
        && !b.is_tree()
        && ours.location() == theirs.location()
        && (a.is_blob(), a.is_link(), a.is_commit()) != (b.is_blob(), b.is_link(), b.is_commit())
}

/// The post-change mode of `change` (the rename destination for rewrites).
fn change_mode(change: &Change) -> gix::object::tree::EntryMode {
    match change {
        Change::Addition { entry_mode, .. }
        | Change::Deletion { entry_mode, .. }
        | Change::Modification { entry_mode, .. }
        | Change::Rewrite { entry_mode, .. } => *entry_mode,
    }
}

/// The post-change id of `change` (the rename destination for rewrites).
fn change_id(change: &Change) -> ObjectId {
    match change {
        Change::Addition { id, .. }
        | Change::Deletion { id, .. }
        | Change::Modification { id, .. }
        | Change::Rewrite { id, .. } => *id,
    }
}

/// git's binary heuristic: a NUL byte within the first 8000 bytes of the blob.
fn is_binary(repo: &gix::Repository, id: &ObjectId) -> Result<bool> {
    let data = repo.find_object(*id)?.data.clone();
    let head = &data[..data.len().min(8000)];
    Ok(head.contains(&0))
}

/// The two command-line operands, and the tree of the first one — peeled at most
/// once, and only when a message class actually needs to name a side.
struct Operands<'r, 's> {
    label1: &'s str,
    label2: &'s str,
    operand1: Operand1<'s>,
    tree1: Option<gix::Tree<'r>>,
}

impl<'r, 's> Operands<'r, 's> {
    fn new(label1: &'s str, label2: &'s str, operand1: Operand1<'s>) -> Self {
        Operands {
            label1,
            label2,
            operand1,
            tree1: None,
        }
    }

    /// Whether operand 1's tree holds a **non-tree** entry at `path`.
    ///
    /// Non-tree, not merely "present": a directory/file clash has one operand
    /// carrying a *directory* at the very path whose file the message is about,
    /// and treating that as a hit would name the wrong branch.
    fn holds(&mut self, repo: &'r gix::Repository, path: &BStr) -> Result<bool> {
        if self.tree1.is_none() {
            let tree = match &self.operand1 {
                // Re-reading an operand this port already resolved, to attribute
                // a path to a side. git holds the trees from the original
                // resolution and never asks again, so this second
                // `get_oid_basic()` is this port's alone and must not add a
                // second `refname … is ambiguous.` to the operand.
                Operand1::Spec(spec) => {
                    let _quiet = crate::objname::AmbiguityWarnings::off();
                    repo.rev_parse_single(*spec)?.object()?.peel_to_tree()?
                }
                Operand1::Tree(id) => repo.find_object(*id)?.peel_to_tree()?,
            };
            self.tree1 = Some(tree);
        }
        let tree = self.tree1.as_ref().expect("just peeled");
        Ok(tree
            .lookup_entry(path.split(|&b| b == b'/'))?
            .is_some_and(|e| !e.mode().is_tree()))
    }

    /// `(<operand holding a file at `path`>, <the other operand>)`.
    fn split_at(&mut self, repo: &'r gix::Repository, path: &BStr) -> Result<(&'s str, &'s str)> {
        Ok(if self.holds(repo, path)? {
            (self.label1, self.label2)
        } else {
            (self.label2, self.label1)
        })
    }
}
