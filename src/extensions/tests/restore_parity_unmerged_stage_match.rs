//! An unmerged path is matched *per index entry*, and what happens to it follows
//! from which of its stages matched.
//!
//! `do_match_pathspec()` skips a wildcard-free item that has already matched
//! exactly (dir.c:552-554), exclusions included. An unmerged path's stages are
//! consecutive entries sharing one `ps_matched`, so the first stage spends a literal
//! item naming it: with `del` alone the later stages are not `CE_MATCHED`, and with
//! `. ':!f'` the exclusion is spent on `f`'s first stage and its later stages are.
//! `checkout_paths()` then works from the first matched entry forward
//! (builtin/checkout.c:257-345, 662-682), and a no-overlay `--source` that lacks the
//! path marks only the matched entries `CE_REMOVE | CE_WT_REMOVE`, while
//! `checkout_worktree()` never unlinks for a stage above 0 (builtin/checkout.c:466-470).
//!
//! `restore` matched whole paths, unlinked such a path from the worktree, dropped
//! every stage from the index (overlay mode included), and treated an excluded
//! conflicted path as untouched.
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

const SIDE_RESOLVED: &str = "\
100644 ace2f8e8075fe2f8899330e798ae0f222e1765c1 0\tf
100644 b68fde2a051d9af2fe3ff4c96c0898e5a3212e4d 0\tkeep
100644 7202bf1a5548b5f00249a5fe645e7a25246c36fa 0\tsnew
";

#[test]
fn a_source_without_the_path_never_unlinks_it_from_the_worktree() {
    let f = Fixture::new("wt");
    let (out, rc) = f.git(&["restore", "--source=side", "--ignore-unmerged", "."]);
    assert_eq!((out.as_str(), rc), ("warning: path 'del' is unmerged\n", 0));
    assert_eq!(f.read("del").as_deref(), Some("md\n"), "CE_WT_REMOVE never reaches a stage-2 entry");
    assert_eq!(f.index(), CONFLICTED_INDEX, "a worktree-only restore from a tree writes no index");
    assert_eq!(f.read("f").as_deref(), Some("1\nS\n3\n"), "f is in side and is checked out");
}

#[test]
fn only_the_matched_stages_leave_the_index() {
    // `.` matches recursively, never exactly: every stage of del goes.
    let f = Fixture::new("dot");
    let (out, rc) = f.git(&["restore", "--source=side", "--staged", "--ignore-unmerged", "."]);
    assert_eq!((out.as_str(), rc), ("warning: path 'del' is unmerged\n", 0));
    assert_eq!(f.index(), SIDE_RESOLVED);
    assert_eq!(f.read("del").as_deref(), Some("md\n"));

    // A literal `del` is spent on stage 1; stage 2 stays.
    let f = Fixture::new("literal");
    let (out, rc) = f.git(&["restore", "--source=side", "--staged", "--ignore-unmerged", "del"]);
    assert_eq!((out.as_str(), rc), ("warning: path 'del' is unmerged\n", 0));
    assert_eq!(
        f.index(),
        CONFLICTED_INDEX.replace("100644 4bcfe98e640c8284511312660fb8709b0afa888e 1\tdel\n", "")
    );

    // A glob is never spent, so it picks stage 2 up.
    let f = Fixture::new("glob");
    let (out, rc) =
        f.git(&["restore", "--source=side", "--staged", "--ignore-unmerged", "del", "de*"]);
    assert_eq!((out.as_str(), rc), ("warning: path 'del' is unmerged\n", 0));
    assert!(!f.index().contains("\tdel\n"), "both stages go: {}", f.index());
}

#[test]
fn a_spent_exclusion_lets_the_later_stages_match() {
    // `:!f` keeps f out of the tree read, excludes its stage 1, and is spent:
    // stages 2 and 3 match, are warned about and removed.
    let f = Fixture::new("excl-staged");
    let (out, rc) = f.git(&["restore", "--source=side", "--staged", "--ignore-unmerged", ":!f"]);
    assert_eq!(rc, 0, "got: {out}");
    assert_eq!(out, "warning: path 'del' is unmerged\nwarning: path 'f' is unmerged\n");
    assert_eq!(
        f.index(),
        "100644 01e79c32a8c99c557f0757da7cb6d65b3414466d 1\tf\n\
         100644 b68fde2a051d9af2fe3ff4c96c0898e5a3212e4d 0\tkeep\n\
         100644 7202bf1a5548b5f00249a5fe645e7a25246c36fa 0\tsnew\n"
    );

    let f = Fixture::new("excl-plain");
    let (out, rc) = f.git(&["restore", ".", ":!f"]);
    assert_eq!(rc, 1, "got: {out}");
    assert_eq!(
        out,
        "error: path 'del' is unmerged\nerror: path 'f' is unmerged\nerror: path 'snew' is unmerged\n"
    );
    f.assert_untouched("restore . :!f");

    let f = Fixture::new("excl-theirs");
    let (out, rc) = f.git(&["restore", "--theirs", ".", ":!f"]);
    assert_eq!((out.as_str(), rc), ("", 0));
    assert_eq!(f.read("f").as_deref(), Some("1\nS\n3\n"), "f's stage 3 matched after all");
}

#[test]
fn a_stage_before_the_first_matched_entry_does_not_exist() {
    // snew is stages 2+3; `:!snew` spends itself on stage 2, so `--ours` scans
    // forward from stage 3, finds no stage 2, and unlinks (no-overlay)...
    let f = Fixture::new("ours-unlink");
    let (out, rc) = f.git(&["restore", "--ours", ".", ":!snew"]);
    assert_eq!((out.as_str(), rc), ("", 0));
    assert_eq!(f.read("snew"), None);
    assert_eq!(f.read("f").as_deref(), Some("1\nM\n3\n"));
    assert_eq!(f.index(), CONFLICTED_INDEX);

    // ...or refuses (overlay).
    let f = Fixture::new("ours-overlay");
    let (out, rc) = f.git(&["restore", "--ours", "--overlay", ":!snew"]);
    assert_eq!((out.as_str(), rc), ("error: path 'snew' does not have our version\n", 1));
    f.assert_untouched("--ours --overlay :!snew");

    let f = Fixture::new("merge");
    let (out, rc) = f.git(&["restore", "--merge", ":!snew"]);
    assert_eq!(rc, 1, "got: {out}");
    assert_eq!(
        out,
        "error: path 'del' does not have all necessary versions\n\
         error: path 'snew' does not have all necessary versions\n"
    );
    f.assert_untouched("--merge :!snew");
}

#[test]
fn overlay_keeps_the_stages_of_a_path_the_source_lacks() {
    let f = Fixture::new("overlay-staged");
    let (out, rc) = f.git(&["restore", "--overlay", "--source=side", "--staged", "."]);
    assert_eq!((out.as_str(), rc), ("", 0));
    assert_eq!(
        f.index(),
        "100644 4bcfe98e640c8284511312660fb8709b0afa888e 1\tdel\n\
         100644 5e8fb3bdb3823b1ee0420f98cccf3cdb5db15ab0 2\tdel\n"
            .to_owned()
            + SIDE_RESOLVED
    );
}
