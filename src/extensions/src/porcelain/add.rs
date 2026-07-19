//! `git add` — stage worktree paths into the index, served natively via the
//! vendored gitoxide crates so tools on PATH see the same staged index.
//!
//! Supported forms (the dominant `git add` invocations):
//!   * `git add <pathspec>...`  — stage files/dirs (recurses, honors `.gitignore`)
//!   * `git add .`              — stage everything under the current prefix
//!   * `git add -A|--all`       — stage the whole worktree (adds, mods, deletes)
//!   * `git add -u|--update`    — restage tracked paths only (mods + deletes)
//!   * flags `-f/--force`, `-n/--dry-run`, `-v/--verbose`, and `--`
//!
//! For each matched worktree file the blob is hashed into the object database and
//! its index entry is (re)written with the current mode and filesystem stat.
//! Tracked paths whose worktree file is gone are staged as deletions, matching
//! modern `git add` semantics. Unmerged (conflicted) entries under a matched path
//! are collapsed to the freshly-staged stage-0 entry.
//!
//! Deviations (bailed or noted, never faked):
//!   * `.gitattributes` content filters (autocrlf, `clean`/`smudge`) are NOT
//!     applied — the blob is the verbatim worktree bytes.
//!   * submodule gitlinks are skipped here (use `git zbump`).
//!   * interactive/patch/edit/intent-to-add/chmod/pathspec-from-file are rejected
//!     with a precise message rather than silently ignored.

use anyhow::{bail, Result};
use std::collections::HashSet;
use std::process::ExitCode;

use gix::bstr::{BStr, BString};
use gix::index::entry::{Flags, Mode, Stage, Stat};

