//! `git submodule` refuses outside a work tree, before it parses anything.
//!
//! `git-submodule.sh:23` calls `require_work_tree`, the fifth line of the
//! script and well ahead of its own option loop:
//!
//! ```sh
//! require_work_tree () {
//!         test "$(git rev-parse --is-inside-work-tree 2>/dev/null)" = true || {
//!                 program_name=$0
//!                 die "$(eval_gettext "fatal: \$program_name cannot be used without a working tree.")"
//!         }
//! }
//! ```
//!
//! (`git-sh-setup.sh:186-191`.) Three details fall out of the shell and are easy
//! to get wrong in a rewrite:
//!
//! * The exit code is **1**, not the 128 a C `die()` would give: `die` is
//!   `die_with_status 1` (`git-sh-setup.sh:49-57`).
//! * The `fatal: ` prefix is part of the message *text*; `die` only adds the
//!   newline.
//! * `$program_name` is `$0`, the path the script was execed under — for stock
//!   that is `<exec-path>/git-submodule`. The shadow serves every `git-*` helper
//!   out of its own bin directory, so `git --exec-path` is the analogue, and a
//!   `git-submodule` link really does live there.
//!
//! The gate precedes subcommand dispatch, so every subcommand refuses — an
//! unknown one included, which is why this is not a per-subcommand check. Only a
//! leading `-h` escapes, because `. git-sh-setup` answers that on line 22 before
//! `require_work_tree` on line 23 runs.
//!
//! `git submodule--helper` is a C builtin that never sources `git-sh-setup`, so
//! it has no such gate: `submodule--helper status` in a bare repository reads an
//! index that is not there, finds no gitlinks and exits 0 in silence. Leaking
//! gitoxide's "An IO error occurred while opening the index" there is the same
//! bug wearing a different hat — git's `repo_read_index()` treats a missing
//! index as an empty one.
//!
//! Every expectation was captured from stock git 2.55.0.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    bare: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A bare repository and a work-tree repository with one commit, both under
    /// the same `HOME` so the exec-path the message names is predictable.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-submodgate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let (bare, work) = (root.join("bare.git"), root.join("work"));
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&work).unwrap();

        // The exec-path is `$HOME/.zvcs/bin` when `GIT_EXEC_PATH` is unset, and
        // the real one holds a `git-submodule` link into the single binary. The
        // fixture's `HOME` gets the same, so the path the message names is a
        // file that exists — the whole point of naming `$0`.
        let exec_dir = root.join(".zvcs/bin");
        std::fs::create_dir_all(&exec_dir).unwrap();
        std::os::unix::fs::symlink(BIN, exec_dir.join("git-submodule")).unwrap();

        let fx = Fixture { root, bare, work };

        let out = fx.run(&fx.bare, &["init", "-q", "--bare", "-b", "main", "."]);
        assert!(out.status.success(), "bare init: {out:?}");

        for args in [
            &["init", "-q", "-b", "main", "."][..],
            &["-c", "user.email=t@e.co", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "one"][..],
        ] {
            let out = fx.run(&fx.work, args);
            assert!(out.status.success(), "work setup git {args:?}: {out:?}");
        }
        fx
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.root.join("gitconfig-system"))
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_EXEC_PATH")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run binary")
    }

    /// The line stock builds out of `$0`, with this build's exec-path in place
    /// of `<libexec>/git-core`. Read back from the binary rather than assumed,
    /// so the test states the contract instead of duplicating the formula.
    fn refusal(&self) -> String {
        let out = self.run(&self.root, &["--exec-path"]);
        assert!(out.status.success(), "git --exec-path: {out:?}");
        let exec_path = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
        assert!(
            Path::new(&exec_path).join("git-submodule").exists(),
            "the exec-path names no git-submodule, so the message would name a file that is not there: {exec_path}"
        );
        format!("fatal: {exec_path}/git-submodule cannot be used without a working tree.\n")
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn every_subcommand_refuses_in_a_bare_repository() {
    let fx = Fixture::new("bare");
    let expected = fx.refusal();

    // The gate is ahead of dispatch, so the list is not "the subcommands that
    // need a work tree" — it is all of them, plus the bare form that defaults to
    // `status`, plus the global flags, plus a name that is not a subcommand at
    // all. A `bogus` that answered with the usage block would mean the gate had
    // slipped behind the parser.
    for args in [
        &["submodule"][..],
        &["submodule", "status"][..],
        &["submodule", "init"][..],
        &["submodule", "update"][..],
        &["submodule", "sync"][..],
        &["submodule", "summary"][..],
        &["submodule", "foreach", "echo"][..],
        &["submodule", "deinit", "--all"][..],
        &["submodule", "absorbgitdirs"][..],
        &["submodule", "add", "x", "y"][..],
        &["submodule", "set-url", "a", "b"][..],
        &["submodule", "set-branch", "--default", "a"][..],
        &["submodule", "-q", "status"][..],
        &["submodule", "--cached"][..],
        &["submodule", "bogus"][..],
    ] {
        let out = fx.run(&fx.bare, args);
        assert_eq!(out.status.code(), Some(1), "git {args:?} exit: {out:?}");
        assert_eq!(stderr(&out), expected, "git {args:?} stderr");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "git {args:?} stdout");
    }
}

