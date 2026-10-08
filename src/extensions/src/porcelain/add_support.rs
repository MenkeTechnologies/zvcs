//! What [`super::add`] shares across its scan: the pathspec accounting, the worktree read and
//! the stat rule `run_diff_files(DIFF_RACY_IS_MODIFIED)` selects tracked paths by.
//!
//! (`git stage` is `cmd_add()` under another name and is served by [`super::add`] itself.)
//!
//! The walk reproduces `run_diff_files(DIFF_RACY_IS_MODIFIED)`'s selection rule for tracked
//! paths — a stat mismatch under [`stat_match`], or the racily-clean window [`racy_paths`]
//! describes — because that rule decides three observable things at once: which paths are
//! hashed (and so which can raise the `core.autocrlf` round-trip warning), which are reported,
//! and which reach `add_to_index()` under `-N` and pull the empty blob into the object database.

use anyhow::{anyhow, Result};
use std::collections::{BTreeSet, HashSet};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::index::entry::{Mode, Stage};

/// Exit code git uses for a fatal error.
const FATAL: u8 = 128;

/// A pathspec that carries `:(exclude)`/`:!` magic never has to match anything,
/// so it is exempt from the "did not match any files" check.
pub(super) fn is_exclude_spec(spec: &str) -> bool {
    spec.starts_with(":!")
        || spec.starts_with(":^")
        || (spec.starts_with(":(") && spec[..spec.find(')').unwrap_or(0)].contains("exclude"))
}

/// True when the pathspec is a plain path with no magic and no wildcard, which is
/// the only form for which git reports the gitignore / not-known-to-git errors
/// instead of the generic "did not match any files".
fn is_literal_spec(spec: &str) -> bool {
    !spec.is_empty() && !spec.starts_with(':') && !spec.contains(['*', '?', '['])
}

/// Mark every positive pathspec that matches at least one of `paths` as seen.
///
/// git marks a pathspec seen the moment it matches any examined path on its own —
/// before exclude pathspecs are applied, and regardless of whether another
/// pathspec also matched that path. gix's combined matcher instead attributes each
/// path to a single pathspec and never yields a path an exclude pathspec shadowed,
/// so it under-reports overlapping specs (`src/ src/lib.rs` both matching
/// `src/lib.rs`) and exclude-shadowed specs (`*.md` whose only match is dropped by
/// `:(exclude)README.md`). Recover the rest by testing each still-unseen positive
/// pathspec against `paths` with its own single-pattern matcher, which carries no
/// exclude and so matches exactly what git counts. `paths` is the universe of
/// tracked and to-be-staged paths — never a gitignored-and-skipped one, so a
/// wildcard whose only match is gitignored still (correctly) stays unseen.
pub(super) fn mark_seen_per_spec(
    repo: &gix::Repository,
    index: &gix::index::File,
    patterns: &[BString],
    specs: &[String],
    paths: &[BString],
    seen: &mut HashSet<usize>,
) -> Result<()> {
    // `patterns` is 1:1 with `specs`, so the index doubles as the seen key.
    for (i, spec) in specs.iter().enumerate() {
        if seen.contains(&i) || is_exclude_spec(spec) || spec.is_empty() {
            continue;
        }
        // `:(attr:…)` elements are checked in attr.c's default direction,
        // `GIT_ATTR_CHECKIN` (attr.h:202-206; builtin/add.c never changes it): the
        // work tree's `.gitattributes` first, the index's only where it is absent.
        // The walk that staged these paths already reads them that way
        // (`Repository::dirwalk`), so an index-only read here left an untracked
        // `.gitattributes` unseen and `add ':(attr:text)'` died with "did not match".
        let mut ps = repo.pathspec(
            true,
            std::slice::from_ref(&patterns[i]),
            false,
            index,
            gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
        )?;
        if paths.iter().any(|p| ps.is_included(p.as_bstr(), Some(false))) {
            seen.insert(i);
        }
    }
    Ok(())
}

