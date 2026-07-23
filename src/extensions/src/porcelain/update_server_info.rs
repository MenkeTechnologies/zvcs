//! `git update-server-info` — regenerate the auxiliary files a dumb HTTP/FTP
//! server needs to serve a repository.
//!
//! Covered: the whole documented surface — `-f`/`--force`/`--no-force`, their
//! parse-options abbreviations (`--forc`, `--no-f`, …), `--`, `-h`, and the
//! usage text plus exit code 129 for a bad invocation. The command prints
//! nothing on success, exits 0, and rewrites exactly the two files git names:
//!
//!   * `$GIT_OBJECT_DIRECTORY/info/packs` — a `P <pack>.pack` line per local
//!     pack followed by a single empty line, as `write_pack_info_file()` emits.
//!   * `$GIT_COMMON_DIR/info/refs` — `<oid> TAB <refname> LF` for every ref
//!     under `refs/` in name order, with an extra `<peeled-oid> TAB
//!     <refname>^{} LF` line after any ref whose object is a tag, exactly as
//!     `add_info_ref()` does.
//!
//! Both are written through `update_info_file()`'s contract: without `--force`
//! a file whose contents already match is left untouched (mtime preserved), and
//! the pack list keeps the order of the packs already named in the old file,
//! dropping stale entries and appending newly-seen packs after them. With
//! `--force` the old file is ignored and the list is rebuilt from scratch.
//!
//! Packs are enumerated the way `add_packed_git()` accepts them: every `*.idx`
//! in `objects/pack` whose `.pack` sibling is a readable regular file. Packs
//! reachable only through alternates are excluded, matching git's `pack_local`
//! filter.
//!
//! Not covered exactly: git orders packs that appear in neither the old file nor
//! a fixed position by `struct packed_git` pointer value — its own source calls
//! that order arbitrary ("then it does not matter but at least keep the
//! comparison stable"). This port orders such newcomers by file name, which is
//! deterministic but can differ from git when two or more previously-unlisted
//! packs are written in the same run. Everything else — the single-pack case,
//! the "preserve existing order" case, and every `info/refs` byte — matches.
//! `core.sharedRepository` permission widening is not applied to the two files;
//! they are created with the process umask, which is what git produces for the
//! default (unshared) configuration.
//!
//! The failure path (`error: unable to update <path>: <reason>`, exit 1, file
//! left as it was) is reproduced for the case that triggers it in practice — a
//! ref pointing at an object that is not in the object database. The path is
//! printed relative to the current directory when it lies below it, which
//! matches git run from the top level of a repository; from a subdirectory git
//! prints its own `$GIT_DIR`-prefixed form instead.

use anyhow::Result;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::Kind;

/// Stock git's usage block, byte-for-byte (108 bytes) including the trailing
/// blank line. Printed on `-h` (stdout) and for a usage error (stderr).
const USAGE: &str = "usage: git update-server-info [-f | --force]\n\
                     \n\
                     \x20   -f, --[no-]force      update the info files from scratch\n\
                     \n";

/// The reason string git ends up printing when ref generation aborts because an
/// object is missing: `parse_object()` leaves `ENOENT` behind and
/// `update_info_file()` reports it through `error_errno()`.
const ENOENT: &str = "No such file or directory";

