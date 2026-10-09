//! `git checkout` judged the `--pathspec-from-file` family in the wrong order.
//!
//! `--pathspec-file-nul` without `--pathspec-from-file` is a fatal in
//! `cmd_checkout()` ahead of every other gate; zvcs ignored the flag and ran
//! the checkout. With `--pathspec-from-file`, a pathspec argument and
//! `--detach` are refused before the file is opened, while branch creation and
//! `--ours/--theirs` are refused only after it was read. Expectations measured
//! from stock git 2.56.0.

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-checkout-pathspec-gates-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["branch", "side"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
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

const NUL_NEEDS_FILE: &str = "fatal: the option '--pathspec-file-nul' requires '--pathspec-from-file'\n";

#[test]
fn file_nul_without_from_file_is_fatal_before_any_other_work() {
    let f = Fixture::new("nul");
    for args in [
        vec!["checkout", "--pathspec-file-nul"],
        vec!["checkout", "--pathspec-file-nul", "side"],
        vec!["checkout", "--pathspec-file-nul", "-b", "new"],
        vec!["checkout", "--pathspec-file-nul", "--detach"],
        vec!["checkout", "-p", "--pathspec-file-nul"],
        vec!["checkout", "--pathspec-file-nul", "--", "a"],
    ] {
        assert_eq!(f.run(&args), (NUL_NEEDS_FILE.to_string(), 128), "{args:?}");
    }
    // The refused invocations created nothing and moved nothing.
    assert_eq!(f.run(&["rev-parse", "--verify", "-q", "refs/heads/new"]).1, 1);
    assert_eq!(f.run(&["symbolic-ref", "HEAD"]).1, 0);
}

#[test]
fn from_file_gates_run_in_stock_order() {
    let f = Fixture::new("order");
    let cannot_open = "fatal: could not open 'x' for reading: No such file or directory\n";
    let cases: [(&[&str], &str); 5] = [
        (&["a"], "fatal: '--pathspec-from-file' and pathspec arguments cannot be used together\n"),
        (&["--", "a"], "fatal: '--pathspec-from-file' and pathspec arguments cannot be used together\n"),
        (&["--detach"], "fatal: options '--pathspec-from-file' and '--detach' cannot be used together\n"),
        // Branch creation and stage sides are refused only once the file was read.
        (&["-b", "n"], cannot_open),
        (&["--ours"], cannot_open),
    ];
    for (extra, want) in cases {
        let mut args = vec!["checkout", "--pathspec-from-file=x"];
        args.extend_from_slice(extra);
        assert_eq!(f.run(&args), (want.to_string(), 128), "{args:?}");
    }
    // A lone revision is still the source tree-ish; the missing file is the error.
    assert_eq!(f.run(&["checkout", "--pathspec-from-file=x", "side"]), (cannot_open.to_string(), 128));
}
