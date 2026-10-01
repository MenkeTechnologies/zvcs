//! `--relative[=<path>]` across `diff`, `diff-index` and `diff-files`, as git
//! 2.56 does it.
//!
//! 2.56 taught `oneway_diff()` (diff-lib.c:541-544) and `diff_unmerge()`
//! (diff.c:7727-7728) to drop an entry outside the prefix, which ended the
//! segfault `git diff --relative --cached` hit on an unmerged path elsewhere
//! (t4045 "diff --relative --cached with change in subdir"). The rest of what is
//! pinned here is the `diff.c` prefix machinery that scenario runs through:
//!
//! * `diffopt.prefix` is the cwd prefix unless `--relative=<path>` replaced it,
//!   verbatim (`diff_opt_relative()`, diff.c:5884-5893); `--no-relative` clears
//!   only the flag (diff.c:5270-5275). `diff-index` ignored a bare `--relative`.
//! * The narrowing is `strncmp(path, prefix, prefix_length)` — a byte prefix, so
//!   `--relative=sub` also takes `sub2/` — and `strip_prefix()` (diff.c:4988-5001)
//!   drops `prefix_length` bytes and then one `/` if one follows.
//! * `diff_summary()`, `show_dirstat()`, an unmerged pair's diffstat row
//!   (diff.c:5071-5075) and combine-diff.c never strip, so they print full paths.
//!   `diff-files -c --relative` stripped the combined path and then read the
//!   *top-level* file of that name.
//!
//! Expectations measured from stock git 2.56.0.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diff-rel-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.git(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1112911993 +0000")
            .env("GIT_COMMITTER_DATE", "1112911993 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(&self.root, args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    /// t4045's `test_commit <msg> <file>`: the file holds `<msg>`, tagged `<msg>`.
    fn test_commit(&self, msg: &str, file: &str) {
        self.write(file, &format!("{msg}\n"));
        self.git(&["add", file]);
        self.git(&["commit", "-q", "-m", msg]);
        self.git(&["tag", msg]);
    }

    /// stdout of a command run in `dir` (relative to the worktree), which must
    /// exit 0.
    fn out_in(&self, dir: &str, args: &[&str]) -> String {
        let out = self.cmd(&self.root.join(dir), args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` in {dir:?} failed: {out:?}");
        assert!(out.stderr.is_empty(), "`git {args:?}` wrote stderr: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }
}

/// t4045's "setup diff --relative unmerged", then `merge sub1` on `br3`: both
/// `file0` and `subdir/file0` conflict.
fn conflicted(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    f.test_commit("zero", "file0");
    f.test_commit("base", "subdir/file0");
    f.git(&["checkout", "-q", "-b", "br1"]);
    f.test_commit("one", "file0");
    f.test_commit("sub1", "subdir/file0");
    f.git(&["checkout", "-q", "-b", "br2", "base"]);
    f.test_commit("two", "file0");
    f.git(&["checkout", "-q", "-b", "br3"]);
    f.test_commit("sub3", "subdir/file0");
    let st = f.cmd(&f.root, &["merge", "sub1"]).output().unwrap().status;
    assert_eq!(st.code(), Some(1), "the merge was expected to conflict");
    f
}

#[test]
fn diff_relative_cached_drops_an_unmerged_path_outside_the_prefix() {
    let f = conflicted("cached");
    assert_eq!(f.out_in("subdir", &["diff", "--relative", "--name-only", "--cached"]), "file0\n");
    assert_eq!(
        f.out_in("subdir", &["diff-index", "--relative", "--cached", "--name-only", "HEAD"]),
        "file0\n"
    );
}

#[test]
fn an_unmerged_stat_row_keeps_its_full_path_under_relative() {
    let f = conflicted("stat");
    assert_eq!(
        f.out_in("subdir", &["diff", "--relative", "--cached", "--stat"]),
        " subdir/file0 | Unmerged\n 0 files changed\n"
    );
    assert_eq!(
        f.out_in("subdir", &["diff", "--relative", "--stat"]),
        " subdir/file0 | Unmerged\n file0        | 4 ++++\n 1 file changed, 4 insertions(+)\n"
    );
    assert_eq!(
        f.out_in("subdir", &["diff-index", "--relative", "--cached", "--numstat", "HEAD"]),
        "0\t0\tsubdir/file0\n"
    );
    assert_eq!(
        f.out_in("subdir", &["diff-files", "--relative", "--numstat"]),
        "0\t0\tsubdir/file0\n4\t0\tfile0\n"
    );
}

#[test]
fn diff_files_combined_relative_reads_and_prints_the_full_path() {
    let f = conflicted("combined");
    assert_eq!(
        f.out_in("subdir", &["diff-files", "--relative", "-c"]),
        "diff --combined subdir/file0\n\
         index b8c2280,48df0cb..0000000\n\
         --- a/subdir/file0\n\
         +++ b/subdir/file0\n\
         @@@ -1,1 -1,1 +1,5 @@@\n\
         ++<<<<<<< HEAD\n \
         +sub3\n\
         ++=======\n\
         + sub1\n\
         ++>>>>>>> sub1\n"
    );
}

/// `top`, `sub/file` and `sub2/file` modified in the worktree, `sub/new` staged.
fn prefixed(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    f.write("top", "a\n");
    f.write("sub/file", "b\n");
    f.write("sub2/file", "c\n");
    f.git(&["add", "."]);
    f.git(&["commit", "-q", "-m", "init"]);
    f.write("top", "a\nz\n");
    f.write("sub/file", "b\nz\n");
    f.write("sub2/file", "c\nz\n");
    f.write("sub/new", "n\n");
    f.git(&["add", "sub/new"]);
    f
}

#[test]
fn relative_with_a_value_is_a_byte_prefix_stripped_with_one_slash() {
    let f = prefixed("byteprefix");
    assert_eq!(f.out_in("", &["diff", "--relative=sub", "--name-only"]), "file\n2/file\n");
    assert_eq!(f.out_in("", &["diff-files", "--relative=sub", "--name-only"]), "file\n2/file\n");
    assert_eq!(
        f.out_in("", &["diff-index", "--relative=sub", "--name-only", "HEAD"]),
        "file\nnew\n2/file\n"
    );
}

#[test]
fn no_relative_clears_only_the_flag_and_relative_brings_the_value_back() {
    let f = prefixed("flag");
    assert_eq!(
        f.out_in("", &["diff", "--relative=sub", "--no-relative", "--relative", "--name-only", "HEAD"]),
        "file\nnew\n2/file\n"
    );
    assert_eq!(
        f.out_in("", &["diff", "--relative=sub", "--no-relative", "--name-only", "HEAD"]),
        "sub/file\nsub/new\nsub2/file\ntop\n"
    );
}

#[test]
fn diff_index_bare_relative_narrows_to_the_cwd() {
    let f = prefixed("cwd");
    assert_eq!(f.out_in("sub", &["diff-index", "--relative", "--name-only", "HEAD"]), "file\nnew\n");
    assert_eq!(f.out_in("sub", &["diff-index", "--relative=sub2", "--name-only", "HEAD"]), "file\n");
}

#[test]
fn summary_and_dirstat_name_full_paths_under_relative() {
    let f = prefixed("summary");
    let stat_summary = " file | 1 +\n new  | 1 +\n 2 files changed, 2 insertions(+)\n create mode 100644 sub/new\n";
    assert_eq!(f.out_in("", &["diff", "--relative=sub/", "--summary", "--stat", "HEAD"]), stat_summary);
    assert_eq!(
        f.out_in("", &["diff-index", "--relative=sub/", "--summary", "--stat", "HEAD"]),
        stat_summary
    );
    assert_eq!(f.out_in("", &["diff", "--relative=sub/", "--dirstat", "HEAD"]), " 100.0% sub/\n");
    assert_eq!(f.out_in("", &["diff-files", "--relative=sub/", "--dirstat"]), " 100.0% sub/\n");
}
