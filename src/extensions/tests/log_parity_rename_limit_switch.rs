//! `log -l<n>` was refused as an unsupported flag.
//!
//! `OPT_INTEGER('l', NULL, &options->rename_limit, …)` (diff.c:6167) is on the
//! diff option table every `setup_revisions()` caller parses, glued or
//! separated; a non-negative value replaces `diff.renameLimit` (diff.c:5317-5318),
//! and a bad one is parse-options' integer error at 129.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

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
    /// `x` and `y` (30 lines each) are renamed to `x2`/`y2` with one line added.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-rename-limit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        let lines = |from: u32| (from..from + 30).map(|n| format!("{n}\n")).collect::<String>();
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("x"), lines(1)).unwrap();
        std::fs::write(f.root.join("y"), lines(101)).unwrap();
        f.run(&["add", "x", "y"]);
        f.run(&["commit", "-q", "-m", "xy"]);
        f.run(&["mv", "x", "x2"]);
        f.run(&["mv", "y", "y2"]);
        std::fs::write(f.root.join("x2"), lines(1) + "31\n").unwrap();
        std::fs::write(f.root.join("y2"), lines(101) + "131\n").unwrap();
        f.run(&["commit", "-q", "-a", "-m", "mv"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
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
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn the_switch_sets_the_rename_limit() {
    let f = Fixture::new("limit");
    assert_eq!(
        f.run(&["log", "-M", "-l1", "--name-status", "--format=%s", "-1"]),
        (
            "mv\n\nD\tx\nA\tx2\nD\ty\nA\ty2\n".into(),
            "warning: exhaustive rename detection was skipped due to too many files.\n\
             warning: you may want to set your diff.renameLimit variable to at least 2 and retry the command.\n"
                .into(),
            0
        )
    );
    assert_eq!(
        f.run(&["log", "-M", "-l", "5", "--name-status", "--format=%s", "-1"]),
        ("mv\n\nR096\tx\tx2\nR096\ty\ty2\n".into(), String::new(), 0)
    );
    assert_eq!(
        f.run(&["log", "-lx", "-1"]),
        (String::new(), "error: switch `l' expects an integer value with an optional k/m/g suffix\n".into(), 129)
    );
}
