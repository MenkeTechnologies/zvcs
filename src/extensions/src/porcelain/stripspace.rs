//! `git stripspace` — clean text the way git cleans commit messages.
//!
//! A faithful port of `builtin/stripspace.c` together with the two strbuf
//! helpers it drives: `strbuf_stripspace()` (default and `-s`) and
//! `strbuf_add_commented_lines()` → `add_lines()` (`-c`). Stdin is read whole,
//! transformed, and written to stdout; nothing else is touched, so there is no
//! repository state to match afterwards.
//!
//! ### Covered (byte-identical stdout, stderr and exit code against stock git)
//!
//! * no arguments — strip trailing whitespace per line, collapse runs of blank
//!   lines to one, drop leading and trailing blank lines, and terminate the last
//!   line with `\n`. All-whitespace input produces no output at all.
//! * `-s` / `--strip-comments` — additionally drop every line that starts with
//!   the comment string. A dropped comment line is fully transparent: it does
//!   not count as a blank-line separator, which is what makes the manual's
//!   example collapse the way it does.
//! * `-c` / `--comment-lines` — prefix every line with the comment string and a
//!   space, except lines beginning with `\n` or `\t`, which get the comment
//!   string alone; whitespace is preserved verbatim and a final `\n` is added
//!   when missing. Empty input stays empty.
//! * `core.commentChar` / `core.commentString` (equivalent multi-byte strings in
//!   git 2.55, last one set wins across both spellings), with `auto` resolving to
//!   `#` and `#` as the default. Read only for `-s`/`-c`, exactly as git does —
//!   the default mode never opens a repository and never touches config, so it
//!   works outside a repository and ignores an invalid comment string.
//! * `--` ends option parsing; a bare `-` is a positional. `-h` anywhere prints
//!   git's usage block on stdout and exits 129; any positional argument prints
//!   the same block on stderr and exits 129.
//! * Option errors in git's own shapes: `` error: unknown option `x' `` and
//!   `` error: unknown switch `x' `` followed by the usage block (129);
//!   `` error: option `strip-comments' takes no value `` (129); and the
//!   cmdmode conflict `error: options '-c' and '-s' cannot be used together`
//!   with no usage block (129), which names each option with the spelling it was
//!   given. Repeating the same mode is not an error.
//! * Unambiguous long-option abbreviations (`--strip`, `--comment`), as
//!   `parse_options` accepts them; the error text still names the full option.
//!   An ambiguous name (`--=x`, whose option name is empty and so prefixes both)
//!   reproduces git's odd split: `error: ambiguous option: …` on stderr with the
//!   usage block on stdout, exit 129.
//!
//! ### Honest limitations
//!
//! * A rejected comment string (empty, or containing a newline) reports git's
//!   `error:` line verbatim and exits 128, but the following `fatal:` line omits
//!   the config line number for file-backed sections — `gix_config` does not
//!   expose one — so it reads `bad config variable '<var>' in file '<path>'`
//!   where git appends ` at line <n>`. The command-line and environment forms
//!   match git exactly.

use anyhow::Result;
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::config::{File as ConfigFile, Source};

/// Stock git's `stripspace` usage block, byte-for-byte, including the trailing
/// blank line. Printed on `-h` (stdout) and for a usage error (stderr).
const USAGE: &str = "usage: git stripspace [-s | --strip-comments]\n\
                     \x20  or: git stripspace [-c | --comment-lines]\n\
                     \n\
                     \x20   -s, --strip-comments  skip and remove all lines starting with comment character\n\
                     \x20   -c, --comment-lines   prepend comment character and space to each line\n\
                     \n";

/// git's exit code for a `die()`.
const FATAL: u8 = 128;

/// git's compiled-in comment string, and what `auto` resolves to here.
const DEFAULT_COMMENT: &[u8] = b"#";

/// The `OPT_CMDMODE` value selected by `-s` / `-c`.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Default,
    StripComments,
    CommentLines,
}

