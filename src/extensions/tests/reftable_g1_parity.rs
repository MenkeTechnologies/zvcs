//! The ref plumbing — `update-ref`, `symbolic-ref`, `for-each-ref`, `show-ref`,
//! `pack-refs` and `refs` — in a repository whose references are stored in
//! reftables, against stock git.
//!
//! Every case builds the same repository twice with stock git, runs one
//! sequence of command lines on one copy with stock git and on the other with
//! zvcs, and compares each command's output and exit code and what stock git
//! then reads back from each copy: `refs verify`, `fsck`,
//! `for-each-ref --include-root-refs`, every reflog, the files of the git
//! directory, and the bytes of every table, which a write through the backend
//! leaves identical to stock's (peeled tag values and log records included).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
}

/// One command line and what it reads on stdin.
struct Step<'a> {
    args: &'a [&'a str],
    stdin: &'a str,
}

/// A command line reading nothing.
const fn cmd<'a>(args: &'a [&'a str]) -> Step<'a> {
    Step { args, stdin: "" }
}

/// A command line fed `stdin`.
const fn fed<'a>(args: &'a [&'a str], stdin: &'a str) -> Step<'a> {
    Step { args, stdin }
}

impl Case {
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-reftable-g1-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Some(Case { root, stock })
    }

    fn run(&self, bin: &str, dir: &Path, step: &Step<'_>) -> (String, String, Option<i32>) {
        let mut child = Command::new(bin)
            .args(step.args)
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
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(step.stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let side = dir.ancestors().find(|p| p.parent() == Some(self.root.as_path())).unwrap().to_owned();
        let norm = |b: &[u8]| String::from_utf8_lossy(b).replace(side.to_str().unwrap(), "ROOT");
        (norm(&out.stdout), norm(&out.stderr), out.status.code())
    }

    fn stock_ok(&self, dir: &Path, args: &[&str]) {
        let (_, err, code) = self.run(self.stock, dir, &cmd(args));
        assert_eq!(code, Some(0), "{args:?}: {err}");
    }

    fn build(&self, side: &str, fixture: Fixture, setup: &[&[&str]]) -> PathBuf {
        let dir = self.root.join(side);
        std::fs::create_dir_all(&dir).unwrap();
        let mut steps: Vec<&[&str]> = vec![
            &["init", "-q", "-b", "main", "--ref-format=reftable", "R"],
            &["-C", "R", "commit", "-q", "--allow-empty", "-m", "one"],
            &["-C", "R", "commit", "-q", "--allow-empty", "-m", "two"],
            &["-C", "R", "branch", "side"],
            &["-C", "R", "tag", "-a", "v1", "-m", "t"],
        ];
        if let Fixture::Rw = fixture {
            steps.push(&["-C", "R", "worktree", "add", "-q", "../wt", "side"]);
        }
        for args in steps {
            self.stock_ok(&dir, args);
        }
        for args in setup {
            self.stock_ok(&dir.join("R"), args);
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
                let (o, e, code) = self.run(self.stock, &dir, &cmd(args));
                out += &format!("$ {args:?} {code:?}\n{o}{e}");
            }
            let (names, _, _) = self.run(self.stock, &dir, &cmd(&["reflog", "list"]));
            for name in names.lines() {
                let (o, e, _) = self.run(
                    self.stock,
                    &dir,
                    &cmd(&["log", "-g", "--date=raw", "--format=%H %gd %gs %gn", name, "--"]),
                );
                out += &format!("-- {name}\n{o}{e}");
            }
        }
        let git_dir = side.join("R/.git");
        let mut files = Vec::new();
        list_files(&git_dir, &git_dir, &mut files);
        files.sort();
        out += &files.join("\n");
        out.push('\n');
        // A worktree's `gitdir` file names the side by its real path.
        let real = std::fs::canonicalize(side).unwrap();
        out.replace(real.to_str().unwrap(), "ROOT").replace(side.to_str().unwrap(), "ROOT")
    }

    /// Run `steps` in `cwd` of `fixture`, after `setup` (stock git on both
    /// copies, in `R`), with stock git and with zvcs, and assert every command
    /// and both resulting repositories look the same to stock git.
    fn compare(&self, fixture: Fixture, setup: &[&[&str]], cwd: &str, steps: &[Step<'_>]) {
        let stock_side = self.build("s", fixture, setup);
        let zvcs_side = self.build("z", fixture, setup);
        for step in steps {
            let expected = self.run(self.stock, &stock_side.join(cwd), step);
            let actual = self.run(BIN, &zvcs_side.join(cwd), step);
            assert_eq!(actual, expected, "{:?} <<< {:?}: output and exit code", step.args, step.stdin);
        }
        assert_eq!(self.state(&zvcs_side), self.state(&stock_side), "state");
        std::fs::remove_dir_all(&stock_side).unwrap();
        std::fs::remove_dir_all(&zvcs_side).unwrap();
    }
}

