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
//! Also covered: the column atoms `%<(<N>)`, `%>(<N>)`, `%><(<N>)`, `%>>(<N>)`,
//! their `%<|(<N>)` column-target and `,trunc`/`,ltrunc`/`,mtrunc` forms, and the
//! `%w(<width>,<indent1>,<indent2>)` wrap atom — all through the shared
//! [`super::pretty_pad`] port, so widths are display columns and a CJK subject
//! costs two per glyph.
//!
//! `--notes=<ref>` is accepted and, like git, has no effect unless the format
//! expands `%N`: git only calls `load_display_notes()` when
//! `userformat_find_requirements()` reports the format wants notes.
//!
//! Not covered — each rejected with a precise message rather than producing
//! divergent output: `%N` itself (the vendored `gix-note` crate is an empty
//! stub, so notes cannot be read), the `email`/`mboxrd` builtin formats (RFC2047 subject encoding
//! and MIME body handling are not built), the `%(trailers…)` atoms (a faithful
//! port of git's `find_trailer_block_start` + folding + the full option matrix
//! could not be validated to byte-parity here without an integration build),
//! the `%+`/`%-`/`% ` conditional line feeds, and `%G*` (signature verification
//! needs GPG, which is absent).
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

use super::pretty_pad::{parse_wrap, FlushType, PadState, WrapState};
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
///
/// The split mirrors what git's driver treats as a placeholder, because that is
/// what the column atoms pad: only [`Item::Literal`] passes through a pending
/// field untouched. `%n`, `%xNN` and `%C*` are placeholders in `pretty.c` even
/// though they expand to fixed bytes, so they get their own [`Ph`] variants
/// rather than folding into a literal run; `%%`, which `strbuf_expand_step()`
/// handles before `format_commit_item()` is reached, stays literal.
enum Item {
    /// Literal bytes, including the expansion of `%%`.
    Literal(Vec<u8>),
    Placeholder(Ph),
    /// A `%<`/`%>` column atom.
    ///
    /// `state` is the deferred padding request the atom stored, snapshotted at
    /// parse time — or `None` when the atom stored nothing, which is every form
    /// git rejects before reaching `c->padding = …` (a zero, missing, negative or
    /// over-limit width). Those must leave the running state alone rather than
    /// replay a snapshot: `PadState::apply` clears `flush` between fields, so a
    /// stale snapshot would reopen one git had already closed.
    ///
    /// `consumed` is false for a malformed truncation modifier too — but that form
    /// *does* store, so it carries a `Some`. It has to ride on the same item
    /// rather than become a following [`Item::Unconsumed`]: git makes *one*
    /// `format_commit_item()` call here, so the atom stores the new state and
    /// prints its bare `%` without ever flushing the field it just opened. Two
    /// items would make the second consume the first's field.
    Pad { state: Option<PadState>, consumed: bool },
    /// A `%w(<width>,<indent1>,<indent2>)` wrap atom.
    Wrap { width: usize, indent1: usize, indent2: usize },
    /// A `%` that `format_commit_item()` answered 0 for: an unknown placeholder,
    /// a malformed column or wrap atom, an unterminated `%(`, or a trailing `%`.
    ///
    /// git prints the `%` and rescans from the next character — which the parser
    /// has already turned into literal text — but only *after*
    /// `format_and_pad_commit()` has laid out an empty field when one is pending.
    /// That flush is why this cannot simply be a literal `%`.
    Unconsumed,
}

