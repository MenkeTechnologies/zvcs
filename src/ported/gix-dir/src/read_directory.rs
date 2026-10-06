//! git's own directory walk: `fill_directory()` / `read_directory()` and the
//! functions they drive — `treat_leading_path()`, `read_directory_recursive()`,
//! `treat_path()`, `treat_directory()` and `add_path_to_appropriate_result_list()`
//! (dir.c:272-294, :1840-2222, :2296-2522, :2645-2860, :3135-3180).
//!
//! [`walk()`](crate::walk()) classifies each path and decides afterwards what to
//! report. git's walk instead fills two lists — `dir->entries` and `dir->ignored`
//! — as it goes, and what lands in them is decided by the `DIR_*` flags the caller
//! sets: whether an untracked directory is listed or descended into, whether its
//! ignored contents are kept, whether an empty one counts, whether a nested
//! repository is a candidate. `ls-files -o [-i] [--directory]`, `status
//! [--ignored] [-u…]` and `clean [-d] [-x|-X]` each set a different combination,
//! and their output *is* those lists. This module reproduces them.
//!
//! With an untracked cache (`dir->untracked`) the walk is the cached one git runs for
//! `status`: a directory whose stat, `check_only` mode and ignore files are what the
//! cache recorded is listed from the cache (`valid_cached_dir()`, `read_cached_dir()`,
//! `treat_path_fast()`), any other is read from disk and its listing recorded
//! (`open_cached_dir()`, `add_untracked()`, `close_cached_dir()`), and the ignore files
//! are consulted in `prep_exclude()`'s order so a changed one invalidates exactly the
//! directories git invalidates. Deciding whether a cache may be used at all
//! (`validate_untracked_cache()`) is the caller's, since it reads configuration and the
//! global ignore files; git bypasses the cache whenever ignored paths are collected or a
//! pathspec is given (dir.c:2977-3010).
use std::path::{Path, PathBuf};

use bstr::{BStr, BString, ByteSlice};

/// `DIR_SHOW_IGNORED` (dir.h:225): list excluded paths *instead of* untracked ones, in `entries`.
pub const DIR_SHOW_IGNORED: u32 = 1 << 0;
/// `DIR_SHOW_OTHER_DIRECTORIES` (dir.h:228): list an untracked directory rather than its contents.
pub const DIR_SHOW_OTHER_DIRECTORIES: u32 = 1 << 1;
/// `DIR_HIDE_EMPTY_DIRECTORIES` (dir.h:231): a directory with nothing to report is not listed.
pub const DIR_HIDE_EMPTY_DIRECTORIES: u32 = 1 << 2;
/// `DIR_NO_GITLINKS` (dir.h:237): recurse into nested repositories as into plain directories.
pub const DIR_NO_GITLINKS: u32 = 1 << 3;
/// `DIR_COLLECT_IGNORED` (dir.h:245): collect excluded paths a pathspec names into `ignored`.
pub const DIR_COLLECT_IGNORED: u32 = 1 << 4;
/// `DIR_SHOW_IGNORED_TOO` (dir.h:252): list untracked paths in `entries` and excluded ones in `ignored`.
pub const DIR_SHOW_IGNORED_TOO: u32 = 1 << 5;
/// `DIR_COLLECT_KILLED_ONLY` (dir.h:254): skip directories the index has nothing at or below.
pub const DIR_COLLECT_KILLED_ONLY: u32 = 1 << 6;
/// `DIR_KEEP_UNTRACKED_CONTENTS` (dir.h:261): with `DIR_SHOW_IGNORED_TOO`, keep what an untracked directory held.
pub const DIR_KEEP_UNTRACKED_CONTENTS: u32 = 1 << 7;
/// `DIR_SHOW_IGNORED_TOO_MODE_MATCHING` (dir.h:277): an ignored directory is listed only if a pattern names it.
pub const DIR_SHOW_IGNORED_TOO_MODE_MATCHING: u32 = 1 << 8;
/// `DIR_SKIP_NESTED_GIT` (dir.h:279): a nested repository is no candidate at all.
pub const DIR_SKIP_NESTED_GIT: u32 = 1 << 9;

/// `enum path_treatment` (dir.c:52-57). The order matters: a directory's state
/// is the greatest of its entries'.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PathTreatment {
    None,
    Recurse,
    Excluded,
    Untracked,
}

/// `enum exist_status` (dir.c:1872-1876).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistStatus {
    Nonexistent,
    Directory,
    Gitdir,
}

/// The `d_type` values `treat_path()` distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DType {
    Unknown,
    Reg,
    Dir,
    Lnk,
    Other,
}

impl From<std::fs::FileType> for DType {
    fn from(t: std::fs::FileType) -> Self {
        if t.is_file() {
            DType::Reg
        } else if t.is_dir() {
            DType::Dir
        } else if t.is_symlink() {
            DType::Lnk
        } else {
            DType::Other
        }
    }
}