/// The files of a git directory other than objects, hooks and the index, each
/// with its first bytes; a table (whose name is random) by a hash of its bytes.
fn list_files(base: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if ["objects", "hooks", "index", "description", "COMMIT_EDITMSG", "exclude"].contains(&name.as_str()) {
            continue;
        }
        let rel = path.strip_prefix(base).unwrap().display().to_string();
        if path.is_dir() {
            out.push(format!("{rel}/"));
            list_files(base, &path, out);
        } else if name.ends_with(".ref") {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::fs::read(&path).unwrap().hash(&mut hasher);
            let parent = path.parent().unwrap().strip_prefix(base).unwrap().display();
            out.push(format!("{parent}/<table {:016x}>", hasher.finish()));
        } else if name == "tables.list" {
            let lines = std::fs::read_to_string(&path).unwrap().lines().count();
            out.push(format!("{rel}: {lines} tables"));
        } else {
            let text = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&text[..text.len().min(200)]).replace('\n', "|");
            out.push(format!("{rel}: {text}"));
        }
    }
}

/// The command-line forms run one transaction of the backend
/// (`refs_update_ref()`, `refs_delete_ref()`): its own old-value check and
/// wording, the `HEAD` log entry for its branch, an annotated tag stored with
/// its peeled value, and a one-level name stored as a record of its own.
#[test]
fn update_ref_command_line_writes_through_the_stack() {
    let Some(case) = Case::new("cmdline") else { return };
    case.compare(
        Fixture::R,
        &[],
        "R",
        &[
            cmd(&["update-ref", "refs/heads/x", "HEAD"]),
            cmd(&["update-ref", "-m", "move", "refs/heads/main", "HEAD~"]),
            cmd(&["update-ref", "refs/heads/x", "HEAD", "v1"]),
            cmd(&["update-ref", "-d", "refs/heads/nope", "HEAD"]),
            cmd(&["update-ref", "-d", "refs/heads/side"]),
            cmd(&["update-ref", "refs/tags/t2", "v1"]),
            cmd(&["update-ref", "top", "main"]),
            cmd(&["update-ref", "refs/heads/x/y", "HEAD"]),
        ],
    );
}

