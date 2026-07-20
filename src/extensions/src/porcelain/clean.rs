use anyhow::{Result, bail};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};

/// `git clean` — remove untracked files from the working tree.
///
/// Backed by gitoxide's directory walk (`Repository::dirwalk_iter`) configured
/// for deletion, which reproduces git's own `dir.c` collapsing rules: a wholly
/// untracked directory folds into a single entry, while a directory that also
/// holds files we are *not* about to delete (e.g. ignored files without `-x`)
/// stays expanded so only the deletable leaves are reported.
///
/// Supported invocations (stdout, exit code and resulting worktree state match
/// stock `git clean`):
///   * `-n`/`--dry-run`   — list as `Would remove <path>` without touching disk.
///   * `-f`/`--force`     — actually delete; `-ff` also deletes nested
///                          repositories, which a single `-f` refuses to touch.
///   * `-d`               — recurse into untracked directories and remove them
///                          (including empty ones) as single entries.
///   * `-q`/`--quiet`     — suppress the per-path lines, keep warnings.
///   * `-x`               — also remove ignored files.
///   * `-X`               — remove *only* ignored files.
///   * `--` and `<pathspec>...` — as with git, any pathspec implies `-d`.
///   * grouped short flags (`-ndx`, `-ffd`, …).
///
/// Paths are sorted by their repository-relative form (directories carrying a
/// trailing `/`) and then rendered relative to the current working directory,
/// C-quoted exactly as git's `quote_path` does.
///
/// `clean.requireForce` is honoured: without `-f` and without `-n` the command
/// prints git's `fatal:` line and exits 128. A removal failure warns on stderr
/// and yields exit code 1.
///
/// Faithfully unsupported — these `bail!` rather than emit wrong results:
/// `-i`/`--interactive` (git's own prompt loop), `-e`/`--exclude=<pattern>`
/// (the vendored `dirwalk` entry points expose no override for the exclude
/// stack), and running from inside a directory that is itself a deletion
/// candidate, where git prints an unsorted, readdir-ordered `./`-prefixed
/// listing after `Refusing to remove current working directory`.
pub fn clean(args: &[String]) -> Result<ExitCode> {
    let mut dry_run = false;
    let mut force = 0usize;
    let mut remove_directories = false;
    let mut quiet = false;
    let mut ignored_too = false; // -x
    let mut ignored_only = false; // -X
    let mut pathspecs: Vec<BString> = Vec::new();
    let mut no_more_opts = false;

    for a in args.iter() {
        if !no_more_opts && a == "--" {
            no_more_opts = true;
            continue;
        }
        if !no_more_opts && a.len() > 1 && a.starts_with('-') {
            if let Some(long) = a.strip_prefix("--") {
                match long {
                    "dry-run" => dry_run = true,
                    "force" => force += 1,
                    "quiet" => quiet = true,
                    "interactive" => bail!(
                        "unsupported flag {a:?} (ported: -n, -f, -d, -q, -x, -X, --, <pathspec>)"
                    ),
                    _ if long == "exclude" || long.starts_with("exclude=") => bail!(
                        "unsupported flag {a:?}: extra exclude patterns need an override for the dirwalk exclude stack, which the vendored gix entry points do not expose"
                    ),
                    _ => bail!(
                        "unsupported flag {a:?} (ported: -n, -f, -d, -q, -x, -X, --, <pathspec>)"
                    ),
                }
            } else {
                for c in a[1..].chars() {
                    match c {
                        'n' => dry_run = true,
                        'f' => force += 1,
                        'd' => remove_directories = true,
                        'q' => quiet = true,
                        'x' => ignored_too = true,
                        'X' => ignored_only = true,
                        'i' => bail!(
                            "unsupported flag \"-i\" (ported: -n, -f, -d, -q, -x, -X, --, <pathspec>)"
                        ),
                        'e' => bail!(
                            "unsupported flag \"-e\": extra exclude patterns need an override for the dirwalk exclude stack, which the vendored gix entry points do not expose"
                        ),
                        _ => bail!(
                            "unsupported flag \"-{c}\" (ported: -n, -f, -d, -q, -x, -X, --, <pathspec>)"
                        ),
                    }
                }
            }
            continue;
        }
        pathspecs.push(BString::from(a.clone().into_bytes()));
    }

    if ignored_too && ignored_only {
        eprintln!("fatal: options '-x' and '-X' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    // With a pathspec, git removes everything it matches, directories included.
    if !pathspecs.is_empty() {
        remove_directories = true;
    }

    let repo = gix::discover(".")?;

    if !dry_run && force == 0 && repo.config_snapshot().boolean("clean.requireForce") != Some(false)
    {
        eprintln!("fatal: clean.requireForce is true and -f not given: refusing to clean");
        return Ok(ExitCode::from(128));
    }

    // The prefix is the repo-relative current directory; it scopes the walk when
    // no pathspec is given, and every reported path is rendered relative to it.
    let prefix: BString = repo
        .prefix()?
        .map(|p| gix::path::to_unix_separators_on_windows(gix::path::into_bstr(p)).into_owned())
        .unwrap_or_default();
    let prefix_parts: Vec<&[u8]> = prefix
        .split(|b| *b == b'/')
        .filter(|c| !c.is_empty())
        .collect();

    let index = repo.index_or_load_from_head_or_empty()?;

    // A directory only exists in the worktree because it holds tracked files or
    // because it is untracked/ignored; if nothing tracked lives under the prefix
    // the current directory is itself a deletion candidate, which git reports in
    // a shape we do not reproduce.
    if !prefix_parts.is_empty() {
        let mut under_prefix = prefix.clone();
        if under_prefix.last() != Some(&b'/') {
            under_prefix.push(b'/');
        }
        let backing = index.path_backing();
        let any_tracked = index
            .entries()
            .iter()
            .any(|e| e.path_in(backing).starts_with_str(&under_prefix));
        if !any_tracked {
            bail!(
                "cleaning from inside a directory that is itself a deletion candidate is not supported"
            );
        }
    }

    // Emission modes, chosen to mirror git's `dir.c` flags for each combination:
    //   * `-X` keeps untracked entries un-collapsed so an untracked directory
    //     never swallows the ignored files inside it (which are the targets).
    //   * `for_deletion` is only set with `-d`; it is what stops a directory
    //     from collapsing when it also holds files we would not delete, so that
    //     `git clean -nd` reports `dir/file` instead of `dir/`.
    let mut options = repo
        .dirwalk_options()?
        .empty_patterns_match_prefix(true)
        .emit_untracked(if ignored_only {
            gix::dir::walk::EmissionMode::Matching
        } else {
            gix::dir::walk::EmissionMode::CollapseDirectory
        })
        .emit_ignored(
            (ignored_too || ignored_only)
                .then_some(gix::dir::walk::EmissionMode::CollapseDirectory),
        )
        .emit_empty_directories(remove_directories);
    options = options.for_deletion(
        remove_directories
            .then_some(gix::dir::walk::ForDeletionMode::IgnoredDirectoriesCanHideNestedRepositories),
    );

    // (sort key = repo-relative path with a trailing '/' for directories, repo-relative path, is_dir)
    let mut targets: Vec<(BString, BString, bool)> = Vec::new();

    let mut iter = repo.dirwalk_iter(index, pathspecs, Default::default(), options)?;
    for item in iter.by_ref() {
        let entry = item?.entry;

        match entry.status {
            gix::dir::entry::Status::Pruned | gix::dir::entry::Status::Tracked => continue,
            gix::dir::entry::Status::Untracked if ignored_only => continue,
            gix::dir::entry::Status::Ignored(_) if !(ignored_too || ignored_only) => continue,
            _ => {}
        }
        if entry.property == Some(gix::dir::entry::Property::EmptyDirectoryAndCWD) {
            bail!(
                "cleaning from inside a directory that is itself a deletion candidate is not supported"
            );
        }

        let is_repo = entry.disk_kind == Some(gix::dir::entry::Kind::Repository);
        let is_dir = is_repo || entry.disk_kind == Some(gix::dir::entry::Kind::Directory);
        if is_dir && !remove_directories {
            continue;
        }
        // A nested repository is only removed with a second -f, as in git.
        if is_repo && force < 2 {
            continue;
        }

        let mut key = entry.rela_path.clone();
        if is_dir {
            key.push(b'/');
        }
        targets.push((key, entry.rela_path, is_dir));
    }

    targets.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = String::new();
    let mut failed = false;
    for (key, rela_path, is_dir) in targets {
        let shown = quote_path(relative_to_prefix(key.as_bstr(), &prefix_parts));

        if dry_run {
            if !quiet {
                out.push_str(&format!("Would remove {shown}\n"));
            }
            continue;
        }

        let Some(abs) = repo.workdir_path(&rela_path) else {
            continue;
        };
        let res = if is_dir {
            std::fs::remove_dir_all(&abs)
        } else {
            std::fs::remove_file(&abs)
        };
        match res {
            Ok(()) => {
                if !quiet {
                    out.push_str(&format!("Removing {shown}\n"));
                }
            }
            Err(err) => {
                print!("{out}");
                out.clear();
                eprintln!("warning: failed to remove {shown}: {}", errno_text(&err));
                failed = true;
            }
        }
    }
    print!("{out}");

    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Render a repository-relative path as git does for display: relative to the
/// current working directory, walking up with `../` for each prefix component
/// the path does not share.
fn relative_to_prefix(path: &BStr, prefix_parts: &[&[u8]]) -> BString {
    let comps: Vec<&[u8]> = path.split(|b| *b == b'/').collect();
    let mut shared = 0;
    while shared < prefix_parts.len() && shared < comps.len() && prefix_parts[shared] == comps[shared]
    {
        shared += 1;
    }

    let mut outp = BString::default();
    for _ in shared..prefix_parts.len() {
        outp.extend_from_slice(b"../");
    }
    for (i, c) in comps[shared..].iter().enumerate() {
        if i > 0 {
            outp.push(b'/');
        }
        outp.extend_from_slice(c);
    }
    outp
}

/// The message text of an I/O error without Rust's ` (os error N)` suffix, so
/// the warning reads like git's `strerror` output.
fn errno_text(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        // All bytes are printable ASCII here, so this is lossless.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => {
                out.push_str(&format!("\\{b:03o}"));
            }
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}
