//! `git zsigs` is a gate: it exits non-zero when any checked commit is not a
//! good signature, which is what makes it usable in a pre-push hook or CI. A
//! gate has exactly two ways to be worthless — passing what it should stop, and
//! stopping what it should pass — and only the second one is loud.
//!
//! The verb is a fleet verb, so verification runs for many repositories inside
//! one process, where git itself resolves gpg config once for the single
//! repository its process serves. The config that decides whether an ssh
//! signature can be verified at all (`gpg.ssh.allowedSignersFile`) is usually
//! per-repository, so a fleet run that read it from the invoking directory
//! reported every signed commit as unsigned (`N`) and failed the gate: measured
//! before the fix, `git zsigs` inside the repo flagged 1 of 2 commits, and the
//! same selection from the parent directory flagged 2 of 2.
//!
//! These tests use ssh signing (`gpg.format = ssh`) because it needs only
//! `ssh-keygen`, which git itself shells out to; a runner without `-Y` support
//! skips rather than reporting a false pass.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("ZVCS_HOME", home)
        // The developer's own signing config must not decide this test.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

fn git(home: &Path, cwd: &Path, args: &[&str]) {
    let out = run(home, cwd, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

/// stdout + stderr + exit code — the gate speaks through all three.
fn zsigs(home: &Path, cwd: &Path, args: &[&str]) -> (String, i32) {
    let out = run(home, cwd, args);
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

/// An ssh keypair plus the allowed-signers file naming it, in `dir`.
fn keypair(dir: &Path) -> (PathBuf, PathBuf) {
    let key = dir.join("id");
    let ok = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "signer@example", "-f"])
        .arg(&key)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "ssh-keygen could not create a test key");
    let pubkey = std::fs::read_to_string(dir.join("id.pub")).unwrap();
    let allowed = dir.join("allowed_signers");
    std::fs::write(&allowed, format!("signer@example {pubkey}")).unwrap();
    (key, allowed)
}

/// Can this machine sign AND verify with ssh-keygen? (`-Y` arrived in OpenSSH
/// 8.2.) Checked through the binary's own `%G?`, since that is the reading
/// `zsigs` must agree with — never by assuming the tool is there.
fn signing_works(home: &Path, repo: &Path) -> bool {
    let out = run(home, repo, &["log", "--format=%G?", "-1"]);
    String::from_utf8_lossy(&out.stdout).trim() == "G"
}

/// A repo with an unsigned commit under a signed one, configured to verify with
/// its OWN config — the shape the fleet path used to be blind to.
fn repo_with_signed_head(home: &Path, root: &Path, name: &str, allowed: &Path, key: &Path) -> PathBuf {
    let r = root.join(name);
    std::fs::create_dir_all(&r).unwrap();
    git(home, &r, &["init", "-q", "-b", "main"]);
    for (k, v) in [
        ("user.email", "signer@example"),
        ("user.name", "Signer"),
        ("gpg.format", "ssh"),
        ("user.signingkey", &format!("{}.pub", key.display())),
        ("gpg.ssh.allowedSignersFile", &allowed.display().to_string()),
    ] {
        git(home, &r, &["config", k, v]);
    }
    std::fs::write(r.join("f.txt"), b"1\n").unwrap();
    git(home, &r, &["add", "f.txt"]);
    git(home, &r, &["commit", "-q", "-m", "unsigned"]);
    std::fs::write(r.join("f.txt"), b"2\n").unwrap();
    git(home, &r, &["commit", "-qS", "-am", "signed"]);
    r
}

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zsigs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let (key, allowed) = keypair(&root);
    (root, home, key, allowed)
}

