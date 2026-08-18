//! One-level ref names in a `fast-import` stream — `commit main`, `reset side`.
//!
//! `fast-import.c:617` validates a branch name with
//! `check_refname_format(name, REFNAME_ALLOW_ONELEVEL)`, so a stream may name a
//! ref with a single component and git accepts it, writing `$GIT_DIR/<name>`
//! rather than anything under `refs/`. A *deletion* is validated differently:
//! `ref_transaction_update()` (`refs.c:1199-1205`) switches to
//! `refname_is_safe()` when the new id is null, which outside `refs/` demands an
//! all-uppercase name — so the same `main` that a stream may create cannot be
//! deleted again, and git says so with `error:` while still exiting 0.
//!
//! Every expectation below was measured from stock git 2.55.0
//! (`/opt/homebrew/bin/git`) in a hermetic environment before it was written
//! down. The measurements, verbatim:
//!
//! ```text
//! $ printf 'blob\nmark :1\ndata 5\nhello\ncommit main\n…' | git fast-import --done
//! $ cat .git/main
//! 4c7afd1bbf23d9379e1997b7262bc8bea0254aed        # no .git/logs/main
//! $ …'commit main' + checkpoint + 'reset main' + 'from 0{40}' | git fast-import --done
//! error: refusing to update ref with bad name 'main'      # rc 0, .git/main intact
//! $ …'commit bad~name'… | git fast-import --done
//! fatal: branch name doesn't conform to Git standards: bad~name   # rc 128
//! $ … an unrelated second root onto an existing one-level `main` …
//! warning: not updating main (new tip 990430429… does not contain 70487d976…)  # rc 1
//! ```

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A fresh repository under a unique temp dir, with its own `ZVCS_HOME`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fi-onelevel-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    assert!(
        Command::new(BIN)
            .args(["init", "-q", "-b", "main"])
            .current_dir(&repo)
            .env("ZVCS_HOME", root.join("home"))
            .status()
            .unwrap()
            .success(),
        "git init failed"
    );
    (root, repo)
}

