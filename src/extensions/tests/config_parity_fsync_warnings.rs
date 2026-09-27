//! The fsync diagnostics come from parsing the configuration, once per walk.
//!
//! `git_default_core_config()` (environment.c:475-501) handles `core.fsync`
//! (`config_error_nonbool`, then `parse_fsync_components()` and its warnings),
//! `core.fsyncMethod` (`ignoring unknown core.fsyncMethod value`) and
//! `core.fsyncObjectFiles` (`is deprecated` while `fsync_object_files < 0`, i.e.
//! once per process, then `git_config_bool()`). So every command that walks its
//! configuration through `git_default_config()` says them — `status`, `log`,
//! `branch` — and says the first two again for every further walk: `commit`
//! and `merge` reach `setup_rerere()`, whose `git_rerere_config()` is one more
//! `repo_config(the_repository, git_default_config, NULL)` (rerere.c:875-880).
//! zvcs said them only where it wrote an index or pack, and only once.
//!
//! `maintenance.auto=false` keeps `commit`'s auto-maintenance child, which walks
//! the configuration for itself, out of the count.
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
        let root = std::env::temp_dir().join(format!("zvcs-config-fsync-warnings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "i"]);
        f.run(&["config", "maintenance.auto", "false"]);
        f
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
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// stderr and exit code of `git -c <config> <args>`.
    fn with(&self, config: &str, args: &[&str]) -> (String, i32) {
        let mut all = vec!["-c", config];
        all.extend_from_slice(args);
        let (_, err, code) = self.run(&all);
        (err, code)
    }
}

const COMPONENT: &str = "warning: ignoring unknown core.fsync component 'bogus'\n";
const METHOD: &str = "warning: ignoring unknown core.fsyncMethod value 'bogus'\n";
const DEPRECATED: &str = "warning: core.fsyncObjectFiles is deprecated; use core.fsync instead\n";

#[test]
fn every_command_that_reads_its_configuration_warns() {
    let f = Fixture::new("once");
    for args in [&["status", "-s"][..], &["add", "a"], &["log", "--oneline", "-1"], &["branch"]] {
        assert_eq!(f.with("core.fsync=bogus", args), (COMPONENT.into(), 0), "{args:?}");
        assert_eq!(f.with("core.fsyncMethod=bogus", args), (METHOD.into(), 0), "{args:?}");
        assert_eq!(f.with("core.fsyncObjectFiles=true", args), (DEPRECATED.into(), 0), "{args:?}");
    }
    // In configuration order, and a bad boolean dies right after its warning.
    let (_, err, code) = f.run(&[
        "-c", "core.fsync=bogus", "-c", "core.fsyncObjectFiles=yes", "-c", "core.fsyncMethod=bogus", "branch",
    ]);
    assert_eq!((err, code), (format!("{COMPONENT}{DEPRECATED}{METHOD}"), 0));
    assert_eq!(
        f.with("core.fsyncObjectFiles=bogus", &["log", "-1"]),
        (format!("{DEPRECATED}fatal: bad boolean config value 'bogus' for 'core.fsyncobjectfiles'\n"), 128)
    );
    assert_eq!(
        f.with("core.fsync", &["status", "-s"]),
        ("error: missing value for 'core.fsync'\nfatal: unable to parse 'core.fsync' from command-line config\n".into(), 128)
    );
}

#[test]
fn setup_rerere_walks_the_configuration_again() {
    let f = Fixture::new("rerere");
    let commit = ["commit", "-q", "--allow-empty", "-m", "j"];
    assert_eq!(f.with("core.fsync=bogus", &commit), (COMPONENT.repeat(2), 0));
    assert_eq!(f.with("core.fsyncMethod=bogus", &commit), (METHOD.repeat(2), 0));
    // The deprecation is once per process, however many walks.
    assert_eq!(f.with("core.fsyncObjectFiles=true", &commit), (DEPRECATED.into(), 0));
    assert_eq!(f.with("core.fsync=bogus", &["merge", "-q", "HEAD"]), (COMPONENT.repeat(2), 0));
    assert_eq!(f.with("core.fsync=bogus", &["rerere", "status"]), (COMPONENT.repeat(2), 0));
}