/// Which of git's four unmatched-pathspec reporters is in force. `cmd_add()`
/// reaches a different one per mode, and they disagree on both wording and
/// severity, so the mode has to be carried explicitly.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum SpecMode {
    /// A plain add. `cmd_add()`'s own loop (builtin/add.c:540-570) dies on an
    /// element that names nothing; one that exists on disk yet matched nothing was
    /// excluded by .gitignore, and `add_files()` (builtin/add.c:344-352) names all
    /// of those together, sets `exit_status = 1` and **keeps staging**.
    Add,
    /// `-u`: that loop stays silent for an element which exists on disk, and
    /// `report_path_error()` (dir.c:637-674) afterwards names *every* such element
    /// with `error:` before `exit(128)` (builtin/add.c:596-597).
    Update,
    /// `--renormalize`: implies `-u`, but the `report_path_error()` call is guarded
    /// by `!add_renormalize`, so an existing untracked element is accepted in
    /// silence.
    Renormalize,
    /// `--refresh`: `refresh()` (builtin/add.c:135-148) runs before that loop and
    /// `goto finish`es past it. Its own check knows only the fatal and the sparse
    /// report — never the gitignore block, never the "known to git" wording.
    Refresh,
}

/// What git does about the pathspecs that matched nothing.
pub(super) enum SpecVerdict {
    /// Every pathspec was accounted for.
    Ok,
    /// `die()` from `cmd_add()`'s own loop (builtin/add.c:566) or from `refresh()`,
    /// both of which run *before* `odb_transaction_begin()` (builtin/add.c:584):
    /// nothing was hashed, so nothing is staged and nothing is written.
    Fatal(ExitCode),
    /// `report_path_error()`'s `exit(128)` (builtin/add.c:596-597), which runs
    /// *after* `add_files_to_cache()` has hashed and deposited the blob of every
    /// path it did match. The index is never written; those objects stay.
    Unknown(ExitCode),
    /// The gitignore block was printed and `exit_status` became 1. Staging carries
    /// on; the 1 surfaces at `finish:` — see [`super::add::finish_code`].
    Ignored,
}

/// The pathspec state both verbs feed to [`unmatched_pathspec_check`].
pub(super) struct SpecCheck<'a> {
    /// `pathspec.items[i].original`: each element exactly as the user typed it,
    /// which is the form every one of these diagnostics quotes back.
    pub original: &'a [String],
    /// `pathspec.items[i].match`: the same elements made repo-relative, the only
    /// form `file_exists()` can be asked about.
    pub resolved: &'a [String],
    /// `--ignore-missing` (legal only with `--dry-run`): suppresses the fatal and
    /// asks `is_excluded()` instead, so a path that does not exist can still be
    /// reported as gitignored. It does **not** silence the gitignore block.
    pub ignore_missing: bool,
    /// `--ignore-errors`: `report_path_error()` is not even called under it
    /// (builtin/add.c:596), so `-u` says nothing about its unmatched elements.
    pub ignore_errors: bool,
    pub mode: SpecMode,
}

