//! `git prune-packed` — remove loose objects that also exist in a pack.
//!
//! Covered: the whole documented option surface — `-n`/`--dry-run`,
//! `-q`/`--quiet`, their `--no-` forms, unambiguous long-option abbreviations
//! (`--dry`, `--q`), clustered short flags (`-nq`), `--`, and `-h`. The dry-run
//! `rm -f <path>` listing is reproduced byte-for-byte, including git's traversal
//! order: fan-out directories are visited by name `00`..`ff`, and entries inside
//! each are visited in raw `readdir` order (git's `for_each_file_in_obj_subdir()`
//! does not sort, and neither does this port). The real run performs the same
//! unlinks and the same trailing `rmdir` of each fan-out directory that
//! `prune_subdir()` does, so post-command repository state matches. Usage text
//! and exit code 129 for `-h`, an unknown option/switch, and a stray positional
//! are reproduced byte-for-byte as well.
//!
//! Membership is decided by `has_object_pack()` semantics: an object is pruned
//! only when a *pack index* contains it — a loose-only object survives — and the
//! packs of every alternate object directory count, both verified against stock
//! git. Local and alternate `objects/pack/*.idx` files are enumerated the way
//! `prepare_packed_git_one()` does, matching `count_objects.rs`.
//!
//! Not covered: the progress meter. Git installs it only when stderr is a tty
//! (`isatty(2)` in `cmd_prune_packed`), so under a pipe — which is how output is
//! compared — it emits nothing; this port emits nothing unconditionally, which
//! makes `-q`/`--no-quiet` accepted but inert. `--help` (which git answers by
//! spawning the man page) bails rather than pretending. The printed path prefix
//! is derived from the discovered repository rather than from git's post-`setup`
//! `GIT_DIR`: a normal worktree yields `.git/objects/...` (identical to git, from
//! the top level or any subdirectory) and a bare repository whose directory is
//! the cwd yields `./objects/...`, but an explicitly relative `GIT_DIR=` or a
//! `.git` *file* pointing outside the worktree yields a cwd-relative or absolute
//! path where git would echo the `GIT_DIR` spelling it was handed. Running
//! outside a repository propagates the discovery error to the central handler
//! rather than emitting git's `fatal: not a git repository` / exit 128, matching
//! every other module in this directory.

use anyhow::{bail, Result};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::odb::pack;

/// Stock git's `prune-packed` usage block, byte-for-byte (127 bytes), including
/// the trailing blank line. Printed on `-h` (stdout) and for a usage error (stderr).
const USAGE: &str = "usage: git prune-packed [-n | --dry-run] [-q | --quiet]\n\
                     \n\
                     \x20   -n, --[no-]dry-run    dry run\n\
                     \x20   -q, --[no-]quiet      be quiet\n\
                     \n";

/// The long options `parse_options()` will resolve an abbreviation against. The
/// `--no-` forms are derived from these, exactly as parse-options does.
const LONG_OPTS: [&str; 2] = ["dry-run", "quiet"];

/// `git prune-packed` — delete loose objects that a pack already holds.
///
/// Supported forms (matching stock git byte-for-byte, including exit codes):
///   * `git prune-packed`                   → unlink duplicates, rmdir emptied fan-outs
///   * `-n` / `--dry-run` / `--no-dry-run`  → list `rm -f <path>` instead of unlinking
///   * `-q` / `--quiet` / `--no-quiet`      → accepted; only ever gated a tty progress meter
///   * `-nq`, `--dry`, `--`                 → clustering, abbreviation, end-of-options
///   * `-h`                                 → usage on stdout, exit 129
pub fn prune_packed(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0; `prune-packed` takes no positional
    // of its own, so dropping a leading copy is unambiguous.
    let args = match args.first().map(String::as_str) {
        Some("prune-packed") => &args[1..],
        _ => args,
    };

    let mut dry_run = false;
    let mut end_of_opts = false;
    // A stray positional is diagnosed only after parsing finishes, because git's
    // `usage_msg_opt()` call sits after `parse_options()` — so `prune-packed foo -h`
    // still prints usage on stdout rather than failing on `foo`.
    let mut extra = false;

    for a in args {
        let a = a.as_str();
        if end_of_opts {
            extra = true;
            continue;
        }
        match a {
            "--" => end_of_opts = true,
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--help" => bail!("--help is not supported (ported: -h, -n/--dry-run, -q/--quiet)"),
            s if s.starts_with("--") => {
                let body = &s[2..];
                // parse-options resolves the positive spelling first, so an input
                // that prefixes both `dry-run` and `no-dry-run` takes the positive.
                if let Some(name) = resolve_long(body) {
                    set_opt(name, true, &mut dry_run);
                } else if let Some(name) = body.strip_prefix("no-").and_then(resolve_long) {
                    set_opt(name, false, &mut dry_run);
                } else {
                    return Ok(usage_error(&format!("error: unknown option `{body}'\n")));
                }
            }
            s if s.len() > 1 && s.starts_with('-') => {
                // Clustered short switches, e.g. `-nq`.
                for c in s[1..].chars() {
                    match c {
                        'n' => dry_run = true,
                        // OPT_NEGBIT: `-q` only ever cleared the progress bit.
                        'q' => {}
                        'h' => {
                            print!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => return Ok(usage_error(&format!("error: unknown switch `{c}'\n"))),
                    }
                }
            }
            _ => extra = true,
        }
    }

    if extra {
        // `usage_msg_opt()` prefixes a `fatal:` line and a blank line.
        return Ok(usage_error("fatal: too many arguments\n\n"));
    }

    let repo = gix::discover(".")?;
    let hash = repo.object_hash();
    let objdir = repo.objects.store_ref().path().to_path_buf();
    let shown_objdir = display_objdir(&repo, &objdir);

    // Every pack index reachable from this repository — local first, then each
    // alternate — since `has_object_pack()` walks `get_all_packs()`.
    let mut indices: Vec<pack::index::File> = pack_indices(&objdir, hash);
    for alt in repo.objects.store_ref().alternate_db_paths()? {
        indices.extend(pack_indices(&alt, hash));
    }

    // `for_each_loose_file_in_objdir()` walks the 256 `00`..`ff` fan-out
    // directories by name, so `info/`, `pack/` and anything else directly under
    // `objects/` is never visited at all.
    let name_len = hash.len_in_hex() - 2;
    let mut out = String::new();

    for fanout in 0u16..256 {
        let prefix = format!("{fanout:02x}");
        let sub = objdir.join(&prefix);
        let Ok(entries) = fs::read_dir(&sub) else {
            // opendir failure (normally ENOENT) skips the subdir callback too,
            // so an absent fan-out directory is not rmdir'd.
            continue;
        };

        // Deliberately unsorted: git consumes `readdir()` output in stream order
        // and the dry-run listing inherits it.
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            // `hex_to_bytes()` accepts either case; anything else is handed to a
            // cruft callback that `prune-packed` does not install, i.e. ignored.
            if name.len() != name_len || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let Ok(oid) = ObjectId::from_hex(format!("{prefix}{name}").to_lowercase().as_bytes())
            else {
                continue;
            };
            if !indices.iter().any(|f| f.lookup(oid).is_some()) {
                continue;
            }

            let shown = shown_objdir.join(&prefix).join(&name);
            if dry_run {
                out.push_str(&format!("rm -f {}\n", shown.display()));
            } else {
                unlink_or_warn(&sub.join(&name));
            }
        }

        // `prune_subdir()`: drop the fan-out directory once emptied. Fails
        // harmlessly (ENOTEMPTY) when unrelated files remain, exactly as rmdir does.
        if !dry_run {
            let _ = fs::remove_dir(&sub);
        }
    }

    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

/// Resolve a long-option body to its canonical name, accepting any unambiguous
/// prefix the way parse-options does. `dry-run` and `quiet` share no prefix, so
/// a match is always unique.
fn resolve_long(body: &str) -> Option<&'static str> {
    if body.is_empty() {
        return None;
    }
    LONG_OPTS.iter().copied().find(|n| n.starts_with(body))
}

