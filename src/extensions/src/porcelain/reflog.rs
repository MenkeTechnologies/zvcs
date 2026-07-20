use anyhow::{anyhow, bail, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;

/// `git reflog` — read the reference logs recorded under `$GIT_DIR/logs`.
///
/// Backed by gitoxide's `gix_ref` reflog reader (`Reference::log_iter()`), which
/// parses the raw `<old> <new> <sig>\t<message>` lines, plus a direct walk of the
/// log directory for the subcommands that are defined in terms of the files
/// themselves.
///
/// Ported subcommands (stdout is byte-identical to stock git for these):
///   * `git reflog [show] [<options>] [<ref>...]` — `show` is the default, and a
///     missing `<ref>` defaults to `HEAD`. Each entry prints as
///     `<abbrev-oid> <ref-as-typed>@{<n>}: <message>`, newest first, exactly as
///     `git log -g --abbrev-commit --pretty=oneline` renders it.
///     A `<ref>@{<n>}` argument starts the listing at entry `n` and keeps
///     numbering from `n`, matching git.
///   * `git reflog list`   — every ref that has a reflog, in git's directory-tree
///     order (per-directory name sort, so `refs/heads/a/b` precedes `refs/heads/a-c`).
///   * `git reflog exists <ref>` — exit 0 if `$GIT_DIR/logs/<ref>` is a file, else 1.
///     Like git this is a literal path test, so `exists master` is 1 while
///     `exists refs/heads/master` is 0.
///
/// Exit codes follow stock git: 128 for the `fatal:` paths (unknown revision,
/// out-of-range `@{n}`, malformed refname), 129 for the `exists` usage error,
/// 1 for `exists` on a ref without a reflog.
///
/// Ported `show` options: `-n <n>`, `-n<n>`, `-<n>`, `--max-count=<n>` (a single
/// budget shared across all `<ref>` arguments, as in git). Every other option
/// bails rather than being ignored.
///
/// Not ported, and rejected with a precise reason instead of wrong output:
///   * `write`, `delete`, `drop`, `expire` — these rewrite or truncate log files;
///     `gix-ref` only appends to a reflog as a side effect of a ref transaction
///     and exposes no rewrite/truncate/expiry API.
///   * `--all` for `show` — git orders and de-duplicates that walk through the
///     revision machinery, which this direct log reader does not reproduce.
///   * date-based selectors (`<ref>@{yesterday}`), the bare `@{<n>}` form,
///     pathspecs, and every `git log` formatting option other than the count limit.
///
/// One known divergence: when a reflog entry names an object that is no longer in
/// the odb, gitoxide's disambiguating `shorten()` fails and the id is abbreviated
/// to the plain `core.abbrev` length ([`abbrev_len`]) without git's uniqueness
/// extension. Entries whose objects are present abbreviate identically to git.
pub fn reflog(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand being present at index 0 regardless of how the
    // dispatcher slices argv.
    let args: &[String] = match args.first() {
        Some(a) if a == "reflog" => &args[1..],
        _ => args,
    };

    let (sub, rest): (&str, &[String]) = match args.first().map(String::as_str) {
        Some("show") => ("show", &args[1..]),
        Some("list") => ("list", &args[1..]),
        Some("exists") => ("exists", &args[1..]),
        Some(s @ ("write" | "delete" | "drop" | "expire")) => bail!(
            "`reflog {s}` is not ported: gix-ref appends to a reflog only as part of a \
             ref transaction and exposes no API to write standalone entries, rewrite, \
             truncate or expire a log"
        ),
        // Anything else is a `<ref>` for the implicit `show`.
        _ => ("show", args),
    };

    let repo = gix::discover(".")?;
    match sub {
        "show" => show(&repo, rest),
        "list" => list(&repo, rest),
        "exists" => exists(&repo, rest),
        _ => unreachable!("subcommand set is closed above"),
    }
}

/// `git reflog show` — render the log of each `<ref>` (default `HEAD`).
fn show(repo: &gix::Repository, rest: &[String]) -> Result<ExitCode> {
    let mut max_count: Option<usize> = None;
    let mut specs: Vec<&str> = Vec::new();

    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        match a {
            "-n" | "--max-count" => {
                i += 1;
                let n = rest
                    .get(i)
                    .ok_or_else(|| anyhow!("option `{a}` requires a value"))?;
                max_count = Some(parse_count(n, a)?);
            }
            "--" => bail!("pathspec filtering is not supported"),
            s if s.starts_with("--max-count=") => {
                max_count = Some(parse_count(&s["--max-count=".len()..], "--max-count")?);
            }
            s if s.len() > 2 && s.starts_with("-n") && all_digits(&s[2..]) => {
                max_count = Some(parse_count(&s[2..], "-n")?);
            }
            s if s.len() > 1 && s.starts_with('-') && all_digits(&s[1..]) => {
                max_count = Some(parse_count(&s[1..], "-<n>")?);
            }
            s if s.starts_with('-') => bail!(
                "unsupported flag {s:?} (ported: -n <n>, -n<n>, -<n>, --max-count=<n>)"
            ),
            s => specs.push(s),
        }
        i += 1;
    }

    // Bare `git reflog` on an unborn HEAD has its own fatal message in git,
    // distinct from the "ambiguous argument" one an explicit `HEAD` produces.
    let defaulted = specs.is_empty();
    if defaulted {
        if let Ok(head) = repo.head() {
            if head.is_unborn() {
                let branch = head
                    .referent_name()
                    .map(|n| n.shorten().to_str_lossy().into_owned())
                    .unwrap_or_else(|| "master".to_owned());
                eprintln!("fatal: your current branch '{branch}' does not have any commits yet");
                return Ok(ExitCode::from(128));
            }
        }
        specs.push("HEAD");
    }

    let fallback_len = abbrev_len(repo);
    let mut remaining = max_count.unwrap_or(usize::MAX);
    let mut out: Vec<u8> = Vec::new();

    for spec in specs {
        let (base, selector) = split_selector(spec)?;

        // Read the whole log, then flip it: the file is oldest-first, git prints
        // newest-first with @{0} as the most recent entry.
        let mut entries: Vec<(ObjectId, Vec<u8>)> = Vec::new();
        let mut has_reflog = false;
        if let Some(reference) = repo.try_find_reference(base).ok().flatten() {
            let mut platform = reference.log_iter();
            if let Some(iter) = platform.all()? {
                has_reflog = true;
                for line in iter {
                    let line = line.map_err(|e| anyhow!("{base}: bad reflog line: {e}"))?;
                    entries.push((line.new_oid(), line.message.to_vec()));
                }
            }
        }
        entries.reverse();

        let start = match selector {
            // `<ref>@{n}` is resolved by git's revision parser, which needs the
            // reflog to exist and to be long enough.
            Some(n) => {
                if !has_reflog || entries.is_empty() {
                    return Ok(fatal_ambiguous(spec));
                }
                if n >= entries.len() {
                    eprintln!("fatal: log for '{base}' only has {} entries", entries.len());
                    return Ok(ExitCode::from(128));
                }
                n
            }
            // Without a selector a ref that simply has no log prints nothing, but
            // an argument that is not a revision at all is fatal.
            None => {
                if !has_reflog {
                    if repo.rev_parse_single(base).is_err() {
                        return Ok(fatal_ambiguous(spec));
                    }
                    continue;
                }
                0
            }
        };

        for (n, (id, message)) in entries.iter().enumerate().skip(start) {
            if remaining == 0 {
                break;
            }
            out.extend_from_slice(short_id(repo, *id, fallback_len).as_bytes());
            out.push(b' ');
            out.extend_from_slice(base.as_bytes());
            out.extend_from_slice(format!("@{{{n}}}: ").as_bytes());
            out.extend_from_slice(message);
            out.push(b'\n');
            remaining -= 1;
        }
    }

    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// `git reflog list` — every ref under `$GIT_DIR/logs` that owns a log file.
fn list(repo: &gix::Repository, rest: &[String]) -> Result<ExitCode> {
    if let Some(a) = rest.first() {
        bail!("unsupported argument {a:?} for `reflog list`");
    }
    if repo.git_dir() != repo.common_dir() {
        bail!("`reflog list` from a linked worktree is not supported");
    }

    let mut names: Vec<String> = Vec::new();
    collect_logs(&repo.git_dir().join("logs"), "", &mut names)?;

    let mut out = String::new();
    for name in names {
        out.push_str(&name);
        out.push('\n');
    }
    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

/// `git reflog exists <ref>` — a literal test for `$GIT_DIR/logs/<ref>`.
fn exists(repo: &gix::Repository, rest: &[String]) -> Result<ExitCode> {
    let [name] = rest else {
        eprint!("usage: git reflog exists <ref>\n\n");
        return Ok(ExitCode::from(129));
    };

    // git validates with REFNAME_ALLOW_ONELEVEL, i.e. `master` is well-formed
    // even though it is not a full ref name — that is gitoxide's partial name.
    if <&gix::refs::PartialNameRef>::try_from(name.as_str()).is_err() {
        eprintln!("fatal: invalid ref format: {name}");
        return Ok(ExitCode::from(128));
    }

    let present = reflog_roots(repo)
        .iter()
        .any(|root| root.join(name).is_file());
    Ok(if present {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Emit git's "unknown revision" fatal block verbatim and return its exit code.
fn fatal_ambiguous(spec: &str) -> ExitCode {
    eprintln!(
        "fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree."
    );
    eprintln!("Use '--' to separate paths from revisions, like this:");
    eprintln!("'git <command> [<revision>...] -- [<file>...]'");
    ExitCode::from(128)
}

/// Split `<ref>@{<n>}` into the ref name as typed and the starting entry index.
/// A spec without a trailing `@{...}` yields `(spec, None)`.
fn split_selector(spec: &str) -> Result<(&str, Option<usize>)> {
    let Some(open) = spec.rfind("@{") else {
        return Ok((spec, None));
    };
    if !spec.ends_with('}') {
        return Ok((spec, None));
    }
    if open == 0 {
        bail!("the bare `@{{<n>}}` form is not supported, name the ref explicitly");
    }
    let inner = &spec[open + 2..spec.len() - 1];
    match inner.parse::<usize>() {
        Ok(n) => Ok((&spec[..open], Some(n))),
        Err(_) => bail!("non-numeric reflog selector {spec:?} (only `<ref>@{{<n>}}` is ported)"),
    }
}

/// Abbreviate `id` the way git does: the shortest unique prefix at least
/// `core.abbrev` long. Falls back to a plain `core.abbrev`-length prefix when the
/// object is missing from the odb and no unique prefix can be computed.
fn short_id(repo: &gix::Repository, id: ObjectId, fallback_len: usize) -> String {
    match id.attach(repo).shorten() {
        Ok(prefix) => prefix.to_string(),
        Err(_) => id.to_hex_with_len(fallback_len).to_string(),
    }
}

/// The configured abbreviation length: `core.abbrev` when set to a number, the
/// full hash for `no`/`false`, otherwise git's automatic length derived from the
/// packed object count (`max(7, ceil(bits(count) / 2))`).
fn abbrev_len(repo: &gix::Repository) -> usize {
    let full = repo.object_hash().len_in_hex();
    if let Some(value) = repo.config_snapshot().string("core.abbrev") {
        match value.to_str_lossy().as_ref() {
            "no" | "false" => return full,
            "auto" => {}
            n => {
                if let Ok(n) = n.parse::<usize>() {
                    return n.clamp(4, full);
                }
            }
        }
    }
    let count = repo.objects.packed_object_count().unwrap_or(0);
    let len = (64 - count.leading_zeros()).div_ceil(2) as usize;
    len.max(7).min(full)
}

/// The directories that hold reflog files. Normally one; a linked worktree keeps
/// its per-worktree logs (`HEAD`, `refs/bisect/*`) beside the shared ones.
fn reflog_roots(repo: &gix::Repository) -> Vec<PathBuf> {
    let git = repo.git_dir().join("logs");
    let common = repo.common_dir().join("logs");
    if git == common {
        vec![git]
    } else {
        vec![git, common]
    }
}

/// Append every log file below `dir` to `out` as a `/`-joined ref name, sorting
/// each directory's entries by name so the result matches git's tree walk (a
/// sub-directory is descended at its own sort position, not after its siblings).
fn collect_logs(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    let mut items: Vec<(String, bool)> = Vec::new();
    for entry in read {
        let entry = entry?;
        let is_dir = entry.file_type()?.is_dir();
        items.push((entry.file_name().to_string_lossy().into_owned(), is_dir));
    }
    items.sort();

    for (name, is_dir) in items {
        let full = format!("{prefix}{name}");
        if is_dir {
            collect_logs(&dir.join(&name), &format!("{full}/"), out)?;
        } else {
            out.push(full);
        }
    }
    Ok(())
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn parse_count(value: &str, flag: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|_| anyhow!("invalid count `{value}` for `{flag}`"))
}
