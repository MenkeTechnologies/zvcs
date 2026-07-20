//! `git merge-index` — run a merge program for every index entry needing merging.
//!
//! Served natively through the vendored gitoxide crates: the index is read with
//! `gix`, unmerged entries are grouped by path, and the `<merge-program>` is
//! spawned once per path with git's exact argument vector:
//!
//! ```text
//! <merge-program> <sha1> <sha2> <sha3> <path> <mode1> <mode2> <mode3>
//! ```
//!
//! where slots 1/2/3 hold the stage-1 (base), stage-2 (ours) and stage-3
//! (theirs) blob ids, each an empty string when that stage is absent, and the
//! mode slots are the 6-digit octal index modes (also empty when absent).
//!
//! Ported flags — the whole documented surface:
//!   * `-o` — one-shot: keep merging after a failure, report the error count at
//!     the end instead of stopping at the first one
//!   * `-q` — do not print `fatal: merge program failed`
//!   * `-a` — merge every unmerged path in the index
//!   * `--` — stop option parsing
//!   * `<file>...` — merge these paths, in the order given
//!
//! Bug-for-bug faithful details, all verified against stock git:
//!   * Option parsing is strictly positional, exactly as in `cmd_merge_index`:
//!     `-o` is only recognised as the first argument and `-q` only as the next
//!     one, so `git merge-index -q -o prog -a` takes `-o` as the *program*.
//!   * Fewer than two arguments prints the usage line and exits 129.
//!   * A path that is not in the index at all is
//!     `fatal: git merge-index: <path> not in the cache`, exit 128; a path
//!     present at stage 0 is already merged and is silently skipped.
//!   * A program that cannot be spawned prints
//!     `fatal: cannot exec '<prog>': <reason>` and counts as a failed merge —
//!     that line is printed even under `-q`.
//!   * Exit codes on failure: without `-o`, 128 (`fatal: merge program failed`)
//!     or 1 under `-q`; with `-o`, still 128 unless `-q`, in which case the
//!     number of failed merges is returned.
//!   * The merge program runs with the worktree root as its working directory,
//!     matching git's `RUN_SETUP` chdir, and index paths are used verbatim
//!     (they are root-relative and are never re-based onto the current prefix).
//!
//! Not covered: nothing in the documented flag set. The one environmental
//! approximation is the program lookup — git prepends its exec-path to `PATH`
//! so that `git merge-index git-merge-one-file ...` resolves, and there is no
//! exec-path concept in the vendored crates, so this port reads `GIT_EXEC_PATH`
//! and otherwise asks the installed git for it once. When neither is available
//! the child is looked up on the inherited `PATH` alone.

use anyhow::Result;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;

const USAGE: &str = "usage: git merge-index [-o] [-q] <merge-program> (-a | [--] [<filename>...])";

/// One index entry, flattened out of the index so the borrow on it ends before
/// we start spawning children.
struct Row {
    path: BString,
    stage: u32,
    id: ObjectId,
    mode: u32,
}

/// Whether to keep going after a step, or stop the whole command right now.
enum Flow {
    Continue,
    Exit(u8),
}

/// Invocation-wide state: the parsed options plus the accumulated error count.
struct Ctx {
    /// The `<merge-program>` to spawn.
    pgm: OsString,
    /// `-o`: run every merge, report failures at the end.
    one_shot: bool,
    /// `-q`: suppress `fatal: merge program failed`.
    quiet: bool,
    /// Number of failed merges so far (only accumulates under `-o`).
    err: u32,
    /// Working directory for the child, i.e. the worktree root (`None` if bare).
    workdir: Option<PathBuf>,
    /// `PATH` to hand the child, with git's exec-path prepended (`None` = inherit).
    path_env: Option<OsString>,
}