/// `git stripspace` — read stdin, clean it, write stdout.
pub fn stripspace(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "stripspace" => &args[1..],
        _ => args,
    };

    let mut mode = Mode::Default;
    // The spelling the mode was given with, for the cmdmode conflict message.
    let mut mode_spelling = String::new();
    let mut saw_positional = false;
    let mut no_more_opts = false;

    for arg in args {
        let arg = arg.as_str();
        // A bare `-` is a positional to `parse_options`, not an option.
        if no_more_opts || arg == "-" || !arg.starts_with('-') {
            saw_positional = true;
            continue;
        }

        if let Some(long) = arg.strip_prefix("--") {
            if long.is_empty() {
                no_more_opts = true;
                continue;
            }
            let (name, value) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let (opt_mode, full_name) = match resolve_long(name) {
                Resolved::One(opt_mode, full_name) => (opt_mode, full_name),
                Resolved::Unknown => return Ok(usage_error(&format!("unknown option `{long}'"))),
                // git's ambiguity path is the odd one out: the reason goes to
                // stderr but the usage block goes to stdout.
                Resolved::Ambiguous(candidates) => {
                    eprintln!("error: ambiguous option: {long} (could be {candidates})");
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            };
            if value.is_some() {
                return Ok(usage_error(&format!("option `{full_name}' takes no value")));
            }
            let given = format!("--{full_name}");
            if let Err(code) = set_mode(&mut mode, &mut mode_spelling, opt_mode, &given) {
                return Ok(code);
            }
            continue;
        }

        // Short options group, e.g. `-sc`, and are handled left to right.
        for c in arg[1..].chars() {
            let opt_mode = match c {
                's' => Mode::StripComments,
                'c' => Mode::CommentLines,
                'h' => {
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                _ => return Ok(usage_error(&format!("unknown switch `{c}'"))),
            };
            let given = format!("-{c}");
            if let Err(code) = set_mode(&mut mode, &mut mode_spelling, opt_mode, &given) {
                return Ok(code);
            }
        }
    }

    // `stripspace` takes no positionals; any leftover is `usage_with_options`.
    if saw_positional {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // Only the comment-aware modes set up the repository and read config, so the
    // default mode neither needs a repository nor notices a bad comment string.
    let comment = match mode {
        Mode::Default => None,
        _ => match comment_string()? {
            Ok(comment) => Some(comment),
            Err(code) => return Ok(code),
        },
    };

    let mut input = Vec::new();
    std::io::stdin().lock().read_to_end(&mut input)?;

    let output = match mode {
        // `comment` is always populated for the two comment-aware modes.
        Mode::CommentLines => comment_lines(&input, comment.as_deref().unwrap_or(DEFAULT_COMMENT)),
        Mode::StripComments => strip_space(&input, comment.as_deref()),
        Mode::Default => strip_space(&input, None),
    };

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&output)?;
    stdout.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Apply an `OPT_CMDMODE` selection, reporting git's conflict error when a
/// different mode was already chosen. Repeating the same mode is not an error.
fn set_mode(
    mode: &mut Mode,
    spelling: &mut String,
    new: Mode,
    given: &str,
) -> Result<(), ExitCode> {
    if *mode == new {
        return Ok(());
    }
    if *mode != Mode::Default {
        // No usage block follows a cmdmode conflict, unlike other option errors.
        eprintln!("error: options '{given}' and '{spelling}' cannot be used together");
        return Err(ExitCode::from(129));
    }
    *mode = new;
    *spelling = given.to_string();
    Ok(())
}

/// The outcome of matching a long option name.
enum Resolved {
    One(Mode, &'static str),
    /// More than one option shares the prefix; carries git's `a or b` list.
    Ambiguous(String),
    Unknown,
}

/// Match a long option to its mode; `parse_options` accepts any unambiguous
/// prefix, and error messages always name the full option.
///
/// An empty name — reachable as `--=value` — prefixes both options and is
/// therefore ambiguous, which is what git reports for it too.
fn resolve_long(name: &str) -> Resolved {
    const SPELLINGS: [(&str, Mode); 2] = [
        ("strip-comments", Mode::StripComments),
        ("comment-lines", Mode::CommentLines),
    ];

    let mut hits: Vec<(Mode, &'static str)> = Vec::new();
    for (spelling, opt_mode) in SPELLINGS {
        if spelling == name {
            return Resolved::One(opt_mode, spelling);
        }
        if spelling.starts_with(name) {
            hits.push((opt_mode, spelling));
        }
    }

    match hits.len() {
        0 => Resolved::Unknown,
        1 => Resolved::One(hits[0].0, hits[0].1),
        // git lists the candidates in declaration order, joined with ` or `.
        _ => Resolved::Ambiguous(
            hits.iter()
                .map(|(_, spelling)| format!("--{spelling}"))
                .collect::<Vec<_>>()
                .join(" or "),
        ),
    }
}

/// git's parse-options failure shape: `error: <msg>` then the usage block on
/// stderr, exit 129.
fn usage_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE}");
    ExitCode::from(129)
}

/// `strbuf_stripspace()`.
///
/// Each line (its terminator included) is right-trimmed with git's `cleanup()`;
/// a line that trims to nothing only bumps the blank-line counter, so runs of
/// blanks collapse to one and leading and trailing blanks vanish. Every kept
/// line is re-terminated with `\n`.
///
/// When `comment` is set, a line starting with it is skipped outright — without
/// bumping the blank counter, so a comment between two paragraphs does not
/// become a blank line.
fn strip_space(input: &[u8], comment: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + 1);
    let mut empties = 0usize;

    for line in lines(input) {
        // The comment prefix can never contain `\n` (config rejects that), so
        // git's NUL-terminated `starts_with` over the rest of the buffer is
        // equivalent to testing this line alone.
        if comment.is_some_and(|c| line.starts_with(c)) {
            continue;
        }

        let trimmed = &line[..cleanup(line)];
        if trimmed.is_empty() {
            empties += 1;
            continue;
        }
        if empties > 0 && !out.is_empty() {
            out.push(b'\n');
        }
        empties = 0;
        out.extend_from_slice(trimmed);
        out.push(b'\n');
    }
    out
}

/// `strbuf_add_commented_lines()` → `add_lines()`.
///
/// Every line keeps its bytes verbatim and gains a prefix: `<comment> ` in
/// general, or a bare `<comment>` when the line starts with `\n` (it is empty)
/// or `\t`. `strbuf_complete_line()` then terminates a non-empty result.
fn comment_lines(input: &[u8], comment: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + input.len() / 8 + 1);

    for line in lines(input) {
        out.extend_from_slice(comment);
        if !matches!(line.first(), Some(b'\n' | b'\t')) {
            out.push(b' ');
        }
        out.extend_from_slice(line);
    }
    if !out.is_empty() && out.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    out
}

