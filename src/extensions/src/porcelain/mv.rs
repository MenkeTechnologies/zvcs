//! `git mv` — rename/move a tracked path in the index and worktree.
//!
//! Served natively on the vendored gitoxide index so tools on PATH observe the
//! same staged state. Supports the invocation forms stock `git mv` uses in
//! practice:
//!
//!   * `git mv <src> <dst>`                 — rename a tracked file or directory
//!   * `git mv <src>... <existing-dir>`     — move one or more paths into a dir
//!   * flags `-f`/`--force`, `-k`, `-n`/`--dry-run`, `-v`/`--verbose`,
//!     `--sparse`, `-h`, `--`
//!
//! A directory source remaps every tracked entry beneath it. Overwriting a
//! tracked/worktree destination requires `-f`. Exit codes match stock git:
//! usage errors return 129, fatal errors return 128, `-k`-skipped failures
//! still return 0. `--sparse` is accepted as a no-op — this server enforces no
//! sparse-checkout cone, so relaxing that cone (all `--sparse` does) is already
//! the behavior of a plain move.

use anyhow::{anyhow, bail, Result};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stage, Stat};

/// `git mv -h` help, printed verbatim to stdout (git exits 129 after it).
const HELP: &str = "\
usage: git mv [-v] [-f] [-n] [-k] <source> <destination>
   or: git mv [-v] [-f] [-n] [-k] <source>... <destination-directory>

    -v, --[no-]verbose    be verbose
    -n, --[no-]dry-run    dry run
    -f, --[no-]force      force move/rename even if target exists
    -k                    skip move/rename errors
    --[no-]sparse         allow updating entries outside of the sparse-checkout cone

";

/// Print a fatal message to stderr and return git's fatal exit code (128).
/// stderr prose is not a compatibility surface (git's own is terse and varies);
/// the exit code is, so it is pinned exactly.
fn fatal(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// Print the usage line to stderr and return git's usage exit code (129).
fn usage_err() -> Result<ExitCode> {
    eprintln!("usage: git mv [-v] [-f] [-n] [-k] <source> <destination>");
    Ok(ExitCode::from(129))
}

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
            "-h" => {
                // git prints the full help to stdout and exits 129, before any
                // repository lookup — so `-h` works outside a work tree too.
                // (`--help` is deliberately NOT handled here: stock git execs the
                //  man pager for it, a foreign op this server cannot reproduce.)
                print!("{HELP}");
                return Ok(ExitCode::from(129));
            }
            "-f" | "--force" => force = true,
            "-k" => skip = true,
            "-n" | "--dry-run" => dry_run = true,
            "-v" | "--verbose" => verbose = true,
            // No sparse-checkout cone is enforced here, so `--sparse` (which only
            // relaxes that cone) is byte-for-byte a plain move. Accept, no-op.
            "--sparse" => {}
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("error: unknown option `{}'", s.trim_start_matches('-'));
                return usage_err();
            }
            s => positional.push(s),
        }
    }

    if positional.len() < 2 {
        return usage_err();
    }

    // 2. Repository + worktree context. All paths are resolved relative to the
    //    current directory via the repo prefix, then made repo-relative.
    let repo = match gix::discover(".") {
        Ok(r) => r,
        Err(_) => return fatal("not a git repository (or any of the parent directories): .git"),
    };
    let workdir = match repo.workdir() {
        Some(w) => w.to_owned(),
        None => return fatal("this operation must be run in a work tree"),
    };
    let prefix = match repo.prefix() {
        Ok(p) => p.map(Path::to_path_buf).unwrap_or_default(),
        Err(e) => return fatal(format!("cannot resolve worktree prefix: {e}")),
    };

    // 3. Split operands: everything but the last is a source; the last is the
    //    destination. Decide file-mode vs into-directory-mode the way git does:
    //    a trailing slash or an existing directory means "into directory".
    let dest_arg = *positional.last().expect("checked len >= 2");
    let sources = &positional[..positional.len() - 1];

    let dest_rel = match normalize_rel(&workdir, &prefix, dest_arg) {
        Ok(r) => r,
        Err(e) => return fatal(e),
    };
    let dest_abs = workdir.join(&dest_rel);
    let trailing_slash = dest_arg.ends_with('/');
    let dest_is_dir = dest_abs.is_dir();

    if trailing_slash && !dest_is_dir {
        let first = match normalize_rel(&workdir, &prefix, sources[0]) {
            Ok(r) => r,
            Err(e) => return fatal(e),
        };
        return fatal(format!(
            "destination directory does not exist, source={first}, destination={dest_arg}"
        ));
    }
    let dir_mode = dest_is_dir;
    if sources.len() > 1 && !dir_mode {
        return fatal(format!("destination '{dest_arg}' is not a directory"));
    }

    // 4. Serialize the whole index read-modify-write through the repo
    //    coordinator for real moves; a dry run mutates nothing and needs no
    //    lock. The guard is held across validation, the disk renames, and the
    //    single index write below.
    let _lock = (!dry_run).then(|| crate::lock::RepoLock::acquire(repo.git_dir()));
    let mut index = match repo.open_index() {
        Ok(i) => i,
        Err(e) => return fatal(format!("index file corrupt: {e}")),
    };

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
                return fatal(format!("{e:#}"));
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
            if let Err(e) = std::fs::rename(&plan.src_abs, &plan.dst_abs) {
                return fatal(format!("renaming '{}' failed: {e}", plan.src_rel));
            }
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
    let src_rel = normalize_rel(workdir, prefix, src_arg)?;
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

