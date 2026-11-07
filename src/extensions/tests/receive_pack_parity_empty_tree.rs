//! `receive-pack` under the unpack limit writes every pushed object the store has
//! no file for (`unpack-objects`, `write_object_file()` freshening only an object
//! some source holds on disk, odb.c:824-832). The empty tree is one git answers
//! for without a file (`find_cached_object()`), so a store that has never held it
//! still gets it from a push of an empty commit, where zvcs skipped it and left
//! the pushed commit pointing at a tree the repository did not have.
//!
//! The same empty commit is pushed with stock `git push` into stock
//! `receive-pack` and into this binary's, and both receiving repositories are
//! compared with stock `fsck` and their loose objects. Stock git is the oracle
//! (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(["-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

/// Push the empty commit into a fresh bare repository served by `server`, then
/// report the pusher's exit code, stock `fsck`'s output and the loose objects.
fn push_into(stock: &str, root: &Path, server: &str, tag: &str) -> (i32, String, Vec<String>) {
    let bare = root.join(format!("{tag}.git"));
    assert!(git(stock, root, root, &["init", "-q", "--bare", bare.to_str().unwrap()]).status.success());
    let receive_pack = format!("{server} receive-pack");
    let out = git(
        stock,
        &root.join("work"),
        root,
        &["push", "-q", "--receive-pack", &receive_pack, bare.to_str().unwrap(), "main"],
    );
    let fsck = git(stock, &bare, root, &["fsck", "--no-dangling"]);
    let mut loose: Vec<String> = Vec::new();
    for shard in std::fs::read_dir(bare.join("objects")).unwrap().flatten() {
        let name = shard.file_name().to_string_lossy().into_owned();
        if name.len() != 2 {
            continue;
        }
        for obj in std::fs::read_dir(shard.path()).unwrap().flatten() {
            loose.push(format!("{name}{}", obj.file_name().to_string_lossy()));
        }
    }
    loose.sort();
    let fsck_out = String::from_utf8_lossy(&fsck.stdout).into_owned() + &String::from_utf8_lossy(&fsck.stderr);
    (out.status.code().unwrap_or(-1), fsck_out, loose)
}

#[test]
fn a_pushed_empty_commit_brings_its_empty_tree() {
    let Some(stock) = stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-receive-empty-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("work")).unwrap();
    let fx = Fixture { root: root.canonicalize().unwrap() };
    let work = fx.root.join("work");
    assert!(git(stock, &work, &fx.root, &["init", "-q", "-b", "main"]).status.success());
    assert!(git(stock, &work, &fx.root, &["commit", "-q", "--allow-empty", "-m", "c0"]).status.success());

    let want = push_into(stock, &fx.root, stock, "stock");
    let got = push_into(stock, &fx.root, ZVCS, "zvcs");
    assert_eq!(want.0, 0, "{want:?}");
    assert!(want.2.iter().any(|o| o == "4b825dc642cb6eb9a060e54bf8d69288fbee4904"), "{want:?}");
    assert_eq!(got, want);
}
