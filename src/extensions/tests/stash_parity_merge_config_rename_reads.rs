//! `merge_recursive_config()`'s rename reads against stock git, and the children
//! that carry them.
//!
//! * `merge_recursive_config()` (merge-ort.c:5453-5493) reads `diff.renames` and
//!   `merge.renames` with `repo_config_get_string()` and hands them to
//!   `git_config_rename()`, whose `git_config_bool()` dies on anything
//!   `git_parse_maybe_bool()` cannot read: a value past `int` range, a `k`/`m`/`g`
//!   suffix that overflows it, or a word such as `always`. A valueless key is
//!   `config_error_nonbool()` plus `git_die_config()`. `merge.renormalize`,
//!   `merge.directoryrenames` and the `ui` read of `diff.algorithm` sit beside
//!   them. `stash apply`/`pop`, `cherry-pick` and `merge` all reach it, and die
//!   there at 128 with the stash still in `refs/stash`.
//! * The `git status` child `do_apply_stash()` runs gets
//!   `GIT_DIR=absolute_path(...)` (builtin/stash.c:762-768), so a refusal names
//!   the repository's config files by their absolute path.
//! * An autostash is re-applied by a `git stash apply` child with stderr closed
//!   (sequencer.c:4798-4826). When that child dies on the configuration the
//!   snapshot is stored and `error: could not apply autostash; ...` is all that is
//!   said; the command carries on and exits 0. The `git stash create` child that
//!   makes the autostash dies on `git_diff_basic_config`, and its parent with
//!   `Cannot autostash`.
//!
//! Each case runs the same script under stock git and zvcs and compares stdout,
//! stderr (with the fixture root replaced), exit status and the refs left behind.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git_at_least;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const DATE: &str = "1136214245 +0000";