/// `git merge-index` — run a merge for files needing merging.
pub fn merge_index(args: &[String]) -> Result<ExitCode> {
    // git checks this before touching the repository, so an argument-starved
    // invocation is a usage error even outside a repository.
    if args.len() < 3 {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // Positional option scan, mirroring cmd_merge_index exactly: `-o` may only
    // appear first, `-q` only second, and whatever follows is the program.
    let mut i = 1usize;
    let mut one_shot = false;
    let mut quiet = false;
    if args[i] == "-o" {
        one_shot = true;
        i += 1;
    }
    if args[i] == "-q" {
        quiet = true;
        i += 1;
    }
    let Some(pgm) = args.get(i) else {
        // C reads argv[argc] == NULL here; with no paths left there is nothing
        // to merge, so this exits successfully without ever using the program.
        return Ok(ExitCode::SUCCESS);
    };
    i += 1;

    let repo = gix::discover(".")?;
    // A missing index is an empty index, as in repo_read_index().
    let index = repo.index_or_empty()?;
    let state: &gix::index::State = &index;
    let rows: Vec<Row> = state
        .entries()
        .iter()
        .map(|e| Row {
            path: e.path(state).to_owned(),
            stage: e.stage_raw(),
            id: e.id,
            mode: e.mode.bits(),
        })
        .collect();

    let mut ctx = Ctx {
        pgm: OsString::from(pgm),
        one_shot,
        quiet,
        err: 0,
        workdir: repo.workdir().map(Path::to_path_buf),
        path_env: path_with_git_exec_path(),
    };

    let mut force_file = false;
    for arg in &args[i..] {
        let flow = if !force_file && arg.starts_with('-') {
            if arg == "--" {
                force_file = true;
                continue;
            }
            if arg == "-a" {
                merge_all(&mut ctx, &rows)?
            } else {
                eprintln!("fatal: git merge-index: unknown option {arg}");
                Flow::Exit(128)
            }
        } else {
            merge_one_path(&mut ctx, &rows, arg.as_bytes().as_bstr())?
        };
        if let Flow::Exit(code) = flow {
            return Ok(ExitCode::from(code));
        }
    }

    if ctx.err != 0 && !ctx.quiet {
        eprintln!("fatal: merge program failed");
        return Ok(ExitCode::from(128));
    }
    Ok(ExitCode::from((ctx.err & 0xff) as u8))
}

/// `-a`: walk the index and merge every path that carries a non-zero stage.
///
/// Entries for one path are adjacent (the index is sorted by path then stage),
/// so each group is consumed in one `merge_entry` call.
fn merge_all(ctx: &mut Ctx, rows: &[Row]) -> Result<Flow> {
    let mut i = 0usize;
    while i < rows.len() {
        if rows[i].stage == 0 {
            i += 1;
            continue;
        }
        let (found, flow) = merge_entry(ctx, rows, i, rows[i].path.as_bstr())?;
        if let Flow::Exit(code) = flow {
            return Ok(Flow::Exit(code));
        }
        i += found.max(1);
    }
    Ok(Flow::Continue)
}

/// Merge a single named path.
///
/// git looks the path up at stage 0 first: a hit means it is already merged and
/// there is nothing to do, a miss hands the insertion point to `merge_entry`,
/// which fails when no entry of that name exists at any stage.
fn merge_one_path(ctx: &mut Ctx, rows: &[Row], path: &BStr) -> Result<Flow> {
    match rows.iter().position(|r| r.path.as_bstr() == path) {
        None => {
            eprintln!("fatal: git merge-index: {path} not in the cache");
            Ok(Flow::Exit(128))
        }
        // Entries are sorted by path then stage, so a leading stage 0 means the
        // path is already merged.
        Some(pos) if rows[pos].stage == 0 => Ok(Flow::Continue),
        Some(pos) => Ok(merge_entry(ctx, rows, pos, path)?.1),
    }
}

/// Collect the stages of `path` starting at `start` and run the merge program
/// over them. Returns how many index entries were consumed.
fn merge_entry(ctx: &mut Ctx, rows: &[Row], start: usize, path: &BStr) -> Result<(usize, Flow)> {
    // Slot 0 exists only to keep the stage numbering natural; a stage-0 entry
    // can never be reached here, since it terminates the lookup in both callers.
    let mut hex: [String; 4] = Default::default();
    let mut modes: [String; 4] = Default::default();
    let mut found = 0usize;

    for row in rows.iter().skip(start) {
        if row.path.as_bstr() != path {
            break;
        }
        found += 1;
        let stage = row.stage as usize;
        if (1..=3).contains(&stage) {
            hex[stage] = row.id.to_hex().to_string();
            modes[stage] = format!("{:06o}", row.mode);
        }
    }

    if found == 0 {
        eprintln!("fatal: git merge-index: {path} not in the cache");
        return Ok((0, Flow::Exit(128)));
    }

    let mut cmd = Command::new(&ctx.pgm);
    cmd.arg(&hex[1])
        .arg(&hex[2])
        .arg(&hex[3])
        .arg(&*gix::path::from_bstr(path))
        .arg(&modes[1])
        .arg(&modes[2])
        .arg(&modes[3]);
    if let Some(dir) = &ctx.workdir {
        cmd.current_dir(dir);
    }
    if let Some(p) = &ctx.path_env {
        cmd.env("PATH", p);
    }

    let ok = match cmd.status() {
        Ok(status) => status.success(),
        Err(e) => {
            // git's run_command reports this itself, before and independently of
            // the `-q`-suppressible "merge program failed" line.
            eprintln!(
                "fatal: cannot exec '{}': {}",
                ctx.pgm.to_string_lossy(),
                errno_text(&e)
            );
            false
        }
    };

    if ok {
        return Ok((found, Flow::Continue));
    }
    if ctx.one_shot {
        ctx.err += 1;
        return Ok((found, Flow::Continue));
    }
    if !ctx.quiet {
        eprintln!("fatal: merge program failed");
        return Ok((found, Flow::Exit(128)));
    }
    Ok((found, Flow::Exit(1)))
}

/// The bare strerror text, without Rust's trailing ` (os error N)`, so the
/// `cannot exec` line reads the way git's does.
fn errno_text(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(at) => s[..at].to_string(),
        None => s,
    }
}

/// `PATH` with git's exec-path prepended, so helper programs shipped in git's
/// libexec (`git-merge-one-file`) resolve the way they do under stock git.
///
/// Returns `None` when the exec-path cannot be determined, in which case the
/// child simply inherits the ambient `PATH`.
fn path_with_git_exec_path() -> Option<OsString> {
    let exec_path = git_exec_path()?;
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![exec_path];
    dirs.extend(std::env::split_paths(&current));
    std::env::join_paths(dirs).ok()
}

/// git's exec-path: `GIT_EXEC_PATH` when set, else whatever the installed git
/// reports. Any failure here is non-fatal and simply yields `None`.
fn git_exec_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GIT_EXEC_PATH") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let out = Command::new("git").arg("--exec-path").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim_end_matches(['\n', '\r']);
    if text.is_empty() {
        None
    } else {
        Some(PathBuf::from(text))
    }
}
