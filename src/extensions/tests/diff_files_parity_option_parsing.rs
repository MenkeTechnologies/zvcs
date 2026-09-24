//! `git diff-files` stage selection and the short value options the clump
//! expansion splits, measured against stock git 2.55.0.
//!
//! * `-0`/`-1`/`-3` were claimed by the shared count parser and ignored, so every
//!   conflict compared against stage #2. In stock they *are* `revs->max_count`,
//!   which `run_diff_files()` reads as the unmerged stage (diff-lib.c:112-126) —
//!   so `-01`, `-n 3` and `--max-count=1` select a stage too, a count above 3 or
//!   any age is the usage text (builtin/diff-files.c:73-76), and `--base`/`--ours`/
//!   `--theirs` override a count from either side of it (diff-files.c:51-62).
//! * `-l1` was split into `-l` `1`, and the `1` was then read as a revision:
//!   `fatal: ambiguous argument '1'`. `-Ofile` lost its file the same way.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Unmerged `c.txt` with all three stages (`base`, `ours`, `theirs`) and `work` on
/// disk, plus two clean-stage paths `a.txt`/`z.txt` modified in the worktree.
struct Fixture {
    dir: PathBuf,
    base: String,
    ours: String,
    theirs: String,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("zvcs-dfstage-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let mut fx = Fixture { dir, base: String::new(), ours: String::new(), theirs: String::new() };
        fx.ok(&["init", "-q", "-b", "main"]);
        fx.base = fx.blob("base\n");
        fx.ours = fx.blob("ours\n");
        fx.theirs = fx.blob("theirs\n");
        let info = format!(
            "100644 {} 1\tc.txt\n100644 {} 2\tc.txt\n100644 {} 3\tc.txt\n",
            fx.base, fx.ours, fx.theirs
        );
        fx.stdin(&["update-index", "--index-info"], &info);
        fx.write("c.txt", "work\n");
        fx.write("a.txt", "a\n");
        fx.write("z.txt", "z\n");
        fx.ok(&["update-index", "--add", "a.txt", "z.txt"]);
        fx.write("a.txt", "a\nA\n");
        fx.write("z.txt", "z\nZ\n");
        fx
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.dir.join(name), body).unwrap();
    }

    fn run(&self, args: &[&str], input: Option<&str>) -> Output {
        use std::io::Write;
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.unwrap_or("").as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn stdin(&self, args: &[&str], input: &str) -> String {
        let out = self.run(args, Some(input));
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.stdin(args, "")
    }

    fn blob(&self, body: &str) -> String {
        self.stdin(&["hash-object", "-w", "--stdin"], body).trim().to_owned()
    }

    /// The raw `c.txt` records `diff-files <args>` prints.
    fn conflict_lines(&self, args: &[&str]) -> Vec<String> {
        let mut full = vec!["diff-files"];
        full.extend_from_slice(args);
        self.ok(&full)
            .lines()
            .filter(|l| l.ends_with("\tc.txt"))
            .map(str::to_owned)
            .collect()
    }

    fn code(&self, args: &[&str]) -> (i32, String) {
        let mut full = vec!["diff-files"];
        full.extend_from_slice(args);
        let out = self.run(&full, None);
        (out.status.code().unwrap(), String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const NULL: &str = "0000000000000000000000000000000000000000";

fn unmerged() -> String {
    format!(":000000 100644 {NULL} {NULL} U\tc.txt")
}

fn against(id: &str) -> Vec<String> {
    vec![unmerged(), format!(":100644 100644 {id} {NULL} M\tc.txt")]
}

#[test]
fn digit_counts_select_the_unmerged_stage() {
    let fx = Fixture::new("digits");
    assert_eq!(fx.conflict_lines(&[]), against(&fx.ours));
    // Stage 0 never exists for an unmerged path: only the marker is left.
    assert_eq!(fx.conflict_lines(&["-0"]), vec![unmerged()]);
    assert_eq!(fx.conflict_lines(&["-1"]), against(&fx.base));
    assert_eq!(fx.conflict_lines(&["-3"]), against(&fx.theirs));
    // Every spelling of `revs->max_count` is the same selector.
    assert_eq!(fx.conflict_lines(&["-01"]), against(&fx.base));
    assert_eq!(fx.conflict_lines(&["-n", "3"]), against(&fx.theirs));
    assert_eq!(fx.conflict_lines(&["--max-count=1"]), against(&fx.base));
    // `-1` is git's "no count", which is stage 2 again.
    assert_eq!(fx.conflict_lines(&["--max-count=-1"]), against(&fx.ours));
}

#[test]
fn named_selectors_override_a_count_on_either_side() {
    let fx = Fixture::new("named");
    assert_eq!(fx.conflict_lines(&["--base", "-3"]), against(&fx.base));
    assert_eq!(fx.conflict_lines(&["-3", "--base"]), against(&fx.base));
    // The out-of-range count is overwritten before `3 < rev.max_count` runs.
    assert_eq!(fx.conflict_lines(&["--theirs", "-4"]), against(&fx.theirs));
}

#[test]
fn a_count_above_three_or_an_age_is_the_usage_text() {
    let fx = Fixture::new("usage");
    for args in [&["-4"][..], &["-n5"], &["--since=1"], &["--base", "--until=x"]] {
        let (code, err) = fx.code(args);
        assert_eq!(code, 129, "{args:?}");
        assert!(err.starts_with("usage: git diff-files "), "{args:?}: {err}");
    }
}

#[test]
fn short_value_options_keep_a_glued_or_separate_value() {
    let fx = Fixture::new("values");
    let plain = fx.ok(&["diff-files"]);
    assert_eq!(fx.ok(&["diff-files", "-l1"]), plain);
    assert_eq!(fx.ok(&["diff-files", "-l", "1"]), plain);
    assert_eq!(fx.ok(&["diff-files", "-l1k"]), plain);
    assert_eq!(
        fx.code(&["-lx"]),
        (129, "error: switch `l' expects an integer value with an optional k/m/g suffix\n".into())
    );
    assert_eq!(fx.code(&["-l"]), (129, "error: switch `l' requires a value\n".into()));

    fx.write("order", "z.txt\n*\n");
    let names: Vec<String> = fx
        .ok(&["diff-files", "--name-only", "-Oorder"])
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(names, ["z.txt", "a.txt", "c.txt", "c.txt"]);
    assert_eq!(fx.code(&["-O"]), (129, "error: switch `O' requires a value\n".into()));
}
