//! `git verify-tag` — check the GPG signature of tag objects.
//!
//! Mirrors stock `git verify-tag` (builtin/verify-tag.c + gpg-interface.c),
//! which does not verify signatures itself: it splits the tag object into
//! payload and signature, then hands both to `gpg` and passes gpg's own output
//! straight through. This port does the same, so the human-readable text is
//! byte-identical by construction.
//!
//! Implemented:
//!   * `git verify-tag <tag>...`   → verify each, stderr carries gpg's output
//!   * `-v` / `--verbose`          → write the tag payload to stdout
//!   * `--raw`                     → emit gpg's `--status-fd` lines instead
//!   * `--format=<fmt>` / `--format <fmt>` / `--no-format` → render the tag
//!     through the ref-filter atoms handled by `render_format`, after a
//!     successful verification only, and suppress gpg's own output — including
//!     the `-v` payload — the way git's `GPG_VERIFY_OMIT_STATUS` does. The
//!     ported atoms are `tag`, `objectname`, `objecttype`, `objectsize`,
//!     `taggername`, `taggeremail`, `taggerdate`, `creatordate`,
//!     `contents:subject`, `contents:body`, `contents:signature`, and the
//!     `%(if)` / `%(then)` / `%(else)` / `%(end)` conditional atoms
//!     (`%(if:equals=…)` / `%(if:notequals=…)` included). The message split is
//!     a byte-for-byte port of ref-filter.c's `find_subpos` over the raw tag
//!     object, and the date atoms render git's default `show_date` format.
//!   * `gpg.minTrustLevel` → the tag is rejected unless gpg's status stream
//!     reports a trust level at or above the configured minimum, matching
//!     gpg-interface.c's `status |= sigc->trust_level < configured_min_trust_level`
//!   * `--no-verbose`, `--no-raw`, `--`, `-h`
//!   * the pre-gpg failure paths, verbatim: unresolvable name, non-tag object,
//!     and a tag carrying no signature block
//!
//! Exit codes match git: 0 when every named tag verified, 1 when any failed,
//! 129 for usage errors.
//!
//! Not covered (each bails rather than producing a plausible-looking result):
//! ref-filter atoms outside the supported set (git has roughly eighty; only the
//! tag-object atoms above are ported, and an unsupported one — including the
//! `:<format>` date variants and `objectsize:disk` — bails at render time
//! rather than at git's up-front `verify_ref_format` position, so git's
//! `fatal: unknown field name: <name>` / exit 128 path is NOT reproduced), and
//! x509/gpgsm and SSH signatures (git drives `gpgsm` / `ssh-keygen` for those).

use anyhow::{bail, Result};
use std::io::Write;
use std::process::{Command, ExitCode, Stdio};

use gix::bstr::ByteSlice;
use gix::objs::Kind;

/// The parse-options usage block, byte-for-byte as git 2.55 emits it.
const USAGE: &str = "\
usage: git verify-tag [-v | --verbose] [--format=<format>] [--raw] <tag>...

    -v, --[no-]verbose    print tag contents
    --[no-]raw            print raw gpg status output
    --[no-]format <format>
                          format to use for the output

";

