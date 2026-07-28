//! `stash push` beyond the bare form: pathspec limiting, `--keep-index`,
//! `--staged`, and the untracked/ignored captures.
//!
//! Every expectation was taken from stock git 2.55.0 against this same fixture
//! and is hardcoded, so the suite needs no `git` on the machine running it.
//!
//! The shape being pinned, which is easy to get subtly wrong:
//!
//! * The index commit `I` is **never** pathspec-limited. git captures the whole
//!   index into it, so a staged change to an unmatched path rides along in the
//!   stash *and* stays staged in the worktree afterwards. Only the worktree tree
//!   `W` and the set of paths that get reset are narrowed.
//! * `--keep-index` resets to `I` rather than `HEAD`, which is what leaves the
//!   staged content both staged and on disk.
//! * `-u`/`-a` put the untracked files in a parentless third parent (`^3`) and
//!   delete them from the worktree; `-a` additionally takes ignored files.
//!
//! Unix-only: the fixture asserts on file removal and mode-free paths.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A repo with, deliberately, one staged path and one unstaged path on each
/// side of the pathspecs used below — the only way the "is `I` limited?"
/// question can actually be answered by an assertion.
struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stashopt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };

        for p in ["a.txt", "b.txt", "sub/s.txt", "sub/t.txt"] {
            std::fs::write(f.work.join(p), format!("{p} base\n")).unwrap();
        }
        std::fs::write(f.work.join(".gitignore"), "*.log\n").unwrap();
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);

        // staged: a.txt, sub/s.txt   unstaged: b.txt, sub/t.txt
        std::fs::write(f.work.join("a.txt"), "a STAGED\n").unwrap();
        std::fs::write(f.work.join("sub/s.txt"), "s STAGED\n").unwrap();
        f.git(&["add", "a.txt", "sub/s.txt"]);
        std::fs::write(f.work.join("b.txt"), "b CHANGED\n").unwrap();
        std::fs::write(f.work.join("sub/t.txt"), "t CHANGED\n").unwrap();
        // untracked + ignored
        std::fs::write(f.work.join("new.txt"), "untracked\n").unwrap();
        std::fs::write(f.work.join("sub/newsub.txt"), "untracked\n").unwrap();
        std::fs::write(f.work.join("ign.log"), "ignored\n").unwrap();
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn lines(&self, args: &[&str]) -> Vec<String> {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    }

    /// Porcelain status as `("XY", "path")` pairs, ignoring zvcs's own database
    /// files — it writes those into the worktree, where they read as untracked.
    fn status(&self) -> Vec<(String, String)> {
        self.lines(&["status", "--porcelain"])
            .into_iter()
            .filter(|l| !l.contains(".zvcs/"))
            .map(|l| (l[..2].to_string(), l[3..].to_string()))
            .collect()
    }

    fn tree_diff(&self, from: &str, to: &str) -> Vec<String> {
        self.lines(&["diff", "--name-only", from, to])
    }

    fn exists(&self, p: &str) -> bool {
        self.work.join(p).exists()
    }
}

/// A pathspec narrows the worktree tree and the reset, but never the index
/// commit — the staged change to the unmatched path is still in `I`, and is
/// still staged on disk afterwards.
#[test]
fn pathspec_limits_the_reset_but_not_the_index_commit() {
    let f = Fixture::new("pathspec");
    let (ok, out, err) = f.run(&["stash", "push", "-m", "t", "--", "a.txt"]);
    assert!(ok, "stash failed: {out}{err}");

    assert_eq!(f.tree_diff("stash@{0}^", "stash@{0}"), ["a.txt", "sub/s.txt"]);
    assert_eq!(f.tree_diff("stash@{0}^", "stash@{0}^2"), ["a.txt", "sub/s.txt"]);

    let status = f.status();
    // a.txt was matched, so it is gone from the status entirely.
    assert!(!status.iter().any(|(_, p)| p == "a.txt"), "a.txt should be reset: {status:?}");
    // sub/s.txt was NOT matched: still staged, not merely dirty.
    assert!(
        status.contains(&("M ".to_string(), "sub/s.txt".to_string())),
        "unmatched staged path must stay staged: {status:?}"
    );
    assert!(status.contains(&(" M".to_string(), "b.txt".to_string())));
    assert!(status.contains(&(" M".to_string(), "sub/t.txt".to_string())));
}

