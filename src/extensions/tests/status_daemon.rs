//! With `[zvcs] autostatus`, the daemon populates every indexed repo's status on
//! start — so `git zstatus --all` is answered from the daemon-maintained cache
//! with no prior `git zstatus` call.

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

#[test]
fn daemon_populates_zstatus_all() {
    let root = std::env::temp_dir().join(format!("zvcs-statusd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::env::set_var("ZVCS_SOCK", root.join("sock"));
    std::env::set_var("ZVCS_HOME", root.join("home"));

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "root"]);

    // Index the repo so the daemon watches it; enable status maintenance.
    assert!(Command::new(BIN).args(["zreindex", repo.to_str().unwrap()]).current_dir(&repo).status().unwrap().success());
    git(&repo, &["config", "zvcs.autostatus", "true"]);

    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start"])
        .current_dir(&repo)
        .spawn()
        .unwrap();
    wait_for(&root.join("sock"), Duration::from_secs(5));

    // Poll zstatus --all — the daemon (not us) must have populated it.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut found = false;
    while Instant::now() < deadline {
        let out = Command::new(BIN).args(["zstatus", "--all"]).current_dir(&repo).output().unwrap();
        if String::from_utf8_lossy(&out.stdout).contains("repo") {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&repo).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(found, "daemon did not populate zstatus --all on start");
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
