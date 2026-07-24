//! `zjob restart` clones a job (parent-linked) and re-enqueues it; `zjob stop`
//! on a finished job reports it is not stoppable. Exercises the daemon job
//! control protocol (JOBRESTART / JOBSTOP) end-to-end.

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=zvcs-test"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

fn zvcs(cwd: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(BIN).args(args).current_dir(cwd).output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn zjob_restart_and_stop_control() {
    let root = std::env::temp_dir().join(format!("zvcs-jobctl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::env::set_var("ZVCS_SOCK", root.join("sock"));
    std::env::set_var("ZVCS_HOME", root.join("home"));

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "root"]);
    std::fs::write(repo.join("foo.txt"), b"hi\n").unwrap();

    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&repo)
        .spawn()
        .unwrap();
    wait_for(&root.join("sock"), Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(300));

    // Submit job #1 and wait for it to finish (state leaves 'queued'/'running').
    zvcs(&repo, &["zcommit", "foo.txt", "-m", "add foo"]);
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let j = zvcs(&repo, &["zjob", "1"]).0;
        if j.contains("done") || j.contains("failed") {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // restart #1 → a new job is created and reported.
    let (out, err, ok) = zvcs(&repo, &["zjob", "restart", "1"]);
    let restarted = ok && out.contains("restarted as job #");
    // The new job (#2) shows up in the ledger.
    let mut has_two = false;
    let d2 = Instant::now() + Duration::from_secs(10);
    while Instant::now() < d2 {
        if zvcs(&repo, &["zjobs"]).0.contains("#2") {
            has_two = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // stop a finished job → reported as not stoppable (non-zero).
    let (_o, stop_err, stop_ok) = zvcs(&repo, &["zjob", "stop", "1"]);

    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&repo).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(restarted, "restart did not report a new job; stdout:{out} stderr:{err}");
    assert!(has_two, "restarted job #2 never appeared in the ledger");
    assert!(
        !stop_ok && stop_err.contains("no stoppable"),
        "stop of a finished job should be reported non-stoppable; stderr:\n{stop_err}"
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
    panic!("daemon socket never appeared");
}
