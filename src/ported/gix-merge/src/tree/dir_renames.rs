//! merge-ort's directory rename detection (merge-ort.c, git v2.55.0).
//!
//! git decides where a directory went from the *file* renames each side made,
//! not from whether a whole tree object moved: `update_dir_rename_counts()`
//! (diffcore-rename.c:455-570) tallies `old_dir -> new_dir` for every rename
//! out of a directory that side removed, `get_provisional_directory_renames()`
//! (merge-ort.c:2457-2511) keeps the destination with a strict majority, and
//! `collect_renames()` (merge-ort.c:3481-3541) moves every path the *other*
//! side added or renamed into such a directory along with it.
//!
//! Which renames count depends on which paths merge-ort looks at at all, and
//! that is decided while it walks the three trees: `collect_merge_info_callback()`
//! (merge-ort.c:1256-1506) stops at subtrees all three sides agree on, defers
//! subtrees one side left alone (`handle_deferred_entries()`, merge-ort.c:1553-1720),
//! and records which deletions are worth pairing with an addition
//! (`relevant_sources`, `add_pair()` at merge-ort.c:1085-1149). Only the parts of
//! that walk directory renames read are kept here: the per-path masks
//! `path_in_way()` consults, `dirs_removed[]` with its relevance, the
//! relevant sources and the add/delete pairs. The remembered-renames cache
//! (`cached_pairs`, `cached_target_names`) that merge-ort carries from one
//! picked commit to the next is not ported, so every merge here behaves like
//! the first commit of a sequence, where that cache is empty.
//!
//! `gix-diff` has already paired deletions with additions by the time this
//! runs. A [`Change::Rewrite`] only counts as git's `'R'` pair when merge-ort
//! would have queued both halves and let diffcore see the source: exact renames
//! are found among every queued source, inexact ones only among
//! `relevant_sources` (diffcore-rename.c:1463-1494), and no side without a
//! relevant source runs rename detection at all (`possible_side_renames()`,
//! merge-ort.c:3233-3238). Anything else is git's `'A'` at the destination.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use bstr::{BStr, BString, ByteSlice, ByteVec};
use gix_diff::tree_with_rewrites::Change;
use gix_hash::ObjectId;
use gix_object::{FindExt, tree::EntryMode};

use crate::tree::{
    Conflict, ConflictIndexEntry, ConflictMapping, DirectoryRenames, Error, Resolution, ResolutionFailure,
    utils::ChangeList,
};

const MERGE_BASE: usize = 0;
const MERGE_SIDE1: usize = 1;
const MERGE_SIDE2: usize = 2;

/// `enum dir_rename_relevance` (diffcore.h:184-188).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Relevance {
    #[default]
    NotRelevant,
    ForAncestor,
    ForSelf,
}

/// One side's version of a path, `struct name_entry` reduced to what is compared.
#[derive(Clone, Copy, Debug)]
struct NameEntry {
    mode: EntryMode,
    id: ObjectId,
}

/// The fields of `struct merged_info`/`struct conflict_info` that directory
/// renames read or update.
#[derive(Clone, Debug)]
struct PathInfo {
    clean: bool,
    filemask: u8,
    dirmask: u8,
    match_mask: u8,
    df_conflict: bool,
    stages: [Option<NameEntry>; 3],
}

/// `renames->deferred[side]` without the remembered-renames `target_dirs`,
/// which stay empty without the cache.
#[derive(Default)]
struct Deferred {
    trivial_merges_okay: bool,
    possible_trivial_merges: Vec<(BString, u8)>,
}

/// What `collect_merge_info()` leaves behind for directory rename detection.
struct MergeInfo<'a, Find> {
    objects: &'a Find,
    paths: HashMap<BString, PathInfo>,
    dirs_removed: [HashMap<BString, Relevance>; 3],
    relevant_sources: [BTreeSet<BString>; 3],
    added: [HashSet<BString>; 3],
    deleted: [HashSet<BString>; 3],
    dir_rename_mask: u8,
    deferred: [Deferred; 3],
}

fn same(a: Option<NameEntry>, b: Option<NameEntry>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a.mode.kind() == b.mode.kind() && a.id == b.id)
}

fn join(dirname: &BStr, name: &BStr) -> BString {
    let mut out = dirname.to_owned();
    if !out.is_empty() {
        out.push(b'/');
    }
    out.push_str(name);
    out
}

