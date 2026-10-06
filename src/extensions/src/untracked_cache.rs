//! The untracked cache as `git status` uses it: `wt_status_collect_untracked()`
//! (wt-status.c:806-850) hands the index's cache to `fill_directory()`, whose
//! `read_directory()` validates it (`validate_untracked_cache()`, dir.c:2977-3110), lists
//! every directory it can vouch for from it, records what it had to read from disk, and
//! asks for the index to be written when anything changed (dir.c:3157-3175).
//!
//! The walk itself is the port in `gix_dir::read_directory`; this module supplies what it
//! cannot know on its own — the configuration and environment the validation reads, the
//! stat and id of the two global ignore files (`setup_standard_excludes()`,
//! dir.c:3487-3506), and `add_patterns()`'s id for a per-directory `.gitignore`
//! (dir.c:1150-1251), which takes the index's id for a tracked file the refresh found
//! unchanged and otherwise hashes the file *with a newline appended*, exactly as git does.

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Stat};
use gix::index::extension::untracked_cache::OidStat;

/// `DIR_SHOW_OTHER_DIRECTORIES | DIR_HIDE_EMPTY_DIRECTORIES`, the flags `status` walks with
/// unless every untracked file is to be listed (wt-status.c:816-818).
pub fn status_dir_flags(all: bool) -> u32 {
    use gix::dir::read_directory::{DIR_HIDE_EMPTY_DIRECTORIES, DIR_SHOW_OTHER_DIRECTORIES};
    if all {
        0
    } else {
        DIR_SHOW_OTHER_DIRECTORIES | DIR_HIDE_EMPTY_DIRECTORIES
    }
}

/// `wt_status_collect_untracked()`'s walk over `index`'s untracked cache with `dir_flags`, for a
/// whole-tree `status` that collects no ignored paths. Updates the cache in place and marks
/// `UNTRACKED_CHANGED` on `index` when git would have, which makes the caller write it.
///
/// Nothing happens without a cache, and the cache is left alone whenever git would bypass it.
pub fn collect(repo: &gix::Repository, index: &mut gix::index::File, dir_flags: u32) -> anyhow::Result<()> {
    if index.untracked().is_none() {
        return Ok(());
    }
    // `setup_standard_excludes()` runs before the walk, with `dir.untracked` already set, so
    // both global files are read with their `oid_stat` filled.
    let excludes_file = repo
        .excludes_file()?
        .filter(|path| is_readable(path))
        .and_then(|path| global_oid_stat(repo, &path));
    let info_exclude = Some(repo.common_dir().join("info").join("exclude"))
        .filter(|path| is_readable(path))
        .and_then(|path| global_oid_stat(repo, &path));

    if !validate(repo, index, dir_flags, info_exclude, excludes_file) {
        return Ok(());
    }

    let mut excludes = repo.excludes(
        index,
        None,
        gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
    )?;
    let no_patterns: [&str; 0] = [];
    let mut ps = repo.pathspec(
        true,
        no_patterns,
        false,
        index,
        gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
    )?;
    let stat_options = repo.stat_options()?;
    let timestamp = index.timestamp().unix_seconds();
    let racy = |sd: &Stat| timestamp != 0 && (timestamp as u32) <= sd.mtime.secs;
    let stat_changed = |stored: &Stat, current: &Stat| racy(stored) || !stored.matches(current, stat_options);
    let mut convert = Convert::new(repo, index)?;
    let workdir = repo.workdir().map(ToOwned::to_owned).unwrap_or_default();

    let stats = index.with_untracked_mut(|state, cache| {
        let mut exclude_oid = |rela: &BStr| {
            add_patterns_oid(repo, state, &workdir.join(gix::path::from_bstr(rela)), rela, &racy, stat_options, &mut convert)
        };
        let ctx = gix::dir::read_directory::UntrackedCacheContext {
            cache,
            exclude_oid: &mut exclude_oid,
            stat_changed: &stat_changed,
        };
        repo.fill_directory(
            state,
            &mut ps,
            dir_flags,
            &mut |path, is_dir| {
                let mode = is_dir.then_some(gix::index::entry::Mode::DIR);
                excludes.at_entry(path, mode).map(|p| p.is_excluded()).unwrap_or(false)
            },
            Some(ctx),
        )
        .map(|_| cache.stats().to_owned())
    });
    let Some(stats) = stats.transpose()? else {
        return Ok(());
    };

    // dir.c:3157-3171: a cache `core.untrackedCache` (or `GIT_FORCE_UNTRACKED_CACHE`) asks to
    // keep is written whenever the walk had to open, or invalidate, anything.
    let force = match std::env::var("GIT_FORCE_UNTRACKED_CACHE") {
        Ok(raw) => crate::setup::git_env_bool_value("GIT_FORCE_UNTRACKED_CACHE", &raw),
        Err(_) => repo.core_untracked_cache() == Some(true),
    };
    if force && (stats.dir_opened > 0 || stats.gitignore_invalidated > 0 || stats.dir_invalidated > 0) {
        index.mark_untracked_changed();
    }
    Ok(())
}

