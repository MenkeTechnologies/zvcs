//! `git check-mailmap` — show canonical names and email addresses of contacts.
//!
//! A faithful port of git's `builtin/check-mailmap.c` together with
//! `split_ident_line()` from `ident.c` (which decides what part of a contact is
//! the name and what part is the email). Reading the mailmap sources and
//! `map_user()` are the shared [`crate::mailmap`] port of `mailmap.c`.
//!
//! Covered: `<contact>...`, `--stdin`, `--mailmap-file=<file>`,
//! `--mailmap-blob=<blob>`, their `--no-` forms, `--`, `-h`, unique-prefix
//! abbreviation of long options, and the mailmap source order git uses
//! (worktree `.mailmap`, then `mailmap.blob` — defaulting to `HEAD:.mailmap` in
//! a bare repository — then `mailmap.file`, then `--mailmap-blob`, then
//! `--mailmap-file`, each later source overriding earlier ones). Exit codes
//! match: 0 on success, 128 for `fatal: no contacts specified`, 129 for every
//! usage error and for `-h`.

use anyhow::Result;
use std::io::{BufWriter, Read, Write};
use std::process::ExitCode;

use crate::mailmap::Mailmap;

/// The exact `usage_with_options` block git prints for `-h` and usage errors.
const USAGE: &str = "\
usage: git check-mailmap [<options>] <contact>...

    --[no-]stdin          also read contacts from stdin
    --[no-]mailmap-file <file>
                          read additional mailmap entries from file
    --[no-]mailmap-blob <blob>
                          read additional mailmap entries from blob

";

/// The long options git's parse-options table declares, in declaration order —
/// which is also the order its ambiguity message lists candidates in.
const LONG_OPTS: [&str; 3] = ["stdin", "mailmap-file", "mailmap-blob"];

/// Parsed command-line options for a single `check-mailmap` invocation.
struct Opts {
    stdin: bool,                       // --stdin: read contacts from stdin too
    mailmap_file: Option<String>,      // --mailmap-file=<file>
    mailmap_blob: Option<String>,      // --mailmap-blob=<blob>
    contacts: Vec<String>,             // positional `<contact>...`
}

