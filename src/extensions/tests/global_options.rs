//! git's top-level `handle_options()` (git.c), which runs before any subcommand
//! is dispatched — so a divergence here is a divergence in *every* command.
//!
//! Two failure shapes are asserted, both verified against stock git 2.55.0:
//!
//!   * `-C <path>` that cannot be entered is `die_errno("cannot change to
//!     '%s'", …)` — `fatal: cannot change to '<path>': <strerror>`, exit 128.
//!     This port used to print `zvcs: -C: cannot chdir to <path>` and exit 1,
//!     which is both the wrong text and the wrong code for a scripted caller.
//!     An *empty* `-C` argument is a deliberate no-op in the C (the chdir is
//!     guarded by `if ((*argv)[1][0])`), and used to be reported as a failure.
//!   * a global option given with no value prints its own complaint, then
//!     `usage(git_usage_string)`, and exits 129. These used to fall out of the
//!     option loop and be mistaken for the subcommand name.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The first line of git's `usage(git_usage_string)` block, so the assertions
/// pin that the usage really followed the complaint without copying all six
/// lines of it into every case.
const USAGE_HEAD: &str = "usage: git [-v | --version] [-h | --help] [-C <path>] [-c <name>=<value>]";

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_NAMESPACE")
        .output()
        .unwrap()
}

/// A repository with one commit and one subdirectory, so `-C` has both a target
/// that exists and a file that is not a directory to aim at.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-globals-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap().join("repo");
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    std::fs::write(repo.join("f"), "a\n").unwrap();
    assert!(run(&repo, &["init", "-q", "-b", "main"]).status.success());
    repo
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn dash_c_into_a_missing_directory_is_git_die_errno() {
    let repo = fixture("missing");

    let out = run(&repo, &["-C", "nope", "status", "--porcelain"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: cannot change to 'nope': No such file or directory\n");
    assert!(out.stdout.is_empty(), "nothing runs after the chdir fails");

    // The errno text is the OS's, not a fixed string: a non-directory is ENOTDIR.
    let out = run(&repo, &["-C", "f", "status", "--porcelain"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: cannot change to 'f': Not a directory\n");

    // A later `-C` fails the same way once an earlier one has already moved.
    let out = run(&repo, &["-C", "sub", "-C", "nope", "status", "--porcelain"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: cannot change to 'nope': No such file or directory\n");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn dash_c_with_an_empty_path_is_a_no_op() {
    let repo = fixture("empty");

    // git guards the chdir with `if ((*argv)[1][0])`, so this succeeds and stays
    // put — `--show-prefix` proves the cwd never moved.
    let out = run(&repo, &["-C", "", "rev-parse", "--show-prefix"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\n", "the top level prefix is one empty line");

    // And an empty one does not swallow the failure of a real one after it.
    let out = run(&repo, &["-C", "", "-C", "nope", "status", "--porcelain"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: cannot change to 'nope': No such file or directory\n");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn dash_c_that_succeeds_moves_the_command() {
    let repo = fixture("ok");

    let out = run(&repo, &["-C", "sub", "rev-parse", "--show-prefix"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "sub/\n");

    // Repeated `-C` is cumulative and relative, as consecutive chdir(2) calls are.
    let out = run(&repo, &["-C", "sub", "-C", "..", "rev-parse", "--show-prefix"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\n", "the top level prefix is one empty line");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_global_option_without_its_value_is_usage_129() {
    let repo = fixture("novalue");

    // Each entry is git's own `fprintf(stderr, …)` for that option — no `fatal: `
    // and no `error: ` prefix — followed by the usage block.
    for (arg, complaint) in [
        ("-C", "no directory given for '-C' option"),
        ("-c", "-c expects a configuration string"),
        ("--git-dir", "no directory given for '--git-dir' option"),
        ("--work-tree", "no directory given for '--work-tree' option"),
        ("--namespace", "no namespace given for --namespace"),
    ] {
        let out = run(&repo, &[arg]);
        assert_eq!(code(&out), 129, "{arg}: stderr: {}", stderr(&out));
        let err = stderr(&out);
        let mut lines = err.lines();
        assert_eq!(lines.next(), Some(complaint), "{arg} complaint");
        assert_eq!(lines.next(), Some(USAGE_HEAD), "{arg} usage block");
        assert!(out.stdout.is_empty(), "{arg} prints nothing on stdout");
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn dash_c_inside_an_alias_expansion_fails_the_same_way() {
    let repo = fixture("alias");
    // `handle_options()` runs again on every turn of `run_argv`'s alias loop, so
    // a `-C` an expansion introduces must die exactly as a command-line one does.
    assert!(run(&repo, &["config", "alias.gone", "-C nope status --porcelain"]).status.success());
    assert!(run(&repo, &["config", "alias.stay", "-C \"\" rev-parse --show-prefix"]).status.success());

    let out = run(&repo, &["gone"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: cannot change to 'nope': No such file or directory\n");

    let out = run(&repo, &["stay"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\n", "the top level prefix is one empty line");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
