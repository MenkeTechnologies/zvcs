//! `git update-ref --stdin --batch-updates`: how the rejections are reported.
//!
//! `print_rejected_refs()` (builtin/update-ref.c:246-266) runs once the batch is
//! through, over the rejections in the order `files_transaction_prepare()` made
//! them — every queued update in turn, then the updates split off from them — and
//! writes each one's `error()` to stderr and its `rejected` line into stdout's
//! buffer, so a caller reading `2>&1` off a terminal gets every error line first.
//! The old-value column is `old_oid ? oid_to_hex(old_oid) : old_target`, and an
//! update given no old value leaves `REF_HAVE_OLD` unset (refs.c:1422-1423,
//! :3050-3054): both are NULL, printed `(null)`. zvcs interleaved the two streams,
//! reported a failed `create` ahead of the updates before it, and printed the zero
//! id for a missing old value.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
        let root = std::env::temp_dir().join(format!("zvcs-update-ref-batch-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "c1"]);
        f.git(&["branch", "b2"]);
        f.git(&["symbolic-ref", "refs/heads/s1", "refs/heads/main"]);
        f.git(&["symbolic-ref", "refs/heads/loop", "refs/heads/loop2"]);
        f.git(&["symbolic-ref", "refs/heads/loop2", "refs/heads/loop"]);
        f
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C");
        cmd
    }

    fn git(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.cmd().args(args).output().unwrap().stdout).into_owned()
    }

    /// `update-ref --stdin --batch-updates` with stdout and stderr on one file.
    fn batch(&self, input: &str) -> (String, i32) {
        let sink = self.root.join("capture");
        let out = std::fs::File::create(&sink).unwrap();
        let mut child = self
            .cmd()
            .args(["update-ref", "--stdin", "--batch-updates"])
            .stdin(Stdio::piped())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let code = child.wait().unwrap().code().expect("no signal");
        (std::fs::read_to_string(&sink).unwrap(), code)
    }
}

#[test]
fn errors_first_then_the_lines_in_prepare_order() {
    let f = Fixture::new();
    let a = f.git(&["rev-parse", "HEAD"]);
    let a = a.trim();
    let bogus = "1111111111111111111111111111111111111111";
    let zero = "0000000000000000000000000000000000000000";
    let input = format!(
        "update refs/heads/s1 {a}\nupdate refs/heads/main {a}\ndelete refs/heads/nosuch\n\
         create refs/heads/b2 {a}\nupdate refs/heads/x {a} {bogus}\nupdate refs/heads/loop {a}\n"
    );
    assert_eq!(
        f.batch(&input),
        (
            format!(
                "error: multiple updates for 'refs/heads/main' (including one via symref 'refs/heads/s1') are not allowed\n\
                 error: cannot lock ref 'refs/heads/b2': reference already exists\n\
                 error: cannot lock ref 'refs/heads/x': unable to resolve reference 'refs/heads/x'\n\
                 error: multiple updates for 'refs/heads/loop' (including one via symref 'refs/heads/loop2') are not allowed\n\
                 rejected refs/heads/s1 {a} (null) refname conflict\n\
                 rejected refs/heads/b2 {a} {zero} reference already exists\n\
                 rejected refs/heads/x {a} {bogus} reference does not exist\n\
                 rejected refs/heads/loop2 {a} (null) refname conflict\n"
            ),
            0
        )
    );
}
