//! A local clone records its source as an absolute path.
//!
//! ```c
//! path = get_repo_path(repo_name, &is_bundle);
//! if (path) {
//!         FREE_AND_NULL(path);
//!         repo = repo_to_free = absolute_pathdup(repo_name);
//! }
//! ```
//!
//! (`cmd_clone()`, builtin/clone.c:1054-1057.) `absolute_pathdup()` prefixes the
//! current directory and normalizes nothing else, so `git clone ./peer` records
//! `<cwd>/./peer` — absolute, `./` and all. Every later `git fetch` in the new
//! repository resolves that against its own working directory, so a relative
//! spelling would only work from the directory the clone happened to run in.
//!
//! The bug this pins: `--bare` and `--mirror` build their remote from the url as
//! typed rather than from the absolutized copy, so they wrote `./peer` and the
//! clone's first fetch from anywhere else failed. Plain clones were unaffected,
//! which is why it survived.
//!
//! No network: the source is a directory beside the destination.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn zvcs(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run zvcs git")
}

fn err_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let out = zvcs(dir, home, args);
    assert!(out.status.success(), "{args:?} failed: {}", err_text(&out));
    out
}

/// `git config <key>` in `dir`, trimmed.
fn config(dir: &Path, home: &Path, key: &str) -> String {
    let out = ok(dir, home, &["config", key]);
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// A one-commit source repository plus a scratch root to clone into.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-cloneurl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    // macOS resolves `$TMPDIR` through a symlink; `absolute_pathdup()` prefixes
    // the *process* working directory, so the expectation has to be built from
    // the same resolved spelling.
    let root = std::fs::canonicalize(&root).expect("canonicalize root");
    let origin = root.join("peer");
    ok(&root, &home, &["init", "-q", "-b", "main", origin.to_str().expect("utf-8")]);
    ok(&origin, &home, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    (root, home)
}

#[test]
fn bare_and_mirror_clones_record_an_absolute_source() {
    let (root, home) = fixture("bare");
    // Cloned from `root` with a relative spelling, exactly as a user would.
    ok(&root, &home, &["clone", "-q", "--bare", "./peer", "bare.git"]);
    ok(&root, &home, &["clone", "-q", "--mirror", "./peer", "mirror.git"]);
    ok(&root, &home, &["clone", "-q", "./peer", "work"]);

    // `absolute_pathdup()` keeps the `./`, so the expected string is the clone's
    // working directory with the argument appended verbatim.
    let expected = format!("{}/./peer", root.display());
    for (dir, what) in [
        (root.join("bare.git"), "--bare"),
        (root.join("mirror.git"), "--mirror"),
        (root.join("work"), "plain"),
    ] {
        assert_eq!(
            config(&dir, &home, "remote.origin.url"),
            expected,
            "{what} clone records the absolute source"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_bare_clone_can_fetch_from_another_directory() {
    let (root, home) = fixture("refetch");
    ok(&root, &home, &["clone", "-q", "--bare", "./peer", "bare.git"]);

    // The point of the absolute path: the recorded url has to resolve from
    // somewhere other than the directory the clone ran in. `elsewhere` is a
    // subdirectory, so a stored `./peer` resolves to a path that does not exist
    // and the fetch dies before it reaches the remote.
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("mkdir elsewhere");
    ok(&root.join("peer"), &home, &["commit", "--allow-empty", "-q", "-m", "c1"]);
    let tip = ok(&root.join("peer"), &home, &["rev-parse", "HEAD"]);
    let tip = String::from_utf8_lossy(&tip.stdout).trim_end().to_string();

    let bare = root.join("bare.git");
    let out = zvcs(
        &elsewhere,
        &home,
        &["--git-dir", bare.to_str().expect("utf-8"), "fetch", "origin"],
    );
    assert!(out.status.success(), "fetch from elsewhere: {}", err_text(&out));
    assert!(
        err_text(&out).starts_with(&format!("From {}/./peer\n", root.display())),
        "the header names the absolute source: {}",
        err_text(&out)
    );

    // `--bare` records no fetch refspec, so the only thing a plain `git fetch`
    // writes is `FETCH_HEAD` — which is enough to show the transport reached the
    // right repository.
    let fetch_head =
        std::fs::read_to_string(bare.join("FETCH_HEAD")).expect("read FETCH_HEAD");
    assert!(fetch_head.starts_with(&tip), "FETCH_HEAD holds the new tip:\n{fetch_head}");

    let _ = std::fs::remove_dir_all(&root);
}
