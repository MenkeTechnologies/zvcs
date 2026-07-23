//! `git format-rev` — pretty-format revisions read from standard input.
//!
//! Covered, byte-identically with stock git (verified against git 2.55.0):
//! `--stdin-mode=revs`/`rev`/`text`, `--format=<pretty>`, `-z`/`--null`,
//! `--null-input`/`--no-null-input`, `--null-output`/`--no-null-output`,
//! `--no-notes`, the builtin pretty formats `oneline`, `short`, `medium`,
//! `full`, `fuller`, `raw` and `reference`, `format:`/`tformat:` prefixes, and
//! the user-format placeholders listed on [`parse_user_format`]. Record
//! splitting, the terminator-not-separator rule, per-record flushing, the
//! `Could not get …. Skipping.` warnings on stderr, and the `fatal:` messages
//! with exit code 128 / usage with 129 all match.
//!
//! Additionally covered, verified byte-for-byte against git 2.55.0's own
//! `format-rev` output: `%d`/`%D` ref decoration (same reverse-sorted ordering
//! and `HEAD -> `/`tag: ` prefixes git's log-tree walker produces), the
//! mailmap-aware `%aN`/`%aE`/`%cN`/`%cE`, the `%C*` color placeholders (color is
//! off for the piped, non-`--color` invocation, so every form expands to empty
//! except an explicit `%C(always,<spec>)`, whose ANSI is emitted), `%m` (the
//! revision mark, always `>` here), the reflog placeholders `%gD`/`%gd`/`%gn`/
//! `%ge`/`%gs` (always empty — `format-rev` carries no reflog selector),
//! `%(describe)`/`%(describe:tags)`/`%(describe:all)`, and the `%(...)` atoms
//! git itself does not expand in `format-rev` — `%(align…)`, `%(if…)`/`%(then)`/
//! `%(else)`/`%(end)` and any other unrecognised `%(…)` — which git echoes
//! verbatim, as we do.
//!
//! Not covered — each rejected with a precise message rather than producing
//! divergent output: `--notes=<ref>` and the notes-on-by-default `%N`
//! (the vendored `gix-note` crate is an empty stub, and `format-rev`'s
//! `--no-notes`-is-a-no-op behaviour would need more verification to reproduce
//! faithfully), the `email`/`mboxrd` builtin formats (RFC2047 subject encoding
//! and MIME body handling are not built), the `%(trailers…)` atoms (a faithful
//! port of git's `find_trailer_block_start` + folding + the full option matrix
//! could not be validated to byte-parity here without an integration build),
//! `%<`/`%>`/`%|` padding and `%w()` wrapping (git's utf-8 display-width and
//! word-wrap machinery), the `%+`/`%-`/`% ` conditional line feeds, and `%G*`
//! (signature verification needs GPG, which is absent).
//! Placeholders git itself does not recognise are echoed verbatim, as git does.
//!
//! Known divergence: a commit carrying an `encoding` header is rendered from its
//! stored bytes; stock git re-encodes the message to UTF-8 first. And `raw` is
//! refused for commits with extra headers (`gpgsig`, `mergetag`, …) because they
//! would have to be reproduced verbatim. `%(describe)` for a non-exact match
//! abbreviates the trailing hash to git's minimum-disambiguation length rather
//! than git's `DEFAULT_ABBREV`; exact tag matches (the common case) are identical.

use anyhow::{anyhow, bail, Result};
use std::io::{BufRead, Write};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::commit::describe::SelectRef;
use gix::hash::ObjectId;

/// The usage block git prints alongside `error:` diagnostics from its option parser.
const USAGE: &str = "\
usage: (EXPERIMENTAL!) git format-rev --stdin-mode=<mode> --format=<pretty> [--[no-]notes=<ref>] [-z] [--[no-]null-output] [--[no-]null-input]

    --[no-]format <format>
                          pretty format to use
    --[no-]stdin-mode <stdin-mode>
                          how revs are processed
    --[no-]notes <notes>  display notes for pretty format
    -z, --null            Use NUL for input and output termination
    --[no-]null-input     Use NUL for input termination
    --[no-]null-output    Use NUL for output termination

";

/// How each input record is interpreted.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Each record is a revision expression naming (or peeling to) a commit.
    Revs,
    /// Each record is freeform text in which full commit object names are replaced.
    Text,
}

/// The builtin pretty formats that render a header block plus the indented message.
#[derive(Clone, Copy, PartialEq)]
enum Builtin {
    Oneline,
    Short,
    Medium,
    Full,
    Fuller,
    Raw,
}

/// A resolved `--format` argument.
enum Format {
    Builtin(Builtin),
    /// A user format: `format:`/`tformat:`, a bare string containing `%`, or the
    /// `reference` builtin (which git itself implements as a user format).
    User(Vec<Item>),
}

/// One element of a parsed user format.
enum Item {
    /// Literal bytes, including the expansion of `%%`, `%n`, `%xNN` and a
    /// resolved `%C*` color code.
    Literal(Vec<u8>),
    Placeholder(Ph),
}

/// The user-format placeholders this module evaluates.
enum Ph {
    /// `%H` / `%h`
    Commit { abbrev: bool },
    /// `%T` / `%t`
    Tree { abbrev: bool },
    /// `%P` / `%p`
    Parents { abbrev: bool },
    /// `%a…` / `%c…`
    Person(Who, Part),
    /// `%aN` / `%aE` / `%cN` / `%cE` — mailmap-resolved name (`email = false`) or email.
    PersonMail(Who, bool),
    /// `%s`
    Subject,
    /// `%f`
    SanitizedSubject,
    /// `%b`
    Body,
    /// `%B`
    RawBody,
    /// `%N` — always empty here: notes are off unless `--notes` is given, which is refused.
    Notes,
    /// `%e`
    Encoding,
    /// `%m` — the revision mark. Always `>` in `format-rev` (no boundary/left flags).
    Mark,
    /// `%gD` / `%gd` / `%gn` / `%ge` / `%gs` — reflog data, always empty here.
    Reflog,
    /// `%d` (`wrap` = true, ` (…)`) / `%D` (`wrap` = false).
    Decoration { wrap: bool },
    /// `%(describe[:opts])`.
    Describe(SelectRef),
}

