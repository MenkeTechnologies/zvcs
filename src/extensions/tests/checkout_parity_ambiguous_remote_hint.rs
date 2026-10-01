//! The ambiguous-DWIM hint never named the remotes the branch was found on.
//!
//! git 2.56 hands `unique_tracking_name()` a string list that
//! `check_tracking_name()` appends every matching remote to, in
//! `for_each_remote()` (configuration) order (checkout.c:44-45, 67), and
//! `parse_remote_branch()` passes it to `advise_disambiguating_remotes()`
//! (builtin/checkout.c:1349-1381), which prints
//!
//! ```text
//! hint: Branch name '<name>' appears in multiple remotes:
//! hint:   <remote>
//! hint: If you meant to check out a remote tracking branch on <remote>,
//! …
//! hint:     git <cmd> --track <remote>/<name>
//! ```
//!
//! zvcs printed the older "on, e.g. 'origin'" / `origin/<name>` wording and
//! listed no remote. Expectations measured from stock git 2.56.0 under the
//! same environment.

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
    /// Remotes `zeta`, `alpha`, `mid` configured in that order, each with a
    /// `topic` tracking branch; `two` exists on `alpha` and `zeta` only.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-checkout-ambiguous-remote-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.run(&["commit", "-q", "--allow-empty", "-m", "base"]);
        for remote in ["zeta", "alpha", "mid"] {
            f.run(&["config", &format!("remote.{remote}.url"), "/nonexistent"]);
            f.run(&[
                "config",
                &format!("remote.{remote}.fetch"),
                &format!("+refs/heads/*:refs/remotes/{remote}/*"),
            ]);
            f.run(&["update-ref", &format!("refs/remotes/{remote}/topic"), "HEAD"]);
        }
        f.run(&["update-ref", "refs/remotes/alpha/two", "HEAD"]);
        f.run(&["update-ref", "refs/remotes/zeta/two", "HEAD"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

fn expected(cmd: &str, name: &str, remotes: &[&str]) -> String {
    let mut s = format!("hint: Branch name '{name}' appears in multiple remotes:\n");
    for r in remotes {
        s.push_str(&format!("hint:   {r}\n"));
    }
    s.push_str(&format!(
        "hint: If you meant to check out a remote tracking branch on <remote>,\n\
         hint: you can do so by fully qualifying the name with the --track option:\n\
         hint:\n\
         hint:     git {cmd} --track <remote>/{name}\n\
         hint:\n\
         hint: If you'd like to always have checkouts of an ambiguous name prefer\n\
         hint: one remote, e.g. the 'origin' remote, consider setting\n\
         hint: checkout.defaultRemote=origin in your config.\n\
         fatal: '{name}' matched multiple ({}) remote tracking branches\n",
        remotes.len()
    ));
    s
}

#[test]
fn checkout_lists_the_matching_remotes_in_config_order() {
    let f = Fixture::new("checkout");
    let (out, err, code) = f.run(&["checkout", "topic"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, expected("checkout", "topic", &["zeta", "alpha", "mid"]));

    // Only the remotes that have the branch.
    let (_, err, code) = f.run(&["checkout", "two"]);
    assert_eq!(code, 128);
    assert_eq!(err, expected("checkout", "two", &["zeta", "alpha"]));
}

#[test]
fn switch_names_itself_in_the_track_example() {
    let f = Fixture::new("switch");
    let (out, err, code) = f.run(&["switch", "topic"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, expected("switch", "topic", &["zeta", "alpha", "mid"]));
}

#[test]
fn the_advice_key_silences_the_whole_hint() {
    let f = Fixture::new("quiet");
    let (_, err, code) = f.run(&[
        "-c",
        "advice.checkoutAmbiguousRemoteBranchName=false",
        "checkout",
        "two",
    ]);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: 'two' matched multiple (2) remote tracking branches\n");
}

#[test]
fn default_remote_still_picks_one() {
    let f = Fixture::new("default");
    let (_, err, code) = f.run(&["-c", "checkout.defaultRemote=mid", "checkout", "topic"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(err, "Switched to a new branch 'topic'\n");
    assert_eq!(
        f.run(&["config", "branch.topic.remote"]).0,
        "mid\n"
    );
}
