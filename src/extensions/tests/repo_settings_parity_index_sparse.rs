//! Three `prepare_repo_settings()` booleans that were never validated.
//!
//! `repo_cfg_bool()` dies on a value `git_parse_maybe_bool()` refuses, and
//! `prepare_repo_settings()` reads `core.multipackindex`, `index.sparse` and
//! `core.usereplacerefs` that way (repo-settings.c:79, :80, :86). So
//! `index.sparse = all` stops `rebase -i` before it touches anything, and
//! `-c core.multiPackIndex=bogus rev-parse --git-dir` is fatal. zvcs skipped
//! the three keys and ran the command.
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
    /// `main` and `theirs` both rewrite `file` from a common base.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-repo-settings-bools-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "theirs"]);
        std::fs::write(f.work.join("file"), "theirs\n").unwrap();
        f.run(&["commit", "-q", "-am", "theirs"]);
        f.run(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("file"), "ours\n").unwrap();
        f.run(&["commit", "-q", "-am", "ours"]);
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
            .env("GIT_MERGE_AUTOEDIT", "no")
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
fn index_sparse_stops_an_interactive_rebase_before_it_starts() {
    let f = Fixture::new("sparse");
    let head = f.run(&["rev-parse", "HEAD"]).0;
    let (out, err, code) =
        f.run(&["-c", "index.sparse=all", "rebase", "-i", "--exec", "false", "HEAD~1"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: bad boolean config value 'all' for 'index.sparse'\n", 128)
    );
    assert!(!f.work.join(".git/rebase-merge").exists());
    assert_eq!(f.run(&["rev-parse", "HEAD"]).0, head);
}

#[test]
fn each_key_is_fatal_for_rev_parse() {
    let f = Fixture::new("keys");
    for (key, lower) in [
        ("core.multiPackIndex", "core.multipackindex"),
        ("index.sparse", "index.sparse"),
        ("core.useReplaceRefs", "core.usereplacerefs"),
    ] {
        let (out, err, code) = f.run(&["-c", &format!("{key}=bogus"), "rev-parse", "--git-dir"]);
        let want = format!("fatal: bad boolean config value 'bogus' for '{lower}'\n");
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{key}");
        // A value git reads as a boolean passes.
        let (out, _, code) = f.run(&["-c", &format!("{key}=0x10"), "rev-parse", "--git-dir"]);
        assert_eq!((out.as_str(), code), (".git\n", 0), "{key}");
    }
}
