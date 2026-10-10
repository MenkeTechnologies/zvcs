//! A rebase that runs the sequencer reads its configuration through `git_sequencer_config()`.
//!
//! `get_replay_opts()` ends its callback chain at `git_diff_basic_config()`, so a diff setting the
//! sequencer cannot parse (`diff.renameLimit=99999999999999999999999999`) is fatal before the
//! rebase touches `HEAD`. zvcs started the replay and only failed, or never noticed, later. A
//! rebase with nothing to replay never builds those options. Expectations measured from stock
//! git 2.56.0.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const REFUSAL: &str = "fatal: bad numeric config value '99999999999999999999999999' for 'diff.renamelimit': out of range\n";

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
        let root = std::env::temp_dir().join(format!("zvcs-rebase-seqcfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&[], &["init", "-q", "-b", "main", "."]);
        for name in ["one", "two", "three"] {
            std::fs::write(f.root.join(name), format!("{name}\n")).unwrap();
            f.run(&[], &["add", name]);
            f.run(&[], &["commit", "-q", "-m", name]);
        }
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
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }
}

#[test]
fn an_interactive_rebase_dies_before_it_moves_head() {
    let f = Fixture::new("bad");
    let before = f.run(&[], &["rev-parse", "HEAD"]);
    assert_eq!(before.1, 0);
    for args in [vec!["rebase", "-i", "HEAD~2"], vec!["rebase", "-i", "--exec", "false", "HEAD~2"]] {
        assert_eq!(f.run(&["diff.renameLimit=99999999999999999999999999"], &args), (REFUSAL.to_string(), 128), "{args:?}");
        assert!(!f.root.join(".git/rebase-merge").exists(), "{args:?} left a rebase behind");
        assert_eq!(f.run(&[], &["symbolic-ref", "HEAD"]).1, 0, "{args:?} detached HEAD");
    }
}

#[test]
fn a_rebase_with_nothing_to_replay_never_reads_it() {
    let f = Fixture::new("noop");
    assert_eq!(f.run(&["diff.renameLimit=99999999999999999999999999"], &["rebase", "HEAD~2"]).1, 0);
}
