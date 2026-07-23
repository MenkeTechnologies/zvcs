//! `git check-ignore` — report which of the given paths are excluded by the
//! gitignore machinery, and (with `-v`) which pattern did it.
//!
//! Backed by gitoxide's exclude stack (`Repository::excludes()` →
//! `gix_worktree::Stack`), which assembles the same sources stock git does:
//! `core.excludesFile` (or the XDG fallback), `$GIT_COMMON_DIR/info/exclude`,
//! and the per-directory `.gitignore` files along the path, with the same
//! precedence and the same "a parent directory is excluded" short-circuit.
//!
//! Covered, byte-for-byte against stock git: the whole documented flag set
//! (`-q`, `-v`, `-n`, `-z`, `--stdin`, `--no-index`, `--`) plus the negations
//! `parse-options` generates for each of them (`--no-quiet`, `--no-verbose`,
//! `--no-non-matching`, `--no-stdin`, and both `--no-no-index` and `--index`
//! for the `no-`-prefixed one), both output formats
//! (`<source>:<line>:<pattern>\t<path>` and its NUL-delimited variant), C-style
//! path quoting, the "non-matching" `::\t` records, the fatal argument-validation
//! errors (exit 128), the usage block with its unknown-option/unknown-switch and
//! `-h` exits (129), and the 0/1 exit convention.
//!
//! Not covered — refused, never faked: pathspec magic (`:(glob)…`) and pathspecs
//! containing wildcards, both of which git resolves through the full pathspec
//! machinery against the index before it ever consults the exclude stack; and
//! unique-prefix option abbreviation (`--verb`), whose ambiguity diagnostics
//! depend on `parse-options`' internal candidate ordering.

use anyhow::{bail, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};

/// Parsed command-line options for a single `check-ignore` invocation.
struct Opts {
    quiet: bool,        // -q/--quiet: no output, exit status only
    verbose: bool,      // -v/--verbose: print the matching pattern too
    non_matching: bool, // -n/--non-matching: also emit records for unmatched paths
    nul: bool,          // -z: NUL-delimited input (with --stdin) and output
    stdin: bool,        // --stdin: read paths from standard input
    no_index: bool,     // --no-index: do not skip paths that are tracked
}

/// The exclude pattern that decided a path, flattened out of the borrowed
/// `gix_ignore::search::Match` so the exclude stack can be re-borrowed.
struct Hit {
    source: Option<PathBuf>,
    line: usize,
    /// The pattern as git prints it: `!` prefix and `/` suffix restored.
    pattern: BString,
    negative: bool,
}

