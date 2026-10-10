//! `commit.cleanup=scissors` moves a stopped pick's `# Conflicts:` block below a cut line.
//!
//! `git_sequencer_config()` stores `commit.cleanup` in `opts->default_msg_cleanup`, and
//! `append_conflicts_hint()` consults that mode. zvcs honoured only `--cleanup=scissors`, so a
//! `rebase`, `cherry-pick` or `revert` stopped under the config wrote the plain block into
//! `MERGE_MSG` (and `rebase-merge/message`). Expectations are the literal text measured from
//! stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `main` and `theirs` both rewrite `c`, so replaying either onto the other conflicts.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-seq-scissors-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "."]);
        for (branch, text) in [("main", "base"), ("main", "ours"), ("theirs", "theirs")] {
            if branch == "theirs" {
                f.run(&["checkout", "-q", "-b", "theirs", "HEAD~1"]);
            }
            std::fs::write(f.root.join("c"), format!("{text}\n")).unwrap();
            f.run(&["add", "c"]);
            f.run(&["commit", "-q", "-m", text]);
        }
        f.run(&["checkout", "-q", "main"]);
        f
    }

    fn run(&self, args: &[&str]) -> i32 {
        Command::new(BIN)
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
            .unwrap()
            .status
            .code()
            .expect("no signal")
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(Path::new(&self.root).join(".git").join(rel)).unwrap()
    }
}

const PLAIN: &str = "ours\n\n# Conflicts:\n#\tc\n";
const CUT: &str = "ours\n\n# ------------------------ >8 ------------------------\n\
# Do not modify or remove the line above.\n# Everything below it will be ignored.\n#\n\
# Conflicts:\n#\tc\n";

#[test]
fn rebase_stop_writes_the_block_below_a_cut_line_under_the_config() {
    let f = Fixture::new("rebase");
    assert_eq!(f.run(&["-c", "commit.cleanup=scissors", "rebase", "theirs"]), 1);
    assert_eq!(f.read("MERGE_MSG"), CUT);
    assert_eq!(f.read("rebase-merge/message"), CUT);
}

#[test]
fn last_recognized_value_wins_and_the_default_stays_plain() {
    let f = Fixture::new("last");
    assert_eq!(
        f.run(&["-c", "commit.cleanup=scissors", "-c", "commit.cleanup=strip", "rebase", "theirs"]),
        1
    );
    assert_eq!(f.read("MERGE_MSG"), PLAIN);

    let f = Fixture::new("plain");
    assert_eq!(f.run(&["rebase", "theirs"]), 1);
    assert_eq!(f.read("MERGE_MSG"), PLAIN);
}

#[test]
fn cherry_pick_and_revert_stops_follow_the_config_too() {
    let f = Fixture::new("pick");
    assert_eq!(f.run(&["-c", "commit.cleanup=scissors", "cherry-pick", "theirs"]), 1);
    assert_eq!(f.read("MERGE_MSG"), CUT.replace("ours\n", "theirs\n"));
    let f = Fixture::new("revert");
    assert_eq!(f.run(&["-c", "commit.cleanup=scissors", "revert", "theirs"]), 1);
    assert!(f.read("MERGE_MSG").contains("# ------------------------ >8 ------------------------\n"));
}
