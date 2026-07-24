//! A thread that already holds a repo's lock must be able to acquire it AGAIN
//! without deadlocking. A nested acquire that went to the daemon would queue
//! behind the outer hold forever (the outer guard can't drop while the thread is
//! blocked). The fix makes RepoLock reentrant per-thread.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use zvcs::lock::RepoLock;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn init_repo(root: &Path, name: &str) -> PathBuf {
    let r = root.join(name);
    std::fs::create_dir_all(&r).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&r).status().unwrap().success());
    r.join(".git").canonicalize().unwrap()
}

#[test]
fn nested_acquire_same_repo_same_thread_does_not_deadlock() {
    let root = std::env::temp_dir().join(format!("zvcs-reentrant-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let sock = root.join("sock");
    std::env::set_var("ZVCS_SOCK", &sock);

    let git_dir = init_repo(&root, "r");

    let mut daemon: Child = Command::new(BIN).args(["zdaemon", "start", "--foreground"]).current_dir(&root).spawn().unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) && !sock.exists() {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(sock.exists(), "daemon socket never appeared");

    // Do the nested acquire on a worker thread; the outer test thread enforces a
    // deadline so a regression (nested acquire blocking) fails instead of hanging.
    let (tx, rx) = mpsc::channel();
    let gd = git_dir.clone();
    let worker = thread::spawn(move || {
        let outer = RepoLock::acquire(&gd);
        assert!(outer.is_held(), "outer acquire must be granted by the live daemon");
        // This is the crux: same thread, same repo, while `outer` is still held.
        let _inner = RepoLock::acquire(&gd); // must return immediately (reentrant)
        // If we got here, no deadlock. Release both (inner then outer).
        drop(_inner);
        drop(outer);
        let _ = tx.send(());
    });

    let finished = rx.recv_timeout(Duration::from_secs(8)).is_ok();
    let _ = worker.join();

    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&root).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(finished, "nested same-thread acquire deadlocked (reentrancy broken)");
}
