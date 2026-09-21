//! `git --bare init` and the two refusals that depend on the repository being
//! bare.
//!
//! `is_bare_repository_cfg` is process-global: `git --bare` sets it to 1 before
//! the subcommand runs (git.c:256-258, v2.55.0) and init's own flag is
//! `OPT_SET_INT(0, "bare", &is_bare_repository_cfg, …, 1)`
//! (builtin/init-db.c:93) writing the same variable. So `git --bare init <dir>`
//! is `git init --bare <dir>`, and in particular it re-exports `GIT_DIR` as the
//! operand directory once init has chdir'd into it
//! (`setenv(GIT_DIR_ENVIRONMENT, cwd, argc > 0)`, builtin/init-db.c:163-166).
//! The port read only init's own flag, so the `GIT_DIR` that `git --bare` had
//! already exported — the *original* working directory — survived and the
//! repository was created there instead of in the operand.
//!
//! The second refusal is the one that fires when the layout, not the command
//! line, is what makes the repository bare: `if (real_git_dir) die(_("--separate-git-dir
//! incompatible with bare repository"))` (builtin/init-db.c:247-250), reached
//! after `guess_repository_type()`. Without it the port went on to move a git
//! directory into a subdirectory of itself.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository, stdout, stderr and exit status compared separately.
#![cfg(unix)]

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
    /// An empty directory to run `init` in — no repository yet.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-init-bare-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        Fixture { root, work }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("LC_ALL", "C")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// The operand wins over the `GIT_DIR` that `git --bare` exported: the bare
/// repository lands in `<dir>`, and the directory `git --bare` was run from
/// gains nothing but that one entry.
#[test]
fn a_global_bare_flag_still_inits_into_the_operand_directory() {
    let f = Fixture::new("operand");
    let (_, err, code) = f.run(&["--bare", "init", "pb"]);
    assert_eq!(code, 0, "{err:?}");

    assert!(f.work.join("pb/config").is_file(), "the repository is in pb");
    assert!(f.work.join("pb/HEAD").is_file());
    assert!(f.work.join("pb/refs").is_dir());
    assert!(!f.work.join("config").exists(), "and not in the cwd");
    assert!(!f.work.join(".git").exists());

    let bare = String::from_utf8_lossy(
        &f.cmd(&["-C", "pb", "config", "core.bare"]).output().unwrap().stdout,
    )
    .into_owned();
    assert_eq!(bare, "true\n");
}

/// With no operand there is nothing to overwrite `GIT_DIR` with, so the bare
/// repository is the directory `git --bare` was run from — the overwrite flag
/// of that `setenv()` is `argc > 0`.
#[test]
fn a_global_bare_flag_without_an_operand_inits_into_the_cwd() {
    let f = Fixture::new("cwd");
    let (_, err, code) = f.run(&["--bare", "init"]);
    assert_eq!(code, 0, "{err:?}");
    assert!(f.work.join("config").is_file());
    assert!(f.work.join("HEAD").is_file());
    assert!(!f.work.join(".git").exists());
}

/// A git directory that is bare only because `guess_repository_type()` said so
/// still refuses `--separate-git-dir`.
#[test]
fn an_implicitly_bare_repository_refuses_separate_git_dir() {
    let f = Fixture::new("implicit");
    std::fs::create_dir(f.work.join("bare.git")).unwrap();
    let out = f
        .cmd(&["-C", "bare.git", "init", "--separate-git-dir", "goop.git"])
        .env("GIT_DIR", ".")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: --separate-git-dir incompatible with bare repository\n"
    );
    assert!(
        !f.work.join("bare.git/goop.git").exists(),
        "nothing was moved"
    );
}

/// The explicit spelling keeps its own earlier refusal, which is a different
/// message: `builtin/init-db.c:118-119` runs before the layout is known.
#[test]
fn an_explicitly_bare_init_refuses_separate_git_dir_with_the_option_message() {
    let f = Fixture::new("explicit");
    let (out, err, code) = f.run(&["--bare", "init", "--separate-git-dir", "goop.git", "r"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err,
        "fatal: options '--separate-git-dir' and '--bare' cannot be used together\n"
    );
}
