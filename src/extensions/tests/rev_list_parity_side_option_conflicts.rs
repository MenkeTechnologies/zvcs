//! The side-selecting options' mutual exclusions were never enforced.
//!
//! `handle_revision_opt()` dies while parsing each of them against what the
//! options already read have set (revision.c:2486-2517): `--left-only` after
//! `--right-only`/`--cherry`, `--right-only` or `--cherry` after `--left-only`,
//! and `--cherry-mark`/`--cherry-pick` after each other (`--cherry` sets
//! `cherry_mark`). zvcs's `rev-list` and `log` accepted every combination and
//! printed a listing.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-side-conflicts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "A"]);
        f.run(&["branch", "side"]);
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
fn the_second_option_dies_naming_the_pair() {
    let f = Fixture::new("pairs");
    for (a, b, first, second) in [
        ("--right-only", "--left-only", "--left-only", "--right-only/--cherry"),
        ("--cherry", "--left-only", "--left-only", "--right-only/--cherry"),
        ("--left-only", "--right-only", "--right-only", "--left-only"),
        ("--left-only", "--cherry", "--cherry", "--left-only"),
        ("--cherry-pick", "--cherry-mark", "--cherry-mark", "--cherry-pick"),
        ("--cherry-mark", "--cherry-pick", "--cherry-pick", "--cherry-mark"),
        ("--cherry", "--cherry-pick", "--cherry-pick", "--cherry-mark"),
    ] {
        let want = format!("fatal: options '{first}' and '{second}' cannot be used together\n");
        for verb in ["rev-list", "log"] {
            let out = f.run(&[verb, a, b, "main...side"]);
            assert_eq!(out, (String::new(), want.clone(), 128), "{verb} {a} {b}");
        }
    }
    // Compatible spellings still run.
    assert_eq!(f.run(&["rev-list", "--cherry", "--right-only", "main...side"]).2, 0);
    assert_eq!(f.run(&["rev-list", "--cherry-mark", "--cherry", "main...side"]).2, 0);
}