/// Apply a resolved long option. `quiet` only ever toggled the progress bit,
/// which this port never renders, so it has no observable effect.
fn set_opt(name: &str, value: bool, dry_run: &mut bool) {
    if name == "dry-run" {
        *dry_run = value;
    }
}

/// git's parse-options failure shape: the caller's `error:`/`fatal:` preamble
/// followed by the usage block, both on stderr, exit 129.
fn usage_error(preamble: &str) -> ExitCode {
    eprint!("{preamble}{USAGE}");
    ExitCode::from(129)
}

/// Open every `.idx` in `<objdir>/pack` whose `.pack` sibling is a readable
/// regular file, as `prepare_packed_git_one()` does. A corrupt or unsupported
/// index is skipped rather than fatal, matching `open_pack_index()`.
fn pack_indices(objdir: &Path, hash: gix::hash::Kind) -> Vec<pack::index::File> {
    let dir = objdir.join("pack");
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();

    let mut out = Vec::new();
    for name in names {
        let Some(base) = name.strip_suffix(".idx") else {
            continue;
        };
        match fs::metadata(dir.join(format!("{base}.pack"))) {
            Ok(md) if md.is_file() => {}
            _ => continue,
        }
        if let Ok(file) = pack::index::File::at(dir.join(&name), hash) {
            out.push(file);
        }
    }
    out
}

/// The prefix git prints in front of `<xx>/<38 hex>`.
///
/// Git echoes `get_object_directory()`, which for a normal repository entered
/// from anywhere in the worktree is the top-level-relative `.git/objects`, and
/// for a bare repository opened in place is `./objects`. Both are reproduced;
/// anything else falls back to a cwd-relative or absolute path.
fn display_objdir(repo: &gix::Repository, objdir: &Path) -> PathBuf {
    let real_objdir = fs::canonicalize(objdir).unwrap_or_else(|_| objdir.to_path_buf());

    if let Some(work) = repo.workdir() {
        let real_work = fs::canonicalize(work).unwrap_or_else(|_| work.to_path_buf());
        if let Ok(rel) = real_objdir.strip_prefix(&real_work) {
            return rel.to_path_buf();
        }
    }

    let Ok(cwd) = env::current_dir() else {
        return real_objdir;
    };
    let real_cwd = fs::canonicalize(&cwd).unwrap_or(cwd);
    // A bare repository opened as `.` gives git the object directory `./objects`.
    if real_objdir.parent() == Some(real_cwd.as_path()) {
        return PathBuf::from(".").join(real_objdir.file_name().unwrap_or_default());
    }
    match real_objdir.strip_prefix(&real_cwd) {
        Ok(rel) => rel.to_path_buf(),
        Err(_) => real_objdir,
    }
}

/// `unlink_or_warn()`: remove the file, staying silent on ENOENT and otherwise
/// warning with git's `unable to unlink '<path>': <strerror>` wording.
fn unlink_or_warn(path: &Path) {
    let Err(e) = fs::remove_file(path) else {
        return;
    };
    if e.kind() == std::io::ErrorKind::NotFound {
        return;
    }
    eprintln!("warning: unable to unlink '{}': {}", path.display(), strerror(&e));
}

/// The bare `strerror()` text, dropping the ` (os error N)` suffix Rust appends
/// to OS errors so the warning reads exactly as git's does.
fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}
