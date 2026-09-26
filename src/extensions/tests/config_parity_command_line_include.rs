//! A command-line `include.path` that `handle_path_include()` refuses.
//!
//! Every value the configuration sequence reads passes through
//! `git_config_include()` (config.c:416-448), and an `include.path` there goes
//! to `handle_path_include()` (config.c:142-191). A `-c` value has no file
//! behind it, so a valueless one is `config_error_nonbool("include.path")`, a
//! `~user` that cannot be expanded is `could not expand include path '%s'`, and
//! any relative path — the empty one included — is `relative config includes
//! must come from files`. The negative return makes `do_git_config_sequence()`
//! die with `unable to parse command-line config` (config.c:1600-1602).
//!
//! zvcs ignored a valueless or empty `include.path`, reported a relative one in
//! gitoxide's words (`zvcs: status: Failed to load the git configuration: …`,
//! exit 1), and let `hash-object` through entirely.
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
            .join(format!("zvcs-config-cmdline-include-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT")
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

fn refused(reason: &str) -> (String, String, i32) {
    (
        String::new(),
        format!("error: {reason}\nfatal: unable to parse command-line config\n"),
        128,
    )
}

#[test]
fn a_relative_include_dies_for_every_verb_that_reads_config() {
    let f = Fixture::new("relative");
    let relative = refused("relative config includes must come from files");
    for verb in [&["status"][..], &["hash-object", "--stdin"], &["config", "--list"]] {
        let mut args = vec!["-c", "include.path=rel"];
        args.extend_from_slice(verb);
        assert_eq!(f.run(&args), relative, "{verb:?}");
    }
    // The key is matched case-insensitively, and the empty value is relative.
    assert_eq!(f.run(&["-c", "Include.Path=rel", "status"]), relative);
    assert_eq!(f.run(&["-c", "include.path=", "status"]), relative);
}

#[test]
fn a_valueless_or_unexpandable_include_is_refused() {
    let f = Fixture::new("nonbool");
    assert_eq!(
        f.run(&["-c", "include.path", "status"]),
        refused("missing value for 'include.path'")
    );
    assert_eq!(
        f.run(&["-c", "include.path=~zvcs-no-such-user/x", "status"]),
        refused("could not expand include path '~zvcs-no-such-user/x'")
    );
}

/// The entries are read in command-line order, so a bad key ahead of the
/// include is the one named, and the include is named ahead of a bad key after
/// it — and ahead of a value the command's own callback would refuse.
#[test]
fn the_first_failing_entry_is_the_one_named() {
    let f = Fixture::new("order");
    assert_eq!(
        f.run(&["-c", "bad key", "-c", "include.path=rel", "status"]),
        refused("key does not contain a section: bad key")
    );
    assert_eq!(
        f.run(&["-c", "include.path=rel", "-c", "bad key", "status"]),
        refused("relative config includes must come from files")
    );
    assert_eq!(
        f.run(&["-c", "core.abbrev=bogus", "-c", "include.path=rel", "status"]),
        refused("relative config includes must come from files")
    );
}

/// An absolute path, or `~/…`, that does not exist is skipped by
/// `access_or_die()`; and `version` never reads the configuration at all.
#[test]
fn a_missing_absolute_include_and_version_are_not_refused() {
    let f = Fixture::new("quiet");
    let (_, err, code) = f.run(&["-c", "include.path=/zvcs/no/such/file", "status", "-s"]);
    assert_eq!((err.as_str(), code), ("", 0));
    let (_, err, code) = f.run(&["-c", "include.path=~/no-such-file", "status", "-s"]);
    assert_eq!((err.as_str(), code), ("", 0));
    let (_, err, code) = f.run(&["-c", "include.path=rel", "version"]);
    assert_eq!((err.as_str(), code), ("", 0));
}
