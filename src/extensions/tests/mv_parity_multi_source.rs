//! `git mv` with several `<source>` operands, where one source can invalidate
//! another before it is reached.
//!
//! `cmd_mv()` validates every source against the *pristine* index and worktree
//! and only then renames anything, so two sources that would collide are refused
//! outright rather than discovered halfway through. Two of those refusals were
//! missing here, and both of them lost data: the command renamed the first source
//! and then died on the second, leaving the worktree in a state neither git nor
//! the caller asked for.
//!
//!   * `cannot move both '<x>' and its parent directory '<y>'`
//!     (builtin/mv.c:499-523) — the ancestor moves first, so the descendant's
//!     source no longer exists.
//!   * `multiple sources for the same target` (builtin/mv.c:438-441) — the second
//!     source is the first one again, already renamed away.
//!
//! The rest of this file pins the diagnostics that name *which* argument is wrong,
//! since an unhelpful-but-fatal message and a helpful one are indistinguishable
//! from an exit code alone.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

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
    /// An empty repository with no commit and, deliberately, no index file: the
    /// state a fresh `git init` leaves behind.
    fn bare_init(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-mvmulti-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f
    }

    /// `d/n/a` and `d/b` tracked, plus a `dest/` to move things into.
    fn new(tag: &str) -> Self {
        let f = Self::bare_init(tag);
        std::fs::create_dir_all(f.work.join("d/n")).unwrap();
        std::fs::create_dir_all(f.work.join("dest")).unwrap();
        f.write("d/n/a", b"a\n");
        f.write("d/b", b"b\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "init"]);
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
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn exists(&self, path: &str) -> bool {
        self.work.join(path).exists()
    }

    /// `ls-files`, so the index half of a refusal can be checked too — a rename
    /// that landed on disk but not in the index is still a rename that happened.
    fn tracked(&self) -> Vec<String> {
        let out = self.run(&["ls-files"]);
        String::from_utf8_lossy(&out.stdout).lines().map(ToOwned::to_owned).collect()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A directory and something beneath it, in either argument order: refused
/// before the first `rename()`, with both paths named.
#[test]
fn a_directory_and_a_path_under_it_cannot_move_together() {
    let f = Fixture::new("parent");

    for args in [
        ["mv", "d", "d/n", "dest"],
        ["mv", "d/n", "d", "dest"],
        ["mv", "d", "d/n/a", "dest"],
    ] {
        let out = f.run(&args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {out:?}");
        let want = if args.contains(&"d/n/a") {
            "fatal: cannot move both 'd/n/a' and its parent directory 'd'\n"
        } else {
            "fatal: cannot move both 'd/n' and its parent directory 'd'\n"
        };
        assert_eq!(stderr(&out), want, "{args:?}");
        // Nothing moved: the refusal runs before the rename loop, which is the
        // whole point — the ancestor would otherwise be gone by now.
        assert!(f.exists("d/n/a"), "{args:?} left the worktree alone");
        assert!(f.exists("d/b"), "{args:?} left the worktree alone");
        assert_eq!(f.tracked(), ["d/b", "d/n/a"], "{args:?} left the index alone");
    }
}

/// `-k` asks for unusable sources to be skipped, not for this pair to be
/// resolved: git's check is a `die()`, outside the `ignore_errors` path.
#[test]
fn skipping_errors_does_not_excuse_a_parent_and_its_child() {
    let f = Fixture::new("parent-k");
    let out = f.run(&["mv", "-k", "d", "d/n", "dest"]);

    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(stderr(&out), "fatal: cannot move both 'd/n' and its parent directory 'd'\n");
    assert!(f.exists("d/n/a"));
}

/// `--dry-run` announces every pair it checks before it refuses, and the entries
/// a directory source expands into come *after* every path the command line
/// named — they are appended to git's source array, not spliced in.
#[test]
fn a_dry_run_announces_the_command_line_sources_before_their_expansions() {
    let f = Fixture::new("parent-n");
    let out = f.run(&["mv", "-n", "d", "d/n", "dest"]);

    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        stdout(&out),
        "Checking rename of 'd' to 'dest/d'\n\
         Checking rename of 'd/n' to 'dest/n'\n\
         Checking rename of 'd/b' to 'dest/d/b'\n\
         Checking rename of 'd/n/a' to 'dest/d/n/a'\n\
         Checking rename of 'd/n/a' to 'dest/n/a'\n"
    );
    assert!(f.exists("d/n/a"), "a dry run never moves anything");
}

/// The same file named twice: the second occurrence is refused before the first
/// one is renamed, instead of failing on a source that no longer exists.
#[test]
fn one_target_cannot_have_two_sources() {
    let f = Fixture::new("dup");
    let out = f.run(&["mv", "d/b", "d/b", "dest"]);

    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        stderr(&out),
        "fatal: multiple sources for the same target, source=d/b, destination=dest/b\n"
    );
    assert!(f.exists("d/b"), "the first copy of the argument did not move either");
    assert!(!f.exists("dest/b"));
    assert_eq!(f.tracked(), ["d/b", "d/n/a"]);
}

/// With `-k` the duplicate is the only thing dropped: the moves around it still
/// happen, and the file lands once.
#[test]
fn skipping_errors_drops_only_the_duplicate_source() {
    let f = Fixture::new("dup-k");
    let out = f.run(&["mv", "-k", "d/b", "d/b", "d/n/a", "dest"]);

    assert!(out.status.success(), "{out:?}");
    assert_eq!(stderr(&out), "");
    assert_eq!(f.tracked(), ["dest/a", "dest/b"]);
    assert!(f.exists("dest/b") && f.exists("dest/a"));
}

/// A directory that exists on disk with nothing tracked under it is not "not
/// under version control" — that answer belongs to files. git names the actual
/// problem so the caller knows the directory itself was found.
#[test]
fn an_untracked_directory_source_is_reported_as_empty() {
    let f = Fixture::new("empty");
    std::fs::create_dir_all(f.work.join("hollow")).unwrap();
    let out = f.run(&["mv", "hollow", "dest"]);

    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        stderr(&out),
        "fatal: source directory is empty, source=hollow, destination=dest/hollow\n"
    );
}

/// A repository that has never staged anything has no index file at all.
/// `repo_read_index()` treats that as an empty index, so `git mv` still reaches
/// its own diagnostics instead of dying on the missing file.
#[test]
fn a_repository_without_an_index_file_still_reports_the_real_problem() {
    let f = Fixture::bare_init("noindex");
    f.write("a", b"a\n");
    assert!(!f.work.join(".git/index").exists(), "the fixture must have no index file");

    let untracked = f.run(&["mv", "a", "b"]);
    assert_eq!(untracked.status.code(), Some(128), "{untracked:?}");
    assert_eq!(
        stderr(&untracked),
        "fatal: not under version control, source=a, destination=b\n"
    );

    let absent = f.run(&["mv", "nosuch", "other"]);
    assert_eq!(absent.status.code(), Some(128), "{absent:?}");
    assert_eq!(stderr(&absent), "fatal: bad source, source=nosuch, destination=other\n");

    assert!(f.exists("a"), "neither refusal touched the worktree");
}

/// `-f` silences the refusal to clobber; `-v` still says which file is about to
/// be destroyed, and says it on stderr as a warning.
#[test]
fn forcing_a_clobber_verbosely_warns_about_the_file_it_destroys() {
    let f = Fixture::new("clobber");
    f.write("victim", b"old\n");
    f.git(&["add", "victim"]);
    f.git(&["commit", "-q", "-m", "victim"]);

    let out = f.run(&["mv", "-f", "-v", "d/b", "victim"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stderr(&out), "warning: overwriting 'victim'\n");
    assert_eq!(stdout(&out), "Renaming d/b to victim\n");
    assert_eq!(std::fs::read(f.work.join("victim")).unwrap(), b"b\n");

    // Without `-v` the clobber is silent, so the warning is tied to the flag and
    // not to `-f`.
    let quiet = Fixture::new("clobber-quiet");
    quiet.write("victim", b"old\n");
    quiet.git(&["add", "victim"]);
    quiet.git(&["commit", "-q", "-m", "victim"]);
    let out = quiet.run(&["mv", "-f", "d/b", "victim"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stderr(&out), "");
}

/// A gitlink source is moved as one entry. It never reaches the expansion that
/// appends a directory's tracked contents, so `-v` reports it once — reporting
/// the entry a second time made `git mv -v` claim two renames for one move.
#[test]
fn a_gitlink_move_is_reported_once() {
    let f = Fixture::new("gitlink");
    let sub = f.work.join("mod");
    std::fs::create_dir_all(&sub).unwrap();
    for args in [
        vec!["-C", "mod", "init", "-q", "-b", "main", "."],
        vec!["-C", "mod", "config", "user.email", "t@e.co"],
        vec!["-C", "mod", "config", "user.name", "t"],
    ] {
        f.git(&args);
    }
    std::fs::write(sub.join("x"), b"x\n").unwrap();
    f.git(&["-C", "mod", "add", "x"]);
    f.git(&["-C", "mod", "commit", "-q", "-m", "sub"]);
    let head = f.run(&["-C", "mod", "rev-parse", "HEAD"]);
    let head = String::from_utf8_lossy(&head.stdout).trim().to_owned();

    f.git(&["update-index", "--add", "--cacheinfo", &format!("160000,{head},mod")]);
    f.write(".gitmodules", b"[submodule \"mod\"]\n\tpath = mod\n\turl = ./mod\n");
    f.git(&["add", ".gitmodules"]);
    f.git(&["commit", "-q", "-m", "gitlink"]);

    let out = f.run(&["mv", "-v", "mod", "mod2"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stdout(&out), "Renaming mod to mod2\n");
    // The move is real: the gitlink followed the rename and `.gitmodules` was
    // rewritten and restaged along with it.
    assert!(f.tracked().contains(&"mod2".to_owned()));
    assert_eq!(
        std::fs::read_to_string(f.work.join(".gitmodules")).unwrap(),
        "[submodule \"mod\"]\n\tpath = mod2\n\turl = ./mod\n"
    );
}