/// The user-format placeholders this module evaluates.
enum Ph {
    /// `%n` — a newline. A placeholder in `pretty.c`, so a pending field pads it.
    Newline,
    /// `%xNN` — one raw byte. Likewise a placeholder, not literal text.
    Byte(u8),
    /// `%C*` — the resolved colour escape, empty for every form but an explicit
    /// `%C(always,<spec>)` while colour is off.
    ///
    /// The one placeholder that *chains*: `format_and_pad_commit()` keeps pulling
    /// the following placeholder into the same field after a `%C…`, so the escape
    /// contributes bytes but no columns and the field measures the text.
    Color(Vec<u8>),
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
    /// `%N` — refused up front: reading notes needs substrate that is not vendored.
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
    // `OPT_STRING_LIST(0, "notes", …)`: refs accumulate, `--no-notes` clears the
    // list. git only *loads* them when the pretty format asks for notes, so the
    // option on its own is inert — see the `Ph::Notes` check below.
    let mut notes: Vec<String> = Vec::new();

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
            "--notes" => notes.push(value(&mut i, "notes")?),
            "--no-notes" => notes.clear(), // the default: notes are not displayed
            "-h" => {
                // `parse_options` prints the usage on stdout for an explicit
                // `-h` and exits 129, leaving stderr empty.
                print!("{USAGE}");
                std::io::stdout().flush()?;
                return Ok(ExitCode::from(129));
            }
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
                } else if let Some(v) = a.strip_prefix("--notes=") {
                    notes.push(v.to_string());
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

    // `userformat_find_requirements()` + `load_display_notes()`: notes are read
    // only when the format actually expands `%N`. Any `--notes=<ref>` given for a
    // format that never asks for them is inert, so it needs no support here.
    if let Format::User(items) = &format {
        if items
            .iter()
            .any(|it| matches!(it, Item::Placeholder(Ph::Notes)))
        {
            bail!(
                "unsupported: `%N` requires reading notes{} (the vendored gix-note crate is a stub)",
                if notes.is_empty() {
                    " from refs/notes/commits".to_string()
                } else {
                    format!(" from {}", notes.join(", "))
                }
            );
        }
    }

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
        "email" | "mboxrd" => anyhow::bail!(
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
        flush_lit(items, lit);
        items.push(Item::Placeholder(ph));
    };
    // The running `struct format_commit_context` padding state. The atoms are
    // resolved here, once, and the snapshots replayed per record.
    let mut pad = PadState::default();

    // Where the field state will stand when the renderer reaches this byte, which
    // decides how a `%%` here is read. See [`ChainState`].
    let mut chain = ChainState::default();

    while i < b.len() {
        if b[i] != b'%' {
            lit.push(b[i]);
            i += 1;
            chain.saw_literal();
            continue;
        }
        let Some(&c) = b.get(i + 1) else {
            // A trailing `%`: `format_commit_one()` switches on the NUL and
            // returns 0.
            push_unconsumed(&mut items, &mut lit);
            i += 1;
            chain.saw(&Item::Unconsumed);
            continue;
        };
        let items_before = items.len();
        let lit_before = lit.len();
        match c {
            // Inside a `%C…` chain the leading `%` is not an escape half at all:
            // `format_and_pad_commit()` has already claimed it, and the second `%`
            // is what `format_commit_one()` is handed — where it is an unknown
            // placeholder worth 0. So only one byte is consumed, the field is laid
            // out, the chain's non-zero `total_consumed` suppresses the bare `%`,
            // and git rescans from the second `%`. Every placeholder after it is
            // one byte out of step with the escape reading, which is why this
            // cannot be sorted out later from the item list.
            b'%' if chain.chaining => {
                push_unconsumed(&mut items, &mut lit);
                i += 1;
            }
            b'%' => {
                lit.push(b'%');
                i += 2;
            }
            b'n' => {
                push(&mut items, &mut lit, Ph::Newline);
                i += 2;
            }
            b'x' => {
                let hi = b.get(i + 2).and_then(|c| (*c as char).to_digit(16));
                let lo = b.get(i + 3).and_then(|c| (*c as char).to_digit(16));
                match (hi, lo) {
                    (Some(hi), Some(lo)) => {
                        push(&mut items, &mut lit, Ph::Byte((hi * 16 + lo) as u8));
                        i += 4;
                    }
                    // Not a valid `%xNN`: git leaves the whole thing alone.
                    _ => {
                        push_unconsumed(&mut items, &mut lit);
                        i += 1;
                    }
                }
            }
            // The column atoms `%<(<N>)`, `%>(<N>)`, `%><(<N>)`, `%>>(<N>)` and
            // their `|`/`trunc`/`ltrunc`/`mtrunc` forms. The snapshot is recorded
            // even when the parse fails, because a bad truncation modifier leaves
            // the width and flush type already stored — and because the attempt
            // itself flushes a field that was already pending.
            b'<' | b'>' => {
                let (parsed, stored) = pad.parse_reporting(b, i + 1);
                flush_lit(&mut items, &mut lit);
                items.push(Item::Pad {
                    state: stored.then_some(pad),
                    consumed: parsed.is_some(),
                });
                i += parsed.map_or(1, |consumed| 1 + consumed);
            }
            // `%w(<width>,<indent1>,<indent2>)`: everything emitted after it is
            // re-wrapped to that width when the parameters next change.
            b'w' => match parse_wrap(b, i + 1) {
                Some((consumed, width, indent1, indent2)) => {
                    flush_lit(&mut items, &mut lit);
                    items.push(Item::Wrap { width, indent1, indent2 });
                    i += 1 + consumed;
                }
                None => {
                    push_unconsumed(&mut items, &mut lit);
                    i += 1;
                }
            },
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
                        push_unconsumed(&mut items, &mut lit);
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
                    anyhow::bail!("unsupported placeholder \"%{ch}\"");
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
                        anyhow::bail!(
                            "unsupported placeholder \"%{ch}{bad}\" (ported: %{ch}n, %{ch}e, %{ch}l, %{ch}N, %{ch}E, %{ch}d, %{ch}D, %{ch}i, %{ch}I, %{ch}s, %{ch}t)"
                        );
                    }
                };
                push(&mut items, &mut lit, ph);
                i += 3;
            }
            b'G' | b'|' | b'+' | b'-' | b' ' => {
                bail!(
                    "unsupported placeholder \"%{}\" (signature and conditional line feeds are not ported)",
                    c as char
                );
            }
            // Unknown to git as well: it consumes nothing, so the `%` prints and
            // the rest is rescanned as literal text.
            _ => {
                push_unconsumed(&mut items, &mut lit);
                i += 1;
            }
        }
        chain.absorb(&items[items_before..], lit.len() > lit_before);
    }

    if !lit.is_empty() {
        items.push(Item::Literal(lit));
    }
    Ok(items)
}

