//! Integration test for the zvcs coordinator: proves that concurrent writers
//! going through `RepoLock` are granted the critical section one at a time, in
//! the daemon's FIFO order — the property that replaces `index.lock` contention.
//!
//! Shape: spawn the real `git zdaemon start` child against a throwaway repo, then
//! race N threads that each `RepoLock::acquire`, mark themselves in the critical
//! section, and release. If mutual exclusion holds, the peak observed occupancy
//! is exactly 1. A broken lock would let occupancy exceed 1.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use zvcs::lock::RepoLock;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn wait_for_socket(sock: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if sock.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn daemon_serializes_concurrent_writers() {
    // Throwaway repo (canonicalized so the path matches what the daemon discovers).
    let tmp = std::env::temp_dir().join(format!("zvcs-coord-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("mkdir tmp");
    let tmp = tmp.canonicalize().expect("canonicalize tmp");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&tmp)
            .status()
            .expect("run git init")
            .success(),
        "git init failed"
    );
    let git_dir: PathBuf = tmp.join(".git");

    // Isolate the singleton socket to this test via ZVCS_SOCK, so the shared
    // ~/.zvcs/zvcs.sock is never touched and parallel test binaries don't collide.
    // Both this process's RepoLock and the spawned daemon read the same override
    // (the child inherits the env).
    let sock = tmp.join("zvcs-test.sock");
    std::env::set_var("ZVCS_SOCK", &sock);

    // Start the coordinator.
    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&tmp)
        .spawn()
        .expect("spawn zdaemon");
    assert!(
        wait_for_socket(&sock, Duration::from_secs(5)),
        "daemon socket never appeared at {}",
        sock.display()
    );

    // Race N writers through the lock.
    const N: usize = 8;
    let in_cs = Arc::new(AtomicUsize::new(0));
    let max_cs = Arc::new(AtomicUsize::new(0));
    let held_via_daemon = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..N)
        .map(|_| {
            let gd = git_dir.clone();
            let in_cs = Arc::clone(&in_cs);
            let max_cs = Arc::clone(&max_cs);
            let held = Arc::clone(&held_via_daemon);
            thread::spawn(move || {
                let guard = RepoLock::acquire(&gd);
                if guard.is_held() {
                    held.fetch_add(1, Ordering::SeqCst);
                }
                // Enter critical section.
                let now = in_cs.fetch_add(1, Ordering::SeqCst) + 1;
                max_cs.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(40));
                // Leave before releasing (guard drops at end of scope -> RELEASE).
                in_cs.fetch_sub(1, Ordering::SeqCst);
            })
        })
        .collect();

    let joined: Vec<_> = handles.into_iter().map(|h| h.join()).collect();

    // Tear down the daemon regardless of assertions below.
    let _ = Command::new(BIN)
        .args(["zdaemon", "stop"])
        .current_dir(&tmp)
        .status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(joined.iter().all(|r| r.is_ok()), "a worker thread panicked");
    assert_eq!(
        held_via_daemon.load(Ordering::SeqCst),
        N,
        "every writer must acquire via the live daemon (not the no-op fallback)"
    );
    assert_eq!(
        max_cs.load(Ordering::SeqCst),
        1,
        "peak concurrent holders must be 1 — the lock failed to serialize"
    );
}
