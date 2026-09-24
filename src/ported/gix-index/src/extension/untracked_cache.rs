use bstr::BString;
use gix_hash::ObjectId;

use crate::{
    entry,
    entry::write::encode_varint,
    extension::{Signature, UntrackedCache},
    util::{read_u32, split_at_byte_exclusive, var_int},
};

impl UntrackedCache {
    /// Something identifying the location and machine that this cache is for.
    pub fn identifier(&self) -> &bstr::BStr {
        self.identifier.as_ref()
    }

    /// Stat and object id for the `.git/info/exclude` file, if available.
    pub fn info_exclude(&self) -> Option<&OidStat> {
        self.info_exclude.as_ref()
    }

    /// Stat and object id for the `core.excludesfile`, if available.
    pub fn excludes_file(&self) -> Option<&OidStat> {
        self.excludes_file.as_ref()
    }

    /// Usually `.gitignore`.
    pub fn exclude_filename_per_dir(&self) -> &bstr::BStr {
        self.exclude_filename_per_dir.as_ref()
    }

    /// The directory flags Git used while populating the cache.
    pub fn dir_flags(&self) -> u32 {
        self.dir_flags
    }

    /// A list of directories and sub-directories, with `directories[0]` being the root.
    pub fn directories(&self) -> &[Directory] {
        &self.directories
    }
}

/// A structure to track filesystem stat information along with an object id, linking a worktree file with what's in our ODB.
#[derive(Clone, Debug)]
pub struct OidStat {
    /// The file system stat information
    pub stat: entry::Stat,
    /// The id of the file in our ODB.
    pub id: ObjectId,
}

impl OidStat {
    /// The file system stat information.
    pub fn stat(&self) -> &entry::Stat {
        &self.stat
    }

    /// The id of the file in our ODB.
    pub fn id(&self) -> ObjectId {
        self.id
    }
}

/// A directory with information about its untracked files, and its sub-directories
#[derive(Clone, Debug)]
pub struct Directory {
    /// The directories name, or an empty string if this is the root directory.
    pub name: BString,
    /// Untracked files and directory names
    pub untracked_entries: Vec<BString>,
    /// indices for sub-directories similar to this one.
    pub sub_directories: Vec<usize>,

    /// The directories stat data, if available or valid // TODO: or is it the exclude file?
    pub stat: Option<entry::Stat>,
    /// The oid of a .gitignore file, if it exists
    pub exclude_file_oid: Option<ObjectId>,
    /// TODO: figure out what this really does
    pub check_only: bool,
}

impl Directory {
    /// The directory name, or an empty string if this is the root directory.
    /// `/` is always used as path-separator.
    pub fn name(&self) -> &bstr::BStr {
        self.name.as_ref()
    }

    /// Untracked files and directory names.
    pub fn untracked_entries(&self) -> &[BString] {
        &self.untracked_entries
    }

    /// Indices for sub-directories similar to this one.
    pub fn sub_directories(&self) -> &[usize] {
        &self.sub_directories
    }

    /// The directory stat data, if available and valid.
    pub fn stat(&self) -> Option<&entry::Stat> {
        self.stat.as_ref()
    }

    /// The oid of a `.gitignore` file, if it exists.
    pub fn exclude_file_oid(&self) -> Option<ObjectId> {
        self.exclude_file_oid
    }

    /// Whether Git marked this directory as check-only.
    pub fn check_only(&self) -> bool {
        self.check_only
    }
}

/// Only used as an indicator
pub const SIGNATURE: Signature = *b"UNTR";

/// `DIR_SHOW_OTHER_DIRECTORIES` (dir.h:228), the one `dir_flags` bit invalidation consults.
pub const DIR_SHOW_OTHER_DIRECTORIES: u32 = 1 << 1;
/// `DIR_HIDE_EMPTY_DIRECTORIES` (dir.h:231).
pub const DIR_HIDE_EMPTY_DIRECTORIES: u32 = 1 << 2;

/// `get_ident_string()` (dir.c:2882-2894) as `set_untracked_ident()` stores it (dir.c:2908-2917):
/// `Location <worktree>, system <sysname>` followed by a NUL, "for backward compatibility" with
/// the NUL-separated list the field used to hold.
///
/// `worktree` is `repo_get_work_tree()`, which is already absolute and free of symlinks; a
/// repository without one prints it as glibc and Darwin libc both print a `NULL` `%s`.
pub fn ident(worktree: Option<&std::path::Path>) -> BString {
    let mut ident: BString = match worktree {
        Some(worktree) => format!("Location {}, system {}", worktree.display(), sysname()),
        None => format!("Location (null), system {}", sysname()),
    }
    .into();
    ident.push(0);
    ident
}