/// `git update-server-info` — refresh `objects/info/packs` and `info/refs`.
///
/// Supported forms (matching stock git byte-for-byte, including exit codes and
/// the resulting file contents):
///   * `git update-server-info`                → refresh both files if stale
///   * `-f` / `--force` / `--no-force`         → rebuild from scratch
///   * abbreviations of the long forms, `--`, `-h`
pub fn update_server_info(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0; this command takes no positional of
    // its own, so dropping a leading copy is unambiguous.
    let args = match args.first().map(String::as_str) {
        Some("update-server-info") => &args[1..],
        _ => args,
    };

    let mut force = false;
    let mut end_of_opts = false;

    for a in args {
        let a = a.as_str();
        if end_of_opts {
            // Any positional is a usage error with no `error:` line — git goes
            // straight to `usage_with_options()`.
            return Ok(usage_error(None));
        }
        match a {
            "--" => end_of_opts = true,
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with("--") => {
                let name = &s[2..];
                // parse-options accepts any unambiguous prefix, and `--no-` in
                // front of it for the negated form.
                if let Some(rest) = name.strip_prefix("no-") {
                    if is_abbrev(rest, "force") {
                        force = false;
                        continue;
                    }
                }
                if is_abbrev(name, "force") {
                    force = true;
                } else if is_abbrev(name, "no-force") {
                    force = false;
                } else {
                    return Ok(usage_error(Some(&format!("unknown option `{name}'"))));
                }
            }
            s if s.len() > 1 && s.starts_with('-') => {
                // Clustered short switches; only `-f` and `-h` exist.
                for c in s[1..].chars() {
                    match c {
                        'f' => force = true,
                        'h' => {
                            print!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => return Ok(usage_error(Some(&format!("unknown switch `{c}'")))),
                    }
                }
            }
            // A bare `-` and every other word is a non-option argument, which
            // `cmd_update_server_info()` rejects outright.
            _ => return Ok(usage_error(None)),
        }
    }

    let repo = gix::discover(".")?;
    let objdir = repo.objects.store_ref().path().to_path_buf();

    // `update_server_info()` runs both updaters unconditionally and ORs their
    // status, so a failing `info/refs` still leaves a refreshed pack list.
    let mut errs = false;

    let packs_path = objdir.join("info").join("packs");
    let packs = info_packs(&objdir, force);
    if let Err(reason) = write_info_file(&packs_path, &packs, force) {
        report(&packs_path, &reason);
        errs = true;
    }

    let refs_path = repo.common_dir().join("info").join("refs");
    match info_refs(&repo) {
        Err(reason) => {
            report(&refs_path, &reason);
            errs = true;
        }
        Ok(content) => {
            if let Err(reason) = write_info_file(&refs_path, &content, force) {
                report(&refs_path, &reason);
                errs = true;
            }
        }
    }

    Ok(if errs {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// git's parse-options failure shape: an optional `error: <msg>` line followed
/// by the usage block, both on stderr, exit 129.
fn usage_error(msg: Option<&str>) -> ExitCode {
    match msg {
        Some(m) => eprint!("error: {m}\n{USAGE}"),
        None => eprint!("{USAGE}"),
    }
    ExitCode::from(129)
}

/// Whether `given` is a non-empty prefix of `full`, which is all the
/// disambiguation parse-options needs here — this command has a single option.
fn is_abbrev(given: &str, full: &str) -> bool {
    !given.is_empty() && full.starts_with(given)
}

/// `error_errno("unable to update %s", path)` — the path shortened to a
/// cwd-relative form when it lies below the current directory.
fn report(path: &Path, reason: &str) {
    let shown = std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| path.to_path_buf());
    eprintln!("error: unable to update {}: {reason}", shown.display());
}

/// Render `objects/info/packs`: one `P <name>.pack` line per local pack, then a
/// single empty line, mirroring `write_pack_info_file()`.
///
/// Ordering follows `compare_info()`: packs already named in the previous file
/// keep their relative order and come first, packs that have since disappeared
/// are dropped, and the rest follow. `--force` skips reading the old file
/// entirely, exactly as `update_info_packs()` does.
fn info_packs(objdir: &Path, force: bool) -> String {
    let present = local_packs(objdir);
    let mut ordered: Vec<String> = Vec::new();

    if !force {
        if let Ok(old) = fs::read_to_string(objdir.join("info").join("packs")) {
            for line in old.lines() {
                let Some(name) = line.strip_prefix("P ") else {
                    continue;
                };
                if present.iter().any(|p| p == name) && !ordered.iter().any(|p| p == name) {
                    ordered.push(name.to_string());
                }
            }
        }
    }
    for name in &present {
        if !ordered.iter().any(|p| p == name) {
            ordered.push(name.clone());
        }
    }

    let mut out = String::new();
    for name in &ordered {
        out.push_str("P ");
        out.push_str(name);
        out.push('\n');
    }
    out.push('\n');
    out
}

/// The `<name>.pack` basenames of every pack in this object directory, in file
/// name order. A pack counts when `<name>.idx` exists and its `.pack` sibling is
/// a regular file — the pair `add_packed_git()` insists on. Alternates are not
/// scanned, matching git's `pack_local` filter.
fn local_packs(objdir: &Path) -> Vec<String> {
    let dir = objdir.join("pack");
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();

    names
        .iter()
        .filter_map(|name| {
            let base = name.strip_suffix(".idx")?;
            let pack = format!("{base}.pack");
            match fs::metadata(dir.join(&pack)) {
                Ok(md) if md.is_file() => Some(pack),
                _ => None,
            }
        })
        .collect()
}

/// Render `info/refs`: `<oid> TAB <refname> LF` for every ref under `refs/`, in
/// name order, plus a `<peeled-oid> TAB <refname>^{} LF` line whenever the ref's
/// object is a tag — `add_info_ref()` verbatim.
///
/// A ref naming an object that is not in the database aborts generation with the
/// errno git would have reported, so the file is left untouched and the command
/// exits 1. Refs that cannot be resolved at all are skipped, as git's ref
/// iteration drops broken refs before the callback ever sees them.
fn info_refs(repo: &gix::Repository) -> Result<String, String> {
    let platform = repo.references().map_err(|e| e.to_string())?;
    let iter = platform.all().map_err(|e| e.to_string())?;

    let mut rows: Vec<(String, ObjectId, Option<ObjectId>)> = Vec::new();
    for reference in iter {
        let Ok(mut reference) = reference else { continue };
        let name = reference.name().as_bstr().to_string();
        // `refs_for_each_ref()` is `for_each_ref_in("refs/")`, so HEAD and the
        // other pseudo-refs never appear.
        if !name.starts_with("refs/") {
            continue;
        }
        let Ok(id) = reference.follow_to_object() else {
            continue;
        };
        let id = id.detach();

        // `parse_object()`: a ref pointing at a missing object is fatal here.
        let kind = repo.find_header(id).map_err(|_| ENOENT.to_string())?.kind();
        // `deref_tag()` walks tag targets until a non-tag is reached; a tag that
        // cannot be followed simply yields no `^{}` line.
        let peeled = if kind == Kind::Tag {
            repo.find_object(id)
                .ok()
                .and_then(|o| o.peel_tags_to_end().ok())
                .map(|o| o.id)
        } else {
            None
        };
        rows.push((name, id, peeled));
    }

    rows.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let mut out = String::new();
    for (name, id, peeled) in &rows {
        out.push_str(&format!("{id}\t{name}\n"));
        if let Some(peeled) = peeled {
            out.push_str(&format!("{peeled}\t{name}^{{}}\n"));
        }
    }
    Ok(out)
}

/// `update_info_file()`: leave a file whose contents already match alone unless
/// `--force` was given, otherwise replace it atomically through a sibling temp
/// file, creating the `info/` directory if it is missing.
fn write_info_file(path: &Path, content: &str, force: bool) -> Result<(), String> {
    if !force {
        if let Ok(old) = fs::read_to_string(path) {
            if old == content {
                return Ok(());
            }
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(errno_string)?;
    }

    let tmp = temp_path(path);
    let written = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(errno_string(e));
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(errno_string(e));
    }
    Ok(())
}

/// git's `%s_XXXXXX` sibling temp name, made unique by the process id rather
/// than by `mkstemp` — the file is renamed over the target immediately.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{name}_{}", std::process::id()))
}

/// `strerror(errno)` as git prints it — Rust appends ` (os error N)`, which git
/// does not.
fn errno_string(e: std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}
