//! `git tag` — list, create (lightweight and annotated) and delete tags.
//!
//! Served natively via the vendored gitoxide crates so tools on PATH observe
//! the same ref store. Implemented forms (matching stock `git tag`):
//!
//!   * `git tag`                       → list every tag, one short name per line,
//!                                       sorted ascending by refname.
//!   * `git tag -l|--list [<pattern>…]`→ list, keeping tags whose *short* name
//!                                       matches any pattern (git's `wildmatch`
//!                                       without `WM_PATHNAME`, so `*` spans `/`).
//!   * `git tag -n[<num>]`             → append the first `<num>` lines (default 1)
//!                                       of each tag's message; implies listing.
//!   * `git tag --sort=[-]refname`     → ascending/descending refname order.
//!   * `git tag --format=<fmt>`        → render each tag through `<fmt>` instead
//!                                       of the default/`-n` layout.
//!   * `git tag <name> [<commit>]`     → create a lightweight tag at `<commit>`
//!                                       (default `HEAD`).
//!   * `git tag -a|-m|-F …`            → create an annotated tag object with the
//!                                       committer identity as tagger.
//!   * `git tag -f …`                  → force, printing `Updated tag '<name>'
//!                                       (was …)` only when the ref value changes.
//!   * `git tag -d <name>…`            → delete each tag.
//!
//! Exit codes follow git rather than the caller's generic failure path: fatal
//! errors exit 128 and a failed delete exits 1.
//!
//! Not backed here, and refused with a terse message rather than faked:
//! signing (`-s`, `-u`), verification (`-v`), an editor-supplied message (`-a`
//! with neither `-m` nor `-F`), `--cleanup`, `--column`, the listing filters
//! (`--contains`, `--points-at`, `--merged`, …), sort keys other than `refname`,
//! and `--format` atoms outside the small set listed in [`render_atom`].

use anyhow::{anyhow, bail, Result};
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::glob::wildmatch;
use gix::glob::wildmatch::Mode;
use gix::hash::ObjectId;
use gix::objs::Kind;
use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};
use gix::refs::FullName;

/// One tag ref, captured before any filtering or ordering is applied.
struct Entry {
    /// Full ref name, e.g. `refs/tags/v1.0`. Ordering key, as in git.
    full: BString,
    /// Short name, e.g. `v1.0`. What patterns match against and what is printed.
    short: BString,
    /// The ref's own target — the tag object for an annotated tag, not the peel.
    id: ObjectId,
}

