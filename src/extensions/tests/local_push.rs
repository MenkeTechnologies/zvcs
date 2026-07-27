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

/// `post-receive` and `post-update` run AFTER the server has sent its
/// `report-status`, and everything they write comes back on side-band 2. A client
/// that stops at the report — or that tears the connection down once it has read
/// it — never lets them run at all, and the pusher never sees a word of what the
/// server said.
///
/// This pins both halves: the hook side-effect files prove the hooks executed,
/// and the `remote:` lines prove their output was demultiplexed rather than
/// dropped. It is the regression test for a `Drop` that killed `git-receive-pack`
/// the instant the report had been read.
#[test]
fn post_receive_hooks_run_and_their_output_comes_back_as_remote_lines() {
    let root = std::env::temp_dir().join(format!("zvcs-pushhooks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let bindir = root.join("bin");
    let work = root.join("work");
    let bare = root.join("remote.git");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bindir).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    for name in ["git", "git-receive-pack", "git-upload-pack"] {
        std::os::unix::fs::symlink(BIN, bindir.join(name)).unwrap();
    }

    run(&work, &home, &bindir, &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
    run(&work, &home, &bindir, &["init", "-q", "-b", "main", "."]);
    run(&work, &home, &bindir, &["config", "user.email", "t@e.co"]);
    run(&work, &home, &bindir, &["config", "user.name", "t"]);

    // Each hook records that it ran and says something on stderr, which the
    // server multiplexes onto band 2. `warning`/`hint` are keywords the sideband
    // colorizer knows, so this also covers the uncolored (non-tty) rendering.
    let hooks = bare.join("hooks");
    for (name, body) in [
        (
            "post-receive",
            "#!/bin/sh\nwhile read -r o n r; do echo \"$r\" >> \"$GIT_DIR/ran-post-receive\"; done\n\
             echo 'warning: post-receive spoke' >&2\n",
        ),
        (
            "post-update",
            "#!/bin/sh\nfor r in \"$@\"; do echo \"$r\" >> \"$GIT_DIR/ran-post-update\"; done\n\
             echo 'hint: post-update spoke' >&2\n",
        ),
    ] {
        let path = hooks.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    std::fs::write(work.join("f"), "one\n").unwrap();
    run(&work, &home, &bindir, &["add", "f"]);
    run(&work, &home, &bindir, &["commit", "-q", "-m", "c0"]);
    run(&work, &home, &bindir, &["remote", "add", "origin", bare.to_str().unwrap()]);

    let out = run(&work, &home, &bindir, &["push", "origin", "main"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "push failed: {stderr}");

    for hook in ["post-receive", "post-update"] {
        let marker = bare.join(format!("ran-{hook}"));
        let recorded = std::fs::read_to_string(&marker)
            .unwrap_or_else(|e| panic!("{hook} never ran ({e}); stderr was:\n{stderr}"));
        assert_eq!(recorded.trim(), "refs/heads/main", "{hook} saw the wrong ref");
    }

    // git prefixes each line with `remote: ` and, when stderr is not a terminal,
    // pads it with the clear-to-eol run of spaces (`DUMB_SUFFIX`).
    assert!(
        stderr.contains("remote: warning: post-receive spoke        \n"),
        "post-receive's output was not demultiplexed onto stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("remote: hint: post-update spoke        \n"),
        "post-update's output was not demultiplexed onto stderr:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
