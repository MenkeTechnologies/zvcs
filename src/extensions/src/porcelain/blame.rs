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

/// `git blame` — line-by-line last-modifying commit, backed by `gix-blame`.
///
/// Supports the common invocation forms and produces byte-for-byte the same
/// human output as stock `git blame` for them:
///   * `git blame <file>`                     — blame `HEAD:<file>`
///   * `git blame <rev> [--] <file>`          — blame `<rev>:<file>`
///   * `-L <start>,<end>` / `-L <start>` / `-L <start>,+<n>` / `-L ,<end>`
///   * `-l`/`--long`, `-s`, `-e`/`--show-email`, `-f`/`--show-name`,
///     `-n`/`--show-number`, `--abbrev=<n>`
///
/// Whole-file rename following is on (matching git's default), so the source
/// filename column appears exactly when git would show it. Boundary commits
/// (roots) are prefixed with `^` as git does.
///
/// Unsupported flags (porcelain/incremental output, `-M`/`-C` line-move
/// detection, `--reverse`, `-w`, regex/function `-L` forms, …) are rejected
/// with a precise message rather than emitting wrong output.
pub fn blame(args: &[String]) -> Result<ExitCode> {
    let opts = Options::parse(args)?;

    let repo = gix::discover(".")?;

    // Resolve the suspect commit (default HEAD), peeling tags to a commit.
    let suspect = match &opts.rev {
        Some(rev) => repo
            .rev_parse_single(rev.as_str())?
            .object()?
            .peel_to_commit()?
            .id()
            .detach(),
        None => repo.head_id()?.detach(),
    };

    // Translate the user's path (relative to CWD) into a repo-root-relative path.
    let rel_path = repo_relative_path(&repo, &opts.file)?;

    // Build blame options: enable rename tracking to mirror git's default of
    // following whole-file renames through history.
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

    let outcome = repo
        .blame_file(rel_path.as_bytes().as_bstr(), suspect, blame_options)
        .map_err(|e| anyhow!("{e}"))?;

    // First pass: materialize every output line so column widths can be sized.
    struct Line {
        commit_id: ObjectId,
        final_no: u32,
        orig_no: u32,
        source_name: Option<Vec<u8>>,
        content: Vec<u8>,
    }
    let mut lines: Vec<Line> = Vec::new();
    for (entry, tokens) in outcome.entries_with_lines() {
        let blamed_start = entry.start_in_blamed_file;
        let source_start = entry.start_in_source_file;
        let source_name = entry.source_file_name.as_ref().map(|n| n.to_vec());
        for (i, token) in tokens.into_iter().enumerate() {
            let i = i as u32;
            // Line tokens include their trailing '\n'; strip exactly one so the
            // final println-style newline reproduces the original terminator.
            let mut content = token.to_vec();
            if content.last() == Some(&b'\n') {
                content.pop();
            }
            lines.push(Line {
                commit_id: entry.commit_id,
                final_no: blamed_start + i + 1,
                orig_no: source_start + i + 1,
                source_name: source_name.clone(),
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
        let date = sig
            .time()
            .map(|t| t.format_or_unix(gix::date::time::format::ISO8601))
            .unwrap_or_else(|_| sig.time.to_string());
        let boundary = commit.parent_ids().next().is_none();
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

    // Abbreviation length, following git's rule (config/`--abbrev`, then +1 for
    // the boundary caret slot; `-l` forces the full hash).
    let hexsz = repo.object_hash().len_in_hex();
    let mut blame_abbrev = if opts.long {
        hexsz
    } else {
        opts.abbrev
            .unwrap_or_else(|| configured_abbrev(&repo, hexsz))
            .clamp(MINIMUM_ABBREV, hexsz)
    };
    if blame_abbrev < hexsz {
        blame_abbrev += 1;
    }

    // Column widths.
    let show_file = opts.show_name || lines.iter().any(|l| l.source_name.is_some());
    let current_path = rel_path.as_bytes();
    let w_line = decimal_width(lines.iter().map(|l| l.final_no).max().unwrap_or(1));
    let w_orig = decimal_width(lines.iter().map(|l| l.orig_no).max().unwrap_or(1));
    let w_file = if show_file {
        lines
            .iter()
            .map(|l| l.source_name.as_deref().unwrap_or(current_path).len())
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let w_author = if opts.suppress {
        0
    } else {
        lines
            .iter()
            .map(|l| info[&l.commit_id].author.len())
            .max()
            .unwrap_or(0)
    };

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut buf: Vec<u8> = Vec::with_capacity(128);

    for line in &lines {
        let ci = &info[&line.commit_id];
        buf.clear();

        // Object name, with the boundary caret occupying one hash slot.
        if ci.boundary {
            let length = if blame_abbrev < hexsz {
                blame_abbrev - 1
            } else {
                blame_abbrev
            };
            buf.push(b'^');
            buf.extend_from_slice(&ci.hex.as_bytes()[..length]);
        } else {
            buf.extend_from_slice(&ci.hex.as_bytes()[..blame_abbrev]);
        }

        // Source filename column (left-justified).
        if show_file {
            let name = line.source_name.as_deref().unwrap_or(current_path);
            buf.push(b' ');
            buf.extend_from_slice(name);
            pad(&mut buf, w_file.saturating_sub(name.len()));
        }

        // Original line number in the source commit (right-justified).
        if opts.show_number {
            let s = line.orig_no.to_string();
            buf.push(b' ');
            pad(&mut buf, w_orig.saturating_sub(s.len()));
            buf.extend_from_slice(s.as_bytes());
        }

        // Author/date block (omitted entirely by `-s`, mirroring git which then
        // leaves the closing paren of the line-number field unmatched).
        if !opts.suppress {
            buf.extend_from_slice(b" (");
            buf.extend_from_slice(&ci.author);
            pad(&mut buf, w_author.saturating_sub(ci.author.len()));
            buf.push(b' ');
            buf.extend_from_slice(ci.date.as_bytes());
        }

        // Final line number (right-justified) + content.
        let s = line.final_no.to_string();
        buf.push(b' ');
        pad(&mut buf, w_line.saturating_sub(s.len()));
        buf.extend_from_slice(s.as_bytes());
        buf.extend_from_slice(b") ");
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
    suppress: bool,
    show_email: bool,
    show_name: bool,
    show_number: bool,
    abbrev: Option<usize>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut ranges: Vec<RangeInclusive<u32>> = Vec::new();
        let mut long = false;
        let mut suppress = false;
        let mut show_email = false;
        let mut show_name = false;
        let mut show_number = false;
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
                "-l" | "--long" => long = true,
                "-s" => suppress = true,
                "-e" | "--show-email" => show_email = true,
                "-f" | "--show-name" => show_name = true,
                "-n" | "--show-number" => show_number = true,
                "-L" => {
                    i += 1;
                    let spec = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `-L` requires a value"))?;
                    parse_line_range(spec, &mut ranges)?;
                }
                "--abbrev" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `--abbrev` requires a value"))?;
                    abbrev = Some(v.parse().map_err(|_| anyhow!("invalid --abbrev value: {v}"))?);
                }
                _ if a.starts_with("-L") => parse_line_range(&a[2..], &mut ranges)?,
                _ if a.starts_with("--abbrev=") => {
                    let v = &a["--abbrev=".len()..];
                    abbrev = Some(v.parse().map_err(|_| anyhow!("invalid --abbrev value: {v}"))?);
                }
                _ if a.starts_with('-') && a.len() > 1 => {
                    bail!("unsupported option: {a}")
                }
                _ => positionals.push(a.to_string()),
            }
            i += 1;
        }

        // `[<rev>] [--] <file>`: the last positional is the path; a single
        // positional before it (if any) is the revision.
        let (rev, file) = match positionals.len() {
            0 => bail!("no path given"),
            1 => (None, positionals.pop().unwrap()),
            2 => {
                let file = positionals.pop().unwrap();
                (Some(positionals.pop().unwrap()), file)
            }
            _ => bail!("too many paths given; only a single file is supported"),
        };

        Ok(Options {
            rev,
            file,
            ranges,
            long,
            suppress,
            show_email,
            show_name,
            show_number,
            abbrev,
        })
    }
}

/// Parse one `-L` spec into a 1-based inclusive range. Only numeric forms are
/// supported; regex (`/re/`) and function (`:name`) forms are rejected.
fn parse_line_range(spec: &str, ranges: &mut Vec<RangeInclusive<u32>>) -> Result<()> {
    if spec.starts_with('/') || spec.starts_with(':') {
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
            other => other.parse::<usize>().unwrap_or_else(|_| auto_abbrev(repo, hexsz)),
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

/// Number of decimal digits needed to print `n` (at least 1).
fn decimal_width(n: u32) -> usize {
    n.to_string().len()
}

/// Append `n` spaces to `buf`.
fn pad(buf: &mut Vec<u8>, n: usize) {
    buf.resize(buf.len() + n, b' ');
}

/// Turn a CWD-relative user path into a repo-root-relative path, so blame works
/// from any subdirectory of the worktree (git resolves pathspecs the same way).
fn repo_relative_path(repo: &gix::Repository, user_path: &str) -> Result<String> {
    let joined = match repo.workdir() {
        Some(workdir) => {
            let cwd = std::env::current_dir()?;
            let workdir_abs = workdir.canonicalize().unwrap_or_else(|_| workdir.to_path_buf());
            let cwd_abs = cwd.canonicalize().unwrap_or(cwd);
            match cwd_abs.strip_prefix(&workdir_abs) {
                Ok(prefix) => prefix.join(user_path),
                Err(_) => PathBuf::from(user_path),
            }
        }
        None => PathBuf::from(user_path),
    };

    let s = joined
        .to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {user_path}"))?;
    Ok(s.strip_prefix("./").unwrap_or(s).to_string())
}