/// Report the pathspecs that matched nothing, in the wording and with the severity
/// git uses for the mode in force.
///
/// `seen` holds indices into `c.original`, in argv order.
///
/// Not ported: `report_path_error()`'s duplicate suppression, which skips an
/// unmatched element when an identically-spelled one *did* match. Identical
/// elements share a pattern here, so [`mark_seen_per_spec`] always marks them
/// alike and the two can never disagree.
pub(super) fn unmatched_pathspec_check(
    repo: &gix::Repository,
    index: &gix::index::File,
    c: &SpecCheck<'_>,
    seen: &HashSet<usize>,
) -> Result<SpecVerdict> {
    // Literal pathspecs that exist on disk yet matched nothing: under `Add` a
    // .gitignore exclusion, reported as one block listing every such path — but
    // only once the loop below has had its chance to die, since the fatal outranks
    // it in either argv order.
    let mut ignored: BTreeSet<&str> = BTreeSet::new();
    // `report_path_error()` collects rather than dying, so `-u` names every
    // unmatched element before it exits.
    let mut unknown: Vec<&str> = Vec::new();
    // `is_excluded()`, built only if the `--ignore-missing` arm actually asks.
    let mut excludes = None;
    // The walk's ignored entries, collapsed as `fill_directory()` records them,
    // built only if an existing element is not excluded on its own account.
    let mut walked: Option<Vec<String>> = None;

    for (i, spec) in c.original.iter().enumerate() {
        if seen.contains(&i) || is_exclude_spec(spec) || spec.is_empty() {
            continue;
        }
        // `file_exists(pathspec.items[i].match)` (builtin/add.c:561): git asks about
        // the element *after* the prefix pass, which is the only form a worktree path
        // can be built from. The message still quotes `original`, i.e. `spec`.
        let relative = c.resolved.get(i).map(String::as_str).unwrap_or(spec.as_str());
        // `path` is `pathspec.items[i].match`: the element with its magic parsed
        // off. `:(attr:-text)` has the empty match and `:(attr:text)f` the match
        // `f`, so testing the typed text instead made both die where git — which
        // skips the first and finds the second on disk — exits 0. The two
        // PATHSPEC_* bits the test reads are the parsed `icase` signature and the
        // `glob` search mode (magic or `GIT_GLOB_PATHSPECS` alike).
        let parsed = repo
            .pathspec_defaults_inherit_ignore_case(false)
            .ok()
            .and_then(|defaults| gix::pathspec::parse(relative.as_bytes(), defaults).ok());
        let (path, glob_or_icase) = match &parsed {
            Some(p) => (
                p.path().to_str_lossy().into_owned(),
                p.signature.contains(gix::pathspec::MagicSignature::ICASE)
                    || p.search_mode == gix::pathspec::SearchMode::PathAwareGlob,
            ),
            None => (relative.to_string(), !is_literal_spec(spec)),
        };
        // `if (!path[0]) continue;` — "don't complain at 'git add .' on empty repo".
        // A `.` at the prefix resolves to the empty match, which selects everything.
        if path.is_empty() || path == "." {
            continue;
        }
        let on_disk = repo
            .workdir_path(BStr::new(path.as_bytes()))
            .is_some_and(|abs| std::fs::symlink_metadata(abs).is_ok());

        // `(magic & (PATHSPEC_GLOB | PATHSPEC_ICASE)) || !file_exists(path)`
        // (builtin/add.c:654-655): a `:(glob)` or `:(icase)` element is judged
        // purely on whether the matcher found anything; any other element — a
        // bare wildcard included — dies only when its match names no file.
        if !on_disk || glob_or_icase || c.mode == SpecMode::Refresh {
            // `if (ignore_missing) { if (is_excluded(...)) dir_add_ignored(...); }`
            // (builtin/add.c:562-567): the flag exists to answer "would this path be
            // ignored if it existed", so the element joins the gitignore block
            // instead of killing the run. `refresh()` has no such arm.
            if !c.ignore_missing || c.mode == SpecMode::Refresh {
                eprintln!("fatal: pathspec '{spec}' did not match any files");
                return Ok(SpecVerdict::Fatal(ExitCode::from(FATAL)));
            }
            let stack = match &mut excludes {
                Some(stack) => stack,
                none => none.insert(repo.excludes(
                    index,
                    None,
                    gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
                )?),
            };
            // `get_dtype()` leaves an absent path `DT_UNKNOWN`, which
            // `last_matching_pattern()` treats as a non-directory.
            let mode = match std::fs::symlink_metadata(
                repo.workdir_path(BStr::new(relative.as_bytes())).unwrap_or_default(),
            ) {
                Ok(md) if md.is_dir() => gix::index::entry::Mode::DIR,
                _ => gix::index::entry::Mode::FILE,
            };
            if stack.at_entry(BStr::new(relative.as_bytes()), Some(mode))?.is_excluded() {
                ignored.insert(relative);
            }
            // `--ignore-missing` only silences this loop. `report_path_error()` reads
            // `ps_matched`, which `add_files_to_cache()` fills and the flag never
            // touches, so under `-u` the element is still named and still exits 128.
            if c.mode == SpecMode::Update {
                unknown.push(spec);
            }
            continue;
        }
        match c.mode {
            SpecMode::Update => unknown.push(spec),
            // `dir_add_ignored()` records `pathspec.items[i].match`, so the block
            // below lists the repo-relative form, not the element as typed.
            SpecMode::Add => {
                // `dir.ignored` only holds what `fill_directory()` kept, and the walk
                // keeps a path only once `match_pathspec()` — attribute filter
                // included (dir.c:365-367) — accepts it. An `:(attr:…)` element
                // whose file exists but carries other attributes was never matched,
                // so it is neither seen nor ignored: git exits 0 without a word.
                let has_attrs = parsed.as_ref().is_some_and(|p| !p.attributes.is_empty());
                if has_attrs {
                    let mut single = repo.pathspec(
                        true,
                        std::slice::from_ref(&relative),
                        false,
                        index,
                        gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
                    )?;
                    if !single.is_included(BStr::new(path.as_bytes()), Some(false)) {
                        continue;
                    }
                }
                // Same rule, stated generally: `dir.ignored` is filled by
                // `fill_directory()` from paths the *exclude* machinery
                // classified, so a path that exists and was simply not matched
                // belongs in no block at all — git exits 0 saying nothing. The
                // element that shows it is a rooted one, whose path is verbatim:
                // `git add ':(top)./a.txt'` names a file that exists, matches no
                // entry (`./a.txt` is not `a.txt`) and is not ignored, and
                // listing it under the gitignore banner claimed an exclude rule
                // that does not exist.
                let stack = match &mut excludes {
                    Some(stack) => stack,
                    none => none.insert(repo.excludes(
                        index,
                        None,
                        gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
                    )?),
                };
                let mode = match std::fs::symlink_metadata(
                    repo.workdir_path(BStr::new(relative.as_bytes())).unwrap_or_default(),
                ) {
                    Ok(md) if md.is_dir() => gix::index::entry::Mode::DIR,
                    _ => gix::index::entry::Mode::FILE,
                };
                // A directory no rule names but whose every entry is ignored is
                // still in `dir.ignored`: `treat_directory()` collapses it into one
                // ignored entry (dir.c), so `git add logs/` under `*.log` lands in
                // the block although `logs` itself is not excluded.
                let excluded = stack.at_entry(BStr::new(relative.as_bytes()), Some(mode))?.is_excluded()
                    || covering_ignored_name(walked.get_or_insert_with(|| walk_ignored_names(repo, index)), relative)
                        .is_some();
                if !excluded {
                    continue;
                }
                ignored.insert(relative);
            }
            SpecMode::Renormalize | SpecMode::Refresh => {}
        }
    }

    // `take_worktree_changes && !add_renormalize && !ignore_add_errors`
    // (builtin/add.c:596): with `--ignore-errors` git never calls
    // `report_path_error()` at all, so `-u --ignore-errors <untracked>` is silent.
    if !unknown.is_empty() && !c.ignore_errors {
        for spec in unknown {
            eprintln!("error: pathspec '{spec}' did not match any file(s) known to git");
        }
        return Ok(SpecVerdict::Unknown(ExitCode::from(FATAL)));
    }
    if !ignored.is_empty() {
        eprintln!("The following paths are ignored by one of your .gitignore files:");
        for p in collapsed_ignored_names(repo, index, &ignored) {
            eprintln!("{p}");
        }
        // `advise_if_enabled(ADVICE_ADD_IGNORED_FILE, …)` (builtin/add.c:351-352):
        // the preamble and the path list are plain stderr writes, only the
        // closing line is the hint — and its `Disable this message with …`
        // trailer is `vadvise()`'s, printed only while the slot is unconfigured.
        crate::advice::Advice::AddIgnoredFile
            .advise_in(repo, "Use -f if you really want to add them.");
        return Ok(SpecVerdict::Ignored);
    }
    Ok(SpecVerdict::Ok)
}

