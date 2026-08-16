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
//! The banner interpolates `sys.argv[0]`, the `git-p4` that printed it. Stock
//! ships `git-p4` inside its exec-path and `setup_path()` prepends the exec-path
//! to `PATH`, so every banner stock can print reads `<exec-path>/git-p4`. The
//! shadow serves `p4` from this binary (`p4` is a dispatcher verb, so
//! `run_argv`'s external arm is never reached) and its own `git-p4` is the
//! `git zdashed` symlink in its exec-path, so the same spelling is the honest
//! one — see `porcelain::p4::prog_path`.
//!
//! That is what these tests pin down: the banner names the shadow's *own*
//! exec-path, and nothing on `PATH` can move it. A `git-p4` belonging to some
//! other installation is not a program this binary would ever exec, so
//! attributing the output to it would be a lie — and, on a developer machine
//! whose `PATH` holds an installed shadow, a machine-dependent one. Every
//! expectation is the text stock git 2.55.0 printed for the same argv with its
//! own exec-path substituted, and every run gets a `PATH` and a `HOME` built
//! inside the fixture.

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
    /// never run — these files exist to prove the banner ignores them.
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
// sys.argv[0] — the program the banner names
// ---------------------------------------------------------------------------

/// `setup_path()` prepends the exec-path to `PATH`, so an exec-path helper wins
/// over one that `PATH` also offers — the case where the shadow's spelling and
/// stock's resolution agree by construction.
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

/// The regression the differential harness caught: an *executable* `git-p4`
/// on `PATH`, outside the exec-path, must not be named. Stock would exec such a
/// script and report it as `sys.argv[0]`; the shadow never execs it — `p4` is a
/// dispatcher verb it serves itself — so naming it would credit output to a
/// program that did not run. On a machine whose `PATH` holds an installed
/// shadow (`~/.zvcs/bin`) this is also what made the banner machine-dependent
/// while stock's stayed rooted at its own exec-path.
#[test]
fn a_helper_on_path_outside_the_exec_path_is_not_named() {
    let f = Fixture::new("pathfallback");
    let exec = f.dir("exec");
    let elsewhere = f.dir("elsewhere");
    Fixture::helper(&elsewhere, 0o755);
    let (code, stdout, _) = f.run(Some(&exec), &[&exec, &elsewhere], &[]);
    assert_eq!((code, stdout), (2, usage_block(&exec.join("git-p4"))));
}

/// The whole banner is a function of the exec-path alone: the same argv under
/// two very different `PATH`s — one strewn with candidate helpers, one holding
/// only an empty directory — is byte-identical. Asserted as an equality between
/// two runs rather than against a literal, so it keeps holding whatever the
/// spelling becomes.
#[test]
fn the_banner_does_not_change_with_path_contents() {
    let f = Fixture::new("pathindep");
    let exec = f.dir("exec");
    let empty = f.dir("empty");
    let usable = f.dir("usable");
    Fixture::helper(&usable, 0o755);
    let bare = f.run(Some(&exec), &[&empty], &[]);
    let crowded = f.run(Some(&exec), &[&usable, &exec, &empty], &[]);
    assert_eq!(bare, crowded);
    assert_eq!(bare, (2, usage_block(&exec.join("git-p4")), String::new()));
}

/// The mode of a file named `git-p4` is not consulted, in the exec-path or out
/// of it. Stock's `is_executable()` skips a non-executable candidate and a
/// group/other-only one (`0o644`, `0o011`) because it is choosing something to
/// exec; the shadow is not choosing, so all three modes give the same banner.
#[test]
fn candidate_file_modes_do_not_affect_the_banner() {
    let f = Fixture::new("modes");
    let expected = |exec: &Path| (2, usage_block(&exec.join("git-p4")), String::new());
    for (tag, mode) in [("unreadable", 0o644), ("groupish", 0o011), ("usable", 0o755)] {
        let exec = f.dir(&format!("exec-{tag}"));
        let elsewhere = f.dir(&format!("path-{tag}"));
        Fixture::helper(&exec, mode);
        Fixture::helper(&elsewhere, mode);
        assert_eq!(f.run(Some(&exec), &[&exec, &elsewhere], &[]), expected(&exec), "mode {mode:o}");
    }
}

/// A searchable *directory* named `git-p4` is the decoy stock's `S_ISREG` test
/// exists to walk past. It cannot mislead the shadow either, in the exec-path
/// (where the spelling is used regardless) or on `PATH` (which is not read).
#[test]
fn a_directory_named_like_the_helper_does_not_change_the_banner() {
    let f = Fixture::new("dircand");
    let exec = f.dir("exec");
    let decoy = f.dir("decoy");
    std::fs::create_dir_all(exec.join("git-p4")).unwrap();
    std::fs::create_dir_all(decoy.join("git-p4")).unwrap();
    let usable = f.dir("usable");
    Fixture::helper(&usable, 0o755);
    let (code, stdout, _) = f.run(Some(&exec), &[&exec, &decoy, &usable], &[]);
    assert_eq!((code, stdout), (2, usage_block(&exec.join("git-p4"))));
}

/// Without `GIT_EXEC_PATH` the shadow's exec-path is `$HOME/.zvcs/bin`, the
/// directory `git zdashed` installs the `git-p4` entry into — so that is what
/// the banner names, even with `PATH` pointing elsewhere entirely.
#[test]
fn without_git_exec_path_the_zvcs_bin_directory_is_named() {
    let f = Fixture::new("homeexec");
    let bin = f.dir(".zvcs/bin");
    Fixture::helper(&bin, 0o755);
    let empty = f.dir("empty");
    let (code, stdout, _) = f.run(None, &[&empty], &[]);
    assert_eq!((code, stdout), (2, usage_block(&bin.join("git-p4"))));
}

/// No `git-p4` on disk anywhere. The banner still has to be printed — the
/// shadow serves `p4` from its own binary — with the same exec-path spelling,
/// and the exit stays 2. Refusing here instead would turn git's usage-and-2 into
/// a failure, which is what this guards.
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
