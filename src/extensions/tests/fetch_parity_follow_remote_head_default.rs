//! `fetch.followRemoteHEAD`, new in git 2.56: the fallback for a remote whose own
//! `remote.<name>.followRemoteHEAD` is unset (builtin/fetch.c:1950-1955).
//!
//! * `git_fetch_config()` (builtin/fetch.c:178-192) reads it for every fetch, before
//!   the options: each unrecognized value — `warn-if-not-<branch>` included, which
//!   only the per-remote key accepts — is a warning; a valueless one is
//!   `config_error_nonbool()` and the config walk dies on it.
//! * A remote value git cannot read is now `FOLLOW_REMOTE_UNCONFIGURED`, so the
//!   fetch-wide value decides instead of the built-in `create`.
//! * The `fetchRemoteHEADWarn` advice was reworded to name both keys and to
//!   suggest `warn-if-not-<branch>` (builtin/fetch.c:1716-1729).
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    clone: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `up` has `main` and `other` with `HEAD` on `main`; `cl` is its clone, so
    /// `refs/remotes/origin/HEAD` points at `origin/main`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fetch-follow-head-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let f = Fixture { clone: root.join("cl"), root };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "i"]);
        f.run_in(&up, &["branch", "other"]);
        f.run_in(&f.root, &["clone", "-q", "up", "cl"]);
        f
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env_remove("GIT_ADVICE")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    fn git(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.clone, args)
    }

    fn origin_head(&self) -> String {
        self.git(&["symbolic-ref", "refs/remotes/origin/HEAD"]).0
    }

    /// Point the upstream's `HEAD` at `branch`.
    fn move_upstream_head(&self, branch: &str) {
        let up = self.root.join("up");
        let (_, err, code) = self.run_in(&up, &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")]);
        assert_eq!(code, 0, "{err}");
    }
}

#[test]
fn unrecognized_values_warn_and_a_valueless_one_dies() {
    let f = Fixture::new("values");
    let (out, err, code) = f.git(&[
        "-c",
        "fetch.followRemoteHEAD=bogus",
        "-c",
        "fetch.followRemoteHEAD=warn-if-not-main",
        "fetch",
        "-q",
        "origin",
        "main",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "");
    assert_eq!(
        err,
        "warning: unrecognized fetch.followRemoteHEAD value 'bogus' ignored\n\
         warning: unrecognized fetch.followRemoteHEAD value 'warn-if-not-main' ignored\n"
    );

    let config = f.clone.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    let line = text.lines().count() + 2;
    text.push_str("[fetch]\n\tfollowRemoteHEAD\n");
    std::fs::write(&config, text).unwrap();
    let (out, err, code) = f.git(&["fetch"]);
    assert_eq!(code, 128);
    assert_eq!(out, "");
    assert_eq!(
        err,
        format!(
            "error: missing value for 'fetch.followremotehead'\n\
             fatal: bad config variable 'fetch.followremotehead' in file '.git/config' at line {line}\n"
        )
    );
}

#[test]
fn the_fetch_wide_value_applies_only_while_the_remote_has_none() {
    let f = Fixture::new("fallback");
    f.move_upstream_head("other");

    // The remote's own `never` wins over the fetch-wide `always`.
    let (_, err, code) = f.git(&["-c", "fetch.followRemoteHEAD=always", "-c", "remote.origin.followRemoteHEAD=never", "fetch"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(f.origin_head(), "refs/remotes/origin/main\n");

    // Unset on the remote, the fetch-wide `always` moves `origin/HEAD` silently.
    let (out, err, code) = f.git(&["-c", "fetch.followRemoteHEAD=always", "fetch"]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
    assert_eq!(f.origin_head(), "refs/remotes/origin/other\n");

    // A remote value git cannot read leaves the remote unconfigured, so
    // `fetch.followRemoteHEAD=never` keeps `origin/HEAD` where it is.
    f.move_upstream_head("main");
    let (_, err, code) = f.git(&["-c", "remote.origin.followRemoteHEAD=bogus", "-c", "fetch.followRemoteHEAD=never", "fetch"]);
    assert_eq!(code, 0);
    assert_eq!(err, "warning: unrecognized followRemoteHEAD value 'bogus' ignored\n");
    assert_eq!(f.origin_head(), "refs/remotes/origin/other\n");

    // An unreadable value after a readable one keeps the readable one: `warn`
    // still wins over the fetch-wide `always`, so nothing moves.
    let (out, _, code) = f.git(&[
        "-c",
        "remote.origin.followRemoteHEAD=warn",
        "-c",
        "remote.origin.followRemoteHEAD=bogus",
        "-c",
        "fetch.followRemoteHEAD=always",
        "-c",
        "advice.fetchRemoteHEADWarn=false",
        "fetch",
    ]);
    assert_eq!(code, 0);
    assert_eq!(out, "'HEAD' at 'origin' is 'main', but we have 'other' locally.\n");
    assert_eq!(f.origin_head(), "refs/remotes/origin/other\n");
}

#[test]
fn the_warn_advice_names_both_keys() {
    let f = Fixture::new("advice");
    f.move_upstream_head("other");
    let (out, err, code) = f.git(&["-c", "fetch.followRemoteHEAD=warn", "fetch"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "'HEAD' at 'origin' is 'other', but we have 'main' locally.\n");
    assert_eq!(
        err,
        "hint: Run 'git remote set-head origin other' to follow the change, or modify\n\
         hint: either of the 'remote.origin.followRemoteHEAD' or 'fetch.followRemoteHEAD'\n\
         hint: configuration variables to handle the situation differently.\n\
         hint:\n\
         hint: Using this specific setting\n\
         hint:\n\
         hint:     git config set remote.origin.followRemoteHEAD warn-if-not-other\n\
         hint:\n\
         hint: will suppress the warning until the remote changes HEAD to something else.\n\
         hint: Disable this message with \"git config set advice.fetchRemoteHEADWarn false\"\n"
    );
    assert_eq!(f.origin_head(), "refs/remotes/origin/main\n");
}
