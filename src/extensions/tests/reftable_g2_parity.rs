//! The verbs that read or rewrite reflogs — `reflog`, `stash`, `checkout`,
//! `gc`, `prune`, `repack`, `pack-objects`, `rev-list --reflog`, `fsck` — in a
//! repository whose references are stored in reftables, against stock git.
//!
//! Every case builds the same repository twice with stock git, runs one
//! sequence of command lines on one copy with stock git and on the other with
//! zvcs, and compares each command's output and exit code and what stock git
//! then reads back from each copy: `refs verify`, `fsck`,
//! `for-each-ref --include-root-refs`, every worktree's reflogs, and which files
//! the git directory holds (a reftable store writes no `logs/`).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Case {
    root: PathBuf,
    stock: &'static str,
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The repositories a case starts from.
#[derive(Clone, Copy)]
enum Fixture {
    /// `R`: reftable, commits `one` and `two` on `main`, branch `side`, annotated tag `v1`.
    R,
    /// `R` with the linked worktree `wt` on `side`.
    Rw,
    /// `R` in the files format.
    RFiles,
}

/// One command line, and whether stock git runs it on both copies as setup.
type Step<'a> = &'a [&'a str];

impl Case {
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-reftable-g2-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Some(Case { root, stock })
    }

    fn run(&self, bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
        let out = Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_DEFAULT_REF_FORMAT")
            .env_remove("GIT_TEST_REFTABLE_AUTOCOMPACTION")
            .env_remove("GIT_REFLOG_ACTION")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()
            .unwrap();
        let side = dir.ancestors().find(|p| p.parent() == Some(self.root.as_path())).unwrap().to_owned();
        let norm = |b: &[u8]| String::from_utf8_lossy(b).replace(side.to_str().unwrap(), "ROOT");
        (norm(&out.stdout), norm(&out.stderr), out.status.code())
    }

    fn build(&self, side: &str, fixture: Fixture, setup: &[Step]) -> PathBuf {
        let dir = self.root.join(side);
        std::fs::create_dir_all(&dir).unwrap();
        let format = match fixture {
            Fixture::RFiles => "--ref-format=files",
            _ => "--ref-format=reftable",
        };
        let mut steps: Vec<Vec<&str>> = vec![
            vec!["init", "-q", "-b", "main", format, "R"],
            vec!["-C", "R", "commit", "-q", "--allow-empty", "-m", "one"],
            vec!["-C", "R", "commit", "-q", "--allow-empty", "-m", "two"],
            vec!["-C", "R", "branch", "side"],
            vec!["-C", "R", "tag", "-a", "v1", "-m", "t"],
        ];
        if let Fixture::Rw = fixture {
            steps.push(vec!["-C", "R", "worktree", "add", "-q", "../wt", "side"]);
        }
        for args in steps {
            let (_, err, code) = self.run(self.stock, &dir, &args);
            assert_eq!(code, Some(0), "{args:?}: {err}");
        }
        for args in setup {
            let (_, err, code) = self.run(self.stock, &dir.join("R"), args);
            assert_eq!(code, Some(0), "setup {args:?}: {err}");
        }
        dir
    }

    /// Stock git's view of every repository under `side`.
    fn state(&self, side: &Path) -> String {
        let mut out = String::new();
        for name in ["R", "wt"] {
            let dir = side.join(name);
            if !dir.exists() {
                continue;
            }
            out += &format!("== {name}\n");
            for args in [
                &["refs", "verify"][..],
                &["fsck"],
                &["for-each-ref", "--include-root-refs"],
                &["reflog", "list"],
            ] {
                let (o, e, code) = self.run(self.stock, &dir, args);
                out += &format!("$ {args:?} {code:?}\n{o}{e}");
            }
            let (names, _, _) = self.run(self.stock, &dir, &["reflog", "list"]);
            for name in names.lines() {
                let (o, e, _) =
                    self.run(self.stock, &dir, &["log", "-g", "--date=raw", "--format=%H %gd %gs %gn", name, "--"]);
                out += &format!("-- {name}\n{o}{e}");
            }
        }
        let git_dir = side.join("R/.git");
        let mut files = Vec::new();
        list_files(&git_dir, &git_dir, &mut files);
        files.sort();
        out += &files.join("\n");
        out.push('\n');
        // The tables themselves, in stack order: their names are random, their
        // bytes are not, so a reference written without the peeled value git
        // stores, or a log record git would not write, shows here.
        for stack in [git_dir.join("reftable"), git_dir.join("worktrees/wt/reftable")] {
            let Ok(list) = std::fs::read_to_string(stack.join("tables.list")) else { continue };
            for table in list.lines() {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::hash::DefaultHasher::new();
                std::fs::read(stack.join(table)).unwrap().hash(&mut hasher);
                out += &format!("table {:016x}\n", hasher.finish());
            }
        }
        // A worktree's `gitdir` file names its side by the resolved path.
        let real = std::fs::canonicalize(side).unwrap();
        out.replace(real.to_str().unwrap(), "ROOT").replace(side.to_str().unwrap(), "ROOT")
    }

    /// Run `steps` in `cwd` of `fixture`, after `setup` (stock git on both
    /// copies, in `R`), with stock git and with zvcs, and assert every command
    /// and both resulting repositories look the same to stock git.
    fn compare(&self, fixture: Fixture, setup: &[Step], cwd: &str, steps: &[Step]) {
        let stock_side = self.build("s", fixture, setup);
        let zvcs_side = self.build("z", fixture, setup);
        for args in steps {
            let expected = self.run(self.stock, &stock_side.join(cwd), args);
            let actual = self.run(BIN, &zvcs_side.join(cwd), args);
            assert_eq!(actual, expected, "{args:?}: output and exit code");
        }
        assert_eq!(self.state(&zvcs_side), self.state(&stock_side), "{steps:?}: state");
        std::fs::remove_dir_all(&stock_side).unwrap();
        std::fs::remove_dir_all(&zvcs_side).unwrap();
    }
}

