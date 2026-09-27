//! A repository whose ownership setup refuses is ignored by gentle commands.
//!
//! `setup_git_directory_gently()` meets `GIT_DIR_INVALID_OWNERSHIP` with
//!
//! ```c
//! if (!nongit_ok) { … die(_("detected dubious ownership in repository at '%s'\n" …)); }
//! *nongit_ok = 1;
//! ```
//!
//! (setup.c:1979-1993): a command that sets up gently — `config`, `diff`,
//! `shortlog`, `hash-object` without `-w` — runs silently as though there were no
//! repository, so the repository's own configuration is never read. zvcs only
//! gated the strict commands and let the gentle ones use the repository, local
//! configuration included.
//!
//! `GIT_TEST_ASSUME_DIFFERENT_OWNER=1` makes setup treat the repository as
//! foreign-owned, as it does in stock.
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
    /// A committed repository with `x.y = local` in its own configuration.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-dubious-gentle-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("r/sub")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("r");
        let f = Fixture { root, work };
        f.git(false, &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("sub/f"), "a\n").unwrap();
        f.git(false, &["add", "."]);
        f.git(false, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "i"]);
        f.git(false, &["config", "x.y", "local"]);
        f
    }

    fn git(&self, foreign: bool, args: &[&str]) -> (String, String, i32) {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null());
        if foreign {
            cmd.env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1");
        }
        let out = cmd.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn config_reads_no_repository_scope() {
    let f = Fixture::new("config");
    assert_eq!(f.git(true, &["config", "--list", "--show-scope"]), (String::new(), String::new(), 0));
    assert_eq!(f.git(true, &["config", "x.y"]), (String::new(), String::new(), 1));
    assert_eq!(f.git(true, &["-C", "sub", "config", "x.y"]), (String::new(), String::new(), 1));
    assert_eq!(
        f.git(true, &["config", "--local", "x.y"]),
        (String::new(), "fatal: --local can only be used inside a git repository\n".into(), 128)
    );
    // `safe.directory` lets it back in.
    let safe = format!("safe.directory={}", f.work.display());
    assert_eq!(f.git(true, &["-c", &safe, "config", "x.y"]), ("local\n".into(), String::new(), 0));
}

#[test]
fn diff_runs_as_outside_a_repository() {
    let f = Fixture::new("diff");
    let (out, err, code) = f.git(true, &["diff"]);
    assert_eq!((out.as_str(), code), ("", 129));
    assert!(
        err.starts_with("warning: Not a git repository. Use --no-index to compare two paths outside a working tree\nusage: git diff --no-index"),
        "{err}"
    );
    // A strict command still dies with the message.
    let (_, err, code) = f.git(true, &["status"]);
    assert_eq!(code, 128);
    assert!(err.starts_with("fatal: detected dubious ownership in repository at "), "{err}");
}
