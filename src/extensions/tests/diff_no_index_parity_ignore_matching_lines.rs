//! `git diff --no-index` with `-I<re>` and the rest of `diff_from_contents`.
//!
//! `-I` / `--ignore-matching-lines` is on `add_diff_options()`'s table, so the
//! no-index parser takes it: `diff_opt_ignore_regex()` (diff.c:5859-5877) appends
//! one `REG_EXTENDED | REG_NEWLINE` pattern per occurrence and a bad one is the
//! callback's `error()`, exit 129. zvcs refused the option outright with
//! `unsupported option`.
//!
//! A pattern list raises `flags.diff_from_contents` (diff.c:5282-5284), as the
//! whitespace family does, and two consequences of that flag were missing for
//! every one of them:
//!
//! * `diff_flush()` runs `diff_flush_patch_quietly()` over each pair before
//!   `--raw` prints it (diff.c:7210-7212), and `run_diff()` fills both sides'
//!   ids on the way (diff.c:5049-5050), so `--raw` shows real ids, not zeros.
//! * `builtin_diffstat()` omits a modified text pair that counted no line either
//!   way (diff.c:4255-4273), so `--stat`/`--numstat` drop it and `--shortstat`
//!   prints nothing at all (diff.c:3225-3226).
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

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
    /// No repository. `A`→`B` changes a `v1` line and the last line; `A`→`C`
    /// changes only the `v1` line. `W1`→`W2` differs only in whitespace,
    /// `W1`→`W3` in content.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-no-index-ignore-lines-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let w = |p: &str, d: &str| std::fs::write(root.join(p), d).unwrap();
        w("A", "a\nv1 x\nb\nc\nd\ne\nf\ng\nh\ni\n");
        w("B", "a\nv1 y\nb\nc\nd\ne\nf\ng\nh\nZ\n");
        w("C", "a\nv1 y\nb\nc\nd\ne\nf\ng\nh\ni\n");
        w("W1", "a\nv1  x\n");
        w("W2", "a\nv1 x\n");
        w("W3", "a\nv1 y\n");
        Fixture { root }
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(["diff", "--no-index"])
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", self.root.parent().unwrap())
            .env("LC_ALL", "C")
            .env_remove("COLUMNS")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// stdout and exit code of a run that must not write to stderr.
    fn clean(&self, args: &[&str]) -> (String, i32) {
        let (out, err, code) = self.run(args);
        assert_eq!(err, "", "{args:?}");
        (out, code)
    }
}

const A_B_TAIL_ONLY: &str = "diff --git a/A b/B
index 629aeec..95f819f 100644
--- a/A
+++ b/B
@@ -7,4 +7,4 @@ e
 f
 g
 h
-i
+Z
";

#[test]
fn every_spelling_drops_the_matching_hunk() {
    let f = Fixture::new("spell");
    for args in [
        &["-I", "v1", "A", "B"][..],
        &["-Iv1", "A", "B"],
        &["--ignore-matching-lines=v1", "A", "B"],
        &["--ignore-matching-lines", "v1", "A", "B"],
        // Every occurrence appends; a pattern that matches nothing changes nothing.
        &["-Iv1", "-Inomatch", "A", "B"],
    ] {
        assert_eq!(f.clean(args), (A_B_TAIL_ONLY.to_owned(), 1), "{args:?}");
    }
}

/// A change only partly made of matching lines is kept whole.
#[test]
fn a_pattern_must_cover_every_line_of_the_change() {
    let f = Fixture::new("partial");
    let (out, code) = f.clean(&["-Ia", "A", "B"]);
    assert_eq!(code, 1);
    assert!(out.contains("-v1 x\n+v1 y\n"), "{out}");
    assert!(out.contains("-i\n+Z\n"), "{out}");
}

#[test]
fn a_fully_ignored_pair_is_no_change_in_any_format() {
    let f = Fixture::new("none");
    for fmt in ["-p", "--stat", "--numstat", "--shortstat", "--raw", "--name-status", "--quiet", "--exit-code"] {
        assert_eq!(f.clean(&["-Iv1", fmt, "A", "C"]), (String::new(), 0), "{fmt}");
    }
}

#[test]
fn bad_and_missing_values_are_option_errors() {
    let f = Fixture::new("err");
    let (out, err, code) = f.run(&["-I(", "A", "B"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "error: invalid regex given to -I: '('\n", 129));
    let (_, err, code) = f.run(&["-I"]);
    assert_eq!((err.lines().next(), code), (Some("error: switch `I' requires a value"), 129));
    let (_, err, code) = f.run(&["--ignore-matching-lines"]);
    assert_eq!(
        (err.lines().next(), code),
        (Some("error: option `ignore-matching-lines' requires a value"), 129)
    );
}

/// `--raw` under `diff_from_contents` prints the ids `diff_fill_oid_info()`
/// hashed; without the flag a no-index side has none.
#[test]
fn raw_shows_hashed_ids_once_contents_decide() {
    let f = Fixture::new("raw");
    assert_eq!(
        f.clean(&["-Iv1", "--raw", "A", "B"]),
        (":100644 100644 629aeec 95f819f M\tA\n".to_owned(), 1)
    );
    assert_eq!(
        f.clean(&["-b", "--raw", "W1", "W3"]),
        (":100644 100644 8ab2899 b2ca08e M\tW1\n".to_owned(), 1)
    );
    assert_eq!(
        f.clean(&["--raw", "W1", "W3"]),
        (":100644 100644 0000000 0000000 M\tW1\n".to_owned(), 1)
    );
}

/// `builtin_diffstat()` drops a modified pair that counted nothing.
#[test]
fn stat_family_omits_a_pair_whose_change_was_discounted() {
    let f = Fixture::new("stat");
    assert_eq!(f.clean(&["-b", "--stat", "W1", "W2"]), (String::new(), 0));
    assert_eq!(f.clean(&["-b", "--numstat", "--shortstat", "W1", "W2"]), (String::new(), 0));
    assert_eq!(
        f.clean(&["-I", "v1", "--stat", "A", "B"]),
        (" A => B | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n".to_owned(), 1)
    );
}
