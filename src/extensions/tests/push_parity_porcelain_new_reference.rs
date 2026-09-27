//! The `--porcelain` summary for a created ref outside `refs/heads/` and
//! `refs/tags/`.
//!
//! `print_ok_ref_status()` picks the summary the same way whatever the format
//! (transport.c:694-699): `[new tag]` under `refs/tags/`, `[new branch]` under
//! `refs/heads/`, `[new reference]` for everything else — the remote-tracking
//! refs a `--mirror` pushes, notes. zvcs's porcelain printer called every
//! non-tag `[new branch]`.
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
            .join(format!("zvcs-push-porcelain-new-ref-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "-b", "main", "../r.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["tag", "v1"]);
        f.run(&["update-ref", "refs/remotes/o/main", "HEAD"]);
        f.run(&["update-ref", "refs/notes/x", "HEAD"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
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

#[test]
fn each_namespace_gets_its_own_summary() {
    let f = Fixture::new("mirror");
    assert_eq!(
        f.run(&["push", "--porcelain", "--mirror", "../r.git"]),
        (
            "To ../r.git\n\
             *\trefs/heads/main:refs/heads/main\t[new branch]\n\
             *\trefs/notes/x:refs/notes/x\t[new reference]\n\
             *\trefs/remotes/o/main:refs/remotes/o/main\t[new reference]\n\
             *\trefs/tags/v1:refs/tags/v1\t[new tag]\n\
             Done\n"
                .to_string(),
            String::new(),
            0
        )
    );
}
