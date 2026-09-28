//! `git symbolic-ref -m ""`.
//!
//! `cmd_symbolic_ref()` checks the reason right after `parse_options()`:
//!
//! ```c
//! if (msg && !*msg)
//!         die("Refusing to perform update with empty message");
//! ```
//!
//! (builtin/symbolic-ref.c:66-67), ahead of the arity checks, so it answers the
//! read, write and delete forms and a missing operand alike, at 128. zvcs wrote
//! the update with an empty reflog message and exited 0.
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
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-symref-empty-msg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.run(&["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "c1"]);
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
fn an_empty_reason_is_refused_before_anything_else() {
    let f = Fixture::new();
    let reflog = std::fs::read(f.work.join(".git/logs/HEAD")).unwrap();
    let refused = (String::new(), "fatal: Refusing to perform update with empty message\n".to_owned(), 128);
    for args in [
        &["symbolic-ref", "-m", "", "HEAD", "refs/heads/other"][..],
        &["symbolic-ref", "-m", "", "HEAD"][..],
        &["symbolic-ref", "-m", ""][..],
        &["symbolic-ref", "-d", "-m", "", "HEAD"][..],
    ] {
        assert_eq!(f.run(args), refused, "{args:?}");
    }
    assert_eq!(f.run(&["symbolic-ref", "HEAD"]).0, "refs/heads/main\n");
    assert_eq!(std::fs::read(f.work.join(".git/logs/HEAD")).unwrap(), reflog);
}
