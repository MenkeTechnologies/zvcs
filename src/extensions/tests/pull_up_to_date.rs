//! `git pull --rebase` says the right up-to-date sentence.
//!
//! git distinguishes two states that look identical from the outside:
//!
//!   * upstream == HEAD — nothing was fetched that we lack, the integration step
//!     never starts, and pull itself reports `Already up to date.`
//!   * local ahead of upstream — the rebase DOES run, finds nothing to replay,
//!     and reports `Current branch <branch> is up to date.`
//!
//! zvcs printed the second sentence for both, because pull delegated to rebase
//! unconditionally. The two lines are what a human (and every script grepping
//! pull output) reads to tell "I already had it" from "I have unpushed work".
//!
//! Hermetic by construction: the fixture, the clone and the pulls all run
//! through the zvcs binary under test, over its own local transport, with a PATH
//! shim that points `git-upload-pack` at that same binary. No external git is
//! involved in any step.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A PATH prefix in which `git`, `git-upload-pack` and `git-receive-pack` all
/// resolve to the binary under test — what the local transport spawns to serve
/// a fetch.
fn shim(root: &Path) -> PathBuf {
    let dir = root.join("shim");
    std::fs::create_dir_all(&dir).expect("mkdir shim");
    for name in ["git", "git-upload-pack", "git-receive-pack"] {
        let link = dir.join(name);
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(BIN, &link).expect("symlink shim");
    }
    dir
}

fn run(dir: &Path, shim_dir: &Path, args: &[&str]) -> Output {
    let path = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("PATH", path)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .output()
        .expect("run zvcs git")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// An upstream repo with one commit, and a clone of it. Returns
/// `(root, shim, upstream, work)`.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-pullutd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir root");
    let shim_dir = shim(&root);

    let up = root.join("up");
    std::fs::create_dir_all(&up).expect("mkdir up");
    assert!(run(&up, &shim_dir, &["init", "-q", "-b", "main"]).status.success(), "init");
    assert!(
        run(&up, &shim_dir, &["commit", "-q", "--allow-empty", "-m", "c1"]).status.success(),
        "first commit"
    );

    let clone = run(&root, &shim_dir, &["clone", "-q", "up", "work"]);
    assert!(clone.status.success(), "clone over the local transport: {}", String::from_utf8_lossy(&clone.stderr));
    (root.clone(), shim_dir, up, root.join("work"))
}

#[test]
fn in_sync_reports_already_up_to_date() {
    let (_root, shim_dir, _up, work) = fixture("sync");

    let out = run(&work, &shim_dir, &["pull", "--rebase"]);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        stdout(&out).contains("Already up to date."),
        "upstream == HEAD is pull's own up-to-date line, got: {:?}",
        stdout(&out)
    );
    assert!(
        !stdout(&out).contains("Current branch"),
        "the rebase must not run at all here: {:?}",
        stdout(&out)
    );
}

#[test]
fn local_ahead_reports_the_rebase_line() {
    let (_root, shim_dir, _up, work) = fixture("ahead");
    assert!(
        run(&work, &shim_dir, &["commit", "-q", "--allow-empty", "-m", "local1"]).status.success(),
        "local commit"
    );

    let out = run(&work, &shim_dir, &["pull", "--rebase"]);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        stdout(&out).contains("Current branch main is up to date."),
        "with local work the rebase runs and reports its own line, got: {:?}",
        stdout(&out)
    );
}

#[test]
fn upstream_ahead_actually_rebases() {
    let (_root, shim_dir, up, work) = fixture("behind");
    assert!(
        run(&up, &shim_dir, &["commit", "-q", "--allow-empty", "-m", "up1"]).status.success(),
        "upstream commit"
    );

    let out = run(&work, &shim_dir, &["pull", "--rebase"]);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let combined = format!("{}{}", stdout(&out), String::from_utf8_lossy(&out.stderr));
    assert!(
        !combined.contains("Already up to date."),
        "there was real work to integrate: {combined:?}"
    );
    // The fetched commit is now in this branch's history.
    let log = run(&work, &shim_dir, &["log", "--oneline"]);
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("up1"),
        "the upstream commit landed: {:?}",
        String::from_utf8_lossy(&log.stdout)
    );
}