/// `validate_untracked_cache()` (dir.c:2977-3110) for a whole-tree walk without a pathspec,
/// without command-line excludes and with `.gitignore` as the per-directory file — the only
/// shape `status` reaches it in. Returns whether the walk may use the cache.
fn validate(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    dir_flags: u32,
    info_exclude: Option<OidStat>,
    excludes_file: Option<OidStat>,
) -> bool {
    if crate::setup::git_env_bool("GIT_DISABLE_UNTRACKED_CACHE", false) {
        return false;
    }
    let Some(cache) = index.untracked() else {
        return false;
    };
    // "If we use .gitignore in the cache and now you change it to .gitexclude, everything
    // will go wrong."
    if cache.exclude_filename_per_dir() != ".gitignore" {
        return false;
    }
    let ident = repo.untracked_cache_ident();
    if !cache.ident_matches(ident.as_ref()) {
        eprintln!("warning: untracked cache is disabled on this system or location");
        return false;
    }
    if cache.dir_flags() != dir_flags {
        // A cache built for the other listing mode is replaced when the configuration no
        // longer asks for that mode, and bypassed for this run when it still does.
        if cache.dir_flags() == repo.new_untracked_cache_flags() {
            return false;
        }
        index.set_untracked(Some(gix::index::extension::UntrackedCache::new(ident, dir_flags)));
        index.mark_untracked_changed();
    }
    let changed = index.with_untracked_mut(|_, cache| {
        // "Untracked cache existed but is not initialized; fix that"
        let created = cache.ensure_root();
        let root = cache.root().expect("just ensured");
        // "Validate $GIT_COMMON_DIR/info/exclude and core.excludesfile"
        let id = |o: &Option<OidStat>| o.as_ref().map(|o| o.id).filter(|id| !id.is_null());
        if id(&info_exclude) != id(&cache.info_exclude().cloned()) {
            cache.invalidate_gitignore(root);
            cache.set_info_exclude(info_exclude);
        }
        if id(&excludes_file) != id(&cache.excludes_file().cloned()) {
            cache.invalidate_gitignore(root);
            cache.set_excludes_file(excludes_file);
        }
        created
    });
    if changed == Some(true) {
        index.mark_untracked_changed();
    }
    true
}

/// `access_or_warn(path, R_OK, 0) == 0`.
fn is_readable(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // Safety: `c` is a NUL-terminated path that outlives the call.
    unsafe { libc::access(c.as_ptr(), libc::R_OK) == 0 }
}

/// `add_patterns()` with an `oid_stat` and no index (dir.c:1311-1325): the global ignore
/// files' stat and id. An empty file is the empty blob; any other is hashed with the
/// newline `add_patterns()` appends before it hashes.
fn global_oid_stat(repo: &gix::Repository, path: &std::path::Path) -> Option<OidStat> {
    let mut file = std::fs::File::open(path).ok()?;
    let stat = Stat::from_fs(&gix::index::fs::Metadata::from_file(&file).ok()?).ok()?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).ok()?;
    let id = hash_with_newline(repo.object_hash(), bytes)?;
    Some(OidStat { stat, id })
}

