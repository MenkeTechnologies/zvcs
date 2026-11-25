//! `git fetch --prune` where the prune and the fetch meet on one ref, run by
//! stock git and by zvcs on twin copies of one fixture at the same path.
//!
//! * Under `--atomic` the stale ref's deletion is queued in the transaction the
//!   updates join (builtin/fetch.c:2020-2040, :1478-1491). An unqualified exact
//!   refspec, `feat:refs/remotes/origin/feat`, prunes its own destination and
//!   updates it too, so the commit refuses the transaction whole with
//!   `ref_update_reject_duplicates()`'s `multiple updates for ref ... not
//!   allowed` (refs.c:2574-2596, :2708-2710): the summary row is printed, then
//!   the error, exit 255, and `FETCH_HEAD` stays empty.
//! * Without `--atomic` a tag pruned the same way (`t1:refs/tags/t1`) is up to
//!   date for the explicit refspec, which queues nothing, so the second
//!   `find_non_local_tags()` call finds it missing and `backfill_tags()` creates
//!   it again (builtin/fetch.c:2070-2090): `* [new tag]` after the first pass's
//!   rows, sorted among the other backfilled tags, and a `not-for-merge` row in
//!   `FETCH_HEAD` behind the explicit one. Under `--atomic` the queued deletion
//!   counts as a queued tag (:361-371) and nothing comes back.
//!
//! No network: the remote is a directory beside the clone.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn env<'a>(cmd: &'a mut Command, home: &Path) -> &'a mut Command {
    cmd.env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
}

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    env(Command::new(bin).args(args).current_dir(dir), home)
        .output()
        .expect("spawn git")
}

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str]) {
    let out = run(bin, dir, home, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `up` (main, feat, lightweight t1, annotated t2) cloned into `cl`; then main
/// and feat advance, and `a0` tags a commit only the new main reaches.
fn base(stock: &str, root: &Path, home: &Path) -> PathBuf {
    let base = root.join("base");
    let up = base.join("up");
    std::fs::create_dir_all(&up).unwrap();
    let g = |dir: &Path, args: &[&str]| ok(stock, dir, home, args);
    g(&up, &["init", "-q", "-b", "main"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "one"]);
    g(&up, &["branch", "feat"]);
    g(&up, &["tag", "t1"]);
    g(&up, &["tag", "-a", "-m", "ann", "t2"]);
    g(&base, &["clone", "-q", "up", "cl"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "two"]);
    g(&up, &["tag", "a0"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "three"]);
    g(&up, &["branch", "-f", "feat"]);
    base
}

/// Everything a case can change, read back with stock git.
fn state(stock: &str, cl: &Path, home: &Path) -> String {
    let refs = run(stock, cl, home, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    let reflogs = run(stock, cl, home, &["reflog", "list"]);
    let fetch_head = std::fs::read_to_string(cl.join(".git/FETCH_HEAD")).unwrap_or_default();
    format!(
        "{}--\n{}--\n{fetch_head}",
        String::from_utf8_lossy(&refs.stdout),
        String::from_utf8_lossy(&reflogs.stdout)
    )
}

/// Runs `cases` (`sh -c` scripts with `$G` naming the binary) from `cl` under
/// both binaries, each on a fresh copy of the fixture at the same path.
fn compare(tag: &str, cases: &[&str]) {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-fetch-prune-meet-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _scratch = Scratch(root.clone());
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".gitconfig"), "[maintenance]\n\tauto = false\n[gc]\n\tauto = 0\n").unwrap();
    let base = base(stock, &root, &home);
    let work = root.join("w");
    let cl = work.join("cl");
    for case in cases {
        let mut seen = Vec::new();
        for bin in [stock, BIN] {
            let _ = std::fs::remove_dir_all(&work);
            let cp = Command::new("cp").arg("-R").arg(&base).arg(&work).status().unwrap();
            assert!(cp.success());
            let out = env(Command::new("sh").arg("-c").arg(case).current_dir(&cl).env("G", bin), &home)
                .output()
                .unwrap();
            seen.push(format!(
                "rc={:?}\n--stdout\n{}--stderr\n{}--state\n{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
                state(stock, &cl, &home)
            ));
        }
        assert_eq!(seen[1], seen[0], "case {case:?}: zvcs (left) vs stock (right)");
    }
}

#[test]
fn atomic_prune_and_update_of_one_ref_is_refused_whole() {
    compare(
        "atomic",
        &[
            "$G fetch --atomic --prune origin feat:refs/remotes/origin/feat",
            "$G fetch --atomic --prune --porcelain origin feat:refs/remotes/origin/feat main:refs/remotes/origin/main",
        ],
    );
}

#[test]
fn a_pruned_tag_comes_back_on_the_second_pass() {
    compare(
        "refollow",
        &[
            "$G fetch --prune origin t1:refs/tags/t1",
            "$G fetch --prune origin t2:refs/tags/t2 t1:refs/tags/t1 main:refs/remotes/origin/main",
            "$G fetch --prune --atomic origin t1:refs/tags/t1",
        ],
    );
}
