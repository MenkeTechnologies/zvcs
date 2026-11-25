//! `git ls-remote --sort=<date atom>` between local repositories, run by stock
//! git and by zvcs on twin copies of one fixture at the same path.
//!
//! * Each date atom is the object type's own (`grab_values()`, ref-filter.c):
//!   `committerdate`/`authordate` of an annotated tag and `taggerdate` of a
//!   commit are 0, `creatordate` is the committer or the tagger.
//! * The atoms are read when a comparison first reaches a ref, and a missing
//!   object dies there (`cmp_ref_sorting()`, ref-filter.c:3548-3551), so the
//!   ref named is the one `git_qsort_s()`'s merge sort reaches first
//!   (compat/qsort_s.c).
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

/// `up` (main, feat; annotated `a1`, lightweight `v1`; author dates that run
/// against the commit order) cloned into `cl` with its remote spelled `../up`,
/// then advanced on both branches so the clone lacks their objects.
fn base(stock: &str, root: &Path, home: &Path) -> PathBuf {
    let base = root.join("base");
    let up = base.join("up");
    std::fs::create_dir_all(&up).unwrap();
    let g = |dir: &Path, args: &[&str]| ok(stock, dir, home, args);
    g(&up, &["init", "-q", "-b", "main"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "one", "--date=@1700000500 +0000"]);
    g(&up, &["branch", "feat"]);
    g(&up, &["tag", "-a", "-m", "ann", "a1"]);
    g(&up, &["tag", "v1"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "two", "--date=@1699999000 +0000"]);
    g(&base, &["clone", "-q", "up", "cl"]);
    let cl = base.join("cl");
    g(&cl, &["config", "remote.origin.url", "../up"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "three"]);
    g(&up, &["checkout", "-q", "feat"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "four"]);
    g(&up, &["checkout", "-q", "main"]);
    base
}
/// Everything a case can change, read back with stock git.
fn state(stock: &str, cl: &Path, home: &Path) -> String {
    let refs = run(stock, cl, home, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    let config = run(stock, cl, home, &["config", "--local", "--list"]);
    let fetch_head = std::fs::read_to_string(cl.join(".git/FETCH_HEAD")).unwrap_or_default();
    format!(
        "{}--\n{}--\n{fetch_head}",
        String::from_utf8_lossy(&refs.stdout),
        String::from_utf8_lossy(&config.stdout)
    )
}

/// Runs `cases` (`sh -c` scripts with `$G` naming the binary) from `cl` under
/// both binaries, each on a fresh copy of the fixture at the same path.
fn compare(tag: &str, cases: &[&str]) {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-lsremotesweep-{tag}-{}", std::process::id()));
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
                .env("S", stock)
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
fn date_atoms_follow_the_object_type() {
    compare(
        "atoms",
        &[
            "$G ls-remote --sort=committerdate .",
            "$G ls-remote --sort=taggerdate --tags .",
            "$G ls-remote --sort=-authordate .",
            "$G ls-remote --sort=taggerdate --sort=committerdate .",
        ],
    );
}

#[test]
fn the_missing_object_named_is_the_first_one_compared() {
    compare(
        "missing",
        &[
            "$G ls-remote --sort=creatordate origin",
            "$G ls-remote --sort=committerdate ../up main feat",
        ],
    );
}
