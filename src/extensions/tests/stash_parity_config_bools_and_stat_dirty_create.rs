//! `git_stash_config()` and `create_stash()`'s two change checks against stock git.
//!
//! * `cmd_stash()` runs `repo_config(the_repository, git_stash_config, NULL)` ahead of
//!   every subcommand, `-h` included (builtin/stash.c:2484). The callback reads
//!   `stash.showStat`, `stash.showPatch`, `stash.showIncludeUntracked` and `stash.index`
//!   through `git_config_bool()` and hands everything else to `git_diff_basic_config`
//!   (builtin/stash.c:978-998), entry by entry, so the first refused value in config
//!   order is the one reported. `show` and `list` read more after it.
//! * The `git stash create`, `apply` and `store` children the autostash callers and
//!   `merge`'s `save_state()` spawn run the same read first.
//! * `create_stash()` asks `check_changes_tracked_files()` over an unrefreshed index,
//!   where a file whose stat moved is a change, then `do_create_stash()` refreshes and
//!   finds nothing: exit 1, nothing printed (builtin/stash.c:1646-1668, 1521-1543).
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
            .join(format!("zvcs-stash-config-bools-{tag}-{label}-{}", std::process::id()));
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
            .env("GIT_CEILING_DIRECTORIES", &self.root)
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

    /// Every ref, and the worktree state a refusal leaves behind.
    fn state(&self) -> (String, String) {
        (
            self.run(&["for-each-ref", "--format=%(refname) %(objectname)"]).stdout,
            self.run(&["status", "--porcelain"]).stdout,
        )
    }
}