/// The id `add_patterns()` gives a file's bytes: the empty blob for none at all, otherwise
/// the blob of the bytes and one more `\n`.
fn hash_with_newline(kind: gix::hash::Kind, mut bytes: Vec<u8>) -> Option<ObjectId> {
    if bytes.is_empty() {
        return Some(kind.empty_blob());
    }
    bytes.push(b'\n');
    gix::objs::compute_hash(kind, gix::objs::Kind::Blob, &bytes).ok()
}

/// `add_patterns(fname, …, istate, PATTERN_NOFOLLOW, &oid_stat)` (dir.c:1150-1251) as
/// `prep_exclude()` calls it for `rela`, a per-directory ignore file: its id, or null when
/// there is none.
///
/// A file that cannot be opened is read from the index when its entry is skip-worktree
/// (`read_skip_worktree_file_from_index()`). One that opens is the empty blob when empty,
/// the index's id when the index holds it at stage 0, the refresh found it up to date and
/// no conversion applies to it, and otherwise its bytes hashed with a newline appended.
fn add_patterns_oid(
    repo: &gix::Repository,
    index: &gix::index::State,
    full: &std::path::Path,
    rela: &BStr,
    racy: &dyn Fn(&Stat) -> bool,
    stat_options: gix::index::entry::stat::Options,
    convert: &mut Convert<'_>,
) -> ObjectId {
    let null = repo.object_hash().null();
    let entry = index.entry_by_path_and_stage(rela, gix::index::entry::Stage::Unconflicted);
    // `open_nofollow()`: a symlink is not followed, and fails to open.
    let opened = match full.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => None,
        Ok(_) => std::fs::File::open(full).ok(),
        Err(_) => None,
    };
    let Some(mut file) = opened else {
        return match entry {
            Some(e) if e.flags.contains(Flags::SKIP_WORKTREE) && repo.find_blob(e.id).is_ok() => e.id,
            _ => null,
        };
    };
    let Ok(meta) = file.metadata() else { return null };
    if meta.len() == 0 {
        return repo.object_hash().empty_blob();
    }
    let mut bytes = Vec::new();
    if std::io::Read::read_to_end(&mut file, &mut bytes).is_err() || bytes.len() as u64 != meta.len() {
        return null;
    }
    if let Some(e) = entry {
        if ce_uptodate(repo, e, full, &bytes, racy, stat_options) && !convert.would_convert_to_git(rela) {
            return e.id;
        }
    }
    hash_with_newline(repo.object_hash(), bytes).unwrap_or(null)
}

/// `ce_uptodate()` after the `refresh_index()` `cmd_status()` ran (builtin/commit.c:1629-1632),
/// recomputed for one entry: `refresh_cache_ent()` marks an entry up to date when
/// `ie_match_stat()` finds nothing changed — trivially for assume-valid and skip-worktree
/// entries — or when only its stat moved and the content is still what the index records.
fn ce_uptodate(
    repo: &gix::Repository,
    entry: &gix::index::Entry,
    full: &std::path::Path,
    bytes: &[u8],
    racy: &dyn Fn(&Stat) -> bool,
    stat_options: gix::index::entry::stat::Options,
) -> bool {
    if entry.flags.intersects(Flags::SKIP_WORKTREE | Flags::ASSUME_VALID) {
        return true;
    }
    if entry.flags.contains(Flags::INTENT_TO_ADD) {
        return false;
    }
    let Some(st) = gix::index::fs::Metadata::from_path_no_follow(full)
        .ok()
        .and_then(|m| Stat::from_fs(&m).ok())
    else {
        return false;
    };
    if entry.stat.size != st.size {
        return false;
    }
    if entry.stat.matches(&st, stat_options) && !racy(&entry.stat) {
        return true;
    }
    gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, bytes).ok() == Some(entry.id)
}

