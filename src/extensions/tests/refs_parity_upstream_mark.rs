//! The `@{u}` mark where it is not the whole operand, and where `--quiet` says
//! not to die on it.
//!
//! * `repo_dwim_log()` opens with `substitute_branch_name(r, &str, &len, 0)`
//!   (refs.c:840-844, v2.55.0), so the ref half of `<branch>@{u}@{1}` is
//!   rewritten to the upstream's full name before any `ref_rev_parse_rules`
//!   spelling is tried — the reflog read is the *upstream's* reflog. The port
//!   handed the reflog reader `my-side@{u}` as a ref name, which matches
//!   nothing, and the operand came back `ambiguous argument`.
//! * `--quiet` is `flags |= GET_OID_QUIETLY` (builtin/rev-parse.c:866-870),
//!   which reaches `interpret_branch_mark()` as `nonfatal_dangling_mark`
//!   (`fatal = !(flags & GET_OID_QUIETLY)`, object-name.c:686, :744-748). With
//!   it set a mark that names no upstream is not a `die()` at all: it returns -1
//!   (object-name.c:1456-1462) and the operand merely fails to resolve. The port
//!   died anyway, so `git rev-parse --verify --quiet @{u}` printed a fatal and
//!   exited 128 where git is silent and exits 1.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository, stdout, stderr and exit status compared separately.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    clone: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// An upstream repository with two commits on `side`, and a clone whose
    /// `my-side` tracks `origin/side` — with the clone's remote-tracking ref one
    /// commit behind, so `@{1}` on it names the first commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-upstream-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        let clone = root.join("clone");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work, clone };

        f.git(&f.work, &["init", "-q", "-b", "main", "."]);
        f.commit(&f.work, "one");
        f.git(&f.work, &["checkout", "-q", "-b", "side"]);
        f.commit(&f.work, "two");
        f.git(
            &f.root,
            &["clone", "-q", f.work.to_str().unwrap(), f.clone.to_str().unwrap()],
        );
        f.git(&f.clone, &["branch", "--track", "my-side", "origin/side"]);
        // A third commit upstream, fetched, so `origin/side@{1}` is the value the
        // tracking ref held before this fetch.
        f.commit(&f.work, "three");
        f.git(&f.clone, &["fetch", "-q", "origin"]);
        f
    }

    fn cmd(&self, dir: &PathBuf, args: &[&str]) -> Command {
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
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, dir: &PathBuf, args: &[&str]) {
        let out = self.cmd(dir, args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn commit(&self, dir: &PathBuf, name: &str) {
        std::fs::write(dir.join(format!("{name}.t")), format!("{name}\n")).unwrap();
        self.git(dir, &["add", &format!("{name}.t")]);
        self.git(dir, &["commit", "-q", "-m", name]);
    }

    fn run(&self, dir: &PathBuf, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(dir, args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn oid(&self, dir: &PathBuf, spec: &str) -> String {
        let (out, err, code) = self.run(dir, &["rev-parse", spec]);
        assert_eq!((err.as_str(), code), ("", 0), "{spec}");
        out.trim().to_string()
    }
}

/// `<branch>@{u}@{1}` reads the *upstream's* reflog, in either case spelling of
/// the mark.
#[test]
fn a_reflog_selector_after_an_upstream_mark_reads_the_upstream_log() {
    let f = Fixture::new("chain");
    let clone = f.clone.clone();
    let want = f.oid(&clone, "refs/remotes/origin/side@{1}");
    assert_eq!(f.oid(&clone, "my-side@{u}@{1}"), want);
    assert_eq!(f.oid(&clone, "my-side@{U}@{1}"), want);
    assert_eq!(f.oid(&clone, "my-side@{upstream}@{1}"), want);

    // And the mark alone still names the tip, which `@{1}` must not be confused
    // with.
    assert_eq!(
        f.oid(&clone, "my-side@{u}"),
        f.oid(&clone, "refs/remotes/origin/side")
    );
}

/// With no upstream, `--quiet` turns the `die()` into a failure to resolve:
/// nothing on stderr, exit 1.
#[test]
fn quiet_turns_a_dangling_upstream_mark_into_a_silent_failure() {
    let f = Fixture::new("quiet");
    let work = f.work.clone();
    let (out, err, code) = f.run(&work, &["rev-parse", "--verify", "--quiet", "@{u}"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 1));
}

/// Without `--quiet` the same operand is the `die()`, naming the branch.
#[test]
fn a_dangling_upstream_mark_is_otherwise_still_fatal() {
    let f = Fixture::new("fatal");
    let work = f.work.clone();
    let (out, err, code) = f.run(&work, &["rev-parse", "--verify", "@{u}"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: no upstream configured for branch 'side'\n");
}

/// `--quiet` outside `--verify` is the same non-fatal rule: the operand falls
/// through to the ordinary ambiguous-argument block instead of the upstream
/// `die()`.
#[test]
fn quiet_without_verify_falls_through_to_the_ambiguous_argument_block() {
    let f = Fixture::new("plain");
    let work = f.work.clone();
    let (_, err, code) = f.run(&work, &["rev-parse", "--quiet", "@{u}"]);
    assert_eq!(code, 128);
    assert!(err.contains("ambiguous argument '@{u}'"), "{err:?}");
    assert!(!err.contains("no upstream configured"), "{err:?}");
}
