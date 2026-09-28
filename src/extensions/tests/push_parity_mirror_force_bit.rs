//! `--no-mirror` clears the force bit.
//!
//! `--mirror` is `OPT_BIT(0, "mirror", &flags, …,
//! (TRANSPORT_PUSH_MIRROR|TRANSPORT_PUSH_FORCE))` (builtin/push.c:686-687), and
//! an `OPT_BIT` owns every bit of its mask: the negated form clears both. So
//! `git push -f --no-mirror` is not a forced push — a rewound branch is
//! rejected as a non-fast-forward — while `--no-mirror -f` still forces.
//! zvcs treated `--mirror`/`--no-mirror` as touching the mirror flag alone and
//! forced the update.
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
    /// Two commits on `main`, cloned bare to `dst.git`, then rewound one commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-mirror-force-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for n in ["one", "two"] {
            std::fs::write(f.work.join("file"), n).unwrap();
            f.run(&["add", "file"]);
            f.run(&["commit", "-q", "-m", n]);
        }
        f.run(&["clone", "-q", "--bare", ".", "../dst.git"]);
        f.run(&["reset", "-q", "--hard", "HEAD~1"]);
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

    fn remote_main(&self) -> String {
        self.run(&["--git-dir=../dst.git", "rev-parse", "main"]).0
    }
}

#[test]
fn no_mirror_after_force_takes_the_force_back() {
    let f = Fixture::new("cleared");
    let before = f.remote_main();
    let (out, err, code) = f.run(&["push", "-f", "--no-mirror", "../dst.git", "main"]);
    assert_eq!((out.as_str(), code), ("", 1));
    assert!(
        err.starts_with(
            "To ../dst.git\n ! [rejected]        main -> main (non-fast-forward)\n\
             error: failed to push some refs to '../dst.git'\n"
        ),
        "{err}"
    );
    assert_eq!(f.remote_main(), before);
}

#[test]
fn force_after_no_mirror_still_forces() {
    let f = Fixture::new("kept");
    let (out, err, code) = f.run(&["push", "--no-mirror", "-f", "../dst.git", "main"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(err.ends_with("main -> main (forced update)\n"), "{err}");
    assert_eq!(f.remote_main(), f.run(&["rev-parse", "main"]).0);
}
