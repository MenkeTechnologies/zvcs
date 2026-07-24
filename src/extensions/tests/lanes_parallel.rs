//! Per-repo lanes must be INDEPENDENT: locks on two different repos can be held
//! at the same time. (coordination.rs proves same-repo serialization; this proves
//! cross-repo parallelism — the whole point of per-repo lanes.)

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use zvcs::lock::RepoLock;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn init_repo(root: &Path, name: &str) -> PathBuf {
    let r = root.join(name);
    std::fs::create_dir_all(&r).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&r).status().unwrap().success());
    r.join(".git")
}

/// Acquire `git_dir`'s lock, announce it, and wait (bounded) for the peer to
/// announce theirs. Returns whether the peer was seen holding concurrently.
fn hold_and_observe(git_dir: PathBuf, mine: Arc<AtomicBool>, peer: Arc<AtomicBool>) -> bool {
    let guard = RepoLock::acquire(&git_dir);
    assert!(guard.is_held(), "must acquire via the live daemon");
    mine.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_peer = false;
    while Instant::now() < deadline {
        if peer.load(Ordering::SeqCst) {
            saw_peer = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    // hold a touch longer so the peer definitely observes us too, then release
    thread::sleep(Duration::from_millis(50));
    drop(guard);
    saw_peer
}

#[test]
fn different_repos_lock_concurrently() {
    let root = std::env::temp_dir().join(format!("zvcs-lanes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let sock = root.join("sock");
    std::env::set_var("ZVCS_SOCK", &sock);

    let a = init_repo(&root, "a");
    let b = init_repo(&root, "b");

    let mut daemon: Child = Command::new(BIN).args(["zdaemon", "start", "--foreground"]).current_dir(&root).spawn().unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) && !sock.exists() {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(sock.exists(), "daemon socket never appeared");

    let a_held = Arc::new(AtomicBool::new(false));
    let b_held = Arc::new(AtomicBool::new(false));
    let (a1, b1) = (Arc::clone(&a_held), Arc::clone(&b_held));
    let (a2, b2) = (Arc::clone(&a_held), Arc::clone(&b_held));

    let t1 = thread::spawn(move || hold_and_observe(a, a1, b1));
    let t2 = thread::spawn(move || hold_and_observe(b, b2, a2));
    let saw_from_a = t1.join().unwrap();
    let saw_from_b = t2.join().unwrap();

    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&root).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        saw_from_a && saw_from_b,
        "different repos must be lockable concurrently (a saw b: {saw_from_a}, b saw a: {saw_from_b}) — lanes are serializing globally"
    );
}