pub fn check_ignore(args: &[String]) -> Result<ExitCode> {
    // Tolerate both dispatch conventions (with or without the verb at index 0).
    let args = match args.first() {
        Some(a) if a == "check-ignore" => &args[1..],
        _ => args,
    };

    let mut o = Opts {
        quiet: false,
        verbose: false,
        non_matching: false,
        nul: false,
        stdin: false,
        no_index: false,
    };
    let mut cli_paths: Vec<BString> = Vec::new();
    let mut no_more_flags = false;

    for a in args {
        if !no_more_flags && a == "--" {
            no_more_flags = true;
            continue;
        }
        // A bare `-` is a pathname, not an option, exactly as in parse-options.
        if !no_more_flags && a.len() > 1 && a.starts_with('-') {
            if let Some(long) = a.strip_prefix("--") {
                // `--opt=value`: every option here is a boolean, so any attached
                // value is an error naming the option's canonical spelling.
                let (name, has_value) = match long.split_once('=') {
                    Some((n, _)) => (n, true),
                    None => (long, false),
                };
                // Each arm yields the name git echoes in diagnostics: the
                // option's own `long_name`, prefixed with `no-` when the
                // spelling is parse-options' generated negation.
                let canonical = match name {
                    "quiet" => {
                        o.quiet = true;
                        "quiet"
                    }
                    "no-quiet" => {
                        o.quiet = false;
                        "no-quiet"
                    }
                    "verbose" => {
                        o.verbose = true;
                        "verbose"
                    }
                    "no-verbose" => {
                        o.verbose = false;
                        "no-verbose"
                    }
                    "non-matching" => {
                        o.non_matching = true;
                        "non-matching"
                    }
                    "no-non-matching" => {
                        o.non_matching = false;
                        "no-non-matching"
                    }
                    "stdin" => {
                        o.stdin = true;
                        "stdin"
                    }
                    "no-stdin" => {
                        o.stdin = false;
                        "no-stdin"
                    }
                    "no-index" => {
                        o.no_index = true;
                        "no-index"
                    }
                    // Both the explicit double negation and the `--index` form
                    // parse-options derives from a `no-`-prefixed long name.
                    "no-no-index" | "index" => {
                        o.no_index = false;
                        "no-no-index"
                    }
                    // `--help` is not handled here: stock git intercepts it above
                    // the builtin and renders the man page, which this layer
                    // cannot reproduce, so it is left to fall through.
                    _ => return Ok(usage_error(&format!("unknown option `{name}'"))),
                };
                if has_value {
                    return Ok(usage_error(&format!(
                        "option `{canonical}' takes no value"
                    )));
                }
            } else {
                // Short flags may be bundled, e.g. `-vn`.
                for c in a[1..].chars() {
                    match c {
                        'q' => o.quiet = true,
                        'v' => o.verbose = true,
                        'n' => o.non_matching = true,
                        'z' => o.nul = true,
                        'h' => return Ok(show_usage()),
                        _ => return Ok(usage_error(&format!("unknown switch `{c}'"))),
                    }
                }
            }
            continue;
        }
        cli_paths.push(BString::from(a.as_str()));
    }

    // Argument validation, in git's own order so the reported error matches.
    if o.stdin {
        if !cli_paths.is_empty() {
            return Ok(fatal("cannot specify pathnames with --stdin"));
        }
    } else {
        if o.nul {
            return Ok(fatal("-z only makes sense with --stdin"));
        }
        if cli_paths.is_empty() {
            return Ok(fatal("no path specified"));
        }
    }
    if o.quiet {
        if cli_paths.len() > 1 {
            return Ok(fatal("--quiet is only valid with a single pathname"));
        }
        if o.verbose {
            return Ok(fatal("cannot have both --quiet and --verbose"));
        }
    }
    if o.non_matching && !o.verbose {
        return Ok(fatal("--non-matching is only valid with --verbose"));
    }

    let originals = if o.stdin {
        match read_stdin_paths(o.nul)? {
            Some(paths) => paths,
            None => return Ok(fatal("line is badly quoted")),
        }
    } else {
        cli_paths
    };

    let repo = gix::discover(".")?;
    let Some(workdir) = repo.workdir().map(Path::to_path_buf) else {
        return Ok(fatal("this operation must be run in a work tree"));
    };

    // The repository root as an absolute path, derived by walking the cwd up by
    // as many components as the repo-root-to-cwd prefix has. `current_dir()` is
    // already absolute, so this needs no symlink resolution of its own.
    let cwd = std::env::current_dir()?;
    let prefix = repo.prefix()?.map(Path::to_path_buf).unwrap_or_default();
    let mut root_abs = cwd.clone();
    for _ in prefix.components() {
        root_abs.pop();
    }
    let prefix_b = gix::path::into_bstr(prefix.as_path()).into_owned();
    let root_b = gix::path::into_bstr(root_abs.as_path()).into_owned();

    let index = repo.index_or_empty()?;
    let mut stack = repo.excludes(
        &index,
        None,
        gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
    )?;

    let mut out: Vec<u8> = Vec::new();
    let mut num_ignored = 0usize;

    for orig in &originals {
        if orig.starts_with(b":") {
            return Ok(fatal(&format!(
                "{orig}: pathspec magic is not supported by check-ignore"
            )));
        }
        if orig.iter().any(|&b| matches!(b, b'*' | b'?' | b'[')) {
            bail!("pathspec with wildcards is not supported: {orig:?}");
        }

        let rel = match to_repo_relative(orig.as_bstr(), prefix_b.as_bstr(), root_b.as_bstr()) {
            Some(rel) => rel,
            None => {
                return Ok(fatal(&format!(
                    "{orig}: '{orig}' is outside repository at '{root_b}'"
                )))
            }
        };

        // git skips tracked paths entirely unless --no-index: they are not
        // subject to exclude rules, so they neither print nor affect the exit
        // code. A pathspec also "matches" the index when it names a directory
        // that contains a tracked entry.
        let tracked = !o.no_index && index_has(&index, rel.as_bstr());

        let mut hit = if tracked || rel.is_empty() {
            None
        } else {
            // Trailing `/` forces directory semantics; otherwise git lstat()s the
            // path, so a symlink-to-directory is *not* a directory here.
            let on_disk = gix::path::from_bstr(orig.as_bstr());
            let is_dir = orig.ends_with(b"/")
                || std::fs::symlink_metadata(&*on_disk)
                    .map(|m| m.is_dir())
                    .unwrap_or(false);
            let mode = if is_dir {
                gix::index::entry::Mode::DIR
            } else {
                gix::index::entry::Mode::FILE
            };
            let plat = stack.at_entry(rel.as_bstr(), Some(mode))?;
            plat.matching_exclude_pattern().map(|m| Hit {
                source: m.source.map(Path::to_path_buf),
                line: m.sequence_number,
                pattern: render_pattern(m.pattern),
                negative: m.pattern.is_negative(),
            })
        };

        // Without -v a negated pattern is reported as "no match at all"; with -v
        // it is shown, and still counts towards the exit status.
        if !o.verbose && hit.as_ref().is_some_and(|h| h.negative) {
            hit = None;
        }
        if hit.is_some() {
            num_ignored += 1;
        }
        if !o.quiet && (hit.is_some() || o.non_matching) {
            emit(&mut out, orig.as_bstr(), hit.as_ref(), &o, &workdir, &root_abs);
        }
    }

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;

    Ok(if num_ignored > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Report a fatal usage error exactly as git does and yield its exit code.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// git's `parse-options` usage block for this command, laid out in the same
/// two-column form (descriptions start at column 26, an option whose flag list
/// reaches that column wraps onto its own line) and ending in a blank line.
const USAGE: &str = "\
usage: git check-ignore [<options>] <pathname>...
   or: git check-ignore [<options>] --stdin

    -q, --[no-]quiet      suppress progress reporting
    -v, --[no-]verbose    be verbose

    --[no-]stdin          read file names from stdin
    -z                    terminate input and output records by a NUL character
    -n, --[no-]non-matching
                          show non-matching input paths
    --no-index            ignore index when checking
    --index               opposite of --no-index

";

/// `-h`/`--help`: the usage block goes to standard output, and the exit code is
/// still the usage-error 129.
fn show_usage() -> ExitCode {
    print!("{USAGE}");
    let _ = std::io::stdout().flush();
    ExitCode::from(129)
}

/// A command-line parsing error: the `error:` line and the usage block both go
/// to standard error, with git's usage exit code.
fn usage_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE}");
    ExitCode::from(129)
}