/// `uts.sysname` from `uname(2)`.
#[cfg(unix)]
fn sysname() -> String {
    rustix::system::uname().sysname().to_string_lossy().into_owned()
}

/// `compat/mingw.c`'s `uname()` fills in `Windows`.
#[cfg(not(unix))]
fn sysname() -> String {
    "Windows".into()
}

impl UntrackedCache {
    /// `new_untracked_cache()` (dir.c:2941-2950): a cache with no directories yet, for the
    /// location `ident` names (see [`ident()`]), built with `dir_flags`.
    pub fn new(ident: BString, dir_flags: u32) -> Self {
        UntrackedCache {
            identifier: ident,
            info_exclude: None,
            excludes_file: None,
            exclude_filename_per_dir: ".gitignore".into(),
            dir_flags,
            directories: Vec::new(),
        }
    }

    /// `ident_in_untracked()` (dir.c:2896-2906): whether this cache was built at the location
    /// `ident` names. Only the first of the NUL-separated strings older gits stored is compared,
    /// as git's `strcmp()` does.
    pub fn ident_matches(&self, ident: &bstr::BStr) -> bool {
        let first = |s: &[u8]| s.split(|b| *b == 0).next().unwrap_or_default().to_vec();
        first(&self.identifier) == first(ident)
    }

    /// `untracked_cache_invalidate_path(istate, path, 1)` (dir.c:4015-4024), which is also
    /// `untracked_cache_add_to_index()` and `untracked_cache_remove_from_index()`
    /// (dir.c:4046-4056): the index gained or lost `path`, so the directory holding it can no
    /// longer vouch for its list of untracked names.
    ///
    /// Only the *safe* form is ported — every caller in `read-cache.c` and `unpack-trees.c`
    /// passes `safe_path = 1`; the `verify_path()` variant belongs to fsmonitor.
    ///
    /// ### Directories the cache does not hold
    ///
    /// `invalidate_one_component()` finds each component with `lookup_untracked()`, which
    /// *creates* a missing directory (dir.c:1086-1094). The one it creates is zeroed, so its
    /// `recurse` bit is clear and `write_one_dir()` never serialises it (dir.c:3653-3669), and a
    /// later walk would have created the same empty, invalid node on its own. The only thing
    /// the descent through it can change is the return value, which is decided by the leaf and
    /// is `dir_flags & DIR_SHOW_OTHER_DIRECTORIES` whatever the leaf is — so a component that
    /// is not in the cache ends the lookups here without being created.
    pub fn invalidate_path(&mut self, path: &bstr::BStr) {
        // `if (!istate->untracked || !istate->untracked->root) return;`
        if self.directories.is_empty() {
            return;
        }
        self.invalidate_one_component(Some(0), path);
    }

    /// `invalidate_one_component()` (dir.c:3992-4013).
    fn invalidate_one_component(&mut self, dir: Option<usize>, path: &[u8]) -> bool {
        let Some(slash) = path.iter().position(|b| *b == b'/') else {
            self.invalidate_one_directory(dir);
            return self.dir_flags & DIR_SHOW_OTHER_DIRECTORIES != 0;
        };
        let sub = dir.and_then(|dir| self.lookup_untracked(dir, &path[..slash]));
        let ret = self.invalidate_one_component(sub, &path[slash + 1..]);
        if ret {
            self.invalidate_one_directory(dir);
        }
        ret
    }

    /// `invalidate_one_directory()` (dir.c:3957-3964): `valid = 0` and the untracked names go.
    fn invalidate_one_directory(&mut self, dir: Option<usize>) {
        if let Some(dir) = dir.and_then(|dir| self.directories.get_mut(dir)) {
            dir.stat = None;
            dir.untracked_entries.clear();
        }
    }

    /// The finding half of `lookup_untracked()` (dir.c:1059-1095): a binary search of `dir`'s
    /// sub-directories, which git keeps in `strncmp()` order — plain byte order, a name sorting
    /// before every longer name it is a prefix of.
    fn lookup_untracked(&self, dir: usize, name: &[u8]) -> Option<usize> {
        let subs = &self.directories.get(dir)?.sub_directories;
        subs.binary_search_by(|&sub| self.directories[sub].name.as_slice().cmp(name))
            .ok()
            .map(|at| subs[at])
    }

