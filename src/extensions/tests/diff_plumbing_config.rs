//! The `diff-tree` and `diff-index` plumbing commands and the diff.* patch-config
//! keys `diff.rs` honors (`diff.context`, `diff.noPrefix`, `diff.srcPrefix`,
//! `diff.dstPrefix`, `diff.algorithm`).
//!
//! `diff-tree` does not render a unified patch: `-p`/`-u`/`--patch` is a documented,
//! deliberate limitation and it refuses the format rather than emitting approximate
//! bytes. `diff-index` *does* render the patch and stat families for real, ported from
//! the byte-identical `diff-files` machinery. The raw `:<mode>…<status>\t<path>`
//! listing, `--name-only` and `--name-status` are invariant under the five patch-only
//! config keys in stock git for both commands, because those keys are diff *UI* config
//! that plumbing does not read — there is no rendered behavior for them to attach to.
//!
//! These tests lock that faithfulness in:
//!   * setting each of the five keys leaves the raw / name-status output of both
//!     commands byte-identical (a future change that wrongly wired patch-config into a
//!     plumbing format, breaking git byte-compat, would trip here);
//!   * `diff-tree -p` keeps refusing cleanly even with the keys set, so a half-built
//!     patch path can never leak config-shaped but wrong bytes; and
//!   * `diff-index`'s newly ported `-p`/`--stat`/`--numstat`/`--shortstat`/`--summary`
//!     output is byte-for-byte identical to stock git.
//!
//! Setup uses stock `git`; the command under test is the zvcs shadow binary
//! (`CARGO_BIN_EXE_git`), matching the sibling `diff_config.rs` convention.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The five patch-only diff.* keys `diff.rs` honors, each with a value distinct
/// from git's built-in default so a leak into a plumbing format would be visible.
const PATCH_KEYS: &[(&str, &str)] = &[
    ("diff.context", "5"),
    ("diff.noPrefix", "true"),
    ("diff.srcPrefix", "OLD/"),
    ("diff.dstPrefix", "NEW/"),
    ("diff.algorithm", "histogram"),
];

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo with two commits touching one 8-line file (a middle line changed), plus a
/// second added file, so both `diff-tree` (commit-to-commit) and `diff-index`
/// (tree-to-index) have real modification and addition records to emit.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-diffplumb-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("f"), "l1\nl2\nl3\nl4\nCHANGED\nl6\nl7\nl8\n").unwrap();
    std::fs::write(repo.join("g"), "new\nfile\n").unwrap();
    git(&repo, &["add", "f", "g"]);
    git(&repo, &["commit", "-q", "-m", "c1"]);
    (repo, home)
}

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn stdout_of(o: &Output) -> Vec<u8> {
    assert!(
        o.status.success(),
        "command failed (rc={:?}); stderr:\n{}",
        o.status.code(),
        String::from_utf8_lossy(&o.stderr)
    );
    o.stdout.clone()
}

fn set_all_keys(repo: &Path) {
    for (k, v) in PATCH_KEYS {
        git(repo, &["config", k, v]);
    }
}