pub fn tag(args: &[String]) -> Result<ExitCode> {
    let mut delete = false;
    let mut list = false;
    let mut force = false;
    let mut annotate = false;
    let mut lines: Option<usize> = None;
    let mut sort: Option<String> = None;
    let mut format: Option<String> = None;
    // `-m` chunks in the order given, and the `-F` file if one was named.
    let mut messages: Vec<Vec<u8>> = Vec::new();
    let mut message_file: Option<String> = None;
    let mut positionals: Vec<&str> = Vec::new();
    let mut operands_only = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        if operands_only || !a.starts_with('-') || a == "-" {
            positionals.push(a);
            continue;
        }
        match a {
            "--" => operands_only = true,
            "-d" | "--delete" => delete = true,
            "-l" | "--list" => list = true,
            "-f" | "--force" => force = true,
            "-a" | "--annotate" => annotate = true,
            "-s" | "--sign" | "-u" | "--local-user" => {
                bail!("signed tags ({a}) are not supported")
            }
            "-v" | "--verify" => bail!("tag verification (-v) is not supported"),
            "-e" | "--edit" => bail!("editing tag messages (-e) is not supported"),
            "-n" => lines = Some(1),
            _ => {
                if let Some(rest) = a.strip_prefix("--sort=") {
                    sort = Some(rest.to_string());
                } else if a == "--sort" {
                    sort = Some(take_value(args, &mut i, "sort")?.to_string());
                } else if let Some(rest) = a.strip_prefix("--format=") {
                    format = Some(rest.to_string());
                } else if a == "--format" {
                    format = Some(take_value(args, &mut i, "format")?.to_string());
                } else if let Some(rest) = a.strip_prefix("--message=") {
                    messages.push(rest.as_bytes().to_vec());
                } else if a == "--message" || a == "-m" {
                    messages.push(take_value(args, &mut i, "message")?.as_bytes().to_vec());
                } else if let Some(rest) = a.strip_prefix("-m") {
                    messages.push(rest.as_bytes().to_vec());
                } else if let Some(rest) = a.strip_prefix("--file=") {
                    message_file = Some(rest.to_string());
                } else if a == "--file" || a == "-F" {
                    message_file = Some(take_value(args, &mut i, "file")?.to_string());
                } else if let Some(rest) = a.strip_prefix("-F") {
                    message_file = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("-n") {
                    // git parses `-n` as an integer with an *attached* optional
                    // value, so `-n3` sets three lines and `-n 3` leaves `3` as
                    // a pattern operand.
                    let n: usize = rest
                        .parse()
                        .map_err(|_| anyhow!("unsupported option {a:?}"))?;
                    lines = Some(n);
                } else {
                    bail!("unsupported option {a:?}")
                }
            }
        }
    }

    let repo = gix::discover(".")?;

    if delete {
        return delete_tags(&repo, &positionals);
    }

    // git switches to listing when there is nothing to create, or when a
    // listing-only option (`-l`, `-n`) was given. `--sort`/`--format` alone do
    // *not* switch modes: `git tag --format=… v0.*` still tries to create.
    if list || lines.is_some() || positionals.is_empty() {
        return list_tags(
            &repo,
            &positionals,
            lines,
            format.as_deref(),
            sort.as_deref(),
        );
    }

    let annotate = annotate || !messages.is_empty() || message_file.is_some();
    create_tag(
        &repo,
        &positionals,
        force,
        annotate,
        &messages,
        message_file.as_deref(),
    )
}

/// Consume the value of a separated long/short option, or explain what is missing.
fn take_value<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str> {
    let v = args
        .get(*i)
        .ok_or_else(|| anyhow!("option `{flag}' requires a value"))?;
    *i += 1;
    Ok(v.as_str())
}