/// The files of a git directory other than objects, hooks, the index and the
/// reftable tables (whose names are random), each with its first bytes.
fn list_files(base: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if ["objects", "hooks", "index", "description", "COMMIT_EDITMSG", "exclude"].contains(&name.as_str())
            || name.ends_with(".ref")
        {
            continue;
        }
        if path.is_dir() {
            out.push(format!("{}/", path.strip_prefix(base).unwrap().display()));
            list_files(base, &path, out);
        } else if name == "tables.list" {
            let lines = std::fs::read_to_string(&path).unwrap().lines().count();
            out.push(format!("{}: {lines} tables", path.strip_prefix(base).unwrap().display()));
        } else {
            let text = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&text[..text.len().min(120)]).replace('\n', "|");
            out.push(format!("{}: {text}", path.strip_prefix(base).unwrap().display()));
        }
    }
}

/// `reflog list` and `reflog exists` ask the ref store (`refs_for_each_reflog()`,
/// `refs_reflog_exists()`): in a linked worktree its own `HEAD` merged with the
/// shared reflogs, and another worktree's through its prefix.
#[test]
fn reflog_list_and_exists_read_the_stacks() {
    let Some(case) = Case::new("list") else { return };
    let steps: &[Step] = &[
        &["reflog", "list"],
        &["reflog", "exists", "refs/heads/main"],
        &["reflog", "exists", "HEAD"],
        &["reflog", "exists", "refs/heads/nope"],
        &["reflog", "exists", "main-worktree/HEAD"],
        &["reflog", "exists", "worktrees/wt/HEAD"],
    ];
    case.compare(Fixture::R, &[], "R", steps);
    case.compare(Fixture::Rw, &[], "wt", steps);
    case.compare(Fixture::RFiles, &[], "R", steps);
}

/// `reflog expire --all` collects every worktree's reflogs in `get_worktrees()`
/// order (builtin/reflog.c:253-260) and prunes through the stack each lives in;
/// `--verbose` prints each entry's stored message, which the reftable writer
/// ends with a newline even when it is empty (reftable/writer.c:466-489).
#[test]
fn reflog_expire_all_walks_every_worktree() {
    let Some(case) = Case::new("expire") else { return };
    case.compare(
        Fixture::Rw,
        &[],
        "R",
        &[
            &["reflog", "expire", "--expire=now", "--all", "--verbose", "-n"],
            &["reflog", "expire", "--expire=now", "--expire-unreachable=now", "--verbose", "refs/heads/side"],
            &["reflog", "expire", "--expire=now", "--all", "--verbose"],
            &["reflog", "expire", "nosuchref"],
        ],
    );
    case.compare(
        Fixture::RFiles,
        &[],
        "R",
        &[&["reflog", "expire", "--expire=now", "--all", "--verbose"]],
    );
    // `UE_HEAD` reads every reference's tip for `HEAD`'s reflog while the
    // store holds the stack it expires; an entry no tip reaches goes.
    for fixture in [Fixture::Rw, Fixture::RFiles] {
        case.compare(
            fixture,
            &[&["commit", "-q", "--allow-empty", "-m", "dropped"], &["reset", "-q", "--hard", "HEAD~1"]],
            "R",
            &[
                &["reflog", "expire", "--all", "--expire=never"],
                &["reflog", "expire", "--expire=never", "--expire-unreachable=now", "--all", "--verbose"],
            ],
        );
    }
}