#[test]
fn standing_in_the_git_directory_is_also_outside_the_work_tree() {
    let fx = Fixture::new("gitdir");
    let expected = fx.refusal();

    // `rev-parse --is-inside-work-tree` answers `false` from inside `.git`, so
    // the gate fires in an ordinary repository too — testing `core.bare` alone
    // would miss this.
    let dot_git = fx.work.join(".git");
    let out = fx.run(&dot_git, &["rev-parse", "--is-inside-work-tree"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "false\n", "fixture assumption: {out:?}");

    let out = fx.run(&dot_git, &["submodule", "status"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(stderr(&out), expected);
}

#[test]
fn a_leading_dash_h_is_answered_before_the_gate() {
    let fx = Fixture::new("help");

    // `. git-sh-setup` (line 22) handles `-h` before `require_work_tree`
    // (line 23), on stdout, with a bare `exit` — status 0.
    let out = fx.run(&fx.bare, &["submodule", "-h"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stderr(&out), "", "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("usage: git submodule [--quiet] [--cached]\n"),
        "{out:?}"
    );

    // Only as the *first* argument: the `case "$1"` never looks past it.
    let out = fx.run(&fx.bare, &["submodule", "--quiet", "-h"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(stderr(&out), fx.refusal());
}

#[test]
fn the_gate_stands_down_where_a_work_tree_exists() {
    let fx = Fixture::new("worktree");

    // A repository with a work tree and no submodules lists nothing and exits 0;
    // the refusal must not have become unconditional.
    let out = fx.run(&fx.work, &["submodule", "status"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stderr(&out), "", "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{out:?}");

    // Outside a repository altogether, `. git-sh-setup` dies with git's own
    // discovery error first, so the work-tree refusal never gets a turn.
    let elsewhere = fx.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let out = fx.run(&elsewhere, &["submodule", "status"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert!(
        stderr(&out).starts_with("fatal: not a git repository"),
        "discovery error was replaced by the work-tree refusal: {out:?}"
    );
}

#[test]
fn submodule_helper_has_no_gate_and_reads_a_missing_index_as_empty() {
    let fx = Fixture::new("helper");

    // The C builtin never sources `git-sh-setup`, so it runs — and a bare
    // repository simply has no index to find gitlinks in. git's
    // `repo_read_index()` calls that zero entries, not an error.
    for verb in ["status", "init", "sync"] {
        let out = fx.run(&fx.bare, &["submodule--helper", verb]);
        assert_eq!(out.status.code(), Some(0), "submodule--helper {verb}: {out:?}");
        assert_eq!(stderr(&out), "", "submodule--helper {verb} stderr");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "submodule--helper {verb} stdout");
    }
}