fn dirname_of(path: &BStr) -> &BStr {
    path.rfind_byte(b'/').map_or_else(|| b"".as_bstr(), |pos| path[..pos].as_bstr())
}

impl<Find: gix_object::FindObjectOrHeader> MergeInfo<'_, Find> {
    fn read_tree(&self, id: Option<ObjectId>) -> Result<Vec<(BString, NameEntry)>, Error> {
        let Some(id) = id else { return Ok(Vec::new()) };
        let mut buf = Vec::new();
        let tree = self.objects.find_tree(&id, &mut buf)?;
        Ok(tree
            .entries
            .iter()
            .map(|e| {
                (
                    e.filename.to_owned(),
                    NameEntry {
                        mode: e.mode,
                        id: e.oid.to_owned(),
                    },
                )
            })
            .collect())
    }

    /// `traverse_trees()` with `collect_merge_info_callback()` as callback, or
    /// `traverse_trees_wrapper()` when `dir_rename_mask` is 2 or 4
    /// (merge-ort.c:996-1036), which reads every entry first so an added file
    /// anywhere in the directory raises the mask to `0x07` for all of them
    /// (merge-ort.c:964-965).
    fn traverse(&mut self, dirname: &BStr, trees: [Option<ObjectId>; 3]) -> Result<(), Error> {
        let mut entries = BTreeMap::<BString, [Option<NameEntry>; 3]>::new();
        for (side, tree) in trees.into_iter().enumerate() {
            for (name, entry) in self.read_tree(tree)? {
                entries.entry(name).or_default()[side] = Some(entry);
            }
        }
        if self.dir_rename_mask == 2 || self.dir_rename_mask == 4 {
            for names in entries.values() {
                let (mask, dirmask) = masks(names);
                let filemask = mask & !dirmask;
                if filemask != 0 && filemask == self.dir_rename_mask {
                    self.dir_rename_mask = 0x07;
                }
            }
        }
        for (name, names) in entries {
            self.callback(dirname, name.as_bstr(), names)?;
        }
        Ok(())
    }

    /// `collect_merge_info_callback()` (merge-ort.c:1256-1506).
    fn callback(&mut self, dirname: &BStr, name: &BStr, names: [Option<NameEntry>; 3]) -> Result<(), Error> {
        let prev_dir_rename_mask = self.dir_rename_mask;
        let (mask, dirmask) = masks(&names);
        let filemask = mask & !dirmask;
        let side1_matches_mbase = same(names[0], names[1]);
        let side2_matches_mbase = same(names[0], names[2]);
        let sides_match = same(names[1], names[2]);
        let df_conflict = filemask != 0 && dirmask != 0;

        let mut match_mask = 0;
        if side1_matches_mbase {
            match_mask = if side2_matches_mbase { 7 } else { 3 };
        } else if side2_matches_mbase {
            match_mask = 5;
        } else if sides_match {
            match_mask = 6;
        }

        let fullpath = join(dirname, name);
        let resolved = (side1_matches_mbase && side2_matches_mbase)
            || (filemask == 0x07 && (sides_match || side1_matches_mbase || side2_matches_mbase));
        if resolved {
            self.paths.insert(
                fullpath,
                PathInfo {
                    clean: true,
                    filemask,
                    dirmask,
                    match_mask: 0,
                    df_conflict: false,
                    stages: names,
                },
            );
            return Ok(());
        }

        self.collect_rename_info(dirname, fullpath.as_bstr(), filemask, dirmask, match_mask);

        let mut info = PathInfo {
            clean: false,
            filemask,
            dirmask,
            match_mask,
            df_conflict,
            stages: names,
        };

        if dirmask == 0 {
            self.paths.insert(fullpath, info);
            return Ok(());
        }

        let mut side = if side1_matches_mbase {
            MERGE_SIDE2
        } else if side2_matches_mbase {
            MERGE_SIDE1
        } else {
            MERGE_BASE
        };
        if filemask == 0 && (dirmask == 2 || dirmask == 4) {
            info.match_mask = 7 - dirmask;
            side = usize::from(dirmask / 2);
        }
        if self.dir_rename_mask != 0x07 && side != MERGE_BASE && self.deferred[side].trivial_merges_okay {
            self.deferred[side]
                .possible_trivial_merges
                .push((fullpath.clone(), self.dir_rename_mask));
            self.paths.insert(fullpath, info);
            self.dir_rename_mask = prev_dir_rename_mask;
            return Ok(());
        }

        info.match_mask &= filemask;
        let trees = tree_ids(&names, dirmask);
        self.paths.insert(fullpath.clone(), info);
        self.traverse(fullpath.as_bstr(), trees)?;
        self.dir_rename_mask = prev_dir_rename_mask;
        Ok(())
    }

    /// `collect_rename_info()` (merge-ort.c:1151-1254) with `add_pair()`
    /// (merge-ort.c:1085-1149) reduced to the pairs and their relevance.
    fn collect_rename_info(
        &mut self,
        dirname: &BStr,
        fullname: &BStr,
        filemask: u8,
        dirmask: u8,
        match_mask: u8,
    ) {
        if self.dir_rename_mask != 0x07 && (dirmask == 3 || dirmask == 5) {
            self.dir_rename_mask = dirmask & !1;
        }

        if dirmask == 1 || dirmask == 3 || dirmask == 5 {
            let sides = (0x07 - dirmask) / 2;
            let relevance = if self.dir_rename_mask == 0x07 {
                Relevance::ForAncestor
            } else {
                Relevance::NotRelevant
            };
            if sides & 1 != 0 {
                self.dirs_removed[MERGE_SIDE1].insert(fullname.to_owned(), relevance);
            }
            if sides & 2 != 0 {
                self.dirs_removed[MERGE_SIDE2].insert(fullname.to_owned(), relevance);
            }
        }

        if self.dir_rename_mask == 0x07 && (filemask == 2 || filemask == 4) {
            let side = 3 - usize::from(filemask >> 1);
            self.dirs_removed[side].insert(dirname.to_owned(), Relevance::ForSelf);
        }

        if filemask == 0 || filemask == 7 {
            return;
        }
        for side in MERGE_SIDE1..=MERGE_SIDE2 {
            let side_mask = 1u8 << side;
            if filemask & 1 != 0 && filemask & side_mask == 0 {
                let content_relevant = match_mask & filemask == 0;
                let location_relevant = self.dir_rename_mask == 0x07;
                if content_relevant || location_relevant {
                    self.relevant_sources[side].insert(fullname.to_owned());
                }
                self.deleted[side].insert(fullname.to_owned());
            }
            if filemask & 1 == 0 && filemask & side_mask != 0 {
                self.added[side].insert(fullname.to_owned());
            }
        }
    }

    /// `handle_deferred_entries()` (merge-ort.c:1553-1720). With no cached
    /// pairs, the loop over `relevant_sources[side]` turns the optimization off
    /// as soon as it sees a single entry.
    fn handle_deferred_entries(&mut self) -> Result<(), Error> {
        for side in MERGE_SIDE1..=MERGE_SIDE2 {
            let optimization_okay = self.relevant_sources[side].is_empty();
            self.deferred[side].trivial_merges_okay = optimization_okay;
            let copy = std::mem::take(&mut self.deferred[side].possible_trivial_merges);
            for (path, dir_rename_mask) in copy {
                if optimization_okay {
                    self.resolve_trivial_directory_merge(path.as_bstr());
                    continue;
                }
                let info = self.paths.get_mut(&path).expect("deferred paths were recorded");
                let trees = tree_ids(&info.stages, info.dirmask);
                info.match_mask &= info.filemask;
                self.dir_rename_mask = dir_rename_mask;
                self.traverse(path.as_bstr(), trees)?;
            }
            for (path, _) in std::mem::take(&mut self.deferred[side].possible_trivial_merges) {
                self.resolve_trivial_directory_merge(path.as_bstr());
            }
        }
        Ok(())
    }

    /// `resolve_trivial_directory_merge()` (merge-ort.c:1508-1551).
    fn resolve_trivial_directory_merge(&mut self, path: &BStr) {
        let info = self.paths.get_mut(path).expect("deferred paths were recorded");
        info.dirmask &= !info.match_mask;
        info.filemask &= !info.match_mask;
        info.match_mask = 0;
        info.clean = !info.df_conflict || info.dirmask != 0;
        info.df_conflict = false;
    }
}

