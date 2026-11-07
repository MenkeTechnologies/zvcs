//! `git stash push --staged` and `git stash apply --index` against stock git 2.56.
//!
//! * `-S` builds its patch from the whole index, `diff-tree -p -U1 HEAD <i_tree>`
//!   (`stash_staged()`, builtin/stash.c:1339-1365): a pathspec narrows neither the stash nor
//!   the `git apply -R` that takes the staged changes back out of the worktree, only the
//!   `git reset -q --refresh -- <pathspec>` that follows when `--keep-index` is not given
//!   (builtin/stash.c:1876-1893). Without a pathspec that reset is a mixed reset to `HEAD`,
//!   which logs `reset: moving to HEAD` and moves `ORIG_HEAD`.
//! * `--index` replays the stash's staged changes with `git apply --cached` of
//!   `diff-tree --binary <w>^2^..<w>^2` (builtin/stash.c:426-453, :670-688): three lines of
//!   context, placed hunk by hunk, so a refusal names the hunk `git apply` could not place
//!   and a path the index already has is `already exists in index`.
//! * `create` asks `check_changes_tracked_files()` a second time after its refresh, under
//!   `ignore_submodules` (builtin/stash.c:1241-1280, :1540-1543): a submodule at another
//!   commit is nothing to stash.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git_at_least;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// One fixture directory per binary; stock git builds it, the binary under test runs the case.
struct Repo {
    root: PathBuf,
    work: PathBuf,
    stock: String,
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
    fn new(tag: &str, label: &str, stock: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stash-staged-index-{tag}-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("work")).unwrap();
        let root = root.canonicalize().unwrap();
        let work = root.join("work");
        let repo = Repo { root, work, stock: stock.to_owned() };
        repo.git(&["init", "-q", "-b", "main", "."]);
        repo
    }

    fn exec(&self, bin: &str, dir: &Path, args: &[&str], input: Option<&str>) -> Ran {
        let mut child = Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("ZVCS_HOME", &self.root)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(text) = input {
            use std::io::Write as _;
            child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let root = self.root.to_string_lossy().into_owned();
        Ran {
            stdout: String::from_utf8_lossy(&out.stdout).replace(&root, "<root>"),
            stderr: String::from_utf8_lossy(&out.stderr).replace(&root, "<root>"),
            code: out.status.code(),
        }
    }

    /// Stock git, for building the fixture and reading the result back.
    fn git(&self, args: &[&str]) -> String {
        let r = self.exec(&self.stock, &self.work, args, None);
        assert_eq!(r.code, Some(0), "stock git {args:?}: {}", r.stderr);
        r.stdout
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.work.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// Refs, `HEAD`'s reflog, `ORIG_HEAD`, the index and the worktree as stock reads them.
    fn state(&self) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        let mut stack = vec![self.work.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let rel = path.strip_prefix(&self.work).unwrap().display().to_string();
                    files.push(format!("{rel}: {}", std::fs::read_to_string(&path).unwrap_or_default()));
                }
            }
        }
        files.sort();
        let orig_head = std::fs::read_to_string(self.work.join(".git/ORIG_HEAD")).unwrap_or_default();
        vec![
            self.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            self.exec(&self.stock, &self.work, &["log", "-g", "--format=%H %gs", "HEAD"], None).stdout,
            orig_head,
            self.git(&["status", "--porcelain=v2", "--branch", "-uall"]),
            self.git(&["ls-files", "-s"]),
            files.join(""),
        ]
    }
}

/// Build the fixture once per binary with stock git, run `case` under each, and compare
/// what the case printed and what it left behind.
fn compare(tag: &str, setup: impl Fn(&Repo), case: impl Fn(&Repo, &str) -> Vec<Ran>) {
    let Some(stock) = stock_git_at_least((2, 56, 0)) else { return };
    let run = |label: &str, bin: &str| {
        let repo = Repo::new(tag, label, stock);
        setup(&repo);
        let ran = case(&repo, bin);
        (ran, repo.state())
    };
    assert_eq!(run("zvcs", BIN), run("stock", stock), "{tag}");
}

/// `main`: `a`, `b`, `dir/c`, `dir/sub/d` and `.gitignore`, then `a` and `b` edited in two
/// more commits; `side` branches off `main~1` and rewrites `b` and adds `s`; `v1` tags `main~1`.
fn base(r: &Repo) {
    r.write("a", "a1\na2\na3\na4\na5\n");
    r.write("b", "b1\n");
    r.write("dir/c", "c\n");
    r.write("dir/sub/d", "d\n");
    r.write(".gitignore", "*.log\n");
    r.git(&["add", "-A"]);
    r.git(&["commit", "-q", "-m", "one"]);
    r.write("a", "a1\na2\na3\na4\na5\na6\n");
    r.git(&["commit", "-q", "-am", "two"]);
    r.git(&["branch", "side"]);
    r.write("b", "b1\nb2\n");
    r.git(&["commit", "-q", "-am", "three"]);
    r.git(&["checkout", "-q", "side"]);
    r.write("b", "b-side\n");
    r.write("s", "s\n");
    r.git(&["add", "b", "s"]);
    r.git(&["commit", "-q", "-m", "side1"]);
    r.git(&["checkout", "-q", "main"]);
    r.git(&["tag", "v1", "HEAD~1"]);
}

fn stash(r: &Repo, bin: &str, args: &[&str]) -> Vec<Ran> {
    let mut argv = vec!["stash"];
    argv.extend_from_slice(args);
    vec![
        r.exec(bin, &r.work, &argv, None),
        r.exec(bin, &r.work, &["stash", "list"], None),
    ]
}

