//! `setup_tracking()` (branch.c:252-351) decides a new branch's upstream from
//! the *refspec* that maps onto the start-point, not from the shape of the
//! start-point's name — and `track` is one value that the command line
//! overwrites rather than a pair of independent knobs.
//!
//! What that changes, measured against stock git 2.55.0:
//!
//!   * `find_tracked_branch()` (branch.c:35-59) records the refspec's
//!     **source**. Under `remote.<r>.fetch = refs/tags/*:refs/remotes/<r>/*`
//!     that source is `refs/tags/<x>`, so `branch.autoSetupMerge=simple` — whose
//!     test is `skip_prefix(src, "refs/heads/", …)` — declines it.
//!   * `--track=direct` sets `BRANCH_TRACK_EXPLICIT`, replacing whatever
//!     `branch.autoSetupMerge` said, so it beats `inherit`.
//!   * The ambiguity die happens *after* `create_branch()` wrote the ref
//!     (branch.c:632-645).
//!   * `install_branch_config_multiple_remotes()` refuses to record a branch as
//!     its own upstream (branch.c:105-116), and refuses multiple upstreams when
//!     `branch.autoSetupRebase` applies (:101-103).
//!   * `refs_rename_ref_available()` (refs/files-backend.c:1616-1633,
//!     :1677-1680) refuses a D/F conflict *before* the old name is moved.
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
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-br-tracking-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
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

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }

    fn cfg(&self, key: &str) -> Option<String> {
        let (out, _, code) = self.run(&["config", key]);
        (code == 0).then(|| out.trim().to_string())
    }
}

/// A remote-tracking ref produced by a *tag* refspec records `refs/tags/<x>` as
/// its merge source, so `simple` declines it while `true` accepts it and writes
/// that source.
#[test]
fn the_merge_source_is_the_refspec_source_not_the_name_tail() {
    let f = Fixture::new("refspecsrc");
    f.git(&["tag", "mytag12", "main"]);
    f.git(&["config", "remote.localtags.url", "."]);
    f.git(&["config", "remote.localtags.fetch", "refs/tags/*:refs/remotes/localtags/*"]);
    f.git(&["update-ref", "refs/remotes/localtags/mytag12", "main"]);

    f.git(&["-c", "branch.autosetupmerge=simple", "branch", "t1", "localtags/mytag12"]);
    assert_eq!(f.cfg("branch.t1.remote"), None);
    assert_eq!(f.cfg("branch.t1.merge"), None);

    f.git(&["-c", "branch.autosetupmerge=true", "branch", "t2", "localtags/mytag12"]);
    assert_eq!(f.cfg("branch.t2.remote").as_deref(), Some("localtags"));
    assert_eq!(f.cfg("branch.t2.merge").as_deref(), Some("refs/tags/mytag12"));
}

/// `branch.autoSetupMerge=simple` tracks only when the remote-side *branch*
/// name matches the new branch's.
#[test]
fn simple_tracks_only_a_matching_remote_branch_name() {
    let f = Fixture::new("simple");
    f.git(&["config", "remote.other.url", "."]);
    f.git(&["config", "remote.other.fetch", "refs/heads/*:refs/remotes/other/*"]);
    f.git(&["update-ref", "refs/remotes/other/feature", "main"]);

    f.git(&["-c", "branch.autosetupmerge=simple", "branch", "feature", "other/feature"]);
    assert_eq!(f.cfg("branch.feature.remote").as_deref(), Some("other"));
    assert_eq!(f.cfg("branch.feature.merge").as_deref(), Some("refs/heads/feature"));

    f.git(&["-c", "branch.autosetupmerge=simple", "branch", "mismatch", "other/feature"]);
    assert_eq!(f.cfg("branch.mismatch.remote"), None);
}

