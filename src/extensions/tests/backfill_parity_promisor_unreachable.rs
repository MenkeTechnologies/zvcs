//! `backfill` dies when the promisor remote cannot hand over what it promised.
//!
//! `download_batch()` calls `promisor_remote_get_direct()`, which ends with
//!
//! ```c
//! for (i = 0; i < remaining_nr; i++) {
//!         if (is_promisor_object(repo, &remaining_oids[i]))
//!                 die(_("could not fetch %s from promisor remote"), oid_to_hex(&remaining_oids[i]));
//! }
//! ```
//!
//! (promisor-remote.c:320-324), after the `git fetch` child has printed why it failed. Typed from
//! `.git/refs`, the relative `./.remote.git` the partial clone recorded no longer names the remote,
//! so the child says `'./.remote.git' does not appear to be a git repository`, git adds the
//! four-line hang-up block and then dies with the `could not fetch` line at 128. zvcs ignored the
//! outcome of the batch and exited 0 without a word.
//!
//! Both worlds are built by stock git, so only the command under test differs.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// `root/par`: a blobless partial clone whose promisor remote is the relative `./.remote.git`.
fn partial_clone(label: &str, stock: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-backfill-promisor-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let run = |dir: &Path, args: &[&str]| {
        let out = git(stock, dir, args);
        assert_eq!(out.2, 0, "{args:?}: {out:?}");
    };
    run(&root, &["init", "-q", "-b", "main", "src"]);
    let src = root.join("src");
    std::fs::write(src.join("README.md"), "# fixture\n").unwrap();
    run(&src, &["add", "README.md"]);
    run(&src, &["commit", "-q", "-m", "one"]);
    run(&root, &["clone", "-q", "--bare", "src", "remote.git"]);
    run(&root.join("remote.git"), &["config", "uploadpack.allowFilter", "true"]);
    let url = format!("file://{}", root.join("remote.git").display());
    run(&root, &["clone", "-q", "--no-checkout", "--filter=blob:none", &url, "par"]);
    let par = root.join("par");
    std::fs::rename(root.join("remote.git"), par.join(".remote.git")).unwrap();
    run(&par, &["config", "remote.origin.url", "./.remote.git"]);
    (root, par)
}

#[test]
fn an_unreachable_promisor_remote_ends_the_batch_with_the_could_not_fetch_fatal() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s_root, s_par) = partial_clone("unreachable-stock", stock);
    let (z_root, z_par) = partial_clone("unreachable-zvcs", stock);
    let clean = |root: &Path, (o, e, c): (String, String, i32)| {
        let root = root.to_string_lossy();
        (o.replace(root.as_ref(), "<root>"), e.replace(root.as_ref(), "<root>"), c)
    };
    let sub = ".git/refs";
    let want = clean(&s_root, git(stock, &s_par.join(sub), &["backfill"]));
    let got = clean(&z_root, git(ZVCS, &z_par.join(sub), &["backfill"]));
    let _ = std::fs::remove_dir_all(&s_root);
    let _ = std::fs::remove_dir_all(&z_root);
    assert_eq!(want.2, 128, "{want:?}");
    assert!(want.1.contains("from promisor remote"), "{want:?}");
    assert_eq!(got, want);
}

#[test]
fn a_reachable_promisor_remote_still_downloads_quietly() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s_root, s_par) = partial_clone("reachable-stock", stock);
    let (z_root, z_par) = partial_clone("reachable-zvcs", stock);
    let want = git(stock, &s_par, &["backfill"]);
    let got = git(ZVCS, &z_par, &["backfill"]);
    let blobs = |par: &Path| git(stock, par, &["rev-list", "--objects", "--missing=print", "HEAD"]).0;
    let (want_missing, got_missing) = (blobs(&s_par), blobs(&z_par));
    let _ = std::fs::remove_dir_all(&s_root);
    let _ = std::fs::remove_dir_all(&z_root);
    assert_eq!(want, ("".into(), "".into(), 0));
    assert_eq!(got, want);
    assert!(!want_missing.contains('?'), "{want_missing}");
    assert!(!got_missing.contains('?'), "zvcs left blobs missing: {got_missing}");
}
