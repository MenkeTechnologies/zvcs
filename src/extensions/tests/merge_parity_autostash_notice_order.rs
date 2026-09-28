//! `Applied autostash.` comes after the merge's own summary.
//!
//! The autostash is re-applied by a `git stash apply` child
//! (sequencer.c:4735-4751), and `start_command()` flushes every stdio buffer
//! before it forks (run-command.c:743). So when stdout and stderr share one
//! pipe — `git pull --autostash 2>&1 | less` — the buffered `Merge made by …`
//! line and diffstat come out ahead of the `Applied autostash.` notice on
//! stderr. zvcs printed the notice first and flushed the summary at exit.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
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
    /// `work` has a local commit and a dirty `f`; `up` has a commit adding `g`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-merge-autostash-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        let up = root.join("up");
        std::fs::create_dir_all(&up).unwrap();
        let f = Fixture { root, work };
        f.git(&up, &["init", "-q", "-b", "main"]);
        std::fs::write(up.join("f"), "a\n").unwrap();
        f.git(&up, &["add", "f"]);
        f.git(&up, &["commit", "-q", "-m", "a"]);
        f.git(&f.root, &["clone", "-q", "up", "work"]);
        std::fs::write(up.join("g"), "b\n").unwrap();
        f.git(&up, &["add", "g"]);
        f.git(&up, &["commit", "-q", "-m", "b"]);
        std::fs::write(f.work.join("l"), "l\n").unwrap();
        f.git(&f.work, &["add", "l"]);
        f.git(&f.work, &["commit", "-q", "-m", "local"]);
        std::fs::write(f.work.join("f"), "a\ndirty\n").unwrap();
        f
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
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
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("LC_ALL", "C");
        c
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        assert!(self.cmd(dir, args).status().unwrap().success(), "git {args:?} failed");
    }

    /// Run with stdout and stderr on one file descriptor, as `2>&1` does.
    fn interleaved(&self, args: &[&str]) -> (String, bool) {
        let log = self.root.join("out.log");
        let file = std::fs::File::create(&log).unwrap();
        let status = self
            .cmd(&self.work, args)
            .stdout(file.try_clone().unwrap())
            .stderr(file)
            .status()
            .unwrap();
        (std::fs::read_to_string(&log).unwrap(), status.success())
    }
}

const TAIL: &str = "Merge made by the 'ort' strategy.\n g | 1 +\n 1 file changed, 1 insertion(+)\n \
create mode 100644 g\nApplied autostash.\n";

#[test]
fn merge_autostash_notice_follows_the_summary() {
    let f = Fixture::new("merge");
    f.git(&f.work, &["fetch", "-q"]);
    let (out, ok) = f.interleaved(&["merge", "--autostash", "origin/main"]);
    assert!(ok);
    assert!(out.ends_with(TAIL), "{out}");
}

#[test]
fn pull_autostash_notice_follows_the_summary() {
    let f = Fixture::new("pull");
    let (out, ok) = f.interleaved(&["pull", "--no-rebase", "--autostash"]);
    assert!(ok);
    assert!(out.ends_with(TAIL), "{out}");
}
