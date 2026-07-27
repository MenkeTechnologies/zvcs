//! `git clone`/`fetch` from a local `file://` remote, served by this binary's
//! `upload-pack`. Exercises the serving half end to end: advertisement (with the
//! `symref=HEAD` hint), want/have negotiation, and the streamed pack — for both a
//! full clone and an incremental fetch. Also covers `git --exec-path`.
//!
//! Unix-only (uses a symlink so the transport spawns this binary as
//! `git-upload-pack`); skipped elsewhere.
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

fn stdout(cwd: &Path, home: &Path, bindir: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&run(cwd, home, bindir, args).stdout).trim().to_string()
}

#[test]
fn clone_and_fetch_from_local_remote() {
    let root = std::env::temp_dir().join(format!("zvcs-localclone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let bindir = root.join("bin");
    let src = root.join("src");
    let dst = root.join("dst");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bindir).unwrap();
    std::fs::create_dir_all(&src).unwrap();

    for name in ["git", "git-upload-pack", "git-receive-pack"] {
        std::os::unix::fs::symlink(BIN, bindir.join(name)).unwrap();
    }

    // Source repo with three commits and a tag.
    run(&src, &home, &bindir, &["init", "-q", "-b", "main"]);
    run(&src, &home, &bindir, &["config", "user.email", "t@e.co"]);
    run(&src, &home, &bindir, &["config", "user.name", "t"]);
    for m in ["c0", "c1", "c2"] {
        std::fs::write(src.join("f"), format!("{m}\n")).unwrap();
        run(&src, &home, &bindir, &["add", "f"]);
        run(&src, &home, &bindir, &["commit", "-q", "-m", m]);
    }
    let src_head = stdout(&src, &home, &bindir, &["rev-parse", "HEAD"]);

    // Full clone: history, branch and worktree must match the source.
    let out = run(&root, &home, &bindir, &["clone", "-q", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(out.status.success(), "local clone failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&dst, &home, &bindir, &["rev-parse", "HEAD"]), src_head, "clone HEAD mismatch");
    assert_eq!(stdout(&dst, &home, &bindir, &["log", "--oneline"]).lines().count(), 3);
    assert_eq!(stdout(&dst, &home, &bindir, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    assert!(dst.join("f").exists(), "worktree was not checked out");

    // Incremental fetch, deliberately with NO committer identity configured in the
    // clone target: fetch's remote-tracking reflog must fall back to a synthesized
    // system identity (as git does) rather than erroring. Add a commit upstream,
    // fetch, and confirm the only-new transfer landed the new tip.
    std::fs::write(src.join("f"), "c3\n").unwrap();
    run(&src, &home, &bindir, &["add", "f"]);
    run(&src, &home, &bindir, &["commit", "-q", "-m", "c3"]);
    let src_head2 = stdout(&src, &home, &bindir, &["rev-parse", "HEAD"]);

    let fout = run(&dst, &home, &bindir, &["fetch", "-q", "origin"]);
    assert!(fout.status.success(), "local fetch failed: {}", String::from_utf8_lossy(&fout.stderr));
    assert_eq!(
        stdout(&dst, &home, &bindir, &["rev-parse", "origin/main"]),
        src_head2,
        "fetch did not advance origin/main"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Three shapes that only show up once a remote is actually talked to, all pinned against stock
/// git 2.55.0's behaviour:
///
/// * `refs/remotes/origin/HEAD` is written by `update_remote_refs()` with `refs_update_symref()`, so
///   it is a *symbolic* ref to the tracking branch. Written as a copy of the object id instead, a
///   clone's `origin/HEAD` would stay pinned to the commit it was cloned at forever.
/// * `get_fetch_map()` dies with `couldn't find remote ref <src>` (exit 128) when an exact refspec
///   names a ref the remote does not have — not a refspec-matching summary at the end.
/// * `<repository>` may be a plain path with no `remote.<name>` section behind it, in which case the
///   pull integrates `FETCH_HEAD`; there is no `refs/remotes/<path>/<branch>` to look for.
#[test]
fn remote_head_is_symbolic_and_url_remotes_work() {
    let root = std::env::temp_dir().join(format!("zvcs-remotehead-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let bindir = root.join("bin");
    let src = root.join("src");
    let dst = root.join("dst");
    for d in [&home, &bindir, &src] {
        std::fs::create_dir_all(d).unwrap();
    }
    for name in ["git", "git-upload-pack", "git-receive-pack"] {
        std::os::unix::fs::symlink(BIN, bindir.join(name)).unwrap();
    }

    run(&src, &home, &bindir, &["init", "-q", "-b", "main"]);
    run(&src, &home, &bindir, &["config", "user.email", "t@e.co"]);
    run(&src, &home, &bindir, &["config", "user.name", "t"]);
    for m in ["c0", "c1"] {
        std::fs::write(src.join("f"), format!("{m}\n")).unwrap();
        run(&src, &home, &bindir, &["add", "f"]);
        run(&src, &home, &bindir, &["commit", "-q", "-m", m]);
    }

    let out = run(&root, &home, &bindir, &["clone", "-q", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(out.status.success(), "clone failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        stdout(&dst, &home, &bindir, &["symbolic-ref", "refs/remotes/origin/HEAD"]),
        "refs/remotes/origin/main",
        "clone must write refs/remotes/origin/HEAD as a symref, not as the object it resolved to"
    );

    let missing = run(&dst, &home, &bindir, &["fetch", "origin", "no-such-branch"]);
    assert_eq!(missing.status.code(), Some(128), "a missing exact refspec is a fatal, not a rejection");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("fatal: couldn't find remote ref no-such-branch"),
        "unexpected diagnostic: {}",
        String::from_utf8_lossy(&missing.stderr)
    );

    // Rewind the clone, then pull the upstream back in by *path* — no remote name involved.
    std::fs::write(src.join("f"), "c2\n").unwrap();
    run(&src, &home, &bindir, &["add", "f"]);
    run(&src, &home, &bindir, &["commit", "-q", "-m", "c2"]);
    let src_head = stdout(&src, &home, &bindir, &["rev-parse", "HEAD"]);
    run(&dst, &home, &bindir, &["reset", "-q", "--hard", "HEAD~1"]);
    run(&dst, &home, &bindir, &["config", "user.email", "t@e.co"]);
    run(&dst, &home, &bindir, &["config", "user.name", "t"]);

    let pull = run(&dst, &home, &bindir, &["pull", src.to_str().unwrap(), "main"]);
    assert!(
        pull.status.success(),
        "pull from a URL failed: {}",
        String::from_utf8_lossy(&pull.stderr)
    );
    assert_eq!(
        stdout(&dst, &home, &bindir, &["rev-parse", "HEAD"]),
        src_head,
        "pull <path> <branch> must fast-forward to the fetched tip"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn exec_path_reports_the_shadow_bindir() {
    let home = std::env::temp_dir().join(format!("zvcs-execpath-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(BIN)
        .args(["--exec-path"])
        .env("HOME", home.to_str().unwrap())
        .env_remove("GIT_EXEC_PATH")
        .output()
        .expect("run");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("{}/.zvcs/bin", home.display())
    );
    let _ = std::fs::remove_dir_all(&home);
}
