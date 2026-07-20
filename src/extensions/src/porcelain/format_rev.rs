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
//! Not covered — each rejected with a precise message rather than producing
//! divergent output: `--notes=<ref>` (no notes lookup here, so `%N` is only
//! correct with notes off, which is the default), the `email`/`mboxrd` builtin
//! formats (RFC2047 subject encoding and MIME body handling are not built), and
//! the placeholders `%C*` (color), `%G*` (signature), `%g*` (reflog), `%d`/`%D`
//! (decoration), `%m`, `%w()`, `%<`/`%>`/`%|` (padding), `%+`/`%-`/`% `
//! (conditional line feeds), `%(...)` (trailers/align/if/describe) and the
//! mailmap-aware `%aN`/`%aE`/`%cN`/`%cE`, plus the relative/human date forms.
//! Placeholders git itself does not recognise are echoed verbatim, as git does.
//!
//! Known divergence: a commit carrying an `encoding` header is rendered from its
//! stored bytes; stock git re-encodes the message to UTF-8 first. And `raw` is
//! refused for commits with extra headers (`gpgsig`, `mergetag`, …) because they
//! would have to be reproduced verbatim.

use anyhow::{anyhow, bail, Result};
use std::io::{BufRead, Write};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
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
    /// Literal bytes, including the expansion of `%%`, `%n` and `%xNN`.
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
            Mode::Revs => emit_rev(&repo, &record, &format, &mut out)?,
            Mode::Text => emit_text(&repo, &record, &format, hex_len, &mut out)?,
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
/// `%T`, `%t`, `%P`, `%p`, `%s`, `%f`, `%b`, `%B`, `%N`, `%e`, and the person
/// placeholders `%a`/`%c` followed by one of `n`, `e`, `l`, `d`, `D`, `i`, `I`,
/// `s`, `t`.
///
/// Placeholders git recognises but this module does not implement are rejected;
/// sequences git does not recognise either are kept verbatim, as git does.
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
            b'a' | b'c' => {
                let ch = c as char;
                let who = if c == b'a' { Who::Author } else { Who::Committer };
                let Some(&sub) = b.get(i + 2) else {
                    bail!("unsupported placeholder \"%{ch}\"");
                };
                let part = match sub {
                    b'n' => Part::Name,
                    b'e' => Part::Email,
                    b'l' => Part::EmailLocal,
                    b'd' => Part::DateNormal,
                    b'D' => Part::DateRfc2822,
                    b'i' => Part::DateIso,
                    b'I' => Part::DateIsoStrict,
                    b's' => Part::DateShort,
                    b't' => Part::DateUnix,
                    _ => {
                        let bad = sub as char;
                        bail!(
                            "unsupported placeholder \"%{ch}{bad}\" (ported: %{ch}n, %{ch}e, %{ch}l, %{ch}d, %{ch}D, %{ch}i, %{ch}I, %{ch}s, %{ch}t)"
                        );
                    }
                };
                push(&mut items, &mut lit, Ph::Person(who, part));
                i += 3;
            }
            b'C' | b'G' | b'g' | b'd' | b'D' | b'm' | b'w' | b'<' | b'>' | b'|' | b'+' | b'-'
            | b' ' | b'(' => {
                bail!(
                    "unsupported placeholder \"%{}\" (color, signature, reflog, decoration, padding, conditional line feeds and %(...) atoms are not ported)",
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

/// `--stdin-mode=revs`: resolve one record to a commit and render it, or warn and
/// emit nothing (git still terminates the empty record).
fn emit_rev(repo: &gix::Repository, record: &[u8], format: &Format, out: &mut Vec<u8>) -> Result<()> {
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
    render(repo, &commit, format, out)
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
            Some(commit) => render(repo, &commit, format, out)?,
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
    out: &mut Vec<u8>,
) -> Result<()> {
    let cr = commit.decode()?;
    let id = commit.id;
    match format {
        Format::User(items) => render_user(repo, &id, &cr, items, out),
        Format::Builtin(b) => render_builtin(repo, &cr, *b, out),
    }
}

/// Evaluate a parsed user format against one commit.
fn render_user(
    repo: &gix::Repository,
    id: &ObjectId,
    cr: &gix::objs::CommitRef<'_>,
    items: &[Item],
    out: &mut Vec<u8>,
) -> Result<()> {
    let msg = cr.message.as_bytes();
    for item in items {
        match item {
            Item::Literal(bytes) => out.extend_from_slice(bytes),
            Item::Placeholder(ph) => match ph {
                Ph::Commit { abbrev } => push_oid(repo, out, id, *abbrev)?,
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
            },
        }
    }
    Ok(())
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