/// The parse-time mirror of the field state [`render_user`] runs on.
///
/// Nothing it tracks depends on the commit — which item a `%` becomes, and which
/// items close a pending field, are fixed by the format string — so the parser can
/// know whether a field will be open when the renderer reaches a given byte.
///
/// It has to, for one case. Inside a pending field `format_and_pad_commit()`
/// swallows the `%` that follows a `%C…` before calling `format_commit_one()`
/// again (pretty.c:1828-1831), so a `%%` there never reaches the driver's escape
/// handling: git consumes the first `%` into the chain and rescans from the
/// second. That shifts every placeholder after it by a byte —
/// `%<(20)%Cred%%s|` renders the *subject*, not a literal `%s` — and a shift is a
/// re-parse, which only the parser can do.
#[derive(Default)]
struct ChainState {
    /// A `%<`/`%>` atom is holding a field open.
    field: bool,
    /// The last item was a `%C…` laid inside that field, so the chain is still
    /// running and the next `%` belongs to it.
    chaining: bool,
}

impl ChainState {
    /// Replay the items one parse step appended. `literal` reports that the step
    /// also began a run of literal bytes, which may not be flushed into an
    /// [`Item::Literal`] until a later step but ends a chain right here.
    fn absorb(&mut self, appended: &[Item], literal: bool) {
        if literal {
            self.saw_literal();
        }
        for item in appended {
            self.saw(item);
        }
    }

    /// Literal text passes through a pending field untouched — but it is not a
    /// `%`, so it ends a `%C…` chain, and the field the chain was holding open is
    /// laid out the moment that happens.
    fn saw_literal(&mut self) {
        if self.chaining {
            self.field = false;
        }
        self.chaining = false;
    }

    fn saw(&mut self, item: &Item) {
        match item {
            Item::Literal(_) => self.saw_literal(),
            // The chain keeps the field open across a `%C…`.
            Item::Placeholder(Ph::Color(_)) if self.field => self.chaining = true,
            // Everything else is the placeholder the field was waiting for, so
            // `PadState::apply` lays it out and clears the flush. A padding atom
            // opens the next field with what it stored — unless it was itself the
            // padded placeholder, in which case `apply` clears what it just stored.
            item => {
                let was_open = self.field;
                self.field = matches!(item, Item::Pad { state: Some(_), .. }) && !was_open;
                self.chaining = false;
            }
        }
    }
}

/// Flush a pending run of literal bytes into `items`.
fn flush_lit(items: &mut Vec<Item>, lit: &mut Vec<u8>) {
    if !lit.is_empty() {
        items.push(Item::Literal(std::mem::take(lit)));
    }
}

/// Record a `%` that `format_commit_item()` answered 0 for. See
/// [`Item::Unconsumed`] for why this is not just a literal `%`.
fn push_unconsumed(items: &mut Vec<Item>, lit: &mut Vec<u8>) {
    flush_lit(items, lit);
    items.push(Item::Unconsumed);
}

