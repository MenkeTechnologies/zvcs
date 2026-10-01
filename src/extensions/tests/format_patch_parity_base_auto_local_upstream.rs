//! `format-patch --base=auto` takes a local upstream.
//!
//! `prepare_bases()` asks `branch_get_upstream(curr_branch, NULL)`
//! (builtin/log.c:1739-1740), and a `branch.<name>.remote` of `.` makes
//! `branch.<name>.merge` the upstream itself — what
//! `git branch --set-upstream-to=<local-branch>` records. zvcs looked only for a
//! remote-tracking ref and died `failed to get upstream`.
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
    /// `upstream` stays at `one`; `main` adds `two` and `three` and tracks it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-format-patch-base-auto-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        for (n, msg) in [("1", "one"), ("2", "two"), ("3", "three")] {
            std::fs::write(f.work.join("a"), format!("{n}\n")).unwrap();
            f.ok(&["add", "a"]);
            f.ok(&["commit", "-q", "-m", msg]);
            if msg == "one" {
                f.ok(&["branch", "upstream"]);
            }
        }
        f.ok(&["branch", "-q", "--set-upstream-to=upstream"]);
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
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    }
}

const TRAILER: &str = "\nbase-commit: 1abd72c2359c68370f2ab7d909a5e0cee80504f5\n\
                       prerequisite-patch-id: abfc42adcd1ac1b0c35ceb534df50e69e6a84f0f\n-- \n2.56.0\n\n";

#[test]
fn base_auto_uses_a_local_upstream() {
    let f = Fixture::new("auto");
    let out = f.ok(&["format-patch", "--base=auto", "--stdout", "-1"]);
    assert!(out.ends_with(TRAILER), "{out}");
    let when_able = f.ok(&["-c", "format.useAutoBase=whenAble", "format-patch", "--stdout", "-1"]);
    assert!(when_able.ends_with(TRAILER), "{when_able}");
}

#[test]
fn without_an_upstream_base_auto_still_dies() {
    let f = Fixture::new("none");
    f.ok(&["branch", "--unset-upstream"]);
    let (out, err, code) = f.run(&["format-patch", "--base=auto", "--stdout", "-1"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "fatal: failed to get upstream, if you want to record base commit automatically,\n\
             please use git branch --set-upstream-to to track a remote branch.\n\
             Or you could specify base commit by --base=<base-commit-id> manually\n",
            128
        )
    );
}
