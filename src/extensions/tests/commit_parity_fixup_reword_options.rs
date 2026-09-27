//! `git commit --fixup=reword:<commit>` refuses content and in-progress
//! operations up front.
//!
//! `check_fixup_reword_options()` (builtin/commit.c:1295-1307) runs while the
//! `reword:` suboption is parsed (:1399-1401): in the middle of a merge or a
//! cherry-pick it dies `cannot reword`, a command-line path dies naming the
//! first one, and `-p`/`--interactive`/`-a`/`-i`/`-o` die together — all before
//! `prepare_index()`'s "No paths with --include/--only" (:387-390).
//!
//! zvcs had no `check_fixup_reword_options()`: `--fixup=reword:X -a` committed
//! the work tree's changes under an `amend!` commit, and a path or a merge in
//! progress was accepted the same way.
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
            .join(format!("zvcs-commit-fixup-reword-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("sub/f"), "1\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-q", "-m", "first"]);
        std::fs::write(f.work.join("sub/f"), "2\n").unwrap();
        f.git(&["commit", "-q", "-am", "second"]);
        f
    }

    /// stderr and exit code, with stdout returned separately where it matters.
    fn git(&self, args: &[&str]) -> (String, i32) {
        let out = self.output(args);
        (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }

    fn output(&self, args: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap()
    }

    fn head(&self) -> String {
        String::from_utf8_lossy(&self.output(&["rev-parse", "HEAD"]).stdout).into_owned()
    }
}

const STAGING: &str = "fatal: reword option of '--fixup' and \
                       '--patch/--interactive/--all/--include/--only' cannot be used together\n";

#[test]
fn a_reword_refuses_every_staging_mode_and_records_nothing() {
    let f = Fixture::new("staging");
    std::fs::write(f.work.join("sub/f"), "3\n").unwrap();
    let before = f.head();
    // `--include` without a path is refused here, not by the later
    // "No paths with --include/--only".
    for mode in ["-a", "-o", "-p", "--include"] {
        let got = f.git(&["commit", "--fixup=reword:HEAD~1", "--no-edit", mode]);
        assert_eq!(got, (STAGING.into(), 128), "{mode}");
    }
    assert_eq!(f.head(), before, "no commit was written");
}

#[test]
fn a_reword_refuses_the_first_command_line_path() {
    let f = Fixture::new("path");
    std::fs::write(f.work.join("sub/f"), "3\n").unwrap();
    assert_eq!(
        f.git(&["commit", "--fixup=reword:HEAD~1", "--no-edit", "--", "sub/f", "other"]),
        ("fatal: reword option of '--fixup' and path 'sub/f' cannot be used together\n".into(), 128)
    );
}

#[test]
fn a_reword_is_refused_in_the_middle_of_a_merge() {
    let f = Fixture::new("merge");
    f.git(&["checkout", "-q", "-b", "side", "HEAD~1"]);
    std::fs::write(f.work.join("sub/f"), "side\n").unwrap();
    f.git(&["commit", "-q", "-am", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["merge", "-q", "side"]);
    f.git(&["add", "sub/f"]);
    assert_eq!(
        f.git(&["commit", "--fixup=reword:HEAD", "--no-edit"]),
        ("fatal: You are in the middle of a merge -- cannot reword.\n".into(), 128)
    );
}
