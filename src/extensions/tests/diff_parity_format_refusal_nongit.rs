//! `git diff` in a repository whose format `verify_repository_format()` refuses.
//!
//! `cmd_diff()` sets up gently (builtin/diff.c:458), so the refusal reaches
//! `read_and_verify_repository_format()` with a non-`NULL` `nongit_ok`
//! (setup.c:771-777): it is a `warning:`, `nongit` is set, and the walk's chdir to
//! the top of the work tree (setup.c:1958-1959) is never undone. `nongit` makes the
//! command `DIFF_NO_INDEX_IMPLICIT` (builtin/diff.c:473-476), so `--cached` is an
//! unknown option of the no-index parser and operands resolve from the top.
//! `setup_diff_pager()` (diff.c:7846-7859) then reads early config, whose
//! `discover_git_directory_reason()` re-finds the repository and warns a second
//! time as `ignoring git dir '.git': …` — unless the exit status was asked for.
//!
//! The port ran the refused repository as a repository: `git diff --cached` exited 0
//! where stock prints the no-index usage and exits 129.
//!
//! Every expectation was measured from stock git 2.56.0 over the same fixture.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const REFUSAL: &str = "repo version is 0, but v1-only extension found:\n\trefstorage\n";

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
    /// `src/conflict.txt` and `outside/drop.txt` committed, then
    /// `extensions.refStorage` added at `core.repositoryFormatVersion = 0`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diff-fmtref-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::create_dir_all(work.join("outside")).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("src/conflict.txt"), "a\n").unwrap();
        std::fs::write(f.work.join("outside/drop.txt"), "b\n").unwrap();
        f.ok(&["add", "."]);
        f.ok(&["commit", "-q", "-m", "base"]);
        f.ok(&["config", "core.repositoryFormatVersion", "0"]);
        f.ok(&["config", "extensions.refStorage", "files"]);
        f
    }

    fn run_in(&self, dir: &str, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(self.work.join(dir))
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join("zvcs"))
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
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) {
        let out = self.run_in(".", args);
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn diff_parity_format_refusal_cached_is_no_index_usage() {
    let f = Fixture::new("cached");
    let out = f.run_in(".", &["diff", "--cached", "--name-status"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(129), "{stderr}");
    let expected_head = format!(
        "warning: {REFUSAL}error: unknown option `cached'\nusage: git diff --no-index [<options>] <path> <path> [<pathspec>...]\n"
    );
    assert!(stderr.starts_with(&expected_head), "{stderr}");
    assert!(out.stdout.is_empty());
}

#[test]
fn diff_parity_format_refusal_operands_resolve_from_top() {
    let f = Fixture::new("top");
    // From `src/`, the operands still name paths from the top of the work tree.
    let out = f.run_in("src", &["diff", "--stat", "src/conflict.txt", "outside/drop.txt"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("warning: {REFUSAL}warning: ignoring git dir '.git': {REFUSAL}")
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        " src/conflict.txt => outside/drop.txt | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n"
    );
}

#[test]
fn diff_parity_format_refusal_exit_status_skips_pager_config() {
    let f = Fixture::new("quiet");
    for flag in ["--quiet", "--exit-code"] {
        let out = f.run_in(".", &["diff", flag, "src/conflict.txt", "outside/drop.txt"]);
        assert_eq!(out.status.code(), Some(1), "{flag}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), format!("warning: {REFUSAL}"), "{flag}");
    }
}