fn masks(names: &[Option<NameEntry>; 3]) -> (u8, u8) {
    let (mut mask, mut dirmask) = (0u8, 0u8);
    for (i, entry) in names.iter().enumerate() {
        if let Some(entry) = entry {
            mask |= 1 << i;
            if entry.mode.is_tree() {
                dirmask |= 1 << i;
            }
        }
    }
    (mask, dirmask)
}

/// The tree each side contributes when recursing: `names[i].oid` for the
/// sides that have a directory here. merge-ort reuses the base's descriptor
/// where a side matches it, which reads the same entries.
fn tree_ids(names: &[Option<NameEntry>; 3], dirmask: u8) -> [Option<ObjectId>; 3] {
    let mut trees = [None; 3];
    for (i, tree) in trees.iter_mut().enumerate() {
        if dirmask & (1 << i) != 0 {
            *tree = names[i].map(|e| e.id);
        }
    }
    trees
}

/// `diff_filepair` with the status `resolve_diffpair_statuses()` gives it,
/// restricted to the `'A'` and `'R'` pairs `collect_renames()` looks at.
struct Pair {
    change_idx: usize,
    renamed: bool,
    /// `p->one->path`
    one: BString,
    /// `p->two->path`
    two: BString,
}

/// `struct collision_info` (merge-ort.c:2322-2325).
#[derive(Default)]
struct CollisionInfo {
    source_files: BTreeSet<BString>,
    reported_already: bool,
}