/// Which ident header a person placeholder reads.
#[derive(Clone, Copy)]
enum Who {
    Author,
    Committer,
}

/// Which component of an ident a person placeholder extracts.
#[derive(Clone, Copy)]
enum Part {
    /// `%an` / `%cn`
    Name,
    /// `%ae` / `%ce`
    Email,
    /// `%al` / `%cl` — the local part of the email.
    EmailLocal,
    /// `%ad` / `%cd` — git's `DATE_NORMAL`.
    DateNormal,
    /// `%aD` / `%cD` — RFC 2822.
    DateRfc2822,
    /// `%ai` / `%ci` — ISO 8601.
    DateIso,
    /// `%aI` / `%cI` — strict ISO 8601.
    DateIsoStrict,
    /// `%as` / `%cs` — `YYYY-MM-DD`.
    DateShort,
    /// `%at` / `%ct` — seconds since the epoch.
    DateUnix,
}

/// `git format-rev` — read revision expressions (or freeform text) from stdin and
/// render each through a pretty format. See the module docs for the covered surface.
pub fn format_rev(args: &[String]) -> Result<ExitCode> {
    let mut format_arg: Option<String> = None;
    let mut mode_arg: Option<String> = None;
    let mut null_input = false;
    let mut null_output = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        // Separate-argument forms consume the next argument.
        let value = |i: &mut usize, name: &str| -> Result<String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| anyhow!("option `{name}' requires a value"))
        };
        match a {
            "--format" => format_arg = Some(value(&mut i, "format")?),
            "--stdin-mode" => mode_arg = Some(value(&mut i, "stdin-mode")?),
            "--notes" => {
                let _ = value(&mut i, "notes")?;
                bail!("unsupported flag \"--notes\" (ported: --format, --stdin-mode, -z, --null-input, --null-output, --no-notes)");
            }
            "--no-notes" => {} // the default: notes are not displayed
            "-z" | "--null" => {
                null_input = true;
                null_output = true;
            }
            "--null-input" => null_input = true,
            "--no-null-input" => null_input = false,
            "--null-output" => null_output = true,
            "--no-null-output" => null_output = false,
            _ => {
                if let Some(v) = a.strip_prefix("--format=") {
                    format_arg = Some(v.to_string());
                } else if let Some(v) = a.strip_prefix("--stdin-mode=") {
                    mode_arg = Some(v.to_string());
                } else if a.starts_with("--notes=") {
                    bail!("unsupported flag \"--notes\" (ported: --format, --stdin-mode, -z, --null-input, --null-output, --no-notes)");
                } else if a.starts_with('-') {
                    let name = a.trim_start_matches('-');
                    eprint!("error: unknown option `{name}'\n{USAGE}");
                    return Ok(ExitCode::from(129));
                } else {
                    eprint!("error: too many arguments\n{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            }
        }
        i += 1;
    }

    // git validates in this order: --format present, --stdin-mode present,
    // --stdin-mode value, then the pretty format itself.
    let Some(format_arg) = format_arg else {
        eprintln!("fatal: '--format' is required");
        return Ok(ExitCode::from(128));
    };
    let Some(mode_arg) = mode_arg else {
        eprintln!("fatal: '--stdin-mode' is required");
        return Ok(ExitCode::from(128));
    };
    let mode = match mode_arg.as_str() {
        "revs" | "rev" => Mode::Revs,
        "text" => Mode::Text,
        _ => {
            eprintln!("fatal: '--stdin-mode' needs to be either text, revs, or rev");
            return Ok(ExitCode::from(128));
        }
    };
    let format = match resolve_format(&format_arg)? {
        Some(f) => f,
        None => {
            eprintln!("fatal: invalid --pretty format: {format_arg}");
            return Ok(ExitCode::from(128));
        }
    };

    let repo = gix::discover(".")?;
    let hex_len = repo.object_hash().len_in_hex();
    // Built once: `%aN`/`%aE`/`%cN`/`%cE` always resolve through the mailmap,
    // regardless of `log.mailmap`. An absent `.mailmap` yields an empty snapshot
    // whose resolution is the identity, matching git.
    let mailmap = repo.open_mailmap();

    let in_term = if null_input { b'\0' } else { b'\n' };
    let out_term = if null_output { b'\0' } else { b'\n' };

    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    let mut record: Vec<u8> = Vec::new();
    loop {
        record.clear();
        if reader.read_until(in_term, &mut record)? == 0 {
            break;
        }
        if record.last() == Some(&in_term) {
            record.pop();
        }

        let mut out: Vec<u8> = Vec::new();
        match mode {
            Mode::Revs => emit_rev(&repo, &record, &format, &mailmap, &mut out)?,
            Mode::Text => emit_text(&repo, &record, &format, hex_len, &mailmap, &mut out)?,
        }
        out.push(out_term);
        writer.write_all(&out)?;
        // The command is documented as safe to use interactively, so every
        // record leaves the process as soon as it is rendered.
        writer.flush()?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Resolve a `--format` argument into a [`Format`], or `None` when git would
/// reject it as an invalid pretty format.
fn resolve_format(arg: &str) -> Result<Option<Format>> {
    if let Some(rest) = arg.strip_prefix("format:").or_else(|| arg.strip_prefix("tformat:")) {
        return Ok(Some(Format::User(parse_user_format(rest)?)));
    }
    Ok(Some(match arg {
        "oneline" => Format::Builtin(Builtin::Oneline),
        "short" => Format::Builtin(Builtin::Short),
        "medium" => Format::Builtin(Builtin::Medium),
        "full" => Format::Builtin(Builtin::Full),
        "fuller" => Format::Builtin(Builtin::Fuller),
        "raw" => Format::Builtin(Builtin::Raw),
        // git implements `reference` as this exact user format with a short date.
        "reference" => Format::User(parse_user_format("%h (%s, %as)")?),
        "email" | "mboxrd" => bail!(
            "unsupported --format \"{arg}\" (ported: oneline, short, medium, full, fuller, raw, reference, format:, tformat:, and user formats)"
        ),
        _ if arg.is_empty() || arg.contains('%') => Format::User(parse_user_format(arg)?),
        _ => return Ok(None),
    }))
}

/// Parse a user format string.
///
/// Understood escapes: `%%`, `%n`, `%xNN`. Understood placeholders: `%H`, `%h`,
/// `%T`, `%t`, `%P`, `%p`, `%s`, `%f`, `%b`, `%B`, `%N`, `%e`, `%m`, `%d`, `%D`,
/// the reflog placeholders `%g[Ddnes]`, the color placeholders `%C*`, the person
/// placeholders `%a`/`%c` followed by one of `n`, `e`, `l`, `N`, `E`, `d`, `D`,
/// `i`, `I`, `s`, `t`, and the atoms `%(describe[:opts])`.
///
/// `%(trailers…)` and other unsupported placeholders git *does* recognise are
/// rejected; `%(align…)`/`%(if…)` and sequences git itself does not expand in
/// `format-rev` are kept verbatim, as git does.
fn parse_user_format(fmt: &str) -> Result<Vec<Item>> {
    let b = fmt.as_bytes();
    let mut items: Vec<Item> = Vec::new();
    let mut lit: Vec<u8> = Vec::new();
    let mut i = 0;

    let push = |items: &mut Vec<Item>, lit: &mut Vec<u8>, ph: Ph| {
        if !lit.is_empty() {
            items.push(Item::Literal(std::mem::take(lit)));
        }
        items.push(Item::Placeholder(ph));
    };

    while i < b.len() {
        if b[i] != b'%' {
            lit.push(b[i]);
            i += 1;
            continue;
        }
        let Some(&c) = b.get(i + 1) else {
            lit.push(b'%');
            i += 1;
            continue;
        };
        match c {
            b'%' => {
                lit.push(b'%');
                i += 2;
            }
            b'n' => {
                lit.push(b'\n');
                i += 2;
            }
            b'x' => {
                let hi = b.get(i + 2).and_then(|c| (*c as char).to_digit(16));
                let lo = b.get(i + 3).and_then(|c| (*c as char).to_digit(16));
                match (hi, lo) {
                    (Some(hi), Some(lo)) => {
                        lit.push((hi * 16 + lo) as u8);
                        i += 4;
                    }
                    // Not a valid `%xNN`: git leaves the whole thing alone.
                    _ => {
                        lit.push(b'%');
                        i += 1;
                    }
                }
            }
            b'H' => {
                push(&mut items, &mut lit, Ph::Commit { abbrev: false });
                i += 2;
            }
            b'h' => {
                push(&mut items, &mut lit, Ph::Commit { abbrev: true });
                i += 2;
            }
            b'T' => {
                push(&mut items, &mut lit, Ph::Tree { abbrev: false });
                i += 2;
            }
            b't' => {
                push(&mut items, &mut lit, Ph::Tree { abbrev: true });
                i += 2;
            }
            b'P' => {
                push(&mut items, &mut lit, Ph::Parents { abbrev: false });
                i += 2;
            }
            b'p' => {
                push(&mut items, &mut lit, Ph::Parents { abbrev: true });
                i += 2;
            }
            b's' => {
                push(&mut items, &mut lit, Ph::Subject);
                i += 2;
            }
            b'f' => {
                push(&mut items, &mut lit, Ph::SanitizedSubject);
                i += 2;
            }
            b'b' => {
                push(&mut items, &mut lit, Ph::Body);
                i += 2;
            }
            b'B' => {
                push(&mut items, &mut lit, Ph::RawBody);
                i += 2;
            }
            b'N' => {
                push(&mut items, &mut lit, Ph::Notes);
                i += 2;
            }
            b'e' => {
                push(&mut items, &mut lit, Ph::Encoding);
                i += 2;
            }
            b'm' => {
                push(&mut items, &mut lit, Ph::Mark);
                i += 2;
            }
            b'd' => {
                push(&mut items, &mut lit, Ph::Decoration { wrap: true });
                i += 2;
            }
            b'D' => {
                push(&mut items, &mut lit, Ph::Decoration { wrap: false });
                i += 2;
            }
            b'g' => {
                // Reflog placeholders. In `format-rev` there is no reflog
                // selector, so every recognised form expands to nothing.
                match b.get(i + 2) {
                    Some(b'D' | b'd' | b'n' | b'e' | b's') => {
                        push(&mut items, &mut lit, Ph::Reflog);
                        i += 3;
                    }
                    _ => {
                        lit.push(b'%');
                        i += 1;
                    }
                }
            }
            b'C' => {
                i = parse_color(b, i, &mut items, &mut lit)?;
            }
            b'(' => {
                i = parse_atom(b, i, &mut items, &mut lit)?;
            }
            b'a' | b'c' => {
                let ch = c as char;
                let who = if c == b'a' { Who::Author } else { Who::Committer };
                let Some(&sub) = b.get(i + 2) else {
                    bail!("unsupported placeholder \"%{ch}\"");
                };
                let ph = match sub {
                    b'n' => Ph::Person(who, Part::Name),
                    b'e' => Ph::Person(who, Part::Email),
                    b'l' => Ph::Person(who, Part::EmailLocal),
                    b'N' => Ph::PersonMail(who, false),
                    b'E' => Ph::PersonMail(who, true),
                    b'd' => Ph::Person(who, Part::DateNormal),
                    b'D' => Ph::Person(who, Part::DateRfc2822),
                    b'i' => Ph::Person(who, Part::DateIso),
                    b'I' => Ph::Person(who, Part::DateIsoStrict),
                    b's' => Ph::Person(who, Part::DateShort),
                    b't' => Ph::Person(who, Part::DateUnix),
                    _ => {
                        let bad = sub as char;
                        bail!(
                            "unsupported placeholder \"%{ch}{bad}\" (ported: %{ch}n, %{ch}e, %{ch}l, %{ch}N, %{ch}E, %{ch}d, %{ch}D, %{ch}i, %{ch}I, %{ch}s, %{ch}t)"
                        );
                    }
                };
                push(&mut items, &mut lit, ph);
                i += 3;
            }
            b'G' | b'w' | b'<' | b'>' | b'|' | b'+' | b'-' | b' ' => {
                bail!(
                    "unsupported placeholder \"%{}\" (signature, padding, wrapping and conditional line feeds are not ported)",
                    c as char
                );
            }
            // Unknown to git as well: echoed verbatim.
            _ => {
                lit.push(b'%');
                i += 1;
            }
        }
    }

    if !lit.is_empty() {
        items.push(Item::Literal(lit));
    }
    Ok(items)
}

/// Parse a `%C*` color placeholder starting at `b[i] == '%'` (`b[i+1] == 'C'`),
/// appending the resolved bytes to `lit`, and return the new index.
///
/// `format-rev` is invoked piped with no `--color`, so color output is off:
/// every form expands to nothing except an explicit `%C(always,<spec>)`.
fn parse_color(b: &[u8], i: usize, _items: &mut Vec<Item>, lit: &mut Vec<u8>) -> Result<usize> {
    let after = &b[i + 2..];
    if after.first() == Some(&b'(') {
        // Find the matching ')'.
        if let Some(rel) = after[1..].iter().position(|&x| x == b')') {
            let inner = &after[1..1 + rel];
            lit.extend_from_slice(&color_from_paren(inner)?);
            // Consumed: '%', 'C', '(', inner, ')'.
            return Ok(i + rel + 4);
        }
        // No closing ')': git treats `%C(` as unknown and echoes '%'.
        lit.push(b'%');
        return Ok(i + 1);
    }
    // Bare color words. With color off they all expand to nothing.
    for (word, len) in [
        (&b"reset"[..], 5usize),
        (&b"green"[..], 5),
        (&b"blue"[..], 4),
        (&b"red"[..], 3),
    ] {
        if after.starts_with(word) {
            return Ok(i + 2 + len);
        }
    }
    // `%C` followed by anything else: unknown, echo '%'.
    lit.push(b'%');
    Ok(i + 1)
}

/// Resolve the content between `%C(` and `)`. Only an `always` spec produces
/// output while color is off; every other form (`auto`, `auto,…`, a bare
/// `<spec>`, `reset`) expands to nothing.
fn color_from_paren(inner: &[u8]) -> Result<Vec<u8>> {
    if inner == b"always".as_slice() {
        return Ok(b"\x1b[m".to_vec());
    }
    if let Some(spec) = inner.strip_prefix(b"always,".as_slice()) {
        return parse_always_color(spec);
    }
    Ok(Vec::new())
}

/// Turn a `%C(always,<spec>)` color spec into its ANSI SGR sequence, exactly as
/// git's `color_parse_mem` does: attribute codes ascending, then the foreground
/// color, then the background color.
fn parse_always_color(spec: &[u8]) -> Result<Vec<u8>> {
    let spec = spec.trim_ascii();
    if spec.is_empty() {
        return Ok(b"\x1b[m".to_vec());
    }
    let tokens: Vec<&[u8]> = spec.split(|&c| c == b' ').filter(|t| !t.is_empty()).collect();
    if tokens.len() == 1 && tokens[0] == b"reset".as_slice() {
        return Ok(b"\x1b[m".to_vec());
    }
    let mut attrs: Vec<u16> = Vec::new();
    let mut colors: Vec<String> = Vec::new();
    let mut color_count = 0usize;
    for t in tokens {
        if t == b"reset".as_slice() {
            bail!("unsupported combined `reset` in %C(always,...)");
        }
        if let Some(a) = attr_code(t) {
            attrs.push(a);
            continue;
        }
        let is_bg = color_count >= 1;
        if let Some(code) = color_code(t, is_bg)? {
            colors.push(code);
        }
        color_count += 1;
    }
    attrs.sort_unstable();
    attrs.dedup();
    let mut codes: Vec<String> = attrs.iter().map(|a| a.to_string()).collect();
    codes.extend(colors);
    Ok(format!("\x1b[{}m", codes.join(";")).into_bytes())
}

/// Map an attribute token to its SGR code, or `None` if it is not an attribute.
fn attr_code(t: &[u8]) -> Option<u16> {
    const TABLE: [(&[u8], u16); 14] = [
        (b"bold", 1),
        (b"dim", 2),
        (b"italic", 3),
        (b"ul", 4),
        (b"blink", 5),
        (b"reverse", 7),
        (b"strike", 9),
        (b"nobold", 22),
        (b"nodim", 22),
        (b"noitalic", 23),
        (b"noul", 24),
        (b"noblink", 25),
        (b"noreverse", 27),
        (b"nostrike", 29),
    ];
    TABLE.iter().find(|(n, _)| *n == t).map(|(_, c)| *c)
}

/// Map a color token to its SGR code string for the foreground (`is_bg = false`)
/// or background (`is_bg = true`) slot. `normal` fills the slot but emits no code
/// (`Ok(None)`); an unrecognised token is rejected.
fn color_code(t: &[u8], is_bg: bool) -> Result<Option<String>> {
    let base = if is_bg { 10u16 } else { 0 };
    if t == b"normal".as_slice() {
        return Ok(None);
    }
    const NAMED: [&[u8]; 8] = [
        b"black", b"red", b"green", b"yellow", b"blue", b"magenta", b"cyan", b"white",
    ];
    for (idx, name) in NAMED.iter().enumerate() {
        if t == *name {
            return Ok(Some((30 + base + idx as u16).to_string()));
        }
    }
    if t == b"default".as_slice() {
        return Ok(Some((39 + base).to_string()));
    }
    if let Some(rest) = t.strip_prefix(b"bright".as_slice()) {
        for (idx, name) in NAMED.iter().enumerate() {
            if rest == *name {
                return Ok(Some((90 + base + idx as u16).to_string()));
            }
        }
        bail!("unsupported color token in %C(always,...)");
    }
    if t.first() == Some(&b'#') && t.len() == 7 {
        let hex = &t[1..];
        let byte = |h: &[u8]| -> Option<u8> {
            u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()
        };
        if let (Some(r), Some(g), Some(bl)) = (byte(&hex[0..2]), byte(&hex[2..4]), byte(&hex[4..6])) {
            let lead = if is_bg { "48" } else { "38" };
            return Ok(Some(format!("{lead};2;{r};{g};{bl}")));
        }
        bail!("unsupported color token in %C(always,...)");
    }
    if !t.is_empty() && t.iter().all(u8::is_ascii_digit) {
        let n: u16 = std::str::from_utf8(t)
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n| n <= 255)
            .ok_or_else(|| anyhow!("unsupported color token in %C(always,...)"))?;
        let s = if n < 8 {
            (30 + base + n).to_string()
        } else if n < 16 {
            (90 + base + (n - 8)).to_string()
        } else {
            let lead = if is_bg { "48" } else { "38" };
            format!("{lead};5;{n}")
        };
        return Ok(Some(s));
    }
    bail!("unsupported color token in %C(always,...)");
}

/// Parse a `%(…)` atom starting at `b[i] == '%'` (`b[i+1] == '('`), appending any
/// resulting placeholder, and return the new index.
///
/// `%(describe[:opts])` is evaluated. `%(trailers…)` is rejected. Every other
/// `%(…)` — `%(align…)`, `%(if…)`/`%(then)`/`%(else)`/`%(end)`, unknown atoms, or
/// an unterminated `%(` — is echoed verbatim, matching git's `format-rev`, which
/// does not expand these.
fn parse_atom(b: &[u8], i: usize, items: &mut Vec<Item>, lit: &mut Vec<u8>) -> Result<usize> {
    let after = &b[i + 2..]; // content following "%("
    let Some(rel) = after.iter().position(|&x| x == b')') else {
        // Unterminated: echo '%'.
        lit.push(b'%');
        return Ok(i + 1);
    };
    let content = &after[..rel];

    if content == b"trailers".as_slice() || content.starts_with(b"trailers:".as_slice()) {
        bail!("unsupported placeholder \"%(trailers…)\" (trailer parsing is not ported)");
    }

    if content == b"describe".as_slice() || content.starts_with(b"describe:".as_slice()) {
        let opts = &content[b"describe".len()..];
        let select = if opts.is_empty() {
            SelectRef::AnnotatedTags
        } else if opts == b":tags".as_slice() || opts == b":tags=true".as_slice() {
            SelectRef::AllTags
        } else if opts == b":tags=false".as_slice() {
            SelectRef::AnnotatedTags
        } else if opts == b":all".as_slice() {
            SelectRef::AllRefs
        } else {
            bail!(
                "unsupported %(describe) options (ported: %(describe), %(describe:tags), %(describe:all))"
            );
        };
        if !lit.is_empty() {
            items.push(Item::Literal(std::mem::take(lit)));
        }
        items.push(Item::Placeholder(Ph::Describe(select)));
        // Consumed: '%', '(', content, ')'.
        return Ok(i + 2 + rel + 1);
    }

    // Anything else: git does not expand it — echo '%'.
    lit.push(b'%');
    Ok(i + 1)
}

/// `--stdin-mode=revs`: resolve one record to a commit and render it, or warn and
/// emit nothing (git still terminates the empty record).
fn emit_rev(
    repo: &gix::Repository,
    record: &[u8],
    format: &Format,
    mailmap: &gix::mailmap::Snapshot,
    out: &mut Vec<u8>,
) -> Result<()> {
    let Ok(id) = repo.rev_parse_single(record.as_bstr()) else {
        eprintln!("Could not get object name for {}. Skipping.", record.to_str_lossy());
        return Ok(());
    };
    let oid = id.detach();
    let peeled = match repo.find_object(oid) {
        Ok(object) => object.peel_to_commit().ok(),
        Err(_) => None,
    };
    let Some(commit) = peeled else {
        eprintln!("Could not get commit for {oid}. Skipping.");
        return Ok(());
    };
    render(repo, &commit, format, mailmap, out)
}

/// `--stdin-mode=text`: copy the record through, replacing every maximal run of
/// lowercase hex digits whose length is exactly the hash's hex length — and which
/// names a commit that exists — with the rendered commit. Everything else, object
/// names of other types included, is echoed unchanged.
fn emit_text(
    repo: &gix::Repository,
    record: &[u8],
    format: &Format,
    hex_len: usize,
    mailmap: &gix::mailmap::Snapshot,
    out: &mut Vec<u8>,
) -> Result<()> {
    let mut i = 0;
    while i < record.len() {
        if !is_lower_hex(record[i]) {
            out.push(record[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < record.len() && is_lower_hex(record[i]) {
            i += 1;
        }
        let run = &record[start..i];
        if run.len() != hex_len {
            out.extend_from_slice(run);
            continue;
        }
        let rendered = ObjectId::from_hex(run)
            .ok()
            .and_then(|oid| repo.find_object(oid).ok())
            .and_then(|obj| obj.try_into_commit().ok());
        match rendered {
            Some(commit) => render(repo, &commit, format, mailmap, out)?,
            None => out.extend_from_slice(run),
        }
    }
    Ok(())
}

/// git parses object names in text mode as lowercase hex only.
fn is_lower_hex(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f')
}

/// Render one commit through `format`, appending to `out`.
fn render(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    format: &Format,
    mailmap: &gix::mailmap::Snapshot,
    out: &mut Vec<u8>,
) -> Result<()> {
    let cr = commit.decode()?;
    match format {
        Format::User(items) => render_user(repo, commit, &cr, items, mailmap, out),
        Format::Builtin(b) => render_builtin(repo, &cr, *b, out),
    }
}

/// Evaluate a parsed user format against one commit.
fn render_user(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    cr: &gix::objs::CommitRef<'_>,
    items: &[Item],
    mailmap: &gix::mailmap::Snapshot,
    out: &mut Vec<u8>,
) -> Result<()> {
    let id = commit.id;
    let msg = cr.message.as_bytes();
    for item in items {
        match item {
            Item::Literal(bytes) => out.extend_from_slice(bytes),
            Item::Placeholder(ph) => match ph {
                Ph::Commit { abbrev } => push_oid(repo, out, &id, *abbrev)?,
                Ph::Tree { abbrev } => push_oid(repo, out, &cr.tree(), *abbrev)?,
                Ph::Parents { abbrev } => {
                    for (n, p) in cr.parents().enumerate() {
                        if n > 0 {
                            out.push(b' ');
                        }
                        push_oid(repo, out, &p, *abbrev)?;
                    }
                }
                Ph::Person(who, part) => {
                    let sig = match who {
                        Who::Author => cr.author()?,
                        Who::Committer => cr.committer()?,
                    };
                    match part {
                        Part::Name => out.extend_from_slice(sig.name.as_bytes()),
                        Part::Email => out.extend_from_slice(sig.email.as_bytes()),
                        Part::EmailLocal => {
                            let e = sig.email.as_bytes();
                            let local = e.iter().position(|&c| c == b'@').map_or(e, |n| &e[..n]);
                            out.extend_from_slice(local);
                        }
                        _ => out.extend_from_slice(format_date(sig.time()?, *part).as_bytes()),
                    }
                }
                Ph::PersonMail(who, email) => {
                    let sig = match who {
                        Who::Author => cr.author()?,
                        Who::Committer => cr.committer()?,
                    };
                    let resolved = mailmap.try_resolve_ref(sig);
                    let val = if *email {
                        resolved.and_then(|r| r.email).unwrap_or(sig.email)
                    } else {
                        resolved.and_then(|r| r.name).unwrap_or(sig.name)
                    };
                    out.extend_from_slice(val.as_bytes());
                }
                Ph::Subject => out.extend_from_slice(&subject(&msg[subject_off(msg)..])),
                Ph::SanitizedSubject => {
                    let from = &msg[subject_off(msg)..];
                    let first = &from[..first_line_len(from)];
                    out.extend_from_slice(&sanitize_subject(&first[..rtrim_len(first)]));
                }
                Ph::Body => out.extend_from_slice(&msg[body_off(msg)..]),
                Ph::RawBody => out.extend_from_slice(&msg[subject_off(msg)..]),
                // Notes are off (`--notes` is refused), so this is always empty.
                Ph::Notes => {}
                Ph::Encoding => {
                    if let Some(enc) = cr.encoding {
                        out.extend_from_slice(enc.as_bytes());
                    }
                }
                // No boundary/left-right flags in `format-rev`: the mark is `>`.
                Ph::Mark => out.push(b'>'),
                // No reflog selector in `format-rev`: always empty.
                Ph::Reflog => {}
                Ph::Decoration { wrap } => push_decoration(repo, &id, *wrap, out)?,
                Ph::Describe(select) => {
                    if let Some(fmt) = commit.describe().names(*select).try_format()? {
                        out.extend_from_slice(fmt.to_string().as_bytes());
                    }
                }
            },
        }
    }
    Ok(())
}

/// `%d`/`%D`: append the ref decoration for `id`. `%d` (`wrap`) wraps a non-empty
/// decoration in ` (…)`; `%D` emits the bare list. Nothing is emitted when the
/// commit carries no decoration.
fn push_decoration(
    repo: &gix::Repository,
    id: &ObjectId,
    wrap: bool,
    out: &mut Vec<u8>,
) -> Result<()> {
    let body = decoration_body(repo, id)?;
    if body.is_empty() {
        return Ok(());
    }
    if wrap {
        out.extend_from_slice(b" (");
        out.extend_from_slice(&body);
        out.push(b')');
    } else {
        out.extend_from_slice(&body);
    }
    Ok(())
}

/// Build the `%D` decoration body for `id`: the refs pointing at the commit, in
/// git's order (full refnames sorted descending — the reverse of the alphabetical
/// order in which git prepends them), with `HEAD` (or `HEAD -> <branch>`) pulled
/// to the front, tags prefixed `tag: `, and everything joined with `, `.
fn decoration_body(repo: &gix::Repository, id: &ObjectId) -> Result<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = Vec::new();
    let refs = repo.references()?;
    for r in refs.all()? {
        let mut r = r.map_err(|e| anyhow::anyhow!("{e}"))?;
        let Ok(peeled) = r.peel_to_id() else { continue };
        if peeled.detach() != *id {
            continue;
        }
        names.push(r.name().as_bstr().to_vec());
    }
    names.sort_by(|a, b| b.cmp(a));

    let head = repo.head()?;
    let mut parts: Vec<Vec<u8>> = Vec::new();
    match head.referent_name().map(|n| n.as_bstr().to_vec()) {
        // HEAD is attached: if its branch points at this commit, render
        // `HEAD -> <branch>` and drop the branch from the remaining list.
        Some(ht) => {
            if let Some(pos) = names.iter().position(|n| *n == ht) {
                let mut p = b"HEAD -> ".to_vec();
                p.extend_from_slice(prettify_ref(&ht));
                parts.push(p);
                names.remove(pos);
            }
        }
        // Detached: prepend `HEAD` when it points at this commit.
        None => {
            if let Ok(hid) = repo.head_id() {
                if hid.detach() == *id {
                    parts.push(b"HEAD".to_vec());
                }
            }
        }
    }
    for n in &names {
        parts.push(format_ref_decoration(n));
    }

    let mut body = Vec::new();
    for (n, p) in parts.iter().enumerate() {
        if n > 0 {
            body.extend_from_slice(b", ");
        }
        body.extend_from_slice(p);
    }
    Ok(body)
}

/// git's `prettify_refname`: strip a `refs/heads/`, `refs/tags/`, `refs/remotes/`
/// or bare `refs/` prefix.
fn prettify_ref(full: &[u8]) -> &[u8] {
    for pfx in [&b"refs/heads/"[..], &b"refs/tags/"[..], &b"refs/remotes/"[..]] {
        if let Some(rest) = full.strip_prefix(pfx) {
            return rest;
        }
    }
    full.strip_prefix(&b"refs/"[..]).unwrap_or(full)
}

/// Format one decoration entry: tags carry a `tag: ` prefix, everything else is
/// its prettified short name.
fn format_ref_decoration(full: &[u8]) -> Vec<u8> {
    if let Some(rest) = full.strip_prefix(&b"refs/tags/"[..]) {
        let mut v = b"tag: ".to_vec();
        v.extend_from_slice(rest);
        v
    } else {
        prettify_ref(full).to_vec()
    }
}

/// Render one of the builtin header-plus-message formats.
///
/// None of them print the commit's own id: in stock git that line comes from the
/// log-tree walker, not from the pretty machinery `format-rev` calls.
fn render_builtin(
    repo: &gix::Repository,
    cr: &gix::objs::CommitRef<'_>,
    fmt: Builtin,
    out: &mut Vec<u8>,
) -> Result<()> {
    let msg = cr.message.as_bytes();
    let body = &msg[subject_off(msg)..];

    if fmt == Builtin::Oneline {
        let mut sb = subject(body);
        rtrim(&mut sb);
        out.extend_from_slice(&sb);
        return Ok(());
    }

    let mut sb: Vec<u8> = Vec::new();
    let author = cr.author()?;
    let committer = cr.committer()?;

    if fmt == Builtin::Raw {
        if !cr.extra_headers.is_empty() {
            bail!("--format=raw is not ported for commits with extra headers (gpgsig, mergetag, …)");
        }
        push_str(&mut sb, &format!("tree {}\n", cr.tree()));
        for p in cr.parents() {
            push_str(&mut sb, &format!("parent {p}\n"));
        }
        push_str(&mut sb, "author ");
        push_ident_raw(&mut sb, &author)?;
        push_str(&mut sb, "committer ");
        push_ident_raw(&mut sb, &committer)?;
    } else {
        // A merge commit lists its abbreviated parents ahead of the ident block.
        let parents: Vec<ObjectId> = cr.parents().collect();
        if parents.len() > 1 {
            push_str(&mut sb, "Merge:");
            for p in &parents {
                sb.push(b' ');
                push_oid(repo, &mut sb, p, true)?;
            }
            sb.push(b'\n');
        }
        let pad = if fmt == Builtin::Fuller { "    " } else { "" };
        push_str(&mut sb, &format!("Author: {pad}"));
        push_ident(&mut sb, &author);
        match fmt {
            Builtin::Medium => push_str(
                &mut sb,
                &format!("Date:   {}\n", format_date(author.time()?, Part::DateNormal)),
            ),
            Builtin::Fuller => {
                push_str(
                    &mut sb,
                    &format!("AuthorDate: {}\n", format_date(author.time()?, Part::DateNormal)),
                );
                push_str(&mut sb, "Commit:     ");
                push_ident(&mut sb, &committer);
                push_str(
                    &mut sb,
                    &format!("CommitDate: {}\n", format_date(committer.time()?, Part::DateNormal)),
                );
            }
            Builtin::Full => {
                push_str(&mut sb, "Commit: ");
                push_ident(&mut sb, &committer);
            }
            _ => {}
        }
    }

    sb.push(b'\n');
    pp_remainder(&mut sb, body, 4, fmt == Builtin::Short);
    // git rtrims the whole buffer, then guarantees exactly one closing newline.
    rtrim(&mut sb);
    sb.push(b'\n');
    out.extend_from_slice(&sb);
    Ok(())
}

/// `<name> <<email>>\n`, as the `Author:`/`Commit:` header lines carry it.
fn push_ident(out: &mut Vec<u8>, sig: &gix::actor::SignatureRef<'_>) {
    out.extend_from_slice(sig.name.as_bytes());
    out.extend_from_slice(b" <");
    out.extend_from_slice(sig.email.as_bytes());
    out.extend_from_slice(b">\n");
}

/// `<name> <<email>> <secs> <tz>\n`, as stored in the commit object itself.
fn push_ident_raw(out: &mut Vec<u8>, sig: &gix::actor::SignatureRef<'_>) -> Result<()> {
    out.extend_from_slice(sig.name.as_bytes());
    out.extend_from_slice(b" <");
    out.extend_from_slice(sig.email.as_bytes());
    out.extend_from_slice(b"> ");
    push_str(out, &sig.time()?.to_string());
    out.push(b'\n');
    Ok(())
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

/// Append an object id, full or abbreviated.
///
/// `format-rev` ignores `core.abbrev` — it always starts from git's
/// `DEFAULT_ABBREV` of 7 and lengthens only to disambiguate.
fn push_oid(repo: &gix::Repository, out: &mut Vec<u8>, id: &ObjectId, abbrev: bool) -> Result<()> {
    if !abbrev {
        push_str(out, &id.to_hex().to_string());
        return Ok(());
    }
    let candidate = gix::odb::store::prefix::disambiguate::Candidate::new(*id, 7)?;
    let text = match repo.objects.disambiguate_prefix(candidate)? {
        Some(prefix) => prefix.to_string(),
        None => id.to_hex_with_len(7).to_string(),
    };
    push_str(out, &text);
    Ok(())
}

/// Render a commit time in the format a date placeholder asks for.
fn format_date(time: gix::date::Time, part: Part) -> String {
    use gix::date::time::format;
    match part {
        Part::DateNormal => time.format_or_unix(format::DEFAULT),
        Part::DateRfc2822 => time.format_or_unix(format::GIT_RFC2822),
        Part::DateIso => time.format_or_unix(format::ISO8601),
        Part::DateIsoStrict => time.format_or_unix(format::ISO8601_STRICT),
        Part::DateShort => time.format_or_unix(format::SHORT),
        Part::DateUnix => time.format_or_unix(format::UNIX),
        _ => unreachable!("format_date is only reached for date parts"),
    }
}

/// Length of the line starting at `msg[0]`, including its trailing newline.
fn first_line_len(msg: &[u8]) -> usize {
    msg.iter().position(|&c| c == b'\n').map_or(msg.len(), |n| n + 1)
}

/// Length of `line` once trailing ASCII whitespace (the newline included) is dropped.
/// This is git's `is_blank_line`, which reports zero for a whitespace-only line.
fn rtrim_len(line: &[u8]) -> usize {
    let mut len = line.len();
    while len > 0 && line[len - 1].is_ascii_whitespace() {
        len -= 1;
    }
    len
}

/// Drop trailing ASCII whitespace from a whole buffer (git's `strbuf_rtrim`).
fn rtrim(buf: &mut Vec<u8>) {
    let len = rtrim_len(buf);
    buf.truncate(len);
}

/// Offset of the first non-blank line — where the subject begins.
fn subject_off(msg: &[u8]) -> usize {
    let mut at = 0;
    while at < msg.len() {
        let len = first_line_len(&msg[at..]);
        if rtrim_len(&msg[at..at + len]) != 0 {
            break;
        }
        at += len;
    }
    at
}

/// Offset of the body — past the subject block and the blank lines after it.
fn body_off(msg: &[u8]) -> usize {
    let mut at = subject_off(msg);
    while at < msg.len() {
        let len = first_line_len(&msg[at..]);
        if rtrim_len(&msg[at..at + len]) == 0 {
            break;
        }
        at += len;
    }
    while at < msg.len() {
        let len = first_line_len(&msg[at..]);
        if rtrim_len(&msg[at..at + len]) != 0 {
            break;
        }
        at += len;
    }
    at
}

/// The subject: every line up to the first blank one, right-trimmed and joined
/// with single spaces (git's `format_subject`).
fn subject(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut first = true;
    while at < body.len() {
        let len = first_line_len(&body[at..]);
        let line = &body[at..at + len];
        at += len;
        let trimmed = rtrim_len(line);
        if trimmed == 0 {
            break;
        }
        if !first {
            out.push(b' ');
        }
        out.extend_from_slice(&line[..trimmed]);
        first = false;
    }
    out
}

/// The message body under a header block: each line right-trimmed and prefixed
/// with `indent` spaces, leading blank lines dropped (git's `pp_remainder`).
/// `short` stops at the first blank line after the subject.
fn pp_remainder(out: &mut Vec<u8>, body: &[u8], indent: usize, short: bool) {
    let mut at = 0;
    let mut first = true;
    while at < body.len() {
        let len = first_line_len(&body[at..]);
        let line = &body[at..at + len];
        at += len;
        let trimmed = rtrim_len(line);
        if trimmed == 0 {
            if first {
                continue;
            }
            if short {
                break;
            }
        }
        first = false;
        out.resize(out.len() + indent, b' ');
        out.extend_from_slice(&line[..trimmed]);
        out.push(b'\n');
    }
}

/// `%f`: the subject reduced to alphanumerics, `.` and `_`, with runs of other
/// characters collapsed to a single `-` and trailing `.`/`-` trimmed. Repeated
/// dots collapse to one. Ported from git's `format_sanitized_subject`.
fn sanitize_subject(subject: &[u8]) -> Vec<u8> {
    fn is_title_char(c: u8) -> bool {
        c.is_ascii_alphanumeric() || c == b'.' || c == b'_'
    }

    let mut out = Vec::new();
    // Starts at 2 so leading non-title characters never produce a leading `-`.
    let mut space: u8 = 2;
    let mut i = 0;
    while i < subject.len() {
        let c = subject[i];
        if is_title_char(c) {
            if space == 1 {
                out.push(b'-');
            }
            space = 0;
            out.push(c);
            if c == b'.' {
                while subject.get(i + 1) == Some(&b'.') {
                    i += 1;
                }
            }
        } else {
            space |= 1;
        }
        i += 1;
    }
    while matches!(out.last(), Some(b'.') | Some(b'-')) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The expected bytes were captured from stock `git format-rev`
    /// `--format='%C(always,<spec>)'` on git 2.55.0.
    #[test]
    fn always_color_specs_match_git() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"red", b"\x1b[31m"),
            (b"green", b"\x1b[32m"),
            (b"blue", b"\x1b[34m"),
            (b"black", b"\x1b[30m"),
            (b"white", b"\x1b[37m"),
            (b"default", b"\x1b[39m"),
            (b"bold", b"\x1b[1m"),
            (b"ul", b"\x1b[4m"),
            (b"reverse", b"\x1b[7m"),
            (b"brightred", b"\x1b[91m"),
            (b"brightblack", b"\x1b[90m"),
            (b"7", b"\x1b[37m"),
            (b"8", b"\x1b[90m"),
            (b"15", b"\x1b[97m"),
            (b"16", b"\x1b[38;5;16m"),
            (b"123", b"\x1b[38;5;123m"),
            (b"255", b"\x1b[38;5;255m"),
            (b"#ff8800", b"\x1b[38;2;255;136;0m"),
            (b"bold red", b"\x1b[1;31m"),
            (b"red bold", b"\x1b[1;31m"),
            (b"ul bold", b"\x1b[1;4m"),
            (b"bold ul red", b"\x1b[1;4;31m"),
            (b"blue ul", b"\x1b[4;34m"),
            (b"red green", b"\x1b[31;42m"),
            (b"black white", b"\x1b[30;47m"),
            (b"normal red", b"\x1b[41m"),
            (b"16 200", b"\x1b[38;5;16;48;5;200m"),
            (b"red 200", b"\x1b[31;48;5;200m"),
            (b"nobold", b"\x1b[22m"),
            (b"noul", b"\x1b[24m"),
            (b"", b"\x1b[m"),
        ];
        for &(spec, want) in cases {
            let got = parse_always_color(spec).unwrap();
            assert_eq!(got, want, "spec {:?}", spec.as_bstr());
        }
    }

    #[test]
    fn color_off_forms_are_empty_only_always_emits() {
        // Color is off for the piped, non-`--color` invocation, so every paren
        // form expands to nothing except an explicit `always`.
        assert!(color_from_paren(b"auto").unwrap().is_empty());
        assert!(color_from_paren(b"auto,red").unwrap().is_empty());
        assert!(color_from_paren(b"red").unwrap().is_empty());
        assert!(color_from_paren(b"reset").unwrap().is_empty());
        assert_eq!(color_from_paren(b"always").unwrap(), b"\x1b[m");
        assert_eq!(color_from_paren(b"always,reset").unwrap(), b"\x1b[m");
        assert_eq!(color_from_paren(b"always,bold red").unwrap(), b"\x1b[1;31m");
        // "underline" is not a valid attribute name in git (only "ul").
        assert!(parse_always_color(b"underline").is_err());
    }

    #[test]
    fn decoration_prefixes_and_prettify() {
        assert_eq!(prettify_ref(b"refs/heads/main"), b"main");
        assert_eq!(prettify_ref(b"refs/remotes/origin/main"), b"origin/main");
        assert_eq!(prettify_ref(b"refs/tags/v1"), b"v1");
        assert_eq!(format_ref_decoration(b"refs/tags/v1"), b"tag: v1");
        assert_eq!(format_ref_decoration(b"refs/heads/main"), b"main");
        assert_eq!(
            format_ref_decoration(b"refs/remotes/origin/main"),
            b"origin/main"
        );
    }
}
