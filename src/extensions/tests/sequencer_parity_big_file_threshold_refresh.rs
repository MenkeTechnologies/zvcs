//! `cherry-pick`, `revert` and `rebase` die on an unreadable `core.bigFileThreshold`.
//!
//! Each of them refreshes the index before it does anything else, and the first entry whose stat
//! data cannot vouch for it is hashed from the work tree. `index_fd()` reads the threshold for
//! that, and `git_config_ulong()` dies on a value it cannot parse. zvcs never hashed anything on
//! those paths, so the command ran on and conflicted or committed (exit 0/1 where git ends at
//! 128). A work tree file rewritten to the same size is racily clean, which is what forces the
//! compare. Expectations measured from stock git 2.56.0.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const REFUSAL: &str = "fatal: bad numeric config value 'input' for 'core.bigfilethreshold': invalid unit\n";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-seq-bigfile-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&[], &["init", "-q", "-b", "main", "."]);
        for (file, branch) in [("a", "main"), ("b", "side"), ("c", "main")] {
            if branch == "side" {
                f.run(&[], &["checkout", "-q", "-b", "side"]);
            } else if file == "c" {
                f.run(&[], &["checkout", "-q", "main"]);
            }
            std::fs::write(f.root.join(file), format!("{file}\n")).unwrap();
            f.run(&[], &["add", file]);
            f.run(&[], &["commit", "-q", "-m", file]);
        }
        // Same size as the committed `a\n`, written after the index: racily clean.
        std::fs::write(f.root.join("a"), "x\n").unwrap();
        f
    }

    fn run(&self, config: &[&str], args: &[&str]) -> (String, i32) {
        let mut cmd = Command::new(BIN);
        for c in config {
            cmd.args(["-c", c]);
        }
        let out = cmd
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }
}

#[test]
fn the_refresh_hashes_a_racily_clean_entry_and_dies_on_the_threshold() {
    for (i, args) in [vec!["cherry-pick", "side"], vec!["revert", "HEAD"], vec!["rebase", "side"]]
        .into_iter()
        .enumerate()
    {
        let f = Fixture::new(&format!("bad{i}"));
        assert_eq!(f.run(&["core.bigFileThreshold=input"], &args), (REFUSAL.to_string(), 128), "{args:?}");
    }
}