/// `f` committed, edited and stashed; `side` changes `g`; `main` is checked out with
/// `f` edited again.
fn fixture(tag: &str, bin: &str) -> Repo {
    let r = Repo::new(tag, bin);
    r.write("f", "a\n");
    r.write("g", "z\n");
    r.ok(&["add", "f", "g"]);
    r.ok(&["commit", "-q", "-m", "i"]);
    r.write("f", "b\n");
    r.ok(&["stash", "-q"]);
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
fn every_subcommand_reads_the_stash_booleans_first() {
    for key in ["stash.showStat", "stash.showPatch", "stash.showIncludeUntracked", "stash.index"] {
        for args in [
            &["stash"][..],
            &["stash", "-h"],
            &["stash", "list"],
            &["stash", "list", "-h"],
            &["stash", "show"],
            &["stash", "apply"],
            &["stash", "pop"],
            &["stash", "drop"],
            &["stash", "clear"],
            &["stash", "create"],
            &["stash", "store", "HEAD"],
            &["stash", "branch", "nb"],
            &["stash", "nosuch"],
        ] {
            compare(|bin| {
                let r = fixture("subcommands", bin);
                let setting = format!("{key}=bogus");
                let mut full = vec!["-c", &setting];
                full.extend_from_slice(args);
                (args, r.run(&full), r.state())
            });
        }
    }
}

#[test]
fn the_first_refused_value_in_config_order_wins() {
    for pair in [
        ["stash.index=bogus", "diff.renameLimit=bogus"],
        ["diff.renameLimit=bogus", "stash.index=bogus"],
        ["stash.showStat=bogus", "diff.context=-1"],
        ["diff.context=-1", "stash.showStat=bogus"],
        ["color.ui=bogus", "stash.showPatch=bogus"],
        ["stash.index=bogus", "stash.index=true"],
    ] {
        for sub in ["push", "show", "list"] {
            compare(|bin| {
                let r = fixture("order", bin);
                (pair, sub, r.run(&["-c", pair[0], "-c", pair[1], "stash", sub]), r.state())
            });
        }
    }
}

#[test]
fn booleans_take_the_int_grammar_and_its_range() {
    for value in ["", "5", "0x", "0x10", "1k", "99999999999", "on"] {
        compare(|bin| {
            let r = fixture("grammar", bin);
            let setting = format!("stash.showStat={value}");
            (value, r.run(&["-c", &setting, "stash", "list"]))
        });
    }
}

#[test]
fn outside_a_repository_only_the_lone_help_reads_it() {
    for args in [&["stash", "-h"][..], &["stash", "list"]] {
        compare(|bin| {
            let r = Repo::new("norepo", bin);
            std::fs::remove_dir_all(r.work.join(".git")).unwrap();
            let mut full = vec!["-c", "stash.showStat=bogus"];
            full.extend_from_slice(args);
            let ran = r.run(&full);
            (args, ran.stderr.lines().next().map(str::to_owned), ran.code)
        });
    }
}

#[test]
fn the_autostash_children_die_on_a_stash_boolean() {
    for args in [
        &["merge", "--autostash", "side"][..],
        &["rebase", "--autostash", "side"],
        &["merge", "-s", "ours", "--no-edit", "side"],
    ] {
        for key in ["stash.showStat", "stash.index"] {
            compare(|bin| {
                let r = fixture("autostash", bin);
                let setting = format!("{key}=bogus");
                let mut full = vec!["-c", &setting];
                full.extend_from_slice(args);
                (args, r.run(&full), r.state())
            });
        }
    }
}

#[test]
fn create_over_a_stat_only_change_exits_1() {
    for args in [
        &["stash", "create"][..],
        &["stash", "create", "msg"],
        &["stash", "push"],
        &["stash"],
        &["stash", "save", "m"],
    ] {
        compare(|bin| {
            let r = Repo::new("statdirty", bin);
            r.write("f", "a\n");
            r.ok(&["add", "f"]);
            r.ok(&["commit", "-q", "-m", "i"]);
            // Same bytes, moved mtime: stat-dirty until something refreshes.
            let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
            std::fs::File::options()
                .write(true)
                .open(r.work.join("f"))
                .unwrap()
                .set_modified(past)
                .unwrap();
            let ran = r.run(args);
            // The refresh `do_create_stash()` wrote leaves nothing for `diff-files`.
            (args, ran, r.run(&["diff-files", "--name-only"]).stdout, r.state())
        });
    }
}

/// The index's extension section — the cache-tree first — as raw bytes: everything from
/// the `TREE` signature to the trailing checksum, which carries no stat data.
fn cache_tree_bytes(r: &Repo) -> Vec<u8> {
    let index = std::fs::read(r.work.join(".git/index")).unwrap();
    match index.windows(4).position(|w| w == b"TREE") {
        Some(at) => index[at..index.len() - 20].to_vec(),
        None => Vec::new(),
    }
}

#[test]
fn an_unborn_create_is_minus_1_and_a_created_stash_writes_the_cache_tree() {
    // `do_create_stash()`'s "You do not have the initial commit yet" is `ret = -1`,
    // which `cmd_stash()` maps to 128 (builtin/stash.c:1530-1535, 2498-2510).
    compare(|bin| {
        let r = Repo::new("unborn", bin);
        r.write("f", "a\n");
        r.ok(&["add", "f"]);
        (r.run(&["stash", "create"]), r.state())
    });
    // A new staged path leaves the root cache-tree invalid; `write_index_as_tree()`
    // (builtin/stash.c:1561, cache-tree.c:818) fills it in and writes the index back. A
    // stat-only change stops at `ret = 1` first and leaves the extension as it was.
    for staged in [true, false] {
        compare(|bin| {
            let r = Repo::new("cachetree", bin);
            std::fs::create_dir(r.work.join("d")).unwrap();
            r.write("f", "a\n");
            r.write("d/x", "b\n");
            r.ok(&["add", "f", "d"]);
            r.ok(&["commit", "-q", "-m", "i"]);
            if staged {
                r.write("n", "c\n");
                r.ok(&["add", "n"]);
            } else {
                r.ok(&["rm", "-q", "--cached", "f"]);
                r.ok(&["add", "f"]);
                let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
                std::fs::File::options().write(true).open(r.work.join("f")).unwrap().set_modified(past).unwrap();
            }
            let ran = r.run(&["stash", "create"]);
            (staged, ran.stderr, ran.code, cache_tree_bytes(&r), r.state())
        });
    }
}
