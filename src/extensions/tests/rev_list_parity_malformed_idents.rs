//! Commits whose author or committer line is malformed, written with
//! `hash-object -t commit --literally`, against stock git as the oracle
//! (`support/stock_git.rs`).
//!
//! git's walks parse a commit with `parse_commit_buffer()` (commit.c:516-598),
//! which reads the tree and the parents strictly and the date with
//! `parse_commit_date()` (commit.c:128-194), 0 for an ident it cannot read; the
//! idents are left to fsck and to the pretty printers, which split each one
//! with `split_ident_line()` and print what they can (pretty.c:788-867,
//! :2053-2107). So such a commit is walked by rev-list, merge-base, log,
//! pack-objects, push and commit-graph like any other, while fsck still reports
//! it.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// The header lines between `parent` and the blank line, one per shape.
const SHAPES: [(&str, &str); 11] = [
    ("bad-author", "author bad <a@e 1700000000 +0000\ncommitter C <c@e> 1700000001 +0000"),
    ("no-author", "committer C <c@e> 1700000001 +0000"),
    ("no-committer", "author A <a@e> 1700000000 +0000"),
    ("bad-date", "author A <a@e> 1700000000 +0000\ncommitter C <c@e> abc +0000"),
    ("bad-tz", "author A <a@e> 1700000000 +0000\ncommitter C <c@e> 1700000001 +00x0"),
    ("bad-committer", "author A <a@e> 1700000000 +0000\ncommitter C c@e> 1700000001 +0000"),
    ("no-email", "author A 1700000000 +0000\ncommitter C <c@e> 1700000001 +0000"),
    ("no-tz", "author A <a@e> 1700000000\ncommitter C <c@e> 1700000001"),
    ("negative-date", "author A <a@e> -5 +0000\ncommitter C <c@e> -7 +0000"),
    ("huge-date", "author A <a@e> 99999999999999999999999 +0000\ncommitter C <c@e> 99999999999999999999999 +0000"),
    (
        "two-authors",
        "author A <a@e> 1700000000 +0000\nauthor B <b@e> 1700000005 +0100\ncommitter C <c@e> 1700000001 +0000",
    ),
];

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture(name: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-malformed-idents-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    Fixture { root }
}

