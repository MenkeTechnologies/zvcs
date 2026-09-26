//! The `--no-` spellings of clone's string, list and integer options.
//!
//! parse-options gives every option without `PARSE_OPT_NONEG` a `--no-` form:
//! `OPT_STRING` unset stores NULL, `OPT_STRING_LIST` unset is
//! `string_list_clear()` (parse-options-cb.c:199-206), `OPT_INTEGER` unset
//! stores 0 (parse-options.c:260-267), and `--naked` is a hidden `OPT_BOOL`
//! alias of `--bare` (builtin/clone.c:931). zvcs had no arm for `--naked`,
//! `--no-naked`, `--no-jobs`, `--no-depth`, `--no-shallow-since`,
//! `--no-shallow-exclude`, `--no-config`, `--no-reference` or
//! `--no-reference-if-able`, so each one fell through to the operand list and
//! the clone died with `fatal: Too many arguments.` and the usage block, exit 129.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
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
    /// `src` with two commits, so a `--depth 1` clone is visibly shallow.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-clone-negated-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let f = Fixture { root };
        let src = f.root.join("src");
        f.run(&src, &["init", "-q", "-b", "main", "."]);
        for name in ["a", "b"] {
            std::fs::write(src.join(name), format!("{name}\n")).unwrap();
            f.run(&src, &["add", name]);
            f.run(&src, &["commit", "-q", "-m", name]);
        }
        f
    }

    /// `git clone -q <opts> src dst`, which must succeed silently.
    fn clone(&self, dst: &str, opts: &[&str]) -> PathBuf {
        let mut args = vec!["clone", "-q"];
        args.extend_from_slice(opts);
        args.extend_from_slice(&["src", dst]);
        assert_eq!(self.run(&self.root, &args), (String::new(), String::new(), 0), "{opts:?}");
        self.root.join(dst)
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(cwd)
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

#[test]
fn naked_is_bare_and_no_naked_takes_it_back() {
    let f = Fixture::new("naked");
    let bare = f.clone("d1", &["--naked"]);
    assert!(bare.join("HEAD").is_file() && !bare.join(".git").exists());
    let work = f.clone("d2", &["--naked", "--no-naked"]);
    assert!(work.join(".git/HEAD").is_file() && work.join("b").is_file());
}

#[test]
fn no_reference_forgets_the_references_before_it() {
    let f = Fixture::new("reference");
    for opts in [["--reference", "src", "--no-reference"], ["--reference-if-able", "src", "--no-reference-if-able"]] {
        let dst = f.clone(opts[0].trim_start_matches('-'), &opts);
        assert!(!dst.join(".git/objects/info/alternates").exists(), "{opts:?}");
    }
    // Only the list it names: `--reference` survives `--no-reference-if-able`.
    let dst = f.clone("kept", &["--reference", "src", "--no-reference-if-able"]);
    assert!(dst.join(".git/objects/info/alternates").is_file());
}

#[test]
fn no_depth_since_and_exclude_leave_a_complete_clone() {
    let f = Fixture::new("shallow");
    for (dst, opts) in [
        ("depth", ["--depth", "1", "--no-depth"]),
        ("since", ["--shallow-since", "2000-01-01", "--no-shallow-since"]),
        ("exclude", ["--shallow-exclude", "main", "--no-shallow-exclude"]),
    ] {
        let mut all = vec!["--no-local"];
        all.extend_from_slice(&opts);
        let dir = f.clone(dst, &all);
        assert!(!dir.join(".git/shallow").exists(), "{opts:?}");
        assert_eq!(f.run(&dir, &["rev-list", "--count", "HEAD"]).0, "2\n", "{opts:?}");
    }
}

#[test]
fn no_config_drops_only_the_pairs_before_it() {
    let f = Fixture::new("config");
    let dir = f.clone("d", &["-c", "foo.bar=1", "-c", "foo.baz=1", "--no-config", "-c", "foo.baz=2"]);
    assert_eq!(f.run(&dir, &["config", "foo.bar"]), (String::new(), String::new(), 1));
    assert_eq!(f.run(&dir, &["config", "foo.baz"]).0, "2\n");
}

#[test]
fn no_jobs_is_accepted() {
    let f = Fixture::new("jobs");
    let dir = f.clone("d", &["--no-jobs"]);
    assert!(dir.join("b").is_file());
}
