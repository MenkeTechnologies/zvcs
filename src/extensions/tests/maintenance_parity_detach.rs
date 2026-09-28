//! `maintenance run`'s two phases, its lock, and the automatic run that waits
//! for it.
//!
//! * `run_auto_maintenance()` (run-command.c:1987-1993) is `run_command()` over
//!   `maintenance run --auto --[no-]quiet --[no-]detach` with the standard
//!   streams inherited: the child reads its configuration where the caller can
//!   see it, so under `core.fsyncObjectFiles` a `commit` prints the deprecation
//!   warning twice, the second from the child. zvcs sent the child to
//!   `/dev/null` and printed it once.
//! * `maintenance_run_tasks()` (builtin/gc.c:1785-1828) holds
//!   `<objdir>/maintenance.lock`; when it is taken, `warning: lock file
//!   '<objdir>/maintenance' exists, skipping maintenance` unless `--auto` or
//!   `--quiet`, exit 0. zvcs had no lock.
//! * The foreground halves run, then `daemonize()` (setup.c:2186-2222) under
//!   `--detach`, then the background halves with `/dev/null` for stdio: a
//!   detached `loose-objects` prints nothing and its pack appears afterwards.
//!   zvcs ignored `--detach`.
//! * The `gc` task's foreground half is `gc_foreground_tasks()`
//!   (builtin/gc.c:834-842), `pack-refs` then `reflog expire`; its background half
//!   is `git gc … --no-detach --skip-foreground-tasks` (builtin/gc.c:1253-1272).
//!   zvcs ran the whole `gc` in one go.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

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
    fn new(tag: &str) -> Self {
        let f = Self::empty(tag);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        // No automatic maintenance: its detached half would hold the lock the
        // tests below look at.
        f.run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
        f
    }

    fn empty(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-maintenance-detach-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn git_path(&self, rel: &str) -> PathBuf {
        self.work.join(".git").join(rel)
    }

    /// Wait for the detached half to leave a condition true.
    fn eventually(&self, what: &str, done: impl Fn() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn the_automatic_run_is_waited_for_with_its_stderr_shown() {
    // `commit` reads the key once itself; the child prints the second line.
    let f = Fixture::empty("auto");
    f.run(&["config", "core.fsyncObjectFiles", "true"]);
    std::fs::write(f.work.join("b"), "b\n").unwrap();
    f.run(&["add", "b"]);
    let warning = "warning: core.fsyncObjectFiles is deprecated; use core.fsync instead\n";
    assert_eq!(f.run(&["commit", "-q", "-m", "one"]), (String::new(), warning.repeat(2), 0));
    // Its detached half releases the lock once the background phase is done.
    let lock = f.git_path("objects/maintenance.lock");
    f.eventually("the daemon to release its lock", || !lock.exists());
}

#[test]
fn a_held_lock_skips_the_run() {
    let f = Fixture::new("lock");
    let lock = f.git_path("objects/maintenance.lock");
    std::fs::write(&lock, "").unwrap();
    assert_eq!(
        f.run(&["maintenance", "run", "--task=pack-refs", "--no-quiet"]),
        (
            String::new(),
            "warning: lock file '.git/objects/maintenance' exists, skipping maintenance\n".to_owned(),
            0
        )
    );
    let silent = (String::new(), String::new(), 0);
    assert_eq!(f.run(&["maintenance", "run", "--task=pack-refs", "--quiet"]), silent);
    assert_eq!(f.run(&["maintenance", "run", "--task=pack-refs", "--auto", "--no-quiet"]), silent);
    assert!(lock.exists(), "someone else's lock is left alone");
    assert!(!f.git_path("packed-refs").exists());
}

#[test]
fn detach_runs_the_background_half_out_of_sight() {
    let f = Fixture::new("detach");
    assert_eq!(
        f.run(&["maintenance", "run", "--task=loose-objects", "--no-quiet", "--detach"]),
        (String::new(), String::new(), 0)
    );
    let pack_dir = f.git_path("objects/pack");
    f.eventually("the loose-objects pack", || {
        std::fs::read_dir(&pack_dir).is_ok_and(|dir| {
            dir.flatten().any(|e| e.file_name().to_string_lossy().starts_with("loose-"))
        })
    });
    let lock = f.git_path("objects/maintenance.lock");
    f.eventually("the daemon to release its lock", || !lock.exists());
}

#[test]
fn the_gc_task_packs_refs_before_it_detaches() {
    let f = Fixture::new("skip");
    f.run(&["branch", "side"]);
    // The foreground half, so the refs are packed by the time the parent exits.
    assert_eq!(f.run(&["maintenance", "run", "--task=gc", "--quiet", "--detach"]), (String::new(), String::new(), 0));
    assert!(f.git_path("packed-refs").exists());
    assert!(!f.git_path("refs/heads/side").exists());
    let lock = f.git_path("objects/maintenance.lock");
    f.eventually("the daemon to release its lock", || !lock.exists());
}
