//! `git range-diff --word-diff` / `--color-words` / `--color-moved`.
//!
//! These options reach the outer diff through the `add_diff_options()` table
//! range-diff parses, and `diff.colorMoved` / `diff.wordRegex` through the
//! `git_diff_ui_config()` it reads first. `diff_flush_patch_all_file_pairs()`
//! collects the whole diff-of-diffs as symbols, marks moved blocks when color
//! is on, and emits (diff.c:7102-7130); `fn_out_consume()` feeds `+`/`-` records
//! to the word diff instead (diff.c:2440-2470). The hunk header keeps range-diff's
//! `suppress_hunk_header_line_count` shape (diff.c:1764-1765) and the content its
//! `dual_color_diffed_diffs` palette (diff.c:1445-1541), each line behind the
//! four-space `output_prefix` (range-diff.c:527-529). zvcs stopped every one of
//! them with `fatal: unsupported flag`, and ignored `diff.colorMoved`.
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
        let root = std::env::temp_dir()
            .join(format!("zvcs-range-diff-word-moved-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    /// `base` → `t1` and `t2`, each writing `files` over it.
    fn series(tag: &str, base: &[(&str, &str)], t1: &[(&str, &str)], t2: &[(&str, &str)]) -> Self {
        let f = Fixture::new(tag);
        f.commit(base, "base", None);
        f.commit(t1, "change", Some("t1"));
        f.run(&["checkout", "-q", "main"]);
        f.commit(t2, "change", Some("t2"));
        f
    }

    fn commit(&self, files: &[(&str, &str)], msg: &str, branch: Option<&str>) {
        if let Some(b) = branch {
            self.run(&["checkout", "-q", "-b", b, "main"]);
        }
        for (name, body) in files {
            std::fs::write(self.work.join(name), body).unwrap();
            self.run(&["add", name]);
        }
        self.run(&["commit", "-q", "-m", msg]);
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@x")
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

/// `a` gets line 3 as `X` / `Y`; `b` gets `TWO` in both, and `t2` also `5`.
fn words(tag: &str) -> Fixture {
    Fixture::series(
        tag,
        &[("a", "1\n2\n3\n4\n5\n6\n7\n8\n"), ("b", "one two three\nfour five\nsix\n")],
        &[("a", "1\n2\nX\n4\n5\n6\n7\n8\n"), ("b", "one TWO three\nfour five\nsix\n")],
        &[("a", "1\n2\nY\n4\n5\n6\n7\n8\n"), ("b", "one TWO three\nfour 5 five\nsix\n")],
    )
}

const A: &str = "alpha line number one is here\nalpha line number two is here\nalpha line number three here\n";
const B: &str = "bravo line number one is here\nbravo line number two is here\nbravo line number three here\n";
const C: &str = "charlie stays in the middle ok\n";

/// `t1` adds the alpha block, a charlie line and the bravo block; `t2` the same
/// lines with alpha and bravo swapped.
fn moved(tag: &str) -> Fixture {
    Fixture::series(
        tag,
        &[("f", "head\n")],
        &[("f", &format!("head\n{A}{C}{B}"))],
        &[("f", &format!("head\n{B}{C}{A}"))],
    )
}

#[test]
fn word_diff_plain() {
    let f = words("plain");
    let want = "\
1:  f6bd514 ! 1:  271a330 change
    @@ a
     1
     2
    -3
    [-+X-]{++Y+}
     4
     5
     6
    @@ a
     ## b ##
    @@
    -one two three
    {+-four five+}
    +one TWO three
    [-four-]{++four 5+} five
     six
";
    for opt in ["--word-diff", "--word-diff=plain"] {
        let (out, err, code) = f.run(&["range-diff", opt, "main..t1", "main..t2"]);
        assert_eq!((out.as_str(), err.as_str(), code), (want, "", 0), "{opt}");
    }
}

#[test]
fn color_words_forces_color() {
    let f = words("color-words");
    let want = "\
\x1b[31m1:  f6bd514 \x1b[m\x1b[33m!\x1b[m\x1b[32m 1:  271a330\x1b[m\x1b[33m change\x1b[m
    \x1b[7m\x1b[36m@@\x1b[m \x1b[ma\x1b[m
     1\x1b[m
     2\x1b[m
    -3\x1b[m
    \x1b[31m+X\x1b[m\x1b[32m+Y\x1b[m
     4\x1b[m
     5\x1b[m
     6\x1b[m
    \x1b[7m\x1b[36m@@\x1b[m \x1b[ma\x1b[m
     ## b ##\x1b[m
    @@\x1b[m
    -one two three\x1b[m
    \x1b[32m-four five\x1b[m
    +one TWO three\x1b[m
    \x1b[31mfour\x1b[m\x1b[32m+four 5\x1b[m five
     six\x1b[m
";
    let (out, err, code) = f.run(&["range-diff", "--color-words", "main..t1", "main..t2"]);
    assert_eq!((out.as_str(), err.as_str(), code), (want, "", 0));
}

#[test]
fn color_moved_under_the_dual_palette() {
    let f = moved("dual");
    let want = "\
\x1b[31m1:  8195006 \x1b[m\x1b[33m!\x1b[m\x1b[32m 1:  43a5c8c\x1b[m\x1b[33m change\x1b[m
    \x1b[7m\x1b[36m@@\x1b[m \x1b[mCommit message\x1b[m
      ## f ##\x1b[m
    \x1b[36m @@\x1b[m
      head\x1b[m
    \x1b[7m\x1b[1;35m-\x1b[m\x1b[2;32m+alpha line number one is here\x1b[m
    \x1b[7m\x1b[1;35m-\x1b[m\x1b[2;32m+alpha line number two is here\x1b[m
    \x1b[7m\x1b[1;35m-\x1b[m\x1b[2;32m+alpha line number three here\x1b[m
    \x1b[7m\x1b[1;34m-\x1b[m\x1b[2;32m+charlie stays in the middle ok\x1b[m
    \x1b[32m +bravo line number one is here\x1b[m
    \x1b[32m +bravo line number two is here\x1b[m
    \x1b[32m +bravo line number three here\x1b[m
    \x1b[7m\x1b[1;36m+\x1b[m\x1b[1;32m+charlie stays in the middle ok\x1b[m
    \x1b[7m\x1b[1;33m+\x1b[m\x1b[1;32m+alpha line number one is here\x1b[m
    \x1b[7m\x1b[1;33m+\x1b[m\x1b[1;32m+alpha line number two is here\x1b[m
    \x1b[7m\x1b[1;33m+\x1b[m\x1b[1;32m+alpha line number three here\x1b[m
";
    for args in [
        &["range-diff", "--creation-factor=200", "--color", "--color-moved=zebra"][..],
        &["-c", "diff.colorMoved=zebra", "range-diff", "--creation-factor=200", "--color"],
    ] {
        let mut args = args.to_vec();
        args.extend_from_slice(&["main..t1", "main..t2"]);
        let (out, err, code) = f.run(&args);
        assert_eq!((out.as_str(), err.as_str(), code), (want, "", 0), "{args:?}");
    }
    // Move detection runs only with color on.
    let (plain, _, _) = f.run(&["range-diff", "--creation-factor=200", "main..t1", "main..t2"]);
    let (out, _, code) =
        f.run(&["range-diff", "--creation-factor=200", "--color-moved", "main..t1", "main..t2"]);
    assert_eq!((out, code), (plain, 0));
}

#[test]
fn color_moved_plain_without_dual_color() {
    let f = moved("simple");
    let want = "\
\x1b[31m1:  8195006 \x1b[m\x1b[33m!\x1b[m\x1b[32m 1:  43a5c8c\x1b[m\x1b[33m change\x1b[m
    \x1b[36m@@\x1b[m \x1b[mCommit message\x1b[m
      ## f ##\x1b[m
     @@\x1b[m
      head\x1b[m
    \x1b[1;35m-+alpha line number one is here\x1b[m
    \x1b[1;35m-+alpha line number two is here\x1b[m
    \x1b[1;35m-+alpha line number three here\x1b[m
    \x1b[1;35m-+charlie stays in the middle ok\x1b[m
     +bravo line number one is here\x1b[m
     +bravo line number two is here\x1b[m
     +bravo line number three here\x1b[m
    \x1b[1;36m+\x1b[m\x1b[1;36m+charlie stays in the middle ok\x1b[m
    \x1b[1;36m+\x1b[m\x1b[1;36m+alpha line number one is here\x1b[m
    \x1b[1;36m+\x1b[m\x1b[1;36m+alpha line number two is here\x1b[m
    \x1b[1;36m+\x1b[m\x1b[1;36m+alpha line number three here\x1b[m
";
    let (out, err, code) = f.run(&[
        "-c",
        "diff.colorMoved=plain",
        "range-diff",
        "--creation-factor=200",
        "--color",
        "--no-dual-color",
        "main..t1",
        "main..t2",
    ]);
    assert_eq!((out.as_str(), err.as_str(), code), (want, "", 0));
}

#[test]
fn a_bad_mode_is_a_parse_error() {
    let f = words("bad");
    let (out, err, code) = f.run(&["range-diff", "--word-diff=bogus", "main..t1", "main..t2"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "error: bad --word-diff argument: bogus\n", 129));
    let (out, err, code) = f.run(&["range-diff", "--word-diff-regex"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "error: option `word-diff-regex' requires a value\n", 129));
}