    /// `write_untracked_extension()` (dir.c:3677-3727): serialise the cache as the body of an
    /// `UNTR` extension, `object_hash` naming the width of every hash in it.
    ///
    /// A null exclude-file hash is written with an all-zero stat. That is what stock writes for
    /// it: `add_patterns()` fills `oid_stat` only when the file opened (dir.c:1164-1206), so
    /// the stat git carries beside a null hash is the zeroed one `dir_struct` started with —
    /// which is also why decoding it as "absent" loses nothing.
    pub fn write_to(&self, out: &mut Vec<u8>, object_hash: gix_hash::Kind) {
        let null = object_hash.null();
        let zero = entry::Stat::default();
        let exclude = |oid_stat: Option<&OidStat>| oid_stat.map_or((zero, null), |o| (o.stat, o.id));
        let (info_exclude_stat, info_exclude_id) = exclude(self.info_exclude.as_ref());
        let (excludes_file_stat, excludes_file_id) = exclude(self.excludes_file.as_ref());

        out.extend_from_slice(&encode_varint(self.identifier.len() as u64));
        out.extend_from_slice(&self.identifier);
        // `struct ondisk_untracked_cache`: the two stats, then `dir_flags`.
        stat_data_to_disk(&info_exclude_stat, out);
        stat_data_to_disk(&excludes_file_stat, out);
        out.extend_from_slice(&self.dir_flags.to_be_bytes());
        out.extend_from_slice(info_exclude_id.as_slice());
        out.extend_from_slice(excludes_file_id.as_slice());
        out.extend_from_slice(&self.exclude_filename_per_dir);
        out.push(0);

        // `if (!untracked->root) { ... encode_varint(0) ...; return; }` — no trailing NUL: the
        // zero is the byte `read_untracked_extension()` strips as its safeguard.
        if self.directories.is_empty() {
            out.extend_from_slice(&encode_varint(0));
            return;
        }

        let mut wd = WriteData::default();
        self.write_one_dir(0, &mut wd);
        out.extend_from_slice(&encode_varint(wd.index as u64));
        out.extend_from_slice(&wd.out);
        wd.valid.write_to(out);
        wd.check_only.write_to(out);
        wd.sha1_valid.write_to(out);
        out.extend_from_slice(&wd.sb_stat);
        out.extend_from_slice(&wd.sb_sha1);
        // "safe guard for string lists"
        out.push(0);
    }

    /// `write_one_dir()` (dir.c:3616-3675).
    ///
    /// An invalid directory is written with no untracked names and without `check_only` — git
    /// clears both on the spot "for safety" — and only the directories whose `recurse` bit is
    /// set are counted and descended into. Every directory this crate holds was either decoded,
    /// which sets `recurse` (dir.c:3780), or is the root, so all of them qualify.
    fn write_one_dir(&self, dir: usize, wd: &mut WriteData) {
        let untracked = &self.directories[dir];
        let i = wd.index;
        wd.index += 1;
        let valid = untracked.stat.is_some();

        if valid && untracked.check_only {
            wd.check_only.set(i);
        }
        if let Some(stat) = &untracked.stat {
            wd.valid.set(i);
            stat_data_to_disk(stat, &mut wd.sb_stat);
        }
        if let Some(oid) = untracked.exclude_file_oid.filter(|oid| !oid.is_null()) {
            wd.sha1_valid.set(i);
            wd.sb_sha1.extend_from_slice(oid.as_slice());
        }

        let names: &[BString] = if valid { &untracked.untracked_entries } else { &[] };
        wd.out.extend_from_slice(&encode_varint(names.len() as u64));
        wd.out
            .extend_from_slice(&encode_varint(untracked.sub_directories.len() as u64));
        wd.out.extend_from_slice(&untracked.name);
        wd.out.push(0);
        for name in names {
            wd.out.extend_from_slice(name);
            wd.out.push(0);
        }
        for &sub in &untracked.sub_directories {
            self.write_one_dir(sub, wd);
        }
    }
}

/// git's `struct write_data` (dir.c:3590-3598).
#[derive(Default)]
struct WriteData {
    index: usize,
    check_only: gix_bitmap::ewah::write::Builder,
    valid: gix_bitmap::ewah::write::Builder,
    sha1_valid: gix_bitmap::ewah::write::Builder,
    out: Vec<u8>,
    sb_stat: Vec<u8>,
    sb_sha1: Vec<u8>,
}

/// `stat_data_to_disk()` (dir.c:3600-3611): the 36-byte `struct stat_data`, which unlike an
/// index entry's stat carries no mode.
fn stat_data_to_disk(stat: &entry::Stat, out: &mut Vec<u8>) {
    for field in [
        stat.ctime.secs,
        stat.ctime.nsecs,
        stat.mtime.secs,
        stat.mtime.nsecs,
        stat.dev,
        stat.ino,
        stat.uid,
        stat.gid,
        stat.size,
    ] {
        out.extend_from_slice(&field.to_be_bytes());
    }
}

