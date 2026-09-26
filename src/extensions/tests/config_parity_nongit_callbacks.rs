//! The config callback of a gently-set-up builtin, run outside any repository.
//!
//! `repo_config(the_repository, fn, data)` (config.c:2334-2342) walks the system
//! and global files and the command line whether or not setup found a git
//! directory (`do_git_config_sequence()`, config.c:1570-1602). So the
//! `RUN_SETUP_GENTLY` and setup-free entries of git.c's `commands[]` table that
//! call it unconditionally — `hash-object` (builtin/hash-object.c:115), `var`
//! (builtin/var.c:234), `diff` (builtin/diff.c:489), `grep`
//! (builtin/grep.c:1182), `interpret-trailers` (builtin/interpret-trailers.c:169),
//! `credential` (builtin/credential.c:20), `shortlog` (builtin/shortlog.c:424),
//! `verify-pack` (builtin/verify-pack.c:85), … — die on a refused value outside a
//! repository exactly as inside one. zvcs only ran its config gate once a
//! repository had been discovered, so outside one it hashed, diffed and grepped
//! through values git refuses.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
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
    /// A plain directory holding `a` and `b`, with discovery fenced off above it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-config-nongit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("a"), "a\n").unwrap();
        std::fs::write(work.join("b"), "b\n").unwrap();
        Fixture { root, work }
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_with_global(args, Path::new("/dev/null"))
    }

    fn run_with_global(&self, args: &[&str], global: &Path) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", global)
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const ABBREV: &str = "fatal: bad numeric config value 'bogus' for 'core.abbrev': invalid unit\n";

#[test]
fn default_callback_verbs_refuse_a_command_line_value() {
    let f = Fixture::new("default");
    for argv in [
        &["hash-object", "a"][..],
        &["hash-object", "--stdin"],
        &["var", "GIT_EDITOR"],
        &["credential", "fill"],
        &["interpret-trailers", "a"],
        &["shortlog"],
        &["verify-pack", "x.idx"],
        &["hook", "run", "--ignore-missing", "pre-commit"],
    ] {
        let mut args = vec!["-c", "core.abbrev=bogus"];
        args.extend_from_slice(argv);
        assert_eq!(f.run(&args), (String::new(), ABBREV.to_owned(), 128), "{argv:?}");
    }
}

#[test]
fn a_valid_configuration_still_runs() {
    let f = Fixture::new("valid");
    let (out, err, code) = f.run(&["-c", "core.abbrev=12", "hash-object", "a"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("78981922613b2afb6025042ff6bd878ac1994e85\n", "", 0)
    );
}

#[test]
fn error_shaped_refusals_name_the_command_line() {
    let f = Fixture::new("error");
    let (out, err, code) = f.run(&["-c", "push.default=bogus", "interpret-trailers", "a"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "error: malformed value for push.default: bogus\n\
             error: must be one of nothing, matching, simple, upstream or current\n\
             fatal: unable to parse 'push.default' from command-line config\n",
            128
        )
    );
    let (out, err, code) = f.run(&["-c", "user.name", "credential", "fill"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "error: missing value for 'user.name'\n\
             fatal: unable to parse 'user.name' from command-line config\n",
            128
        )
    );
}

/// `diff` installs `git_diff_ui_config` and `grep` installs `grep_cmd_config`,
/// so their own keys are refused too — not just the `git_default_config` tail.
#[test]
fn diff_and_grep_run_their_own_callbacks() {
    let f = Fixture::new("callbacks");
    assert_eq!(
        f.run(&["-c", "diff.context=bogus", "diff", "--no-index", "a", "b"]),
        (
            String::new(),
            "fatal: bad numeric config value 'bogus' for 'diff.context': invalid unit\n".to_owned(),
            128
        )
    );
    assert_eq!(
        f.run(&["-c", "grep.patternType=bogus", "grep", "--no-index", "x", "a"]),
        (String::new(), "fatal: bad grep.patterntype argument: bogus\n".to_owned(), 128)
    );
}

/// With no repository the global file is still read, and a numeric refusal
/// from a file names it.
#[test]
fn a_global_file_value_is_refused_with_its_path() {
    let f = Fixture::new("global");
    let global = f.root.join("gitconfig");
    std::fs::write(&global, "[core]\n\tabbrev = bogus\n").unwrap();
    let (out, err, code) = f.run_with_global(&["hash-object", "a"], &global);
    let want = format!(
        "fatal: bad numeric config value 'bogus' for 'core.abbrev' in file {}: invalid unit\n",
        global.display()
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128));
}

/// The modes whose C never reaches the read: `hash-object -w` dies in
/// `setup_git_directory()` first (builtin/hash-object.c:102-103), and `var`
/// with the wrong argument count is `usage()` (builtin/var.c:226-227).
#[test]
fn modes_that_do_not_read_the_configuration_are_not_refused() {
    let f = Fixture::new("modes");
    let (_, err, code) = f.run(&["-c", "core.abbrev=bogus", "hash-object", "-w", "a"]);
    assert!(!err.contains("core.abbrev"), "{err}");
    assert_eq!(code, 128);
    let (_, err, code) = f.run(&["-c", "core.abbrev=bogus", "var"]);
    assert_eq!((err.as_str(), code), ("usage: git var (-l | <variable>)\n", 129));
}
