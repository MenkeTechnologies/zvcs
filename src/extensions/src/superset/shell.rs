//! Shell-convenience verbs for the `zrepl` console — process-state verbs `zcd`,
//! `zpwd`, `zenv`, `zunset`, `zecho`, and native filesystem verbs `zmkdir`,
//! `ztouch`, `zrm`, `zcp`, `zmv`, `zcat`, `zln`. (`zls`, the git-aware listing,
//! lives in [`crate::superset::gitls`].)
//!
//! `zrepl` runs each line as `git <line>` in one long-lived process, so a verb
//! that mutates process state persists across lines: `zcd` navigates like a
//! shell's cd, `zenv NAME=VALUE` sets a variable every later `git` line sees, and
//! `zunset` clears one. `zpwd` and `zecho` round out the basics. The filesystem
//! verbs are the common shell commands, implemented natively (no fork) so the
//! console can create, copy, move, and remove files without leaving it. Outside
//! the console these still run, but the process-state mutators (`zcd`, `zenv`,
//! `zunset`) only affect this process (they cannot change the parent shell).

use anyhow::{bail, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// `git zcd [<dir>]` — change the working directory. No argument goes to `$HOME`;
/// `-` goes to `$OLDPWD` (the previous directory). A leading `~` is expanded to
/// `$HOME`. `OLDPWD`/`PWD` are updated so a subsequent `zcd -` works, exactly as
/// a shell's `cd` does. Nothing is printed on success (shell-like).
pub fn zcd(args: &[String]) -> Result<ExitCode> {
    let old = std::env::current_dir().ok();
    let target: PathBuf = match args.first().map(String::as_str) {
        None | Some("~") => home()?,
        Some("-") => match std::env::var_os("OLDPWD") {
            Some(p) => PathBuf::from(p),
            None => bail!("OLDPWD not set"),
        },
        Some(dir) => expand_tilde(dir)?,
    };

    std::env::set_current_dir(&target)
        .map_err(|e| anyhow::anyhow!("{}: {e}", target.display()))?;

    // Track OLDPWD/PWD so `zcd -` round-trips, matching a shell.
    if let Some(old) = old {
        std::env::set_var("OLDPWD", old);
    }
    if let Ok(now) = std::env::current_dir() {
        std::env::set_var("PWD", now);
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zpwd` — print the current working directory.
pub fn zpwd(_args: &[String]) -> Result<ExitCode> {
    let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("cannot read cwd: {e}"))?;
    println!("{}", cwd.display());
    Ok(ExitCode::SUCCESS)
}

/// `git zenv [NAME=VALUE...|NAME...]` — with no arguments, print every
/// environment variable as `NAME=VALUE`, sorted. A `NAME=VALUE` argument sets
/// that variable (persisting for later `git` lines in the zrepl console); a bare
/// `NAME` prints that variable's value, or nothing if it is unset.
pub fn zenv(args: &[String]) -> Result<ExitCode> {
    if args.is_empty() {
        let mut vars: Vec<(String, String)> = std::env::vars().collect();
        vars.sort();
        for (name, value) in vars {
            println!("{name}={value}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    for arg in args {
        match arg.split_once('=') {
            Some((name, value)) => std::env::set_var(name, value),
            None => {
                if let Ok(value) = std::env::var(arg) {
                    println!("{value}");
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zunset <NAME>...` — remove one or more environment variables.
pub fn zunset(args: &[String]) -> Result<ExitCode> {
    if args.is_empty() {
        bail!("usage: git zunset <NAME>...");
    }
    for name in args {
        std::env::remove_var(name);
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zecho [-n] [<arg>...]` — print the arguments joined by a single space.
/// `-n` as the first argument suppresses the trailing newline. Arguments are
/// printed literally; there is no shell variable or glob expansion.
pub fn zecho(args: &[String]) -> Result<ExitCode> {
    let (newline, rest) = match args.split_first() {
        Some((flag, rest)) if flag == "-n" => (false, rest),
        _ => (true, args),
    };
    let line = rest.join(" ");
    if newline {
        println!("{line}");
    } else {
        print!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `$HOME` as a path, or an error if it is unset.
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME not set"))
}

/// Expand a leading `~` (`~` or `~/rest`) to `$HOME`; other paths pass through.
fn expand_tilde(dir: &str) -> Result<PathBuf> {
    if dir == "~" {
        return home();
    }
    if let Some(rest) = dir.strip_prefix("~/") {
        return Ok(home()?.join(rest));
    }
    Ok(Path::new(dir).to_path_buf())
}

// --- native filesystem verbs ---

/// Split `args` into bundled single-char flags (from `-x` / `-xy` tokens) and the
/// remaining operands, honoring a `--` end-of-flags terminator.
fn split_flags(args: &[String]) -> (String, Vec<&str>) {
    let mut flags = String::new();
    let mut rest = Vec::new();
    let mut only_operands = false;
    for a in args {
        if !only_operands && a == "--" {
            only_operands = true;
        } else if !only_operands && a.len() > 1 && a.starts_with('-') {
            flags.push_str(&a[1..]);
        } else {
            rest.push(a.as_str());
        }
    }
    (flags, rest)
}

/// `git zmkdir [-p] <dir>...` — create directories; `-p` makes parents as needed
/// and does not error if the directory already exists.
pub fn zmkdir(args: &[String]) -> Result<ExitCode> {
    let (flags, dirs) = split_flags(args);
    if dirs.is_empty() {
        bail!("usage: git zmkdir [-p] <dir>...");
    }
    let parents = flags.contains('p');
    for d in dirs {
        let r = if parents { std::fs::create_dir_all(d) } else { std::fs::create_dir(d) };
        r.map_err(|e| anyhow::anyhow!("{d}: {e}"))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `git ztouch <file>...` — create each file if missing, else bump its mtime.
pub fn ztouch(args: &[String]) -> Result<ExitCode> {
    let (_flags, files) = split_flags(args);
    if files.is_empty() {
        bail!("usage: git ztouch <file>...");
    }
    for f in files {
        // create(true) + write(true) without truncate: makes the file if absent,
        // leaves contents intact if present.
        let handle = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(f)
            .map_err(|e| anyhow::anyhow!("{f}: {e}"))?;
        handle
            .set_modified(std::time::SystemTime::now())
            .map_err(|e| anyhow::anyhow!("{f}: {e}"))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zcat <file>...` — write each file's bytes to stdout, in order.
pub fn zcat(args: &[String]) -> Result<ExitCode> {
    let (_flags, files) = split_flags(args);
    if files.is_empty() {
        bail!("usage: git zcat <file>...");
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for f in files {
        let bytes = std::fs::read(f).map_err(|e| anyhow::anyhow!("{f}: {e}"))?;
        out.write_all(&bytes).map_err(|e| anyhow::anyhow!("write: {e}"))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zrm [-r] [-f] <path>...` — remove files, or directories with `-r`. `-f`
/// ignores missing paths. Symlinks are removed, never followed.
pub fn zrm(args: &[String]) -> Result<ExitCode> {
    let (flags, paths) = split_flags(args);
    let recursive = flags.contains('r') || flags.contains('R');
    let force = flags.contains('f');
    if paths.is_empty() {
        if force {
            return Ok(ExitCode::SUCCESS);
        }
        bail!("usage: git zrm [-r] [-f] <path>...");
    }
    for p in paths {
        match std::fs::symlink_metadata(p) {
            Ok(m) if m.is_dir() => {
                if !recursive {
                    bail!("{p}: is a directory (use -r)");
                }
                std::fs::remove_dir_all(p).map_err(|e| anyhow::anyhow!("{p}: {e}"))?;
            }
            Ok(_) => std::fs::remove_file(p).map_err(|e| anyhow::anyhow!("{p}: {e}"))?,
            Err(_) if force => {}
            Err(e) => bail!("{p}: {e}"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zcp [-r] <src>... <dst>` — copy files; `-r` copies directories. Several
/// sources require `<dst>` to be a directory.
pub fn zcp(args: &[String]) -> Result<ExitCode> {
    let (flags, paths) = split_flags(args);
    let recursive = flags.contains('r') || flags.contains('R');
    if paths.len() < 2 {
        bail!("usage: git zcp [-r] <src>... <dst>");
    }
    let (srcs, dst) = paths.split_at(paths.len() - 1);
    apply_to_dst(srcs, Path::new(dst[0]), |src, target| copy_tree(src, target, recursive))
}

/// `git zmv <src>... <dst>` — move/rename; several sources require `<dst>` to be a
/// directory. Falls back to copy+remove across filesystems.
pub fn zmv(args: &[String]) -> Result<ExitCode> {
    let (_flags, paths) = split_flags(args);
    if paths.len() < 2 {
        bail!("usage: git zmv <src>... <dst>");
    }
    let (srcs, dst) = paths.split_at(paths.len() - 1);
    apply_to_dst(srcs, Path::new(dst[0]), move_path)
}

/// `git zln [-s] <target> <link>` — create a hard link, or a symlink with `-s`.
pub fn zln(args: &[String]) -> Result<ExitCode> {
    let (flags, paths) = split_flags(args);
    if paths.len() != 2 {
        bail!("usage: git zln [-s] <target> <link>");
    }
    let (target, link) = (Path::new(paths[0]), Path::new(paths[1]));
    let r = if flags.contains('s') {
        std::os::unix::fs::symlink(target, link)
    } else {
        std::fs::hard_link(target, link)
    };
    r.map_err(|e| anyhow::anyhow!("{}: {e}", link.display()))?;
    Ok(ExitCode::SUCCESS)
}

/// Shared src→dst dispatch for cp/mv: with several sources (or a directory
/// destination) each source lands *inside* `dst` under its own name; with a
/// single source and a non-directory `dst`, `dst` is the new name.
fn apply_to_dst(srcs: &[&str], dst: &Path, op: impl Fn(&Path, &Path) -> Result<()>) -> Result<ExitCode> {
    let dst_is_dir = dst.is_dir();
    if srcs.len() > 1 && !dst_is_dir {
        bail!("target {} is not a directory", dst.display());
    }
    for src in srcs {
        let src = Path::new(src);
        let target = if dst_is_dir {
            match src.file_name() {
                Some(name) => dst.join(name),
                None => bail!("invalid source {}", src.display()),
            }
        } else {
            dst.to_path_buf()
        };
        op(src, &target)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Recursively copy `src` to `dst` — a whole tree when `recursive`, a single file
/// otherwise (a directory without `-r` is an error).
fn copy_tree(src: &Path, dst: &Path, recursive: bool) -> Result<()> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| anyhow::anyhow!("{}: {e}", src.display()))?;
    if meta.is_dir() {
        if !recursive {
            bail!("{}: is a directory (use -r)", src.display());
        }
        std::fs::create_dir_all(dst).map_err(|e| anyhow::anyhow!("{}: {e}", dst.display()))?;
        for entry in std::fs::read_dir(src).map_err(|e| anyhow::anyhow!("{}: {e}", src.display()))? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()), true)?;
        }
    } else {
        std::fs::copy(src, dst).map_err(|e| anyhow::anyhow!("{}: {e}", src.display()))?;
    }
    Ok(())
}

/// Move `src` to `dst`: a rename when possible, else copy the tree and remove the
/// original (crossing filesystems, where `rename` fails with `EXDEV`).
fn move_path(src: &Path, dst: &Path) -> Result<()> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    copy_tree(src, dst, true)?;
    let meta = std::fs::symlink_metadata(src).map_err(|e| anyhow::anyhow!("{}: {e}", src.display()))?;
    if meta.is_dir() {
        std::fs::remove_dir_all(src).map_err(|e| anyhow::anyhow!("{}: {e}", src.display()))?;
    } else {
        std::fs::remove_file(src).map_err(|e| anyhow::anyhow!("{}: {e}", src.display()))?;
    }
    Ok(())
}
