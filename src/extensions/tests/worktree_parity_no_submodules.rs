//! `git worktree move` / `remove` on a worktree that holds a submodule.
//!
//! `validate_no_submodules()` (builtin/worktree.c:1203-1242) dies with
//! `working trees containing submodules cannot be moved or removed` when the
//! worktree's administrative directory has a `modules/` directory, or its index
//! names a gitlink whose checkout is populated (`is_submodule_populated_gently()`,
//! submodule.c:295-305: `<path>/.git` is a git directory or a gitfile that
//! resolves). `move_worktree()` calls it unconditionally (worktree.c:1289) — not
//! even `-f -f` gets past it — and `remove_worktree()` reaches it through
//! `check_clean_worktree()` (:1336), which `--force` skips. zvcs moved such a
//! worktree and removed it without `--force`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const REFUSAL: &str = "fatal: working trees containing submodules cannot be moved or removed\n";

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
    /// `super` records a submodule `sm` cloned from the local `subsrc`, and has a
    /// linked worktree `../w1` with nothing populated in it yet.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-worktree-no-submodules-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("super");
        let sub = root.join("subsrc");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        let f = Fixture { root, work };
        f.run_in(&sub, &["init", "-q", "-b", "main", "."]);
        std::fs::write(sub.join("s"), "s\n").unwrap();
        f.run_in(&sub, &["add", "s"]);
        f.run_in(&sub, &["commit", "-q", "-m", "s1"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("x"), "x\n").unwrap();
        f.run(&["add", "x"]);
        f.run(&["commit", "-q", "-m", "base"]);
        assert_eq!(f.run(&["-c", "protocol.file.allow=always", "submodule", "add", "-q", "../subsrc", "sm"]).2, 0);
        f.run(&["commit", "-q", "-m", "sm"]);
        assert_eq!(f.run(&["worktree", "add", "-q", "../w1"]).2, 0);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
fn an_absorbed_submodule_blocks_move_even_forced_and_remove_unforced() {
    let f = Fixture::new("absorbed");
    let w1 = f.root.join("w1");
    // `submodule update --init` in the worktree absorbs into worktrees/w1/modules/sm.
    assert_eq!(
        f.run_in(&w1, &["-c", "protocol.file.allow=always", "submodule", "update", "--init", "-q"]).2,
        0
    );
    assert!(f.work.join(".git/worktrees/w1/modules").is_dir());

    for args in [&["worktree", "move", "../w1", "../w2"][..], &["worktree", "move", "-f", "-f", "../w1", "../w2"]] {
        assert_eq!(f.run(args), (String::new(), REFUSAL.to_string(), 128), "{args:?}");
    }
    assert!(w1.join("sm/s").exists());
    assert!(!f.root.join("w2").exists());

    assert_eq!(f.run(&["worktree", "remove", "../w1"]), (String::new(), REFUSAL.to_string(), 128));
    assert!(w1.join("sm/s").exists());
    // `--force` skips `check_clean_worktree()`, and with it the submodule check.
    assert_eq!(f.run(&["worktree", "remove", "-f", "../w1"]), (String::new(), String::new(), 0));
    assert!(!w1.exists());
    assert!(!f.work.join(".git/worktrees").exists());
}

#[test]
fn a_populated_gitlink_counts_and_a_dangling_gitfile_does_not() {
    let f = Fixture::new("gitlink");
    let w1 = f.root.join("w1");
    // A plain clone at the gitlink's path: no `modules/`, but `sm/.git` is a repository.
    f.run(&["clone", "-q", "../subsrc", "../w1/sm"]);
    assert!(!f.work.join(".git/worktrees/w1/modules").exists());
    assert_eq!(f.run(&["worktree", "move", "../w1", "../w2"]), (String::new(), REFUSAL.to_string(), 128));
    assert_eq!(f.run(&["worktree", "remove", "../w1"]), (String::new(), REFUSAL.to_string(), 128));

    // A gitfile that resolves nowhere is not a populated submodule.
    std::fs::remove_dir_all(w1.join("sm/.git")).unwrap();
    std::fs::write(w1.join("sm/.git"), "gitdir: /nonexistent\n").unwrap();
    assert_eq!(f.run(&["worktree", "move", "../w1", "../w2"]), (String::new(), String::new(), 0));
    assert!(f.root.join("w2/sm/.git").is_file());
}