/// Signature of the attribute lookup `:(attr:…)` pathspec items need, as in
/// [`gix_pathspec::Search::pattern_matching_relative_path()`].
pub type PathspecAttributes<'a> = dyn FnMut(
        &BStr,
        gix_pathspec::attributes::glob::pattern::Case,
        bool,
        &mut gix_pathspec::attributes::search::Outcome,
    ) -> bool
    + 'a;

/// Everything `read_directory()` consults besides the disk.
pub struct Context<'a> {
    /// `istate`.
    pub index: &'a gix_index::State,
    /// `ignore_case` (`core.ignoreCase`): when `Some`, index lookups fold ASCII
    /// case through this accelerator, as git's name hash does.
    pub ignore_case: Option<&'a gix_index::AccelerateLookup<'a>>,
    /// The pathspec. An empty search matches everything.
    pub pathspec: &'a mut gix_pathspec::Search,
    /// Attribute lookups for `:(attr:…)` items.
    pub pathspec_attributes: &'a mut PathspecAttributes<'a>,
    /// `is_excluded(dir, istate, path, &dtype)` (dir.c:1765-1778) against the
    /// exclude lists the caller set up — a path under an excluded directory
    /// is excluded too (`prep_exclude()`'s `dir->internal.pattern`). The second
    /// argument says whether the path is a directory.
    pub is_excluded: &'a mut dyn FnMut(&BStr, bool) -> bool,
    /// `real_pathdup(the_repository->gitdir)`, to tell our own `.git` from a
    /// nested repository's when the git directory sits inside the worktree.
    pub git_dir_realpath: &'a Path,
    /// `core.precomposeUnicode`: directory entries are read back precomposed.
    pub precompose_unicode: bool,
}

/// What a walk over an untracked cache needs besides the cache.
pub struct UntrackedCacheContext<'a> {
    /// The cache, read and updated in place.
    pub cache: &'a mut gix_index::extension::UntrackedCache,
    /// `add_patterns()`'s id for the per-directory ignore file at the given
    /// worktree-relative path (dir.c:1150-1251), null when there is none.
    pub exclude_oid: &'a mut dyn FnMut(&BStr) -> gix_index::hash::ObjectId,
    /// `match_stat_data_racy(istate, stored, current) != 0`: whether a directory's stat
    /// moved, or is too close to the index's own timestamp to say.
    pub stat_changed: &'a dyn Fn(&gix_index::entry::Stat, &gix_index::entry::Stat) -> bool,
}

/// `dir->entries` and `dir->ignored` after `read_directory()` sorted them
/// (`cmp_dir_entry()`, dir.c:2799-2805). A directory's name ends in `/`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// `dir->entries`.
    pub entries: Vec<BString>,
    /// `dir->ignored`.
    pub ignored: Vec<BString>,
}

/// `fill_directory()` (dir.c:272-294): walk from the directory every pathspec
/// item shares, with `flags` a combination of the `DIR_*` constants. `untracked` is
/// `dir->untracked` once the caller ran `validate_untracked_cache()`; it is used only
/// for a walk from the top of the worktree, as git uses it.
///
/// # Panics
///
/// If both `DIR_SHOW_IGNORED` and `DIR_SHOW_IGNORED_TOO` are set, as git's `BUG()` does.
pub fn fill_directory(
    worktree_root: &Path,
    flags: u32,
    ctx: &mut Context<'_>,
    untracked: Option<UntrackedCacheContext<'_>>,
) -> Outcome {
    let exclusive = DIR_SHOW_IGNORED | DIR_SHOW_IGNORED_TOO;
    assert!(
        flags & exclusive != exclusive,
        "BUG: DIR_SHOW_IGNORED and DIR_SHOW_IGNORED_TOO are exclusive"
    );
    let prefix = ctx.pathspec.git_common_prefix();
    read_directory(worktree_root, flags, ctx, prefix.as_bstr(), untracked)
}

/// `read_directory()` (dir.c:3140-3180) from `path`, a worktree-relative
/// directory that is empty or ends in `/`, listing from and filling `untracked` when
/// the walk starts at the top.
pub fn read_directory(
    worktree_root: &Path,
    flags: u32,
    ctx: &mut Context<'_>,
    path: &BStr,
    untracked: Option<UntrackedCacheContext<'_>>,
) -> Outcome {
    // "Optimize for the main use case only: whole-tree git status" (dir.c:3001-3008).
    let untracked = untracked.filter(|_| path.is_empty());
    let mut dir = Dir {
        untracked,
        root: worktree_root,
        flags,
        ctx,
        out: Outcome::default(),
        exclude_stack: Vec::new(),
        exclude_basebuf: BString::default(),
        exclude_pattern: false,
    };
    if dir.has_symlink_leading_path(path) {
        return dir.out;
    }
    if path.is_empty() || dir.treat_leading_path(path) {
        // `validate_untracked_cache()` gave the root its `recurse` bit (dir.c:3104-3105).
        let untracked = dir.untracked.as_mut().and_then(|c| {
            let root = c.cache.root()?;
            c.cache.directory_mut(root).recurse = true;
            Some(root)
        });
        dir.read_directory_recursive(path, untracked, false, false);
    }
    dir.out.entries.sort();
    dir.out.ignored.sort();
    dir.out
}