/// Run merge-ort's directory rename detection over both sides' changes,
/// moving each change a directory rename carries along to its new location
/// and returning the conflicts and notices it produces, in the order
/// merge-ort's `path_msg()` calls would record them.
///
/// `changes[0]` is ignored; `changes[1]` and `changes[2]` are the changes of
/// `side1_tree` and `side2_tree` against `base_tree`.
pub(super) fn detect_and_apply<Find: gix_object::FindObjectOrHeader>(
    objects: &Find,
    trees: [&gix_hash::oid; 3],
    changes: [&mut ChangeList; 2],
    directory_renames: DirectoryRenames,
    call_depth: u8,
) -> Result<Vec<Conflict>, Error> {
    let mut info = MergeInfo {
        objects,
        paths: HashMap::new(),
        dirs_removed: Default::default(),
        relevant_sources: Default::default(),
        added: Default::default(),
        deleted: Default::default(),
        dir_rename_mask: 0,
        deferred: [
            Deferred::default(),
            Deferred {
                trivial_merges_okay: true,
                possible_trivial_merges: Vec::new(),
            },
            Deferred {
                trivial_merges_okay: true,
                possible_trivial_merges: Vec::new(),
            },
        ],
    };
    info.traverse(b"".as_bstr(), trees.map(|t| Some(t.to_owned())))?;
    info.handle_deferred_entries()?;

    let [side1_changes, side2_changes] = changes;
    let changes: [&mut ChangeList; 3] = [&mut Vec::new(), side1_changes, side2_changes];

    let mut pairs: [Vec<Pair>; 3] = Default::default();
    let mut dir_rename_count: [BTreeMap<BString, BTreeMap<BString, usize>>; 3] = Default::default();
    for side in MERGE_SIDE1..=MERGE_SIDE2 {
        // `possible_side_renames()` (merge-ort.c:3233-3238): a side with no
        // relevant source skips diffcore entirely, leaving every pair an add
        // or a delete.
        let detection_runs = !info.relevant_sources[side].is_empty()
            && (!info.added[side].is_empty() || !info.deleted[side].is_empty());
        for (change_idx, tracked) in changes[side].iter().enumerate() {
            let change = &tracked.inner;
            if change.entry_mode().is_tree() {
                continue;
            }
            match change {
                Change::Addition { location, .. } if info.added[side].contains(location) => {
                    pairs[side].push(Pair {
                        change_idx,
                        renamed: false,
                        one: location.clone(),
                        two: location.clone(),
                    });
                }
                Change::Rewrite {
                    source_location,
                    source_id,
                    id,
                    location,
                    copy: false,
                    ..
                } if info.added[side].contains(location) => {
                    let exact = source_id == id;
                    let renamed = detection_runs
                        && info.deleted[side].contains(source_location)
                        && (exact || info.relevant_sources[side].contains(source_location));
                    pairs[side].push(Pair {
                        change_idx,
                        renamed,
                        one: if renamed {
                            source_location.clone()
                        } else {
                            location.clone()
                        },
                        two: location.clone(),
                    });
                }
                Change::Addition { .. }
                | Change::Rewrite { .. }
                | Change::Deletion { .. }
                | Change::Modification { .. } => {}
            }
        }
        pairs[side].sort_by(|a, b| a.two.cmp(&b.two));
        if detection_runs {
            for pair in pairs[side].iter().filter(|p| p.renamed) {
                update_dir_rename_counts(
                    &mut dir_rename_count[side],
                    &info.dirs_removed[side],
                    pair.one.as_bstr(),
                    pair.two.as_bstr(),
                );
            }
            // `cleanup_dir_rename_info()` keeping the counts
            // (diffcore-rename.c:716-740): only directories whose removal was
            // found relevant survive.
            dir_rename_count[side].retain(|dir, _| {
                info.dirs_removed[side]
                    .get(dir)
                    .is_some_and(|relevance| *relevance != Relevance::NotRelevant)
            });
        }
    }

    let mut out = Vec::new();
    let need_dir_renames = call_depth == 0 && directory_renames != DirectoryRenames::Disabled;
    let mut dir_renames: [BTreeMap<BString, BString>; 3] = Default::default();
    if need_dir_renames {
        for side in MERGE_SIDE1..=MERGE_SIDE2 {
            get_provisional_directory_renames(
                &dir_rename_count[side],
                &mut dir_renames[side],
                trees[0].kind(),
                &mut out,
            );
        }
        handle_directory_level_conflicts(&mut dir_renames);
    }

    let mut collisions = [
        BTreeMap::new(),
        compute_collisions(&dir_renames[MERGE_SIDE2], &pairs[MERGE_SIDE1]),
        compute_collisions(&dir_renames[MERGE_SIDE1], &pairs[MERGE_SIDE2]),
    ];
    // `pair->two->path` after `apply_directory_rename_modifications()`, and the
    // renames whose stages were recorded with the conflict they produced.
    let mut final_two: [Vec<BString>; 3] = [
        Vec::new(),
        pairs[MERGE_SIDE1].iter().map(|p| p.two.clone()).collect(),
        pairs[MERGE_SIDE2].iter().map(|p| p.two.clone()).collect(),
    ];
    let mut moved_renames = Vec::new();
    for side in MERGE_SIDE1..=MERGE_SIDE2 {
        let other_side = 3 - side;
        for (pair_idx, pair) in pairs[side].iter().enumerate() {
            if directory_renames == DirectoryRenames::Disabled && pair.renamed {
                continue;
            }
            let Some(new_path) = check_for_directory_rename(
                &mut info.paths,
                pair,
                side,
                &dir_renames[other_side],
                &dir_renames[side],
                &mut collisions,
                &changes[side][pair.change_idx].inner,
                &mut out,
            ) else {
                continue;
            };
            final_two[side][pair_idx] = new_path.clone();
            apply_directory_rename_modifications(
                &mut info.paths,
                pair,
                side,
                new_path,
                &mut *changes[side],
                directory_renames,
                &mut out,
            );
            if pair.renamed {
                moved_renames.push((out.len() - 1, side, pair_idx));
            }
        }
    }

    // `process_renames()` runs once every pair is where directory renames put
    // it. Two renames of one source to different paths are a
    // rename/rename(1to2) (merge-ort.c:2961-3068), which leaves the base at
    // the old path and carries neither it nor the other side's version to a
    // new one. Renames that ended at the same path are a rename/rename(1to1),
    // which does carry the base (merge-ort.c:2983-2995).
    for (conflict_idx, side, pair_idx) in moved_renames {
        let other_side = 3 - side;
        let pair = &pairs[side][pair_idx];
        let renamed_to_elsewhere = pairs[other_side]
            .iter()
            .enumerate()
            .any(|(other_idx, other)| {
                other.renamed && other.one == pair.one && final_two[other_side][other_idx] != final_two[side][pair_idx]
            });
        if renamed_to_elsewhere {
            let entries = &mut out[conflict_idx].entries;
            entries[MERGE_BASE] = None;
            entries[other_side] = None;
        }
    }
    Ok(out)
}

