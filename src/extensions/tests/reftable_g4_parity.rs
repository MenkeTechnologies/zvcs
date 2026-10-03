//! The verbs that lay out or move references wholesale — `worktree`, `init`,
//! `branch`, `remote`, `clone`, `fetch` — in a repository whose references are
//! stored in reftables, against stock git.
//!
//! Every case builds the same repository twice with stock git, runs one command
//! line on one copy with stock git and on the other with zvcs, and compares the
//! output, the exit code and what stock git then reads back from each copy:
//! `refs verify`, `for-each-ref --include-root-refs`, `HEAD`, every reflog, and
//! which files the git directory holds (a reftable store keeps only the
//! `HEAD`/`refs/heads` stubs as files).
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

impl Case {
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-reftable-g4-{tag}-{}", std::process::id()));
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
            .env("LC_ALL", "C")
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

    fn build(&self, side: &str, fixture: Fixture, setup: &[&[&str]]) -> PathBuf {
        let dir = self.root.join(side);
        std::fs::create_dir_all(&dir).unwrap();
        let format = match fixture {
            Fixture::RFiles => "--ref-format=files",
            _ => "--ref-format=reftable",
        };
        let mut steps = vec![
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
            assert_eq!(code, Some(0), "{args:?}: {err}");
        }
        dir
    }

    /// Stock git's view of every repository under `side`.
    fn state(&self, side: &Path) -> String {
        let mut out = String::new();
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(side).unwrap().map(|e| e.unwrap().path()).collect();
        dirs.sort();
        for dir in dirs {
            if !dir.join(".git").exists() && !dir.join("HEAD").is_file() {
                continue;
            }
            out += &format!("== {}\n", dir.file_name().unwrap().to_string_lossy());
            for args in [
                &["refs", "verify"][..],
                &["for-each-ref", "--include-root-refs"],
                &["symbolic-ref", "HEAD"],
                &["rev-parse", "HEAD"],
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
            if dir.join(".git").is_dir() {
                let mut files = Vec::new();
                list_files(&dir.join(".git"), &dir.join(".git"), &mut files);
                files.sort();
                out += &files.join("\n");
                out.push('\n');
            }
        }
        out.replace(side.to_str().unwrap(), "ROOT")
    }

    /// Run `args` in `cwd` of `fixture` with stock git and with zvcs, and assert
    /// both runs and both resulting repositories look the same to stock git.
    fn compare(&self, fixture: Fixture, cwd: &str, args: &[&str]) {
        self.compare_after(fixture, &[], cwd, args);
    }

    /// [`Case::compare`] after running each of `setup` with stock git in `R`.
    fn compare_after(&self, fixture: Fixture, setup: &[&[&str]], cwd: &str, args: &[&str]) {
        let stock_side = self.build("s", fixture, setup);
        let zvcs_side = self.build("z", fixture, setup);
        let expected = self.run(self.stock, &stock_side.join(cwd), args);
        let actual = self.run(BIN, &zvcs_side.join(cwd), args);
        assert_eq!(actual, expected, "{args:?}: output and exit code");
        assert_eq!(self.state(&zvcs_side), self.state(&stock_side), "{args:?}: state");
        std::fs::remove_dir_all(&stock_side).unwrap();
        std::fs::remove_dir_all(&zvcs_side).unwrap();
    }
}

/// The files of a git directory other than objects, hooks, the index and the
/// reftable tables (whose names are random), each with its first bytes (FETCH_HEAD whole); directories are left out, as the files backend leaves empty ones behind that stock removes and vice versa.
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
            list_files(base, &path, out);
        } else if name == "tables.list" {
            let lines = std::fs::read_to_string(&path).unwrap().lines().count();
            out.push(format!("{}: {lines} tables", path.strip_prefix(base).unwrap().display()));
        } else {
            let text = std::fs::read(&path).unwrap();
            let keep = if name == "FETCH_HEAD" { text.len() } else { text.len().min(120) };
            let text = String::from_utf8_lossy(&text[..keep]).replace('\n', "|");
            out.push(format!("{}: {text}", path.strip_prefix(base).unwrap().display()));
        }
    }
}

/// `worktree add` creates the new worktree's own reftable stack and stubs
/// (builtin/worktree.c:551-563) and writes `HEAD`, then the `reset --hard`
/// child's `ORIG_HEAD` and `HEAD` entries, through it — never a files-format
/// `HEAD`, `ORIG_HEAD` or `logs/HEAD`.
#[test]
fn worktree_add_writes_the_worktree_stack() {
    let Some(case) = Case::new("wt-add") else { return };
    for args in [
        &["worktree", "add", "../wt", "side"][..],
        &["worktree", "add", "-b", "nb", "../wt"],
        &["worktree", "add", "--detach", "../wt"],
        &["worktree", "add", "--orphan", "../wt"],
        &["worktree", "add", "--no-checkout", "../wt", "side"],
        &["worktree", "add", "../wt", "v1"],
    ] {
        case.compare(Fixture::R, "R", args);
    }
    // The files format keeps its `HEAD`, `ORIG_HEAD` and `logs/HEAD` files.
    case.compare(Fixture::RFiles, "R", &["worktree", "add", "../wt", "side"]);
}