/// Pathspec magic reaches stash too — `:(glob)` stops at a slash here exactly as
/// it does for `ls-files`.
#[test]
fn pathspec_magic_is_honored() {
    let f = Fixture::new("globspec");
    let (ok, _, err) = f.run(&["stash", "push", "-m", "t", "--", ":(glob)sub/*.txt"]);
    assert!(ok, "stash failed: {err}");

    assert_eq!(f.tree_diff("stash@{0}^", "stash@{0}"), ["a.txt", "sub/s.txt", "sub/t.txt"]);
    let status = f.status();
    assert!(
        status.contains(&("M ".to_string(), "a.txt".to_string())),
        "a.txt is outside sub/ and stays staged: {status:?}"
    );
    // Tracked paths under sub/ were reset. The untracked one stays: no `-u`.
    assert!(
        !status.iter().any(|(x, p)| p.starts_with("sub/") && x != "??"),
        "tracked sub/ paths should be reset: {status:?}"
    );
    assert!(status.contains(&("??".to_string(), "sub/newsub.txt".to_string())), "{status:?}");
}

/// A pathspec matching nothing tracked is refused before anything is written.
#[test]
fn pathspec_matching_nothing_is_an_error() {
    let f = Fixture::new("nomatch");
    let before = f.status();
    let (ok, _, err) = f.run(&["stash", "push", "-m", "t", "--", "nosuch.txt"]);
    assert!(!ok, "expected failure");
    assert!(
        err.contains("did not match any file(s) known to git"),
        "unexpected message: {err}"
    );
    assert_eq!(f.status(), before, "a refused stash must not touch the worktree");
    assert!(f.run(&["stash", "list"]).1.is_empty(), "no entry may be created");
}

/// A pathspec that matches only clean paths is not an error — there is simply
/// nothing to save.
#[test]
fn pathspec_matching_only_clean_paths_saves_nothing() {
    let f = Fixture::new("cleanpath");
    let before = f.status();
    let (ok, out, _) = f.run(&["stash", "push", "-m", "t", "--", ".gitignore"]);
    assert!(ok);
    assert!(out.contains("No local changes to save"), "unexpected output: {out}");
    assert_eq!(f.status(), before);
    assert!(f.run(&["stash", "list"]).1.is_empty());
}

/// `--keep-index` resets the worktree but leaves the staged state staged, with
/// the staged content still on disk.
#[test]
fn keep_index_leaves_the_index_staged() {
    let f = Fixture::new("keepindex");
    let (ok, _, err) = f.run(&["stash", "push", "-k", "-m", "t"]);
    assert!(ok, "stash failed: {err}");

    let status = f.status();
    let staged: BTreeSet<&str> =
        status.iter().filter(|(x, _)| x == "M ").map(|(_, p)| p.as_str()).collect();
    assert_eq!(staged, BTreeSet::from(["a.txt", "sub/s.txt"]), "status: {status:?}");
    // The staged content is what remains on disk, not HEAD's.
    assert_eq!(std::fs::read_to_string(f.work.join("a.txt")).unwrap(), "a STAGED\n");
    // Unstaged edits were still stashed away.
    assert_eq!(std::fs::read_to_string(f.work.join("b.txt")).unwrap(), "b.txt base\n");
}

/// `--staged` takes the index diff alone and leaves unstaged work untouched.
#[test]
fn staged_only_leaves_unstaged_work_alone() {
    let f = Fixture::new("stagedonly");
    let (ok, _, err) = f.run(&["stash", "push", "-S", "-m", "t"]);
    assert!(ok, "stash failed: {err}");

    assert_eq!(f.tree_diff("stash@{0}^", "stash@{0}"), ["a.txt", "sub/s.txt"]);
    let status = f.status();
    assert!(!status.iter().any(|(x, _)| x == "M "), "nothing staged remains: {status:?}");
    assert!(status.contains(&(" M".to_string(), "b.txt".to_string())));
    assert!(status.contains(&(" M".to_string(), "sub/t.txt".to_string())));
    assert_eq!(std::fs::read_to_string(f.work.join("a.txt")).unwrap(), "a.txt base\n");
}

/// `-u` captures untracked files into the third parent and removes them from
/// the worktree; an ignored file is left alone.
#[test]
fn include_untracked_captures_and_removes_them() {
    let f = Fixture::new("untracked");
    let (ok, _, err) = f.run(&["stash", "push", "-u", "-m", "t"]);
    assert!(ok, "stash failed: {err}");

    let u: BTreeSet<String> =
        f.lines(&["ls-tree", "-r", "--name-only", "stash@{0}^3"]).into_iter().collect();
    assert!(u.contains("new.txt"), "third parent: {u:?}");
    assert!(u.contains("sub/newsub.txt"), "third parent: {u:?}");
    assert!(!u.contains("ign.log"), "-u must not take ignored files: {u:?}");

    assert!(!f.exists("new.txt"), "untracked file should be removed");
    assert!(!f.exists("sub/newsub.txt"), "untracked file should be removed");
    assert!(f.exists("ign.log"), "ignored file must survive -u");
}

