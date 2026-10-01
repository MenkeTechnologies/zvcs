//! `rev-list` takes the diff options `setup_revisions()` hands to
//! `diff_opt_parse()` (revision.c:2758-2762), and only afterwards refuses the
//! diff they asked for (`revs.diff`, builtin/rev-list.c:926-933).
//!
//! zvcs treated every one of them as an unknown flag: `-S edited` left `edited`
//! to be resolved as a revision (`fatal: ambiguous argument`, 128) where stock
//! consumes it as the needle and answers with the usage block (129), and options
//! that ask for no diff at all (`-M`, `-O <file>`, `--pickaxe-regex`, `-s`) were
//! refused instead of accepted.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const ONE: &str = "53181693e090b76ba96ba743759c4712782043c6";
const TWO: &str = "e55e71abcb191113759133627f75e8660b385399";

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
    /// Two commits on `main`, the second changing `a` from `one` to `edited`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rl-diff-options-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "one\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("a"), "edited\n").unwrap();
        f.run(&["commit", "-q", "-a", "-m", "two"]);
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
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn usage(&self, args: &[&str]) {
        let (out, err, code) = self.run(args);
        assert_eq!((out.as_str(), code), ("", 129), "{args:?}: {err}");
        assert!(err.starts_with("usage: git rev-list "), "{args:?}: {err}");
    }
}

/// The needle is the option's value, and a pickaxe asks for a diff.
#[test]
fn a_pickaxe_takes_its_value_and_is_refused_with_the_usage() {
    let f = Fixture::new("pickaxe");
    f.usage(&["rev-list", "-S", "edited", "main"]);
    f.usage(&["rev-list", "--count", "-G", "edited", "main"]);
    f.usage(&["rev-list", "-Sedited", "main"]);
    // An output format, and `-p` beside a letter the diff table lacks.
    f.usage(&["rev-list", "--stat", "main"]);
    f.usage(&["rev-list", "-pq", "main"]);
    // `-q` was never rev-list's: only the diff table's `--quiet` is.
    f.usage(&["rev-list", "-q", "main"]);
}

/// Options that set no output format, pickaxe, filter or follow are accepted.
#[test]
fn diff_options_that_ask_for_no_diff_are_accepted() {
    let f = Fixture::new("accepted");
    let walk = format!("{TWO}\n{ONE}\n");
    for args in [
        &["rev-list", "-M", "main"][..],
        &["rev-list", "-O", "x", "--pickaxe-regex", "--no-color", "-l5", "main"][..],
        // `-s` overwrites the format `-p` and `--name-only` set.
        &["rev-list", "-p", "-s", "main"][..],
        &["rev-list", "--name-only", "-s", "main"][..],
    ] {
        assert_eq!(f.run(args), (walk.clone(), String::new(), 0), "{args:?}");
    }
    assert_eq!(f.run(&["rev-list", "--quiet", "main"]), (String::new(), String::new(), 0));
}

/// parse-options' own refusals and the callbacks' `error()`s, at 129; argv ends
/// at `--` for a detached value.
#[test]
fn value_errors_are_raised_while_parsing() {
    let f = Fixture::new("errors");
    let cases: &[(&[&str], &str)] = &[
        (&["rev-list", "-S"], "error: switch `S' requires a value\n"),
        (&["rev-list", "-S", "--", "a"], "error: switch `S' requires a value\n"),
        (&["rev-list", "-raw", "main"], "error: did you mean `--raw` (with two dashes)?\n"),
        (&["rev-list", "--unified=x", "main"], "error: --unified expects a numerical value\n"),
        (&["rev-list", "--patch=x", "main"], "error: option `patch' takes no value\n"),
        (
            &["rev-list", "--diff-filter=Q", "main"],
            "error: unknown change class 'Q' in --diff-filter=Q\n",
        ),
    ];
    for (args, err) in cases {
        assert_eq!(f.run(args), (String::new(), (*err).to_string(), 129), "{args:?}");
    }
}

/// `diff_setup_done()`'s `die()`s run at the end of `setup_revisions()`, ahead of
/// rev-list's own usage.
#[test]
fn diff_setup_done_conflicts_are_fatal() {
    let f = Fixture::new("setup-done");
    let cases: &[(&[&str], &str)] = &[
        (
            &["rev-list", "-Sx", "-Gy", "main"],
            "fatal: options '-G', '-S', and '--find-object' cannot be used together\n",
        ),
        (&["rev-list", "--follow", "main"], "fatal: --follow requires exactly one pathspec\n"),
        (
            &["rev-list", "--max-depth=1", "main", "--", "*"],
            "fatal: max-depth cannot be used with wildcard pathspecs\n",
        ),
    ];
    for (args, err) in cases {
        assert_eq!(f.run(args), (String::new(), (*err).to_string(), 128), "{args:?}");
    }
}