/// List tags, honoring pattern operands, `--sort`, and `--format`/`-n` rendering.
fn list_tags(
    repo: &gix::Repository,
    patterns: &[&str],
    lines: Option<usize>,
    format: Option<&str>,
    sort: Option<&str>,
) -> Result<ExitCode> {
    let mut reverse = false;
    if let Some(spec) = sort {
        let key = match spec.strip_prefix('-') {
            Some(rest) => {
                reverse = true;
                rest
            }
            None => spec,
        };
        if key != "refname" {
            bail!("--sort={spec} is not supported (only refname and -refname)");
        }
    }

    let mut entries: Vec<Entry> = Vec::new();
    for r in repo.references()?.tags()? {
        let r = r.map_err(|e| anyhow!("failed to read a tag reference: {e}"))?;
        let Some(id) = r.try_id().map(|id| id.detach()) else {
            continue;
        };
        let short = BString::from(r.name().shorten().to_vec());
        if !patterns.is_empty() && !patterns.iter().any(|p| matches(p, short.as_bstr())) {
            continue;
        }
        entries.push(Entry {
            full: BString::from(r.name().as_bstr().to_vec()),
            short,
            id,
        });
    }

    entries.sort_by(|a, b| a.full.cmp(&b.full));
    if reverse {
        entries.reverse();
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for e in &entries {
        let mut line: Vec<u8> = Vec::new();
        if let Some(fmt) = format {
            render_format(repo, e, fmt, &mut line)?;
        } else if let Some(n) = lines {
            // git renders `-n` as `%(align:15)%(refname:lstrip=2)%(end) %(contents:lines=N)`.
            line.extend_from_slice(&e.short);
            let width = e.short.to_str_lossy().chars().count();
            if width < 15 {
                line.resize(line.len() + (15 - width), b' ');
            }
            line.push(b' ');
            append_lines(&mut line, &contents(repo, e.id)?, n);
        } else {
            line.extend_from_slice(&e.short);
        }
        line.push(b'\n');
        out.write_all(&line)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// git's `ref-filter.c` `match_pattern`: `wildmatch` with no flags against the
/// short name, so `*` also crosses `/`.
fn matches(pattern: &str, short: &BStr) -> bool {
    wildmatch(pattern.as_bytes().as_bstr(), short, Mode::empty())
}

/// The object's message: everything after the header block, as git's
/// `%(contents)` defines it. Empty for objects that carry no message.
fn contents(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>> {
    let object = repo.find_object(id)?;
    if !matches!(object.kind, Kind::Commit | Kind::Tag) {
        return Ok(Vec::new());
    }
    let data = &object.data;
    Ok(match data.windows(2).position(|w| w == b"\n\n") {
        Some(i) => data[i + 2..].to_vec(),
        None => Vec::new(),
    })
}

/// Port of git's `append_lines`: the first `lines` lines of `buf`, with every
/// line after the first prefixed by a newline and four spaces.
fn append_lines(out: &mut Vec<u8>, buf: &[u8], lines: usize) {
    let mut sp = 0;
    for i in 0..lines {
        if sp >= buf.len() {
            break;
        }
        if i > 0 {
            out.extend_from_slice(b"\n    ");
        }
        match buf[sp..].iter().position(|&b| b == b'\n') {
            Some(nl) => {
                out.extend_from_slice(&buf[sp..sp + nl]);
                sp += nl + 1;
            }
            None => {
                out.extend_from_slice(&buf[sp..]);
                break;
            }
        }
    }
}

/// Expand a `--format` string for one tag.
///
/// `%%` is a literal percent and `%xx` a hex byte, as in `ref-filter.c`; `%(…)`
/// is delegated to [`render_atom`]. Anything else is refused rather than being
/// passed through, so a format this module cannot honor never looks like a
/// success.
fn render_format(repo: &gix::Repository, e: &Entry, fmt: &str, out: &mut Vec<u8>) -> Result<()> {
    let b = fmt.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        match b.get(i + 1) {
            Some(b'%') => {
                out.push(b'%');
                i += 2;
            }
            Some(b'(') => {
                let Some(end) = b[i + 2..].iter().position(|&c| c == b')') else {
                    bail!("format string has an unmatched '%('")
                };
                let atom = std::str::from_utf8(&b[i + 2..i + 2 + end])
                    .map_err(|_| anyhow!("format atom is not valid UTF-8"))?;
                render_atom(repo, e, atom, out)?;
                i += 2 + end + 1;
            }
            _ => {
                let hex = b
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => bail!("unsupported '%' escape in --format"),
                }
            }
        }
    }
    Ok(())
}

/// Render one `%(<atom>)` field.
///
/// Backed: `refname`, `refname:short`, `refname:lstrip=<n>`, `refname:rstrip=<n>`,
/// `objectname`, `objectname:short`, `objectname:short=<n>`, `objecttype`,
/// `objectsize`. Every other atom — including ones stock git does implement —
/// is refused, because rendering it wrongly would be worse than not rendering it.
fn render_atom(repo: &gix::Repository, e: &Entry, atom: &str, out: &mut Vec<u8>) -> Result<()> {
    match atom {
        "refname" => out.extend_from_slice(&e.full),
        "refname:short" => out.extend_from_slice(&e.short),
        "objectname" => out.extend_from_slice(e.id.to_hex().to_string().as_bytes()),
        "objectname:short" => out.extend_from_slice(short_hex(repo, e.id).as_bytes()),
        "objecttype" => out.extend_from_slice(repo.find_header(e.id)?.kind().as_bytes()),
        "objectsize" => {
            out.extend_from_slice(repo.find_header(e.id)?.size().to_string().as_bytes())
        }
        _ => {
            if let Some(n) = strip_arg(atom, "refname:lstrip=") {
                out.extend_from_slice(&strip_components(&e.full, n?, true));
            } else if let Some(n) = strip_arg(atom, "refname:rstrip=") {
                out.extend_from_slice(&strip_components(&e.full, n?, false));
            } else if let Some(n) = strip_arg(atom, "objectname:short=") {
                let n: usize = n?.try_into().map_err(|_| anyhow!("bad abbrev length"))?;
                out.extend_from_slice(e.id.to_hex_with_len(n).to_string().as_bytes());
            } else {
                bail!("--format atom %({atom}) is not supported")
            }
        }
    }
    Ok(())
}

/// Parse the numeric argument of an atom like `refname:lstrip=2`.
fn strip_arg(atom: &str, prefix: &str) -> Option<Result<i64>> {
    let rest = atom.strip_prefix(prefix)?;
    Some(
        rest.parse::<i64>()
            .map_err(|_| anyhow!("--format atom %({atom}) has a non-numeric argument")),
    )
}

/// `%(refname:lstrip=<n>)` / `%(refname:rstrip=<n>)`.
///
/// A positive `n` drops `n` components from the given end; a negative `n` keeps
/// `-n` components at that end. Over-stripping yields an empty string for
/// positive counts and the full name for negative ones — never an error.
fn strip_components(name: &[u8], n: i64, from_left: bool) -> Vec<u8> {
    let parts: Vec<&[u8]> = name.split(|&b| b == b'/').collect();
    let len = parts.len() as i64;
    let kept: &[&[u8]] = if n >= 0 {
        if n >= len {
            &[]
        } else if from_left {
            &parts[n as usize..]
        } else {
            &parts[..(len - n) as usize]
        }
    } else {
        let keep = -n;
        if keep >= len {
            &parts[..]
        } else if from_left {
            &parts[(len - keep) as usize..]
        } else {
            &parts[..keep as usize]
        }
    };
    kept.join(&b'/')
}

/// Create a lightweight or annotated tag `<name>` at `[<commit>]` (default `HEAD`).
///
/// The order of the checks is git's: too-many-arguments, then object
/// resolution, then tag-name validity, then the already-exists guard. Getting
/// this order wrong changes which `fatal:` a caller sees.
fn create_tag(
    repo: &gix::Repository,
    positionals: &[&str],
    force: bool,
    annotate: bool,
    messages: &[Vec<u8>],
    message_file: Option<&str>,
) -> Result<ExitCode> {
    if positionals.len() > 2 {
        return fatal("too many arguments");
    }
    let name = positionals[0];
    let spec = positionals.get(1).copied().unwrap_or("HEAD");

    let Ok(target) = repo.rev_parse_single(BStr::new(spec)) else {
        return fatal(&format!("Failed to resolve '{spec}' as a valid ref."));
    };
    let target = target.detach();

    let ref_name = format!("refs/tags/{name}");
    if FullName::try_from(ref_name.as_str()).is_err() {
        return fatal(&format!("'{name}' is not a valid tag name."));
    }

    // Serialize the ref read-modify-write through the repo coordinator so
    // concurrent zvcs writers queue instead of racing the ref lock.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let prev = repo
        .try_find_reference(ref_name.as_str())?
        .and_then(|r| r.try_id().map(|id| id.detach()));

    if prev.is_some() && !force {
        return fatal(&format!("tag '{name}' already exists"));
    }
    let constraint = if force {
        PreviousValue::Any
    } else {
        PreviousValue::MustNotExist
    };

    let new_id = if annotate {
        if !messages.is_empty() && message_file.is_some() {
            bail!("only one of -F or -m is supported");
        }
        let raw = match message_file {
            Some(path) => read_message_file(path)?,
            None if messages.is_empty() => {
                bail!("`-a` without `-m`/`-F` needs an editor, which is not supported")
            }
            None => join_messages(messages),
        };
        let message = stripspace(&raw);

        let tagger = repo
            .committer()
            .ok_or_else(|| {
                anyhow!(
                    "no committer identity configured (set user.name/user.email or \
                     GIT_COMMITTER_NAME/GIT_COMMITTER_EMAIL); git's gecos fallback is not ported"
                )
            })??
            .to_owned()?;

        let object = gix::objs::Tag {
            target,
            target_kind: repo.find_header(target)?.kind(),
            name: BString::from(name.as_bytes().to_vec()),
            tagger: Some(tagger),
            message: BString::from(message),
            pgp_signature: None,
        };
        let id = repo.write_object(&object)?.detach();
        repo.tag_reference(name, id, constraint)?;
        id
    } else {
        repo.tag_reference(name, target, constraint)?;
        target
    };

    // git prints the update line only when the ref actually moved, so a force
    // that re-creates the identical value stays silent.
    if let Some(old) = prev {
        if old != new_id {
            println!("Updated tag '{name}' (was {})", short_hex(repo, old));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Read a `-F <file>` message, or stdin for `-`.
fn read_message_file(path: &str) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    if path == "-" {
        std::io::stdin().lock().read_to_end(&mut buf)?;
    } else {
        buf = std::fs::read(path).map_err(|e| anyhow!("could not read '{path}': {e}"))?;
    }
    Ok(buf)
}

/// Port of git's `opt_parse_m`: each `-m` chunk is newline-terminated, and a
/// further newline separates it from the previous one.
fn join_messages(messages: &[Vec<u8>]) -> Vec<u8> {
    let mut buf = Vec::new();
    for chunk in messages {
        if !buf.is_empty() {
            buf.push(b'\n');
        }
        buf.extend_from_slice(chunk);
        if buf.last() != Some(&b'\n') {
            buf.push(b'\n');
        }
    }
    buf
}

/// Port of git's `strbuf_stripspace(buf, NULL)`, which is the cleanup `git tag`
/// applies to a `-m`/`-F` message: trailing whitespace is removed from every
/// line, runs of blank lines collapse to one, leading and trailing blank lines
/// go away, and a non-empty result ends in a newline. Comment lines are *not*
/// stripped — verified against stock git, which keeps a leading `#` for `-m`.
fn stripspace(input: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    let mut pending_blank = false;
    for line in input.split(|&b| b == b'\n') {
        let end = line
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(0, |i| i + 1);
        let trimmed = &line[..end];
        if trimmed.is_empty() {
            pending_blank = !out.is_empty();
            continue;
        }
        if pending_blank {
            out.push(b'\n');
            pending_blank = false;
        }
        out.extend_from_slice(trimmed);
        out.push(b'\n');
    }
    out
}

/// Delete each named tag, printing `Deleted tag '<name>' (was <short>)`.
///
/// Mirrors git: a missing tag is reported on stderr and does not abort the
/// remaining deletions; the command exits 1 if any tag was missing. `-d` with
/// no names is a silent success, as in git.
fn delete_tags(repo: &gix::Repository, positionals: &[&str]) -> Result<ExitCode> {
    if positionals.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut had_failure = false;
    for name in positionals {
        let ref_name = format!("refs/tags/{name}");
        // An unusable name cannot name an existing ref, and git reports it the
        // same way it reports a well-formed name that is simply absent.
        let found = if FullName::try_from(ref_name.as_str()).is_err() {
            None
        } else {
            repo.try_find_reference(ref_name.as_str())?
        };
        let Some(r) = found else {
            eprintln!("error: tag '{name}' not found.");
            had_failure = true;
            continue;
        };
        let old = r.try_id().map(|id| id.detach());

        let full: FullName = ref_name
            .as_str()
            .try_into()
            .map_err(|e| anyhow!("invalid tag name {name:?}: {e}"))?;
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::MustExist,
                log: RefLog::AndReference,
            },
            name: full,
            deref: false,
        })?;

        match old {
            Some(id) => println!("Deleted tag '{name}' (was {})", short_hex(repo, id)),
            None => println!("Deleted tag '{name}'"),
        }
    }

    Ok(if had_failure {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Report a git `fatal:` failure on stderr and yield git's exit code for it.
fn fatal(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// Abbreviated hex for `id`, honoring the repo's shortening rules (falls back to
/// the full id when the object isn't present to disambiguate against).
fn short_hex(repo: &gix::Repository, id: ObjectId) -> String {
    use gix::prelude::ObjectIdExt;
    id.attach(repo).shorten_or_id().to_string()
}