/// `b` edited and staged, `n` added; nothing unstaged.
fn staged(r: &Repo) {
    base(r);
    r.write("n", "new\n");
    r.write("b", "b1\nb2\nst\n");
    r.git(&["add", "n", "b"]);
}

#[test]
fn staged_push_ends_on_a_mixed_reset_that_logs_and_moves_orig_head() {
    compare("s-plain", staged, |r, bin| stash(r, bin, &["push", "--staged"]));
    compare("s-quiet", staged, |r, bin| stash(r, bin, &["push", "-S", "-q"]));
    // `--keep-index` skips the reset, and with it the reflog line and `ORIG_HEAD`.
    compare("s-keep", staged, |r, bin| stash(r, bin, &["push", "-S", "-k"]));
}

#[test]
fn staged_push_with_a_pathspec_reverses_every_staged_path_and_resets_only_the_named_ones() {
    // `n` leaves the worktree with the reversed patch but stays staged: `AD`.
    compare("s-ps", staged, |r, bin| stash(r, bin, &["push", "--staged", "--", "b"]));
    compare("s-ps-n", staged, |r, bin| stash(r, bin, &["push", "-S", "--", "n"]));
}

#[test]
fn staged_push_naming_only_an_unstaged_path_still_stashes_the_whole_index() {
    let dirty = |r: &Repo| {
        staged(r);
        r.write("a", "A1\na2\na3\na4\na5\na6\n");
        r.write("b", "b1\nb2\nst\nwt\n");
    };
    // Not `No staged changes`: the patch is the whole index, and its reverse no longer
    // applies to `b`, so the stash is stored and the worktree is left alone.
    compare("s-unstaged", dirty, |r, bin| stash(r, bin, &["push", "-S", "--", "a"]));
}

#[test]
fn apply_index_refusals_are_git_applys_own() {
    // The stash stages `a` with a line at each end, deletes `dir/c` and adds `n`; the index
    // then edits the middle of `a`, rewrites `dir/c` and stages another `n`. A three-way
    // merge would take `a` cleanly; the patch's context does not match.
    let setup = |r: &Repo| {
        base(r);
        r.write("n", "n1\n");
        r.write("a", "a0\na1\na2\na3\na4\na5\na6\nX\n");
        r.git(&["add", "n", "a"]);
        r.git(&["rm", "-q", "dir/c"]);
        r.git(&["stash", "-q"]);
        r.write("n", "n2\n");
        r.write("dir/c", "cc\n");
        r.write("a", "a1\na2\nQ\na4\na5\na6\n");
        r.git(&["add", "n", "dir/c", "a"]);
    };
    compare("apply-refused", setup, |r, bin| stash(r, bin, &["apply", "--index"]));
    compare("pop-refused", setup, |r, bin| stash(r, bin, &["pop", "--index"]));
}

#[test]
fn apply_index_names_the_hunk_with_three_lines_of_context() {
    let setup = |r: &Repo| {
        base(r);
        r.write("b", "b1\nb2\nbi\n");
        r.git(&["add", "b"]);
        r.write("a", "a1\na2\na3\na4\na5\na6\naw\n");
        r.git(&["stash", "-q"]);
        r.write("b", "b1\nb2\nzz\n");
        r.git(&["add", "b"]);
    };
    compare("hunk-line", setup, |r, bin| stash(r, bin, &["apply", "--index"]));
    compare("hunk-branch", setup, |r, bin| stash(r, bin, &["branch", "nb"]));
    // A patch that applies is staged as `git apply --cached` stages it.
    let clean = |r: &Repo| {
        base(r);
        r.write("b", "b1\nb2\nbi\n");
        r.git(&["add", "b"]);
        r.git(&["stash", "-q"]);
        r.write("a", "a1\na2\na3\na4\na5\na6\nmore\n");
        r.git(&["add", "a"]);
    };
    compare("applies", clean, |r, bin| stash(r, bin, &["apply", "--index"]));
}

#[test]
fn create_ignores_a_submodule_at_another_commit() {
    let setup = |r: &Repo| {
        base(r);
        let sub = r.root.join("subsrc");
        std::fs::create_dir_all(&sub).unwrap();
        for args in [&["init", "-q", "-b", "main", "."][..], &["commit", "-q", "--allow-empty", "-m", "s1"], &["commit", "-q", "--allow-empty", "-m", "s2"]] {
            let ran = r.exec(&r.stock, &sub, args, None);
            assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        }
        // Relative to the superproject, so `.gitmodules` is the same in both fixtures.
        r.git(&["-c", "protocol.file.allow=always", "submodule", "-q", "add", "../subsrc", "sm"]);
        r.git(&["commit", "-q", "-m", "addsub"]);
        let ran = r.exec(&r.stock, &r.work.join("sm"), &["checkout", "-q", "HEAD~1"], None);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        r.write("sm/new", "untracked\n");
        // Stat data that moved without the content: the unrefreshed first check sees a
        // change, the refreshed second one only the submodule it ignores.
        let file = std::fs::File::options().write(true).open(r.work.join("a")).unwrap();
        file.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000)).unwrap();
    };
    compare("create-sub", setup, |r, bin| {
        vec![
            r.exec(bin, &r.work, &["stash", "create", "msg"], None),
            r.exec(bin, &r.work, &["stash", "create"], None),
        ]
    });
}
