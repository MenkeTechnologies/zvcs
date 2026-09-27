//! `rev-list --graph -<n>` dropped the lanes of parents the count cut off.
//!
//! `graph_is_interesting()` (graph.c:457) asks `get_commit_action()`, which
//! judges a commit by the filters alone; `--max-count` stops `get_revision()`
//! rather than rejecting a commit. So `rev-list --graph -1 <octopus>` still
//! draws `*-.` and `|\ \` for three parents none of which is printed, exactly
//! as `log --graph -1` does. zvcs's rev-list measured interest against the
//! commits it printed, so a merge cut off by the count came out as a plain `*`.
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
    /// `main` is an octopus of `o1`, `o2` and `o3`, each one commit on `A`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-graph-max-count-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "A"]);
        for b in ["o1", "o2", "o3"] {
            f.run(&["checkout", "-q", "-b", b, "main"]);
            std::fs::write(f.work.join(b), format!("{b}\n")).unwrap();
            f.run(&["add", b]);
            f.run(&["commit", "-q", "-m", b]);
        }
        f.run(&["checkout", "-q", "main"]);
        f.run(&["merge", "-q", "--no-edit", "o1", "o2", "o3", "-m", "OCT"]);
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

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0.trim_end().to_string()
    }
}

#[test]
fn a_counted_off_parent_keeps_its_lane() {
    let f = Fixture::new("count");
    let (m, o3) = (f.rev("main"), f.rev("o3"));

    let (out, err, code) = f.run(&["rev-list", "--graph", "-1", "main"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(out, format!("*-.   {m}\n|\\ \\  \n"));

    let (out, _, _) = f.run(&["rev-list", "--graph", "-2", "main"]);
    assert_eq!(out, format!("*-.   {m}\n|\\ \\  \n| | * {o3}\n"));

    // A skipped commit never reaches `graph_update()`, so the graph starts clean.
    let (out, _, _) = f.run(&["rev-list", "--graph", "--skip=1", "-1", "main"]);
    assert_eq!(out, format!("* {o3}\n"));
}
