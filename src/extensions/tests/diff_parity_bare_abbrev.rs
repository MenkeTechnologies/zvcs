//! Bare `--abbrev`, with no `=<n>`.
//!
//! `setup_revisions()` treats it as
//!
//! ```c
//! } else if (!strcmp(arg, "--abbrev")) {
//!         revs->abbrev = DEFAULT_ABBREV;
//! ```
//! (revision.c:2641-2642), and `DEFAULT_ABBREV` is the `default_abbrev` global
//! (object-name.h:137): `core.abbrev` when it is set, and otherwise -1, which
//! `repo_find_unique_abbrev()` turns into the width derived from the
//! repository's approximate object count. The literal 7 is
//! `FALLBACK_DEFAULT_ABBREV` (object-name.h:140), used only where there is no
//! object database to size against.
//!
//! Two defects: `git diff --abbrev` hardcoded that 7, so it disagreed with stock
//! on every repository whose `core.abbrev` is set; and `git diff-tree --abbrev`
//! refused outright, which is how t4013-diff-various.sh lost six cases.
//!
//! `diff-tree` is the verb where the flag matters most, because its raw listing
//! starts from `cmd_diff_tree`'s `opt->abbrev = 0` — full object names — so
//! `--abbrev` is the only way to shorten it and `--no-abbrev` puts it back.
//!
//! Every expectation was measured from stock git 2.55.0 over the same fixture.
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
    /// One commit adding `a`, then `a` modified and staged — so both a tree diff
    /// and an index diff have something to name.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diff-babb-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "one\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("a"), "two\n").unwrap();
        f.git(&["add", "a"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join("zvcs"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat")
            .env_remove("GIT_PRINT_SHA1_ELLIPSIS");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "`git {args:?}`: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The two object-name columns of a single-record raw listing.
    fn raw_ids(&self, args: &[&str]) -> (String, String) {
        let out = self.stdout(args);
        let record = out.lines().find(|l| l.starts_with(':')).unwrap_or_else(|| panic!("{out:?}"));
        let f: Vec<&str> = record.split_whitespace().collect();
        (f[2].to_owned(), f[3].to_owned())
    }
}

/// `core.abbrev = <n>` is what bare `--abbrev` means, in every verb that takes
/// it. A hardcoded `FALLBACK_DEFAULT_ABBREV` would print seven here.
#[test]
fn bare_abbrev_is_core_abbrev_when_that_is_set() {
    let f = Fixture::new("cfg");
    f.git(&["config", "core.abbrev", "12"]);
    for verb in [
        vec!["diff", "--cached", "--raw", "--abbrev", "HEAD"],
        vec!["diff-index", "--cached", "--raw", "--abbrev", "HEAD"],
        vec!["diff-tree", "--root", "-r", "--abbrev", "HEAD"],
    ] {
        let (old, new) = f.raw_ids(&verb);
        assert_eq!((old.len(), new.len()), (12, 12), "{verb:?}");
    }
    // The same width reaches the patch `index` line, which reads `o->abbrev` too.
    let patch = f.stdout(&["diff", "--cached", "--abbrev", "HEAD"]);
    let index = patch.lines().find(|l| l.starts_with("index ")).expect("an index line");
    let (old, new) = index["index ".len()..].split_once("..").expect("two names");
    assert_eq!(old.len(), 12, "{index}");
    assert_eq!(new.split_whitespace().next().unwrap().len(), 12, "{index}");
}

/// With `core.abbrev` unset the width is the auto one, which for a repository
/// this small is `FALLBACK_DEFAULT_ABBREV`. The assertion that matters is that
/// bare `--abbrev` and the plain default agree — both are `DEFAULT_ABBREV`.
#[test]
fn bare_abbrev_matches_the_default_width_when_core_abbrev_is_unset() {
    let f = Fixture::new("auto");
    let (bare_old, _) = f.raw_ids(&["diff", "--cached", "--raw", "--abbrev", "HEAD"]);
    let (plain_old, _) = f.raw_ids(&["diff", "--cached", "--raw", "HEAD"]);
    assert_eq!(bare_old, plain_old);
    assert_eq!(bare_old.len(), 7);
}

/// `diff-tree` is the verb the flag exists for: `cmd_diff_tree` starts at
/// `opt->abbrev = 0`, so its raw listing is full-width until `--abbrev` shortens
/// it and `--no-abbrev` puts it back.
#[test]
fn diff_tree_shortens_only_when_asked() {
    let f = Fixture::new("tree");
    let (full_old, full_new) = f.raw_ids(&["diff-tree", "--root", "-r", "HEAD"]);
    assert_eq!((full_old.len(), full_new.len()), (40, 40));

    let (old, new) = f.raw_ids(&["diff-tree", "--root", "-r", "--abbrev", "HEAD"]);
    assert_eq!((old.len(), new.len()), (7, 7));
    assert_eq!(full_new[..7], new);

    let (old, new) = f.raw_ids(&["diff-tree", "--root", "-r", "--abbrev", "--no-abbrev", "HEAD"]);
    assert_eq!((old, new), (full_old, full_new));
}

/// The combined raw listing reads the same width: `show_raw_diff()` renders
/// every parent column through `diff_aligned_abbrev(&oid, opt->abbrev)`
/// (combine-diff.c:1257-1259).
#[test]
fn bare_abbrev_reaches_the_combined_raw_listing() {
    let f = Fixture::new("comb");
    f.git(&["commit", "-q", "-m", "second"]);
    f.git(&["checkout", "-q", "-b", "side", "HEAD~1"]);
    std::fs::write(f.work.join("a"), "side\n").unwrap();
    f.git(&["commit", "-q", "-a", "-m", "side"]);
    f.git(&["checkout", "-q", "main"]);
    let out = f.cmd(&["merge", "-q", "side"]).output().unwrap();
    assert!(!out.status.success(), "the merge is meant to conflict: {out:?}");
    std::fs::write(f.work.join("a"), "merged\n").unwrap();
    f.git(&["commit", "-q", "-a", "-m", "merge"]);

    let out = f.stdout(&["diff-tree", "-c", "--abbrev", "HEAD"]);
    let record = out.lines().find(|l| l.starts_with("::")).expect("a combined record");
    // `::<mode> <mode> <mode> <oid> <oid> <oid> <status><TAB><path>`
    let columns: Vec<&str> = record.split_whitespace().collect();
    assert_eq!(columns.len(), 8, "{record}");
    for oid in &columns[3..6] {
        assert_eq!(oid.len(), 7, "{record}");
    }

    let full = f.stdout(&["diff-tree", "-c", "HEAD"]);
    let record = full.lines().find(|l| l.starts_with("::")).expect("a combined record");
    for oid in record.split_whitespace().skip(3).take(3) {
        assert_eq!(oid.len(), 40, "{record}");
    }
}
