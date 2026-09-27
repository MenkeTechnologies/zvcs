//! A switch saw stat-only changes as local modifications.
//!
//! `merge_working_tree()` refreshes the index before anything else
//! (builtin/checkout.c:883, `refresh_index(the_repository->index, REFRESH_QUIET,
//! NULL, NULL, NULL)`), so a file whose mtime moved but whose content still
//! matches the index is repaired in memory, carried through `unpack_trees()`,
//! and written with the new index. `show_local_changes()`'s `diff-index` at the
//! end therefore lists nothing. zvcs skipped the refresh: every touched file came
//! out as `M\t<path>` on stdout, on the switch and on every `git bisect` step
//! (which checks out through the same path), and stayed stat-dirty afterwards.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::fs::File;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

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
    /// `main` has two commits: `a` added, then `b` added.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-checkout-stat-dirty-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "A"]);
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "b"]);
        f.run(&["commit", "-q", "-m", "B"]);
        f
    }

    /// Move `path`'s mtime into the past without touching its content.
    fn touch(&self, path: &str) {
        let file = File::options().write(true).open(self.work.join(path)).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000))
            .unwrap();
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
fn a_touched_file_is_not_a_local_change_on_switch() {
    let f = Fixture::new("switch");
    f.touch("a");
    assert_eq!(f.run(&["diff-files", "--name-only"]).0, "a\n");

    let (out, _, code) = f.run(&["-c", "advice.detachedHead=false", "checkout", "HEAD~1"]);
    assert_eq!((out.as_str(), code), ("", 0));
    // The refreshed stat data was written with the index.
    assert_eq!(f.run(&["diff-files", "--name-only"]).0, "");

    f.touch("a");
    let (out, err, code) = f.run(&["checkout", "main"]);
    assert_eq!(out, "");
    assert!(err.ends_with("Switched to branch 'main'\n"), "{err}");
    assert_eq!(code, 0);
    assert_eq!(f.run(&["diff-files", "--name-only"]).0, "");
}

#[test]
fn a_touched_file_is_not_listed_by_bisect_steps() {
    let f = Fixture::new("bisect");
    std::fs::write(f.work.join("c"), "c\n").unwrap();
    f.run(&["add", "c"]);
    f.run(&["commit", "-q", "-m", "C"]);
    f.touch("a");
    let (out, _, code) = f.run(&["bisect", "start", "main", "main~2"]);
    assert_eq!(code, 0);
    assert!(out.ends_with("] B\n"), "{out}");
    assert!(!out.contains("M\t"), "{out}");
    f.touch("a");
    let (out, err, code) = f.run(&["bisect", "reset"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(err.ends_with("Switched to branch 'main'\n"), "{err}");
}
