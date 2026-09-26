//! `init.defaultBranch` judged by git's rule rather than gitoxide's.
//!
//! `repo_default_branch_name()` (refs.c:691-720) takes the configured string
//! as it is — the empty string included — and only asks
//! `check_refname_format("refs/heads/<name>", 0)` about it, dying with
//!
//! ```c
//! die(_("invalid branch name: %s = %s"), config_display_key, ret);
//! ```
//!
//! It runs from `create_reference_database()`, after the skeleton is laid
//! down, and not at all when `-b` names the branch. zvcs created the repository
//! through `gix::ThreadSafeRepository::init`, whose own branch-name validator
//! refused `a..b` as `zvcs: init: Invalid default branch name: "a..b"` at exit 1
//! — even under `-b ok`, and even for `HEAD`, which git accepts — while an empty
//! value was treated as unset and silently fell back to `master`.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-init-default-branch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn head(&self, repo: &str) -> String {
        std::fs::read_to_string(self.root.join(repo).join(".git/HEAD")).unwrap()
    }
}

#[test]
fn an_invalid_configured_name_dies_under_the_key_name() {
    let f = Fixture::new("invalid");
    for (value, repo) in [("a..b", "dots"), ("", "empty"), (" ", "blank")] {
        let (out, err, code) =
            f.run(&["-c", &format!("init.defaultBranch={value}"), "init", "-q", repo]);
        let want = format!("fatal: invalid branch name: init.defaultBranch = {value}\n");
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{value:?}");
        // The skeleton exists — git dies after laying it down — but HEAD and the
        // object store do not.
        let git_dir = f.root.join(repo).join(".git");
        assert!(git_dir.join("config").is_file(), "{value:?}");
        assert!(!git_dir.join("HEAD").exists(), "{value:?}");
        assert!(!git_dir.join("objects").exists(), "{value:?}");
    }
}

#[test]
fn bare_init_reports_the_same_refusal() {
    let f = Fixture::new("bare");
    let (out, err, code) = f.run(&["-c", "init.defaultBranch=a..b", "init", "-q", "--bare", "b.git"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: invalid branch name: init.defaultBranch = a..b\n", 128)
    );
}

#[test]
fn an_explicit_initial_branch_never_consults_the_key() {
    let f = Fixture::new("explicit");
    let (out, err, code) =
        f.run(&["-c", "init.defaultBranch=a..b", "init", "-q", "-b", "ok", "r"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(f.head("r"), "ref: refs/heads/ok\n");
    // A bad `-b` keeps its own wording.
    let (_, err, code) = f.run(&["-c", "init.defaultBranch=a..b", "init", "-q", "-b", "x..y", "s"]);
    assert_eq!((err.as_str(), code), ("fatal: invalid initial branch name: 'x..y'\n", 128));
}

/// `refs/heads/HEAD` passes `check_refname_format()`, so git accepts it.
#[test]
fn head_is_a_valid_default_branch() {
    let f = Fixture::new("head");
    let (out, err, code) = f.run(&["-c", "init.defaultBranch=HEAD", "init", "-q", "r"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(f.head("r"), "ref: refs/heads/HEAD\n");
    let (_, err, code) = f.run(&["-c", "init.defaultBranch=trunk", "init", "-q", "t"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(f.head("t"), "ref: refs/heads/trunk\n");
}
