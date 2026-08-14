//! `git add --renormalize` / `git stage --renormalize` — re-running the content
//! filters over already-tracked files.
//!
//! The flag exists for exactly one situation: a repository that gains
//! `text=auto` (or `core.autocrlf`) after files with CRLF are already committed.
//! A plain `add` deliberately leaves those alone — `crlf_to_git()` consults
//! `has_crlf_in_index()` so a mixed-ending file is not rewritten behind the user's
//! back — and `--renormalize` (`CONV_EOL_RENORMALIZE`) is what switches that guard
//! off. A port that accepts the flag and stages the same blob has done nothing.
//!
//! `git stage` is `cmd_add()` under another name, so both spellings are exercised
//! here: they must agree with each other and with the blob ids stock git 2.55.0
//! writes for this fixture (`4e349b5` verbatim CRLF, `814f4a4` normalized).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const DATE: &str = "1136214245 +0000";

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        // The whole point of these cases is which conversion the *repository*
        // configures, so a developer's global `core.autocrlf` must not reach them.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // zvcs keeps its own cache under HOME, so it points outside the repo —
        // otherwise the cache files show up as untracked noise in `status`.
        .env("HOME", home_for(dir))
        .env("ZVCS_HOME", home_for(dir))
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .output()
        .unwrap()
}

/// The throwaway HOME beside the repository.
fn home_for(dir: &Path) -> PathBuf {
    let home = dir.parent().expect("repo has a parent").join("home");
    let _ = std::fs::create_dir_all(&home);
    home
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

/// The blob id the index currently records for `path`.
fn indexed(dir: &Path, path: &str) -> String {
    git_ok(dir, &["rev-parse", &format!(":{path}")])
}

/// A repository holding a committed CRLF file, with `* text=auto` added only
/// afterwards — the state `--renormalize` exists to clean up.
fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-renorm-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    git_ok(&repo, &["config", "user.email", "author@example.com"]);
    git_ok(&repo, &["config", "user.name", "A U Thor"]);
    std::fs::write(repo.join("crlf.txt"), "one\r\ntwo\r\n").unwrap();
    git_ok(&repo, &["add", "crlf.txt"]);
    git_ok(&repo, &["commit", "-q", "-m", "base"]);
    std::fs::write(repo.join(".gitattributes"), "* text=auto\n").unwrap();
    git_ok(&repo, &["add", ".gitattributes"]);
    git_ok(&repo, &["commit", "-q", "-m", "attrs"]);
    repo
}

/// Stock git 2.55.0 hashes the verbatim CRLF content to this.
const CRLF_BLOB: &str = "4e349b596c5c9d38a82829fafbaf52281c21e319";
/// …and the same content with LF endings to this.
const LF_BLOB: &str = "814f4a422927b82f5f8a43f8fab6d3839e3983f2";

#[test]
fn a_plain_add_keeps_the_indexed_crlf() {
    let repo = fixture("plain");
    assert_eq!(indexed(&repo, "crlf.txt"), CRLF_BLOB);
    // `text=auto` alone must NOT rewrite it: the blob in the index has CRLF, which
    // is git's signal to leave the file's endings alone.
    let out = git(&repo, &["add", "."]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(indexed(&repo, "crlf.txt"), CRLF_BLOB, "a plain add renormalized");
}

#[test]
fn add_renormalize_restages_the_normalized_blob() {
    let repo = fixture("add");
    let out = git(&repo, &["add", "--renormalize", "."]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(indexed(&repo, "crlf.txt"), LF_BLOB, "--renormalize staged the old bytes");
    // The staged change is visible, and the worktree file keeps its CRLF.
    assert_eq!(git_ok(&repo, &["status", "--short"]), "M  crlf.txt");
    assert_eq!(std::fs::read_to_string(repo.join("crlf.txt")).unwrap(), "one\r\ntwo\r\n");
}

#[test]
fn stage_renormalize_agrees_with_add_renormalize() {
    // Same command under two names in git (`cmd_stage()` *is* `cmd_add()`), so it
    // must neither refuse nor produce a different blob.
    let repo = fixture("stage");
    let out = git(&repo, &["stage", "--renormalize", "."]);
    assert!(
        out.status.success(),
        "stage --renormalize refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(indexed(&repo, "crlf.txt"), LF_BLOB);
    assert_eq!(git_ok(&repo, &["status", "--short"]), "M  crlf.txt");
}

#[test]
fn stage_applies_the_content_filters_like_add() {
    // A brand-new CRLF file under `text=auto` has no indexed version to consult,
    // so both spellings normalize it on the way in.
    for verb in ["add", "stage"] {
        let repo = fixture(&format!("newfile-{verb}"));
        std::fs::write(repo.join("new.txt"), "a\r\nb\r\n").unwrap();
        let out = git(&repo, &[verb, "new.txt"]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let stored = git_ok(&repo, &["cat-file", "-p", &indexed(&repo, "new.txt")]);
        assert_eq!(stored, "a\nb", "{verb} stored the verbatim CRLF bytes");
        // git warns once, from `index_path()`; the scan that precedes it writes no
        // object and so must stay quiet.
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            stderr.matches("CRLF will be replaced by LF").count(),
            1,
            "{verb} stderr: {stderr}"
        );
    }
}

#[test]
fn dry_run_neither_warns_nor_stages() {
    let repo = fixture("dry");
    std::fs::write(repo.join("new.txt"), "a\r\nb\r\n").unwrap();
    for verb in ["add", "stage"] {
        let out = git(&repo, &[verb, "-n", "new.txt"]);
        assert!(out.status.success());
        // `hash_flags` is 0 under `--dry-run`, so the round-trip check is off.
        assert_eq!(String::from_utf8_lossy(&out.stderr), "", "{verb} warned on a dry run");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "add 'new.txt'\n");
    }
    assert!(git(&repo, &["rev-parse", ":new.txt"]).status.code() != Some(0));
}
