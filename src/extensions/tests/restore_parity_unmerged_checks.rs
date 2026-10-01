//! `checkout_paths()` judges every matched unmerged path before anything is
//! written, whatever the restore mode.
//!
//! ```c
//! if (ce->ce_flags & CE_MATCHED) {
//!         if (!ce_stage(ce))
//!                 continue;
//!         if (opts->ignore_unmerged) {
//!                 if (!opts->quiet)
//!                         warning(_("path '%s' is unmerged"), ce->name);
//!         } else if (opts->writeout_stage) {
//!                 errs |= check_stage(opts->writeout_stage, ce, pos, opts->overlay_mode);
//!         } else if (opts->merge) {
//!                 errs |= check_stages((1<<2) | (1<<3), ce, pos);
//!         } else {
//!                 errs = 1;
//!                 error(_("path '%s' is unmerged"), ce->name);
//!         }
//! ```
//!
//! (builtin/checkout.c:662-682.) `restore` ran that check only for the plain
//! worktree-from-index form, so `--merge` on a modify/delete conflict wrote a
//! conflict against an empty side instead of `does not have all necessary
//! versions`, `--overlay --theirs` on a path without stage 3 deleted it instead of
//! `does not have their version`, `--source=<tree> --staged` silently dropped an
//! unmerged path the tree lacks instead of refusing it, and `--ours
//! --ignore-unmerged` lost its warning.
//!
//! Every expectation below was measured against stock git 2.56.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// The index of a stopped `main` <- `side` merge: `f` both-modified, `del`
/// modified on main / deleted on side (stages 1+2), `snew` added on both (2+3).
const CONFLICTED_INDEX: &str = "\
100644 4bcfe98e640c8284511312660fb8709b0afa888e 1\tdel
100644 5e8fb3bdb3823b1ee0420f98cccf3cdb5db15ab0 2\tdel
100644 01e79c32a8c99c557f0757da7cb6d65b3414466d 1\tf
100644 e95635a8eae8594d2b02938195eb45a8e170a8cd 2\tf
100644 ace2f8e8075fe2f8899330e798ae0f222e1765c1 3\tf
100644 b68fde2a051d9af2fe3ff4c96c0898e5a3212e4d 0\tkeep
100644 56f8877a9350939b0efdc8eb8b45b5374fba9e37 2\tsnew
100644 7202bf1a5548b5f00249a5fe645e7a25246c36fa 3\tsnew
";

const F_CONFLICT: &str = "1\n<<<<<<< HEAD\nM\n=======\nS\n>>>>>>> side\n3\n";

impl Fixture {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("zvcs-rumc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let f = Fixture { base, root };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("f", "1\n2\n3\n");
        f.write("keep", "k\n");
        f.write("del", "d\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["checkout", "-qb", "side"]);
        f.write("f", "1\nS\n3\n");
        f.git(&["rm", "-q", "del"]);
        f.write("snew", "sn\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "side"]);
        f.git(&["checkout", "-q", "main"]);
        f.write("f", "1\nM\n3\n");
        f.write("del", "md\n");
        f.write("snew", "mn\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "main"]);
        f.git(&["merge", "-q", "side"]);
        assert_eq!(f.index(), CONFLICTED_INDEX, "fixture merge must stop with these stages");
        f
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(rel)).ok()
    }

    fn index(&self) -> String {
        self.git(&["ls-files", "-s"]).0
    }

    fn git(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .arg("-C")
            .arg(&self.root)
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

    /// The refusal left every stage and every worktree file as the merge did.
    fn assert_untouched(&self, ctx: &str) {
        assert_eq!(self.index(), CONFLICTED_INDEX, "{ctx}: index must be untouched");
        assert_eq!(self.read("f").as_deref(), Some(F_CONFLICT), "{ctx}: f must be untouched");
        assert_eq!(self.read("del").as_deref(), Some("md\n"), "{ctx}: del must be untouched");
    }
}

#[test]
fn merge_needs_both_sides_and_refuses_before_writing_anything() {
    let f = Fixture::new("merge-del");
    let (out, rc) = f.git(&["restore", "--merge", "del"]);
    assert_eq!(rc, 1, "got: {out}");
    assert_eq!(out, "error: path 'del' does not have all necessary versions\n");
    f.assert_untouched("--merge del");

    // One bad path stops the others too: f is not re-merged in zdiff3 style.
    let f = Fixture::new("zdiff3-all");
    let (out, rc) = f.git(&["restore", "--conflict=zdiff3", "."]);
    assert_eq!(rc, 1, "got: {out}");
    assert_eq!(out, "error: path 'del' does not have all necessary versions\n");
    f.assert_untouched("--conflict=zdiff3 .");
}

#[test]
fn a_missing_side_is_an_error_only_in_overlay_mode() {
    let f = Fixture::new("overlay-theirs");
    let (out, rc) = f.git(&["restore", "--overlay", "--theirs", "del"]);
    assert_eq!(rc, 1, "got: {out}");
    assert_eq!(out, "error: path 'del' does not have their version\n");
    f.assert_untouched("--overlay --theirs del");

    // No-overlay: `checkout_stage()` unlinks the path instead; the index keeps
    // its stages because only the worktree was restored.
    let f = Fixture::new("theirs");
    let (out, rc) = f.git(&["restore", "--theirs", "del"]);
    assert_eq!((out.as_str(), rc), ("", 0));
    assert_eq!(f.read("del"), None, "del has no stage 3, so it is removed");
    assert_eq!(f.index(), CONFLICTED_INDEX);
}

#[test]
fn a_source_tree_without_the_unmerged_path_is_refused() {
    let f = Fixture::new("source-staged");
    let (out, rc) = f.git(&["restore", "--source=side", "--staged", "."]);
    assert_eq!(rc, 1, "got: {out}");
    assert_eq!(out, "error: path 'del' is unmerged\n", "f and snew are in side, so they resolve");
    f.assert_untouched("--source=side --staged .");
}

#[test]
fn ignore_unmerged_warns_unless_quiet_and_still_writes_the_chosen_stage() {
    let f = Fixture::new("ours-ignore");
    let (out, rc) = f.git(&["restore", "--ours", "--ignore-unmerged", "f"]);
    assert_eq!(rc, 0, "got: {out}");
    assert_eq!(out, "warning: path 'f' is unmerged\n");
    assert_eq!(f.read("f").as_deref(), Some("1\nM\n3\n"), "stage 2 is still checked out");
    assert_eq!(f.index(), CONFLICTED_INDEX);

    let f = Fixture::new("quiet");
    let (out, rc) = f.git(&["restore", "-q", "--ignore-unmerged", "."]);
    assert_eq!((out.as_str(), rc), ("", 0), "--quiet silences the warnings");
    f.assert_untouched("-q --ignore-unmerged .");

    let f = Fixture::new("noquiet");
    let (out, rc) = f.git(&["restore", "--quiet", "--no-quiet", "--ignore-unmerged", "f"]);
    assert_eq!((out.as_str(), rc), ("warning: path 'f' is unmerged\n", 0));
}
