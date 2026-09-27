//! `git status` of an intent-to-add gitlink.
//!
//! `run_diff_files()` tests `ce_intent_to_add()` right after `check_removed()`
//! and before `match_stat_with_submodule()` looks inside a submodule
//! (diff-lib.c:252-269), so a gitlink added with `-N` is queued as a worktree
//! addition at `ce_mode_from_stat(ce, st.st_mode)` — `160000` for a gitlink entry
//! over a directory (read-cache.h:8), whether or not that repository has a
//! commit. `wt_status_collect_changed_cb()` reports it as ` A`.
//!
//! zvcs inspected the entry as a submodule, so `status -s` listed neither
//! repository, and porcelain v2 gave the one without a commit worktree mode
//! `000000` and no `S` flag.
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
            .join(format!("zvcs-status-ita-gitlink-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f
    }

    /// An embedded repository at `name`, with a commit when `born`.
    fn embed(&self, name: &str, born: bool) {
        self.git(&["init", "-q", name]);
        if born {
            self.git(&["-C", name, "commit", "-q", "--allow-empty", "-m", "e"]);
        }
    }

    fn git(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
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
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// Two intent-to-add gitlinks: one over a repository with a commit, one over a
/// repository whose HEAD is unborn.
fn fixture(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    f.embed("emb", true);
    f.embed("emb2", false);
    let (_, _, code) = f.git(&["-c", "advice.addEmbeddedRepo=false", "add", "-N", "emb", "emb2"]);
    assert_eq!(code, 0);
    f
}

#[test]
fn an_intent_to_add_gitlink_is_a_worktree_addition() {
    let f = fixture("short");
    let (out, _, code) = f.git(&["status", "-s"]);
    assert_eq!((out.as_str(), code), (" A emb\n A emb2\n", 0));
}

#[test]
fn porcelain_v2_gives_it_the_gitlink_mode_whatever_its_head() {
    let f = fixture("v2");
    let zero = "0".repeat(40);
    let (out, _, code) = f.git(&["status", "--porcelain=v2"]);
    assert_eq!(
        (out, code),
        (
            format!(
                "1 .A S... 000000 000000 160000 {zero} {zero} emb\n\
                 1 .A S... 000000 000000 160000 {zero} {zero} emb2\n"
            ),
            0
        )
    );
}