/// `update_dir_rename_counts()` (diffcore-rename.c:455-570).
fn update_dir_rename_counts(
    dir_rename_count: &mut BTreeMap<BString, BTreeMap<BString, usize>>,
    dirs_removed: &HashMap<BString, Relevance>,
    oldname: &BStr,
    newname: &BStr,
) {
    let mut old_dir: &BStr = oldname;
    let mut new_dir: &BStr = newname;
    let mut first_time_in_loop = true;
    loop {
        let old_sub_dir = &old_dir[old_dir.rfind_byte(b'/').map_or(0, |p| p + 1)..];
        let new_sub_dir = &new_dir[new_dir.rfind_byte(b'/').map_or(0, |p| p + 1)..];
        old_dir = dirname_of(old_dir);
        let Some(drd_flag) = dirs_removed.get(old_dir).copied() else {
            break;
        };
        new_dir = if new_dir.contains(&b'/') {
            dirname_of(new_dir)
        } else {
            b"".as_bstr()
        };

        // Only the trailing directory components that match keep the chain
        // of implied renames going; the first step only strips the basename.
        if !first_time_in_loop && old_sub_dir != new_sub_dir {
            break;
        }

        if drd_flag == Relevance::ForSelf || first_time_in_loop {
            *dir_rename_count
                .entry(old_dir.to_owned())
                .or_default()
                .entry(new_dir.to_owned())
                .or_default() += 1;
        }

        first_time_in_loop = false;
        if drd_flag == Relevance::NotRelevant {
            break;
        }
        if old_dir.is_empty() || new_dir.is_empty() {
            break;
        }
    }
}

