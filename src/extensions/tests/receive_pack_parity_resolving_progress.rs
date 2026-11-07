//! The delta meter a push shows for the far end's `index-pack`.
//!
//! `receive-pack` hands a pack at or above `receive.unpackLimit` to `index-pack`,
//! with `--show-resolving-progress` when it is not quiet and has the sideband
//! muxer for the child's stderr (object-file.c:1401-1402,
//! builtin/receive-pack.c:2293-2324), so `Resolving deltas` reaches the pusher as
//! `remote:` lines, ending `done.` or `completed with <n> local objects.`
//! (builtin/index-pack.c:1340-1343, :1370-1397). `-q`, the client's `quiet`
//! capability (`push -q`) and `unpack-objects` (a pack under the limit) show none.
//!
//! The pusher is stock `push --progress`; the far end is stock or zvcs
//! `receive-pack`. The `Writing objects` line carries a rate, which is cut off
//! before comparing. Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> Output {
    Command::new(bin)
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

/// `src` has three revisions of one long file; `dst.git` has the first, with
/// `receive.unpackLimit` as given.
fn fixture(stock: &str, tag: &str, unpack_limit: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-rpresolve-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let ok = |cwd: &Path, args: &[&str]| {
        let out = run(stock, cwd, &root, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    ok(&root, &["init", "-q", "-b", "main", "src"]);
    let src = root.join("src");
    for i in 1..=3 {
        let body: String = (0..300).map(|n| format!("line {n} of a long file, revision {}\n", if n == 7 { i } else { 0 })).collect();
        std::fs::write(src.join("f"), body).unwrap();
        std::fs::write(src.join(format!("g{i}")), format!("{i}\n")).unwrap();
        ok(&src, &["add", "."]);
        ok(&src, &["commit", "-qm", &format!("c{i}")]);
    }
    ok(&root, &["init", "-q", "--bare", "-b", "main", "dst.git"]);
    ok(&src, &["push", "-q", "../dst.git", "HEAD~2:refs/heads/main"]);
    ok(&root.join("dst.git"), &["config", "receive.unpackLimit", unpack_limit]);
    Fixture { root }
}

/// stderr with each `\r` turned into a line break and the throughput cut off.
fn normalized(stderr: &[u8], root: &Path) -> String {
    String::from_utf8_lossy(stderr)
        .replace('\r', "<CR>\n")
        .replace(root.to_str().unwrap(), "ROOT")
        .lines()
        .map(|line| match line.find(", ").filter(|_| line.starts_with("Writing objects: 100%")) {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn check(tag: &str, unpack_limit: &str, receive_pack_args: &str, push_args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut results = Vec::new();
    for far_end in [stock, ZVCS] {
        let fx = fixture(stock, tag, unpack_limit);
        let receive_pack = format!("--receive-pack={far_end} receive-pack{receive_pack_args}");
        let mut args = vec!["push", "--progress", receive_pack.as_str()];
        args.extend_from_slice(push_args);
        args.extend_from_slice(&["../dst.git", "main"]);
        let out = run(stock, &fx.root.join("src"), &fx.root, &args);
        let fsck = run(stock, &fx.root.join("dst.git"), &fx.root, &["fsck"]);
        let refs = run(stock, &fx.root.join("dst.git"), &fx.root, &["for-each-ref"]);
        results.push((
            normalized(&out.stderr, &fx.root),
            out.status.code(),
            fsck.status.code(),
            String::from_utf8_lossy(&refs.stdout).into_owned(),
        ));
    }
    assert_eq!(results[0], results[1], "{tag}: stock receive-pack (left) vs zvcs (right)");
}

#[test]
fn receive_pack_parity_kept_pack_shows_resolving_deltas() {
    check("thin", "1", "", &[]);
    check("nothin", "1", "", &["--no-thin"]);
}

#[test]
fn receive_pack_parity_quiet_or_unpacked_shows_no_meter() {
    check("quiet", "1", " -q", &[]);
    check("clientquiet", "1", "", &["-q"]);
    check("unpacked", "100", "", &[]);
}
