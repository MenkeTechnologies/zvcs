//! `set_merge()` for the local remote `.` (remote.c:1798-1808): once
//! `remote_find_tracking()` finds no refspec, `branch.<name>.merge` goes through
//! `repo_dwim_ref()`, and only a name that resolves under exactly one
//! `ref_rev_parse_rules` spelling is replaced by that ref. A name that resolves
//! under none, or under several (a branch and a tag both called `dup`), stays
//! as configured, and that raw string is the upstream every reader reports.
//!
//! The readers compared here: `%(upstream)` and its `:short`, `:track`,
//! `:remotename` and `:remoteref` forms, `branch -vv`, `rev-parse` of
//! `<branch>@{u}` (whose `set_shortened_ref()` short form is what makes an
//! upstream of `refs/tags/dup` ambiguous), `log`, and `status` in its short and
//! long formats, with `core.warnAmbiguousRefs` on and off. Each run is compared
//! against stock git on an identical fixture: stdout, stderr and exit status.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");
#[path = "support/stock_git.rs"]
mod stock_git;

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, root: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("HOME", root)
        .env("ZVCS_HOME", root.join("zvcs-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"))
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run binary")
}

impl Fixture {
    /// `main` two commits deep, `dup` both a branch (at the first commit) and a
    /// tag (at the second), a dangling symref `dang`, a branch `FOO` shadowed by
    /// a root ref `$GIT_DIR/FOO`, and `work` checked out with `branch.work.remote
    /// = .` and the given `branch.work.merge`.
    fn new(stock: &str, tag: &str, merge: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-upstream-dwim-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        let f = Fixture { root, repo };
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["commit", "-q", "--allow-empty", "-m", "one"][..],
            &["commit", "-q", "--allow-empty", "-m", "two"][..],
            &["branch", "dup", "HEAD~1"][..],
            &["tag", "dup", "HEAD"][..],
            &["branch", "FOO", "HEAD~1"][..],
            &["update-ref", "FOO", "HEAD"][..],
            &["symbolic-ref", "refs/heads/dang", "refs/heads/nowhere"][..],
            &["checkout", "-q", "-b", "work"][..],
            &["config", "branch.work.remote", "."][..],
            &["config", "branch.work.merge", merge][..],
        ] {
            let o = run(stock, &f.root, &f.repo, args);
            assert!(o.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        }
        f
    }

    fn run(&self, bin: &str, args: &[&str]) -> String {
        let o = run(bin, &self.root, &self.repo, args);
        format!(
            "status={:?}\nstdout:\n{}\nstderr:\n{}",
            o.status.code(),
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    }
}

const READERS: &[&[&str]] = &[
    &[
        "for-each-ref",
        "--format=%(upstream)|%(upstream:short)|%(upstream:track)|%(upstream:remotename)|%(upstream:remoteref)",
        "refs/heads/work",
    ],
    &["branch", "-vv", "--list", "work"],
    &["rev-parse", "work@{u}"],
    &["rev-parse", "--symbolic-full-name", "work@{u}"],
    &["rev-parse", "--abbrev-ref", "@{u}"],
    &["log", "-1", "--format=%s", "@{u}"],
    &["status", "-sb"],
    &["status"],
    &["-c", "core.warnAmbiguousRefs=false", "for-each-ref", "--format=%(upstream)|%(upstream:track)", "refs/heads/work"],
    &["-c", "core.warnAmbiguousRefs=false", "rev-parse", "--symbolic-full-name", "work@{u}"],
    &["-c", "core.warnAmbiguousRefs=false", "status", "-sb"],
];

#[test]
fn local_upstream_dwims_as_set_merge_does() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else {
        eprintln!("skipping: no stock git >= 2.56.0 installed");
        return;
    };
    // `dup`: two refs, left raw. `tags/dup`, `refs/heads/dup`, `HEAD`: one ref
    // each, qualified (`HEAD` to the branch it names). `nonexist`: none, raw.
    // `dang`: the dangling symref counts as nothing and warns. `FOO`: the root
    // ref and `refs/heads/FOO`, raw, but a name `refs_read_ref()` can read.
    for (i, merge) in ["dup", "tags/dup", "refs/heads/dup", "HEAD", "nonexist", "dang", "FOO"]
        .iter()
        .enumerate()
    {
        let want = Fixture::new(stock, &format!("s{i}"), merge);
        let got = Fixture::new(stock, &format!("z{i}"), merge);
        for args in READERS {
            assert_eq!(
                got.run(BIN, args),
                want.run(stock, args),
                "branch.work.merge = {merge}: `git {args:?}`"
            );
        }
    }
}

/// A merge value whose dwim reaches `work@{u}` again finds the branch set up
/// with no destination, and `interpret_branch_mark()` dies on it instead of
/// recursing (remote.c:1794, object-name.c:1455-1461).
#[test]
fn self_referential_merge_dies_like_stock() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else {
        eprintln!("skipping: no stock git >= 2.56.0 installed");
        return;
    };
    let want = Fixture::new(stock, "self-s", "work@{u}");
    let got = Fixture::new(stock, "self-z", "work@{u}");
    for args in [
        &["for-each-ref", "--format=%(upstream) %(upstream:track)", "refs/heads/work"][..],
        &["rev-parse", "work@{u}"][..],
    ] {
        assert_eq!(got.run(BIN, args), want.run(stock, args), "`git {args:?}`");
    }
}
