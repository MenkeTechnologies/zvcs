//! `git mv` — rename/move a tracked path in the index and worktree.
//!
//! Served natively on the vendored gitoxide index so tools on PATH observe the
//! same staged state. Supports the invocation forms stock `git mv` uses in
//! practice:
//!
//!   * `git mv <src> <dst>`                 — rename a tracked file or directory
//!   * `git mv <src>... <existing-dir>`     — move one or more paths into a dir
//!   * flags `-f`/`--force`, `-k`, `-n`/`--dry-run`, `-v`/`--verbose`, `--`
//!
//! A directory source remaps every tracked entry beneath it. Overwriting a
//! tracked/worktree destination requires `-f`. What is intentionally NOT served
//! is documented where it `bail!`s: sparse-checkout (`--sparse`) semantics and
//! force-overwriting a directory destination.

use anyhow::{anyhow, bail, Result};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stage, Stat};

/// A fully validated move: the on-disk rename plus the index path remaps it
/// implies. For a file the remap list has one pair; for a directory it has one
/// pair per tracked entry beneath the source.
struct Plan {
    src_abs: PathBuf,
    dst_abs: PathBuf,
    src_rel: String,
    dst_rel: String,
    /// (old repo-relative path, new repo-relative path) for each index entry.
    remaps: Vec<(String, String)>,
}

pub fn mv(args: &[String]) -> Result<ExitCode> {
    // 1. Parse flags and collect positional operands. `--` ends option parsing.
    let mut force = false;
    let mut skip = false;
    let mut dry_run = false;
    let mut verbose = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut opts_done = false;
    for a in args {
        if opts_done {
            positional.push(a);
            continue;
        }
        match a.as_str() {
            "--" => opts_done = true,
            "-f" | "--force" => force = true,
            "-k" => skip = true,
            "-n" | "--dry-run" => dry_run = true,
            "-v" | "--verbose" => verbose = true,
            "--sparse" => bail!("--sparse (sparse-checkout) is not supported"),
            s if s.starts_with('-') && s.len() > 1 => bail!("unknown switch `{s}`"),
            s => positional.push(s),
        }
    }

    if positional.len() < 2 {
        bail!("usage: git mv [-v] [-f] [-n] [-k] <source> <destination>");
    }

    // 2. Repository + worktree context. All paths are resolved relative to the
    //    current directory via the repo prefix, then made repo-relative.
    let repo = gix::discover(".")?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("this operation must be run in a work tree"))?
        .to_owned();
    let prefix = repo
        .prefix()
        .map_err(|e| anyhow!("cannot resolve worktree prefix: {e}"))?
        .map(Path::to_path_buf)
        .unwrap_or_default();

    // 3. Split operands: everything but the last is a source; the last is the
    //    destination. Decide file-mode vs into-directory-mode the way git does:
    //    a trailing slash or an existing directory means "into directory".
    let dest_arg = *positional.last().expect("checked len >= 2");
    let sources = &positional[..positional.len() - 1];

    let dest_rel = normalize_rel(&prefix, dest_arg)?;
    let dest_abs = workdir.join(&dest_rel);
    let trailing_slash = dest_arg.ends_with('/');
    let dest_is_dir = dest_abs.is_dir();

    if trailing_slash && !dest_is_dir {
        let first = normalize_rel(&prefix, sources[0])?;
        bail!("destination directory does not exist, source={first}, destination={dest_arg}");
    }
    let dir_mode = dest_is_dir;
    if sources.len() > 1 && !dir_mode {
        bail!("destination '{dest_arg}' is not a directory");
    }

    // 4. Serialize the whole index read-modify-write through the repo
    //    coordinator for real moves; a dry run mutates nothing and needs no
    //    lock. The guard is held across validation, the disk renames, and the
    //    single index write below.
    let _lock = (!dry_run).then(|| crate::lock::RepoLock::acquire(repo.git_dir()));
    let mut index = repo.open_index()?;

    // 5. Validation phase — build a plan per source against the pristine index.
    //    Without `-k` the first failure aborts before ANY disk/index mutation,
    //    matching git's all-or-nothing behavior. With `-k` a failing source is
    //    silently skipped and the command still succeeds.
    let mut plans: Vec<Plan> = Vec::new();
    for s in sources {
        match plan_source(&index, &workdir, &prefix, s, dir_mode, &dest_rel, force) {
            Ok(plan) => plans.push(plan),
            Err(e) => {
                if skip {
                    continue;
                }
                return Err(e);
            }
        }
    }

    // 6. Apply phase — print the same lines git prints, then (unless dry-run)
    //    rename on disk and remap the index entries.
    let mut modified = false;
    for plan in &plans {
        if dry_run {
            println!("Checking rename of '{}' to '{}'", plan.src_rel, plan.dst_rel);
        }
        if verbose || dry_run {
            println!("Renaming {} to {}", plan.src_rel, plan.dst_rel);
        }
        if !dry_run {
            std::fs::rename(&plan.src_abs, &plan.dst_abs)
                .map_err(|e| anyhow!("renaming '{}' failed: {e}", plan.src_rel))?;
            apply_remaps(&mut index, &plan.remaps);
            modified = true;
        }
    }

    // 7. Persist once. `dangerously_push_entry` appends out of order, so restore
    //    the sort invariant before writing, and drop the stale tree-cache so a
    //    later commit doesn't capture a subtree that no longer exists.
    if modified {
        index.sort_entries();
        index.remove_tree();
        index.write(gix::index::write::Options::default())?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Validate a single source against the current index and worktree and return
/// the resulting [`Plan`], or `bail!` with a git-compatible reason.
fn plan_source(
    index: &gix::index::File,
    workdir: &Path,
    prefix: &Path,
    src_arg: &str,
    dir_mode: bool,
    dest_rel: &str,
    force: bool,
) -> Result<Plan> {
    let src_rel = normalize_rel(prefix, src_arg)?;
    let src_abs = workdir.join(&src_rel);

    // When moving into a directory the destination basename is the source's.
    let dst_rel = if dir_mode {
        let base = src_rel.rsplit('/').next().unwrap_or(&src_rel);
        format!("{dest_rel}/{base}")
    } else {
        dest_rel.to_owned()
    };
    let dst_abs = workdir.join(&dst_rel);

    // git reports a same-path move (and a move into a subpath of itself) with
    // this exact phrasing regardless of the item being a file.
    if src_rel == dst_rel || dst_rel.starts_with(&format!("{src_rel}/")) {
        bail!("can not move directory into itself, source={src_rel}, destination={dst_rel}");
    }

    // The source must exist on disk first (git lstat's it before consulting the
    // index): a tracked-but-deleted path reports "bad source", not "not under
    // version control".
    let meta = std::fs::symlink_metadata(&src_abs)
        .map_err(|_| anyhow!("bad source, source={src_rel}, destination={dst_rel}"))?;

    let remaps: Vec<(String, String)> = if meta.is_dir() {
        // Directory: remap every stage-0 entry beneath `src_rel/`.
        let sub_prefix = format!("{src_rel}/");
        let mut remaps = Vec::new();
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted {
                continue;
            }
            let p = e.path_in(backing);
            if p.starts_with(sub_prefix.as_bytes()) {
                let old = String::from_utf8_lossy(p).into_owned();
                let new = format!("{dst_rel}{}", &old[src_rel.len()..]);
                remaps.push((old, new));
            }
        }
        if remaps.is_empty() {
            bail!("not under version control, source={src_rel}, destination={dst_rel}");
        }
        // A directory destination that already exists on disk can't be merged
        // here; git refuses it too (only file destinations honor -f).
        if dst_abs.exists() {
            bail!("destination already exists, source={src_rel}, destination={dst_rel}");
        }
        remaps
    } else {
        // Regular file / symlink: it must be tracked at stage 0.
        if !is_tracked(index, &src_rel) {
            bail!("not under version control, source={src_rel}, destination={dst_rel}");
        }
        // Refuse to clobber an existing destination (tracked or on disk) unless
        // forced. `-f` relies on POSIX rename() replacing the destination file.
        if !force && (dst_abs.exists() || is_tracked(index, &dst_rel)) {
            bail!("destination exists, source={src_rel}, destination={dst_rel}");
        }
        vec![(src_rel.clone(), dst_rel.clone())]
    };

    // Fail early (before any mutation) if the destination's parent is missing,
    // so the abort stays atomic instead of surfacing mid-rename.
    if let Some(parent) = dst_abs.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            bail!("renaming '{src_rel}' failed: No such file or directory");
        }
    }

    Ok(Plan {
        src_abs,
        dst_abs,
        src_rel,
        dst_rel,
        remaps,
    })
}