/// Split into lines the way git's `memchr`-driven loops do: each slice keeps its
/// trailing `\n`, and a final unterminated line is yielded as-is.
fn lines(input: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = input;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let len = match rest.iter().position(|&b| b == b'\n') {
            Some(offset) => offset + 1,
            None => rest.len(),
        };
        let (line, tail) = rest.split_at(len);
        rest = tail;
        Some(line)
    })
}

/// git's `cleanup()`: the length of `line` with trailing whitespace removed.
///
/// Whitespace is git's own `sane_ctype` `isspace`, which — unlike C's — marks
/// only space, `\t`, `\n` and `\r` as `GIT_SPACE`. Vertical tab and form feed
/// are therefore kept, and bytes above 0x7f are never trimmed whatever the
/// locale.
fn cleanup(line: &[u8]) -> usize {
    let mut len = line.len();
    while len > 0 && matches!(line[len - 1], b' ' | b'\t' | b'\n' | b'\r') {
        len -= 1;
    }
    len
}

/// Resolve the comment string the way `git_default_core_config()` does.
///
/// `core.commentChar` and `core.commentString` are equivalent in git 2.55 — both
/// take an arbitrary string — so the winner is whichever was set last in
/// configuration order, across both names. `auto` resolves to `#`, as does an
/// absent value. Every occurrence is validated as it is seen, so a bad value
/// that a later section overrides is still fatal.
///
/// The outer `Result` carries an I/O failure; the inner one carries git's
/// already-reported exit code.
fn comment_string() -> Result<Result<Vec<u8>, ExitCode>> {
    // Like `setup_git_directory_gently()`: a repository is preferred, but the
    // command is legal outside one, where git reads the global set plus the
    // `GIT_CONFIG_*` overrides.
    let config = match gix::discover(".") {
        Ok(repo) => repo.config_snapshot().plumbing().clone(),
        Err(_) => {
            let mut file = ConfigFile::from_globals()?;
            file.append(ConfigFile::from_environment_overrides()?)?;
            file
        }
    };

    let mut chosen = DEFAULT_COMMENT.to_vec();

    // `sections()` yields in merged configuration order, so a later hit wins.
    for section in config.sections() {
        let header = section.header();
        if header.subsection_name().is_some()
            || !header.name().to_string().eq_ignore_ascii_case("core")
        {
            continue;
        }

        // Recover the order the two names were written in: value names come out
        // in order, and each name's values come out in order, so walking the
        // names while advancing per-name cursors interleaves them correctly.
        let body = section.body();
        let chars = body.values("commentChar");
        let strings = body.values("commentString");
        let (mut char_at, mut string_at) = (0usize, 0usize);

        for value_name in body.value_names() {
            let (var, value) = if value_name.eq_ignore_ascii_case("commentChar") {
                let value = chars.get(char_at);
                char_at += 1;
                ("core.commentchar", value)
            } else if value_name.eq_ignore_ascii_case("commentString") {
                let value = strings.get(string_at);
                string_at += 1;
                ("core.commentstring", value)
            } else {
                continue;
            };
            let Some(value) = value else {
                continue; // valueless entry; nothing for git to accept either
            };

            let value: &[u8] = value.as_slice();
            if value.eq_ignore_ascii_case(b"auto") {
                chosen = DEFAULT_COMMENT.to_vec();
            } else if value.is_empty() {
                return Ok(Err(config_fatal(
                    var,
                    "must have at least one character",
                    section.meta(),
                )));
            } else if value.contains(&b'\n') {
                return Ok(Err(config_fatal(
                    var,
                    "cannot contain newline",
                    section.meta(),
                )));
            } else {
                chosen = value.to_vec();
            }
        }
    }

    Ok(Ok(chosen))
}

/// Report a rejected comment string as git does — the `error:` reason, then a
/// `fatal:` naming where the value came from — and yield exit 128.
fn config_fatal(var: &str, reason: &str, meta: &gix::config::file::Metadata) -> ExitCode {
    let origin = match meta.source {
        Source::Cli | Source::Env => format!("unable to parse '{var}' from command-line config"),
        _ => match &meta.path {
            Some(path) => format!("bad config variable '{var}' in file '{}'", path.display()),
            None => format!("bad config variable '{var}'"),
        },
    };
    eprintln!("error: {var} {reason}");
    eprintln!("fatal: {origin}");
    ExitCode::from(FATAL)
}
