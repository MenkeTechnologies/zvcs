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
//!     the `-v` payload — the way git's `GPG_VERIFY_OMIT_STATUS` does
//!   * `--no-verbose`, `--no-raw`, `--`, `-h`
//!   * the pre-gpg failure paths, verbatim: unresolvable name, non-tag object,
//!     and a tag carrying no signature block
//!
//! Exit codes match git: 0 when every named tag verified, 1 when any failed,
//! 129 for usage errors.
//!
//! Not covered (each bails rather than producing a plausible-looking result):
//! ref-filter atoms outside the supported set (git has roughly eighty; only the
//! tag-object atoms below are ported, and an unsupported one bails at render
//! time rather than at git's up-front `verify_ref_format` position, so git's
//! `fatal: unknown field name: <name>` / exit 128 path is NOT reproduced),
//! x509/gpgsm and SSH signatures (git drives `gpgsm` / `ssh-keygen` for those),
//! and a configured `gpg.minTrustLevel` (its trust-level gate is not ported).

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

    let mut had_error = false;
    for name in names {
        if !verify_one(&repo, name, verbose, raw, format.as_deref())? {
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

    if repo.config_snapshot().string("gpg.minTrustLevel").is_some() {
        bail!("gpg.minTrustLevel is not supported");
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

    // A verification counts as good only when gpg exited cleanly and its status
    // stream reported GOODSIG.
    let ok = gpg.exit_ok && status_result(&gpg.status) == Some(b'G');

    // git renders the format only for tags that verified.
    if let Some(tokens) = format.filter(|_| ok) {
        let tag = object
            .try_to_tag_ref()
            .map_err(|e| anyhow::anyhow!("could not decode tag {name:?}: {e}"))?;
        let mut line = render_format(tokens, &tag, &object.id)?;
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
/// Only the tag-object atoms are ported; anything else git accepts bails rather
/// than rendering a plausible-looking substitute.
fn render_format(
    tokens: &[Token],
    tag: &gix::objs::TagRef<'_>,
    id: &gix::hash::ObjectId,
) -> Result<Vec<u8>> {
    let tagger = tag.tagger().ok().flatten();
    let mut out = Vec::new();

    for token in tokens {
        match token {
            Token::Literal(bytes) => out.extend_from_slice(bytes),
            Token::Atom(name) => match name.as_str() {
                "tag" => out.extend_from_slice(tag.name),
                "objectname" => out.extend_from_slice(id.to_hex().to_string().as_bytes()),
                // The atom describes the object being verified, which this far
                // in is always the tag object itself.
                "objecttype" => out.extend_from_slice(b"tag"),
                "taggername" => {
                    if let Some(t) = &tagger {
                        out.extend_from_slice(t.name);
                    }
                }
                // git wraps the address in angle brackets; gix strips them.
                "taggeremail" => {
                    if let Some(t) = &tagger {
                        out.push(b'<');
                        out.extend_from_slice(t.email);
                        out.push(b'>');
                    }
                }
                "contents:subject" => out.extend_from_slice(&subject(tag.message)),
                _ => bail!("unsupported format atom \"%({name})\" (ported: tag, objectname, objecttype, taggername, taggeremail, contents:subject)"),
            },
        }
    }
    Ok(out)
}

/// git's `copy_subject`: the message up to the first blank line, with the
/// newlines inside it folded to spaces and CR dropped before LF.
fn subject(message: &[u8]) -> Vec<u8> {
    let start = message
        .iter()
        .position(|&b| b != b'\n')
        .unwrap_or(message.len());
    let body = &message[start..];

    let mut end = body
        .windows(2)
        .position(|w| w == b"\n\n")
        .unwrap_or(body.len());
    while end > 0 && body[end - 1] == b'\n' {
        end -= 1;
    }
    let region = &body[..end];

    let mut out = Vec::with_capacity(region.len());
    for (i, &b) in region.iter().enumerate() {
        match b {
            b'\r' if region.get(i + 1) == Some(&b'\n') => {}
            b'\n' => out.push(b' '),
            _ => out.push(b),
        }
    }
    out
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
