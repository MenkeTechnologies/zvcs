//! `cmd_push()` checks its mode options before it looks a remote up
//! (builtin/push.c:728-733):
//!
//! ```c
//! die_for_incompatible_opt4(deleterefs, "--delete",
//!                           tags, "--tags",
//!                           flags & TRANSPORT_PUSH_ALL, "--all/--branches",
//!                           flags & TRANSPORT_PUSH_MIRROR, "--mirror");
//! if (deleterefs && argc < 2)
//!         die(_("--delete doesn't make sense without any refs"));
//! ```
//!
//! `die_for_incompatible_opt4()` (parse-options.c:1528-1560) names the set
//! options in that order. zvcs refused only `--all --tags`, with a message of
//! its own; `--delete` with no ref pushed nothing and exited 0, and `--mirror`
//! with `--tags` or `--all` created `refs/remotes/*` on the remote.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// A bare `up.git` holding `main`, and a work repository `w` whose remote `o`
    /// points at it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-incompat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.root, &["init", "-q", "--bare", "up.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["remote", "add", "o", "../up.git"]);
        f.run(&["push", "-q", "o", "main"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn remote_refs(&self) -> String {
        self.run(&["--git-dir=../up.git", "for-each-ref", "--format=%(refname)"]).0
    }
}

#[test]
fn every_pair_and_triple_names_the_set_options_in_c_order() {
    let f = Fixture::new("pairs");
    for (args, named) in [
        (&["--tags", "--mirror"][..], "options '--tags' and '--mirror'"),
        (&["--all", "--mirror"][..], "options '--all/--branches' and '--mirror'"),
        (&["--branches", "--tags"][..], "options '--tags' and '--all/--branches'"),
        (&["--delete", "--all"][..], "options '--delete' and '--all/--branches'"),
        (&["--mirror", "--all", "--tags"][..], "options '--tags', '--all/--branches', and '--mirror'"),
        (
            &["--mirror", "-d", "--all", "--tags"][..],
            "options '--delete', '--tags', '--all/--branches', and '--mirror'",
        ),
    ] {
        let mut argv = vec!["push"];
        argv.extend_from_slice(args);
        argv.extend_from_slice(&["o", "x"]);
        let (out, err, code) = f.run(&argv);
        let want = format!("fatal: {named} cannot be used together\n");
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{args:?}");
    }
    assert_eq!(f.remote_refs(), "refs/heads/main\n");
}

#[test]
fn delete_needs_a_ref_on_the_command_line() {
    let f = Fixture::new("delete");
    for argv in [
        &["push", "--delete", "o"][..],
        // Checked before a remote is chosen, so no default is looked up.
        &["push", "-d"][..],
        // `--repo` seeds the remote but is not a positional.
        &["push", "-d", "--repo=o"][..],
    ] {
        let (out, err, code) = f.run(argv);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "fatal: --delete doesn't make sense without any refs\n", 128),
            "{argv:?}"
        );
    }
    // `push -d --repo=o main`: `main` is the repository, and still no ref.
    let (_, err, code) = f.run(&["push", "-d", "--repo=o", "main"]);
    assert_eq!((err.as_str(), code), ("fatal: --delete doesn't make sense without any refs\n", 128));
    assert_eq!(f.remote_refs(), "refs/heads/main\n");
}
