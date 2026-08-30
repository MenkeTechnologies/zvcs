//! The daemon's half of `git zpin`, which `zpin_freeze.rs` says it cannot cover:
//! that `react()` actually honours the flag it reads.
//!
//! A pin is per repository — `git zpin <path>` pins each named repository, and
//! the verb documents that "a pinned repo is skipped by autobump and reconcile,
//! so its gitlink and HEAD will not move on their own until it is unpinned".
//! The flag was read once, for the top-level repo, while the reconcile walked
//! the whole tree, so pinning a submodule wrote a row nothing consulted and the
//! next reaction fast-forwarded it anyway. Measured before the fix: a pinned
//! submodule one commit behind its origin was moved onto origin/main.
//!
//! Both directions are asserted in one daemon lifetime, because "it did not
//! move" proves nothing unless the same fixture moves once the pin is lifted.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new(BIN)
        .args(["-c", "user.email=test@example.com", "-c", "user.name=zvcs-test", "-c", "protocol.file.allow=always"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn head(dir: &Path) -> String {
    String::from_utf8_lossy(&git(dir, &["rev-parse", "HEAD"]).stdout).trim().to_string()
}

fn wait_for(path: &Path, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon socket never appeared at {}", path.display());
}

fn wait_for_log(log: &Path, needle: &str, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if std::fs::read_to_string(log).map(|s| s.contains(needle)).unwrap_or(false) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Poll until `dir`'s HEAD equals `want`, or the timeout elapses.
fn moved_to(dir: &Path, want: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if head(dir) == want {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// The control: the same fixture with no pin at all must reconcile, or the
/// pinned case below proves only that nothing was reconciling.
#[test]
fn an_unpinned_submodule_is_reconciled() {
    assert!(
        reconciles_with(Pin::No),
        "the fixture never reconciled without a pin, so the pinned case would prove nothing"
    );
}

#[test]
fn a_pinned_submodule_is_not_reconciled() {
    assert!(
        !reconciles_with(Pin::Yes),
        "the daemon reconciled a PINNED submodule (the pin was read for the top-level repo only)"
    );
}

#[derive(Clone, Copy, PartialEq)]
enum Pin {
    Yes,
    No,
}

/// Build a parent+submodule tree with the submodule one commit behind its
/// already-fetched origin/main, optionally pin the submodule, run one daemon
/// over it, and report whether the submodule was fast-forwarded.
///
/// Each call owns a fresh tree and a fresh daemon: pinning or unpinning under a
/// daemon that is already watching mixes the two states in one measurement.
fn reconciles_with(pin: Pin) -> bool {
    // Short path: a unix socket must fit in SUN_LEN (~104 bytes).
    let tag = if pin == Pin::Yes { "y" } else { "n" };
    let root = std::env::temp_dir().join(format!("zv-pin{tag}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    // Before ANY command runs: `git zpin` writes to the database named by
    // ZVCS_HOME, and the daemon reads the one named when it starts. Setting these
    // late puts the pin in one database and the reader on another, and the pin
    // then looks ignored when it was simply never seen.
    let sock = root.join("s");
    std::env::set_var("ZVCS_SOCK", &sock);
    std::env::set_var("ZVCS_HOME", root.join("h"));

    // A submodule source two commits deep, so its checkout can sit one behind.
    let sub_src = root.join("sub_src");
    std::fs::create_dir_all(&sub_src).unwrap();
    git(&sub_src, &["init", "-q", "-b", "main"]);
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s0"]);
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s1"]);

    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    git(&parent, &["commit", "--allow-empty", "-q", "-m", "p0"]);
    git(&parent, &["submodule", "add", "-q", sub_src.to_str().unwrap(), "sub"]);
    git(&parent, &["commit", "-q", "-m", "add sub"]);

    // Put the submodule one commit behind its already-fetched origin/main, which
    // is exactly what the fetch-free reconcile fast-forwards.
    let sub = parent.join("sub");
    git(&sub, &["fetch", "-q", "origin"]);
    let target = String::from_utf8_lossy(&git(&sub, &["rev-parse", "origin/main"]).stdout).trim().to_string();
    git(&sub, &["reset", "-q", "--hard", "HEAD~1"]);
    let behind = head(&sub);
    assert_ne!(behind, target, "precondition: the submodule is behind origin/main");

    // Pin BEFORE autonomy is switched on. The daemon runs one reaction the
    // moment it starts, so a pin applied afterwards would be tested against a
    // submodule that had already been reconciled — and `git zpin` itself
    // autostarts a daemon once autonomy is configured, which is the other reason
    // this has to come first.
    if pin == Pin::Yes {
        git(&parent, &["zpin", sub.to_str().unwrap()]);
    }

    // Written directly rather than through `git config`: every zvcs command
    // autostarts the daemon when autonomy is configured, and this test needs to
    // own the daemon's lifetime.
    let mut cfg = std::fs::read_to_string(parent.join(".git/config")).unwrap();
    cfg.push_str("[zvcs]\n\tautoreconcile = true\n\tinterval = 1\n");
    std::fs::write(parent.join(".git/config"), cfg).unwrap();

    let log_path = root.join("daemon.log");
    let logf = std::fs::File::create(&log_path).unwrap();
    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&parent)
        .stdout(Stdio::from(logf.try_clone().unwrap()))
        .stderr(Stdio::from(logf))
        .spawn()
        .expect("spawn zdaemon");
    wait_for(&sock, Duration::from_secs(5));
    wait_for_log(&log_path, "[zvcs watch] watching", Duration::from_secs(10));

    std::fs::write(parent.join("poke.txt"), b"one\n").unwrap();
    let moved = moved_to(&sub, &target, Duration::from_secs(20));

    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&parent).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);
    moved
}
