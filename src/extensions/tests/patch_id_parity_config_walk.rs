//! `git_patch_id_config()` is a config callback, not a pair of lookups.
//!
//! `cmd_patch_id()` runs `repo_config(the_repository, git_patch_id_config,
//! &config)` before `parse_options()` (builtin/patch-id.c:238-244). The callback
//! reads `patchid.stable` and `patchid.verbatim` with `git_config_bool()` and hands
//! every other key to `git_default_config()` (builtin/patch-id.c:204-219). So:
//!
//! * a value `git_config_bool()` refuses is fatal, ahead of `-h` and of an
//!   option conflict, inside a repository and outside one;
//! * `1k` is a boolean (`git_parse_maybe_bool` falls back to the integer grammar);
//! * every occurrence is walked in parse order, so the first bad value wins even
//!   when a later one overrides it, and a `core.*` refusal is reported only where
//!   it sits in that walk.
//!
//! zvcs read the two keys through gitoxide's boolean reader, treated anything it
//! could not decode as false, and ran `git_default_config()` only inside a
//! repository and before the command's own keys.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// One hunk whose only change is whitespace, so `--verbatim` hashes differently.
const PATCH: &str = "commit 1111111111111111111111111111111111111111\n\n\
diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a b\n+a  b\n";
const STRIPPED: &str =
    "ca76acfde49f69a183f6056aca9fdbe3f07b21b1 1111111111111111111111111111111111111111\n";
const VERBATIM: &str =
    "00835c3a8e642ed72261b5e456f25ad0f55c7d97 1111111111111111111111111111111111111111\n";

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    outside: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-patch-id-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        let outside = root.join("outside");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let f = Fixture { root, work, outside };
        f.run_in(&f.work, &["init", "-q", "-b", "main", "."]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &PathBuf, args: &[&str]) -> (String, String, i32) {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // A refusal exits before reading stdin; a closed pipe is not a failure.
        let _ = child.stdin.take().unwrap().write_all(PATCH.as_bytes());
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

fn fatal(msg: &str) -> (String, String, i32) {
    (String::new(), format!("fatal: {msg}\n"), 128)
}

#[test]
fn a_bad_boolean_is_fatal_before_options_inside_and_outside_a_repository() {
    let f = Fixture::new("bad");
    let want = fatal("bad boolean config value 'bogus' for 'patchid.stable'");
    for dir in [&f.work, &f.outside] {
        assert_eq!(f.run_in(dir, &["-c", "patchid.stable=bogus", "patch-id"]), want);
        assert_eq!(f.run_in(dir, &["-c", "patchid.stable=bogus", "patch-id", "-h"]), want);
        assert_eq!(
            f.run_in(dir, &["-c", "patchid.stable=bogus", "patch-id", "--stable", "--verbatim"]),
            want
        );
    }
}

#[test]
fn an_integer_with_a_unit_reads_as_true() {
    let f = Fixture::new("unit");
    for dir in [&f.work, &f.outside] {
        let (out, err, code) = f.run_in(dir, &["-c", "patchid.verbatim=1k", "patch-id"]);
        assert_eq!((out.as_str(), err.as_str(), code), (VERBATIM, "", 0));
    }
    let (out, _, code) = f.run(&["-c", "patchid.verbatim=0", "patch-id"]);
    assert_eq!((out.as_str(), code), (STRIPPED, 0));
}

#[test]
fn the_first_bad_occurrence_in_parse_order_wins() {
    let f = Fixture::new("order");
    // A later valid override does not rescue an earlier bad value.
    assert_eq!(
        f.run(&["-c", "patchid.verbatim=x", "-c", "patchid.verbatim=true", "patch-id"]),
        fatal("bad boolean config value 'x' for 'patchid.verbatim'")
    );
    // The command's own key and the default callback's are one walk.
    assert_eq!(
        f.run(&["-c", "patchid.verbatim=x", "-c", "core.abbrev=bogus", "patch-id"]),
        fatal("bad boolean config value 'x' for 'patchid.verbatim'")
    );
    let abbrev = fatal("bad numeric config value 'bogus' for 'core.abbrev': invalid unit");
    assert_eq!(
        f.run(&["-c", "core.abbrev=bogus", "-c", "patchid.verbatim=x", "patch-id"]),
        abbrev
    );
    // `git_default_config()` is the tail outside a repository as well.
    assert_eq!(f.run_in(&f.outside, &["-c", "core.abbrev=bogus", "patch-id"]), abbrev);
}