/// `get_provisional_directory_renames()` (merge-ort.c:2457-2511).
fn get_provisional_directory_renames(
    dir_rename_count: &BTreeMap<BString, BTreeMap<BString, usize>>,
    dir_renames: &mut BTreeMap<BString, BString>,
    hash_kind: gix_hash::Kind,
    out: &mut Vec<Conflict>,
) {
    for (source_dir, counts) in dir_rename_count {
        let (mut max, mut bad_max) = (0, 0);
        let mut best = None;
        for (target_dir, &count) in counts {
            if count == max {
                bad_max = max;
            } else if count > max {
                max = count;
                best = Some(target_dir);
            }
        }
        if max == 0 {
            continue;
        }
        if bad_max == max {
            out.push(directory_conflict(
                Err(ResolutionFailure::DirectoryRenameSplit {
                    source_dir: source_dir.clone(),
                }),
                source_dir.as_bstr(),
                hash_kind,
            ));
        } else {
            dir_renames.insert(source_dir.clone(), best.expect("max > 0").clone());
        }
    }
}

/// `handle_directory_level_conflicts()` (merge-ort.c:2513-2533): a directory
/// both sides renamed is renamed by neither as far as the other side's paths go.
fn handle_directory_level_conflicts(dir_renames: &mut [BTreeMap<BString, BString>; 3]) {
    let duplicated: Vec<BString> = dir_renames[MERGE_SIDE1]
        .keys()
        .filter(|dir| dir_renames[MERGE_SIDE2].contains_key(*dir))
        .cloned()
        .collect();
    for dir in duplicated {
        dir_renames[MERGE_SIDE1].remove(&dir);
        dir_renames[MERGE_SIDE2].remove(&dir);
    }
}

/// `check_dir_renamed()` (merge-ort.c:2535-2550).
fn check_dir_renamed<'a>(path: &BStr, dir_renames: &'a BTreeMap<BString, BString>) -> Option<(&'a BString, &'a BString)> {
    let mut temp = path;
    while let Some(end) = temp.rfind_byte(b'/') {
        temp = temp[..end].as_bstr();
        if let Some(entry) = dir_renames.get_key_value(temp) {
            return Some(entry);
        }
    }
    None
}

/// `apply_dir_rename()` (merge-ort.c:2334-2360).
fn apply_dir_rename((old_dir, new_dir): (&BString, &BString), old_path: &BStr) -> BString {
    let mut oldlen = old_dir.len();
    if new_dir.is_empty() {
        oldlen += 1;
    }
    let mut new_path = new_dir.clone();
    new_path.push_str(&old_path[oldlen..]);
    new_path
}

/// `compute_collisions()` (merge-ort.c:2552-2603).
fn compute_collisions(dir_renames: &BTreeMap<BString, BString>, pairs: &[Pair]) -> BTreeMap<BString, CollisionInfo> {
    let mut collisions = BTreeMap::<BString, CollisionInfo>::new();
    if dir_renames.is_empty() {
        return collisions;
    }
    for pair in pairs {
        let Some(rename_info) = check_dir_renamed(pair.two.as_bstr(), dir_renames) else {
            continue;
        };
        let new_path = apply_dir_rename(rename_info, pair.two.as_bstr());
        collisions.entry(new_path).or_default().source_files.insert(pair.two.clone());
    }
    collisions
}

