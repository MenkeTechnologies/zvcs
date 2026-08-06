//! What `checkout`/`switch` do about upstreams, and how a bare `--` is read.
//!
//! `git checkout -B main origin/main --` is how the JetBrains client spells a branch
//! reset: the separator introduces no pathspec, so it is not a path restore.
//! `setup_tracking()` then configures the upstream on its own — `branch.autoSetupMerge`
//! defaults to tracking a remote-tracking start point — and `report_tracking()` prints
//! the ahead/behind summary when the branch already existed.
//!
//! Expectations measured against stock git 2.55.0.
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
    /// A `src` repository and a `work` clone of it, on `master`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-cotrack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        let src = f.root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        f.git(&src, &["init", "-q", "-b", "master", "."]);
        f.git(&src, &["config", "user.email", "t@e.co"]);
        f.git(&src, &["config", "user.name", "t"]);
        std::fs::write(src.join("f.txt"), "a\n").unwrap();
        f.git(&src, &["add", "-A"]);
        f.git(&src, &["commit", "-q", "-m", "seed"]);
        f.git(&f.root, &["clone", "-q", "src", "work"]);
        let work = f.root.join("work");
        f.git(&work, &["config", "user.email", "t@e.co"]);
        f.git(&work, &["config", "user.name", "t"]);
        f
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let out = self.cmd(dir, args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, dir: &Path, args: &[&str]) -> std::process::Output {
        self.cmd(dir, args).output().unwrap()
    }

    fn config(&self, key: &str) -> String {
        String::from_utf8_lossy(&self.run(&self.work(), &["config", "--get", key]).stdout)
            .trim_end()
            .to_owned()
    }
}

/// A trailing `--` names no path, so branch creation is not a path restore.
#[test]
fn a_bare_separator_is_not_a_path_restore() {
    let f = Fixture::new("dashdash");
    let work = f.work();

    let out = f.run(&work, &["checkout", "-B", "master", "origin/master", "--"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("Reset branch 'master'"));

    let head = String::from_utf8_lossy(&f.run(&work, &["rev-parse", "HEAD"]).stdout)
        .trim_end()
        .to_owned();
    let out = f.run(&work, &["checkout", "-b", "adg", &head, "--"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("Switched to a new branch 'adg'"));

    // A path *after* the separator is still a path restore, and still refused.
    let out = f.run(&work, &["checkout", "-b", "other", "master", "--", "f.txt"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: Cannot update paths and switch to branch 'other' at the same time.\n"
    );
}

/// `branch.autoSetupMerge` (default) configures the upstream without `-t`.
#[test]
fn a_remote_start_point_sets_up_tracking() {
    let f = Fixture::new("track");
    let work = f.work();

    let out = f.run(&work, &["checkout", "-b", "feature", "origin/master"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "branch 'feature' set up to track 'origin/master'.\n"
    );
    assert_eq!(f.config("branch.feature.remote"), "origin");
    assert_eq!(f.config("branch.feature.merge"), "refs/heads/master");

    // `--no-track` opts out, and a local start point is not tracked by default.
    let out = f.run(&work, &["checkout", "--no-track", "-b", "nt", "origin/master"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(f.config("branch.nt.remote").is_empty());

    f.git(&work, &["checkout", "-q", "-b", "local-start", "master"]);
    assert!(f.config("branch.local-start.remote").is_empty());
}

/// Switching to a branch that has an upstream reports where it stands.
#[test]
fn switching_reports_the_tracking_status() {
    let f = Fixture::new("report");
    let work = f.work();
    f.git(&work, &["checkout", "-q", "-b", "detour"]);

    for cmd in [["checkout", "master"], ["switch", "master"]] {
        f.git(&work, &["checkout", "-q", "detour"]);
        let out = f.run(&work, &cmd);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "Your branch is up to date with 'origin/master'.\n",
            "{cmd:?}"
        );
    }

    // One commit ahead reads as such, with git's push hint.
    f.git(&work, &["checkout", "-q", "master"]);
    std::fs::write(work.join("f.txt"), "a\nb\n").unwrap();
    f.git(&work, &["commit", "-qam", "more"]);
    f.git(&work, &["checkout", "-q", "detour"]);
    let out = f.run(&work, &["checkout", "master"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Your branch is ahead of 'origin/master' by 1 commit.\n  (use \"git push\" to publish your local commits)\n"
    );

    // A branch with no upstream reports nothing.
    let out = f.run(&work, &["checkout", "detour"]);
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// `blame --encoding=UTF-8` — what every IDE passes — is the output this port already
/// produces, so it is accepted rather than refused.
#[test]
fn blame_accepts_a_utf8_encoding_request() {
    let f = Fixture::new("blame");
    let work = f.work();
    let plain = f.run(&work, &["blame", "f.txt"]);
    assert_eq!(plain.status.code(), Some(0), "{plain:?}");

    for spelling in ["--encoding=UTF-8", "--encoding=utf-8", "--encoding=none"] {
        let out = f.run(&work, &["blame", spelling, "f.txt"]);
        assert_eq!(out.status.code(), Some(0), "{spelling}: {out:?}");
        assert_eq!(out.stdout, plain.stdout, "{spelling} changed the output");
    }

    // An encoding that would need transcoding is refused rather than guessed at.
    let out = f.run(&work, &["blame", "--encoding=ISO-8859-1", "f.txt"]);
    assert_ne!(out.status.code(), Some(0), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("only utf-8 and none are ported"),
        "{out:?}"
    );
}
