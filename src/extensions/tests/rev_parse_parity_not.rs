//! `git rev-parse --not` flips `show_type` (`show_type ^= REVERSED;`,
//! builtin/rev-parse.c:905-908), and `show_with_type()` prints the `^` when a
//! revision's own type differs from it (:135-140). Every revision after an odd
//! number of `--not`s is therefore printed with the caret inverted: a plain
//! name gains it, a `^name` and the lower end of `a..b` lose it, and the merge
//! bases of `a...b` — `show_rev(REVERSED, …)` at :316-319 — lose theirs too.
//! zvcs refused the option as "not ported yet".
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
        let root = std::env::temp_dir().join(format!("zvcs-rev-parse-not-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("a", "a\n");
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f
    }

    fn write(&self, path: &str, body: &str) {
        let path = self.work.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args, None)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str], stdin: Option<&[u8]>) -> (String, String, i32) {
        use std::io::Write;
        let mut child = Command::new(BIN)
            .args(args)
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
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(stdin.unwrap_or_default()).unwrap();
        drop(pipe);
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

impl Fixture {
    /// `main` is `one` → `two`; `side` stays on `one`.
    fn with_side(tag: &str) -> (Self, String, String) {
        let f = Fixture::new(tag);
        f.write("a", "a2\n");
        f.run(&["commit", "-q", "-am", "two"]);
        f.run(&["branch", "side", "HEAD~1"]);
        let head = f.run(&["rev-parse", "HEAD"]).0.trim().to_string();
        let one = f.run(&["rev-parse", "side"]).0.trim().to_string();
        (f, head, one)
    }
}

#[test]
fn not_inverts_the_caret_until_the_next_not() {
    let (f, head, one) = Fixture::with_side("toggle");
    assert_eq!(
        f.run(&["rev-parse", "--not", "HEAD", "side", "--not", "HEAD~1"]),
        (format!("^{head}\n^{one}\n{one}\n"), String::new(), 0)
    );
    assert_eq!(f.run(&["rev-parse", "--not", "^HEAD"]), (format!("{head}\n"), String::new(), 0));
    // The caret stays outside the quotes under `--sq`.
    assert_eq!(
        f.run(&["rev-parse", "--not", "--sq", "HEAD", "^side"]),
        (format!("^'{head}' '{one}' "), String::new(), 0)
    );
    // `--verify` holds the revision back and still prints it inverted.
    assert_eq!(
        f.run(&["rev-parse", "--not", "--verify", "HEAD"]),
        (format!("^{head}\n"), String::new(), 0)
    );
}

#[test]
fn not_inverts_both_ends_of_a_range_and_the_merge_base() {
    let (f, head, one) = Fixture::with_side("range");
    assert_eq!(
        f.run(&["rev-parse", "--not", "HEAD~1..HEAD"]),
        (format!("^{head}\n{one}\n"), String::new(), 0)
    );
    assert_eq!(
        f.run(&["rev-parse", "--not", "HEAD...side"]),
        (format!("^{one}\n^{head}\n{one}\n"), String::new(), 0)
    );
}
