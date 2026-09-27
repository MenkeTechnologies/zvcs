//! `git clean` run from inside a directory that is itself a deletion candidate.
//!
//! `fill_directory()` has no notion of the working directory, so the untracked
//! directory the command runs in is listed like any other, as `rel =
//! relative_path(ent->name, prefix)` — `./` for the directory itself
//! (builtin/clean.c:1043). `abs_path` is `prefix` + `rel` (builtin/clean.c:1053-1056),
//! and `remove_dirs()` extends that buffer entry by entry and prints each path as
//! `quote_path(path, prefix)`. When the directory it empties is the original
//! working directory it refuses the `rmdir` (builtin/clean.c:252-265) — `Would
//! refuse to remove current working directory` — and, the directory not being
//! gone, lists what did go: `./f`, or `../deep/z` when the candidate was reached
//! as `../`.
//!
//! zvcs refused the whole command ("not supported").
//!
//! Every directory here holds at most one entry, so readdir order cannot vary.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
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
    /// Tracked `t`; untracked `ud/f`, `ux/y`, `ui/deep/z` and the empty `e/`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-clean-inside-candidate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(".", &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("t"), "a\n").unwrap();
        f.run(".", &["add", "t"]);
        f.run(".", &["commit", "-q", "-m", "a"]);
        for (path, body) in [("ud/f", "x\n"), ("ux/y", "x\n"), ("ui/deep/z", "x\n")] {
            let p = f.work.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        std::fs::create_dir_all(f.work.join("e")).unwrap();
        f
    }

    fn run(&self, cwd: &str, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(self.work.join(cwd))
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

    fn exists(&self, path: &str) -> bool {
        Path::new(&self.work.join(path)).exists()
    }
}

const WOULD_REFUSE: &str = "Would refuse to remove current working directory\n";

#[test]
fn the_working_directory_is_listed_and_refused() {
    let f = Fixture::new("dry");
    let (out, err, code) = f.run("ud", &["clean", "-nd"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (format!("{WOULD_REFUSE}Would remove ./f\n").as_str(), "", 0)
    );

    // Reached through a pathspec from the top, the same directory is `./` too.
    let (out, err, code) = f.run("ud", &["clean", "-nd", "../u*/"]);
    let want = format!("{WOULD_REFUSE}Would remove ./f\nWould remove ../ui/\nWould remove ../ux/\n");
    assert_eq!((out.as_str(), err.as_str(), code), (want.as_str(), "", 0));

    // An empty working directory has nothing to list beyond the refusal.
    let (out, err, code) = f.run("e", &["clean", "-nd"]);
    assert_eq!((out.as_str(), err.as_str(), code), (WOULD_REFUSE, "", 0));
}

#[test]
fn a_parent_candidate_renders_its_contents_through_the_parent() {
    let f = Fixture::new("parent");
    // `ui/` is the candidate, `rel` is `../`, and `ui/deep/../deep/z` relative to
    // `ui/deep/` is `../deep/z`. `ui/` itself survives, so it gets no line.
    let (out, err, code) = f.run("ui/deep", &["clean", "-nd", ".."]);
    let want = format!("{WOULD_REFUSE}Would remove ../deep/z\n");
    assert_eq!((out.as_str(), err.as_str(), code), (want.as_str(), "", 0));
}

#[test]
fn a_real_run_empties_the_working_directory_but_keeps_it() {
    let f = Fixture::new("real");
    let (out, err, code) = f.run("ud", &["clean", "-fd"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("Refusing to remove current working directory\nRemoving ./f\n", "", 0)
    );
    assert!(f.exists("ud"));
    assert!(!f.exists("ud/f"));
    assert!(f.exists("ux/y"));
}