/// `path_in_way()` (merge-ort.c:2362-2376).
fn path_in_way(paths: &HashMap<BString, PathInfo>, path: &BStr, side_mask: u8, pair: &Pair) -> bool {
    let Some(mi) = paths.get(path) else {
        return false;
    };
    // The last clause is for testcases 12[npq] of t6423.
    mi.clean
        || side_mask & (mi.filemask | mi.dirmask) != 0
        || (mi.filemask & 0x01 != 0 && pair.one.as_bstr() != path)
}

/// `check_for_directory_rename()` (merge-ort.c:2626-2692) together with
/// `handle_path_level_conflicts()` (merge-ort.c:2378-2455).
#[expect(clippy::too_many_arguments)]
fn check_for_directory_rename(
    paths: &mut HashMap<BString, PathInfo>,
    pair: &Pair,
    side_index: usize,
    dir_renames: &BTreeMap<BString, BString>,
    dir_rename_exclusions: &BTreeMap<BString, BString>,
    collisions: &mut [BTreeMap<BString, CollisionInfo>; 3],
    change: &Change,
    out: &mut Vec<Conflict>,
) -> Option<BString> {
    let path = pair.two.as_bstr();
    let other_side = 3 - side_index;
    if dir_renames.is_empty() {
        return None;
    }
    if collisions[other_side].contains_key(path) {
        return None;
    }
    let rename_info = check_dir_renamed(path, dir_renames)?;

    let (old_dir, new_dir) = rename_info;
    if dir_rename_exclusions.contains_key(new_dir) {
        out.push(Conflict::with_resolution(
            Resolution::DirectoryRenameSkippedDueToRerename {
                old_dir: old_dir.clone(),
                path: path.to_owned(),
                new_dir: new_dir.clone(),
            },
            (change, change, ConflictMapping::Original, ConflictMapping::Original),
            [None, None, None],
        ));
        return None;
    }

    // handle_path_level_conflicts()
    let new_path = apply_dir_rename(rename_info, path);
    let c_info = collisions[side_index]
        .get_mut(&new_path)
        .expect("BUG: compute_collisions() recorded every renamed path");
    let failure = if c_info.reported_already {
        return None;
    } else if path_in_way(paths, new_path.as_bstr(), 1 << side_index, pair) {
        c_info.reported_already = true;
        ResolutionFailure::DirectoryRenameFileInWay {
            new_path: new_path.clone(),
            source_files: c_info.source_files.iter().cloned().collect(),
        }
    } else if c_info.source_files.len() > 1 {
        c_info.reported_already = true;
        ResolutionFailure::DirectoryRenameCollision {
            new_path: new_path.clone(),
            source_files: c_info.source_files.iter().cloned().collect(),
        }
    } else {
        return Some(new_path);
    };
    out.push(Conflict::without_resolution(
        failure,
        (change, change, ConflictMapping::Original, ConflictMapping::Original),
        [None, None, None],
    ));
    None
}