struct Dir<'r, 'c, 'a, 'u> {
    /// `dir->untracked`: the untracked cache to list directories from and fill, once
    /// `validate_untracked_cache()` accepted it for this walk.
    untracked: Option<UntrackedCacheContext<'u>>,
    root: &'r Path,
    flags: u32,
    ctx: &'c mut Context<'a>,
    out: Outcome,
    /// `dir->internal.exclude_stack` as far as the untracked cache needs it: the length of
    /// each directory prefix whose ignore file was consulted, and its cache directory.
    exclude_stack: Vec<(usize, Option<usize>)>,
    /// `dir->internal.basebuf`.
    exclude_basebuf: BString,
    /// `dir->internal.pattern != NULL`: the directory on top of the stack is excluded.
    exclude_pattern: bool,
}

impl Dir<'_, '_, '_, '_> {
    fn has(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    fn disk_path(&self, rela: &BStr) -> PathBuf {
        let rela = rela.strip_suffix(b"/").unwrap_or(rela);
        self.root.join(gix_path::from_bstr(rela.as_bstr()))
    }

    fn match_pathspec(&mut self, name: &BStr, flags: u32) -> u8 {
        self.ctx
            .pathspec
            .match_pathspec_with_flags(name, flags, &mut *self.ctx.pathspec_attributes)
    }

    /// `has_symlink_leading_path()` (symlinks.c:215): whether a leading
    /// directory of `path` is a symbolic link.
    fn has_symlink_leading_path(&self, path: &BStr) -> bool {
        for (at, _) in path.iter().enumerate().filter(|(_, b)| **b == b'/') {
            match self.disk_path(path[..at].as_bstr()).symlink_metadata() {
                Ok(meta) if meta.file_type().is_symlink() => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }

    /// `index_file_exists(istate, name, len, ignore_case)` (name-hash.c): an
    /// entry of exactly this name at any stage.
    fn index_file_exists(&self, name: &BStr, ignore_case: bool) -> Option<&gix_index::Entry> {
        let index = self.ctx.index;
        match self.ctx.ignore_case.filter(|_| ignore_case) {
            Some(lookup) => index.entry_by_path_icase(name, true, lookup),
            None => index.entry_index_by_path(name).ok().map(|idx| &index.entries()[idx]),
        }
    }

    /// The first index position whose name does not sort before `name`, git's
    /// `-pos-1` from a missed `index_name_pos()`.
    fn index_name_pos(&self, name: &BStr) -> usize {
        let index = self.ctx.index;
        index.entries().partition_point(|e| e.path(index) < name)
    }

    /// `index_name_is_other()` (read-cache.c:3442-3459).
    fn index_name_is_other(&self, name: &BStr) -> bool {
        let name = name.strip_suffix(b"/").unwrap_or(name).as_bstr();
        self.ctx.index.entry_index_by_path(name).is_err()
    }

    /// `directory_exists_in_index()` (dir.c:1905-1930) and its `_icase` form
    /// (dir.c:1883-1897).
    fn directory_exists_in_index(&self, dirname: &BStr) -> ExistStatus {
        let index = self.ctx.index;
        if let Some(lookup) = self.ctx.ignore_case {
            if let Some(ce) = index.entry_by_path_icase(dirname, true, lookup) {
                if ce.mode.is_submodule() {
                    return ExistStatus::Gitdir;
                }
            }
            return if index
                .entry_closest_to_directory_or_directory_icase(dirname, true, lookup)
                .is_some_and(|ce| !ce.mode.is_submodule() || ce.path(index).len() > dirname.len())
            {
                ExistStatus::Directory
            } else {
                ExistStatus::Nonexistent
            };
        }
        let len = dirname.len();
        for ce in &index.entries()[self.index_name_pos(dirname)..] {
            let name = ce.path(index);
            if name.get(..len) != Some(dirname.as_bytes()) {
                break;
            }
            match name.get(len) {
                Some(&b) if b > b'/' => break,
                Some(&b'/') => return ExistStatus::Directory,
                None if ce.mode.is_submodule() => return ExistStatus::Gitdir,
                _ => {}
            }
        }
        ExistStatus::Nonexistent
    }

    /// `get_index_dtype()` (dir.c:2296-2327).
    fn get_index_dtype(&self, path: &BStr) -> DType {
        let uptodate = |ce: &gix_index::Entry| ce.flags.contains(gix_index::entry::Flags::UPTODATE);
        if let Some(ce) = self.index_file_exists(path, false) {
            if !uptodate(ce) {
                return DType::Unknown;
            }
            return if ce.mode.is_submodule() { DType::Dir } else { DType::Reg };
        }
        let index = self.ctx.index;
        let len = path.len();
        for ce in &index.entries()[self.index_name_pos(path)..] {
            let name = ce.path(index);
            if name.get(..len) != Some(path.as_bytes()) {
                break;
            }
            match name.get(len) {
                Some(&b) if b > b'/' => break,
                Some(&b) if b < b'/' => continue,
                _ => {}
            }
            if !uptodate(ce) {
                break;
            }
            return DType::Dir;
        }
        DType::Unknown
    }

    /// `resolve_dtype()` (dir.c:2368-2387).
    fn resolve_dtype(&self, dtype: DType, path: &BStr) -> DType {
        if dtype != DType::Unknown {
            return dtype;
        }
        let dtype = self.get_index_dtype(path);
        if dtype != DType::Unknown {
            return dtype;
        }
        match self.disk_path(path).symlink_metadata() {
            Ok(meta) => match DType::from(meta.file_type()) {
                DType::Other => DType::Unknown,
                known => known,
            },
            Err(_) => DType::Unknown,
        }
    }

    /// `is_nonbare_repository_dir()` (setup.c:455-470) for `dirname` (with its
    /// trailing `/`), followed by `treat_directory()`'s check that the `.git` found
    /// there is not our own (dir.c:2016-2033).
    fn is_nested_repo(&self, dirname: &BStr) -> bool {
        let dot_git = self.disk_path(dirname).join(gix_discover::DOT_GIT_DIR);
        let nonbare = match read_gitfile_gently(&dot_git) {
            Ok(()) => true,
            Err(GitfileError::OpenFailed | GitfileError::ReadFailed) => true,
            Err(_) => is_git_directory(&dot_git),
        };
        nonbare
            && gix_path::realpath(&dot_git)
                .map_or(true, |real| real != self.ctx.git_dir_realpath)
    }

    /// `treat_leading_path()` (dir.c:2811-2877).
    fn treat_leading_path(&mut self, path: &BStr) -> bool {
        let mut len = path.len();
        while len > 0 && path[len - 1] == b'/' {
            len -= 1;
        }
        if len == 0 {
            return true;
        }
        let path = &path[..len];
        let mut state = PathTreatment::None;
        let mut sb = BString::default();
        let mut baselen = 0;
        loop {
            let prevlen = baselen + usize::from(baselen != 0);
            baselen = path[prevlen..].find_byte(b'/').map_or(len, |at| prevlen + at);
            sb.clear();
            sb.extend_from_slice(&path[..baselen]);
            if !self.disk_path(sb.as_bstr()).is_dir() {
                break;
            }
            sb.truncate(prevlen);
            let name = BString::from(&path[prevlen..baselen]);
            state = self.treat_path(&mut sb, prevlen, name.as_bstr(), DType::Dir, None);
            if state != PathTreatment::Recurse || len <= baselen {
                break;
            }
        }
        self.add_path_to_appropriate_result_list(sb.as_bstr(), 0, None, false, state);
        state == PathTreatment::Recurse
    }

    /// `read_directory_recursive()` (dir.c:2703-2804). `untracked` is the cache's
    /// directory for `base` when the untracked cache is in use.
    fn read_directory_recursive(
        &mut self,
        base: &BStr,
        untracked: Option<usize>,
        check_only: bool,
        stop_at_first_file: bool,
    ) -> PathTreatment {
        let mut dir_state = PathTreatment::None;
        let mut path = BString::from(base);
        let baselen = base.len();

        // `open_cached_dir()` (dir.c:2576-2600): a directory the cache vouches for is
        // listed from it; any other is read from disk, and one that cannot be opened is
        // warned about and contributes nothing.
        let (cached, stat_data) = self.valid_cached_dir(untracked, base, check_only);
        let mut disk = None;
        if !cached {
            let at = if base.is_empty() {
                self.root.to_owned()
            } else {
                self.disk_path(base)
            };
            let opened = gix_fs::read_dir(&at, self.ctx.precompose_unicode);
            if let (Some(cache), Some(u)) = (self.untracked.as_mut(), untracked) {
                cache.cache.invalidate_directory(u);
                cache.cache.stats_mut().dir_opened += 1;
            }
            match opened {
                Ok(entries) => disk = Some(entries),
                Err(err) => {
                    let shown = if base.is_empty() { ".".into() } else { base.to_str_lossy() };
                    eprintln!(
                        "warning: could not open directory '{shown}': {}",
                        crate::walk::readdir::errno_text(&err)
                    );
                    return dir_state;
                }
            }
        }
        if let (Some(cache), Some(u)) = (self.untracked.as_mut(), untracked) {
            cache.cache.directory_mut(u).check_only = check_only;
        }

        // `read_cached_dir()`'s cursor over the cache: sub-directories first, then names.
        let (mut nr_dirs, mut nr_files) = (0, 0);
        loop {
            // `read_cached_dir()` (dir.c:2602-2634) feeding `treat_path()`.
            let mut state = if let Some(entries) = disk.as_mut() {
                let Some(Ok(entry)) = entries.next() else { break };
                let name: BString = gix_path::try_os_str_into_bstr(entry.file_name())
                    .map(|n| n.into_owned())
                    .unwrap_or_default();
                let dtype = entry.file_type().map_or(DType::Unknown, DType::from);
                self.treat_path(&mut path, baselen, name.as_bstr(), dtype, untracked)
            } else {
                let cache = &self.untracked.as_ref().expect("a cached listing has a cache").cache;
                let dir = &cache.directories()[untracked.expect("a cached listing has a directory")];
                let mut ucd = None;
                while nr_dirs < dir.sub_directories.len() {
                    let sub = dir.sub_directories[nr_dirs];
                    nr_dirs += 1;
                    if cache.directories()[sub].recurse {
                        ucd = Some(sub);
                        break;
                    }
                }
                match ucd {
                    Some(ucd) => self.treat_path_fast(&mut path, baselen, ucd),
                    None if nr_files < dir.untracked_entries.len() => {
                        path.truncate(baselen);
                        path.extend_from_slice(&dir.untracked_entries[nr_files]);
                        nr_files += 1;
                        PathTreatment::Untracked
                    }
                    None => break,
                }
            };
            dir_state = dir_state.max(state);

            if state == PathTreatment::Recurse {
                let ud = self.lookup_untracked(untracked, &path[baselen..]);
                let sub = path.clone();
                let subdir_state = self.read_directory_recursive(sub.as_bstr(), ud, check_only, stop_at_first_file);
                dir_state = dir_state.max(subdir_state);
                if self.match_pathspec(path.as_bstr(), 0) == 0 {
                    state = PathTreatment::None;
                }
            }

            if check_only {
                if stop_at_first_file && dir_state >= PathTreatment::Excluded {
                    dir_state = PathTreatment::Excluded;
                    break;
                }
                if dir_state == PathTreatment::Untracked {
                    if disk.is_some() {
                        self.add_untracked(untracked, &path[baselen..]);
                    }
                    break;
                }
                continue;
            }
            self.add_path_to_appropriate_result_list(path.as_bstr(), baselen, untracked, disk.is_some(), state);
        }

        // `close_cached_dir()` (dir.c:2636-2649): the directory was gone through, so what
        // the cache now holds for it is complete.
        if let (Some(cache), Some(u)) = (self.untracked.as_mut(), untracked) {
            let dir = cache.cache.directory_mut(u);
            dir.stat = Some(stat_data);
            dir.recurse = true;
        }
        dir_state
    }

    /// `valid_cached_dir()` (dir.c:2528-2574): whether the cache's listing of `base` can be
    /// used instead of reading it, together with the `stat_data` the directory keeps —
    /// refreshed whenever the stored one no longer matches.
    fn valid_cached_dir(&mut self, untracked: Option<usize>, base: &BStr, check_only: bool) -> (bool, gix_index::entry::Stat) {
        let (Some(cache), Some(u)) = (self.untracked.as_ref(), untracked) else {
            return (false, Default::default());
        };
        let dir = &cache.cache.directories()[u];
        let valid = dir.stat.is_some();
        let mut stat_data = dir.stat.unwrap_or_default();
        // "With fsmonitor, we can trust the untracked cache's valid field."
        if !(cache.cache.use_fsmonitor() && valid) {
            let at = if base.is_empty() {
                self.root.to_owned()
            } else {
                self.disk_path(base)
            };
            let Some(st) = gix_index::fs::Metadata::from_path_no_follow(&at)
                .ok()
                .and_then(|meta| gix_index::entry::Stat::from_fs(&meta).ok())
            else {
                return (false, Default::default());
            };
            if !valid || (cache.stat_changed)(&stat_data, &st) {
                return (false, st);
            }
            stat_data = dir.stat.unwrap_or_default();
        }
        if dir.check_only != check_only {
            return (false, stat_data);
        }
        // "prep_exclude will be called eventually on this directory, but it's called much
        // later in last_matching_pattern(). We need it now to determine the validity of the
        // cache for this path."
        self.prep_exclude(base);
        let valid = self.untracked.as_ref().is_some_and(|c| c.cache.directories()[u].stat.is_some());
        (valid, stat_data)
    }

    /// `treat_path_fast()` (dir.c:2394-2429): a sub-directory the cache listed. It is
    /// walked again only if it was last read to see whether it held anything; otherwise
    /// the recursion picks it up.
    fn treat_path_fast(&mut self, path: &mut BString, baselen: usize, ucd: usize) -> PathTreatment {
        let cache = &self.untracked.as_ref().expect("cached").cache;
        let dir = &cache.directories()[ucd];
        path.truncate(baselen);
        path.extend_from_slice(&dir.name);
        // `strbuf_complete(path, '/')`
        if path.last() != Some(&b'/') {
            path.push(b'/');
        }
        if dir.check_only {
            let sub = path.clone();
            return self.read_directory_recursive(sub.as_bstr(), Some(ucd), true, false);
        }
        PathTreatment::Recurse
    }

    /// `lookup_untracked()` (dir.c:1059-1095) for a walk: no directory without a cache.
    fn lookup_untracked(&mut self, dir: Option<usize>, name: &[u8]) -> Option<usize> {
        let cache = self.untracked.as_mut()?;
        Some(cache.cache.lookup_or_create(dir?, name))
    }

    /// `add_untracked()` (dir.c:2519-2526).
    fn add_untracked(&mut self, dir: Option<usize>, name: &[u8]) {
        if let (Some(cache), Some(dir)) = (self.untracked.as_mut(), dir) {
            cache.cache.directory_mut(dir).untracked_entries.push(name.into());
        }
    }

    /// The untracked-cache half of `prep_exclude()` (dir.c:1654-1803) for `base`, a
    /// directory that is empty or ends in `/`: the per-directory ignore file of every
    /// directory from the root down to `base` is looked at once per descent, and a
    /// directory whose ignore file is not the one its listing was made under loses that
    /// listing, as does everything below it.
    ///
    /// The patterns themselves are matched by [`Context::is_excluded`]; only the stack
    /// that decides *when* each ignore file is consulted is kept here, since that is
    /// what decides which directories are invalidated. Without a cache there is nothing
    /// to do.
    fn prep_exclude(&mut self, base: &BStr) {
        if self.untracked.is_none() {
            return;
        }
        let baselen = base.len();
        // Pop the directories that are not a prefix of `base`.
        while let Some(&(len, _)) = self.exclude_stack.last() {
            if len <= baselen && self.exclude_basebuf.get(..len) == base.get(..len) {
                break;
            }
            self.exclude_stack.pop();
            self.exclude_pattern = false;
        }
        // "Skip traversing into sub directories if the parent is excluded"
        if self.exclude_pattern {
            return;
        }
        let top = self.exclude_stack.last().copied();
        let mut current: Option<usize> = top.map(|(len, _)| len);
        self.exclude_basebuf.truncate(current.unwrap_or(0));
        let mut untracked = match top {
            Some((_, ucd)) => ucd,
            None => self.untracked.as_ref().and_then(|c| c.cache.root()),
        };
        while current.is_none_or(|c| c < baselen) {
            let (start, end) = match current {
                None => (0, 0),
                Some(c) => {
                    let Some(slash) = base[c + 1..].find_byte(b'/') else {
                        panic!("oops in prep_exclude");
                    };
                    let end = c + 1 + slash + 1;
                    untracked = self.lookup_untracked(untracked, &base[c..end]);
                    (c, end)
                }
            };
            self.exclude_stack.push((end, untracked));
            self.exclude_basebuf.extend_from_slice(&base[start..end]);

            // "Abort if the directory is excluded"
            if end > 0 {
                let dirname = BString::from(&self.exclude_basebuf[..end - 1]);
                if (self.ctx.is_excluded)(dirname.as_bstr(), true) {
                    self.exclude_pattern = true;
                    return;
                }
            }

            // "Try to read per-directory file", which an untracked cache skips for a
            // directory it knows to hold no ignore file and no new names.
            if let Some(u) = untracked {
                let cache = self.untracked.as_mut().expect("checked above");
                let dir = &cache.cache.directories()[u];
                let known = dir.exclude_file_oid.filter(|oid| !oid.is_null());
                let read = dir.stat.is_none() || known.is_some();
                let mut oid = None;
                if read {
                    let mut file = BString::from(&self.exclude_basebuf[..end]);
                    file.extend_from_slice(cache.cache.exclude_filename_per_dir());
                    oid = Some((cache.exclude_oid)(file.as_bstr())).filter(|oid| !oid.is_null());
                }
                if oid != known {
                    cache.cache.invalidate_gitignore(u);
                    cache.cache.directory_mut(u).exclude_file_oid = oid;
                }
            }
            current = Some(end);
        }
        self.exclude_basebuf.truncate(baselen);
    }

    /// `treat_path()` (dir.c:2426-2521). On return `path` holds `base` +
    /// `d_name`, with a `/` appended for a directory that reached
    /// `treat_directory()`.
    fn treat_path(
        &mut self,
        path: &mut BString,
        baselen: usize,
        d_name: &BStr,
        dtype: DType,
        untracked: Option<usize>,
    ) -> PathTreatment {
        let is_dot_git = if self.ctx.ignore_case.is_some() {
            d_name.eq_ignore_ascii_case(b".git")
        } else {
            d_name == ".git"
        };
        if d_name == "." || d_name == ".." || is_dot_git {
            return PathTreatment::None;
        }
        path.truncate(baselen);
        path.extend_from_slice(d_name);
        if self.ctx.pathspec.simplify_away(path.as_bstr()) {
            return PathTreatment::None;
        }

        let dtype = self.resolve_dtype(dtype, path.as_bstr());

        // Always exclude indexed files.
        let has_path_in_index = self.index_file_exists(path.as_bstr(), true).is_some();
        if dtype != DType::Dir && has_path_in_index {
            return PathTreatment::None;
        }

        if self.has(DIR_COLLECT_KILLED_ONLY)
            && dtype == DType::Dir
            && !has_path_in_index
            && self.directory_exists_in_index(path.as_bstr()) == ExistStatus::Nonexistent
        {
            return PathTreatment::None;
        }

        // `is_excluded()` loads the ignore files down to the directory holding `path` first.
        let base_end = path.rfind_byte(b'/').map_or(0, |at| at + 1);
        self.prep_exclude(path[..base_end].as_bstr());
        let excluded = (self.ctx.is_excluded)(path.as_bstr(), dtype == DType::Dir);

        if excluded && !self.has(DIR_SHOW_IGNORED | DIR_SHOW_IGNORED_TOO) {
            return PathTreatment::Excluded;
        }

        match dtype {
            DType::Dir => {
                path.push(b'/');
                let dirname = path.clone();
                self.treat_directory(dirname.as_bstr(), baselen, excluded, untracked)
            }
            DType::Reg | DType::Lnk => {
                if self.match_pathspec(path.as_bstr(), 0) == 0 {
                    PathTreatment::None
                } else if excluded {
                    PathTreatment::Excluded
                } else {
                    PathTreatment::Untracked
                }
            }
            DType::Unknown | DType::Other => PathTreatment::None,
        }
    }

    /// `treat_directory()` (dir.c:1966-2221). `dirname` ends in `/`.
    fn treat_directory(&mut self, dirname: &BStr, baselen: usize, excluded: bool, untracked: Option<usize>) -> PathTreatment {
        use gix_pathspec::search::git_match::{DO_MATCH_LEADING_PATHSPEC, MATCHED_RECURSIVELY_LEADING_PATHSPEC};

        match self.directory_exists_in_index(dirname[..dirname.len() - 1].as_bstr()) {
            ExistStatus::Directory => return PathTreatment::Recurse,
            ExistStatus::Gitdir => return PathTreatment::None,
            ExistStatus::Nonexistent => {}
        }

        // An excluded directory already matched the exclude patterns, so the
        // pathspec is not asked about it.
        let mut matches_how = 0;
        if !excluded {
            matches_how = self.match_pathspec(dirname, DO_MATCH_LEADING_PATHSPEC);
            if matches_how == 0 {
                return PathTreatment::None;
            }
        }

        if (self.has(DIR_SKIP_NESTED_GIT) || !self.has(DIR_NO_GITLINKS)) && self.is_nested_repo(dirname) {
            if self.has(DIR_SKIP_NESTED_GIT) || matches_how == MATCHED_RECURSIVELY_LEADING_PATHSPEC {
                return PathTreatment::None;
            }
            return if excluded {
                PathTreatment::Excluded
            } else {
                PathTreatment::Untracked
            };
        }

        if !self.has(DIR_SHOW_OTHER_DIRECTORIES) {
            if excluded && self.has(DIR_SHOW_IGNORED_TOO) && self.has(DIR_SHOW_IGNORED_TOO_MODE_MATCHING) {
                // An excluded directory under `--ignored=matching`: listed as
                // excluded unless it is empty and empty directories are hidden.
                if !self.has(DIR_HIDE_EMPTY_DIRECTORIES) {
                    return PathTreatment::Excluded;
                }
                if self.read_directory_recursive(dirname, untracked, true, true) == PathTreatment::Excluded {
                    return PathTreatment::Excluded;
                }
                return PathTreatment::None;
            }
            return PathTreatment::Recurse;
        }

        // A pathspec that could match something *below* this directory
        // (`subdir/some/deep/path/file`, `subdir/widget-*.c`) needs the recursion.
        if matches_how == MATCHED_RECURSIVELY_LEADING_PATHSPEC {
            return PathTreatment::Recurse;
        }

        if excluded {
            if !self.has(DIR_HIDE_EMPTY_DIRECTORIES) {
                return PathTreatment::Excluded;
            }
            if self.has(DIR_SHOW_IGNORED_TOO) && self.has(DIR_SHOW_IGNORED_TOO_MODE_MATCHING) {
                return PathTreatment::Excluded;
            }
        }

        // Only these flags need to know what is under an untracked directory.
        if !excluded && !self.has(DIR_SHOW_IGNORED | DIR_SHOW_IGNORED_TOO | DIR_HIDE_EMPTY_DIRECTORIES) {
            return PathTreatment::Untracked;
        }

        // With empty directories hidden but ignored paths not wanted, it is
        // enough to learn whether there is anything below; an excluded directory
        // can stop at its first file, since that file is excluded too.
        let check_only = self.has(DIR_HIDE_EMPTY_DIRECTORIES) && !self.has(DIR_SHOW_IGNORED_TOO);
        let stop_early = check_only && excluded;

        let old_ignored_nr = self.out.ignored.len();
        let old_untracked_nr = self.out.entries.len();

        // "Actually recurse into dirname now, we'll fixup the state later."
        let untracked = self.lookup_untracked(untracked, &dirname[baselen..]);
        let mut state = self.read_directory_recursive(dirname, untracked, check_only, stop_early);

        if state == PathTreatment::Excluded {
            // Everything below was ignored. `--ignored=matching` wants those
            // paths instead of the directory; otherwise the directory stands for
            // them and they are dropped.
            if self.has(DIR_SHOW_IGNORED_TOO) && self.has(DIR_SHOW_IGNORED_TOO_MODE_MATCHING) {
                state = PathTreatment::None;
            } else {
                self.out.ignored.truncate(old_ignored_nr);
            }
        }

        // The untracked paths found below are not wanted when only the ignored
        // ones are being collected.
        if self.has(DIR_SHOW_IGNORED_TOO) && !self.has(DIR_KEEP_UNTRACKED_CONTENTS) {
            self.out.entries.truncate(old_untracked_nr);
        }

        // Nothing below and empty directories are shown: report the directory itself.
        if state == PathTreatment::None && !self.has(DIR_HIDE_EMPTY_DIRECTORIES) {
            state = if excluded {
                PathTreatment::Excluded
            } else {
                PathTreatment::Untracked
            };
        }
        state
    }

    /// `add_path_to_appropriate_result_list()` (dir.c:2645-2677) with
    /// `dir_add_name()` / `dir_add_ignored()` (dir.c:1849-1870).
    fn add_path_to_appropriate_result_list(
        &mut self,
        path: &BStr,
        baselen: usize,
        untracked: Option<usize>,
        from_disk: bool,
        state: PathTreatment,
    ) {
        match state {
            PathTreatment::Excluded => {
                if self.has(DIR_SHOW_IGNORED) {
                    self.dir_add_name(path);
                } else if self.has(DIR_SHOW_IGNORED_TOO)
                    || (self.has(DIR_COLLECT_IGNORED) && self.ctx.pathspec.exclude_matches_pathspec(path))
                {
                    if self.index_name_is_other(path) {
                        self.out.ignored.push(path.to_owned());
                    }
                }
            }
            PathTreatment::Untracked => {
                if !self.has(DIR_SHOW_IGNORED) {
                    self.dir_add_name(path);
                    if from_disk {
                        self.add_untracked(untracked, &path[baselen..]);
                    }
                }
            }
            PathTreatment::None | PathTreatment::Recurse => {}
        }
    }

    fn dir_add_name(&mut self, path: &BStr) {
        if self.index_file_exists(path, true).is_none() {
            self.out.entries.push(path.to_owned());
        }
    }
}

/// The `READ_GITFILE_ERR_*` codes `is_nonbare_repository_dir()` tells apart.
enum GitfileError {
    Other,
    OpenFailed,
    ReadFailed,
}

/// `read_gitfile_gently()` (setup.c:956-1035), as far as its verdict goes: `Ok`
/// when `path` is a `gitdir: <dir>` file naming a git directory.
fn read_gitfile_gently(path: &Path) -> Result<(), GitfileError> {
    const MAX_FILE_SIZE: u64 = 1 << 20;
    let meta = std::fs::metadata(path).map_err(|_| GitfileError::Other)?;
    if !meta.is_file() || meta.len() > MAX_FILE_SIZE {
        return Err(GitfileError::Other);
    }
    let mut file = std::fs::File::open(path).map_err(|_| GitfileError::OpenFailed)?;
    let mut buf = Vec::new();
    use std::io::Read;
    match file.read_to_end(&mut buf) {
        Ok(n) if n as u64 == meta.len() => {}
        _ => return Err(GitfileError::ReadFailed),
    }
    let Some(rest) = buf.strip_prefix(b"gitdir: ") else {
        return Err(GitfileError::Other);
    };
    let mut len = rest.len();
    while len > 0 && matches!(rest[len - 1], b'\n' | b'\r') {
        len -= 1;
    }
    if len == 0 {
        return Err(GitfileError::Other);
    }
    let target = gix_path::from_bstr(rest[..len].as_bstr()).into_owned();
    let target = match path.parent() {
        Some(parent) if target.is_relative() => parent.join(target),
        _ => target,
    };
    if is_git_directory(&target) {
        Ok(())
    } else {
        Err(GitfileError::Other)
    }
}

/// `is_git_directory()` (setup.c:415-453), answered by the repository probe the
/// rest of the walk uses.
fn is_git_directory(path: &Path) -> bool {
    path.is_dir() && gix_discover::is_git(path).is_ok()
}
