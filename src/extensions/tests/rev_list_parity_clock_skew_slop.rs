//! `rev-list <commit> --not <tips>` when a commit is dated after its own child.
//!
//! limit_list() does not stop at the first uninteresting commit popped once no
//! interesting one is queued: still_interesting() renews `slop` while the next
//! queued commit is no older than the last interesting one, and otherwise allows
//! SLOP (5) more uninteresting pops (revision.c:1304-1327, :1480-1485). Those extra
//! pops are what carry the uninteresting mark through a parent dated later than
//! its child. gix-traverse's hidden-frontier paint left on the first hidden pop,
//! so `B --not main side topic v1` listed B although `main` reaches it, and
//! `submodule update` took such a recorded commit for unreachable and fetched it.
//!
//! History (dates in seconds after 1700000000):
//!
//! ```text
//!   A(60, v1) - B(120) - C(50) - D(0)   main = D, side = C
//!        \
//!         T(180)                         topic = T
//! ```
//!
//! Skipped when no stock git is available.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str], date: &str) -> Output {
    let mut c = Command::new(bin);
    for (k, _) in std::env::vars() {
        if k.starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    c.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("LC_ALL", "C")
        .output()
        .expect("spawn git")
}

fn ok(bin: &str, dir: &Path, args: &[&str], date: &str) -> String {
    let out = run(bin, dir, args, date);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn at(offset: u32) -> String {
    format!("{} +0000", 1_700_000_000 + offset)
}

#[test]
fn hidden_tips_reach_a_commit_dated_after_its_child() {
    let Some(stock) = stock_git::stock_git() else { return };
    let dir = std::env::temp_dir().join(format!("zvcs-revlist-skew-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    ok(stock, &dir, &["init", "-q", "-b", "main"], &at(0));
    let commit = |msg: &str, offset: u32| {
        ok(stock, &dir, &["commit", "-q", "--allow-empty", "-m", msg], &at(offset));
    };
    commit("A", 60);
    ok(stock, &dir, &["tag", "v1"], &at(0));
    ok(stock, &dir, &["branch", "topic"], &at(0));
    commit("B", 120);
    let b = ok(stock, &dir, &["rev-parse", "HEAD"], &at(0));
    commit("C", 50);
    ok(stock, &dir, &["branch", "side"], &at(0));
    commit("D", 0);
    ok(stock, &dir, &["checkout", "-q", "topic"], &at(0));
    commit("T", 180);
    ok(stock, &dir, &["checkout", "-q", "main"], &at(0));

    let cases: &[&[&str]] = &[
        &[&b, "--not", "main", "side", "topic", "v1"],
        &["-n", "1", &b, "--not", "--all"],
        &[&b, "--not", "--branches", "--tags"],
        &["--count", &b, "--not", "side", "v1", "topic"],
        &["--boundary", &b, "--not", "--all"],
    ];
    for args in cases {
        let mut full = vec!["rev-list"];
        full.extend_from_slice(args);
        let s = run(stock, &dir, &full, &at(0));
        let z = run(BIN, &dir, &full, &at(0));
        assert_eq!(String::from_utf8_lossy(&z.stdout), String::from_utf8_lossy(&s.stdout), "{full:?}");
        assert_eq!(z.status.code(), s.status.code(), "{full:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
