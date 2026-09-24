//! `read-tree -u` under `core.sparseCheckout`.
//!
//! ```c
//! if (!cfg->apply_sparse_checkout || !o->update)
//!         o->skip_sparse_checkout = 1;
//! ```
//!
//! (unpack-trees.c:1928-1929.) When the filter does apply, `unpack_trees()` runs
//! `mark_new_skip_worktree()` over the source index (:1974-1976) and again over
//! the entries the merge created (:2041-2043), then `apply_sparse_checkout()`
//! (:523-585) turns each crossing of the checkout boundary into worktree work:
//! `CE_WT_REMOVE` for a path that left it (:576), `CE_UPDATE` for one that
//! entered it (:582).
//!
//! The cases here are lifted from t1011-read-tree-sparse-checkout.sh and run
//! against both the single-tree path and the two-tree merge, which reach the
//! filter through different code in this port.
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
    /// Two commits over `init.t`, `sub/added`, `sub/addedtoo` and `subsub/added`,
    /// with the worktree fully populated at the tip.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rtsparse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::create_dir_all(work.join("subsub")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.work.join("init.t"), b"init\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("init.t"), b"init\nmore\n").unwrap();
        for p in ["sub/added", "sub/addedtoo", "subsub/added"] {
            std::fs::write(f.work.join(p), b"").unwrap();
        }
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "two"]);
        std::fs::create_dir_all(f.work.join(".git/info")).unwrap();
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn enable_sparse(&self, patterns: &str) {
        self.git(&["config", "core.sparsecheckout", "true"]);
        std::fs::write(self.work.join(".git/info/sparse-checkout"), patterns).unwrap();
    }

    /// `ls-files -t`, which tags a skip-worktree entry `S` and a plain one `H`.
    fn tags(&self) -> String {
        self.stdout(&["ls-files", "-t"])
    }

    fn exists(&self, p: &str) -> bool {
        self.work.join(p).exists()
    }

    fn tree(&self, rev: &str) -> String {
        self.stdout(&["rev-parse", &format!("{rev}^{{tree}}")]).trim().to_owned()
    }
}

const ALL_IN: &str = "H init.t\nH sub/added\nH sub/addedtoo\nH subsub/added\n";
const ONLY_SUB: &str = "S init.t\nH sub/added\nH sub/addedtoo\nS subsub/added\n";

/// An empty pattern list takes nothing in, so every entry leaves the checkout
/// area and its worktree file is unlinked.
#[test]
fn empty_pattern_list_sparsifies_everything() {
    let f = Fixture::new("empty");
    f.enable_sparse("\n");

    f.git(&["read-tree", "-m", "-u", "HEAD"]);

    assert_eq!(f.tags(), "S init.t\nS sub/added\nS sub/addedtoo\nS subsub/added\n");
    assert!(!f.exists("init.t"), "CE_WT_REMOVE must have unlinked it");
    assert!(!f.exists("sub/added"));
}

/// A directory pattern decides everything beneath it, with or without the
/// trailing slash, and the paths it leaves out are unlinked.
#[test]
fn directory_pattern_narrows_the_checkout_area() {
    for pattern in ["sub/\n", "sub\n"] {
        let f = Fixture::new("dir");
        f.enable_sparse(pattern);

        f.git(&["read-tree", "-m", "-u", "HEAD"]);

        assert_eq!(f.tags(), ONLY_SUB, "pattern {pattern:?}");
        assert!(f.exists("sub/added"), "pattern {pattern:?}");
        assert!(!f.exists("init.t"), "pattern {pattern:?}");
        assert!(!f.exists("subsub/added"), "pattern {pattern:?}");
    }
}

/// A later negative pattern overrides an earlier positive one, per
/// `last_matching_pattern_from_list()`.
#[test]
fn negated_pattern_carves_a_path_back_out() {
    let f = Fixture::new("neg");
    f.enable_sparse("sub\n!sub/added\n");

    f.git(&["read-tree", "-m", "-u", "HEAD"]);

    assert_eq!(f.tags(), "S init.t\nS sub/added\nH sub/addedtoo\nS subsub/added\n");
    assert!(!f.exists("sub/added"));
    assert!(f.exists("sub/addedtoo"));
}

