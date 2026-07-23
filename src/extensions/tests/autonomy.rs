//! End-to-end test of the reactive autonomy: a commit inside a submodule must
//! make the daemon's file watcher autobump the parent pointer and *commit* it,
//! so the parent's `modified: <sub> (new commits)` marker clears — with no
//! manual `git add`/`commit` at the root and no timer/poll.
//!
//! Shape: build a parent repo with one real (local) submodule, enable
//! `[zvcs] autobump`, start the singleton daemon (isolated via `ZVCS_SOCK`),
//! commit in the submodule, and poll until an `zvcs: autobump` commit appears in
//! the parent and its status is clean.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .args([
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=zvcs-test",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stdout(out: std::process::Output) -> String {
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn watcher_autobumps_submodule_pointer_on_commit() {
    let root = std::env::temp_dir().join(format!("zvcs-auto-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    // A submodule source repo with one commit.
    let sub_src = root.join("sub_src");
    std::fs::create_dir_all(&sub_src).unwrap();
    git(&sub_src, &["init", "-q", "-b", "main"]);
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "sub root"]);

    // Parent repo with the submodule added and committed.
    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    git(&parent, &["commit", "--allow-empty", "-q", "-m", "parent root"]);
    git(
        &parent,
        &["submodule", "add", "-q", sub_src.to_str().unwrap(), "sub"],
    );
    git(&parent, &["commit", "-q", "-m", "add submodule"]);

    // Enable autobump with a short debounce.
    git(&parent, &["config", "zvcs.autobump", "true"]);
    git(&parent, &["config", "zvcs.interval", "1"]);

    // Isolated singleton socket; the daemon (cwd=parent) inherits it. Capture its
    // output so we can wait for the watcher to actually arm before we move a ref —
    // `notify` does not replay events that predate the watch, so a fixed sleep is a
    // race on a slow runner (the submodule commit below could land before the
    // watches exist and be missed entirely).
    let sock = root.join("zvcs-test.sock");
    std::env::set_var("ZVCS_SOCK", &sock);
    let daemon_log = root.join("daemon.log");
    let logf = std::fs::File::create(&daemon_log).unwrap();
    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start"])
        .current_dir(&parent)
        .stdout(Stdio::from(logf.try_clone().unwrap()))
        .stderr(Stdio::from(logf))
        .spawn()
        .expect("spawn zdaemon");
    wait_for(&sock, Duration::from_secs(5));
    // Block until the watch loop has armed its watches (printed only after the
    // initial converge pass AND every `watcher.watch()` call in
    // superset::watch::run). Best-effort: on timeout we proceed and let the 20s
    // poll below be the real assertion.
    wait_for_log(&daemon_log, "[zvcs watch] watching", Duration::from_secs(10));

    // Commit inside the checked-out submodule -> its HEAD moves -> parent shows
    // `modified: sub (new commits)`.
    let sub_wt = parent.join("sub");
    git(&sub_wt, &["commit", "--allow-empty", "-q", "-m", "sub work"]);

    // Poll until the parent autobumps + commits the pointer (marker cleared).
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut bumped = false;
    while Instant::now() < deadline {
        let log = stdout(git(&parent, &["log", "--oneline", "-5"]));
        let status = stdout(git(&parent, &["status", "--porcelain"]));
        if log.contains("autobump") && !status.contains("sub") {
            bumped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    // Teardown before asserting.
    let _ = Command::new(BIN)
        .args(["zdaemon", "stop"])
        .current_dir(&parent)
        .status();
    let _ = daemon.kill();
    let _ = daemon.wait();

    let final_log = stdout(git(&parent, &["log", "--oneline", "-5"]));
    let final_status = stdout(git(&parent, &["status", "--porcelain"]));
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        bumped,
        "watcher never autobumped the submodule pointer.\nparent log:\n{final_log}\nparent status:\n{final_status}"
    );
}

fn wait_for(sock: &Path, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if sock.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon socket never appeared at {}", sock.display());
}

/// Poll `log` until it contains `needle`, or `timeout` elapses. Best-effort: it
/// does not panic on timeout — the caller's downstream assertion is the real gate.
/// Used to confirm the daemon's watcher is armed before we mutate refs.
fn wait_for_log(log: &Path, needle: &str, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(s) = std::fs::read_to_string(log) {
            if s.contains(needle) {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
