//! `safe.bareRepository=explicit` and the commands that set up gently.
//!
//! `setup_git_directory_gently()` meets `GIT_DIR_DISALLOWED_BARE` with
//! `if (!nongit_ok) die(...); *nongit_ok = 1;` (setup.c:1994-2001): a command
//! that sets up gently — `config` among them — runs as though there were no
//! repository, so a bare repository found by walking is not read at all.
//! zvcs refused only the strict commands and let the gentle ones read the
//! bare repository's configuration.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// A bare `b.git` with `x.y = bare` and a work-tree repository `r` with
    /// `x.y = work`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-safe-bare-gentle-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        f.git(&["init", "-q", "--bare", "b.git"]);
        f.git(&["-C", "b.git", "config", "x.y", "bare"]);
        f.git(&["init", "-q", "-b", "main", "r"]);
        f.git(&["-C", "r", "config", "x.y", "work"]);
        f
    }

    fn git(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env("LC_ALL", "C")
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
fn a_walked_bare_repository_is_not_read() {
    let f = Fixture::new("walked");
    let explicit = ["-c", "safe.bareRepository=explicit"];
    let with = |rest: &[&str]| {
        let mut all = explicit.to_vec();
        all.extend_from_slice(rest);
        f.git(&all)
    };
    assert_eq!(with(&["-C", "b.git", "config", "x.y"]), (String::new(), String::new(), 1));
    assert_eq!(with(&["-C", "b.git/refs", "config", "x.y"]), (String::new(), String::new(), 1));
    assert_eq!(
        with(&["-C", "b.git", "config", "--list", "--show-scope"]),
        ("command\tsafe.barerepository=explicit\n".into(), String::new(), 0)
    );
    // A strict command still dies.
    let bare = f.root.join("b.git").display().to_string();
    assert_eq!(
        with(&["-C", "b.git", "rev-parse", "--git-dir"]),
        (String::new(), format!("fatal: cannot use bare repository '{bare}' (safe.bareRepository is 'explicit')\n"), 128)
    );
}

#[test]
fn a_named_or_implicit_one_is() {
    let f = Fixture::new("named");
    assert_eq!(f.git(&["-c", "safe.bareRepository=explicit", "--git-dir=b.git", "config", "x.y"]).0, "bare\n");
    assert_eq!(f.git(&["-c", "safe.bareRepository=explicit", "-C", "r/.git", "config", "x.y"]).0, "work\n");
    assert_eq!(
        f.git(&["-c", "safe.bareRepository=explicit", "-c", "safe.bareRepository=all", "-C", "b.git", "config", "x.y"]).0,
        "bare\n"
    );
}
