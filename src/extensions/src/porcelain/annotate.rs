//! `git annotate` — `git blame` rendered in the CVS-compatible output format
//! (`builtin/blame.c`'s `OUTPUT_ANNOTATE_COMPAT` path), backed by `gix-blame`.
//!
//! Covered: `[<rev>] [--] <file>`, `-L` numeric ranges, `-l` / `--abbrev=<n>` /
//! `--no-abbrev`, `-t`, `-e`, `-b`, `--root` / `--no-root`, the inert-in-compat
//! `-c` / `-f` / `-n` / `-s`, and the `blame.blankBoundary`, `blame.showRoot`,
//! `core.abbrev` config knobs.
//!
//! Not covered (rejected, never silently ignored): porcelain/incremental output,
//! `-M` / `-C` line-move detection, `-w`, `--reverse`, `-S`, `--contents`,
//! `--ignore-rev(s-file)`, `--show-stats`, `--score-debug`, coloring,
//! `--diff-algorithm`, and the regex / `:funcname` `-L` forms.

use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::io::Write;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;

/// git's smallest permitted abbreviation length.
const MINIMUM_ABBREV: usize = 4;

/// `git annotate` — line-by-line last-modifying commit in the CVS-compatible
/// output format, backed by `gix-blame`.
///
/// Upstream `git annotate` is `git blame` with `OUTPUT_ANNOTATE_COMPAT` forced
/// on; `builtin/blame.c:emit_other()` then renders each line as
/// `"%.*s\t(%10s\t%10s\t%d)"` — object name, author (or `<email>` under `-e`),
/// author date, and the final line number — followed immediately by the line
/// content with no separating space. Boundary commits get no `^` marker in this
/// mode; they are only distinguishable via `-b`, which blanks the hash column.
///
/// Supported invocations (output byte-for-byte matches stock `git annotate`):
///   * `git annotate <file>`             — annotate `HEAD:<file>`
///   * `git annotate <rev> [--] <file>`  — annotate `<rev>:<file>`
///   * `-L <start>,<end>` / `-L <start>` / `-L <start>,+<n>` / `-L ,<end>`
///   * `-l` (full object name), `--abbrev=<n>`
///   * `-t` (raw timestamp), `-e`/`--show-email`
///   * `-b` (blank boundary object names), `--root` / `--no-root`
///   * `-c`, `-f`/`--show-name`, `-n`/`--show-number`, `-s` — accepted and
///     inert, because the compat renderer never consults them (verified against
///     stock git: their output is identical to the bare invocation).
///   * config: `blame.blankBoundary`, `blame.showRoot`, `core.abbrev`
///
/// Whole-file rename following is on, matching git's default. Flags that would
/// change the result but are not implemented (`-p`/`--porcelain`,
/// `--line-porcelain`, `--incremental`, `-w`, `-M`/`-C`, `--reverse`, `-S`,
/// `--contents`, `--ignore-rev(s-file)`, `--show-stats`, `--score-debug`,
/// `--color-*`, `--diff-algorithm`, regex/function `-L` forms) are rejected
/// with a precise message rather than emitting wrong output.
pub fn annotate(args: &[String]) -> Result<ExitCode> {
    // `args[0]` is the subcommand itself when dispatched; tolerate its absence.
    let rest = match args.first() {
        Some(a) if a == "annotate" => &args[1..],
        _ => args,
    };
    let mut opts = match Options::parse(rest)? {
        Some(opts) => opts,
        None => {
            eprintln!("usage: git annotate [<options>] [<rev-opts>] [<rev>] [--] <file>");
            return Ok(ExitCode::from(129));
        }
    };

    let repo = gix::discover(".")?;

    // Command-line flags win; otherwise fall back to the two blame config knobs
    // git honours here (`blame.blankBoundary`, `blame.showRoot`).
    {
        let config = repo.config_snapshot();
        if opts.blank_boundary.is_none() {
            opts.blank_boundary = config.boolean("blame.blankBoundary");
        }
        if opts.show_root.is_none() {
            opts.show_root = config.boolean("blame.showRoot");
        }
    }
    let blank_boundary = opts.blank_boundary.unwrap_or(false);
    let show_root = opts.show_root.unwrap_or(false);

    // Resolve the suspect commit (default HEAD), peeling tags to a commit.
    let suspect = match &opts.rev {
        Some(rev) => match repo
            .rev_parse_single(rev.as_str())
            .ok()
            .and_then(|id| id.object().ok())
            .and_then(|obj| obj.peel_to_commit().ok())
        {
            Some(commit) => commit.id().detach(),
            None => {
                eprintln!("fatal: bad revision '{rev}'");
                return Ok(ExitCode::from(128));
            }
        },
        None => repo.head_id()?.detach(),
    };

    // Translate the user's path (relative to CWD) into a repo-root-relative path.
    let rel_path = repo_relative_path(&repo, &opts.file)?;

    let ranges = if opts.ranges.is_empty() {
        gix::blame::BlameRanges::default()
    } else {
        gix::blame::BlameRanges::from_one_based_inclusive_ranges(opts.ranges.clone())
            .map_err(|e| anyhow!("{e}"))?
    };
    let blame_options = gix::repository::blame_file::Options {
        diff_algorithm: None,
        ranges,
        since: None,
        rewrites: Some(gix::diff::Rewrites::default()),
    };

    let outcome = match repo.blame_file(rel_path.as_bytes().as_bstr(), suspect, blame_options) {
        Ok(outcome) => outcome,
        Err(_) => {
            // git quotes the path only when no explicit revision was given.
            match &opts.rev {
                Some(rev) => eprintln!("fatal: no such path {} in {rev}", opts.file),
                None => eprintln!("fatal: no such path '{}' in HEAD", opts.file),
            }
            return Ok(ExitCode::from(128));
        }
    };

    // Materialize every output line; the compat format has no computed column
    // widths, so a single pass over the entries is all that is required.
    struct Line {
        commit_id: ObjectId,
        final_no: u32,
        content: Vec<u8>,
    }
    let mut lines: Vec<Line> = Vec::new();
    for (entry, tokens) in outcome.entries_with_lines() {
        let blamed_start = entry.start_in_blamed_file;
        for (i, token) in tokens.into_iter().enumerate() {
            // Line tokens include their trailing '\n'; strip exactly one so the
            // newline we append reproduces the original terminator.
            let mut content = token.to_vec();
            if content.last() == Some(&b'\n') {
                content.pop();
            }
            lines.push(Line {
                commit_id: entry.commit_id,
                final_no: blamed_start + i as u32 + 1,
                content,
            });
        }
    }

    if lines.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    // Per-commit metadata (author display + date + boundary flag + hex), cached.
    struct CommitInfo {
        author: Vec<u8>,
        date: String,
        boundary: bool,
        hex: String,
    }
    let mut info: HashMap<ObjectId, CommitInfo> = HashMap::new();
    for line in &lines {
        if info.contains_key(&line.commit_id) {
            continue;
        }
        let commit = repo.find_commit(line.commit_id)?;
        let sig = commit.author()?;
        let author = if opts.show_email {
            let email = sig.email.to_vec();
            let mut v = Vec::with_capacity(email.len() + 2);
            v.push(b'<');
            v.extend_from_slice(&email);
            v.push(b'>');
            v
        } else {
            sig.name.to_vec()
        };
        // `-t` reproduces git's raw form (`<seconds> <tz>`); otherwise the
        // default `ISO8601` shape `YYYY-MM-DD HH:MM:SS +ZZZZ`.
        let format: gix::date::time::Format = if opts.raw_timestamp {
            gix::date::time::format::RAW
        } else {
            gix::date::time::format::ISO8601.into()
        };
        let date = sig
            .time()
            .map(|t| t.format_or_unix(format))
            .unwrap_or_else(|_| sig.time.to_string());
        // Only root commits are marked UNINTERESTING here; `--root` clears that,
        // and the flag is inert unless `-b` blanks the hash column.
        let boundary = !show_root && commit.parent_ids().next().is_none();
        info.insert(
            line.commit_id,
            CommitInfo {
                author,
                date,
                boundary,
                hex: line.commit_id.to_hex().to_string(),
            },
        );
    }

    // Abbreviation length, following git's rule: config/`--abbrev`, clamped, then
    // +1 for the boundary-marker slot (`-l` forces the full hash). Compat mode
    // never prints the `^`, but it still uses the widened length.
    let hexsz = repo.object_hash().len_in_hex();
    let mut length = if opts.long {
        hexsz
    } else {
        opts.abbrev
            .unwrap_or_else(|| configured_abbrev(&repo, hexsz))
            .clamp(MINIMUM_ABBREV, hexsz)
    };
    if length < hexsz {
        length += 1;
    }

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut buf: Vec<u8> = Vec::with_capacity(128);

    for line in &lines {
        let ci = &info[&line.commit_id];
        buf.clear();

        // Object name column — blanked for boundary commits under `-b`.
        if ci.boundary && blank_boundary {
            buf.resize(buf.len() + length, b' ');
        } else {
            buf.extend_from_slice(&ci.hex.as_bytes()[..length]);
        }

        // `\t(%10s\t%10s\t%d)` then the content, with no separating space.
        buf.push(b'\t');
        buf.push(b'(');
        pad_left(&mut buf, &ci.author, 10);
        buf.push(b'\t');
        pad_left(&mut buf, ci.date.as_bytes(), 10);
        buf.push(b'\t');
        buf.extend_from_slice(line.final_no.to_string().as_bytes());
        buf.push(b')');
        buf.extend_from_slice(&line.content);
        buf.push(b'\n');

        out.write_all(&buf)?;
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Parsed command line.
struct Options {
    rev: Option<String>,
    file: String,
    ranges: Vec<RangeInclusive<u32>>,
    long: bool,
    raw_timestamp: bool,
    show_email: bool,
    /// `None` = unspecified on the command line, defer to `blame.blankBoundary`.
    blank_boundary: Option<bool>,
    /// `None` = unspecified on the command line, defer to `blame.showRoot`.
    show_root: Option<bool>,
    abbrev: Option<usize>,
}

impl Options {
    /// Returns `Ok(None)` when no path was given, i.e. git's usage error (129).
    fn parse(args: &[String]) -> Result<Option<Self>> {
        let mut ranges: Vec<RangeInclusive<u32>> = Vec::new();
        let mut long = false;
        let mut raw_timestamp = false;
        let mut show_email = false;
        let mut blank_boundary: Option<bool> = None;
        let mut show_root: Option<bool> = None;
        let mut abbrev: Option<usize> = None;
        let mut positionals: Vec<String> = Vec::new();
        let mut only_paths = false;

        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if only_paths {
                positionals.push(a.to_string());
                i += 1;
                continue;
            }
            match a {
                "--" => only_paths = true,
                "-l" => long = true,
                "-t" => raw_timestamp = true,
                "-e" | "--show-email" => show_email = true,
                "--no-show-email" => show_email = false,
                "-b" => blank_boundary = Some(true),
                "--root" => show_root = Some(true),
                "--no-root" => show_root = Some(false),
                // Inert in the compat renderer — accepted so real scripts work.
                "-c" | "-f" | "--show-name" | "--no-show-name" | "-n" | "--show-number"
                | "--no-show-number" | "-s" => {}
                "-L" => {
                    i += 1;
                    let spec = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `-L` requires a value"))?;
                    parse_line_range(spec, &mut ranges)?;
                }
                // git declares this as `--[no-]abbrev[=<n>]`: the value is
                // optional and is never taken from the following argument, so a
                // bare `--abbrev` just means "use the configured default".
                "--abbrev" => abbrev = None,
                "--no-abbrev" => abbrev = Some(usize::MAX),
                _ if a.starts_with("-L") => parse_line_range(&a[2..], &mut ranges)?,
                _ if a.starts_with("--abbrev=") => {
                    let v = &a["--abbrev=".len()..];
                    abbrev = Some(v.parse().map_err(|_| anyhow!("invalid --abbrev value: {v}"))?);
                }
                _ if a.starts_with('-') && a.len() > 1 => bail!(
                    "unsupported flag {a:?} (ported: -b, -c, -e/--show-email, -f, -l, -n, -s, \
                     -t, --root/--no-root, -L <range>, --abbrev=<n>)"
                ),
                _ => positionals.push(a.to_string()),
            }
            i += 1;
        }

        // `[<rev>] [--] <file>`: the last positional is the path; a single
        // positional before it (if any) is the revision.
        let (rev, file) = match positionals.len() {
            0 => return Ok(None),
            1 => (None, positionals.pop().unwrap()),
            2 => {
                let file = positionals.pop().unwrap();
                (Some(positionals.pop().unwrap()), file)
            }
            _ => bail!("too many paths given; only a single file is supported"),
        };

        Ok(Some(Options {
            rev,
            file,
            ranges,
            long,
            raw_timestamp,
            show_email,
            blank_boundary,
            show_root,
            abbrev,
        }))
    }
}

/// Parse one `-L` spec into a 1-based inclusive range. Only numeric forms are
/// supported; regex (`/re/`) and function (`:name`) forms are rejected.
fn parse_line_range(spec: &str, ranges: &mut Vec<RangeInclusive<u32>>) -> Result<()> {
    if spec.starts_with('/') || spec.starts_with(':') || spec.starts_with("^/") {
        bail!("unsupported -L form: only numeric ranges are supported");
    }
    let (start_part, end_part) = match spec.split_once(',') {
        Some((s, e)) => (s, Some(e)),
        None => (spec, None),
    };

    let start: u32 = if start_part.is_empty() {
        1
    } else {
        start_part
            .parse()
            .map_err(|_| anyhow!("invalid -L range: {spec}"))?
    };
    if start == 0 {
        bail!("invalid -L range: line numbers are 1-based");
    }

    let end: u32 = match end_part {
        None => u32::MAX,
        Some(e) if e.is_empty() => u32::MAX,
        Some(e) if e.starts_with('+') => {
            let count: u32 = e[1..]
                .parse()
                .map_err(|_| anyhow!("invalid -L range: {spec}"))?;
            start.saturating_add(count.saturating_sub(1))
        }
        Some(e) if e.starts_with('-') => {
            bail!("unsupported -L form: relative end offsets are not supported")
        }
        Some(e) => e.parse().map_err(|_| anyhow!("invalid -L range: {spec}"))?,
    };

    ranges.push(start..=end.max(start));
    Ok(())
}

/// git's effective `core.abbrev`: an explicit number, `auto`/absent → derived
/// from the object count, or `no`/`off`/`false` → the full hash length.
fn configured_abbrev(repo: &gix::Repository, hexsz: usize) -> usize {
    match repo
        .config_snapshot()
        .string("core.abbrev")
        .as_ref()
        .and_then(|v| v.to_str().ok().map(str::to_ascii_lowercase))
    {
        None => auto_abbrev(repo, hexsz),
        Some(v) => match v.as_str() {
            "auto" => auto_abbrev(repo, hexsz),
            "no" | "off" | "false" => hexsz,
            other => other
                .parse::<usize>()
                .unwrap_or_else(|_| auto_abbrev(repo, hexsz)),
        },
    }
}

/// Auto abbreviation length: `ceil(log2(objects) / 2)`, floored at 7 — the same
/// heuristic `gix` uses for `core.abbrev = auto`.
fn auto_abbrev(repo: &gix::Repository, hexsz: usize) -> usize {
    let count = repo.objects.packed_object_count().unwrap_or(0);
    let mut len = (64 - count.leading_zeros()) as usize;
    len = len.div_ceil(2);
    len.max(7).min(hexsz)
}

/// Append `field` to `buf` right-justified in at least `width` bytes, matching
/// C's `%*s` (which pads but never truncates, and counts bytes not characters).
fn pad_left(buf: &mut Vec<u8>, field: &[u8], width: usize) {
    buf.resize(buf.len() + width.saturating_sub(field.len()), b' ');
    buf.extend_from_slice(field);
}

/// Turn a CWD-relative user path into a repo-root-relative path, so annotate
/// works from any subdirectory of the worktree (git resolves pathspecs the same
/// way).
fn repo_relative_path(repo: &gix::Repository, user_path: &str) -> Result<String> {
    let joined = match repo.workdir() {
        Some(workdir) => {
            let cwd = std::env::current_dir()?;
            let workdir_abs = workdir
                .canonicalize()
                .unwrap_or_else(|_| workdir.to_path_buf());
            let cwd_abs = cwd.canonicalize().unwrap_or(cwd);
            match cwd_abs.strip_prefix(&workdir_abs) {
                Ok(prefix) => prefix.join(user_path),
                Err(_) => PathBuf::from(user_path),
            }
        }
        None => PathBuf::from(user_path),
    };

    // Normalize `a/../b` style segments the join may have produced.
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for c in joined.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    let normalized: PathBuf = parts.iter().collect();

    let s = normalized
        .to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {user_path}"))?;
    Ok(s.to_string())
}