/// The names git's walk recorded for an ignored `relative`, which are not the
/// path itself.
///
/// `add_files()` prints `dir->ignored[i]->name` (builtin/add.c:349-350), and what
/// the walk put there is what `treat_directory()` decided: a directory an ignore
/// rule names, *or* one whose every entry is ignored, is collapsed into a single
/// ignored entry and its contents are never visited. So `git add logs/debug.log`
/// in a tree where `*.log` covers all of `logs/` reports `logs`, not the file —
/// and the recorded name carries no trailing slash.
///
/// The collapse is the walk's, so it is asked of the walk rather than
/// re-derived: the same dirwalk in `CollapseDirectory` mode over this one path.
/// An element that names nothing on disk — the `--ignore-missing` arm, where git
/// calls `dir_add_ignored()` with the pathspec's own path — walks to nothing and
/// keeps its name.
fn collapsed_ignored_names(
    repo: &gix::Repository,
    index: &gix::index::File,
    relative: &BTreeSet<&str>,
) -> BTreeSet<String> {
    let collapsed = walk_ignored_names(repo, index);
    relative
        .iter()
        .map(|spec| {
            covering_ignored_name(&collapsed, spec)
                .unwrap_or_else(|| spec.trim_end_matches('/'))
                .to_string()
        })
        .collect()
}

