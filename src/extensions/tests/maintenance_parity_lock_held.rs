//! `maintenance run` against a `maintenance.lock` another run still holds.
//!
//! `maintenance_run_tasks()` (builtin/gc.c:1392-1436) takes
//! `<objdir>/maintenance.lock` with no timeout. When it is already held the run
//! returns 0 without running a task, and the `warning: lock file '%s' exists,
//! skipping maintenance` line is printed only when neither `--auto` nor
//! `--quiet` is in effect — and `--quiet` defaults to `!isatty(2)`, so with a
//! piped stderr the skip is silent.
//!
//! Every `commit` starts `maintenance run --auto --detach` (run-command.c:1987-1993),
//! whose daemonized half holds that lock through its background phase, after
//! `commit` has returned (builtin/gc.c:1422-1434). A `maintenance run` started in
//! that window skips silently in stock git 2.56.0 as well; the tests here pin the
//! skip itself with a lock created by hand, and the collection with the commit's
//! auto-maintenance switched off, so neither depends on a race.
//!
//! Expectations are stock's, measured on identical fixtures with the environment
//! isolated and dates pinned.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const ENTRY: &str = "0123456789abcdef0123456789abcdef01234567";

/// One side's repository: a commit made with auto-maintenance off, rerere on,
/// and one unresolved `rr-cache` record 100 days old.
struct Side {
    root: PathBuf,
}

impl Drop for Side {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn command(bin: &str, root: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.current_dir(cwd)
        .env("HOME", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000");
    cmd
}

impl Side {
    fn new(bin: &str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-maint-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let side = Side { root: root.canonicalize().unwrap() };
        side.ok(bin, &["init", "-q", "-b", "main", "."]);
        side.ok(bin, &["config", "rerere.enabled", "true"]);
        std::fs::write(side.root.join("sub/f"), "v1\n").unwrap();
        side.ok(bin, &["add", "sub/f"]);
        // No detached `maintenance run --auto` left holding the lock behind us.
        side.ok(bin, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "c1"]);

        let entry = side.entry();
        std::fs::create_dir_all(&entry).unwrap();
        let preimage = entry.join("preimage");
        std::fs::write(&preimage, "<<<<<<<\nours\n=======\ntheirs\n>>>>>>>\n").unwrap();
        let old = SystemTime::now() - Duration::from_secs(100 * 86400);
        std::fs::File::options()
            .write(true)
            .open(&preimage)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_accessed(old).set_modified(old))
            .unwrap();
        side
    }

    fn entry(&self) -> PathBuf {
        self.root.join(".git/rr-cache").join(ENTRY)
    }

    fn lock(&self) -> PathBuf {
        self.root.join(".git/objects/maintenance.lock")
    }

    fn ok(&self, bin: &str, args: &[&str]) {
        let out = command(bin, &self.root, &self.root).args(args).output().unwrap();
        assert!(out.status.success(), "{bin} {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// stdout, stderr, exit code, and what is left behind: whether the record
    /// and the lock still exist.
    fn run(&self, bin: &str, cwd: &str, args: &[&str]) -> (String, String, Option<i32>, bool, bool) {
        let out = command(bin, &self.root, &self.root.join(cwd)).args(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code(),
            self.entry().exists(),
            self.lock().exists(),
        )
    }
}

/// Run `args` from `cwd` on a fresh pair of fixtures, with `maintenance.lock`
/// held beforehand when `held`, and compare zvcs against stock.
fn compare(tag: &str, held: bool, cwd: &str, args: &[&str]) -> Option<(String, String, Option<i32>, bool, bool)> {
    let stock = stock_git()?;
    let s = Side::new(stock, &format!("{tag}-s"));
    let z = Side::new(stock, &format!("{tag}-z"));
    if held {
        std::fs::write(s.lock(), "").unwrap();
        std::fs::write(z.lock(), "").unwrap();
    }
    let want = s.run(stock, cwd, args);
    let got = z.run(BIN, cwd, args);
    assert_eq!(got, want, "{args:?} from {cwd:?} (lock held: {held})");
    Some(want)
}

/// A held lock with stderr piped: `--quiet` is the default, so the run skips
/// every task silently and exits 0, leaving the expired record and the other
/// run's lock alone.
#[test]
fn a_held_lock_skips_the_run_silently_when_stderr_is_not_a_terminal() {
    let Some((out, err, code, entry, lock)) = compare("quiet", true, ".", &["maintenance", "run", "--task=rerere-gc"])
    else {
        return;
    };
    assert_eq!((out.as_str(), err.as_str(), code, entry, lock), ("", "", Some(0), true, true));
}

/// `--no-quiet` brings the warning back; the path is `<objdir>/maintenance`,
/// relative to the top of the work tree even from a subdirectory.
#[test]
fn a_held_lock_warns_under_no_quiet() {
    for cwd in [".", "sub"] {
        let Some((_, err, code, entry, _)) =
            compare(&format!("noquiet-{}", cwd.len()), true, cwd, &["maintenance", "run", "--task=rerere-gc", "--no-quiet"])
        else {
            return;
        };
        assert_eq!(err, "warning: lock file '.git/objects/maintenance' exists, skipping maintenance\n");
        assert_eq!((code, entry), (Some(0), true));
    }
}

/// Under `--auto` a held lock reads as a recursive process stack and is not
/// reported even with `--no-quiet`; a `--schedule` run warns like a manual one.
#[test]
fn a_held_lock_under_auto_and_schedule() {
    let Some((_, err, ..)) = compare("auto", true, ".", &["maintenance", "run", "--auto", "--no-quiet"]) else {
        return;
    };
    assert_eq!(err, "");
    let Some((_, err, ..)) = compare("sched", true, ".", &["maintenance", "run", "--schedule=weekly", "--no-quiet"])
    else {
        return;
    };
    assert_eq!(err, "warning: lock file '.git/objects/maintenance' exists, skipping maintenance\n");
}

/// With nothing holding the lock the task runs `rerere gc`, which expires the
/// unresolved record (`gc.rerereUnresolved`, 15 days), and the lock is released.
#[test]
fn a_free_lock_runs_the_task_and_releases_it() {
    let Some((out, err, code, entry, lock)) = compare("free", false, ".", &["maintenance", "run", "--task=rerere-gc"])
    else {
        return;
    };
    assert_eq!((out.as_str(), err.as_str(), code, entry, lock), ("", "", Some(0), false, false));
}