/// Decode an untracked cache extension from `data`, assuming object hashes are of type `object_hash`.
pub fn decode(data: &[u8], object_hash: gix_hash::Kind, alloc_limit_bytes: Option<usize>) -> Option<UntrackedCache> {
    if data.last().is_none_or(|b| *b != 0) {
        return None;
    }
    let (identifier_len, data) = var_int(data)?;
    let (identifier, data) = data.split_at_checked(identifier_len.try_into().ok()?)?;

    // The on-disk layout matches git's `ondisk_untracked_cache` struct
    // https://github.com/git/git/blob/2855562ca6a9c6b0e7bc780b050c1e83c9fcfbd0/dir.c#L3582-L3586
    // https://github.com/git/git/blob/2855562ca6a9c6b0e7bc780b050c1e83c9fcfbd0/dir.c#L3668-L3722
    //   info_exclude_stat  (36 bytes)
    //   excludes_file_stat (36 bytes)
    //   dir_flags          ( 4 bytes)
    //   info_exclude hash  (hash_len bytes)
    //   excludes_file hash (hash_len bytes)
    //   exclude_per_dir    (NUL-terminated)
    let hash_len = object_hash.len_in_bytes();
    let (info_exclude_stat, data) = crate::decode::stat(data)?;
    let (excludes_file_stat, data) = crate::decode::stat(data)?;
    let (dir_flags, data) = read_u32(data)?;
    let (info_exclude_hash, data) = data.split_at_checked(hash_len)?;
    let (excludes_file_hash, data) = data.split_at_checked(hash_len)?;
    let info_exclude = OidStat {
        stat: info_exclude_stat,
        id: ObjectId::from_bytes_or_panic(info_exclude_hash),
    };
    let excludes_file = OidStat {
        stat: excludes_file_stat,
        id: ObjectId::from_bytes_or_panic(excludes_file_hash),
    };
    let (exclude_filename_per_dir, data) = split_at_byte_exclusive(data, 0)?;

    let (num_directory_blocks, data) = var_int(data)?;

    let mut res = UntrackedCache {
        identifier: identifier.into(),
        info_exclude: (!info_exclude.id.is_null()).then_some(info_exclude),
        excludes_file: (!excludes_file.id.is_null()).then_some(excludes_file),
        exclude_filename_per_dir: exclude_filename_per_dir.into(),
        dir_flags,
        directories: Vec::new(),
    };
    if num_directory_blocks == 0 {
        return data.is_empty().then_some(res);
    }

    let num_directory_blocks: usize = num_directory_blocks.try_into().ok()?;
    if num_directory_blocks > data.len() {
        return None;
    }
    if alloc_limit_bytes
        .is_some_and(|limit| num_directory_blocks.saturating_mul(std::mem::size_of::<Directory>()) > limit)
    {
        return None;
    }
    let directories = &mut res.directories;
    directories.try_reserve(num_directory_blocks).ok()?;

    let data = decode_directory_block(data, directories, alloc_limit_bytes)?;
    if directories.len() != num_directory_blocks {
        return None;
    }
    let (valid, data) = gix_bitmap::ewah::decode(data).ok()?;
    let (check_only, data) = gix_bitmap::ewah::decode(data).ok()?;
    let (hash_valid, mut data) = gix_bitmap::ewah::decode(data).ok()?;

    if valid.num_bits() > num_directory_blocks
        || check_only.num_bits() > num_directory_blocks
        || hash_valid.num_bits() > num_directory_blocks
    {
        return None;
    }

    check_only.for_each_set_bit(|index| {
        let directory = directories.get_mut(index)?;
        directory.check_only = true;
        Some(())
    })?;
    valid.for_each_set_bit(|index| {
        let directory = directories.get_mut(index)?;
        let (stat, rest) = crate::decode::stat(data)?;
        directory.stat = stat.into();
        data = rest;
        Some(())
    })?;
    hash_valid.for_each_set_bit(|index| {
        let directory = directories.get_mut(index)?;
        let (hash, rest) = data.split_at_checked(hash_len)?;
        data = rest;
        directory.exclude_file_oid = ObjectId::from_bytes_or_panic(hash).into();
        Some(())
    })?;

    // null-byte checked in the beginning
    if data.len() != 1 {
        return None;
    }
    res.into()
}