/// Every ignored entry of a whole-worktree dirwalk in `CollapseDirectory` mode.
///
/// The collapse is a property of the walk, not of the element that reached it,
/// so the walk has to run over the whole worktree — restricting it to the
/// element would hand back the element again, which is the very thing the
/// collapse replaces.
fn walk_ignored_names(repo: &gix::Repository, index: &gix::index::File) -> Vec<String> {
    let collapsed = || -> Result<Vec<String>> {
        let options = repo
            .dirwalk_options()?
            .emit_ignored(Some(gix::dir::walk::EmissionMode::CollapseDirectory))
            .emit_untracked(gix::dir::walk::EmissionMode::CollapseDirectory);
        let patterns = vec![BString::from(":/")];
        let mut names = Vec::new();
        for item in repo.dirwalk_iter(index.clone(), patterns, Default::default(), options)? {
            let item = item?;
            if !matches!(item.entry.status, gix::dir::entry::Status::Ignored(_)) {
                continue;
            }
            names.push(item.entry.rela_path.to_string());
        }
        Ok(names)
    };
    collapsed().unwrap_or_default()
}

/// The walked ignored entry that covers `spec` — the entry that *is* it, or a
/// directory it sits under — without its trailing slash.
fn covering_ignored_name<'a>(collapsed: &'a [String], spec: &str) -> Option<&'a str> {
    let spec = spec.trim_end_matches('/');
    collapsed.iter().find_map(|name| {
        let name = name.trim_end_matches('/');
        let covers = spec == name || spec.strip_prefix(name).is_some_and(|r| r.starts_with('/'));
        covers.then_some(name)
    })
}

/// The conflicted paths `refresh()`'s `refresh_index()` names (read-cache.c:1518,
/// 1559-1560): `<path>: needs merge` under `REFRESH_QUIET`, and under `-v`'s
/// `REFRESH_IN_PORCELAIN` `U\t<path>`, the first one preceded by the header
/// `show_file()` prints once (read-cache.c:1450-1456, builtin/add.c:133-134).
pub(super) fn print_refresh_unmerged(paths: &[BString], verbose: bool) {
    for (n, path) in paths.iter().enumerate() {
        if !verbose {
            println!("{path}: needs merge");
            continue;
        }
        if n == 0 {
            println!("Unstaged changes after refreshing the index:");
        }
        println!("U\t{path}");
    }
}

/// The worktree bytes of `abs` as git would store them: a regular file goes
/// through `convert_to_git()` (`.gitattributes` filters, `core.autocrlf`, …), a
/// symlink's target is stored verbatim.
///
/// Shared with [`super::add::renormalize_tracked_files`], which writes the objects
/// of a `--renormalize` run for both verbs and needs exactly these bytes.
pub(super) fn read_converted_bytes(
    repo: &gix::Repository,
    filters: &mut super::convert_to_git::WorktreeFilter,
    rela: &BStr,
    abs: &std::path::Path,
    md: &gix::index::fs::Metadata,
) -> Result<(Vec<u8>, Mode)> {
    let (bytes, mode) = read_worktree_bytes(abs, md)?;
    if mode == Mode::SYMLINK {
        return Ok((bytes, mode));
    }
    let rela = gix::path::from_bstr(rela).into_owned();
    let converted = filters
        .convert(repo, &rela, &bytes)
        .map_err(|e| anyhow!("{e}"))?;
    Ok((converted, mode))
}