#[test]
fn gate_passes_a_verifiable_signature_and_fails_an_unsigned_commit() {
    let (root, home, key, allowed) = fixture("verdicts");
    let repo = repo_with_signed_head(&home, &root, "signed", &allowed, &key);
    run(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);
    if !signing_works(&home, &repo) {
        eprintln!("skipping: this runner cannot sign/verify with ssh-keygen -Y");
        return;
    }

    // HEAD only: signed and verifiable → the gate opens, and says nothing about
    // a commit it cleared.
    let (out, rc) = zsigs(&home, &root, &["zsigs"]);
    assert_eq!(rc, 0, "a verifiable signature must pass the gate:\n{out}");
    assert!(!out.contains("signed\n") || !out.contains('N'), "a good signature must not be flagged:\n{out}");
    assert!(out.contains("0 unverified"), "the summary must report nothing unverified:\n{out}");

    // Reaching one commit deeper hits the unsigned parent → the gate closes.
    let (out2, rc2) = zsigs(&home, &root, &["zsigs", "-n", "2"]);
    assert_eq!(rc2, 1, "an unsigned commit in range must fail the gate:\n{out2}");
    assert!(out2.contains("unsigned"), "the offending commit must be named:\n{out2}");
    assert!(out2.contains("1 unverified"), "exactly the unsigned commit is unverified:\n{out2}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn verdict_does_not_depend_on_the_invoking_directory() {
    let (root, home, key, allowed) = fixture("cwd");
    let repo = repo_with_signed_head(&home, &root, "signed", &allowed, &key);
    run(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);
    if !signing_works(&home, &repo) {
        eprintln!("skipping: this runner cannot sign/verify with ssh-keygen -Y");
        return;
    }

    // The same selection, judged from inside the repo and from the tree above
    // it. `gpg.ssh.allowedSignersFile` is set in the repo's own config, which is
    // the only place it can be read from once the fleet spans repositories.
    let (inside, rc_in) = zsigs(&home, &repo, &["zsigs", "-n", "2"]);
    let (outside, rc_out) = zsigs(&home, &root, &["zsigs", "-n", "2"]);
    assert_eq!(rc_in, rc_out, "the gate's verdict changed with the caller's cwd:\nin:\n{inside}\nout:\n{outside}");
    assert_eq!(
        inside, outside,
        "the fleet run must report what the in-repo run reports (per-repo signing config was ignored)"
    );
    assert!(inside.contains("1 unverified"), "exactly the unsigned commit is unverified:\n{inside}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn selectors_scope_the_gate_to_the_repos_named() {
    let (root, home, key, allowed) = fixture("scope");
    let clean = repo_with_signed_head(&home, &root, "clean", &allowed, &key);
    // A second repo whose HEAD is unsigned, so the fleet-wide answer differs
    // from the scoped one.
    let dirty = root.join("unsigned");
    std::fs::create_dir_all(&dirty).unwrap();
    git(&home, &dirty, &["init", "-q", "-b", "main"]);
    git(&home, &dirty, &["config", "user.email", "signer@example"]);
    git(&home, &dirty, &["config", "user.name", "Signer"]);
    std::fs::write(dirty.join("g.txt"), b"1\n").unwrap();
    git(&home, &dirty, &["add", "g.txt"]);
    git(&home, &dirty, &["commit", "-q", "-m", "no signature here"]);
    run(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);
    if !signing_works(&home, &clean) {
        eprintln!("skipping: this runner cannot sign/verify with ssh-keygen -Y");
        return;
    }

    // Scoped to the signed repo: passes, and never mentions the other.
    let (scoped, rc) = zsigs(&home, &root, &["zsigs", "--repo", "clean"]);
    assert_eq!(rc, 0, "the signed repo alone must pass:\n{scoped}");
    assert!(!scoped.contains("no signature here"), "a selector must exclude the other repo:\n{scoped}");
    assert!(scoped.contains("across 1"), "exactly one repo must be checked:\n{scoped}");

    // Unscoped: the unsigned repo drags the whole run to a failure — a gate over
    // the fleet is the AND of its repos.
    let (all, rc_all) = zsigs(&home, &root, &["zsigs"]);
    assert_eq!(rc_all, 1, "one unsigned repo must fail the fleet gate:\n{all}");
    assert!(all.contains("no signature here"), "the offending commit must be named:\n{all}");

    let _ = std::fs::remove_dir_all(&root);
}