/// `--stdin` hands its commands to one transaction, `verify` without a new
/// value and the `symref-*` commands with targets, and the explicit `prepare`
/// takes the stack's lock as git does.
#[test]
fn update_ref_stdin_runs_one_transaction() {
    let Some(case) = Case::new("stdin") else { return };
    case.compare(
        Fixture::R,
        &[],
        "R",
        &[
            fed(&["update-ref", "--stdin"], "create refs/heads/a HEAD\nupdate refs/heads/side HEAD~\ndelete refs/heads/a~0\n"),
            fed(&["update-ref", "--stdin"], "start\ncreate refs/heads/a HEAD\nprepare\ncommit\n"),
            fed(&["update-ref", "--stdin"], "verify refs/heads/main HEAD\nverify refs/heads/nope\nverify HEAD HEAD\n"),
            fed(&["update-ref", "--stdin"], "verify refs/heads/main HEAD~\n"),
            fed(&["update-ref", "--stdin"], "create refs/heads/side HEAD\n"),
            fed(&["update-ref", "--stdin"], "symref-create refs/heads/s refs/heads/main\nsymref-update HEAD refs/heads/side\n"),
            fed(
                &["update-ref", "--stdin"],
                "option no-deref\nsymref-verify HEAD refs/heads/side\noption no-deref\nsymref-update HEAD refs/heads/main ref refs/heads/side\n",
            ),
            fed(&["update-ref", "-m", "via stdin", "--stdin"], "update HEAD HEAD~\n"),
            fed(&["update-ref", "--stdin"], "create refs/heads/q/x HEAD\ncreate refs/heads/q HEAD\n"),
        ],
    );
}

/// `--batch-updates` lets single updates fail (`REF_TRANSACTION_ALLOW_FAILURE`)
/// and reports each with its reason. The availability check searches
/// `transaction->refnames` by bisection after a split appended to it unsorted,
/// so `refs/heads/n` is not found to conflict with `refs/heads/n/m` here, while
/// `refs/heads/n/m` is.
#[test]
fn batch_updates_reject_single_updates_like_git() {
    let Some(case) = Case::new("batch") else { return };
    case.compare(
        Fixture::R,
        &[],
        "R",
        &[
            fed(
                &["update-ref", "--stdin", "--batch-updates"],
                "create refs/heads/side HEAD\ncreate refs/heads/n HEAD\nupdate refs/heads/main HEAD~ HEAD~\n\
                 create refs/heads/main/x HEAD\nverify refs/heads/zz HEAD\nverify refs/tags/v1\n\
                 create refs/heads/n/m HEAD\nupdate HEAD HEAD~\n",
            ),
            fed(
                &["update-ref", "--stdin", "--batch-updates"],
                "create refs/heads/k HEAD\ncreate refs/heads/k/l HEAD\nupdate refs/heads/side HEAD~ HEAD\n",
            ),
        ],
    );
}

/// `symbolic-ref` writes and deletes through the backend, which logs the
/// symref and the `HEAD` entry a change of its branch implies, and reads no
/// `core.preferSymlinkRefs`.
#[test]
fn symbolic_ref_writes_through_the_stack() {
    let Some(case) = Case::new("symref") else { return };
    case.compare(
        Fixture::R,
        &[],
        "R",
        &[
            cmd(&["-c", "core.preferSymlinkRefs=bogus", "symbolic-ref", "HEAD"]),
            cmd(&["symbolic-ref", "-m", "why", "HEAD", "refs/heads/side"]),
            cmd(&["symbolic-ref", "refs/heads/s", "refs/heads/main"]),
            cmd(&["symbolic-ref", "refs/heads/side", "refs/heads/nope"]),
            cmd(&["symbolic-ref", "-d", "refs/heads/s"]),
            cmd(&["symbolic-ref", "--short", "HEAD"]),
        ],
    );
}

/// `pack-refs` and `refs optimize` compact the stack (`reftable_be_optimize()`),
/// geometrically with `--auto`.
#[test]
fn pack_refs_compacts_the_stack() {
    let Some(case) = Case::new("pack") else { return };
    let setup: &[&[&str]] = &[
        &["update-ref", "refs/heads/b1", "HEAD"],
        &["update-ref", "refs/heads/b2", "HEAD"],
        &["update-ref", "refs/heads/b3", "HEAD"],
    ];
    case.compare(Fixture::R, setup, "R", &[cmd(&["pack-refs", "--auto"])]);
    case.compare(Fixture::R, setup, "R", &[cmd(&["pack-refs", "--all"])]);
    case.compare(Fixture::R, setup, "R", &[cmd(&["refs", "optimize"])]);
}