/// Widening the area again is the other half of `apply_sparse_checkout()`: the
/// entry loses `CE_SKIP_WORKTREE` and gains `CE_UPDATE`, so the file comes back.
#[test]
fn widening_the_area_writes_the_files_back() {
    let f = Fixture::new("widen");
    f.enable_sparse("sub/\n");
    f.git(&["read-tree", "-m", "-u", "HEAD"]);
    assert!(!f.exists("init.t"));

    f.enable_sparse("/*\n");
    f.git(&["read-tree", "-m", "-u", "HEAD"]);

    assert_eq!(f.tags(), ALL_IN);
    assert!(f.exists("init.t"), "CE_UPDATE must have restored it");
    assert!(f.exists("subsub/added"));
    assert_eq!(f.stdout(&["status", "--porcelain"]), "");
}

/// `!o->update`: without `-u` the filter is never consulted, however sparse the
/// worktree is configured to be.
#[test]
fn without_update_the_filter_never_runs() {
    let f = Fixture::new("noupdate");
    f.enable_sparse("sub/\n");

    f.git(&["read-tree", "-m", "HEAD"]);

    assert_eq!(f.tags(), ALL_IN);
    assert!(f.exists("init.t"));
}

/// `--no-sparse-checkout` sets `o->skip_sparse_checkout` directly, and
/// `--sparse-checkout` is the same `OPT_BOOL` unset.
#[test]
fn no_sparse_checkout_opts_out_and_back_in() {
    let f = Fixture::new("optout");
    f.enable_sparse("sub/\n");

    f.git(&["read-tree", "--no-sparse-checkout", "-m", "-u", "HEAD"]);
    assert_eq!(f.tags(), ALL_IN);
    assert!(f.exists("init.t"));

    f.git(&["read-tree", "--no-sparse-checkout", "--sparse-checkout", "-m", "-u", "HEAD"]);
    assert_eq!(f.tags(), ONLY_SUB);
    assert!(!f.exists("init.t"));
}

/// The two-tree merge reaches the filter through a different path in this port
/// than the single-tree read, so it gets its own case.
#[test]
fn two_tree_merge_applies_the_filter() {
    let f = Fixture::new("twotree");
    let one = f.tree("HEAD^");
    let two = f.tree("HEAD");
    f.enable_sparse("sub/\n");

    f.git(&["read-tree", "-m", "-u", &one, &two]);

    assert_eq!(f.tags(), ONLY_SUB);
    assert!(f.exists("sub/added"));
    assert!(!f.exists("init.t"));
    assert!(!f.exists("subsub/added"));
}

/// `merged_entry()` migrates `CE_SKIP_WORKTREE` across a two-tree merge
/// (unpack-trees.c:2606, :2614) even when no pattern file is in play — losing it
/// would make `git status` report every excluded path as deleted.
#[test]
fn two_tree_merge_carries_skip_worktree_without_any_patterns() {
    let f = Fixture::new("carry");
    let one = f.tree("HEAD^");
    let two = f.tree("HEAD");
    f.git(&["update-index", "--skip-worktree", "subsub/added"]);

    f.git(&["read-tree", "-m", &one, &two]);

    assert_eq!(f.tags(), "H init.t\nH sub/added\nH sub/addedtoo\nS subsub/added\n");
    assert_eq!(f.stdout(&["status", "--porcelain"]), "");
}

/// No `info/sparse-checkout` at all is not an empty pattern list: the failed
/// `open()` makes `get_sparse_checkout_patterns()` return -1, and
/// `populate_from_existing_patterns()` (unpack-trees.c:1829-1836) answers that with
/// `o->skip_sparse_checkout = 1`. Nothing is sparsified, and the bits an earlier
/// sparse read left behind are carried rather than recomputed.
#[test]
fn missing_pattern_file_skips_the_filter() {
    let f = Fixture::new("nofile");
    f.git(&["config", "core.sparsecheckout", "true"]);

    f.git(&["read-tree", "-m", "-u", "HEAD"]);
    assert_eq!(f.tags(), ALL_IN);
    assert!(f.exists("init.t"));

    f.enable_sparse("sub/\n");
    f.git(&["read-tree", "-m", "-u", "HEAD"]);
    std::fs::remove_file(f.work.join(".git/info/sparse-checkout")).unwrap();
    f.git(&["read-tree", "-m", "-u", "HEAD"]);
    assert_eq!(f.tags(), ONLY_SUB);
    assert!(!f.exists("init.t"), "a skipped filter must not write the file back");
}
