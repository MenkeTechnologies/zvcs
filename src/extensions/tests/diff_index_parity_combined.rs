//! `git diff-index -c|--cc <tree-ish>` without `--cached`: `show_modified()`'s
//! combined arm (diff-lib.c:408-424) turns a path whose tree, index and worktree
//! sides disagree into a two-parent `combine_diff_path` — parent 0 the index entry,
//! parent 1 the tree entry, result the worktree file — and `show_combined_diff()`
//! (combine-diff.c:1284-1296) prints it during the walk, ahead of the queue. zvcs
//! refused every such run with `unsupported output format "-c"`.
//!
//! The textconv half is `show_patch_diff()`'s worktree branch
//! (combine-diff.c:1069-1073) and `grab_blob()`'s textconv arm
//! (combine-diff.c:321-325), which `git diff -c <rev>` and `diff-index --textconv
//! -c` reach and which zvcs skipped, diffing the raw bytes.
//!
//! Every expectation is stock git's output for the same command on the same
//! fixture.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, root: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("ZVCS_HOME", root.join(".zvcs"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap()
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    stock: &'static str,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A merge whose conflicted `f` and `g` were resolved to text neither side
    /// had, then: `f` edited in the worktree only, `g` edited in the index and
    /// again in the worktree, `n` added to the index, `m` made executable in the
    /// worktree, and `b` deleted from it. `t` carries a `diff=up` textconv driver
    /// and an index-and-worktree edit like `g`.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-diff-index-combined-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work, stock };
        let w = |name: &str, text: &str| std::fs::write(fx.work.join(name), text).unwrap();
        fx.stock(&["init", "-q", "-b", "main", "."]);
        fx.stock(&["config", "diff.up.textconv", "tr a-z A-Z <"]);
        w(".gitattributes", "t diff=up\n");
        w("f", "fn a\n1\n2\n3\n4\n5\n");
        w("g", "base\n");
        w("t", "one\ntwo\n");
        w("b", "b\n");
        w("m", "m\n");
        fx.stock(&["add", "."]);
        fx.stock(&["commit", "-q", "-m", "base"]);
        fx.stock(&["checkout", "-q", "-b", "side"]);
        w("f", "fn a\n1\n2\nside\n4\n5\n");
        w("g", "side\n");
        fx.stock(&["commit", "-q", "-am", "side"]);
        fx.stock(&["checkout", "-q", "main"]);
        w("f", "fn a\n1\n2\nmain\n4\n5\n");
        w("g", "main\n");
        fx.stock(&["commit", "-q", "-am", "main"]);
        let _ = run(stock, &fx.work, &fx.root, &["merge", "-q", "side"]);
        w("f", "fn a\n1\n2\nresolved\n4\n5\n");
        w("g", "res\n");
        fx.stock(&["add", "f", "g"]);
        fx.stock(&["commit", "-q", "-m", "merge"]);
        w("f", "fn a\n1\n2\nresolved\n4\n5\nwt\n");
        w("g", "res\nidx\n");
        w("t", "one\ntwo\nidx\n");
        fx.stock(&["add", "g", "t"]);
        w("g", "res\nidx\nwt2\n");
        w("t", "one\ntwo\nidx\nwt\n");
        w("n", "new\n");
        fx.stock(&["add", "n"]);
        std::fs::remove_file(fx.work.join("b")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(fx.work.join("m"), std::fs::Permissions::from_mode(0o755)).unwrap();
        Some(fx)
    }

    fn stock(&self, args: &[&str]) {
        let out = run(self.stock, &self.work, &self.root, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Run `args` through both binaries and require identical stdout, stderr and
    /// exit status.
    fn same(&self, args: &[&str]) {
        let want = run(self.stock, &self.work, &self.root, args);
        let got = run(BIN, &self.work, &self.root, args);
        assert_eq!(
            (String::from_utf8_lossy(&got.stdout), String::from_utf8_lossy(&got.stderr), got.status.code()),
            (String::from_utf8_lossy(&want.stdout), String::from_utf8_lossy(&want.stderr), want.status.code()),
            "{args:?}"
        );
    }
}

#[test]
fn combined_sections_print_ahead_of_the_queue() {
    let Some(fx) = Fixture::new("patch") else { return };
    for args in [
        &["diff-index", "-c", "HEAD"][..],
        &["diff-index", "--cc", "HEAD"],
        &["diff-index", "--diff-merges=dense-combined", "HEAD"],
        &["diff-index", "--cc", "-U1", "HEAD"],
        &["diff-index", "--cc", "--combined-all-paths", "HEAD"],
        &["diff-index", "--cc", "--no-prefix", "--full-index", "HEAD"],
        &["diff-index", "--cc", "--line-prefix=> ", "HEAD"],
        &["diff-index", "--cc", "--color=always", "HEAD"],
        &["-c", "color.diff.frag=yellow", "diff-index", "--cc", "--color=always", "HEAD"],
        &["diff-index", "--cc", "-R", "HEAD"],
        &["diff-index", "--cc", "HEAD", "--", "g", "b"],
    ] {
        fx.same(args);
    }
}

#[test]
fn combined_raw_and_name_records_replace_the_patch() {
    let Some(fx) = Fixture::new("raw") else { return };
    for args in [
        &["diff-index", "-c", "--raw", "HEAD"][..],
        &["diff-index", "-c", "-p", "--raw", "HEAD"],
        &["diff-index", "-c", "--raw", "-z", "HEAD"],
        &["diff-index", "-c", "--raw", "--combined-all-paths", "HEAD"],
        &["diff-index", "--cc", "--name-status", "HEAD"],
        &["diff-index", "--cc", "--name-only", "HEAD"],
        &["diff-index", "--cc", "--stat", "HEAD"],
        &["diff-index", "--cc", "--exit-code", "HEAD"],
        &["diff-index", "--cc", "--quiet", "HEAD"],
        &["diff-index", "--cc", "--cached", "HEAD"],
    ] {
        fx.same(args);
    }
}

#[test]
fn combined_reads_go_through_textconv() {
    let Some(fx) = Fixture::new("textconv") else { return };
    for args in [
        &["diff-index", "--cc", "HEAD", "--", "t"][..],
        &["diff-index", "--cc", "--textconv", "HEAD", "--", "t"],
        &["diff", "-c", "HEAD", "--", "t"],
        &["diff", "--cc", "HEAD"],
        &["diff", "--cc", "--no-textconv", "HEAD", "--", "t"],
    ] {
        fx.same(args);
    }
}
