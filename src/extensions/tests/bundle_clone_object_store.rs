//! `git clone [--no-local] <file.bundle>`: the pack `unbundle()` installs must survive the
//! clone, compared against stock git.
//!
//! `transport_get()` gives a bundle a `fetch_refs` that is `unbundle()`, so the clone's
//! object store is exactly the pack the bundle carried (`index-pack --fix-thin --stdin`).
//! zvcs materializes the bundle in a scratch repository and adopts its objects; the
//! adoption must read the scratch repository, not the bundle file, or the clone ends with
//! no objects, `refs/heads/main` unborn and `origin/HEAD` missing.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/stock_git.rs"]
mod stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn ok(bin: &str, dir: &Path, args: &[&str]) -> String {
    let out = run(bin, dir, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Everything observable about a finished clone, read by stock git for both sides.
fn snapshot(stock: &str, clone: &Path) -> Vec<String> {
    let mut objects: Vec<String> = ok(stock, clone, &["cat-file", "--batch-check", "--batch-all-objects"])
        .lines()
        .map(str::to_owned)
        .collect();
    objects.sort();
    let mut snap = vec![
        format!("refs:\n{}", ok(stock, clone, &["for-each-ref"])),
        format!("HEAD: {}", ok(stock, clone, &["symbolic-ref", "HEAD"])),
        format!("origin/HEAD: {}", ok(stock, clone, &["symbolic-ref", "refs/remotes/origin/HEAD"])),
        format!("worktree: {}", ok(stock, clone, &["ls-files"])),
        format!("objects:\n{}", objects.join("\n")),
    ];
    let packs = std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect::<std::collections::BTreeSet<_>>();
    snap.push(format!("packs: {packs:?}"));
    ok(stock, clone, &["fsck", "--strict"]);
    snap
}

#[test]
fn clone_of_a_bundle_keeps_the_bundles_pack() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = Root(std::env::temp_dir().join(format!("zvcs-bundleclone-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&root.0);
    std::fs::create_dir_all(&root.0).unwrap();
    let src = root.0.join("src");
    std::fs::create_dir_all(&src).unwrap();
    ok(stock, &src, &["init", "-q", "-b", "main"]);
    for (i, body) in ["one\n", "one\ntwo\n", "one\ntwo\nthree\n"].into_iter().enumerate() {
        std::fs::write(src.join("f"), body).unwrap();
        ok(stock, &src, &["add", "f"]);
        ok(stock, &src, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    ok(stock, &src, &["branch", "feature", "main~1"]);
    ok(stock, &src, &["tag", "-a", "-m", "release", "v1"]);
    ok(stock, &src, &["bundle", "create", "../all.bundle", "--all"]);

    for flags in [&["-q"][..], &["-q", "--no-local"][..]] {
        let mut snaps = Vec::new();
        for (bin, dest) in [(stock, "stock"), (BIN, "zvcs")] {
            let mut args = vec!["clone"];
            args.extend_from_slice(flags);
            args.extend(["./all.bundle", dest]);
            ok(bin, &root.0, &args);
            snaps.push(snapshot(stock, &root.0.join(dest)));
            std::fs::remove_dir_all(root.0.join(dest)).unwrap();
        }
        assert_eq!(snaps[1], snaps[0], "clone {flags:?} of a bundle diverged from stock");
    }
}