/// An explicit `--track=<mode>` replaces `branch.autoSetupMerge` rather than
/// being read through it, and the plain default does not track a local branch.
#[test]
fn an_explicit_track_overrides_auto_setup_merge() {
    let f = Fixture::new("override");
    f.git(&["config", "remote.local.url", "."]);
    f.git(&["config", "remote.local.fetch", "refs/heads/*:refs/remotes/local/*"]);
    f.git(&["update-ref", "refs/remotes/local/main", "main"]);
    f.git(&["branch", "--track", "my1", "local/main"]);

    // inherit copies my1's upstream…
    f.git(&["-c", "branch.autosetupmerge=inherit", "branch", "foo3", "my1"]);
    assert_eq!(f.cfg("branch.foo3.remote").as_deref(), Some("local"));
    assert_eq!(f.cfg("branch.foo3.merge").as_deref(), Some("refs/heads/main"));

    // …but `--track=direct` names my1 itself.
    f.git(&["-c", "branch.autosetupmerge=inherit", "branch", "--track=direct", "foo4", "my1"]);
    assert_eq!(f.cfg("branch.foo4.remote").as_deref(), Some("."));
    assert_eq!(f.cfg("branch.foo4.merge").as_deref(), Some("refs/heads/my1"));

    // …and `--no-track` writes nothing.
    f.git(&["-c", "branch.autosetupmerge=inherit", "branch", "--no-track", "foo5", "my1"]);
    assert_eq!(f.cfg("branch.foo5.remote"), None);

    // The default never tracks a local start-point.
    f.git(&["branch", "foo-no-inherit", "my1"]);
    assert_eq!(f.cfg("branch.foo-no-inherit.remote"), None);
    assert_eq!(f.cfg("branch.foo-no-inherit.merge"), None);
}

/// Two remotes whose fetch refspecs land on the same ref is fatal — after the
/// branch has already been created.
#[test]
fn ambiguous_tracking_fails_but_the_branch_is_created() {
    let f = Fixture::new("ambiguous");
    f.git(&["config", "branch.autosetupmerge", "true"]);
    f.git(&["config", "remote.ambi1.url", "lalala"]);
    f.git(&["config", "remote.ambi1.fetch", "refs/heads/lalala:refs/heads/main"]);
    f.git(&["config", "remote.ambi2.url", "lilili"]);
    f.git(&["config", "remote.ambi2.fetch", "refs/heads/lilili:refs/heads/main"]);

    let (out, err, code) = f.run(&["branch", "all1", "main"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(
        err.starts_with("fatal: not tracking: ambiguous information for ref 'refs/heads/main'\n"),
        "{err}"
    );
    assert!(err.contains("hint:   ambi1\nhint:   ambi2\n"), "{err}");
    assert_eq!(f.stdout(&["rev-parse", "--verify", "refs/heads/all1"]).len(), 41);
    assert_eq!(f.cfg("branch.all1.merge"), None);
}

/// A rename that would collide with another ref is refused before anything
/// moves, in both D/F directions.
#[test]
fn a_blocked_rename_leaves_both_names_alone() {
    let f = Fixture::new("dfguard");
    f.git(&["branch", "o/o"]);
    f.git(&["branch", "o/p"]);

    let (out, err, code) = f.run(&["branch", "-m", "o/o", "o"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err,
        "error: 'refs/heads/o/p' exists; cannot create 'refs/heads/o'\n\
         fatal: branch rename failed\n"
    );
    assert_eq!(f.stdout(&["rev-parse", "--verify", "refs/heads/o/o"]).len(), 41);

    f.git(&["branch", "q"]);
    f.git(&["branch", "r"]);
    let (out, err, code) = f.run(&["branch", "-m", "q", "r/q"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err,
        "error: 'refs/heads/r' exists; cannot create 'refs/heads/r/q'\n\
         fatal: branch rename failed\n"
    );
    assert_eq!(f.stdout(&["rev-parse", "--verify", "refs/heads/q"]).len(), 41);
}
