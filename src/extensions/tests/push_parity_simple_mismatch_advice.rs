//! The `simple` refusal for a differently-named upstream lost its advice.
//!
//! `die_push_simple()` ends with two optional paragraphs: the `push.default`
//! one when that key is unset (builtin/push.c:152-155), and the
//! `branch.autoSetupMerge` one unless it is `simple` (:156-161). zvcs stopped
//! after `git push <remote> HEAD`.
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
    /// `work` on `topic`, which tracks `origin/main`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-simple-advice-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "../r.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        f.run(&["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run(&["remote", "add", "origin", "../r.git"]);
        f.run(&["push", "-q", "origin", "main"]);
        f.run(&["checkout", "-q", "-b", "topic", "origin/main"]);
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

const HEAD: &str = "fatal: The upstream branch of your current branch does not match
the name of your current branch.  To push to the upstream branch
on the remote, use

    git push origin HEAD:main

To push to the branch of the same name on the remote, use

    git push origin HEAD
";

const PUSH_DEFAULT: &str = "
To choose either option permanently, see push.default in 'git help config'.
";

const AUTO_SETUP_MERGE: &str = "
To avoid automatically configuring an upstream branch when its name
won't match the local branch, see option 'simple' of branch.autoSetupMerge
in 'git help config'.
";

#[test]
fn each_paragraph_follows_its_own_key() {
    let f = Fixture::new("advice");
    for (config, tail) in [
        (&[][..], format!("{PUSH_DEFAULT}{AUTO_SETUP_MERGE}\n")),
        (&["-c", "push.default=simple"][..], format!("{AUTO_SETUP_MERGE}\n")),
        (&["-c", "branch.autoSetupMerge=simple"][..], format!("{PUSH_DEFAULT}\n")),
        (&["-c", "push.default=simple", "-c", "branch.autoSetupMerge=simple"][..], "\n".to_string()),
        (&["-c", "branch.autoSetupMerge=always"][..], format!("{PUSH_DEFAULT}{AUTO_SETUP_MERGE}\n")),
    ] {
        let mut args = config.to_vec();
        args.push("push");
        let (out, err, code) = f.run(&args);
        let want = format!("{HEAD}{tail}");
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{config:?}");
    }
}
