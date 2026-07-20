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
//!   * `-v` / `--verbose`          → write the tag payload to stdout first
//!   * `--raw`                     → emit gpg's `--status-fd` lines instead
//!   * `--no-verbose`, `--no-raw`, `--`, `-h`
//!   * the pre-gpg failure paths, verbatim: unresolvable name, non-tag object,
//!     and a tag carrying no signature block
//!
//! Exit codes match git: 0 when every named tag verified, 1 when any failed,
//! 129 for usage errors.
//!
//! Not covered (each bails rather than producing a plausible-looking result):
//! `--format=<fmt>` (needs the ref-filter formatter), x509/gpgsm and SSH
//! signatures (git drives `gpgsm` / `ssh-keygen` for those), and a configured
//! `gpg.minTrustLevel` (its trust-level gate is not ported).

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
            // Accepted by git but only applied after a successful verification;
            // rendering it needs the ref-filter formatter, so refuse precisely.
            "--format" | "--no-format" => bail!("unsupported flag {a:?} (ported: -v, --raw)"),
            _ if a.starts_with("--format=") => {
                bail!("unsupported flag {a:?} (ported: -v, --raw)")
            }
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

    if names.is_empty() {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;

    let mut had_error = false;
    for name in names {
        if !verify_one(&repo, name, verbose, raw)? {
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
fn verify_one(repo: &gix::Repository, name: &str, verbose: bool, raw: bool) -> Result<bool> {
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

    if verbose {
        std::io::stdout().write_all(payload)?;
    }

    let gpg = run_gpg(repo, payload, signature)?;

    // git prints gpg's stderr by default, or its `--status-fd` stream under
    // --raw; either way verbatim, on stderr.
    let shown = if raw { &gpg.status } else { &gpg.output };
    std::io::stderr().write_all(shown)?;

    // A verification counts as good only when gpg exited cleanly and its status
    // stream reported GOODSIG.
    Ok(gpg.exit_ok && status_result(&gpg.status) == Some(b'G'))
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