/// `reflog delete` counts its `@{n}` through the ref store and expires exactly
/// that entry; `--rewrite --updateref` chains the kept entries and points the
/// reference at the newest one.
#[test]
fn reflog_delete_rewrites_and_updates_the_ref() {
    let Some(case) = Case::new("delete") else { return };
    let setup: &[Step] = &[
        &["update-ref", "-m", "back", "refs/heads/side", "HEAD~1"],
        &["update-ref", "-m", "fwd", "refs/heads/side", "HEAD"],
    ];
    let steps: &[Step] = &[
        &["reflog", "delete", "--verbose", "--dry-run", "side@{1}"],
        &["reflog", "delete", "--rewrite", "--updateref", "side@{0}"],
        &["rev-parse", "side"],
        &["reflog", "delete", "HEAD@{1}"],
        &["reflog", "delete", "--verbose", "--dry-run", "HEAD@{0}~0"],
        &["reflog", "delete", "nope@{0}"],
        &["reflog", "delete", "side"],
    ];
    case.compare(Fixture::R, setup, "R", steps);
    case.compare(Fixture::RFiles, setup, "R", steps);
}

/// `--updateref` onto an entry naming an annotated tag stores the reference
/// with the peeled commit, which the policy's object-database `peel()` supplies
/// (`write_reflog_expiry_table()`, refs/reftable-backend.c:2534-2543).
#[test]
fn reflog_delete_updateref_peels_an_annotated_tag() {
    let Some(case) = Case::new("peel") else { return };
    case.compare(
        Fixture::R,
        &[
            &["update-ref", "--create-reflog", "-m", "a", "refs/tags/t2", "HEAD~1"],
            &["update-ref", "-m", "b", "refs/tags/t2", "v1"],
            &["update-ref", "-m", "c", "refs/tags/t2", "HEAD"],
        ],
        "R",
        &[
            &["reflog", "delete", "--updateref", "refs/tags/t2@{0}"],
            &["for-each-ref", "--format=%(refname) %(objectname) %(*objectname)", "refs/tags/"],
            &["show-ref", "-d", "--tags"],
        ],
    );
}

/// `reflog drop` deletes whole reflogs through the ref store.
#[test]
fn reflog_drop_deletes_through_the_stack() {
    let Some(case) = Case::new("drop") else { return };
    for fixture in [Fixture::R, Fixture::RFiles] {
        case.compare(
            fixture,
            &[],
            "R",
            &[&["reflog", "drop", "side", "nope"], &["reflog", "list"], &["reflog", "drop", "--all"]],
        );
    }
}

/// `stash drop` is `reflog_delete(rev, REWRITE | UPDATE_REF)` and clears the
/// stash once its reflog is empty (builtin/stash.c:826-843); the reflog the
/// stash lives in is read and rewritten through the ref store.
#[test]
fn stash_reflog_lives_in_the_stack() {
    let Some(case) = Case::new("stash") else { return };
    let steps: &[Step] = &[
        &["stash", "list"],
        &["stash", "drop", "stash@{1}"],
        &["stash", "list"],
        &["stash", "pop"],
        &["stash", "drop"],
        &["stash", "list"],
    ];
    for fixture in [Fixture::R, Fixture::RFiles] {
        let stock_side = case.build("s", fixture, &[]);
        let zvcs_side = case.build("z", fixture, &[]);
        // Two stashes, made by stock git on both copies.
        for side in [&stock_side, &zvcs_side] {
            let r = side.join("R");
            for content in ["a", "b"] {
                std::fs::write(r.join("f"), content).unwrap();
                let (_, err, code) = case.run(case.stock, &r, &["add", "f"]);
                assert_eq!(code, Some(0), "{err}");
                let (_, err, code) = case.run(case.stock, &r, &["stash", "-q"]);
                assert_eq!(code, Some(0), "{err}");
            }
        }
        for args in steps {
            let expected = case.run(case.stock, &stock_side.join("R"), args);
            let actual = case.run(BIN, &zvcs_side.join("R"), args);
            assert_eq!(actual, expected, "{args:?}: output and exit code");
        }
        assert_eq!(case.state(&zvcs_side), case.state(&stock_side), "stash: state");
        std::fs::remove_dir_all(&stock_side).unwrap();
        std::fs::remove_dir_all(&zvcs_side).unwrap();
    }
}