/// Turn an operand into a clean, repo-relative, slash-separated path.
///
/// Relative operands are resolved against the worktree `prefix` (the repo-
/// relative CWD). Absolute operands are resolved against the worktree root
/// `workdir` and stripped back to repo-relative — stock git accepts an absolute
/// path that lands inside the worktree (verified: `git mv /abs/inside/a b`
/// exits 0). `.`/`..` are folded lexically. Any path that escapes the worktree
/// is a fatal "outside repository", matching git's exit 128.
fn normalize_rel(workdir: &Path, prefix: &Path, arg: &str) -> Result<String> {
    let arg_path = Path::new(arg);
    let joined = if arg_path.is_absolute() {
        // Resolve symlinks on the longest existing ancestor (macOS /tmp ->
        // /private/tmp), keep any not-yet-created tail, then strip the worktree
        // root. Anything not under it is outside the repository.
        let canon_wd = workdir
            .canonicalize()
            .unwrap_or_else(|_| workdir.to_path_buf());
        let real = canonicalize_lenient(arg_path);
        match real.strip_prefix(&canon_wd) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
            _ => bail!("'{arg}' is outside repository at '{}'", canon_wd.display()),
        }
    } else {
        prefix.join(arg)
    };
    let mut parts: Vec<String> = Vec::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    bail!("'{arg}' is outside repository at '{}'", workdir.display());
                }
            }
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => {
                // Absolute inputs are stripped to worktree-relative above, so a
                // residual root component here means the path escaped.
                bail!("'{arg}' is outside repository at '{}'", workdir.display())
            }
        }
    }
    if parts.is_empty() {
        bail!("invalid path: {arg}");
    }
    Ok(parts.join("/"))
}

/// Canonicalize the longest existing prefix of `p`, re-appending the trailing
/// components that don't exist yet (a not-yet-created move destination). Falls
/// back to the path as given when nothing along it can be canonicalized.
fn canonicalize_lenient(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p;
    while let Some(parent) = cur.parent() {
        if let Some(name) = cur.file_name() {
            tail.push(name.to_os_string());
        }
        if let Ok(c) = parent.canonicalize() {
            let mut out = c;
            for name in tail.iter().rev() {
                out.push(name);
            }
            return out;
        }
        cur = parent;
    }
    p.to_path_buf()
}
