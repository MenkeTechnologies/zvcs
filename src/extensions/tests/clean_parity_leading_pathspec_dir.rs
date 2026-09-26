//! An untracked directory a pathspec only reaches as a leading path.
//!
//! `treat_directory()` asks the pathspec about the directory name with its
//! trailing slash (dir.c:1996-2002). A wildcard spec that does not match `ud/`
//! itself but could match below it — `*f`, `:(glob)**/f` — answers
//! `MATCHED_RECURSIVELY_LEADING_PATHSPEC` (dir.c:486-488), and that case recurses
//! (dir.c:2080-2081) so the matching files are reported one by one. Only a spec
//! that matches the directory itself (`u*`, `ud`, `ud/*`) reports `ud/` whole.
//! zvcs collapsed the directory whenever every file in it matched, so
//! `git clean -n '*f'` said `ud/` and `git clean -f '*f'` removed the whole
//! directory, including files the pathspec never named.
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
    /// A tracked `file`, `*.o` ignored, and untracked `ud/f` plus `ig/q.o`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-clean-leading-pathspec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        std::fs::write(f.work.join(".gitignore"), "*.o\n").unwrap();
        f.run(&["add", "file", ".gitignore"]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::create_dir_all(f.work.join("ud")).unwrap();
        std::fs::write(f.work.join("ud/f"), "u\n").unwrap();
        std::fs::create_dir_all(f.work.join("ig")).unwrap();
        std::fs::write(f.work.join("ig/q.o"), "o\n").unwrap();
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

    fn stdout_ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }
}

#[test]
fn a_leading_wildcard_lists_the_files_inside() {
    let f = Fixture::new("leading");
    assert_eq!(f.stdout_ok(&["clean", "-n", "*f"]), "Would remove ud/f\n");
    assert_eq!(f.stdout_ok(&["clean", "-n", ":(glob)**/f"]), "Would remove ud/f\n");
    assert_eq!(f.stdout_ok(&["clean", "-nx", "*.o"]), "Would remove ig/q.o\n");
    assert_eq!(f.stdout_ok(&["clean", "-nX", "*.o"]), "Would remove ig/q.o\n");
}

#[test]
fn a_spec_matching_the_directory_itself_still_reports_it_whole() {
    let f = Fixture::new("whole");
    assert_eq!(f.stdout_ok(&["clean", "-n", "u*"]), "Would remove ud/\n");
    assert_eq!(f.stdout_ok(&["clean", "-n", "ud"]), "Would remove ud/\n");
    assert_eq!(f.stdout_ok(&["clean", "-n", "ud/*"]), "Would remove ud/\n");
    assert_eq!(f.stdout_ok(&["clean", "-n", ":(glob)ud/*"]), "Would remove ud/\n");
}

#[test]
fn clean_f_removes_the_file_and_leaves_the_directory() {
    let f = Fixture::new("remove");
    std::fs::write(f.work.join("ud/g"), "g\n").unwrap();
    assert_eq!(f.stdout_ok(&["clean", "-f", "*f"]), "Removing ud/f\n");
    assert!(!f.work.join("ud/f").exists());
    assert!(f.work.join("ud/g").exists());
}

#[test]
fn status_names_the_file_under_the_same_pathspec() {
    let f = Fixture::new("status");
    assert_eq!(f.stdout_ok(&["status", "--porcelain", "--", "*f"]), "?? ud/f\n");
    assert_eq!(
        f.stdout_ok(&["status", "--porcelain", "--ignored", "--", "*.o"]),
        "!! ig/q.o\n"
    );
}