/// `checkout` moves `HEAD` through the store, which logs each move once; no
/// files-format `HEAD` or `logs/HEAD` is written, `--orphan` included.
#[test]
fn checkout_logs_head_once() {
    let Some(case) = Case::new("checkout") else { return };
    case.compare(
        Fixture::R,
        &[],
        "R",
        &[
            &["checkout", "-q", "side"],
            &["checkout", "-q", "-b", "x"],
            &["checkout", "-q", "--detach"],
            &["checkout", "-q", "-"],
            &["checkout", "-q", "v1"],
            &["checkout", "-q", "main"],
            &["checkout", "--orphan", "o"],
            &["symbolic-ref", "HEAD"],
            &["checkout", "-q", "main"],
        ],
    );
}

/// Every reachability walk reads the reflogs of every worktree through the
/// ref store (`add_reflogs_to_pending()`, revision.c:1735-1747), and a reflog
/// naming an object the repository lost is `handle_one_reflog_commit()`'s
/// warning (revision.c:1670-1685) — or `fsck`'s `invalid reflog entry`.
#[test]
fn reachability_walks_read_reflogs_from_the_stacks() {
    let Some(case) = Case::new("reach") else { return };
    for fixture in [Fixture::Rw, Fixture::RFiles] {
        let stock_side = case.build("s", fixture, &[]);
        let zvcs_side = case.build("z", fixture, &[]);
        for side in [&stock_side, &zvcs_side] {
            let r = side.join("R");
            // A commit only `side`'s reflog names, its object then removed.
            let (tree, _, _) = case.run(case.stock, &r, &["rev-parse", "HEAD^{tree}"]);
            let (id, _, _) = case.run(case.stock, &r, &["commit-tree", "-m", "lost", tree.trim()]);
            let id = id.trim().to_owned();
            let (_, err, code) = case.run(case.stock, &r, &["update-ref", "-m", "lost", "refs/heads/side", &id]);
            assert_eq!(code, Some(0), "{err}");
            let (_, err, code) = case.run(case.stock, &r, &["update-ref", "-m", "back", "refs/heads/side", "HEAD"]);
            assert_eq!(code, Some(0), "{err}");
            std::fs::remove_file(r.join(".git/objects").join(&id[..2]).join(&id[2..])).unwrap();
        }
        for args in [
            &["rev-list", "--reflog"][..],
            &["rev-list", "--reflog", "--all", "--count"],
            &["fsck"],
            &["prune", "-n"],
            &["repack", "-a", "-d", "-q"],
            &["gc", "-q"],
            &["rev-list", "--reflog", "--count"],
        ] {
            let expected = case.run(case.stock, &stock_side.join("R"), args);
            let actual = case.run(BIN, &zvcs_side.join("R"), args);
            assert_eq!(actual, expected, "{args:?}: output and exit code");
        }
        assert_eq!(case.state(&zvcs_side), case.state(&stock_side), "reach: state");
        std::fs::remove_dir_all(&stock_side).unwrap();
        std::fs::remove_dir_all(&zvcs_side).unwrap();
    }
}

/// `add_reflogs_to_pending()` visits the reflogs in the ref store's name order,
/// so a root ref's reflog sorting before `HEAD` pends first and decides the
/// order of commits that share a date.
#[test]
fn reflog_walk_follows_the_store_order() {
    let Some(case) = Case::new("order") else { return };
    for fixture in [Fixture::R, Fixture::RFiles] {
        case.compare(
            fixture,
            &[
                &["update-ref", "--create-reflog", "-m", "am", "AUTO_MERGE", "HEAD~1"],
                &["update-ref", "-m", "o", "refs/heads/side", "HEAD~1"],
            ],
            "R",
            &[&["rev-list", "--reflog"], &["rev-list", "--reflog", "--no-walk"]],
        );
    }
}
