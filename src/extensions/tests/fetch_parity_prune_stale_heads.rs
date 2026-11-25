//! `git fetch --prune` deciding what is stale as `get_stale_heads()` does, run by
//! stock git and by zvcs on twin copies of one fixture at the same path.
//!
//! * Every local ref a refspec maps a remote name onto is mapped back through
//!   that refspec, and it is stale when no name it maps back to is in the ref map
//!   (remote.c:2644-2692). An exact refspec with an unqualified source,
//!   `feat:refs/remotes/origin/feat`, maps its destination back to the literal
//!   `feat`, which no ref-map entry (`refs/heads/feat`) is called, so its
//!   destination is pruned; a negative refspec excludes what it names
//!   (refspec.c:349-404).
//! * `get_ref_map()` read every destination's old value before the prune
//!   (builtin/fetch.c:630-652), so the update of a ref the prune just deleted
//!   starts from that value and fails to lock it: `cannot lock ref`, a `!` row,
//!   exit 1.
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

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
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
        .output()
        .expect("spawn git")
}

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str]) {
    let out = run(bin, dir, home, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `up` (main, feat, gone) cloned into `cl`; then main and feat advance and gone
/// is deleted upstream, so the clone has a stale `origin/gone`.
fn base(stock: &str, root: &Path, home: &Path) -> PathBuf {
    let base = root.join("base");
    let up = base.join("up");
    std::fs::create_dir_all(&up).unwrap();
    let g = |dir: &Path, args: &[&str]| ok(stock, dir, home, args);
    g(&up, &["init", "-q", "-b", "main"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "one"]);
    g(&up, &["branch", "feat"]);
    g(&up, &["branch", "gone"]);
    g(&base, &["clone", "-q", "up", "cl"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "two"]);
    g(&up, &["checkout", "-q", "feat"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "three"]);
    g(&up, &["checkout", "-q", "main"]);
    g(&up, &["branch", "-q", "-D", "gone"]);
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
    let root = std::env::temp_dir().join(format!("zvcs-fetch-stale-{tag}-{}", std::process::id()));
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
            let out = Command::new("sh")
                .arg("-c")
                .arg(case)
                .current_dir(&cl)
                .env("G", bin)
                .env("HOME", &home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
                .env_remove("XDG_CONFIG_HOME")
                .env("GIT_EDITOR", "true")
                .env("GIT_AUTHOR_NAME", "A U Thor")
                .env("GIT_AUTHOR_EMAIL", "author@example.com")
                .env("GIT_COMMITTER_NAME", "C O Mitter")
                .env("GIT_COMMITTER_EMAIL", "committer@example.com")
                .env("GIT_AUTHOR_DATE", "1700000100 +0000")
                .env("GIT_COMMITTER_DATE", "1700000100 +0000")
                .env("LC_ALL", "C")
                .env("TERM", "dumb")
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
fn an_unqualified_exact_source_prunes_its_destination_and_fails_the_update() {
    compare(
        "unqualified",
        &[
            "$G fetch --prune origin feat:refs/remotes/origin/feat",
            "$G fetch --prune --porcelain origin main:refs/remotes/origin/main feat:refs/remotes/origin/feat",
            "$G fetch --prune --dry-run origin feat:refs/remotes/origin/feat",
        ],
    );
}

#[test]
fn qualified_and_negative_refspecs() {
    compare(
        "qualified",
        &[
            "$G fetch --prune origin refs/heads/feat:refs/remotes/origin/feat",
            "$G fetch --prune origin 'refs/heads/*:refs/remotes/origin/*' '^refs/heads/gone'",
        ],
    );
}
