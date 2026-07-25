//! `git push` to a local `file://` remote, served by this binary's `receive-pack`.
//! Exercises the receiving half end to end: command list → pack ingest → ref
//! compare-and-swap → `report-status`. The transport spawns `git-receive-pack`, so
//! the test puts a symlink to the binary under test first on PATH.
//!
//! Unix-only (uses a symlink); skipped elsewhere.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, bindir: &Path, args: &[&str]) -> Output {
    let path = format!("{}:{}", bindir.display(), std::env::var("PATH").unwrap_or_default());
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("PATH", path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary")
}

#[test]
fn push_to_local_bare_remote() {
    let root = std::env::temp_dir().join(format!("zvcs-localpush-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let bindir = root.join("bin");
    let work = root.join("work");
    let bare = root.join("remote.git");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bindir).unwrap();
    std::fs::create_dir_all(&work).unwrap();

    // The transport spawns `git-receive-pack`/`git-upload-pack`; point both (and
    // `git`) at the binary under test so the push is served by our code.
    for name in ["git", "git-receive-pack", "git-upload-pack"] {
        std::os::unix::fs::symlink(BIN, bindir.join(name)).unwrap();
    }

    // `-b main` explicitly: a runner has no `init.defaultBranch`, so the bare
    // repo would init to `master` and every later `main` reference — the
    // clone's branch, a checkout, a refspec — would miss.
    run(&work, &home, &bindir, &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
    run(&work, &home, &bindir, &["init", "-q", "-b", "main", "."]);
    run(&work, &home, &bindir, &["config", "user.email", "t@e.co"]);
    run(&work, &home, &bindir, &["config", "user.name", "t"]);
    std::fs::write(work.join("f"), "one\n").unwrap();
    run(&work, &home, &bindir, &["add", "f"]);
    run(&work, &home, &bindir, &["commit", "-q", "-m", "c0"]);
    std::fs::write(work.join("f"), "two\n").unwrap();
    run(&work, &home, &bindir, &["add", "f"]);
    run(&work, &home, &bindir, &["commit", "-q", "-m", "c1"]);
    run(&work, &home, &bindir, &["remote", "add", "origin", bare.to_str().unwrap()]);

    let out = run(&work, &home, &bindir, &["push", "origin", "main"]);
    assert!(
        out.status.success(),
        "local push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The ref and its objects must have landed: remote main == local HEAD.
    let local = String::from_utf8_lossy(&run(&work, &home, &bindir, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let remote = String::from_utf8_lossy(
        &run(&work, &home, &bindir, &["--git-dir", bare.to_str().unwrap(), "rev-parse", "main"]).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(local, remote, "pushed ref did not land on the remote");

    // The remote can read the pushed history (objects arrived, not just the ref).
    let log = run(&work, &home, &bindir, &["--git-dir", bare.to_str().unwrap(), "log", "--oneline"]);
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).lines().count(),
        2,
        "remote history should have both commits"
    );

    // A second push (fast-forward update, not a create) also works.
    std::fs::write(work.join("f"), "three\n").unwrap();
    run(&work, &home, &bindir, &["add", "f"]);
    run(&work, &home, &bindir, &["commit", "-q", "-m", "c2"]);
    assert!(run(&work, &home, &bindir, &["push", "origin", "main"]).status.success());

    let _ = std::fs::remove_dir_all(&root);
}