/// Parse a `%C*` color placeholder starting at `b[i] == '%'` (`b[i+1] == 'C'`),
/// appending the resolved bytes to `lit`, and return the new index.
///
/// `format-rev` is invoked piped with no `--color`, so color output is off:
/// every form expands to nothing except an explicit `%C(always,<spec>)`.
fn parse_color(b: &[u8], i: usize, items: &mut Vec<Item>, lit: &mut Vec<u8>) -> Result<usize> {
    let mut color = |items: &mut Vec<Item>, lit: &mut Vec<u8>, bytes: Vec<u8>| {
        flush_lit(items, lit);
        items.push(Item::Placeholder(Ph::Color(bytes)));
    };
    let after = &b[i + 2..];
    if after.first() == Some(&b'(') {
        // Find the matching ')'.
        if let Some(rel) = after[1..].iter().position(|&x| x == b')') {
            let inner = &after[1..1 + rel];
            color(items, lit, color_from_paren(inner)?);
            // Consumed: '%', 'C', '(', inner, ')'.
            return Ok(i + rel + 4);
        }
        // No closing ')': git treats `%C(` as unknown and consumes nothing.
        push_unconsumed(items, lit);
        return Ok(i + 1);
    }
    // Bare color words. With color off they all expand to nothing — but they are
    // still `%C…` placeholders, so they consume a pending field and chain into
    // the placeholder that follows.
    for (word, len) in [
        (&b"reset"[..], 5usize),
        (&b"green"[..], 5),
        (&b"blue"[..], 4),
        (&b"red"[..], 3),
    ] {
        if after.starts_with(word) {
            color(items, lit, Vec::new());
            return Ok(i + 2 + len);
        }
    }
    // `%C` followed by anything else: unknown, consumes nothing.
    push_unconsumed(items, lit);
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
            anyhow::bail!("unsupported combined `reset` in %C(always,...)");
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
        anyhow::bail!("unsupported color token in %C(always,...)");
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
        anyhow::bail!("unsupported color token in %C(always,...)");
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
    anyhow::bail!("unsupported color token in %C(always,...)");
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
        // Unterminated: consumes nothing.
        push_unconsumed(items, lit);
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
            anyhow::bail!(
                "unsupported %(describe) options (ported: %(describe), %(describe:tags), %(describe:all))"
            );
        };
        flush_lit(items, lit);
        items.push(Item::Placeholder(Ph::Describe(select)));
        // Consumed: '%', '(', content, ')'.
        return Ok(i + 2 + rel + 1);
    }

    // Anything else: git does not expand it, so it consumes nothing — the `%`
    // prints and `(…)` is rescanned as literal text.
    push_unconsumed(items, lit);
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

/// Evaluate a parsed user format against one commit — git's
/// `repo_format_commit_message()` driver loop over the pre-parsed item list.
///
/// [`Item::Literal`] is copied straight through, leaving a pending field pending.
/// Everything else came from a `%` and so goes through `format_commit_item()`:
/// directly when no field is open, or into a buffer of its own whose *display*
/// width the field measures. A `%C…` keeps pulling the following placeholder into
/// the same field, which is why the colour escape adds bytes but no columns.
fn render_user(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    cr: &gix::objs::CommitRef<'_>,
    items: &[Item],
    mailmap: &gix::mailmap::Snapshot,
    out: &mut Vec<u8>,
) -> Result<()> {
    // The deferred state `struct format_commit_context` carries.
    let mut pad = PadState::default();
    let mut wrap = WrapState::default();
    let mut k = 0;
    while k < items.len() {
        if let Item::Literal(bytes) = &items[k] {
            out.extend_from_slice(bytes);
            k += 1;
            continue;
        }
        if pad.flush == FlushType::None {
            match &items[k] {
                Item::Pad { state, consumed } => {
                    if let Some(state) = state {
                        pad = *state;
                    }
                    if !consumed {
                        out.push(b'%');
                    }
                }
                Item::Wrap { width, indent1, indent2 } => {
                    wrap.rewrap_message_tail(out, *width, *indent1, *indent2);
                }
                Item::Unconsumed => out.push(b'%'),
                Item::Placeholder(ph) => render_placeholder(repo, commit, cr, ph, mailmap, out)?,
                Item::Literal(_) => unreachable!("handled above"),
            }
            k += 1;
            continue;
        }
        // `format_and_pad_commit()`. `padding` is read before the placeholder
        // expands, so a nested `%<(…)` retargets the *next* field, not this one;
        // the flush and truncation modes are read afterwards and do see it.
        let padding = pad.padding;
        let mut local: Vec<u8> = Vec::new();
        let mut unconsumed = false;
        // Whether the chain has already swallowed a `%`. git counts it in
        // `total_consumed`, so a later placeholder that consumes nothing still
        // leaves a non-zero count — and only a zero count prints a bare `%`.
        let mut chained = false;
        loop {
            let modifier = matches!(&items[k], Item::Placeholder(Ph::Color(_)));
            match &items[k] {
                Item::Pad { state, consumed } => {
                    if let Some(state) = state {
                        pad = *state;
                    }
                    unconsumed = !consumed;
                }
                Item::Wrap { width, indent1, indent2 } => {
                    // git rewraps whatever buffer `format_commit_one()` was handed,
                    // which inside a pending field is the field's own.
                    wrap.rewrap_message_tail(&mut local, *width, *indent1, *indent2);
                }
                Item::Unconsumed => unconsumed = true,
                Item::Placeholder(ph) => {
                    render_placeholder(repo, commit, cr, ph, mailmap, &mut local)?;
                }
                Item::Literal(_) => unreachable!("handled above"),
            }
            k += 1;
            if !modifier || unconsumed {
                break;
            }
            // git chains only while the next character is a `%` — every item but
            // a literal run, and the end of the format is not one either.
            match items.get(k) {
                Some(Item::Literal(_)) | None => break,
                Some(_) => chained = true,
            }
        }
        pad.apply(out, local, padding, 0);
        if unconsumed && !chained {
            out.push(b'%');
        }
    }
    // `repo_format_commit_message()` closes with a rewrap to width 0, which wraps
    // whatever a trailing `%w()` was still governing.
    wrap.rewrap_message_tail(out, 0, 0, 0);
    Ok(())
}