pub fn add(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    if repo.workdir().is_none() {
        bail!("this operation must be run in a work tree");
    }

    // --- argument parse -----------------------------------------------------
    let mut dry_run = false;
    let mut verbose = false;
    let mut force = false;
    let mut all = false;
    let mut update_only = false;
    let mut pathspecs: Vec<String> = Vec::new();
    let mut positional_only = false;

    for a in args {
        if positional_only {
            pathspecs.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => positional_only = true,
            "-n" | "--dry-run" => dry_run = true,
            "-v" | "--verbose" => verbose = true,
            "-f" | "--force" => force = true,
            "-A" | "--all" | "--no-ignore-removal" => all = true,
            "-u" | "--update" => update_only = true,
            // Recognized git flags that this port does not implement: name them.
            "-p" | "--patch" => bail!("interactive patch mode (-p/--patch) is not supported"),
            "-i" | "--interactive" => bail!("interactive mode (-i/--interactive) is not supported"),
            "-e" | "--edit" => bail!("--edit is not supported"),
            "-N" | "--intent-to-add" => bail!("--intent-to-add (-N) is not supported"),
            "--refresh" => bail!("--refresh is not supported"),
            "--renormalize" => bail!("--renormalize is not supported"),
            "--sparse" => bail!("--sparse is not supported"),
            "--ignore-errors" => bail!("--ignore-errors is not supported"),
            "--ignore-missing" => bail!("--ignore-missing is not supported"),
            other if other.starts_with("--chmod") => bail!("--chmod is not supported"),
            other if other.starts_with("--pathspec-from-file") => {
                bail!("--pathspec-from-file is not supported")
            }
            // Bundled short flags like `-nv`; every char must be a known toggle.
            other if other.starts_with('-') && !other.starts_with("--") && other.len() > 1 => {
                for c in other[1..].chars() {
                    match c {
                        'n' => dry_run = true,
                        'v' => verbose = true,
                        'f' => force = true,
                        'A' => all = true,
                        'u' => update_only = true,
                        _ => bail!("unsupported flag -{c}"),
                    }
                }
            }
            other if other.starts_with("--") => bail!("unsupported flag {other}"),
            _ => pathspecs.push(a.clone()),
        }
    }

    if pathspecs.is_empty() && !(all || update_only) {
        bail!("Nothing specified, nothing added.");
    }

    // --- index snapshot: read-only, drives staging decisions and deletions.
    // The authoritative mutation index is re-read under the lock further below.
    let index = if repo.index_path().exists() {
        repo.open_index()?
    } else {
        gix::index::File::from_state(gix::index::State::new(repo.object_hash()), repo.index_path())
    };

    // Repo-relative paths of the current stage-0 entries (tracked set).
    let existing: HashSet<BString> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .filter(|e| e.stage() == Stage::Unconflicted)
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };

    // --- directory walk over the worktree, filtered by the pathspecs --------
    // Emit tracked and untracked files individually; also emit ignored ones so a
    // path that is both tracked and gitignored can still be restaged. Ignored
    // entries are only kept when forced or already tracked (decided below).
    let patterns: Vec<BString> = pathspecs
        .iter()
        .map(|s| BString::from(s.clone().into_bytes()))
        .collect();
    let options = repo
        .dirwalk_options()?
        .emit_tracked(true)
        .emit_ignored(Some(gix::dir::walk::EmissionMode::Matching));

    let dirwalk_index = repo.index_or_load_from_head_or_empty()?;
    let mut iter = repo.dirwalk_iter(dirwalk_index, patterns, Default::default(), options)?;

    // A staged entry to be written into the index.
    struct Staged {
        path: BString,
        id: gix::hash::ObjectId,
        mode: Mode,
        stat: Stat,
    }
    let mut staged: Vec<Staged> = Vec::new();

    for item in iter.by_ref() {
        let entry = item?.entry;
        // Only regular files and symlinks are stageable content; skip directories,
        // submodule repositories, and anything untrackable.
        match entry.disk_kind {
            Some(gix::dir::entry::Kind::File) | Some(gix::dir::entry::Kind::Symlink) => {}
            _ => continue,
        }

        let path = entry.rela_path;
        let already_tracked = existing.contains(&path);

        // Ignore semantics: an ignored path is only staged if forced or already
        // tracked. Tracked/untracked (non-ignored) paths are always eligible.
        if matches!(entry.status, gix::dir::entry::Status::Ignored(_)) && !force && !already_tracked {
            continue;
        }
        // `-u/--update` restages tracked paths only; skip brand-new files.
        if update_only && !already_tracked {
            continue;
        }

        let Some(abs) = repo.workdir_path(&path) else {
            continue;
        };
        let md = gix::index::fs::Metadata::from_path_no_follow(&abs)?;

        let (bytes, mode) = if md.is_symlink() {
            let target = std::fs::read_link(&abs)?;
            #[cfg(unix)]
            let bytes = {
                use std::os::unix::ffi::OsStrExt;
                target.as_os_str().as_bytes().to_vec()
            };
            #[cfg(not(unix))]
            let bytes = target.to_string_lossy().into_owned().into_bytes();
            (bytes, Mode::SYMLINK)
        } else {
            let bytes = std::fs::read(&abs)?;
            let mode = if md.is_executable() {
                Mode::FILE_EXECUTABLE
            } else {
                Mode::FILE
            };
            (bytes, mode)
        };

        let id = repo.write_blob(&bytes)?.detach();
        let stat = Stat::from_fs(&md)?;
        staged.push(Staged { path, id, mode, stat });
    }

    // Recover the pathspec matcher (usable without borrowing the repo) to decide
    // deletions and to validate that each explicit pathspec matched something.
    let mut pathspec = match iter.into_outcome() {
        Some(outcome) => outcome.pathspec,
        None => bail!("directory walk did not complete"),
    };

    let staged_set: HashSet<BString> = staged.iter().map(|s| s.path.clone()).collect();

    // --- deletions: tracked stage-0 paths, matched, whose file is gone ------
    let mut deletions: Vec<BString> = Vec::new();
    {
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted || e.mode == Mode::COMMIT {
                continue; // leave conflicted stages and submodule gitlinks alone
            }
            let path = e.path_in(backing);
            let owned = path.to_owned();
            if staged_set.contains(&owned) {
                continue;
            }
            if !pathspec.is_included(path, Some(false)) {
                continue;
            }
            let gone = match repo.workdir_path(path) {
                Some(p) => std::fs::symlink_metadata(p).is_err(),
                None => true,
            };
            if gone {
                deletions.push(owned);
            }
        }
    }

    // --- validate explicit literal pathspecs matched something --------------
    // Mirrors git's `pathspec '<x>' did not match any files` and its refusal to
    // add a gitignored path without `-f`. Magic pathspecs are left to the matcher.
    let deletion_set: HashSet<&BString> = deletions.iter().collect();
    for p in &pathspecs {
        if p == "." || p.is_empty() || p.starts_with(':') || p.contains(['*', '?', '[']) {
            continue;
        }
        let on_disk = repo
            .workdir_path(BStr::new(p.as_bytes()))
            .is_some_and(|abs| std::fs::symlink_metadata(abs).is_ok());
        let matched_staged = path_is_or_under(staged_set.iter(), p);
        let matched_tracked = path_is_or_under(existing.iter(), p);
        let matched_deleted = path_is_or_under(deletion_set.iter().copied(), p);

        if matched_staged || matched_tracked || matched_deleted {
            continue;
        }
        if on_disk && !force {
            // Present on disk but not staged/tracked ⇒ excluded by .gitignore.
            bail!("path '{p}' is ignored by a .gitignore file (use -f to force add)");
        }
        if !on_disk {
            bail!("pathspec '{p}' did not match any files");
        }
    }

    if staged.is_empty() && deletions.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    // --- dry run: report only, never touch the index ------------------------
    if dry_run {
        for s in &staged {
            println!("add '{}'", s.path);
        }
        for d in &deletions {
            println!("remove '{}'", d);
        }
        return Ok(ExitCode::SUCCESS);
    }

    // --- write path: serialize the read-modify-write through the coordinator.
    // Hold the lock across a FRESH re-read of the on-disk index and the write, so
    // a concurrent writer's changes to other paths are not clobbered — only the
    // paths this invocation touches are replaced.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut index = if repo.index_path().exists() {
        repo.open_index()?
    } else {
        gix::index::File::from_state(gix::index::State::new(repo.object_hash()), repo.index_path())
    };

    // Drop every prior version (any stage) of a staged path and every deletion,
    // then append the fresh stage-0 entries and restore sort order.
    let remove: HashSet<BString> = staged_set
        .iter()
        .cloned()
        .chain(deletions.iter().cloned())
        .collect();
    index.remove_entries(|_, path, _| remove.contains(&path.to_owned()));
    for s in &staged {
        index.dangerously_push_entry(s.stat, s.id, Flags::empty(), s.mode, s.path.as_ref());
    }
    index.sort_entries();

    // The tree-cache extension is written verbatim by `File::write`; drop it after
    // mutating entries so a later commit can't capture a stale subtree.
    index.remove_tree();
    index.write(gix::index::write::Options::default())?;

    if verbose {
        for s in &staged {
            println!("add '{}'", s.path);
        }
        for d in &deletions {
            println!("remove '{}'", d);
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Return `true` if any path in `iter` equals `p` or lives under the directory
/// `p` (i.e. starts with `p` + `/`), the way a directory pathspec matches.
fn path_is_or_under<'a>(mut iter: impl Iterator<Item = &'a BString>, p: &str) -> bool {
    let pb = p.as_bytes();
    let mut prefix = pb.to_vec();
    prefix.push(b'/');
    iter.any(|x| x.as_slice() == pb || x.as_slice().starts_with(&prefix))
}