/// Every non-patch format `diff-tree` renders is byte-identical before and after the
/// five patch-only keys are set — they have no attach point in the raw/name/numstat
/// output, exactly as in stock git.
#[test]
fn diff_tree_formats_invariant_under_patch_config() {
    let (repo, home) = fixture("tree");
    for fmt in [
        &["diff-tree", "-r", "HEAD~", "HEAD"][..],
        &["diff-tree", "-r", "--name-only", "HEAD~", "HEAD"][..],
        &["diff-tree", "-r", "--name-status", "HEAD~", "HEAD"][..],
        &["diff-tree", "--numstat", "HEAD~", "HEAD"][..],
        &["diff-tree", "--shortstat", "HEAD~", "HEAD"][..],
    ] {
        let before = stdout_of(&run(&repo, &home, fmt));
        assert!(!before.is_empty(), "fixture must produce output for {fmt:?}");
        set_all_keys(&repo);
        let after = stdout_of(&run(&repo, &home, fmt));
        for (k, _) in PATCH_KEYS {
            git(&repo, &["config", "--unset", k]);
        }
        assert_eq!(
            before,
            after,
            "diff-tree {fmt:?} must be byte-identical with and without patch-only diff.* config;\n\
             before:\n{}\nafter:\n{}",
            String::from_utf8_lossy(&before),
            String::from_utf8_lossy(&after),
        );
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// Every non-patch format `diff-index` renders is byte-identical before and after the
/// five keys are set.
#[test]
fn diff_index_formats_invariant_under_patch_config() {
    let (repo, home) = fixture("index");
    // Re-modify the worktree so `diff-index HEAD` has a live modification to report.
    std::fs::write(repo.join("f"), "l1\nl2\nCHANGED\nl4\nl5\nl6\nl7\nl8\n").unwrap();
    for fmt in [
        &["diff-index", "HEAD"][..],
        &["diff-index", "--name-only", "HEAD"][..],
        &["diff-index", "--name-status", "HEAD"][..],
        &["diff-index", "--cached", "HEAD~"][..],
    ] {
        let before = stdout_of(&run(&repo, &home, fmt));
        assert!(!before.is_empty(), "fixture must produce output for {fmt:?}");
        set_all_keys(&repo);
        let after = stdout_of(&run(&repo, &home, fmt));
        for (k, _) in PATCH_KEYS {
            git(&repo, &["config", "--unset", k]);
        }
        assert_eq!(
            before,
            after,
            "diff-index {fmt:?} must be byte-identical with and without patch-only diff.* config;\n\
             before:\n{}\nafter:\n{}",
            String::from_utf8_lossy(&before),
            String::from_utf8_lossy(&after),
        );
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `diff-tree --numstat` add/delete counts do not change across `diff.algorithm`
/// values. The counts come from git's shortest-edit-script and every built-in
/// algorithm yields the same totals for these inputs, so the hardcoded-Myers numstat
/// stays byte-compatible with git regardless of the configured algorithm. A future
/// change that made numstat honor `diff.algorithm` and diverge would trip this.
#[test]
fn diff_tree_numstat_stable_across_algorithm_config() {
    let (repo, home) = fixture("numstat");
    let baseline = stdout_of(&run(&repo, &home, &["diff-tree", "--numstat", "HEAD~", "HEAD"]));
    for algo in ["myers", "minimal", "histogram"] {
        git(&repo, &["config", "diff.algorithm", algo]);
        let got = stdout_of(&run(&repo, &home, &["diff-tree", "--numstat", "HEAD~", "HEAD"]));
        git(&repo, &["config", "--unset", "diff.algorithm"]);
        assert_eq!(
            baseline,
            got,
            "diff-tree --numstat must be identical under diff.algorithm={algo}"
        );
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `diff-tree -p` is refused (no patch rendering), and setting the patch-only keys
/// does not change that — no config path can smuggle out a partial/wrong patch.
#[test]
fn diff_tree_patch_refused_with_and_without_config() {
    let (repo, home) = fixture("treep");
    for setup in [false, true] {
        if setup {
            set_all_keys(&repo);
        }
        let o = run(&repo, &home, &["diff-tree", "-p", "HEAD~", "HEAD"]);
        assert!(
            !o.status.success() && o.stdout.is_empty(),
            "diff-tree -p must refuse with no stdout (config set={setup}); \
             rc={:?} stdout={:?}",
            o.status.code(),
            String::from_utf8_lossy(&o.stdout),
        );
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `diff-index`'s ported content formats — the unified patch and the stat family —
/// are byte-for-byte identical to stock `git diff-index`, for both the worktree
/// (`HEAD`) and the `--cached` comparison. This locks the port to git's own bytes: a
/// regression in the hunk framing, the `diff --git`/`index` header, the diffstat graph
/// or the summary would trip here. No patch-only config is set, so both engines use
/// git's defaults (context 3, `a/`/`b/` prefixes, Myers) — plumbing does not read the
/// diff UI keys anyway.
#[test]
fn diff_index_content_formats_match_git() {
    let (repo, home) = fixture("indexp");
    // A live worktree modification (two hunks: a line added early, one changed late) so
    // `diff-index HEAD` has real content to render.
    std::fs::write(repo.join("f"), "l1\nl2\nCHANGED\nl4\nl5\nl6\nl7\nl8\n").unwrap();
    for args in [
        &["diff-index", "-p", "HEAD"][..],
        &["diff-index", "--patch", "HEAD"][..],
        &["diff-index", "--stat", "HEAD"][..],
        &["diff-index", "--numstat", "HEAD"][..],
        &["diff-index", "--shortstat", "HEAD"][..],
        &["diff-index", "--summary", "HEAD"][..],
        &["diff-index", "--compact-summary", "HEAD"][..],
        &["diff-index", "--patch-with-stat", "HEAD"][..],
        &["diff-index", "-p", "--cached", "HEAD~"][..],
        &["diff-index", "--numstat", "--cached", "HEAD~"][..],
        &["diff-index", "--summary", "--cached", "HEAD~"][..],
    ] {
        let ours = stdout_of(&run(&repo, &home, args));
        let theirs = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(theirs.status.success(), "git {args:?} failed");
        assert_eq!(
            ours,
            theirs.stdout,
            "diff-index {args:?} must be byte-identical to git;\nours:\n{}\ngit:\n{}",
            String::from_utf8_lossy(&ours),
            String::from_utf8_lossy(&theirs.stdout),
        );
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