/// Run `git fast-import --quiet --done` with `stream` on stdin.
fn fast_import(repo: &Path, home: &Path, stream: &str) -> (String, i32) {
    let mut child = Command::new(BIN)
        .args(["fast-import", "--quiet", "--done"])
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .env("GIT_COMMITTER_NAME", "zvcs test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// A one-commit stream landing on `ref`, with `body` as the blob so two calls
/// can produce two unrelated roots.
fn stream(reference: &str, body: &str, when: u64) -> String {
    format!(
        "blob\nmark :1\ndata {}\n{body}\n\
         commit {reference}\nmark :2\n\
         committer zvcs test <test@example.invalid> {when} +0000\n\
         data 2\nc1\nM 100644 :1 f\n\n\
         done\n",
        body.len()
    )
}

/// The text of a loose ref file, trimmed, or `None` when there is no such file.
fn loose(repo: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(repo.join(".git").join(name))
        .ok()
        .map(|s| s.trim().to_string())
}

/// `commit main` — one level, lowercase — is a name git accepts and writes to
/// `$GIT_DIR/main`, with no reflog, because `log_ref_setup()` auto-creates one
/// only for `HEAD` and `refs/{heads,remotes,notes}/`.
#[test]
fn a_one_level_branch_name_is_accepted_and_written_beside_head() {
    let (root, repo) = fixture("commit");
    let home = root.join("home");
    let (err, code) = fast_import(&repo, &home, &stream("main", "hello", 1700000000));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(err, "", "a one-level name is not an error");

    let id = loose(&repo, "main").expect("$GIT_DIR/main written");
    assert_eq!(id.len(), 40, "an object id, not a symref: {id}");
    assert!(
        loose(&repo, "refs/heads/main").is_none(),
        "the name is taken literally, not qualified into refs/heads/"
    );
    assert!(
        !repo.join(".git").join("logs").join("main").exists(),
        "a one-level ref gets no reflog"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `reset <one-level>` reaches the same `new_branch()` check as `commit`, so it
/// is accepted on the same terms.
#[test]
fn reset_accepts_a_one_level_name_too() {
    let (root, repo) = fixture("reset");
    let home = root.join("home");
    let s = format!(
        "blob\nmark :1\ndata 5\nhello\n\
         commit refs/heads/main\nmark :2\n\
         committer zvcs test <test@example.invalid> 1700000000 +0000\n\
         data 2\nc1\nM 100644 :1 f\n\n\
         reset side\nfrom :2\n\n\
         done\n"
    );
    let (err, code) = fast_import(&repo, &home, &s);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        loose(&repo, "side"),
        loose(&repo, "refs/heads/main"),
        "`reset side` points $GIT_DIR/side at the same commit"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The fast-forward guard reads the one-level ref back, so a second import of an
/// unrelated root is declined with git's warning and exit 1 — which only works
/// if the ref is read from `$GIT_DIR/<name>` the way `read_ref()` does, and not
/// through a partial-name search that would answer for `refs/heads/main`.
#[test]
fn the_fast_forward_guard_reads_a_one_level_ref_back() {
    let (root, repo) = fixture("guard");
    let home = root.join("home");
    let (err, code) = fast_import(&repo, &home, &stream("main", "aa", 1700000000));
    assert_eq!(code, 0, "stderr: {err}");
    let first = loose(&repo, "main").expect("$GIT_DIR/main written");

    let (err, code) = fast_import(&repo, &home, &stream("main", "bb", 1700000001));
    assert_eq!(code, 1, "a declined update exits 1; stderr: {err}");
    assert!(
        err.starts_with("warning: not updating main (new tip "),
        "unexpected stderr: {err}"
    );
    assert_eq!(loose(&repo, "main").as_deref(), Some(first.as_str()), "ref left alone");

    // `--force` is the escape hatch, and it does move the same ref.
    let mut child = Command::new(BIN)
        .args(["fast-import", "--quiet", "--force", "--done"])
        .current_dir(&repo)
        .env("ZVCS_HOME", &home)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream("main", "bb", 1700000001).as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_ne!(loose(&repo, "main").as_deref(), Some(first.as_str()), "--force moved it");
    let _ = std::fs::remove_dir_all(&root);
}

/// Deleting a lowercase one-level ref is refused by `refname_is_safe()`, and git
/// reports that without failing the run: `update_branch()` ignores what
/// `delete_ref()` returned, so the exit code stays 0 and the ref stays put.
#[test]
fn deleting_a_one_level_ref_is_refused_but_does_not_fail_the_run() {
    let (root, repo) = fixture("delete-lower");
    let home = root.join("home");
    let s = "blob\nmark :1\ndata 3\naa\n\
             commit main\nmark :2\n\
             committer zvcs test <test@example.invalid> 1700000000 +0000\n\
             data 2\nc1\nM 100644 :1 f\n\n\
             checkpoint\n\n\
             reset main\nfrom 0000000000000000000000000000000000000000\n\n\
             done\n";
    let (err, code) = fast_import(&repo, &home, s);
    assert_eq!(code, 0, "the refusal is not a failure; stderr: {err}");
    assert_eq!(err.trim_end(), "error: refusing to update ref with bad name 'main'");
    assert!(loose(&repo, "main").is_some(), "the ref survives the refused deletion");
    let _ = std::fs::remove_dir_all(&root);
}

/// The same deletion of an all-uppercase one-level ref *is* safe by
/// `refname_is_safe()`'s rule, and goes through — so the refusal above is that
/// rule firing, not one-level deletion being unimplemented.
#[test]
fn deleting_an_uppercase_one_level_ref_goes_through() {
    let (root, repo) = fixture("delete-upper");
    let home = root.join("home");
    let s = "blob\nmark :1\ndata 3\naa\n\
             commit TOPIC\nmark :2\n\
             committer zvcs test <test@example.invalid> 1700000000 +0000\n\
             data 2\nc1\nM 100644 :1 f\n\n\
             checkpoint\n\n\
             reset TOPIC\nfrom 0000000000000000000000000000000000000000\n\n\
             done\n";
    let (err, code) = fast_import(&repo, &home, s);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(err, "", "no refusal for an uppercase name");
    assert!(loose(&repo, "TOPIC").is_none(), "the ref was deleted");
    let _ = std::fs::remove_dir_all(&root);
}

/// Accepting one-level names does not accept malformed ones: every name git's
/// `check_refname_component()` rejects is still fatal, with git's own message.
#[test]
fn malformed_names_are_still_fatal() {
    for (tag, name) in [
        ("tilde", "bad~name"),
        ("dotdot", "a..b"),
        ("leading-dot", ".hidden"),
        ("dotlock", "foo.lock"),
        ("at", "@"),
        ("space", "two words"),
        ("colon", "a:b"),
        ("star", "a*b"),
        ("reflog", "a@{0}"),
        ("trailing-dot", "abc."),
        ("empty-component", "a//b"),
        ("trailing-slash", "abc/"),
    ] {
        let (root, repo) = fixture(tag);
        let home = root.join("home");
        let (err, code) = fast_import(&repo, &home, &stream(name, "hello", 1700000000));
        assert_eq!(code, 128, "{name} should be fatal; stderr: {err}");
        assert_eq!(
            err.trim_end(),
            format!("fatal: branch name doesn't conform to Git standards: {name}"),
            "unexpected message for {name}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// `commit HEAD` names a one-level ref that happens to be symbolic, and
/// `update_branch()` updates it through `ref_transaction_update()` *without*
/// `REF_NO_DEREF` — so the branch HEAD points at is what moves, HEAD stays
/// symbolic, and both reflogs get an entry. Measured from stock 2.55.0:
/// `.git/HEAD` still `ref: refs/heads/main`, `.git/refs/heads/main` at the new
/// commit, `logs/HEAD` and `logs/refs/heads/main` both written.
#[test]
fn commit_head_moves_the_branch_head_points_at() {
    let (root, repo) = fixture("head");
    let home = root.join("home");
    let (err, code) = fast_import(&repo, &home, &stream("HEAD", "aa", 1700000000));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(loose(&repo, "HEAD").as_deref(), Some("ref: refs/heads/main"), "HEAD stays symbolic");
    let id = loose(&repo, "refs/heads/main").expect("the branch HEAD names was written");
    assert_eq!(id.len(), 40, "an object id: {id}");
    for log in ["logs/HEAD", "logs/refs/heads/main"] {
        let text = std::fs::read_to_string(repo.join(".git").join(log)).unwrap_or_default();
        assert!(text.contains(&id), "{log} records the update: {text:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A name with a slash that is not under `refs/` is one git also takes
/// literally, and the two-component form must keep working while one-level names
/// are being accepted.
#[test]
fn a_two_component_name_outside_refs_is_taken_literally() {
    let (root, repo) = fixture("slashed");
    let home = root.join("home");
    let (err, code) = fast_import(&repo, &home, &stream("foo/bar", "hello", 1700000000));
    assert_eq!(code, 0, "stderr: {err}");
    assert!(loose(&repo, "foo/bar").is_some(), "$GIT_DIR/foo/bar written");
    assert!(
        loose(&repo, "refs/heads/foo/bar").is_none(),
        "not qualified into refs/heads/"
    );
    let _ = std::fs::remove_dir_all(&root);
}
