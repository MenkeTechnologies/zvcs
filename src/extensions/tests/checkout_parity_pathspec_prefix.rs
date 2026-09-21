//! `git checkout`'s pathspecs are relative to the directory the command was run
//! in, and the index it writes stays sorted.
//!
//! **Prefix.** `parse_pathspec()` prepends git's `prefix` — the cwd expressed
//! relative to the worktree root — to every non-magic element (`prefix_path()`,
//! setup.c), so inside `sub/` the spec `s.txt` names `sub/s.txt` and a bare `.`
//! names only what lies under `sub/`. The port matched the spec text against
//! whole-index paths with no prefix at all, so from a subdirectory
//! `git checkout <tree> -- s.txt` was `error: pathspec 's.txt' did not match any
//! file(s) known to git` (exit 1) while `git checkout <tree> -- .` silently
//! widened to the entire worktree and rewrote files outside the directory the user
//! was standing in. `git restore` was already correct, which is what made the two
//! verbs disagree on the same spec. A spec whose `..`s climb past the worktree root
//! is `prefix_path_gently()` failing:
//! `fatal: <spec>: '<spec>' is outside repository at '<wd>'`, exit 128.
//!
//! **Sorted index.** `update_some()` ends in
//! `add_index_entry(the_repository->index, ce, ADD_CACHE_OK_TO_ADD | ADD_CACHE_OK_TO_REPLACE)`
//! (builtin/checkout.c:231-232), which keeps the index ordered at every step. The
//! port appended new entries and only sorted at the end of the loop, while looking
//! each path up with a binary search — so the first appended path desynced the
//! search and a later path was reported missing and appended a second time. The
//! result was two stage-0 entries for one path in a written index, which stock git
//! then reads back as an added-but-not-in-HEAD path. `git checkout <branch> -- .`
//! in a repo where one path is new in the source tree and another lives in a
//! subdirectory was enough to hit it.
//!
//! Every expectation below was measured against stock git 2.55.0 first.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    /// Scratch root holding `repo/` plus a `home/` kept outside the worktree, so
    /// zvcs's own state files never land among the files under test.
    base: PathBuf,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fixture {
    /// `a.txt`, `sub/s.txt`, `sub/deep/d.txt` on `main`; `other` changes all three
    /// and adds `fresh.txt` at the root.
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("zvcs-copfx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repo");
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let f = Fixture { base, root };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("a.txt", "a\n");
        f.write("sub/s.txt", "s\n");
        f.write("sub/deep/d.txt", "d\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["checkout", "-q", "-b", "other"]);
        f.write("a.txt", "a2\n");
        f.write("sub/s.txt", "s2\n");
        f.write("sub/deep/d.txt", "d2\n");
        f.write("fresh.txt", "fresh\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "other"]);
        f.git(&["checkout", "-q", "main"]);
        f
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    /// Run in the worktree root.
    fn git(&self, args: &[&str]) -> (String, i32) {
        self.git_in(".", args)
    }

    /// Run with the cwd set to `rel` inside the worktree — what supplies git's
    /// `prefix`.
    fn git_in(&self, rel: &str, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .arg("-C")
            .arg(self.root.join(rel))
            .args(args)
            .env("HOME", self.base.join("home"))
            .env("ZVCS_HOME", self.base.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
            .output()
            .unwrap();
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        (s, out.status.code().unwrap_or(-1))
    }

    /// Index paths, in index order, with duplicates preserved.
    fn index_paths(&self) -> Vec<String> {
        let (out, _) = self.git(&["ls-files", "--stage"]);
        out.lines()
            .filter_map(|l| l.split_once('\t').map(|(_, p)| p.to_string()))
            .collect()
    }
}

#[test]
fn a_bare_pathspec_from_a_subdirectory_names_that_subdirectory() {
    let f = Fixture::new("bare");
    let (out, rc) = f.git_in("sub", &["checkout", "other", "--", "s.txt"]);
    assert_eq!(rc, 0, "`s.txt` inside sub/ must name sub/s.txt, got: {out}");
    assert_eq!(f.read("sub/s.txt"), "s2\n", "sub/s.txt must come from `other`");
    assert_eq!(f.read("a.txt"), "a\n", "the root file must be untouched: {out}");

    // Two levels deep, and a directory spec relative to the cwd.
    let (out, rc) = f.git_in("sub", &["checkout", "other", "--", "deep"]);
    assert_eq!(rc, 0, "`deep` inside sub/ must name sub/deep, got: {out}");
    assert_eq!(f.read("sub/deep/d.txt"), "d2\n", "sub/deep/d.txt must come from `other`");
}

#[test]
fn a_dot_pathspec_from_a_subdirectory_does_not_widen_to_the_worktree() {
    let f = Fixture::new("dot");
    let (out, rc) = f.git_in("sub", &["checkout", "other", "--", "."]);
    assert_eq!(rc, 0, "`.` inside sub/ should succeed, got: {out}");
    assert_eq!(f.read("sub/s.txt"), "s2\n", "everything under sub/ is in scope");
    assert_eq!(f.read("sub/deep/d.txt"), "d2\n", "including deeper paths");
    assert_eq!(
        f.read("a.txt"),
        "a\n",
        "`.` must stay scoped to the prefix and leave the root file alone: {out}"
    );
    assert!(
        !f.root.join("fresh.txt").exists(),
        "a path the source adds outside the prefix must not be checked out: {out}"
    );
}

#[test]
fn dotdot_climbs_back_out_and_past_the_root_is_fatal() {
    let f = Fixture::new("up");
    let (out, rc) = f.git_in("sub", &["checkout", "other", "--", "../a.txt"]);
    assert_eq!(rc, 0, "`../a.txt` must reach the root file, got: {out}");
    assert_eq!(f.read("a.txt"), "a2\n", "../a.txt must come from `other`");
    assert_eq!(f.read("sub/s.txt"), "s\n", "and nothing under sub/ moves");

    // `prefix_path_gently()` refuses a spec that leaves the worktree.
    let (out, rc) = f.git_in("sub", &["checkout", "other", "--", "../../escape"]);
    assert_eq!(rc, 128, "escaping the worktree is a fatal, got: {out}");
    assert!(
        out.starts_with("fatal: ../../escape: '../../escape' is outside repository at '"),
        "stock names the spec twice and then the worktree, got: {out:?}"
    );
}

#[test]
fn a_nonmatching_spec_is_reported_as_the_user_spelled_it() {
    let f = Fixture::new("miss");
    // `report_path_error()` prints `pathspec_item.original`, not the prefixed form.
    let (out, rc) = f.git_in("sub", &["checkout", "other", "--", "nosuch.txt"]);
    assert_eq!(rc, 1, "a spec matching nothing exits 1, got: {out}");
    assert_eq!(
        out, "error: pathspec 'nosuch.txt' did not match any file(s) known to git\n",
        "the message must carry the spec as typed, unprefixed: {out:?}"
    );
}

#[test]
fn the_index_form_and_the_conflict_stages_honour_the_prefix_too() {
    let f = Fixture::new("index");
    // `git checkout -- <path>` (from the index) and `git checkout --ours <path>`
    // go through different entry points than the tree form above; all three parse
    // their pathspec the same way.
    f.write("sub/s.txt", "DIRTY\n");
    let (out, rc) = f.git_in("sub", &["checkout", "--", "s.txt"]);
    assert_eq!(rc, 0, "index-form checkout inside sub/ should succeed, got: {out}");
    assert_eq!(f.read("sub/s.txt"), "s\n", "the index copy must be restored");

    // `--ours` on an unconflicted path falls back to its stage-0 blob, so this
    // only proves the spec resolved; a miss would be exit 1.
    let (out, rc) = f.git_in("sub", &["checkout", "--ours", "s.txt"]);
    assert_eq!(rc, 0, "--ours inside sub/ should resolve the spec, got: {out}");
}

#[test]
fn a_tree_checkout_that_adds_and_updates_leaves_no_duplicate_index_entry() {
    let f = Fixture::new("dup");
    // `fresh.txt` is new in `other` and sorts before `sub/s.txt`, so writing it
    // appends an out-of-order entry that a binary search for `sub/s.txt` must not
    // be allowed to miss.
    let (out, rc) = f.git(&["checkout", "other", "--", "."]);
    assert_eq!(rc, 0, "checkout other -- . should succeed, got: {out}");

    let paths = f.index_paths();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "add_index_entry() keeps the index sorted: {paths:?}");

    let mut deduped = paths.clone();
    deduped.dedup();
    assert_eq!(
        paths, deduped,
        "no path may appear twice at stage 0: {paths:?}"
    );
    assert_eq!(
        paths,
        vec!["a.txt", "fresh.txt", "sub/deep/d.txt", "sub/s.txt"],
        "the checked-out tree's paths, each exactly once: {out}"
    );

    // A duplicate surfaces in a stock read of the written index as a *second*
    // `sub/s.txt` line, reported added-but-not-in-HEAD (`1 A.`) next to the real
    // modification (`1 M.`).
    let (status, _) = f.git(&["status", "--porcelain=v2"]);
    let lines: Vec<&str> = status.lines().filter(|l| l.ends_with(" sub/s.txt")).collect();
    assert_eq!(lines.len(), 1, "sub/s.txt must be reported exactly once: {status:?}");
    assert!(
        lines[0].starts_with("1 M. "),
        "its one line is the staged modification, never an added duplicate: {:?}",
        lines[0]
    );
}
