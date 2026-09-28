//! The `advice.detachedHead` block a clone that ends on a detached `HEAD` prints.
//!
//! `checkout()` gives it whenever `HEAD` resolves to itself, gated by `advice_enabled()` alone
//! (builtin/clone.c:662-664) — `-q` does not reach it. zvcs printed it only from the
//! `--branch <tag>` fixup and only without `-q`, so `clone -q --branch v1` and any clone of a
//! remote whose `HEAD` is detached away from every branch stayed silent.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
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
    /// `src`: `one` (tag `v1`) and `two` on `main`, `HEAD` detached at `one`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-clone-detached-advice-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "src"], &[]);
        for (name, msg) in [("a", "one"), ("b", "two")] {
            std::fs::write(f.root.join("src").join(name), format!("{name}\n")).unwrap();
            f.run(&["-C", "src", "add", name], &[]);
            f.run(&["-C", "src", "commit", "-q", "-m", msg], &[]);
            if msg == "one" {
                f.run(&["-C", "src", "tag", "v1"], &[]);
            }
        }
        f.run(&["-C", "src", "checkout", "-q", "--detach", "v1"], &[]);
        f
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
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
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn advice(&self) -> String {
        let (one, _, _) = self.run(&["-C", "src", "rev-parse", "v1"], &[]);
        format!(
            "Note: switching to '{}'.\n\n\
You are in 'detached HEAD' state. You can look around, make experimental
changes and commit them, and you can discard any commits you make in this
state without impacting any branches by switching back to a branch.

If you want to create a new branch to retain commits you create, you may
do so (now or later) by using -c with the switch command. Example:

  git switch -c <new-branch-name>

Or undo this operation with:

  git switch -

Turn off this advice by setting config variable advice.detachedHead to false

",
            one.trim_end()
        )
    }
}

#[test]
fn quiet_does_not_silence_it() {
    let f = Fixture::new("quiet");
    let advice = f.advice();
    for (i, args) in [vec!["clone", "-q", "--branch", "v1", "src"], vec!["clone", "-q", "src"]].into_iter().enumerate() {
        let dir = format!("d{i}");
        let mut args = args;
        args.push(&dir);
        let (out, err, code) = f.run(&args, &[]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", advice.as_str(), 0), "{args:?}");
    }
}

#[test]
fn a_remote_head_detached_away_from_every_branch_gets_it() {
    let f = Fixture::new("remote");
    let (_, err, code) = f.run(&["clone", "src", "d"], &[]);
    assert_eq!((err, code), (format!("Cloning into 'd'...\ndone.\n{}", f.advice()), 0));
}

#[test]
fn only_the_advice_switches_and_the_missing_checkout_silence_it() {
    let f = Fixture::new("off");
    for (args, env) in [
        (vec!["clone", "-q", "src", "d0"], vec![("GIT_ADVICE", "0")]),
        (vec!["-c", "advice.detachedHead=false", "clone", "-q", "src", "d1"], vec![]),
        (vec!["clone", "-q", "--no-checkout", "--branch", "v1", "src", "d2"], vec![]),
        (vec!["clone", "-q", "--bare", "src", "d3"], vec![]),
    ] {
        let (out, err, code) = f.run(&args, &env);
        assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0), "{args:?}");
    }
}