fn decode_directory_block<'a>(
    data: &'a [u8],
    directories: &mut Vec<Directory>,
    alloc_limit_bytes: Option<usize>,
) -> Option<&'a [u8]> {
    let (num_untracked, data) = var_int(data)?;
    let (num_dirs, data) = var_int(data)?;
    let (name, mut data) = split_at_byte_exclusive(data, 0)?;
    // Untracked names are encoded as `name\0name\0...`, and we assume names are non-empty:
    // `a\0b\0` is 4 bytes for 2 entries, so each entry needs at least 2 bytes.
    let max_entries_from_remaining_data = data.len() / 2;
    let num_untracked: usize = num_untracked.try_into().ok()?;
    let num_dirs: usize = num_dirs.try_into().ok()?;
    if num_untracked > max_entries_from_remaining_data || num_dirs > max_entries_from_remaining_data {
        return None;
    }
    if alloc_limit_bytes.is_some_and(|limit| {
        num_untracked.saturating_mul(std::mem::size_of::<BString>()) > limit
            || num_dirs.saturating_mul(std::mem::size_of::<usize>()) > limit
    }) {
        return None;
    }
    let mut untracked_entries = Vec::<BString>::new();
    untracked_entries.try_reserve(num_untracked).ok()?;
    for _ in 0..num_untracked {
        let (name, rest) = split_at_byte_exclusive(data, 0)?;
        data = rest;
        untracked_entries.push(name.into());
    }

    let index = directories.len();
    directories.push(Directory {
        name: name.into(),
        untracked_entries,
        sub_directories: {
            let mut sub_directories = Vec::new();
            sub_directories.try_reserve(num_dirs).ok()?;
            sub_directories
        },
        // the following are set later through their bitmaps
        stat: None,
        exclude_file_oid: None,
        check_only: false,
    });

    for _ in 0..num_dirs {
        let subdir_index = directories.len();
        let rest = decode_directory_block(data, directories, alloc_limit_bytes)?;
        data = rest;
        directories[index].sub_directories.push(subdir_index);
    }

    data.into()
}

/// The `(path, stage)` pairs an index held at one moment, kept beside its untracked cache.
///
/// git invalidates the untracked cache inside the two primitives every index mutation goes
/// through: `add_index_entry_with_check()` for a name it did not hold yet (read-cache.c:1270-1271)
/// and `remove_index_entry_at()`'s callers `remove_file_from_index()`,
/// `remove_marked_cache_entries()` and `rename_index_entry_at()` (read-cache.c:170, :614, :635).
/// This crate's entry list has no such chokepoint — callers push, retain and swap entries
/// directly — so the same set is recovered at the one point all of them meet, the write: a pair
/// present on exactly one side of this snapshot and the entries being written is a name git
/// added or removed. A replaced entry (same name, same stage) is on both sides and is left
/// alone, exactly as git's `replace_index_entry()` path leaves it (read-cache.c:1263-1267).
///
/// The one sequence the comparison cannot see is a pair removed and added back within the same
/// command; git invalidates its directory twice, this invalidates it not at all. The untracked
/// names in that directory are unchanged by it, so what stock reads back is still correct.
#[derive(Clone, Debug)]
pub(crate) struct IndexNames {
    backing: Vec<u8>,
    names: Vec<(std::ops::Range<usize>, u32)>,
}

impl IndexNames {
    /// The pairs `state`'s entries hold now. Entries flagged `CE_REMOVE` are not written, so
    /// they are not held.
    pub(crate) fn of(state: &crate::State) -> Self {
        let mut backing = Vec::new();
        let names = state
            .entries()
            .iter()
            .filter(|e| !e.flags.contains(entry::Flags::REMOVE))
            .map(|e| {
                let path = e.path(state);
                let start = backing.len();
                backing.extend_from_slice(path);
                (start..backing.len(), e.stage_raw())
            })
            .collect();
        IndexNames { backing, names }
    }

    /// Every path in exactly one of `self` and `other`.
    pub(crate) fn changed_paths<'a>(&'a self, other: &'a IndexNames) -> Vec<&'a bstr::BStr> {
        use std::collections::HashSet;
        let pairs = |names: &'a IndexNames| -> HashSet<(&'a [u8], u32)> {
            names
                .names
                .iter()
                .map(|(range, stage)| (&names.backing[range.clone()], *stage))
                .collect()
        };
        let (before, after) = (pairs(self), pairs(other));
        before
            .symmetric_difference(&after)
            .map(|(path, _)| bstr::BStr::new(*path))
            .collect()
    }
}