/// Read the path list from standard input: NUL-separated with `-z`, otherwise
/// one per line with `"…"`-quoted lines decoded, matching git's `--stdin`.
///
/// `None` signals a line that opens a quote it never closes — git's
/// `fatal: line is badly quoted`.
fn read_stdin_paths(nul: bool) -> Result<Option<Vec<BString>>> {
    let mut buf = Vec::new();
    std::io::stdin().lock().read_to_end(&mut buf)?;
    let sep = if nul { b'\0' } else { b'\n' };

    let mut chunks: Vec<&[u8]> = buf.split(|&b| b == sep).collect();
    // A trailing separator yields one empty tail chunk that is not a record.
    if buf.last() == Some(&sep) {
        chunks.pop();
    }

    let mut paths = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let mut line: &[u8] = chunk;
        if !nul {
            if let Some(stripped) = line.strip_suffix(b"\r") {
                line = stripped;
            }
            if line.starts_with(b"\"") {
                match unquote_c_style(line) {
                    Some(p) => {
                        paths.push(p);
                        continue;
                    }
                    None => return Ok(None),
                }
            }
        }
        paths.push(BString::from(line));
    }
    Ok(Some(paths))
}

/// Decode a `"…"`-wrapped C-quoted path (git's `unquote_c_style`).
fn unquote_c_style(quoted: &[u8]) -> Option<BString> {
    let inner = quoted.strip_prefix(b"\"")?;
    let mut out: Vec<u8> = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        match inner[i] {
            b'"' => return Some(out.into()),
            b'\\' => {
                i += 1;
                let e = *inner.get(i)?;
                i += 1;
                match e {
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(0x0b),
                    b'"' | b'\\' => out.push(e),
                    b'0'..=b'7' => {
                        let mut v = u32::from(e - b'0');
                        for _ in 0..2 {
                            match inner.get(i).copied() {
                                Some(d) if (b'0'..=b'7').contains(&d) => {
                                    v = v * 8 + u32::from(d - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push(u8::try_from(v).ok()?);
                    }
                    _ => return None,
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    None // unterminated
}

/// Resolve a caller-supplied path to the repository-relative path git matches
/// against, mirroring git's pathspec prefixing. Returns `None` when the path
/// escapes the repository root (git's "is outside repository" fatal).
///
/// Both branches normalise lexically — `.` and `..` are folded without touching
/// the filesystem, which is what git's pathspec normalisation does too.
fn to_repo_relative(orig: &BStr, prefix: &BStr, root: &BStr) -> Option<BString> {
    if orig.starts_with(b"/") {
        // Absolute: normalise both sides and strip the repository root off.
        let arg = normalize(orig)?;
        let root = normalize(root)?;
        if arg == root {
            return Some(BString::default());
        }
        let mut root_prefix = root.to_vec();
        root_prefix.push(b'/');
        return arg
            .strip_prefix(root_prefix.as_slice())
            .map(BString::from);
    }
    let joined: Vec<u8> = if prefix.is_empty() {
        orig.to_vec()
    } else {
        let mut v = prefix.to_vec();
        v.push(b'/');
        v.extend_from_slice(orig);
        v
    };
    normalize(&joined)
}

/// Fold `.` and empty components away and pop one component per `..`.
/// Returns `None` if a `..` would escape past the start of the path.
fn normalize(path: &[u8]) -> Option<BString> {
    let mut parts: Vec<&[u8]> = Vec::new();
    for comp in path.split(|&b| b == b'/') {
        match comp {
            b"" | b"." => {}
            b".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(BString::from(parts.join(&b'/')))
}

/// Whether the index holds `rel` itself or anything beneath it — git's
/// `find_pathspecs_matching_against_index` for a literal pathspec.
fn index_has(index: &gix::index::State, rel: &BStr) -> bool {
    if rel.is_empty() {
        return !index.entries().is_empty();
    }
    let mut dir_prefix = rel.to_vec();
    dir_prefix.push(b'/');
    index.entries().iter().any(|e| {
        let p = e.path(index);
        p == rel || p.starts_with(&dir_prefix)
    })
}

/// Re-render a parsed pattern the way git prints it: gitoxide strips the leading
/// `!`, the leading `/` and the trailing `/` into flags, git keeps them in the
/// text it echoes back.
fn render_pattern(p: &gix::glob::Pattern) -> BString {
    use gix::glob::pattern::Mode;
    let mut out: Vec<u8> = Vec::with_capacity(p.text.len() + 3);
    if p.mode.contains(Mode::NEGATIVE) {
        out.push(b'!');
    }
    if p.mode.contains(Mode::ABSOLUTE) {
        out.push(b'/');
    }
    out.extend_from_slice(&p.text);
    if p.mode.contains(Mode::MUST_BE_DIR) {
        out.push(b'/');
    }
    out.into()
}

/// The `<source>` column: repository-root relative for in-tree `.gitignore`
/// files and `.git/info/exclude`, left as configured (typically absolute) for
/// `core.excludesFile`, exactly as git prints it.
fn source_display(src: &Path, workdir: &Path, root_abs: &Path) -> BString {
    for base in [workdir, root_abs] {
        if let Ok(rel) = src.strip_prefix(base) {
            return gix::path::into_bstr(rel).into_owned();
        }
    }
    gix::path::into_bstr(src).into_owned()
}

/// Append one output record for `orig`, in whichever of the four format
/// combinations (`-v` × `-z`) is active.
fn emit(
    out: &mut Vec<u8>,
    orig: &BStr,
    hit: Option<&Hit>,
    o: &Opts,
    workdir: &Path,
    root_abs: &Path,
) {
    if o.nul {
        if o.verbose {
            match hit {
                Some(h) => {
                    if let Some(src) = &h.source {
                        out.extend_from_slice(&source_display(src, workdir, root_abs));
                    }
                    out.push(0);
                    out.extend_from_slice(h.line.to_string().as_bytes());
                    out.push(0);
                    out.extend_from_slice(&h.pattern);
                    out.push(0);
                }
                None => out.extend_from_slice(&[0, 0, 0]),
            }
        }
        out.extend_from_slice(orig);
        out.push(0);
        return;
    }

    if o.verbose {
        match hit {
            Some(h) => {
                let src = h
                    .source
                    .as_deref()
                    .map(|s| source_display(s, workdir, root_abs))
                    .unwrap_or_default();
                out.extend_from_slice(&quote_c_style(&src));
                out.extend_from_slice(format!(":{}:", h.line).as_bytes());
                out.extend_from_slice(&h.pattern);
                out.push(b'\t');
            }
            None => out.extend_from_slice(b"::\t"),
        }
    }
    out.extend_from_slice(&quote_c_style(orig));
    out.push(b'\n');
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_c_style(path: &[u8]) -> Vec<u8> {
    let needs = path
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return path.to_vec();
    }
    let mut out: Vec<u8> = vec![b'"'];
    for &b in path {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x07 => out.extend_from_slice(b"\\a"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x09 => out.extend_from_slice(b"\\t"),
            0x0a => out.extend_from_slice(b"\\n"),
            0x0b => out.extend_from_slice(b"\\v"),
            0x0c => out.extend_from_slice(b"\\f"),
            0x0d => out.extend_from_slice(b"\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => {
                out.extend_from_slice(format!("\\{b:03o}").as_bytes());
            }
            b => out.push(b),
        }
    }
    out.push(b'"');
    out
}