/// `would_convert_to_git()` (convert.h:139-143): `convert_to_git()` without a buffer, which
/// answers from the attributes and configuration alone — a clean filter driver, a
/// working-tree encoding, any end-of-line action but binary, or `ident`.
struct Convert<'r> {
    stack: gix::AttributeStack<'r>,
    outcome: gix::attrs::search::Outcome,
    auto_crlf: AutoCrlf,
    drivers: Vec<BString>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AutoCrlf {
    False,
    True,
    Input,
}

impl<'r> Convert<'r> {
    fn new(repo: &'r gix::Repository, index: &gix::index::State) -> anyhow::Result<Self> {
        let config = repo.config_snapshot();
        let auto_crlf = match config.string("core.autocrlf").as_deref().map(|v| v.to_ascii_lowercase()) {
            Some(v) if v.as_slice() == b"input" => AutoCrlf::Input,
            _ => match config.boolean("core.autocrlf") {
                Some(true) => AutoCrlf::True,
                _ => AutoCrlf::False,
            },
        };
        // `read_convert_config()`: every `filter.<name>.*` key makes a driver called `<name>`.
        let drivers = config
            .plumbing()
            .sections_by_name("filter")
            .into_iter()
            .flatten()
            .filter_map(|s| s.header().subsection_name().map(ToOwned::to_owned))
            .collect();
        Ok(Convert {
            stack: repo.attributes_only(index, gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping)?,
            outcome: gix::attrs::search::Outcome::default(),
            auto_crlf,
            drivers,
        })
    }

    fn would_convert_to_git(&mut self, rela: &BStr) -> bool {
        use gix::attrs::StateRef;
        let mode = Some(gix::index::entry::Mode::FILE);
        if self.stack.at_entry(rela, mode).is_err() {
            return false;
        }
        self.outcome.initialize_with_selection(
            self.stack.attributes_collection(),
            ["crlf", "ident", "filter", "eol", "text", "working-tree-encoding"],
        );
        let Ok(platform) = self.stack.at_entry(rela, mode) else {
            return false;
        };
        platform.matching_attributes(&mut self.outcome);
        let states: Vec<StateRef<'_>> = self.outcome.iter_selected().map(|m| m.assignment.state).collect();
        let [crlf, ident, filter, eol, text, encoding] = states[..] else {
            return false;
        };
        // `git_path_check_crlf()` of `text`, then of `crlf`.
        let check_crlf = |state: StateRef<'_>| match state {
            StateRef::Set => Some(Crlf::Text),
            StateRef::Unset => Some(Crlf::Binary),
            StateRef::Value(v) if v.as_bstr() == "input" => Some(Crlf::Text),
            StateRef::Value(v) if v.as_bstr() == "auto" => Some(Crlf::Auto),
            _ => None,
        };
        let mut crlf_action = check_crlf(text).or_else(|| check_crlf(crlf));
        if crlf_action != Some(Crlf::Binary) && matches!(eol, StateRef::Value(v) if v.as_bstr() == "lf" || v.as_bstr() == "crlf") {
            crlf_action = Some(Crlf::Text);
        }
        let crlf_converts = match crlf_action {
            Some(Crlf::Binary) => false,
            Some(_) => true,
            None => self.auto_crlf != AutoCrlf::False,
        };
        let driver = matches!(filter, StateRef::Value(v) if self.drivers.iter().any(|d| d.as_bstr() == v.as_bstr()));
        let encoding = match encoding {
            StateRef::Value(v) => {
                let v = v.as_bstr().to_ascii_lowercase();
                !v.is_empty() && v.as_slice() != b"utf-8" && v.as_slice() != b"utf8"
            }
            _ => false,
        };
        driver || encoding || crlf_converts || matches!(ident, StateRef::Set)
    }
}

/// The `crlf_action`s `would_convert_to_git()` tells apart: binary converts nothing, every
/// other action converts something.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Crlf {
    Binary,
    Text,
    Auto,
}