/// Signature block openers git recognises, with the signing backend each implies.
const SIG_MARKERS: &[(&str, SigKind)] = &[
    ("-----BEGIN PGP SIGNATURE-----", SigKind::OpenPgp),
    ("-----BEGIN PGP MESSAGE-----", SigKind::OpenPgp),
    ("-----BEGIN SIGNED MESSAGE-----", SigKind::X509),
    ("-----BEGIN SSH SIGNATURE-----", SigKind::Ssh),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum SigKind {
    OpenPgp,
    X509,
    Ssh,
}

pub fn verify_tag(args: &[String]) -> Result<ExitCode> {
    let mut verbose = false;
    let mut raw = false;
    let mut format: Option<&str> = None;
    let mut names: Vec<&str> = Vec::new();
    let mut operands_only = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;

        if operands_only || !a.starts_with('-') || a == "-" {
            names.push(a);
            continue;
        }
        match a {
            "--" => operands_only = true,
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "--raw" => raw = true,
            "--no-raw" => raw = false,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // `OPT_STRING`: the separate-argument spelling swallows the next
            // argv entry even when that entry looks like an operand, and
            // running out of arguments is git's own "requires a value" error.
            "--format" => match args.get(i) {
                Some(v) => {
                    format = Some(v.as_str());
                    i += 1;
                }
                None => {
                    eprintln!("error: option `format' requires a value");
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            },
            "--no-format" => format = None,
            _ if a.starts_with("--format=") => format = Some(&a["--format=".len()..]),
            _ => {
                // git's parse-options wording, then the usage block.
                let (kind, name) = match a.strip_prefix("--") {
                    Some(long) => ("option", long),
                    None => ("switch", &a[1..]),
                };
                eprintln!("error: unknown {kind} `{name}'");
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
    }

    // git checks for missing operands before it validates the format, so a
    // format that is both operand-less and malformed reports only the usage.
    if names.is_empty() {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // `verify_ref_format` runs once, up front, and a syntax error there is a
    // usage error rather than a per-tag failure.
    let format = match format.map(parse_format).transpose() {
        Ok(f) => f,
        Err(unterminated) => {
            eprintln!("error: malformed format string {unterminated}");
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
    };

    let repo = gix::discover(".")?;

    // git reads `gpg.mintrustlevel` while loading config, before it verifies
    // anything; an unparseable value is a fatal config error, not a per-tag
    // failure.
    let min_trust = match configured_min_trust_level(&repo) {
        Ok(level) => level,
        Err(value) => {
            eprintln!("error: invalid value for 'gpg.mintrustlevel': '{value}'");
            return Ok(ExitCode::from(128));
        }
    };

    let mut had_error = false;
    for name in names {
        if !verify_one(&repo, name, verbose, raw, format.as_deref(), min_trust)? {
            had_error = true;
        }
    }

    Ok(if had_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Verify a single named tag. Returns `false` when git would count it as a
/// failure; diagnostics go to stderr exactly as git words them.
fn verify_one(
    repo: &gix::Repository,
    name: &str,
    verbose: bool,
    raw: bool,
    format: Option<&[Token]>,
    min_trust: u8,
) -> Result<bool> {
    let Ok(id) = repo.rev_parse_single(name) else {
        eprintln!("error: tag '{name}' not found.");
        return Ok(false);
    };

    // git asks `oid_object_info` for the type first; a missing object yields no
    // type name at all, which its `error()` renders as `(null)`.
    let Ok(object) = repo.find_object(id.detach()) else {
        eprintln!("error: {name}: cannot verify a non-tag object of type (null).");
        return Ok(false);
    };
    if object.kind != Kind::Tag {
        eprintln!(
            "error: {name}: cannot verify a non-tag object of type {}.",
            object.kind
        );
        return Ok(false);
    }

    let Some((split, kind)) = split_signature(&object.data) else {
        // Unsigned: the whole object is the payload, and git still prints it
        // under -v before reporting the failure.
        if verbose {
            std::io::stdout().write_all(&object.data)?;
        }
        eprintln!("error: no signature found");
        return Ok(false);
    };
    let (payload, signature) = object.data.split_at(split);

    match kind {
        SigKind::OpenPgp => {}
        SigKind::X509 => bail!("x509 signatures are not supported (needs the gpgsm backend)"),
        SigKind::Ssh => bail!("ssh signatures are not supported (needs the ssh-keygen backend)"),
    }

    let gpg = run_gpg(repo, payload, signature)?;

    // `print_signature_buffer` runs after the check, and `--format` sets
    // GPG_VERIFY_OMIT_STATUS, which skips the whole thing — so under --format
    // even `-v` prints no payload here (an unsigned tag still does, above,
    // because that path returns before the omit-status gate).
    if format.is_none() {
        if verbose {
            std::io::stdout().write_all(payload)?;
        }
        // gpg's stderr by default, or its `--status-fd` stream under --raw;
        // either way verbatim, on stderr.
        let shown = if raw { &gpg.status } else { &gpg.output };
        std::io::stderr().write_all(shown)?;
    }

    // A verification counts as good only when gpg exited cleanly, its status
    // stream reported GOODSIG, and the reported trust level clears the
    // configured minimum (git's `status |= sigc->trust_level < min`).
    let ok = gpg.exit_ok
        && status_result(&gpg.status) == Some(b'G')
        && status_trust_level(&gpg.status) >= min_trust;

    // git renders the format only for tags that verified.
    if let Some(tokens) = format.filter(|_| ok) {
        let tag = object
            .try_to_tag_ref()
            .map_err(|e| anyhow::anyhow!("could not decode tag {name:?}: {e}"))?;
        let mut line = render_format(tokens, &tag, &object.id, &object.data)?;
        line.push(b'\n');
        std::io::stdout().write_all(&line)?;
    }

    Ok(ok)
}

/// One piece of a parsed `--format` string: literal bytes or a `%(...)` atom.
enum Token {
    Literal(Vec<u8>),
    Atom(String),
}

/// Split a `--format` string into literals and atoms.
///
/// `%%` is a literal percent and a `%` that does not open an atom stays
/// literal; an unterminated `%(` is git's "malformed format string", and the
/// error carries the remainder git echoes back, starting at that `%(`.
fn parse_format(fmt: &str) -> std::result::Result<Vec<Token>, String> {
    let bytes = fmt.as_bytes();
    let mut tokens = Vec::new();
    let mut literal: Vec<u8> = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'%' {
            literal.push(bytes[i]);
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            Some(b'%') => {
                literal.push(b'%');
                i += 2;
            }
            Some(b'(') => {
                let Some(end) = bytes[i + 2..].iter().position(|&b| b == b')') else {
                    return Err(fmt[i..].to_string());
                };
                let end = i + 2 + end;
                if !literal.is_empty() {
                    tokens.push(Token::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(Token::Atom(fmt[i + 2..end].to_string()));
                i = end + 1;
            }
            _ => {
                literal.push(b'%');
                i += 1;
            }
        }
    }
    if !literal.is_empty() {
        tokens.push(Token::Literal(literal));
    }
    Ok(tokens)
}

/// Expand the parsed format against a verified tag object.
///
/// This is a port of ref-filter.c's `format_ref_array_item` driver plus the
/// tag-object atoms and the `%(if)`/`%(then)`/`%(else)`/`%(end)` conditional
/// machinery. Literals and value atoms append to the top of a formatting stack;
/// the conditional atoms push and pop stack frames exactly as git does. Only
/// the tag-object atoms are ported; anything else git accepts bails rather than
/// rendering a plausible-looking substitute.
fn render_format(
    tokens: &[Token],
    tag: &gix::objs::TagRef<'_>,
    id: &gix::hash::ObjectId,
    data: &[u8],
) -> Result<Vec<u8>> {
    let tagger = tag.tagger().ok().flatten();
    let sub = find_subpos(data);

    // The base frame; `%(if)` pushes further frames on top of it.
    let mut stack: Vec<Frame> = vec![Frame::default()];

    for token in tokens {
        match token {
            Token::Literal(bytes) => stack
                .last_mut()
                .expect("base frame")
                .output
                .extend_from_slice(bytes),
            Token::Atom(name) => {
                let name = name.as_str();
                // Conditional atoms manipulate the stack; every other atom is a
                // value that is appended to the current top frame.
                if name == "if" {
                    push_if(&mut stack, CmpStatus::None, None);
                } else if let Some(s) = name.strip_prefix("if:equals=") {
                    push_if(&mut stack, CmpStatus::Equal, Some(s.as_bytes().to_vec()));
                } else if let Some(s) = name.strip_prefix("if:notequals=") {
                    push_if(&mut stack, CmpStatus::Unequal, Some(s.as_bytes().to_vec()));
                } else if name == "then" {
                    then_atom(&mut stack)?;
                } else if name == "else" {
                    else_atom(&mut stack)?;
                } else if name == "end" {
                    end_atom(&mut stack)?;
                } else if let Some(bad) = name.strip_prefix("if:") {
                    bail!("unrecognized %(if) argument: {bad}");
                } else {
                    let value = atom_value(name, tag, id, data, &sub, &tagger)?;
                    stack.last_mut().expect("base frame").output.extend(value);
                }
            }
        }
    }

    if stack.len() != 1 {
        bail!("format: %(end) atom missing");
    }
    Ok(stack.pop().expect("base frame").output)
}

/// Compute the byte value of a single tag-object atom.
fn atom_value(
    name: &str,
    tag: &gix::objs::TagRef<'_>,
    id: &gix::hash::ObjectId,
    data: &[u8],
    sub: &SubPos,
    tagger: &Option<gix::actor::SignatureRef<'_>>,
) -> Result<Vec<u8>> {
    Ok(match name {
        "tag" => tag.name.to_vec(),
        "objectname" => id.to_hex().to_string().into_bytes(),
        // The atom describes the object being verified, which this far in is
        // always the tag object itself.
        "objecttype" => b"tag".to_vec(),
        // git's `objectsize` is the object's content length.
        "objectsize" => data.len().to_string().into_bytes(),
        "taggername" => tagger.as_ref().map(|t| t.name.to_vec()).unwrap_or_default(),
        // git wraps the address in angle brackets; gix strips them.
        "taggeremail" => match tagger {
            Some(t) => {
                let mut v = Vec::with_capacity(t.email.len() + 2);
                v.push(b'<');
                v.extend_from_slice(t.email);
                v.push(b'>');
                v
            }
            None => Vec::new(),
        },
        // A tag's creator is its tagger, so both dates come from the same line,
        // rendered in git's default `show_date` format. A missing or malformed
        // tagger is git's empty-string `bad:` path.
        "taggerdate" | "creatordate" => match tagger.as_ref().and_then(|t| t.time().ok()) {
            Some(time) => time
                .format_or_unix(gix::date::time::format::DEFAULT)
                .into_bytes(),
            None => Vec::new(),
        },
        "contents:subject" => copy_subject(&data[sub.subject.0..sub.subject.0 + sub.subject.1]),
        // C_BODY: the body up to, but excluding, the signature.
        "contents:body" => data[sub.body_start..sub.body_start + sub.nonsiglen].to_vec(),
        // C_SIG: the signature block, verbatim.
        "contents:signature" => data[sub.sig.0..sub.sig.0 + sub.sig.1].to_vec(),
        _ => bail!("unsupported format atom \"%({name})\" (ported: tag, objectname, objecttype, objectsize, taggername, taggeremail, taggerdate, creatordate, contents:subject, contents:body, contents:signature, if, then, else, end)"),
    })
}

/// git's `copy_subject`: fold the subject region's newlines to spaces and drop
/// a CR that immediately precedes an LF.
fn copy_subject(buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'\r' && buf.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(if buf[i] == b'\n' { b' ' } else { buf[i] });
        i += 1;
    }
    out
}

/// A frame of the ref-filter formatting stack: the accumulated output plus, for
/// a `%(if)` block, the conditional bookkeeping attached to it.
#[derive(Default)]
struct Frame {
    output: Vec<u8>,
    cond: Option<IfThenElse>,
}

/// git's `cmp_status` for the `%(if)` variants.
#[derive(Clone, Copy)]
enum CmpStatus {
    Equal,
    Unequal,
    None,
}

/// git's `struct if_then_else`.
struct IfThenElse {
    cmp_status: CmpStatus,
    str: Option<Vec<u8>>,
    then_atom_seen: bool,
    else_atom_seen: bool,
    condition_satisfied: bool,
}

/// `%(if)` / `%(if:equals=…)` / `%(if:notequals=…)`: push a fresh frame whose
/// output collects the condition text until `%(then)`.
fn push_if(stack: &mut Vec<Frame>, cmp_status: CmpStatus, str: Option<Vec<u8>>) {
    stack.push(Frame {
        output: Vec::new(),
        cond: Some(IfThenElse {
            cmp_status,
            str,
            then_atom_seen: false,
            else_atom_seen: false,
            condition_satisfied: false,
        }),
    });
}

/// `%(then)`: decide the condition from the text collected since `%(if)`, then
/// reset the frame so it collects the then-branch.
fn then_atom(stack: &mut Vec<Frame>) -> Result<()> {
    let cur = stack.last_mut().expect("base frame");
    let (cmp_status, needle) = match &mut cur.cond {
        Some(c) => {
            if c.then_atom_seen {
                bail!("format: %(then) atom used more than once");
            }
            if c.else_atom_seen {
                bail!("format: %(then) atom used after %(else)");
            }
            c.then_atom_seen = true;
            (c.cmp_status, c.str.clone())
        }
        None => bail!("format: %(then) atom used without a %(if) atom"),
    };

    let satisfied = match cmp_status {
        CmpStatus::Equal => needle.as_deref().unwrap_or(b"") == cur.output.as_slice(),
        CmpStatus::Unequal => needle.as_deref().unwrap_or(b"") != cur.output.as_slice(),
        // git: a non-empty, non-whitespace-only condition is truthy.
        CmpStatus::None => !is_empty(&cur.output),
    };
    let cond = cur.cond.as_mut().expect("condition present");
    cond.condition_satisfied = satisfied;
    cur.output.clear();
    Ok(())
}

/// `%(else)`: hand the condition to a new frame that collects the else-branch,
/// leaving the then-branch text behind in the previous frame.
fn else_atom(stack: &mut Vec<Frame>) -> Result<()> {
    let cur = stack.last_mut().expect("base frame");
    match &cur.cond {
        Some(c) if !c.then_atom_seen => {
            bail!("format: %(else) atom used without a %(then) atom")
        }
        Some(c) if c.else_atom_seen => bail!("format: %(else) atom used more than once"),
        Some(_) => {}
        None => bail!("format: %(else) atom used without a %(if) atom"),
    }
    let mut cond = cur.cond.take().expect("condition present");
    cond.else_atom_seen = true;
    stack.push(Frame {
        output: Vec::new(),
        cond: Some(cond),
    });
    Ok(())
}

/// `%(end)`: resolve the `%(if)` block and fold the surviving branch into the
/// parent frame — a port of `end_atom_handler` + `if_then_else_handler`.
fn end_atom(stack: &mut Vec<Frame>) -> Result<()> {
    let cond = match stack.last_mut().and_then(|f| f.cond.take()) {
        Some(c) => c,
        None => bail!("format: %(end) atom used without corresponding atom"),
    };
    if !cond.then_atom_seen {
        bail!("format: %(if) atom used without a %(then) atom");
    }

    if cond.else_atom_seen {
        // Top frame holds the else-branch; the frame below holds the then-branch.
        if cond.condition_satisfied {
            stack.last_mut().expect("else frame").output.clear();
        } else {
            let n = stack.len();
            let else_out = std::mem::take(&mut stack[n - 1].output);
            stack[n - 2].output = else_out;
            stack[n - 1].output.clear();
        }
        pop_into_prev(stack); // fold the emptied else frame away
        pop_into_prev(stack); // fold the chosen branch into the parent
    } else {
        if !cond.condition_satisfied {
            stack.last_mut().expect("if frame").output.clear();
        }
        pop_into_prev(stack);
    }
    Ok(())
}

/// git's `pop_stack_element`: append the top frame's output to its parent and
/// drop it.
fn pop_into_prev(stack: &mut Vec<Frame>) {
    let cur = stack.pop().expect("frame to pop");
    if let Some(prev) = stack.last_mut() {
        prev.output.extend_from_slice(&cur.output);
    }
}

/// git's `is_empty`: whether the buffer is empty or all-whitespace, using the
/// same set of bytes as C's `isspace` in the C locale.
fn is_empty(buf: &[u8]) -> bool {
    buf.iter()
        .all(|b| matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
}

/// The byte ranges of a tag object's subject, body, and signature, matching
/// ref-filter.c's `find_subpos`.
struct SubPos {
    /// `(start, len)` of the subject, trailing newlines already trimmed.
    subject: (usize, usize),
    /// Start of the body (after the subject and its blank line).
    body_start: usize,
    /// Length of the body up to the signature (git's `nonsiglen`).
    nonsiglen: usize,
    /// `(start, len)` of the signature block.
    sig: (usize, usize),
}

/// Port of ref-filter.c's `find_subpos`, run over the raw tag object.
fn find_subpos(buf: &[u8]) -> SubPos {
    let end = buf.len();

    // parse_signature over the whole object: the signature runs from the last
    // signature-start line to the end.
    let sig_start = parse_signed_buffer(buf);
    let sig = (sig_start, end - sig_start);

    // Skip past the header until the blank line, then over the blank lines.
    let mut i = 0;
    while i < end && buf[i] != b'\n' {
        i = match buf[i..].iter().position(|&b| b == b'\n') {
            Some(p) => i + p + 1,
            None => end,
        };
    }
    while i < end && buf[i] == b'\n' {
        i += 1;
    }

    // Where the signature begins relative to the message start.
    let sigstart = i + parse_signed_buffer(&buf[i..]);

    // Subject: the first paragraph, capped at the signature.
    let rel = &buf[i..end];
    let para = find_subslice(rel, b"\n\n")
        .or_else(|| find_subslice(rel, b"\r\n\r\n"))
        .map(|p| i + p);
    let subject_end = para.map_or(sigstart, |e| e.min(sigstart));
    let mut sublen = subject_end - i;
    while sublen > 0 && matches!(buf[i + sublen - 1], b'\n' | b'\r') {
        sublen -= 1;
    }

    // Body: after the subject's trailing blank lines, up to the signature.
    let mut b = subject_end;
    while b < end && matches!(buf[b], b'\n' | b'\r') {
        b += 1;
    }
    let nonsiglen = sigstart.saturating_sub(b);

    SubPos {
        subject: (i, sublen),
        body_start: b,
        nonsiglen,
        sig,
    }
}

/// git's `parse_signed_buffer`: the offset of the last line that starts a
/// signature block, or the buffer length when there is none.
fn parse_signed_buffer(buf: &[u8]) -> usize {
    let size = buf.len();
    let mut len = 0;
    let mut m = size;
    while len < size {
        if is_signature_start(&buf[len..]) {
            m = len;
        }
        len = match buf[len..].iter().position(|&b| b == b'\n') {
            Some(p) => len + p + 1,
            None => size,
        };
    }
    m
}

/// git's `get_format_by_sig`: whether `line` opens any known signature block.
fn is_signature_start(line: &[u8]) -> bool {
    const MARKERS: &[&[u8]] = &[
        b"-----BEGIN PGP SIGNATURE-----",
        b"-----BEGIN PGP MESSAGE-----",
        b"-----BEGIN SIGNED MESSAGE-----",
        b"-----BEGIN SSH SIGNATURE-----",
    ];
    MARKERS.iter().any(|m| line.starts_with(m))
}

/// First index of `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Byte offset at which the signature block starts, plus the backend it names.
///
/// Only a marker anchored at the start of a line counts, so a marker quoted
/// inside the tag message does not truncate the payload. The earliest such
/// marker wins, matching git's `parse_signature`.
fn split_signature(data: &[u8]) -> Option<(usize, SigKind)> {
    let mut best: Option<(usize, SigKind)> = None;
    for (marker, kind) in SIG_MARKERS {
        let Some(at) = find_at_line_start(data, marker.as_bytes()) else {
            continue;
        };
        match best {
            Some((prev, _)) if prev <= at => {}
            _ => best = Some((at, *kind)),
        }
    }
    best
}

/// First occurrence of `needle` in `haystack` that begins a line.
fn find_at_line_start(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .find(|&i| (i == 0 || haystack[i - 1] == b'\n') && &haystack[i..i + needle.len()] == needle)
}

/// What `gpg` reported: its two output streams and whether it exited cleanly.
struct GpgRun {
    status: Vec<u8>,
    output: Vec<u8>,
    exit_ok: bool,
}

/// Run the configured OpenPGP program the way git does: the detached signature
/// in a temporary file, the payload on stdin, status lines on fd 1.
fn run_gpg(repo: &gix::Repository, payload: &[u8], signature: &[u8]) -> Result<GpgRun> {
    let snapshot = repo.config_snapshot();
    let program = snapshot
        .string("gpg.openpgp.program")
        .or_else(|| snapshot.string("gpg.program"))
        .map(|v| v.to_str_lossy().into_owned())
        .unwrap_or_else(|| "gpg".to_string());

    let sig_path = std::env::temp_dir().join(format!(
        ".git_vtag_tmp{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&sig_path, signature)?;

    let spawned = Command::new(&program)
        .arg("--keyid-format=long")
        .arg("--status-fd=1")
        .arg("--verify")
        .arg(&sig_path)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&sig_path);
            bail!("could not run {program:?}: {e}");
        }
    };

    // Feed the payload from a helper thread: gpg may emit its status lines
    // before it has drained stdin, and a single-threaded write would deadlock
    // on a full pipe for large tag messages.
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let payload = payload.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&payload);
    });

    let out = child.wait_with_output();
    let _ = writer.join();
    let _ = std::fs::remove_file(&sig_path);
    let out = out?;

    Ok(GpgRun {
        status: out.stdout,
        output: out.stderr,
        exit_ok: out.status.success(),
    })
}

/// The result character git derives from gpg's status stream (`G` = GOODSIG,
/// `B` = BADSIG, and so on). The last matching line wins, as in git.
fn status_result(status: &[u8]) -> Option<u8> {
    const CHECKS: &[(u8, &str)] = &[
        (b'G', "GOODSIG "),
        (b'B', "BADSIG "),
        (b'E', "ERRSIG "),
        (b'X', "EXPSIG "),
        (b'Y', "EXPKEYSIG "),
        (b'R', "REVKEYSIG "),
    ];

    let mut result = None;
    for line in status.split(|&b| b == b'\n') {
        let Some(rest) = line.strip_prefix(b"[GNUPG:] ") else {
            continue;
        };
        for (ch, check) in CHECKS {
            if rest.starts_with(check.as_bytes()) {
                result = Some(*ch);
            }
        }
    }
    result
}

/// git's default `configured_min_trust_level` — `TRUST_UNDEFINED`, the lowest
/// level, so an unset `gpg.minTrustLevel` never rejects a signature.
const TRUST_UNDEFINED: u8 = 0;

/// Map a trust-level name to git's numeric `enum signature_trust_level`.
///
/// The keys are as gpg emits them in its status stream, and as git stores them
/// after upper-casing the config value.
fn trust_level_from_key(key: &[u8]) -> Option<u8> {
    match key {
        b"UNDEFINED" => Some(0),
        b"NEVER" => Some(1),
        b"MARGINAL" => Some(2),
        b"FULLY" => Some(3),
        b"ULTIMATE" => Some(4),
        _ => None,
    }
}

/// The minimum trust level from `gpg.minTrustLevel`, or `TRUST_UNDEFINED` when
/// unset. `Err` carries the offending value for git's fatal config error.
fn configured_min_trust_level(repo: &gix::Repository) -> std::result::Result<u8, String> {
    let snapshot = repo.config_snapshot();
    match snapshot.string("gpg.minTrustLevel") {
        None => Ok(TRUST_UNDEFINED),
        Some(value) => {
            let value = value.to_str_lossy().into_owned();
            trust_level_from_key(value.to_uppercase().as_bytes()).ok_or(value)
        }
    }
}

/// The trust level git reads from gpg's status stream: the level named on the
/// last `TRUST_<LEVEL>` line, or `TRUST_UNDEFINED` when none is present.
fn status_trust_level(status: &[u8]) -> u8 {
    let mut level = TRUST_UNDEFINED;
    for line in status.split(|&b| b == b'\n') {
        let Some(rest) = line.strip_prefix(b"[GNUPG:] ") else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(b"TRUST_") else {
            continue;
        };
        // git takes the token up to the first space (`strcspn(line, " \n")`).
        let token: &[u8] = rest.split(|&b| b == b' ').next().unwrap_or(rest);
        if let Some(parsed) = trust_level_from_key(token) {
            level = parsed;
        }
    }
    level
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrap a message body in a minimal, well-formed annotated tag object.
    fn tag_bytes(body: &str) -> Vec<u8> {
        format!(
            "object {}\ntype commit\ntag v1\ntagger A U Thor <a@example.com> 1000000000 +0000\n\n{body}",
            "0".repeat(40),
        )
        .into_bytes()
    }

    #[test]
    fn find_subpos_splits_subject_body_signature() {
        let data = tag_bytes(
            "Subject line\n\nBody one\nBody two\n\
             -----BEGIN PGP SIGNATURE-----\nAAAA\n-----END PGP SIGNATURE-----\n",
        );
        let sub = find_subpos(&data);
        assert_eq!(
            data[sub.subject.0..sub.subject.0 + sub.subject.1].to_vec(),
            b"Subject line".to_vec()
        );
        // git's contents:body keeps the newline that precedes the signature.
        assert_eq!(
            data[sub.body_start..sub.body_start + sub.nonsiglen].to_vec(),
            b"Body one\nBody two\n".to_vec()
        );
        assert_eq!(
            data[sub.sig.0..sub.sig.0 + sub.sig.1].to_vec(),
            b"-----BEGIN PGP SIGNATURE-----\nAAAA\n-----END PGP SIGNATURE-----\n".to_vec()
        );
    }

    #[test]
    fn find_subpos_folds_multiline_subject() {
        let data = tag_bytes("first\nsecond\n\nbody\n-----BEGIN PGP SIGNATURE-----\nX\n");
        let sub = find_subpos(&data);
        assert_eq!(
            copy_subject(&data[sub.subject.0..sub.subject.0 + sub.subject.1]),
            b"first second".to_vec()
        );
    }

    #[test]
    fn copy_subject_drops_cr_before_lf() {
        assert_eq!(copy_subject(b"a\r\nb"), b"a b".to_vec());
        assert_eq!(copy_subject(b"a\nb"), b"a b".to_vec());
    }

    #[test]
    fn parse_signed_buffer_finds_marker_line() {
        assert_eq!(parse_signed_buffer(b"hello\n-----BEGIN PGP SIGNATURE-----\nsig\n"), 6);
        assert_eq!(parse_signed_buffer(b"no marker\n"), b"no marker\n".len());
    }

    #[test]
    fn is_empty_matches_isspace() {
        assert!(is_empty(b""));
        assert!(is_empty(b"  \t\n"));
        assert!(!is_empty(b" x "));
    }

    #[test]
    fn status_trust_level_reads_last_trust_line() {
        let status = b"[GNUPG:] GOODSIG ABC\n[GNUPG:] TRUST_FULLY 0 pgp\n";
        assert_eq!(status_trust_level(status), 3);
        assert_eq!(status_trust_level(b"[GNUPG:] GOODSIG ABC\n"), TRUST_UNDEFINED);
        assert_eq!(status_trust_level(b"[GNUPG:] TRUST_ULTIMATE\n"), 4);
    }

    #[test]
    fn trust_levels_are_ordered() {
        assert_eq!(trust_level_from_key(b"UNDEFINED"), Some(0));
        assert_eq!(trust_level_from_key(b"NEVER"), Some(1));
        assert_eq!(trust_level_from_key(b"MARGINAL"), Some(2));
        assert_eq!(trust_level_from_key(b"FULLY"), Some(3));
        assert_eq!(trust_level_from_key(b"ULTIMATE"), Some(4));
        assert_eq!(trust_level_from_key(b"bogus"), None);
    }

    /// Drive the `%(if)`/`%(then)`/`%(else)`/`%(end)` stack the way
    /// `render_format` does, without needing a gpg-verified tag.
    fn eval_if(cond: &[u8], then_branch: &[u8], else_branch: Option<&[u8]>) -> Vec<u8> {
        let mut stack: Vec<Frame> = vec![Frame::default()];
        push_if(&mut stack, CmpStatus::None, None);
        stack.last_mut().unwrap().output.extend_from_slice(cond);
        then_atom(&mut stack).unwrap();
        stack.last_mut().unwrap().output.extend_from_slice(then_branch);
        if let Some(e) = else_branch {
            else_atom(&mut stack).unwrap();
            stack.last_mut().unwrap().output.extend_from_slice(e);
        }
        end_atom(&mut stack).unwrap();
        assert_eq!(stack.len(), 1);
        stack.pop().unwrap().output
    }

    #[test]
    fn if_then_else_selects_branch() {
        assert_eq!(eval_if(b"x", b"THEN", Some(b"ELSE")), b"THEN".to_vec());
        assert_eq!(eval_if(b"", b"THEN", Some(b"ELSE")), b"ELSE".to_vec());
        assert_eq!(eval_if(b"  \n", b"THEN", Some(b"ELSE")), b"ELSE".to_vec());
        assert_eq!(eval_if(b"", b"THEN", None), Vec::<u8>::new());
        assert_eq!(eval_if(b"x", b"THEN", None), b"THEN".to_vec());
    }

    #[test]
    fn if_equals_compares_condition() {
        let mut stack: Vec<Frame> = vec![Frame::default()];
        push_if(&mut stack, CmpStatus::Equal, Some(b"tag".to_vec()));
        stack.last_mut().unwrap().output.extend_from_slice(b"tag");
        then_atom(&mut stack).unwrap();
        stack.last_mut().unwrap().output.extend_from_slice(b"EQ");
        end_atom(&mut stack).unwrap();
        assert_eq!(stack.pop().unwrap().output, b"EQ".to_vec());

        // A mismatch drops the then-branch.
        let mut stack: Vec<Frame> = vec![Frame::default()];
        push_if(&mut stack, CmpStatus::Equal, Some(b"tag".to_vec()));
        stack.last_mut().unwrap().output.extend_from_slice(b"commit");
        then_atom(&mut stack).unwrap();
        stack.last_mut().unwrap().output.extend_from_slice(b"EQ");
        end_atom(&mut stack).unwrap();
        assert_eq!(stack.pop().unwrap().output, Vec::<u8>::new());
    }
}
