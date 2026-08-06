//! `-m` per-parent records, the `-w` queue suppression, and the mailmap placeholders.
//!
//! `log_tree_commit()` under `--diff-merges=separate` renders a merge once per parent,
//! each record carrying its own ` (from <oid>)` insert; `diff_flush()` re-renders the
//! queue quietly under a whitespace rule and drops the pairs whose patch came out
//! empty — but the message/diff separator is decided *before* that, from the queue as
//! it stood. `%aN`/`%aE`/`%cN`/`%cE` resolve through `.mailmap` whether or not the
//! header formats do, and `--author`/`--committer` grep the mailmapped headers when a
//! mailmap is in effect.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-logsep-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            // This suite attributes commits to particular authors through config
            // so that `.mailmap` has something to rewrite. `GIT_AUTHOR_*` and
            // `GIT_COMMITTER_*` outrank config — including `-c user.name=…` — so
            // an environment that sets them (every CI runner here does) would
            // silently replace the identities the assertions are about.
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn text(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// A merge under `-m` prints one record per parent: its own header with `(from <oid>)`
/// and the diff against that parent.
#[test]
fn separate_merges_repeat_the_record_per_parent() {
    let f = Fixture::new("m");
    f.write("base.txt", b"base\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("side.txt", b"side\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("main.txt", b"main\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "main2"]);
    f.git(&["merge", "-q", "--no-ff", "side", "-m", "merge side"]);

    let raw = f.text(&["log", "-1", "-m", "--raw"]);
    assert_eq!(raw.matches("Merge: ").count(), 2, "one record per parent:\n{raw}");
    assert_eq!(raw.matches(" (from ").count(), 2, "each names its parent:\n{raw}");
    // Each record shows only what that parent is missing.
    assert!(raw.contains("A\tside.txt"), "{raw}");
    assert!(raw.contains("A\tmain.txt"), "{raw}");

    // `--oneline` abbreviates both ids and keeps the insert before the subject.
    let one = f.text(&["log", "-1", "-m", "--raw", "--oneline"]);
    assert_eq!(one.matches(" (from ").count(), 2, "{one}");
    assert!(one.contains(") merge side"), "{one}");

    // With no diff format asked for there is nothing to repeat.
    assert_eq!(f.text(&["log", "-1", "-m"]).matches("Merge: ").count(), 1);
}

/// Under `-w` a whitespace-only commit reports no files at all, but the record still
/// separates its message from the (empty) diff.
#[test]
fn whitespace_suppression_empties_the_name_and_stat_formats() {
    let f = Fixture::new("ws");
    f.write("f.txt", b"a\n\tb\nc\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "seed"]);
    f.write("f.txt", b"a\n    b\nc\n");
    f.git(&["commit", "-q", "-am", "reindent"]);

    for fmt in ["--stat", "--raw", "--name-only", "--numstat", "--shortstat"] {
        let out = f.text(&["log", "-1", "--format=", "-w", fmt]);
        assert!(out.trim().is_empty(), "{fmt} reports nothing under -w: {out:?}");
    }
    // The separator survives: the queue was non-empty when it was decided.
    let with_header = f.text(&["log", "-1", "-w", "--stat"]);
    assert!(with_header.ends_with("reindent\n\n"), "{with_header:?}");
    // Without `-w` the same commit does report its file.
    assert!(f.text(&["log", "-1", "--format=", "--stat"]).contains("f.txt"));
}

/// `%aN`/`%aE` map through `.mailmap` even under `--no-use-mailmap`, while `%an`/`%ae`
/// never do; `--author` greps the mapped header unless the mailmap is off.
#[test]
fn mailmap_placeholders_and_author_filter() {
    let f = Fixture::new("mm");
    f.write(
        ".mailmap",
        b"Proper Name <proper@e.co> Typo Name <typo@e.co>\n",
    );
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "seed"]);
    f.write("a.txt", b"a\n");
    f.git(&["add", "-A"]);
    f.git(&[
        "-c",
        "user.name=Typo Name",
        "-c",
        "user.email=typo@e.co",
        "commit",
        "-q",
        "-m",
        "by alias",
    ]);

    assert_eq!(f.text(&["log", "-1", "--format=%an|%aN"]), "Typo Name|Proper Name\n");
    assert_eq!(f.text(&["log", "-1", "--format=%ae|%aE"]), "typo@e.co|proper@e.co\n");
    // `--no-use-mailmap` turns off the header rewrite, not these placeholders.
    assert_eq!(
        f.text(&["log", "-1", "--no-use-mailmap", "--format=%aN"]),
        "Proper Name\n"
    );
    assert!(f.text(&["log", "-1", "--no-use-mailmap"]).contains("Author: Typo Name"));
    assert!(f.text(&["log", "-1"]).contains("Author: Proper Name"));

    // The author grep runs over the mapped header while the mailmap is in effect.
    assert!(f.text(&["log", "--author=Proper", "--oneline"]).contains("by alias"));
    assert!(f.text(&["log", "--author=Typo", "--oneline"]).trim().is_empty());
    assert!(f
        .text(&["log", "--no-use-mailmap", "--author=Typo", "--oneline"])
        .contains("by alias"));
}
