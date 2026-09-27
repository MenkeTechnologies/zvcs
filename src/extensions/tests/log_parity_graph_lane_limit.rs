//! `--graph-lane-limit=<n>` (git 2.55) was refused.
//!
//! `revs->graph_max_lanes` (revision.c:2627-2628) caps the graph at `<n>` lanes:
//! `graph_update_columns()` caps the width at `2n + 2` and every
//! `graph_output_*_line()` draws `~ ` where `graph_needs_truncation()` says a
//! lane is past the limit (graph.c:320-327, 708-718, 855-1500). Without
//! `--graph` it dies (revision.c:3200-3201); only the stuck `=<n>` form exists.
//! zvcs's `log` answered `unrecognized argument` and `rev-list` a usage error.
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
    /// R on `main`; `b1`..`b4` each add one commit on R, ten seconds apart; `main`
    /// is then an octopus of `b1 b2 b3`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-graph-lane-limit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], 0);
        std::fs::write(f.work.join("r"), "r\n").unwrap();
        f.run(&["add", "r"], 0);
        f.run(&["commit", "-q", "-m", "R"], 0);
        for (n, b) in ["b1", "b2", "b3", "b4"].into_iter().enumerate() {
            let at = 10 * (n as u64 + 1);
            f.run(&["checkout", "-q", "-b", b, "main"], at);
            std::fs::write(f.work.join(b), format!("{b}\n")).unwrap();
            f.run(&["add", b], at);
            f.run(&["commit", "-q", "-m", b], at);
        }
        f.run(&["checkout", "-q", "main"], 100);
        f.run(&["merge", "-q", "--no-edit", "b1", "b2", "b3", "-m", "OCT"], 100);
        f
    }

    fn run(&self, args: &[&str], at: u64) -> (String, String, i32) {
        let date = format!("@{} +0000", 1_700_000_000 + at);
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
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_PAGER", "cat")
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

#[test]
fn lanes_past_the_limit_are_drawn_as_tildes() {
    let f = Fixture::new("draw");
    let one = "*-~  OCT\n|\\~ \n| ~ b3\n| * b2\n| ~ \n* ~ b1\n|/  \n| * b4\n|/  \n* R\n";
    let out = f.run(&["log", "--graph", "--format=%s", "--graph-lane-limit=1", "--all"], 0);
    assert_eq!(out, (one.to_string(), String::new(), 0));
    let two = "*-.   OCT\n|\\ \\  \n| | * b3\n| * ~ b2\n| |/  \n* / b1\n|/  \n| * b4\n|/  \n* R\n";
    let out = f.run(&["log", "--graph", "--format=%s", "--graph-lane-limit=2", "--all"], 0);
    assert_eq!(out, (two.to_string(), String::new(), 0));
    // `rev-list` draws the same lanes in front of the object names.
    let (out, _, code) = f.run(&["rev-list", "--graph", "--graph-lane-limit=1", "--all"], 0);
    assert_eq!(code, 0);
    assert!(out.starts_with("*-~  ") && out.contains("\n|\\~ \n"), "{out}");
    // Zero or less is no limit.
    let plain = f.run(&["log", "--graph", "--format=%s", "--all"], 0);
    assert_eq!(f.run(&["log", "--graph", "--format=%s", "--graph-lane-limit=0", "--all"], 0), plain);
}

#[test]
fn the_limit_requires_graph_and_a_number() {
    let f = Fixture::new("errors");
    for verb in ["log", "rev-list"] {
        let out = f.run(&[verb, "--graph-lane-limit=2", "--all"], 0);
        assert_eq!(
            out,
            (String::new(), "fatal: the option '--graph-lane-limit' requires '--graph'\n".into(), 128),
            "{verb}"
        );
        let out = f.run(&[verb, "--graph", "--graph-lane-limit=x", "--all"], 0);
        assert_eq!(out, (String::new(), "fatal: 'x': not an integer\n".into(), 128), "{verb}");
    }
}