/// `-a` additionally takes ignored files.
#[test]
fn all_also_captures_ignored_files() {
    let f = Fixture::new("all");
    let (ok, _, err) = f.run(&["stash", "push", "-a", "-m", "t"]);
    assert!(ok, "stash failed: {err}");

    let u: BTreeSet<String> =
        f.lines(&["ls-tree", "-r", "--name-only", "stash@{0}^3"]).into_iter().collect();
    assert!(u.contains("ign.log"), "-a must take ignored files: {u:?}");
    assert!(!f.exists("ign.log"), "ignored file should be removed by -a");
}

/// The half that makes `-u` safe: `pop` restores the captured files, and they
/// come back untracked. Without this, `-u` would delete a file and leave it
/// reachable only by digging the third parent out by hand.
#[test]
fn pop_restores_untracked_files() {
    let f = Fixture::new("untrackedroundtrip");
    f.run(&["stash", "push", "-u", "-m", "t"]);
    assert!(!f.exists("new.txt"));

    let (ok, _, err) = f.run(&["stash", "pop"]);
    assert!(ok, "pop failed: {err}");
    assert_eq!(std::fs::read_to_string(f.work.join("new.txt")).unwrap(), "untracked\n");
    assert_eq!(std::fs::read_to_string(f.work.join("sub/newsub.txt")).unwrap(), "untracked\n");

    let status = f.status();
    assert!(
        status.contains(&("??".to_string(), "new.txt".to_string())),
        "restored file must still be untracked: {status:?}"
    );
}

/// A file sitting where a captured one would land stops the restore before
/// anything is written, rather than silently overwriting it.
#[test]
fn pop_refuses_to_clobber_an_existing_untracked_file() {
    let f = Fixture::new("clobber");
    f.run(&["stash", "push", "-u", "-m", "t"]);
    std::fs::write(f.work.join("new.txt"), "SOMETHING ELSE\n").unwrap();

    let (ok, _, err) = f.run(&["stash", "pop"]);
    assert!(!ok, "expected refusal");
    assert!(err.contains("could not restore untracked file"), "unexpected message: {err}");
    assert_eq!(
        std::fs::read_to_string(f.work.join("new.txt")).unwrap(),
        "SOMETHING ELSE\n",
        "the file in the way must be left untouched"
    );
    assert!(!f.run(&["stash", "list"]).1.is_empty(), "a refused pop keeps the entry");
}

/// git refuses the combination rather than picking a winner.
#[test]
fn staged_with_untracked_is_refused() {
    let f = Fixture::new("conflict");
    let before = f.status();
    let (ok, _, err) = f.run(&["stash", "push", "-S", "-u", "-m", "t"]);
    assert!(!ok, "expected failure");
    assert!(
        err.contains("Can't use --staged and --include-untracked or --all at the same time"),
        "unexpected message: {err}"
    );
    assert_eq!(f.status(), before);
}

/// `--pathspec-from-file` is the same limiting, read from a file.
#[test]
fn pathspec_from_file_limits_the_same_way() {
    let f = Fixture::new("psfile");
    let list = f.root.join("specs");
    std::fs::write(&list, "sub/t.txt\n").unwrap();
    let (ok, _, err) =
        f.run(&["stash", "push", "-m", "t", "--pathspec-from-file", list.to_str().unwrap()]);
    assert!(ok, "stash failed: {err}");

    let status = f.status();
    assert!(!status.iter().any(|(_, p)| p == "sub/t.txt"), "matched path reset: {status:?}");
    assert!(status.contains(&(" M".to_string(), "b.txt".to_string())), "{status:?}");
    assert!(status.contains(&("M ".to_string(), "a.txt".to_string())), "{status:?}");
}

/// `--only-untracked` is not a git option; accepting it as a synonym for `-u`
/// would silently stash more than asked.
#[test]
fn only_untracked_is_not_an_option() {
    let f = Fixture::new("onlyuntracked");
    let (ok, _, err) = f.run(&["stash", "push", "--only-untracked", "-m", "t"]);
    assert!(!ok, "expected refusal");
    assert!(err.contains("only-untracked"), "unexpected message: {err}");
    assert!(f.run(&["stash", "list"]).1.is_empty(), "nothing may be stashed");
}
