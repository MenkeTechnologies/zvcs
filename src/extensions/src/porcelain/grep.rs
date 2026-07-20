//! `git grep` — search the contents of tracked files for a pattern.
//!
//! Covered: the tracked-worktree default and `--cached`, pathspec limiting (via
//! gitoxide's pathspec platform, so magic and globs work and a subdirectory
//! invocation searches only that subtree), and the line/name/count/quiet output
//! modes with byte-identical formatting, including git's binary-file handling
//! and `core.quotePath` path quoting.
//!
//! Not covered, and rejected loudly rather than approximated: patterns that need
//! a regex engine (the vendored gitoxide crates ship none — `gix`'s optional
//! `regex` dependency is behind the unused `revparse-regex` feature and is not
//! re-exported), searching `<tree>` revisions, `--untracked`/`--no-index`,
//! context lines (`-A`/`-B`/`-C`), `--heading`/`--break`, `-p`/`-W`, `-f`,
//! `--and`/`--or`/`--not`, `-m`, `--color`, and `-O`.

use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};

/// git's `FIRST_FEW_BYTES`: only this much of a file is scanned for NUL when
/// deciding whether it is binary (`buffer_is_binary()` in `xdiff-interface.c`).
const FIRST_FEW_BYTES: usize = 8000;

/// Which regex dialect the patterns were written in. Only the subset of each
/// dialect that is a plain literal is executable here; see [`literal_of`].
#[derive(Clone, Copy, PartialEq)]
enum Dialect {
    Basic,
    Extended,
    Fixed,
}

/// Parsed command-line options for a single `grep` invocation.
struct Opts {
    invert: bool,       // -v
    ignore_case: bool,  // -i
    word: bool,         // -w
    text: bool,         // -a: treat binary files as text
    no_binary: bool,    // -I: never match in binary files
    line_number: bool,  // -n
    column: bool,       // --column
    files_with: bool,   // -l/--files-with-matches/--name-only
    files_without: bool, // -L/--files-without-match
    count: bool,        // -c
    quiet: bool,        // -q
    nul: bool,          // -z
    only_matching: bool, // -o
    show_names: bool,   // -h clears, -H sets (default: on)
    full_name: bool,    // --full-name
    cached: bool,       // --cached
}