/// `for-each-ref --include-root-refs` lists the root records of the stack, not
/// the `HEAD` stub in the git directory, and no reference is `packed`.
#[test]
fn for_each_ref_reads_root_records() {
    let Some(case) = Case::new("feref") else { return };
    case.compare(
        Fixture::R,
        &[&["update-ref", "ORIG_HEAD", "HEAD~"], &["update-ref", "AUTO_MERGE", "HEAD"]],
        "R",
        &[
            cmd(&["for-each-ref", "--include-root-refs"]),
            cmd(&["for-each-ref", "--include-root-refs", "--format=%(refname) %(symref) %(flag)"]),
            cmd(&["refs", "list", "--format=%(refname) %(flag)"]),
        ],
    );
}

/// `show-ref --exists` and `refs exists` read the record of exactly that name.
#[test]
fn exists_reads_the_record() {
    let Some(case) = Case::new("exists") else { return };
    case.compare(
        Fixture::R,
        &[],
        "R",
        &[
            cmd(&["show-ref", "--exists", "HEAD"]),
            cmd(&["show-ref", "--exists", "refs/heads/side"]),
            cmd(&["show-ref", "--exists", "refs/heads/nope"]),
            cmd(&["refs", "exists", "refs/tags/v1"]),
            cmd(&["refs", "exists", "side"]),
        ],
    );
}

/// In a linked worktree the per-worktree names go to its own stack, and
/// `pack-refs` compacts that one.
#[test]
fn worktree_writes_go_to_their_stacks() {
    let Some(case) = Case::new("worktree") else { return };
    case.compare(
        Fixture::Rw,
        &[],
        "wt",
        &[
            cmd(&["update-ref", "HEAD", "HEAD~"]),
            cmd(&["update-ref", "refs/bisect/x", "HEAD"]),
            cmd(&["update-ref", "main-worktree/HEAD", "HEAD"]),
            cmd(&["symbolic-ref", "HEAD", "refs/heads/main"]),
            cmd(&["for-each-ref", "--include-root-refs"]),
            cmd(&["pack-refs"]),
            cmd(&["refs", "migrate", "--ref-format=files"]),
        ],
    );
}

/// `refs migrate` in both directions (`repo_migrate_ref_storage_format()`):
/// into files every plain reference lands in `packed-refs`, symrefs and root
/// refs as loose files and the reflogs under `logs/`, and back again.
#[test]
fn refs_migrate_goes_both_ways() {
    let Some(case) = Case::new("migrate") else { return };
    let setup: &[&[&str]] =
        &[&["update-ref", "ORIG_HEAD", "HEAD~"], &["symbolic-ref", "refs/heads/sym", "refs/heads/main"]];
    case.compare(Fixture::R, setup, "R", &[cmd(&["refs", "migrate", "--ref-format=files"])]);
    case.compare(Fixture::R, setup, "R", &[cmd(&["refs", "migrate", "--ref-format=files", "--no-reflog"])]);
    case.compare(
        Fixture::R,
        setup,
        "R",
        &[
            cmd(&["refs", "migrate", "--ref-format=files"]),
            cmd(&["refs", "migrate", "--ref-format=reftable"]),
        ],
    );
}

/// A reftable repository whose configuration lost `extensions.refStorage` is
/// read by the files backend, where its stubs are broken references: `HEAD`
/// names the invalid `refs/heads/.invalid`, a dangling symref that iteration
/// omits without a word, and the `refs/heads` file is warned about.
#[test]
fn stubs_read_as_broken_refs_without_ref_storage() {
    let Some(case) = Case::new("stubs") else { return };
    case.compare(
        Fixture::R,
        &[&["config", "--unset", "extensions.refStorage"]],
        "R",
        &[
            cmd(&["for-each-ref"]),
            cmd(&["for-each-ref", "--include-root-refs"]),
            cmd(&["symbolic-ref", "HEAD"]),
            cmd(&["symbolic-ref", "--no-recurse", "HEAD"]),
            cmd(&["show-ref"]),
        ],
    );
}