fn read_worktree_bytes(
    abs: &std::path::Path,
    md: &gix::index::fs::Metadata,
) -> Result<(Vec<u8>, Mode)> {
    if md.is_symlink() {
        let target = std::fs::read_link(abs)?;
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            target.as_os_str().as_bytes().to_vec()
        };
        #[cfg(not(unix))]
        let bytes = target.to_string_lossy().into_owned().into_bytes();
        Ok((bytes, Mode::SYMLINK))
    } else {
        let bytes = std::fs::read(abs)?;
        let mode = if md.is_executable() {
            Mode::FILE_EXECUTABLE
        } else {
            Mode::FILE
        };
        Ok((bytes, mode))
    }
}

/// `ce_match_stat_basic()`'s field selection (read-cache.c), which is configurable
/// and therefore cannot be a constant.
///
/// * `core.trustctime` (default true) gates the `CTIME_CHANGED` comparison.
/// * `core.checkStat` — `default` (true) or `minimal` (false) — gates the ctime,
///   uid/gid and inode comparisons together. `minimal` leaves only mode, size and
///   mtime, which is what a repository whose worktree has been copied or restored
///   from a backup needs: an inode cannot survive a copy, so with the default
///   setting every entry in the copy reads as modified.
/// * `USE_NSEC` is a compile-time option git does not enable by default and stock
///   2.55.0 as shipped does not carry, so the nanosecond fields are never compared.
///   [`racy_paths`] leaves them out for the same reason.
/// * `st_dev` is left out for the same reason gitoxide leaves it out.
pub(super) fn stat_match(repo: &gix::Repository) -> gix::index::entry::stat::Options {
    let cfg = repo.config_snapshot();
    gix::index::entry::stat::Options {
        trust_ctime: cfg.boolean("core.trustctime").unwrap_or(true),
        check_stat: cfg
            .string("core.checkStat")
            .is_none_or(|v| v.as_bstr() != "minimal"),
        use_nsec: false,
        use_stdev: false,
    }
}

/// The stage-0 paths `is_racy_stat()` (read-cache.c) considers racily clean: the
/// index file's own timestamp is not newer than the entry's recorded mtime, so a
/// write in the same filesystem tick could have gone unnoticed.
///
/// `run_diff_files()` matches with `CE_MATCH_RACY_IS_DIRTY`, and so does
/// `add_to_index()` itself (read-cache.c:717), so those paths are reported as
/// modified regardless of content — which is why a freshly checked out or freshly
/// copied worktree hands every tracked path to `add_file_to_index()`, `-N`
/// included, and why every one of them is hashed with `INDEX_WRITE_OBJECT` and can
/// therefore raise the `core.safecrlf` round-trip warning.
///
/// The comparison is whole seconds. `is_racy_stat()` only consults the nanosecond
/// field under `USE_NSEC`, which is a compile-time option git does not enable by
/// default and which stock git 2.55.0 as shipped by Homebrew does not carry: a
/// repository whose index and worktree files were written in the same second
/// reports the warning indefinitely, not for a sub-second window. Verified against
/// that binary — a fixture whose index mtime is `…986.092380002` and whose
/// `crlf.txt` mtime is `…986.072577080` (index strictly *newer* in nanoseconds, so
/// not racy under `USE_NSEC`) still warns, three seconds later and every time.
/// [`stat_match`] leaves `use_nsec` off for the same reason.
pub(super) fn racy_paths(index: &gix::index::File, repo: &gix::Repository) -> HashSet<BString> {
    let Ok(meta) = std::fs::metadata(repo.index_path()) else {
        return HashSet::new();
    };
    let Ok(modified) = meta.modified() else {
        return HashSet::new();
    };
    let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return HashSet::new();
    };
    let idx_sec = since.as_secs();
    if idx_sec == 0 {
        return HashSet::new();
    }
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .filter(|e| e.stage() == Stage::Unconflicted)
        .filter(|e| idx_sec <= u64::from(e.stat.mtime.secs))
        .map(|e| e.path_in(backing).to_owned())
        .collect()
}