struct Repo {
    root: PathBuf,
    work: PathBuf,
    bin: String,
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, PartialEq)]
struct Ran {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

impl Repo {
    fn new(tag: &str, bin: &str) -> Self {
        let label = if bin == BIN { "zvcs" } else { "stock" };
        let root = std::env::temp_dir()
            .join(format!("zvcs-stash-rename-reads-{tag}-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("work")).unwrap();
        let root = root.canonicalize().unwrap();
        let work = root.join("work");
        let repo = Repo { root, work, bin: bin.to_owned() };
        repo.ok(&["init", "-q", "-b", "main", "."]);
        repo
    }

    fn run(&self, args: &[&str]) -> Ran {
        let out = Command::new(&self.bin)
            .args(args)
            .current_dir(&self.work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .env("GIT_AUTHOR_DATE", DATE)
            .env("GIT_COMMITTER_DATE", DATE)
            .env("ZVCS_HOME", &self.root)
            .output()
            .unwrap();
        let root = self.root.to_string_lossy().into_owned();
        Ran {
            stdout: String::from_utf8_lossy(&out.stdout).replace(&root, "<root>"),
            stderr: String::from_utf8_lossy(&out.stderr).replace(&root, "<root>"),
            code: out.status.code(),
        }
    }

    fn ok(&self, args: &[&str]) {
        let r = self.run(args);
        assert_eq!(r.code, Some(0), "git {args:?}: {}", r.stderr);
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.work.join(rel), body).unwrap();
    }

    fn append_config(&self, text: &str) {
        let path = self.work.join(".git/config");
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str(text);
        std::fs::write(path, body).unwrap();
    }

    /// Every ref and the stash list, for the state a refusal leaves behind.
    fn refs(&self) -> String {
        self.run(&["for-each-ref", "--format=%(refname) %(objectname)"]).stdout
    }
}

/// `f` committed, then edited and stashed.
fn stashed(tag: &str, bin: &str) -> Repo {
    let r = Repo::new(tag, bin);
    r.write("f", "a\n");
    r.ok(&["add", "f"]);
    r.ok(&["commit", "-q", "-m", "i"]);
    r.write("f", "b\n");
    r.ok(&["stash", "-q"]);
    r
}

/// `main` and `side` each change a different file from `i`; `main` is checked
/// out with `f` edited locally.
fn branched(tag: &str, bin: &str) -> Repo {
    let r = Repo::new(tag, bin);
    r.write("f", "a\n");
    r.write("g", "z\n");
    r.ok(&["add", "f", "g"]);
    r.ok(&["commit", "-q", "-m", "i"]);
    r.ok(&["checkout", "-q", "-b", "side"]);
    r.write("g", "b\n");
    r.ok(&["commit", "-q", "-a", "-m", "s"]);
    r.ok(&["checkout", "-q", "main"]);
    r.write("f", "a\nd\n");
    r
}

/// Run `case` under both binaries and compare what it returns.
fn compare<T: PartialEq + std::fmt::Debug>(case: impl Fn(&str) -> T) {
    let Some(stock) = stock_git_at_least((2, 56, 0)) else { return };
    assert_eq!(case(BIN), case(stock));
}

#[test]
fn an_overflowing_diff_renames_kills_pop_before_the_status_child() {
    compare(|bin| {
        let r = stashed("fuzz", bin);
        r.ok(&["config", "diff.renames", "99999999999999999999999999"]);
        r.ok(&["config", "extensions.worktreeConfig", "true"]);
        r.ok(&["config", "--worktree", "status.renameLimit", "always"]);
        let ran = r.run(&["-c", "status.renames=off", "stash", "pop"]);
        assert_eq!(ran.code, Some(128));
        (ran, r.refs())
    });
}

#[test]
fn bool_reads_take_the_int_grammar_and_its_range() {
    for (key, value) in [
        ("diff.renames", "2147483648"),
        ("diff.renames", "2147483647"),
        ("diff.renames", "-2147483649"),
        ("diff.renames", "0x80000000"),
        ("diff.renames", "1g"),
        ("diff.renames", "2g"),
        ("diff.renames", "always"),
        ("diff.renames", "copy"),
        ("merge.renames", "3g"),
        ("merge.renames", "1m"),
        ("merge.renormalize", "9999999999"),
        ("merge.directoryRenames", "bogus"),
        ("merge.renameLimit", "always"),
        ("diff.renameLimit", "3g"),
    ] {
        compare(|bin| {
            let r = stashed("bools", bin);
            r.ok(&["config", key, value]);
            (key, value, r.run(&["stash", "pop"]), r.refs())
        });
    }
}

#[test]
fn a_valueless_string_read_is_refused_with_its_line() {
    for (section, name) in [
        ("diff", "renames"),
        ("merge", "renames"),
        ("merge", "directoryRenames"),
        ("merge", "renormalize"),
        ("diff", "algorithm"),
    ] {
        compare(|bin| {
            let r = stashed("valueless", bin);
            r.append_config(&format!("[{section}]\n\t{name}\n"));
            (name, r.run(&["stash", "pop"]), r.refs())
        });
    }
}

#[test]
fn the_status_child_names_config_files_by_absolute_path() {
    for worktree in [false, true] {
        compare(|bin| {
            let r = stashed("child", bin);
            r.ok(&["config", "extensions.worktreeConfig", "true"]);
            let scope = if worktree { "--worktree" } else { "--local" };
            r.ok(&["config", scope, "status.renameLimit", "always"]);
            (r.run(&["stash", "pop"]), r.refs())
        });
    }
}

#[test]
fn merges_die_on_the_rename_reads() {
    for args in [
        &["merge", "--no-edit", "side"][..],
        &["cherry-pick", "side"],
        &["merge-tree", "--write-tree", "main", "side"],
    ] {
        compare(|bin| {
            let r = branched("merges", bin);
            r.ok(&["commit", "-q", "-a", "-m", "m"]);
            r.ok(&["config", "merge.renames", "3g"]);
            (r.run(args), r.refs())
        });
    }
}

#[test]
fn an_autostash_the_apply_child_cannot_read_is_stored() {
    for args in [
        &["merge", "--autostash", "side"][..],
        &["rebase", "--autostash", "side"],
        &["checkout", "-m", "side"],
    ] {
        for (key, value) in [("merge.renames", "3g"), ("merge.conflictStyle", "bogus")] {
            compare(|bin| {
                let r = branched("apply", bin);
                r.ok(&["config", key, value]);
                (r.run(args), r.refs(), r.run(&["stash", "list"]).stdout)
            });
        }
    }
}

#[test]
fn an_autostash_the_create_child_cannot_read_is_refused() {
    compare(|bin| {
        let r = branched("create", bin);
        r.ok(&["config", "diff.renameLimit", "3g"]);
        (r.run(&["rebase", "--autostash", "side"]), r.refs())
    });
}