/// `apply_directory_rename_modifications()` (merge-ort.c:2694-2901): record the
/// move in `paths` so later `path_in_way()` checks see it, move the change
/// itself, and report it.
fn apply_directory_rename_modifications(
    paths: &mut HashMap<BString, PathInfo>,
    pair: &Pair,
    side: usize,
    new_path: BString,
    changes: &mut ChangeList,
    directory_renames: DirectoryRenames,
    out: &mut Vec<Conflict>,
) {
    let old_path = pair.two.as_bstr();
    let mut ci = paths.get(old_path).cloned().expect("BUG: every pair has a path entry");

    // Parent directories missing from `paths`, outermost first.
    let mut dirs_to_insert = Vec::new();
    let mut cur_path = new_path.as_bstr();
    while let Some(last_slash) = cur_path.rfind_byte(b'/') {
        let parent_name = cur_path[..last_slash].as_bstr();
        if paths.contains_key(parent_name) {
            break;
        }
        dirs_to_insert.push(parent_name.to_owned());
        cur_path = parent_name;
    }
    for dir in dirs_to_insert.into_iter().rev() {
        paths.insert(
            dir,
            PathInfo {
                clean: false,
                filemask: 0,
                dirmask: ci.filemask,
                match_mask: 0,
                df_conflict: false,
                stages: [None; 3],
            },
        );
    }

    if ci.dirmask == 0 {
        paths.remove(old_path);
    } else {
        let dir_ci = paths.get_mut(old_path).expect("present");
        dir_ci.filemask = 0;
        dir_ci.clean = true;
        ci.dirmask = 0;
    }
    match paths.get_mut(&new_path) {
        None => {
            paths.insert(new_path.clone(), ci.clone());
        }
        Some(new_ci) => {
            new_ci.filemask |= ci.filemask;
            if new_ci.dirmask != 0 {
                new_ci.df_conflict = true;
            }
        }
    }

    let (mode, id) = changes[pair.change_idx].inner.entry_mode_and_id();
    let (mode, id) = (mode, id.to_owned());
    // The notice names git's pair status, so a rewrite diffcore would not have
    // paired is reported as the addition git saw.
    let original = if pair.renamed {
        changes[pair.change_idx].inner.clone()
    } else {
        Change::Addition {
            location: pair.two.clone(),
            relation: None,
            entry_mode: mode,
            id,
        }
    };
    let mut entries: [Option<ConflictIndexEntry>; 3] = [None, None, None];
    entries[side] = Some(ConflictIndexEntry {
        mode,
        id,
        path_hint: None,
    });
    if pair.renamed {
        // A rename carries the base and the other side's version of its source
        // along to the destination (`process_renames()`, merge-ort.c:3192-3211),
        // and with `path_conflict` set they stay there as stages.
        // `detect_and_apply()` takes them back for a rename/rename(1to2).
        if let Some(source) = paths.get(pair.one.as_bstr()) {
            for stage in [MERGE_BASE, 3 - side] {
                entries[stage] = source.stages[stage]
                    .filter(|e| !e.mode.is_tree())
                    .map(|e| ConflictIndexEntry {
                        mode: e.mode,
                        id: e.id,
                        path_hint: None,
                    });
            }
        }
    }

    let conflict = if directory_renames == DirectoryRenames::Applied {
        Conflict::with_resolution(
            Resolution::SourceLocationAffectedByRename {
                final_location: new_path.clone(),
            },
            (&original, &original, ConflictMapping::Original, ConflictMapping::Original),
            [None, None, None],
        )
    } else {
        Conflict::without_resolution(
            ResolutionFailure::DirectoryRenameSuggested {
                final_location: new_path.clone(),
            },
            (&original, &original, ConflictMapping::Original, ConflictMapping::Original),
            entries,
        )
    };
    out.push(conflict);

    let tracked = &mut changes[pair.change_idx];
    match &mut tracked.inner {
        // "Directory renames can result in rename-to-self" (merge-ort.c:2931-2942):
        // `process_renames()` skips a pair whose old and new path are the same
        // entry, so `process_entry()` finds the base and this side's version
        // at one path, which is a modification of it and no rename at all.
        Change::Rewrite {
            source_location,
            source_entry_mode,
            source_id,
            entry_mode,
            id,
            location,
            ..
        } if pair.renamed && *source_location == new_path => {
            tracked.location_before_directory_rename = Some(location.clone());
            tracked.inner = Change::Modification {
                location: new_path,
                previous_entry_mode: *source_entry_mode,
                previous_id: *source_id,
                entry_mode: *entry_mode,
                id: *id,
            };
        }
        Change::Addition { location, .. } | Change::Rewrite { location, .. } => {
            tracked.location_before_directory_rename = Some(std::mem::replace(location, new_path));
        }
        Change::Deletion { .. } | Change::Modification { .. } => unreachable!("only 'A' and 'R' pairs are moved"),
    }
}

/// A conflict about a directory rather than a single change: the directory is
/// carried as the location of a tree deletion on both sides so that consumers
/// reading `ours`/`theirs` find the path the conflict is about.
fn directory_conflict(
    resolution: Result<Resolution, ResolutionFailure>,
    dir: &BStr,
    hash_kind: gix_hash::Kind,
) -> Conflict {
    let change = Change::Deletion {
        location: dir.to_owned(),
        relation: None,
        entry_mode: gix_object::tree::EntryKind::Tree.into(),
        id: hash_kind.null(),
    };
    match resolution {
        Ok(resolution) => Conflict::with_resolution(
            resolution,
            (&change, &change, ConflictMapping::Original, ConflictMapping::Original),
            [None, None, None],
        ),
        Err(failure) => Conflict::without_resolution(
            failure,
            (&change, &change, ConflictMapping::Original, ConflictMapping::Original),
            [None, None, None],
        ),
    }
}
