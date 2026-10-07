//! Undoing `git mv a A`, where the order of removals and writes decides what is
//! left on a case-insensitive file system.
//!
//! `check_updates()` (unpack-trees.c:455-469) unlinks every `CE_WT_REMOVE` entry
//! before it checks a single `CE_UPDATE` one out, and `oneway_merge()` settles
//! which kept entries need writing before either happens. `reset --hard`,
//! `reset --merge`, `read-tree -u --reset` and `stash` wrote `a` first and then
//! removed `A` — the same file there — leaving ` D a`. `checkout_worktree()`
//! (builtin/checkout.c:465-491) instead walks the index in path order, unlinking or
//! writing each matched entry as it comes, so `restore -SW .` after `mv a A` removes
//! `A` and writes `a`, while after `mv A a` it writes `A` and then removes `a`,
//! which takes the file just written.
//!
//! The first group holds on any file system; the rest only where `A` and `a` name
//! one file, which is probed at run time, and are asserted only there.
//!
//! Every scenario also runs under stock git in the same hermetic environment (its
//! own `HOME`, `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM`), and zvcs must
//! leave the same exit code, files and status as stock does on this file system.

use std::path::PathBuf;
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    bin: &'static str,
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `name` and `b`, committed, by `bin`.
    fn new(bin: &'static str, tag: &str, name: &str) -> Self {
        let side = if bin == BIN { "zvcs" } else { "stock" };
        let root = std::env::temp_dir()
            .join(format!("zvcs-case-only-rename-{tag}-{side}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { bin, root, work };
        f.run(&["init", "-q", "."]);
        std::fs::write(f.work.join(name), "a\n").unwrap();
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "base"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(self.bin)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join(".config"))
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

    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.work)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != ".git")
            .collect();
        names.sort();
        names
    }

    fn status(&self) -> String {
        self.run(&["status", "--short"]).0
    }

    fn case_insensitive(&self) -> bool {
        self.work.join("B").exists()
    }
}

/// What a scenario leaves: each step's exit code, then the files and the status.
type Outcome = (Vec<i32>, Vec<String>, String);

/// Run `steps` over a fresh fixture holding `name` and `b` under `bin`.
fn outcome(bin: &'static str, tag: &str, name: &str, steps: &[&[&str]]) -> Outcome {
    let f = Fixture::new(bin, tag, name);
    let codes = steps.iter().map(|s| f.run(s).2).collect();
    (codes, f.files(), f.status())
}

/// zvcs's outcome, checked against stock git's on the same file system when a
/// stock git is installed.
fn zvcs_outcome(tag: &str, name: &str, steps: &[&[&str]]) -> Outcome {
    let got = outcome(BIN, tag, name, steps);
    if let Some(stock) = stock_git() {
        assert_eq!(got, outcome(stock, tag, name, steps), "{tag}: zvcs must leave what stock leaves");
    }
    got
}

#[test]
fn every_reset_brings_the_lowercase_name_back() {
    for (tag, undo) in [
        ("hard", &["reset", "-q", "--hard"][..]),
        ("merge", &["reset", "-q", "--merge", "HEAD"][..]),
        ("readtree", &["read-tree", "-u", "--reset", "HEAD"][..]),
        ("stash", &["stash", "-q"][..]),
        ("restore", &["restore", "-SW", "."][..]),
        ("checkoutf", &["checkout", "-q", "-f", "HEAD"][..]),
    ] {
        let (codes, files, status) = zvcs_outcome(tag, "a", &[&["mv", "a", "A"], undo]);
        assert_eq!(codes, [0, 0], "{tag}");
        assert_eq!(files, ["a", "b"], "{tag}");
        assert_eq!(status, "", "{tag}");
    }
}

#[test]
fn restore_follows_index_order_when_the_names_collide() {
    if !Fixture::new(BIN, "probe-upper", "A").case_insensitive() {
        return;
    }
    let (_, files, status) =
        zvcs_outcome("upper", "A", &[&["mv", "A", "a"], &["restore", "-SW", "."]]);
    // `A` is written, then the removal of `a` takes the same file.
    assert_eq!(files, ["b"]);
    assert_eq!(status, " D A\n");
}

#[test]
fn a_kept_entry_is_settled_before_the_removals() {
    let f = Fixture::new(BIN, "both", "a");
    if !f.case_insensitive() {
        return;
    }
    f.run(&["mv", "a", "A"]);
    // Overlay mode: `a` comes back into the index beside `A`.
    f.run(&["checkout", "HEAD", "--", "."]);
    assert_eq!(f.run(&["ls-files"]).0, "A\na\nb\n");
    assert_eq!(f.run(&["reset", "-q", "--hard"]).2, 0);
    // `a` was up to date when `oneway_merge()` looked, so it is not written after
    // the removal of `A` took its file.
    assert_eq!(f.files(), ["b"]);
    assert_eq!(f.status(), " D a\n");
    if let Some(stock) = stock_git() {
        let steps: &[&[&str]] =
            &[&["mv", "a", "A"], &["checkout", "HEAD", "--", "."], &["reset", "-q", "--hard"]];
        assert_eq!(
            outcome(stock, "both", "a", steps),
            (vec![0, 0, 0], f.files(), f.status()),
            "zvcs must leave what stock leaves"
        );
    }
}
