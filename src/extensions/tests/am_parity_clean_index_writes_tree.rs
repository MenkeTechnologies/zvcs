//! `am --abort` / `am --skip` write the index they unwind out as a tree.
//!
//! Both end in `clean_index(head, remote)` (builtin/am.c:2071-2107), which is
//! four unpack steps, not one reset: `fast_forward_to(head, head, 1)`, then
//! `write_index_as_tree()`, then `fast_forward_to(index_tree, remote, 0)`, then
//! `merge_tree(remote)`. The middle step stores the index — still holding what
//! the refused patch staged — as a tree object. After a `pre-applypatch`
//! refusal the patch's paths are staged, so that tree is new, and it stays in
//! the object store once the index is unwound. zvcs reset with a single
//! `read-tree -u --reset`, reaching the same index and worktree but writing no
//! tree.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The tree of `file` + `other` + `veto.txt`: the index the refused patch left
/// staged. No commit holds it, so only `clean_index()` can have written it.
const STAGED_TREE: &str = "789c8afcea1c11bb8e7eb54c55e3093f23c9a436";
/// `main`'s tip, where HEAD stays.
const MAIN: &str = "4d2c9eae48c22ae3e700805878c16e1787941ed2";

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
    /// `main` holds `file` and then `other`; `veto`, forked before `other`, adds
    /// `veto.txt`, exported as one patch in `<root>/p`. A `pre-applypatch` hook
    /// refuses any patch that brings `veto.txt`, after it has been applied to the
    /// index and worktree.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-am-clean-index-tree-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.ok(&["add", "file"]);
        f.ok(&["commit", "-q", "-m", "base"]);
        f.ok(&["checkout", "-q", "-b", "veto"]);
        std::fs::write(f.work.join("veto.txt"), "stop\n").unwrap();
        f.ok(&["add", "veto.txt"]);
        f.ok(&["commit", "-q", "-m", "trips"]);
        f.ok(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("other"), "other\n").unwrap();
        f.ok(&["add", "other"]);
        f.ok(&["commit", "-q", "-m", "other"]);
        f.ok(&["format-patch", "-q", "-o", "../p", "-1", "veto"]);
        let hook = f.work.join(".git/hooks/pre-applypatch");
        std::fs::write(&hook, "#!/bin/sh\ntest -f veto.txt && exit 1\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        f
    }

    fn run(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_EDITOR", "true")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code().unwrap_or(-1))
    }

    fn ok(&self, args: &[&str]) -> String {
        let (stdout, code) = self.run(args);
        assert_eq!(code, 0, "git {args:?}");
        stdout
    }

    /// `am` stops on the hook's refusal with `veto.txt` staged and no tree of
    /// that index written yet; then `op` unwinds it.
    fn refuse_then(&self, op: &str) {
        let patch = std::fs::read_dir(self.root.join("p")).unwrap().next().unwrap().unwrap().path();
        let (_, code) = self.run(&["am", patch.to_str().unwrap()]);
        assert_eq!(code, 1);
        assert_eq!(self.ok(&["status", "--porcelain"]), "A  veto.txt\n");
        assert_ne!(self.run(&["cat-file", "-e", STAGED_TREE]).1, 0, "no tree before {op}");

        self.ok(&["am", op]);
        assert_eq!(self.ok(&["cat-file", "-t", STAGED_TREE]), "tree\n");
        assert_eq!(self.ok(&["status", "--porcelain"]), "");
        assert_eq!(self.ok(&["rev-parse", "HEAD"]), format!("{MAIN}\n"));
        assert!(!self.work.join("veto.txt").exists());
        assert!(!self.work.join(".git/rebase-apply").exists());
    }
}

#[test]
fn abort_after_pre_applypatch_refusal_stores_the_staged_tree() {
    Fixture::new("abort").refuse_then("--abort");
}

#[test]
fn skip_after_pre_applypatch_refusal_stores_the_staged_tree() {
    Fixture::new("skip").refuse_then("--skip");
}
