//! Receive-side hooks run in the git directory with `GIT_DIR=.`, whether the
//! receiving repository is bare or has a work tree.
//!
//! `enter_repo()` (setup.c:1817-1893) settles on the git directory — the
//! `<path>/.git` candidate for a repository with a work tree — `chdir()`s into
//! it and calls `set_git_dir(repo, ".", 0)`. `run_hook_ve()` leaves the child's
//! `dir` unset, so `pre-receive`, `update`, `post-receive`, `post-update` and
//! `proc-receive` all start there with `GIT_DIR=.`.
//!
//! zvcs started them in the work tree with an absolute `GIT_DIR` whenever the
//! repository was not bare, so a hook that ran `git` with a relative path, or
//! relied on `$PWD` being the git directory, saw a different repository layout.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
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
    /// A non-bare `up` whose four receive hooks report where they run, and a
    /// work repository `w` with one commit and a remote `o` at `../up`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-receive-hook-dir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("w")).unwrap();
        let f = Fixture { root };
        f.run("", &["init", "-q", "-b", "main", "up"]);
        f.run("w", &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("w/a"), "a\n").unwrap();
        f.run("w", &["add", "a"]);
        f.run("w", &["commit", "-q", "-m", "a"]);
        f.run("w", &["remote", "add", "o", "../up"]);
        for hook in ["pre-receive", "update", "post-receive", "post-update"] {
            f.hook(
                hook,
                &format!("#!/bin/sh\necho {hook} \"$(basename \"$PWD\")\" \"GIT_DIR=$GIT_DIR\"\ncat >/dev/null\n"),
            );
        }
        f
    }

    fn hook(&self, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.root.join("up/.git/hooks").join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn run(&self, dir: &str, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(self.root.join(dir))
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

/// The `remote:` lines of a push's stderr, trailing padding removed.
fn remote_lines(err: &str) -> Vec<String> {
    err.lines()
        .filter_map(|l| l.strip_prefix("remote: "))
        .map(|l| l.trim_end().to_string())
        .collect()
}

#[test]
fn hooks_of_a_repository_with_a_work_tree_run_in_its_git_directory() {
    let f = Fixture::new("hooks");
    let (_, err, code) = f.run("w", &["push", "o", "main:x"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        remote_lines(&err),
        [
            "pre-receive .git GIT_DIR=.",
            "update .git GIT_DIR=.",
            "post-receive .git GIT_DIR=.",
            "post-update .git GIT_DIR=.",
        ]
    );
}

#[test]
fn proc_receive_runs_there_too() {
    let f = Fixture::new("proc");
    f.run("up", &["config", "receive.procReceiveRefs", "refs/for"]);
    // Report, then take part in no protocol at all: the push is refused either
    // way, and only where it ran is this test's business.
    f.hook(
        "proc-receive",
        "#!/bin/sh\necho proc-receive \"$(basename \"$PWD\")\" \"GIT_DIR=$GIT_DIR\" >&2\nexit 1\n",
    );
    let (_, err, code) = f.run("w", &["push", "o", "main:refs/for/main"]);
    assert_eq!(code, 1, "{err}");
    let lines = remote_lines(&err);
    assert_eq!(lines.first().map(String::as_str), Some("pre-receive .git GIT_DIR=."));
    assert!(lines.iter().any(|l| l == "proc-receive .git GIT_DIR=."), "{err}");
}
