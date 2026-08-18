//! Signature verification reaches the code that does it.
//!
//! Every expectation below was measured from stock git 2.55.0
//! (`/opt/homebrew/bin/git`) first and is asserted here as a literal, so the
//! test needs no stock git of its own to run.
//!
//! What it pins down, in the order the cases appear:
//!
//!   * `pull --no-verify-signatures` **pulls**. It used to share an arm with the
//!     positive spelling and refuse, so the negation of a check turned into a
//!     hard failure — the one bug here that needed no signature at all to see.
//!   * `pull --verify-signatures` against an unsigned head is git's
//!     `fatal: Commit <short> does not have a GPG signature.` at 128, with the
//!     merge not performed. No key material is involved: the head has no
//!     signature, so no checker is ever spawned.
//!   * the same on an **unborn** `HEAD`, where `pull_into_void()` tests the
//!     `OPT_PASSTHRU` slot for non-NULL rather than for the positive spelling —
//!     so `--no-verify-signatures` arms the check there too. That asymmetry is
//!     git's, and a port that "cleaned it up" would diverge.
//!   * `%(is-base)`'s two parse-time `die()`s, which are git's own and must wear
//!     git's `fatal:`/128, against `%(is-base:<resolvable>)` and `%(deltabase)`,
//!     which are gaps in this port and must **not** — see `crate::fatal`.
//!   * the ssh backend end to end: sign a tag and a commit, then check that
//!     `verify-tag`, `verify-commit` and `%(signature)` relay `ssh-keygen`'s own
//!     verdict. This block skips when the machine has no `ssh-keygen -Y`.
//!
//! The OpenPGP and x509 backends are the same `verify_gpg_signed_buffer()` with
//! a different program, and reaching them needs a secret key this harness cannot
//! conjure — a throwaway `GNUPGHOME` needs a running `gpg-agent`, whose socket
//! path does not survive a temp directory of realistic length. Their status
//! parsing is covered by the unit tests in `gitsig`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A throwaway root, removed on the way out of each test.
fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-sigv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// Run the port with a fixed identity and a private state directory.
fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@e.x")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "{args:?} failed ({:?}): {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// An upstream with two commits and a clone that stopped after the first, so a
/// pull has something to integrate. Returns `(upstream, clone, home)`.
fn upstream_and_clone(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let up = root.join("up");
    std::fs::create_dir_all(&up).unwrap();
    ok(&up, &home, &["init", "-q", "-b", "main", "."]);
    std::fs::write(up.join("f.txt"), "one\n").unwrap();
    ok(&up, &home, &["add", "f.txt"]);
    ok(&up, &home, &["commit", "-q", "-m", "one"]);

    let url = format!("file://{}", up.display());
    ok(root, &home, &["clone", "-q", &url, "work"]);
    let work = root.join("work");

    std::fs::write(up.join("g.txt"), "two\n").unwrap();
    ok(&up, &home, &["add", "g.txt"]);
    ok(&up, &home, &["commit", "-q", "-m", "two"]);

    (up, work, home)
}

/// `--no-verify-signatures` asks for no check, so the pull must happen.
#[test]
fn pull_no_verify_signatures_still_pulls() {
    let root = scratch("nover");
    let (_up, work, home) = upstream_and_clone(&root);

    let out = run(&work, &home, &["pull", "--no-verify-signatures"]);
    assert!(
        out.status.success(),
        "pull --no-verify-signatures refused: rc={:?} {}",
        out.status.code(),
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("Fast-forward"),
        "expected a fast-forward, got: {}",
        stdout(&out)
    );
    assert!(work.join("g.txt").is_file(), "the fetched commit was not checked out");

    let _ = std::fs::remove_dir_all(&root);
}

/// `--verify-signatures` against an unsigned head: git's exact fatal, at 128,
/// with nothing integrated. Reaching this needs no key — the head carries no
/// signature, so `check_signature()` is never called.
#[test]
fn pull_verify_signatures_refuses_an_unsigned_head() {
    let root = scratch("verunsig");
    let (_up, work, home) = upstream_and_clone(&root);
    let before = stdout(&ok(&work, &home, &["rev-parse", "HEAD"]));

    let out = run(&work, &home, &["pull", "--no-rebase", "--verify-signatures"]);
    assert_eq!(out.status.code(), Some(128), "stderr: {}", stderr(&out));
    let err = stderr(&out);
    let line = err
        .lines()
        .find(|l| l.starts_with("fatal: Commit "))
        .unwrap_or_else(|| panic!("no `fatal: Commit …` line in: {err}"));
    assert!(
        line.ends_with(" does not have a GPG signature."),
        "unexpected wording: {line}"
    );

    assert_eq!(stdout(&ok(&work, &home, &["rev-parse", "HEAD"])), before, "HEAD moved");
    assert!(!work.join("g.txt").exists(), "the merge was performed anyway");

    let _ = std::fs::remove_dir_all(&root);
}

/// `pull_into_void()` (builtin/pull.c:467) tests `opt_verify_signatures` for
/// non-NULL, and `OPT_PASSTHRU` sets it for *both* spellings — so on an unborn
/// `HEAD`, `--no-verify-signatures` verifies. Confirmed against stock 2.55.0.
#[test]
fn pull_into_void_verifies_on_either_spelling() {
    let root = scratch("void");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let up = root.join("up");
    std::fs::create_dir_all(&up).unwrap();
    ok(&up, &home, &["init", "-q", "-b", "main", "."]);
    std::fs::write(up.join("f.txt"), "one\n").unwrap();
    ok(&up, &home, &["add", "f.txt"]);
    ok(&up, &home, &["commit", "-q", "-m", "one"]);

    for spelling in ["--verify-signatures", "--no-verify-signatures"] {
        let work = root.join(spelling.trim_start_matches('-'));
        std::fs::create_dir_all(&work).unwrap();
        ok(&work, &home, &["init", "-q", "-b", "main", "."]);
        ok(
            &work,
            &home,
            &["remote", "add", "origin", &format!("file://{}", up.display())],
        );

        let out = run(&work, &home, &["pull", spelling, "origin", "main"]);
        assert_eq!(
            out.status.code(),
            Some(128),
            "{spelling} into an unborn HEAD should have verified; stderr: {}",
            stderr(&out)
        );
        assert!(
            stderr(&out).contains(" does not have a GPG signature."),
            "{spelling}: {}",
            stderr(&out)
        );
        assert!(!work.join("f.txt").exists(), "{spelling}: pulled anyway");
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// `%(is-base)`'s parse-time rejections are git's own `die()`s and wear git's
/// voice; the compute path and `%(deltabase)` are gaps in this port and must not.
#[test]
fn unported_atoms_do_not_borrow_gits_fatal() {
    let root = scratch("atoms");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    ok(&root, &home, &["init", "-q", "-b", "main", "."]);
    std::fs::write(root.join("f.txt"), "one\n").unwrap();
    ok(&root, &home, &["add", "f.txt"]);
    ok(&root, &home, &["commit", "-q", "-m", "one"]);

    // Parse-time rejections, which are git's own `die()`s: a missing operand, an
    // operand that will not peel, and an argument on an atom that takes none.
    for (format, want) in [
        ("%(is-base)", "fatal: expected format: %(is-base:<committish>)"),
        ("%(is-base:nosuchrev)", "fatal: failed to find 'nosuchrev'"),
        ("%(deltabase:x)", "fatal: %(deltabase) does not take arguments"),
    ] {
        let out = run(&root, &home, &["for-each-ref", &format!("--format={format}")]);
        assert_eq!(out.status.code(), Some(128), "{format}: {}", stderr(&out));
        assert_eq!(stderr(&out).trim_end(), want, "{format}");
    }

    // Not git's failures. `fatal:` at 128 would claim they were.
    for format in ["%(is-base:HEAD)", "%(deltabase)"] {
        let out = run(&root, &home, &["for-each-ref", &format!("--format={format}")]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(1), "{format}: {err}");
        assert!(
            err.starts_with("zvcs: for-each-ref: "),
            "{format} did not speak in the port's voice: {err}"
        );
        assert!(!err.contains("fatal:"), "{format} borrowed git's voice: {err}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// Whether this machine's `ssh-keygen` has the `-Y` signature subcommands
/// (openssh 8.2p1+). Without them the block below has nothing to drive.
fn ssh_signing_available() -> bool {
    let out = Command::new("ssh-keygen").args(["-Y", "check-novalidate", "-h"]).output();
    match out {
        // `-h` is not a real flag; what matters is that `-Y` was understood at
        // all, which an older ssh-keygen reports as an unknown option instead.
        Ok(o) => !String::from_utf8_lossy(&o.stderr).contains("unknown option"),
        Err(_) => false,
    }
}

/// git's line folding for an extra commit header: the first line follows the
/// name, every continuation line is prefixed with one space. This is the shape
/// `gitsig::split_signed` de-folds, so building the object by hand here checks
/// the reader against bytes the *signer* produced rather than against the port's
/// own encoder.
fn folded_gpgsig(signature: &str) -> String {
    let mut lines = signature.trim_end_matches('\n').split('\n');
    let mut out = format!("gpgsig {}\n", lines.next().unwrap_or(""));
    for line in lines {
        out.push(' ');
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// `ssh-keygen -Y sign -n git -f <key> <file>`, returning the `SSH SIGNATURE`
/// block it leaves in `<file>.sig`.
fn ssh_sign(key: &Path, payload: &[u8], scratch: &Path) -> String {
    std::fs::write(scratch, payload).unwrap();
    // ssh-keygen prompts before replacing an existing `.sig` and, with no tty,
    // keeps the old one — which would silently sign the second payload with the
    // first payload's signature.
    let mut sig = scratch.as_os_str().to_owned();
    sig.push(".sig");
    let sig = PathBuf::from(sig);
    let _ = std::fs::remove_file(&sig);

    let out = Command::new("ssh-keygen")
        .args(["-q", "-Y", "sign", "-n", "git", "-f"])
        .arg(key)
        .arg(scratch)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ssh-keygen -Y sign failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(&sig).unwrap()
}

/// The ssh backend, end to end: `ssh-keygen -Y verify`'s own verdict is what
/// `verify-tag`, `verify-commit` and `%(signature)` relay.
#[test]
fn ssh_signatures_verify_through_every_reader() {
    if !ssh_signing_available() {
        eprintln!("skipping: ssh-keygen has no -Y subcommands");
        return;
    }

    let root = scratch("ssh");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let key = root.join("id");
    let generated = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "signer@example.com", "-f"])
        .arg(&key)
        .status();
    if !generated.map(|s| s.success()).unwrap_or(false) {
        eprintln!("skipping: ssh-keygen could not generate a key");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }
    let pubkey = std::fs::read_to_string(root.join("id.pub")).unwrap();
    let allowed = root.join("allowed_signers");
    std::fs::write(&allowed, format!("signer@example.com {pubkey}")).unwrap();

    let sshcfg: Vec<String> = vec![
        "-c".into(),
        "gpg.format=ssh".into(),
        "-c".into(),
        format!("user.signingkey={}", key.display()),
        "-c".into(),
        format!("gpg.ssh.allowedSignersFile={}", allowed.display()),
    ];
    let with_cfg = |args: &[&str]| -> Vec<String> {
        sshcfg.iter().cloned().chain(args.iter().map(|a| a.to_string())).collect()
    };
    let call = |args: Vec<String>| -> Output {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run(&root, &home, &refs)
    };

    ok(&root, &home, &["init", "-q", "-b", "main", "."]);
    std::fs::write(root.join("f.txt"), "one\n").unwrap();
    ok(&root, &home, &["add", "f.txt"]);
    ok(&root, &home, &["commit", "-q", "-m", "base"]);

    // The objects are signed with `ssh-keygen` directly: `git commit -S` under
    // `gpg.format=ssh` and `git tag -s` are both unported here, and signing is
    // not what this test is about. Building them from the signer's own bytes
    // also keeps the reader honest — nothing round-trips through the port's
    // encoder on the way in.
    let scratch_sig = root.join("payload");

    // A commit: its own unsigned object is the payload, and the armored
    // signature goes back in as a folded `gpgsig` header.
    let payload = ok(&root, &home, &["cat-file", "commit", "HEAD"]).stdout;
    let sig = ssh_sign(&key, &payload, &scratch_sig);
    let text = String::from_utf8(payload).unwrap();
    let (headers, body) = text.split_once("\n\n").expect("a header block");
    // `{headers}\n` + the folded header (which ends in its own newline) + the
    // blank line that ends the header block + the body.
    let object = format!("{headers}\n{}\n{body}", folded_gpgsig(&sig));
    std::fs::write(root.join("commit.obj"), &object).unwrap();
    let id = stdout(&ok(&root, &home, &["hash-object", "-w", "-t", "commit", "commit.obj"]));
    ok(&root, &home, &["update-ref", "refs/heads/main", id.trim()]);

    // A tag: the signature is appended to the body rather than folded into a
    // header, and the payload is everything before the marker line.
    let tag_payload = format!(
        "object {}\ntype commit\ntag sshtag\ntagger A <a@e.x> 1700000000 +0000\n\nssh tag\n",
        id.trim()
    );
    let tag_sig = ssh_sign(&key, tag_payload.as_bytes(), &scratch_sig);
    std::fs::write(root.join("tag.obj"), format!("{tag_payload}{tag_sig}")).unwrap();
    let tag_id = stdout(&ok(&root, &home, &["hash-object", "-w", "-t", "tag", "tag.obj"]));
    ok(&root, &home, &["update-ref", "refs/tags/sshtag", tag_id.trim()]);

    // `ssh-keygen -Y verify`'s first line, verbatim on stderr.
    let expected = "Good \"git\" signature for signer@example.com with ED25519 key SHA256:";

    let vt = call(with_cfg(&["verify-tag", "sshtag"]));
    assert!(vt.status.success(), "verify-tag rc={:?} {}", vt.status.code(), stderr(&vt));
    assert!(stderr(&vt).starts_with(expected), "verify-tag said: {}", stderr(&vt));

    let vc = call(with_cfg(&["verify-commit", "HEAD"]));
    assert!(vc.status.success(), "verify-commit rc={:?} {}", vc.status.code(), stderr(&vc));
    assert!(stderr(&vc).starts_with(expected), "verify-commit said: {}", stderr(&vc));

    // `-v` writes the signed payload — the commit object without its `gpgsig`
    // block — to stdout before the checker's report.
    let vv = call(with_cfg(&["verify-commit", "-v", "HEAD"]));
    assert!(stdout(&vv).starts_with("tree "), "-v payload missing: {}", stdout(&vv));
    assert!(!stdout(&vv).contains("gpgsig"), "-v printed the signature header");

    // `%(signature)` is `sigc->output`: the same report, on stdout this time.
    let bare = call(with_cfg(&[
        "for-each-ref",
        "--format=%(signature)",
        "refs/heads/main",
    ]));
    assert!(bare.status.success(), "%(signature) rc={:?} {}", bare.status.code(), stderr(&bare));
    assert!(stdout(&bare).starts_with(expected), "%(signature) said: {}", stdout(&bare));

    // The derived options come off the same check.
    let graded = call(with_cfg(&[
        "for-each-ref",
        "--format=%(signature:grade) %(signature:trustlevel) %(signature:signer)",
        "refs/heads/main",
    ]));
    assert_eq!(
        stdout(&graded).trim_end(),
        "G fully signer@example.com",
        "stderr: {}",
        stderr(&graded)
    );

    // Without `gpg.ssh.allowedSignersFile` git checks nothing and says so, and
    // the verification fails rather than passing by default.
    let unconfigured = run(&root, &home, &["-c", "gpg.format=ssh", "verify-tag", "sshtag"]);
    assert_eq!(unconfigured.status.code(), Some(1));
    assert_eq!(
        stderr(&unconfigured).trim_end(),
        "error: gpg.ssh.allowedSignersFile needs to be configured and exist \
         for ssh signature verification"
    );

    let _ = std::fs::remove_dir_all(&root);
}
