//! `git p4`'s top-level dispatch — the part of the command that runs before a
//! Perforce server, a `p4` client or a git repository is involved.
//!
//! `git-p4.py`'s `main()` looks `sys.argv[1]` up in the `commands` dict *before
//! any option parsing happens*, so an option in that position is a command name
//! that is not in the table: `git p4 -h` is `unknown command -h`, not help, and
//! `--no-such-flag` is the same. Both take the `except KeyError` arm — the
//! message, a blank line, `printUsage()`, exit 2 — and no arguments at all take
//! `printUsage()` and exit 2 directly. Everything lands on stdout; stderr stays
//! empty.
//!
//! The banner interpolates `sys.argv[0]`, the `git-p4` git resolved out of its
//! exec-path, which is per-installation and is therefore what these tests pin
//! down: the resolution order (exec-path, then `PATH`), the executable-bit test
//! that skips a candidate, and the spelling used when nothing is found. Every
//! expectation is the text stock git 2.55.0 printed for the same argv with its
//! own path substituted, and every run gets a `PATH` and a `HOME` built inside
//! the fixture — nothing here reads the machine's `PATH`, so a `git-p4` sitting
//! in a real install cannot change an answer.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `printUsage(commands.keys())`, verbatim, for a given `sys.argv[0]`.
fn usage_block(prog: &Path) -> String {
    let prog = prog.display();
    format!(
        "usage: {prog} <command> [options]\n\n\
         valid commands: submit, commit, sync, rebase, clone, branches, unshelve\n\n\
         Try {prog} <command> --help for command specific help.\n\n"
    )
}

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
        let root = std::env::temp_dir().join(format!("zvcs-p4argv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    /// An empty directory under the fixture root.
    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A `git-p4` inside `dir` with the given mode, and its path. The contents
    /// never run: only `access(X_OK)` looks at this file.
    fn helper(dir: &Path, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("git-p4");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    /// Run `git p4 <args>` with `GIT_EXEC_PATH` and `PATH` set exactly as given.
    /// Returns `(exit code, stdout, stderr)`.
    fn run(
        &self,
        exec_path: Option<&Path>,
        path_dirs: &[&Path],
        args: &[&str],
    ) -> (i32, String, String) {
        let path = std::env::join_paths(path_dirs).unwrap();
        let mut cmd = Command::new(BIN);
        cmd.arg("p4")
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("PATH", path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        match exec_path {
            Some(dir) => cmd.env("GIT_EXEC_PATH", dir),
            None => cmd.env_remove("GIT_EXEC_PATH"),
        };
        let out = cmd.output().unwrap();
        (
            out.status.code().expect("the command exited rather than being signalled"),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

// ---------------------------------------------------------------------------
// main()'s dispatch
// ---------------------------------------------------------------------------

/// `if len(sys.argv[1:]) == 0: printUsage(...); sys.exit(2)`.
#[test]
fn no_arguments_print_the_usage_block_and_exit_two() {
    let f = Fixture::new("noargs");
    let exec = f.dir("exec");
    let prog = Fixture::helper(&exec, 0o755);
    assert_eq!(
        f.run(Some(&exec), &[&exec], &[]),
        (2, usage_block(&prog), String::new())
    );
}

/// The case that makes `git p4` unlike every other git command: `-h` is read as
/// a command name, misses the `commands` dict, and is reported as one. optparse
/// never sees it, so no help is printed and the exit is 2, not 0.
#[test]
fn dash_h_is_an_unknown_command_not_a_help_request() {
    let f = Fixture::new("dashh");
    let exec = f.dir("exec");
    let prog = Fixture::helper(&exec, 0o755);
    let expected = format!("unknown command -h\n\n{}", usage_block(&prog));
    assert_eq!(f.run(Some(&exec), &[&exec], &["-h"]), (2, expected, String::new()));
}

/// Same arm for a long option, which the dict lookup treats no differently.
#[test]
fn an_unknown_long_option_is_an_unknown_command() {
    let f = Fixture::new("badflag");
    let exec = f.dir("exec");
    let prog = Fixture::helper(&exec, 0o755);
    let expected = format!("unknown command --no-such-flag\n\n{}", usage_block(&prog));
    assert_eq!(
        f.run(Some(&exec), &[&exec], &["--no-such-flag"]),
        (2, expected, String::new())
    );
}

/// The ordinary spelling of the same failure: the name is echoed as given.
#[test]
fn an_unknown_subcommand_is_echoed_in_the_message() {
    let f = Fixture::new("badsub");
    let exec = f.dir("exec");
    let prog = Fixture::helper(&exec, 0o755);
    let expected = format!("unknown command no-such-sub\n\n{}", usage_block(&prog));
    assert_eq!(
        f.run(Some(&exec), &[&exec], &["no-such-sub"]),
        (2, expected, String::new())
    );
}

// ---------------------------------------------------------------------------
// sys.argv[0] — how git resolves the helper it execs
// ---------------------------------------------------------------------------

/// `setup_path()` prepends the exec-path to `PATH`, so an exec-path helper wins
/// over one that `PATH` also offers.
#[test]
fn the_exec_path_helper_wins_over_one_on_path() {
    let f = Fixture::new("execwins");
    let exec = f.dir("exec");
    let elsewhere = f.dir("elsewhere");
    let prog = Fixture::helper(&exec, 0o755);
    Fixture::helper(&elsewhere, 0o755);
    let (code, stdout, _) = f.run(Some(&exec), &[&elsewhere, &exec], &[]);
    assert_eq!((code, stdout), (2, usage_block(&prog)));
}

/// With no helper in the exec-path, `locate_in_PATH()` keeps looking and the
/// banner names the `PATH` entry that has one.
#[test]
fn a_path_entry_is_used_when_the_exec_path_has_no_helper() {
    let f = Fixture::new("pathfallback");
    let exec = f.dir("exec");
    let elsewhere = f.dir("elsewhere");
    let prog = Fixture::helper(&elsewhere, 0o755);
    let (code, stdout, _) = f.run(Some(&exec), &[&exec, &elsewhere], &[]);
    assert_eq!((code, stdout), (2, usage_block(&prog)));
}

/// `locate_in_PATH()` accepts a candidate on git's `is_executable()`, so a file
/// of the right name that cannot be executed is passed over rather than named.
#[test]
fn a_non_executable_candidate_is_skipped() {
    let f = Fixture::new("noexec");
    let exec = f.dir("exec");
    let unusable = f.dir("unusable");
    let usable = f.dir("usable");
    Fixture::helper(&unusable, 0o644);
    let prog = Fixture::helper(&usable, 0o755);
    let (code, stdout, _) = f.run(Some(&exec), &[&exec, &unusable, &usable], &[]);
    assert_eq!((code, stdout), (2, usage_block(&prog)));
}

/// `is_executable()` reads the *owner* execute bit, so group and other bits do
/// not qualify a candidate — the distinction `access(X_OK)` would blur for a
/// file owned by someone else. Stock git 2.55.0 skips a mode `0o011` candidate
/// and names the next one.
#[test]
fn only_the_owner_execute_bit_qualifies_a_candidate() {
    let f = Fixture::new("ownerbit");
    let exec = f.dir("exec");
    let groupish = f.dir("groupish");
    let usable = f.dir("usable");
    Fixture::helper(&groupish, 0o011);
    let prog = Fixture::helper(&usable, 0o755);
    let (code, stdout, _) = f.run(Some(&exec), &[&exec, &groupish, &usable], &[]);
    assert_eq!((code, stdout), (2, usage_block(&prog)));
}

/// A searchable *directory* named `git-p4` passes `access(X_OK)` but fails
/// `is_executable()`'s `S_ISREG`, and stock git 2.55.0 walks past it.
#[test]
fn a_directory_named_like_the_helper_is_not_a_candidate() {
    let f = Fixture::new("dircand");
    let exec = f.dir("exec");
    let decoy = f.dir("decoy");
    std::fs::create_dir_all(decoy.join("git-p4")).unwrap();
    let usable = f.dir("usable");
    let prog = Fixture::helper(&usable, 0o755);
    let (code, stdout, _) = f.run(Some(&exec), &[&exec, &decoy, &usable], &[]);
    assert_eq!((code, stdout), (2, usage_block(&prog)));
}

/// Without `GIT_EXEC_PATH` the shadow's exec-path is `$HOME/.zvcs/bin`, the
/// directory `git zdashed` installs the `git-p4` entry into.
#[test]
fn without_git_exec_path_the_zvcs_bin_directory_is_searched() {
    let f = Fixture::new("homeexec");
    let bin = f.dir(".zvcs/bin");
    let prog = Fixture::helper(&bin, 0o755);
    let empty = f.dir("empty");
    let (code, stdout, _) = f.run(None, &[&empty], &[]);
    assert_eq!((code, stdout), (2, usage_block(&prog)));
}

/// No `git-p4` anywhere leaves nothing to resolve. The banner still has to be
/// printed — the shadow serves `p4` from its own binary — so it keeps the
/// exec-path spelling, and the exit stays 2. Refusing here instead would turn
/// git's usage-and-2 into a failure, which is what this guards.
#[test]
fn with_no_helper_anywhere_the_exec_path_spelling_is_used() {
    let f = Fixture::new("nohelper");
    let exec = f.dir("exec");
    let empty = f.dir("empty");
    let (code, stdout, stderr) = f.run(Some(&exec), &[&empty], &["no-such-sub"]);
    let expected =
        format!("unknown command no-such-sub\n\n{}", usage_block(&exec.join("git-p4")));
    assert_eq!((code, stdout, stderr), (2, expected, String::new()));
}