fn git_in(bin: &str, cwd: &Path, root: &Path, args: &[&str], stdin: &[u8]) -> Output {
    use std::io::Write;
    let mut child = Command::new(bin)
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
        .env("GIT_AUTHOR_EMAIL", "a@e")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@e")
        .env("GIT_AUTHOR_DATE", "1600000000 +0000")
        .env("GIT_COMMITTER_DATE", "1600000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut sink = child.stdin.take().unwrap();
    let _ = sink.write_all(stdin);
    drop(sink);
    child.wait_with_output().unwrap()
}

fn git(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> Output {
    git_in(bin, cwd, root, args, b"")
}

fn ok(bin: &str, cwd: &Path, root: &Path, args: &[&str], stdin: &[u8]) -> String {
    let out = git_in(bin, cwd, root, args, stdin);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// `repo` with `base`, the malformed commit on it (`header` between its
/// `parent` and the message) and `top` on that, `main` at `top`. With no
/// `base` the malformed commit is the root.
fn build(stock: &str, root: &Path, repo: &str, header: &str, with_base: bool) -> PathBuf {
    ok(stock, root, root, &["init", "-q", "-b", "main", repo], b"");
    let dir = root.join(repo);
    let mut parent = String::new();
    if with_base {
        std::fs::write(dir.join("a"), "a\n").unwrap();
        ok(stock, &dir, root, &["add", "a"], b"");
        ok(stock, &dir, root, &["commit", "-q", "-m", "base"], b"");
        parent = format!("parent {}\n", ok(stock, &dir, root, &["rev-parse", "HEAD"], b""));
    }
    std::fs::write(dir.join("b"), "b\n").unwrap();
    ok(stock, &dir, root, &["add", "b"], b"");
    let tree = ok(stock, &dir, root, &["write-tree"], b"");
    let body = format!("tree {tree}\n{parent}{header}\n\nmalformed\n");
    let bad = ok(
        stock,
        &dir,
        root,
        &["hash-object", "-t", "commit", "-w", "--literally", "--stdin"],
        body.as_bytes(),
    );
    ok(stock, &dir, root, &["update-ref", "refs/heads/main", &bad], b"");
    std::fs::write(dir.join("c"), "c\n").unwrap();
    ok(stock, &dir, root, &["add", "c"], b"");
    ok(stock, &dir, root, &["commit", "-q", "-m", "top"], b"");
    dir
}

fn same(stock: &str, dir: &Path, root: &Path, shape: &str, args: &[&str]) {
    let want = git(stock, dir, root, args);
    let got = git(ZVCS, dir, root, args);
    assert_eq!(
        (String::from_utf8_lossy(&got.stdout), String::from_utf8_lossy(&got.stderr), got.status.code()),
        (String::from_utf8_lossy(&want.stdout), String::from_utf8_lossy(&want.stderr), want.status.code()),
        "{shape}: git {args:?}"
    );
}

/// The walks, the pretty printers and fsck over every shape.
#[test]
fn walks_print_and_fsck_like_stock() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture("read");
    let cases: &[&[&str]] = &[
        &["rev-list", "main"],
        &["rev-list", "--objects", "main"],
        &["rev-list", "--timestamp", "main"],
        &["rev-list", "--date-order", "--since=1650000000", "main"],
        &["rev-list", "--format=%an|%ae|%cd", "main"],
        &["rev-parse", "main~1", "main~2"],
        &["merge-base", "main", "main~2"],
        &["merge-base", "--is-ancestor", "main~1", "main"],
        &["merge-base", "--independent", "main", "main~1", "main~2"],
        &["log"],
        &["log", "--format=%an|%ae|%ad|%cn|%ce|%cd|%at|%ct"],
        &["log", "--format=%aI|%as|%al|%ah|%aN"],
        &["log", "--pretty=raw"],
        &["log", "--pretty=fuller"],
        &["log", "--pretty=email"],
        &["log", "--pretty=reference"],
        &["log", "--oneline"],
        &["show", "-s", "main~1"],
        &["fsck"],
    ];
    for (shape, header) in SHAPES {
        let dir = build(stock, &fx.root, shape, header, true);
        for args in cases {
            same(stock, &dir, &fx.root, shape, args);
        }
    }
    let dir = build(stock, &fx.root, "root", "committer C <c@e> 1700000001 +0000", false);
    for args in cases {
        same(stock, &dir, &fx.root, "root", args);
    }
}

/// The object ids a pack holds, read by stock `index-pack`/`show-index`.
fn pack_ids(stock: &str, root: &Path, pack: &[u8], name: &str) -> Vec<String> {
    let file = root.join(format!("{name}.pack"));
    std::fs::write(&file, pack).unwrap();
    ok(stock, root, root, &["index-pack", file.to_str().unwrap()], b"");
    let idx = std::fs::read(root.join(format!("{name}.idx"))).unwrap();
    let mut ids: Vec<String> = ok(stock, root, root, &["show-index"], &idx)
        .lines()
        .map(|l| l.split(' ').nth(1).unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

/// `pack-objects --revs` packs the malformed commit and what it reaches, and
/// `commit-graph write` records it with `parse_commit_date()`'s date and the
/// generation `compute_generation_from_max()` gives it (commit-graph.c:1627-1645),
/// byte for byte.
#[test]
fn pack_objects_and_commit_graph_like_stock() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture("pack");
    let mut shapes: Vec<(&str, &str, bool)> = SHAPES.iter().map(|&(s, h)| (s, h, true)).collect();
    shapes.push(("root", "committer C <c@e> 1700000001 +0000", false));
    for (shape, header, with_base) in shapes {
        let dir = build(stock, &fx.root, shape, header, with_base);
        let want = git_in(stock, &dir, &fx.root, &["pack-objects", "--revs", "--stdout", "-q"], b"main\n");
        let got = git_in(ZVCS, &dir, &fx.root, &["pack-objects", "--revs", "--stdout", "-q"], b"main\n");
        assert_eq!(got.status.code(), want.status.code(), "{shape}: pack-objects");
        assert_eq!(
            pack_ids(stock, &fx.root, &got.stdout, &format!("{shape}-z")),
            pack_ids(stock, &fx.root, &want.stdout, &format!("{shape}-s")),
            "{shape}: pack-objects --revs"
        );

        let graph = dir.join(".git/objects/info/commit-graph");
        let mut written = Vec::new();
        for bin in [stock, ZVCS] {
            let _ = std::fs::remove_file(&graph);
            let out = git(bin, &dir, &fx.root, &["commit-graph", "write", "--reachable"]);
            assert!(out.status.success(), "{shape}: {bin} commit-graph write: {}", String::from_utf8_lossy(&out.stderr));
            written.push(std::fs::read(&graph).unwrap());
        }
        assert!(written[0] == written[1], "{shape}: commit-graph differs");
        // With the graph in place the walks read it rather than the objects.
        same(stock, &dir, &fx.root, shape, &["rev-list", "--timestamp", "main"]);
        same(stock, &dir, &fx.root, shape, &["merge-base", "--all", "main", "main~1"]);
    }
}

/// A push of the malformed commit, from either binary into either binary's
/// `receive-pack`, with and without `receive.fsckObjects`, onto an empty
/// repository and onto one already holding its parent.
#[test]
fn push_like_stock() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture("push");
    for (shape, header) in [SHAPES[0], SHAPES[1], SHAPES[2], SHAPES[3]] {
        let dir = build(stock, &fx.root, shape, header, true);
        for seeded in [false, true] {
            for fsck in ["false", "true"] {
                for receiver in [stock, ZVCS] {
                    let mut seen = Vec::new();
                    for pusher in [stock, ZVCS] {
                        let dst = fx.root.join(format!("{shape}-dst"));
                        let _ = std::fs::remove_dir_all(&dst);
                        ok(stock, &fx.root, &fx.root, &["init", "-q", "--bare", dst.to_str().unwrap()], b"");
                        ok(stock, &dst, &fx.root, &["config", "receive.fsckObjects", fsck], b"");
                        ok(stock, &dst, &fx.root, &["config", "receive.autogc", "false"], b"");
                        if seeded {
                            ok(stock, &dir, &fx.root, &["push", "-q", dst.to_str().unwrap(), "main~2:refs/heads/main"], b"");
                        }
                        let rp = format!("--receive-pack={receiver} receive-pack");
                        let out = git(pusher, &dir, &fx.root, &["push", &rp, dst.to_str().unwrap(), "main"]);
                        let refs = ok(stock, &dst, &fx.root, &["for-each-ref"], b"");
                        let stderr = String::from_utf8_lossy(&out.stderr).replace(dst.to_str().unwrap(), "DST");
                        seen.push((String::from_utf8_lossy(&out.stdout).into_owned(), stderr, out.status.code(), refs));
                    }
                    assert_eq!(
                        seen[1], seen[0],
                        "{shape}: push seeded={seeded} receive.fsckObjects={fsck} receive-pack={receiver}"
                    );
                }
            }
        }
    }
}