/// Reinitializing applies `repository_format_configure()` (setup.c:2765-2838):
/// with no format on the command line or in the environment the configured or
/// compiled-in default replaces the recorded one, so `git init` turns a
/// reftable repository into a files one, and a configured `reftable` default
/// over a files repository writes the stubs until `refs/heads` — a directory
/// there — makes `write_file()` die.
#[test]
fn reinit_takes_the_default_ref_format() {
    let Some(case) = Case::new("reinit") else { return };
    case.compare(Fixture::R, "R", &["init"]);
    case.compare(Fixture::R, "R", &["-c", "init.defaultRefFormat=reftable", "init"]);
    case.compare(Fixture::RFiles, "R", &["-c", "init.defaultRefFormat=reftable", "init"]);
    case.compare(Fixture::Rw, "wt", &["init"]);
}

/// `branch -m`/`-c` go through the backend's `rename_ref()`/`copy_ref()`
/// (refs/reftable-backend.c:1773-2055), which carry the reflog across, and a
/// rename re-points every worktree `HEAD` on the branch through that
/// worktree's own stack (`replace_each_worktree_head_symref()`,
/// builtin/branch.c:579-603). `branch -a` marks a branch checked out in a
/// linked worktree with `+`, reading that worktree's `HEAD` from its stack.
#[test]
fn branch_rename_copy_and_worktree_marker() {
    let Some(case) = Case::new("branch") else { return };
    for args in [
        &["branch", "-m", "side", "s2"][..],
        &["branch", "-m", "main", "m2"],
        &["branch", "-M", "main", "main"],
        &["branch", "-c", "side", "s2"],
        &["branch", "-c", "main"],
        &["branch", "-C", "side", "main"],
        &["branch", "-m", "side", "main"],
    ] {
        case.compare(Fixture::R, "R", args);
    }
    // A symref source is refused by the backend with its own message.
    case.compare_after(Fixture::R, &[&["symbolic-ref", "refs/heads/sym", "refs/heads/side"]], "R", &[
        "branch", "-m", "sym", "s3",
    ]);
    case.compare(Fixture::Rw, "R", &["branch", "-m", "side", "s2"]);
    case.compare(Fixture::Rw, "wt", &["branch", "-m", "s2"]);
    case.compare(Fixture::Rw, "R", &["branch", "-a", "-v"]);
}

/// `remote remove` deletes the remote's refs in one `refs_delete_refs()`
/// transaction (builtin/remote.c:1070-1073), and `remote set-head` leaves the
/// symref's reflog to the backend, which writes it with the update.
#[test]
fn remote_remove_and_set_head() {
    let Some(case) = Case::new("remote") else { return };
    let setup: &[&[&str]] = &[
        &["remote", "add", "o", "."],
        &["fetch", "-q", "o"],
        &["remote", "set-head", "o", "main"],
    ];
    for fixture in [Fixture::R, Fixture::RFiles] {
        case.compare_after(fixture, setup, "R", &["remote", "remove", "o"]);
        case.compare_after(fixture, setup, "R", &["remote", "set-head", "o", "side"]);
    }
}

/// A `remote.<name>.fetch` value without a destination is not folded with an
/// identical one (`ref_remove_duplicates()`, remote.c:915-947, keeps every
/// mapping that has no `peer_ref`), so FETCH_HEAD gets its line once per
/// occurrence; values with a destination are still folded.
#[test]
fn fetch_head_keeps_repeated_refspecs_without_destination() {
    let Some(case) = Case::new("fetch-head") else { return };
    let setup: &[&[&str]] = &[
        &["remote", "add", "o", "."],
        // Keeps `refs/remotes/o/HEAD` out of it: only FETCH_HEAD is under test.
        &["config", "set", "remote.o.followRemoteHEAD", "never"],
        &["config", "set", "--append", "remote.o.fetch", "HEAD"],
        &["config", "set", "--append", "remote.o.fetch", "HEAD"],
        &["config", "set", "--append", "remote.o.fetch", "refs/heads/side"],
        &["config", "set", "--append", "remote.o.fetch", "refs/heads/side"],
        &["config", "set", "--append", "remote.o.fetch", "refs/heads/side:refs/x"],
        &["config", "set", "--append", "remote.o.fetch", "refs/heads/side:refs/x"],
    ];
    for fixture in [Fixture::R, Fixture::RFiles] {
        case.compare_after(fixture, setup, "R", &["fetch", "o"]);
    }
    case.compare(Fixture::RFiles, "R", &["fetch", ".", "HEAD", "HEAD", "side", "side"]);
}