/// `format_commit_one()` for the placeholders that render commit data, appending
/// to `out` — which is the output buffer directly, or a pending field's own.
fn render_placeholder(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    cr: &gix::objs::CommitRef<'_>,
    ph: &Ph,
    mailmap: &gix::mailmap::Snapshot,
    out: &mut Vec<u8>,
) -> Result<()> {
    let id = commit.id;
    let msg = cr.message.as_bytes();
    {
        {
            match ph {
                Ph::Newline => out.push(b'\n'),
                Ph::Byte(b) => out.push(*b),
                Ph::Color(bytes) => out.extend_from_slice(bytes),
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
            }
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

    /// A `%C…` inside a pending field swallows the `%` that follows it, so the
    /// `%%` after one is not the escape pair the driver would have expanded: git
    /// takes the first `%` into the chain and rescans from the second, shifting
    /// every placeholder after it by a byte. Captured from stock git 2.55.0 on a
    /// repository whose `HEAD` subject is `short`:
    ///
    /// ```text
    /// $ printf 'HEAD\n' | git format-rev --stdin-mode=revs --format='%<(20)%Cred%%s|'
    ///                     short|
    /// $ printf 'HEAD\n' | git format-rev --stdin-mode=revs --format='%<(20)%Cred%%%s|'
    ///                     %s|
    /// $ printf 'HEAD\n' | git format-rev --stdin-mode=revs --format='%s%Cred%%s|'
    /// short%s|
    /// ```
    ///
    /// The third shows the rule's edge: with no field open there is no chain, so
    /// the `%%` is an ordinary escape and `%s` stays literal text.
    #[test]
    fn a_color_chain_swallows_the_first_half_of_a_following_escape() {
        let kinds = |fmt: &str| -> Vec<&'static str> {
            parse_user_format(fmt)
                .unwrap()
                .iter()
                .map(|item| match item {
                    Item::Literal(_) => "lit",
                    Item::Placeholder(Ph::Color(_)) => "color",
                    Item::Placeholder(Ph::Subject) => "subject",
                    Item::Placeholder(_) => "ph",
                    Item::Pad { .. } => "pad",
                    Item::Wrap { .. } => "wrap",
                    Item::Unconsumed => "unconsumed",
                })
                .collect()
        };
        // Inside a field: the pair is broken up, and `%s` past it is the subject.
        assert_eq!(
            kinds("%<(20)%Cred%%s|"),
            ["pad", "color", "unconsumed", "subject", "lit"]
        );
        // One `%` further along, the rescan lands on a real pair again.
        assert_eq!(
            kinds("%<(20)%Cred%%%s|"),
            ["pad", "color", "unconsumed", "lit"]
        );
        // No field open, so no chain: the pair is an ordinary escape.
        assert_eq!(kinds("%s%Cred%%s|"), ["subject", "color", "lit"]);
        // A literal between the colour atom and the `%%` ends the chain too.
        assert_eq!(kinds("%<(20)%Credx%%s|"), ["pad", "color", "lit"]);
        // The chain survives a second colour atom.
        assert_eq!(
            kinds("%<(20)%Cred%Cgreen%%s|"),
            ["pad", "color", "color", "unconsumed", "subject", "lit"]
        );
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