/// `git grep` — print lines matching a pattern.
///
/// Supported flags (output byte-for-byte identical to stock git for these):
///   * source: default (tracked files in the worktree), `--cached`
///   * matching: `-i`, `-v`, `-w`, `-F`/`--fixed-strings`, `-E`, `-G`,
///     `-e <pattern>` (repeatable; patterns are OR'd)
///   * binary: `-a`/`--text`, `-I`
///   * output: `-n`, `--column`, `-l`/`--files-with-matches`/`--name-only`,
///     `-L`/`--files-without-match`, `-c`/`--count`, `-q`/`--quiet`, `-o`,
///     `-z`/`--null`, `-h`, `-H`, `--full-name`
///   * `[--] <pathspec>...`
///
/// Exit status matches git: `0` when at least one line matched, `1` when none
/// did, and a non-zero error exit (via the caller's error path) otherwise.
pub fn grep(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts {
        invert: false,
        ignore_case: false,
        word: false,
        text: false,
        no_binary: false,
        line_number: false,
        column: false,
        files_with: false,
        files_without: false,
        count: false,
        quiet: false,
        nul: false,
        only_matching: false,
        show_names: true,
        full_name: false,
        cached: false,
    };
    let mut dialect = Dialect::Basic;
    let mut patterns: Vec<String> = Vec::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut no_more_opts = false;

    // Skip args[0], which is the subcommand name itself.
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if no_more_opts || a == "-" || !a.starts_with('-') {
            positionals.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            i += 1;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            match long {
                "invert-match" => opts.invert = true,
                "ignore-case" => opts.ignore_case = true,
                "word-regexp" => opts.word = true,
                "text" => opts.text = true,
                "line-number" => opts.line_number = true,
                "column" => opts.column = true,
                "files-with-matches" | "name-only" => opts.files_with = true,
                "files-without-match" => opts.files_without = true,
                "count" => opts.count = true,
                "quiet" => opts.quiet = true,
                "null" => opts.nul = true,
                "only-matching" => opts.only_matching = true,
                "full-name" => opts.full_name = true,
                "cached" => opts.cached = true,
                "recursive" => {} // the default
                "extended-regexp" => dialect = Dialect::Extended,
                "basic-regexp" => dialect = Dialect::Basic,
                "fixed-strings" => dialect = Dialect::Fixed,
                _ => bail!("{}", unsupported(a)),
            }
            i += 1;
            continue;
        }

        // Short flags, possibly grouped (`-in`). `-e` consumes the rest of the
        // group as its value, or the next argument when the group ends with it.
        let group: Vec<char> = a[1..].chars().collect();
        let mut c = 0;
        while c < group.len() {
            match group[c] {
                'i' => opts.ignore_case = true,
                'v' => opts.invert = true,
                'w' => opts.word = true,
                'a' => opts.text = true,
                'I' => opts.no_binary = true,
                'n' => opts.line_number = true,
                'l' => opts.files_with = true,
                'L' => opts.files_without = true,
                'c' => opts.count = true,
                'q' => opts.quiet = true,
                'z' => opts.nul = true,
                'o' => opts.only_matching = true,
                'h' => opts.show_names = false,
                'H' => opts.show_names = true,
                'r' => {} // the default
                'E' => dialect = Dialect::Extended,
                'G' => dialect = Dialect::Basic,
                'F' => dialect = Dialect::Fixed,
                'e' => {
                    let rest: String = group[c + 1..].iter().collect();
                    if rest.is_empty() {
                        i += 1;
                        let Some(p) = args.get(i) else {
                            bail!("switch `e' requires a value");
                        };
                        patterns.push(p.clone());
                    } else {
                        patterns.push(rest);
                    }
                    c = group.len();
                    continue;
                }
                other => bail!("{}", unsupported(&format!("-{other}"))),
            }
            c += 1;
        }
        i += 1;
    }

    // Without `-e`, the first positional is the pattern.
    if patterns.is_empty() {
        if positionals.is_empty() {
            bail!("no pattern given");
        }
        patterns.push(positionals.remove(0));
    }

    // Combinations whose exact output this port does not reproduce.
    if opts.only_matching && patterns.len() > 1 {
        bail!("-o with more than one pattern is not supported");
    }
    if opts.only_matching && opts.invert {
        bail!("-o combined with -v is not supported");
    }
    if opts.column && opts.invert {
        bail!("--column combined with -v is not supported");
    }
    if opts.quiet && opts.files_without {
        bail!("-q combined with -L is not supported");
    }

    let needles: Vec<Vec<u8>> = patterns
        .iter()
        .map(|p| literal_of(p, dialect))
        .collect::<Result<_>>()?;

    let repo = gix::discover(".")?;
    let index = repo.open_index()?;

    // A positional left over after the pattern is a `<tree>` in git's grammar
    // when it resolves as a revision; otherwise it is a pathspec.
    let mut specs: Vec<BString> = Vec::new();
    for p in &positionals {
        if repo.rev_parse_single(p.as_str()).is_ok() {
            bail!("searching a tree/revision ({p:?}) is not supported");
        }
        specs.push(BString::from(p.as_str()));
    }

    // The repo-root-relative prefix of the current directory; git strips it from
    // printed paths unless `--full-name` was given.
    let prefix: Option<Vec<u8>> = if opts.full_name {
        None
    } else {
        match repo.prefix()? {
            Some(p) if !p.as_os_str().is_empty() => {
                let mut b = gix::path::into_bstr(p).into_owned().to_vec();
                b.push(b'/');
                Some(b)
            }
            _ => None,
        }
    };

    // `empty_patterns_match_prefix = true` reproduces git's behaviour of
    // limiting a bare invocation to the current directory's subtree.
    let mut ps = repo.pathspec(
        true,
        &specs,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;
    let mut files: Vec<(BString, gix::hash::ObjectId)> = Vec::new();
    if let Some(iter) = ps.index_entries_with_paths(&index) {
        for (path, entry) in iter {
            // git's `grep_cache()` only visits regular files: symlinks and
            // gitlinks are skipped, and higher conflict stages are collapsed.
            if entry.mode != gix::index::entry::Mode::FILE
                && entry.mode != gix::index::entry::Mode::FILE_EXECUTABLE
            {
                continue;
            }
            if files.last().is_some_and(|(last, _)| last.as_bstr() == path) {
                continue;
            }
            files.push((path.to_owned(), entry.id));
        }
    }

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut any_hit = false;

    for (path, id) in &files {
        let content = if opts.cached {
            let object = repo.find_object(*id)?;
            object.data.clone()
        } else {
            let Some(abs) = repo.workdir_path(path.as_bstr()) else {
                continue;
            };
            match std::fs::read(&abs) {
                Ok(bytes) => bytes,
                // git silently ignores index entries whose file is gone.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            }
        };

        let binary = !opts.text && is_binary(&content);
        if binary && opts.no_binary {
            continue;
        }

        let name = display_name(path.as_bstr(), prefix.as_deref(), &opts);
        if search_file(&mut out, &content, &name, binary, &needles, &opts)? {
            any_hit = true;
            if opts.quiet {
                break;
            }
        }
    }

    out.flush()?;
    Ok(if any_hit {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Search one file's `content`, emitting whatever the active output mode calls
/// for. Returns whether the file contained at least one (post-`-v`) match.
fn search_file(
    out: &mut impl Write,
    content: &[u8],
    name: &[u8],
    binary: bool,
    needles: &[Vec<u8>],
    opts: &Opts,
) -> Result<bool> {
    let mut count = 0usize;
    let mut hit = false;
    // Once a binary file is known to match, git prints a single notice in place
    // of the matching lines and moves on; the counting modes are unaffected.
    let mut binary_notice_pending = binary;

    for (lno, line) in lines(content).enumerate() {
        let first = first_match(line, needles, opts);
        let matched = first.is_some() != opts.invert;
        if !matched {
            continue;
        }
        hit = true;
        count += 1;

        if opts.quiet || opts.files_with || opts.files_without {
            break;
        }
        if opts.count {
            continue;
        }
        if binary_notice_pending {
            binary_notice_pending = false;
            out.write_all(b"Binary file ")?;
            out.write_all(name)?;
            out.write_all(b" matches\n")?;
            break;
        }

        if opts.only_matching {
            let needle = &needles[0];
            let mut at = 0usize;
            while let Some((start, len)) = find_from(line, needle, at, opts) {
                if len == 0 {
                    break; // an empty pattern has no non-empty part to show
                }
                write_prefix(out, name, lno + 1, start + 1, opts)?;
                out.write_all(&line[start..start + len])?;
                out.write_all(b"\n")?;
                at = start + len;
            }
        } else {
            write_prefix(out, name, lno + 1, first.unwrap_or(0) + 1, opts)?;
            out.write_all(line)?;
            out.write_all(b"\n")?;
        }
    }

    // git's precedence: -q suppresses all output, then -L, then -l, then -c.
    if opts.quiet {
        return Ok(hit);
    }
    let term: &[u8] = if opts.nul { b"\0" } else { b"\n" };
    if opts.files_without {
        if !hit {
            out.write_all(name)?;
            out.write_all(term)?;
        }
        return Ok(hit);
    }
    if opts.files_with {
        if hit {
            out.write_all(name)?;
            out.write_all(term)?;
        }
        return Ok(hit);
    }
    if opts.count && count > 0 {
        if opts.show_names {
            out.write_all(name)?;
            out.write_all(if opts.nul { b"\0" } else { b":" })?;
        }
        writeln!(out, "{count}")?;
    }
    Ok(hit)
}

/// Emit the `<name><sep><lineno><sep><column><sep>` header of a match line.
/// With `-z` every separator is a NUL instead of `:`, exactly as git's
/// `show_line()` does when `null_following_name` is set.
fn write_prefix(
    out: &mut impl Write,
    name: &[u8],
    lno: usize,
    column: usize,
    opts: &Opts,
) -> Result<()> {
    let sep: &[u8] = if opts.nul { b"\0" } else { b":" };
    if opts.show_names {
        out.write_all(name)?;
        out.write_all(sep)?;
    }
    if opts.line_number {
        write!(out, "{lno}")?;
        out.write_all(sep)?;
    }
    if opts.column {
        write!(out, "{column}")?;
        out.write_all(sep)?;
    }
    Ok(())
}

/// Split `content` the way git does: on `\n`, with a trailing newline *not*
/// producing a final empty line, and an empty file producing no lines at all.
fn lines(content: &[u8]) -> impl Iterator<Item = &[u8]> {
    let body = content.strip_suffix(b"\n").unwrap_or(content);
    let empty = content.is_empty();
    body.split(|&b| b == b'\n')
        .take(if empty { 0 } else { usize::MAX })
}

/// The 0-based offset of the earliest match of any pattern in `line`.
fn first_match(line: &[u8], needles: &[Vec<u8>], opts: &Opts) -> Option<usize> {
    needles
        .iter()
        .filter_map(|n| find_from(line, n, 0, opts).map(|(start, _)| start))
        .min()
}

/// Find `needle` in `hay` at or after `from`, honouring `-i` and `-w`.
/// An empty needle matches at `from` with length zero (git: "an empty string as
/// search expression matches all lines").
fn find_from(hay: &[u8], needle: &[u8], from: usize, opts: &Opts) -> Option<(usize, usize)> {
    if from > hay.len() {
        return None;
    }
    let n = needle.len();
    if n == 0 {
        return Some((from, 0));
    }
    let mut i = from;
    while i + n <= hay.len() {
        let eq = if opts.ignore_case {
            hay[i..i + n]
                .iter()
                .zip(needle)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        } else {
            &hay[i..i + n] == needle
        };
        if eq && (!opts.word || word_bounded(hay, i, i + n)) {
            return Some((i, n));
        }
        i += 1;
    }
    None
}

/// Whether `hay[start..end]` sits on word boundaries, with git's word alphabet
/// (ASCII alphanumerics plus `_`).
fn word_bounded(hay: &[u8], start: usize, end: usize) -> bool {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    (start == 0 || !is_word(hay[start - 1])) && (end == hay.len() || !is_word(hay[end]))
}

/// git's `buffer_is_binary()`: a NUL within the first 8000 bytes.
fn is_binary(content: &[u8]) -> bool {
    let head = &content[..content.len().min(FIRST_FEW_BYTES)];
    head.contains(&0)
}

/// Reduce a pattern to the literal byte string it denotes, or fail.
///
/// `-F` patterns are literal by definition. In the regex dialects only patterns
/// free of that dialect's metacharacters are literal, and those are the only
/// ones this port can execute — there is no regex engine among the vendored
/// gitoxide crates to hand the rest to.
fn literal_of(pattern: &str, dialect: Dialect) -> Result<Vec<u8>> {
    let meta: &[char] = match dialect {
        Dialect::Fixed => &[],
        Dialect::Basic => &['.', '*', '[', ']', '^', '$', '\\'],
        Dialect::Extended => &[
            '.', '*', '[', ']', '^', '$', '\\', '+', '?', '{', '}', '(', ')', '|',
        ],
    };
    if let Some(c) = pattern.chars().find(|c| meta.contains(c)) {
        bail!(
            "pattern {pattern:?} contains the regex metacharacter {c:?}; \
             the vendored gitoxide crates ship no regex engine, so only literal \
             patterns are supported (use -F to match it literally)"
        );
    }
    Ok(pattern.as_bytes().to_vec())
}

/// The path as git prints it: repo-root-relative with the current-directory
/// prefix stripped, C-quoted unless `-z` asked for verbatim bytes.
fn display_name(path: &BStr, prefix: Option<&[u8]>, opts: &Opts) -> Vec<u8> {
    let bytes = path.as_bytes();
    let rel = match prefix {
        Some(p) if bytes.starts_with(p) => &bytes[p.len()..],
        _ => bytes,
    };
    if opts.nul {
        rel.to_vec()
    } else {
        quote_path(rel).into_bytes()
    }
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(bytes: &[u8]) -> String {
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
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// The terse rejection used for every flag this port does not implement.
fn unsupported(flag: &str) -> String {
    format!(
        "unsupported flag {flag:?} (ported: -e, -i, -v, -w, -a, -I, -n, --column, \
         -l/--files-with-matches/--name-only, -L/--files-without-match, -c, -q, -z, -o, \
         -h, -H, -E, -G, -F, --full-name, --cached, and pathspecs)"
    )
}