/// Whether a stage-0 index entry exists at exactly `rel`.
fn is_tracked(index: &gix::index::File, rel: &str) -> bool {
    let backing = index.path_backing();
    index.entries().iter().any(|e| {
        e.stage() == Stage::Unconflicted
            && AsRef::<[u8]>::as_ref(e.path_in(backing)) == rel.as_bytes()
    })
}

/// Apply the (old → new) path remaps to the in-memory index: capture the moved
/// entries' fields, drop the old entries and any entry occupying a new path
/// (the force-overwrite case), then re-append the entries at their new paths.
/// A `sort_entries()` by the caller restores lookup invariants afterward.
fn apply_remaps(index: &mut gix::index::File, remaps: &[(String, String)]) {
    // Capture (new_path, fields) for each source entry before mutating.
    let mut pushes: Vec<(Stat, ObjectId, Flags, Mode, String)> = Vec::with_capacity(remaps.len());
    {
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted {
                continue;
            }
            let p = e.path_in(backing);
            if let Some((_, new)) = remaps
                .iter()
                .find(|(old, _)| old.as_bytes() == AsRef::<[u8]>::as_ref(p))
            {
                pushes.push((e.stat, e.id, e.flags, e.mode, new.clone()));
            }
        }
    }

    // Remove the old source paths and any destination they overwrite.
    let doomed: Vec<&[u8]> = remaps
        .iter()
        .flat_map(|(old, new)| [old.as_bytes(), new.as_bytes()])
        .collect();
    index.remove_entries(|_, path, _| doomed.iter().any(|d| *d == AsRef::<[u8]>::as_ref(path)));

    // Re-append each entry at its new path with the original blob and mode.
    for (stat, id, flags, mode, new) in pushes {
        let new_bytes = BString::from(new);
        index.dangerously_push_entry(stat, id, flags, mode, BStr::new(&new_bytes));
    }
}

/// Turn a CWD-relative operand into a clean, repo-relative, slash-separated
/// path by prepending the worktree `prefix` and resolving `.`/`..` lexically.
/// Rejects absolute paths and any `..` that escapes the worktree.
fn normalize_rel(prefix: &Path, arg: &str) -> Result<String> {
    let joined = prefix.join(arg);
    let mut parts: Vec<String> = Vec::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    bail!("path escapes the working tree: {arg}");
                }
            }
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => {
                bail!("absolute paths are not supported: {arg}")
            }
        }
    }
    if parts.is_empty() {
        bail!("invalid path: {arg}");
    }
    Ok(parts.join("/"))
}
