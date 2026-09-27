//! `rev-list --use-bitmap-index` was a usage error.
//!
//! `cmd_rev_list()` takes the option in its leftover loop
//! (builtin/rev-list.c:801-804) and, after `setup_revisions()`, hands the walk
//! to `try_bitmap_count()`/`try_bitmap_disk_usage()`/`try_bitmap_traversal()`
//! (builtin/rev-list.c:923-930). Each asks `prepare_bitmap_walk()`, which
//! answers NULL when no reachability bitmap loads (pack-bitmap.c:2132-2133), and
//! the ordinary walk then runs unchanged. It is also one of the options `-z`
//! refuses (builtin/rev-list.c:876-882).
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
            .join(format!("zvcs-rev-list-use-bitmap-index-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for (file, msg) in [("a", "A"), ("b", "B")] {
            std::fs::write(f.work.join(file), format!("{file}\n")).unwrap();
            f.run(&["add", file]);
            f.run(&["commit", "-q", "-m", msg]);
        }
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

#[test]
fn without_a_bitmap_the_ordinary_walk_answers() {
    let f = Fixture::new("plain");
    for extra in [&[][..], &["--count"], &["--objects"], &["--disk-usage"]] {
        let mut with: Vec<&str> = vec!["rev-list", "--use-bitmap-index"];
        with.extend_from_slice(extra);
        with.push("main");
        let mut without: Vec<&str> = vec!["rev-list"];
        without.extend_from_slice(extra);
        without.push("main");
        let out = f.run(&with);
        assert_eq!(out.2, 0, "{extra:?}: {}", out.1);
        assert_eq!(out, f.run(&without), "{extra:?}");
    }
    let out = f.run(&["rev-list", "--use-bitmap-index", "-z", "main"]);
    assert_eq!(
        out,
        (String::new(), "fatal: -z option used with unsupported option\n".to_string(), 128)
    );
}