/// `git check-mailmap` — map each contact through the mailmap and print it.
///
/// For every `Name <user@host>`, `<user@host>` or bare `user@host`, the
/// canonical form is printed as `Name <user@host>`, or as `<user@host>` when
/// neither the input nor the mailmap supplies a name.
pub fn check_mailmap(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts {
        stdin: false,
        mailmap_file: None,
        mailmap_blob: None,
        contacts: Vec::new(),
    };

    // Dispatch already strips the verb, so every element is a real argument.
    let rest = args;
    let mut no_more_opts = false;
    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();

        if no_more_opts || a == "-" || !a.starts_with('-') {
            opts.contacts.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            i += 1;
            continue;
        }
        // `parse_options_step()` tests `--help-all` with a `strcmp()` of its own,
        // ahead of `parse_long_opt()`: the name never abbreviates and never takes
        // an `=<value>`. This table has no `PARSE_OPT_HIDDEN` entry, so
        // `USAGE_FULL` renders the same block `-h` prints.
        if a == "--help-all" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }

        if let Some(long) = a.strip_prefix("--") {
            // Split `--opt=value` before resolving the (possibly abbreviated) name.
            let (given, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let (name, negated) = match given.strip_prefix("no-") {
                // `--no-mailmap-file` negates; a literal option never starts with `no-`.
                Some(stripped) if !stripped.is_empty() => (stripped, true),
                _ => (given, false),
            };

            let resolved = match resolve_long(name) {
                Ok(n) => n,
                Err(Ambiguity::Unknown) => {
                    return Ok(usage_error(&format!("unknown option `{}'", &a[2..])));
                }
                // The ambiguity report is the odd one out: the explanation is
                // `error()` on stderr, but the block reaches
                // `usage_with_options_internal(..., USAGE_TO_STDOUT)` and lands
                // on **stdout**. `usage_error` puts both on stderr, which is the
                // `unknown option` shape and not this one.
                Err(Ambiguity::Multiple(cands)) => {
                    return Ok(super::ambiguous_option(a, cands[0], cands[1], USAGE));
                }
            };

            match resolved {
                "stdin" => {
                    if inline.is_some() {
                        eprintln!("error: option `stdin' takes no value");
                        return Ok(ExitCode::from(129));
                    }
                    opts.stdin = !negated;
                }
                slot @ ("mailmap-file" | "mailmap-blob") => {
                    let value = if negated {
                        None
                    } else {
                        match inline {
                            Some(v) => Some(v.to_string()),
                            None => {
                                i += 1;
                                let Some(v) = rest.get(i) else {
                                    eprintln!("error: option `{slot}' requires a value");
                                    return Ok(ExitCode::from(129));
                                };
                                Some(v.clone())
                            }
                        }
                    };
                    if slot == "mailmap-file" {
                        opts.mailmap_file = value;
                    } else {
                        opts.mailmap_blob = value;
                    }
                }
                _ => unreachable!("resolve_long only yields LONG_OPTS entries"),
            }
            i += 1;
            continue;
        }

        // Short switches: `-h` is the only one check-mailmap declares, and git
        // reports the first unrecognised switch of a bundle.
        match a[1..].chars().next() {
            Some('h') => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            Some(c) => return Ok(usage_error(&format!("unknown switch `{c}'"))),
            None => {}
        }
        i += 1;
    }

    // git runs repository setup before the contact check, so an invocation with
    // no contacts outside a repository reports the missing repository first.
    let repo = crate::setup::discover()?;

    if opts.contacts.is_empty() && !opts.stdin {
        eprintln!("fatal: no contacts specified");
        return Ok(ExitCode::from(128));
    }

    // cmd_check_mailmap() (builtin/check-mailmap.c:66-70): the configured
    // sources, then `--mailmap-blob`, then `--mailmap-file`.
    let mut map = Mailmap::read(Some(&repo));
    if let Some(blob) = &opts.mailmap_blob {
        map.read_blob(&repo, blob);
    }
    if let Some(file) = &opts.mailmap_file {
        map.read_file(file);
    }

    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    for contact in &opts.contacts {
        emit(&mut out, &map, contact.as_bytes())?;
    }

    if opts.stdin {
        let mut buf = Vec::new();
        std::io::stdin().lock().read_to_end(&mut buf)?;
        for line in buf.split_inclusive(|&b| b == b'\n') {
            // `strbuf_getline`: strip the trailing LF, then a trailing CR.
            let line = line.strip_suffix(b"\n").unwrap_or(line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            emit(&mut out, &map, line)?;
        }
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Why a long option name could not be resolved to exactly one option.
enum Ambiguity {
    /// No declared option starts with the given name.
    Unknown,
    /// Several do; the first two are the ones git names in its message.
    Multiple([&'static str; 2]),
}

/// Resolve a possibly-abbreviated long option name against [`LONG_OPTS`], using
/// git's parse-options rule: an exact match wins, otherwise a unique prefix.
fn resolve_long(name: &str) -> Result<&'static str, Ambiguity> {
    if let Some(exact) = LONG_OPTS.iter().copied().find(|o| *o == name) {
        return Ok(exact);
    }
    let matches: Vec<&'static str> = LONG_OPTS
        .iter()
        .copied()
        .filter(|o| o.starts_with(name))
        .collect();
    match matches.len() {
        0 => Err(Ambiguity::Unknown),
        1 => Ok(matches[0]),
        _ => Err(Ambiguity::Multiple([matches[0], matches[1]])),
    }
}

/// git's `error: ...` line followed by the usage block, on stderr, exit 129.
fn usage_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE}");
    ExitCode::from(129)
}

/// One `check_mailmap()` call: split the contact, map it, print the result.
fn emit(out: &mut impl Write, map: &Mailmap, contact: &[u8]) -> Result<()> {
    // git hands `split_ident_line` a C string, so an interior NUL ends it.
    let contact = match contact.iter().position(|&b| b == 0) {
        Some(n) => &contact[..n],
        None => contact,
    };
    // On a contact without a `<...>` pair git falls back to treating the whole
    // string as the email and carrying no name (so `user@host` still maps).
    let (mut name, mut mail): (&[u8], &[u8]) =
        split_ident(contact).unwrap_or((b"".as_slice(), contact));

    map.map_user(&mut mail, &mut name);

    if !name.is_empty() {
        out.write_all(name)?;
        out.write_all(b" ")?;
    }
    out.write_all(b"<")?;
    out.write_all(mail)?;
    out.write_all(b">\n")?;
    Ok(())
}

/// git's `split_ident_line`, reduced to the two spans `check-mailmap` uses.
///
/// The name runs from the start of the line (leading whitespace included, as in
/// git) to the last non-space byte before the first `<`; the email runs from
/// that `<` to the first following `>`. Returns `None` when either bracket is
/// missing, which is git's `-1` status.
fn split_ident(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let lt = line.iter().position(|&b| b == b'<')?;
    let name_end = line[..lt]
        .iter()
        .rposition(|b| !is_space(*b))
        .map_or(0, |i| i + 1);
    let mail_begin = lt + 1;
    let gt = line[mail_begin..].iter().position(|&b| b == b'>')? + mail_begin;
    Some((&line[..name_end], &line[mail_begin..gt]))
}

/// `isspace` in the C locale, which is what git's ident parser sees.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}
