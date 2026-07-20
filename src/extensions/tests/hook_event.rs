//! Hooks receive a *typed* event: a commit fires the hook with
//! `ZVCS_EVENT=commit` (and old/new SHA), enabling cross-repo reactive rules.

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

#[test]
fn hook_receives_typed_commit_event() {
    let root = std::env::temp_dir().join(format!("zvcs-hookev-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::env::set_var("ZVCS_SOCK", root.join("sock"));
    std::env::set_var("ZVCS_HOME", root.join("home"));

    let repo = root.join("watched");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "root"]);

    let marker = root.join("event.txt");
    let hook = format!("printf '%s %s' \"$ZVCS_EVENT\" \"$ZVCS_NEW_SHA\" > {}", marker.display());
    git(&repo, &["config", "zvcs.hook", &hook]);
    git(&repo, &["config", "zvcs.interval", "1"]);

    assert!(Command::new(BIN).args(["zreindex", repo.to_str().unwrap()]).current_dir(&repo).status().unwrap().success());

    let mut daemon: Child = Command::new(BIN).args(["zdaemon", "start"]).current_dir(&repo).spawn().unwrap();
    wait_for(&root.join("sock"), Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(800));

    git(&repo, &["commit", "--allow-empty", "-q", "-m", "trigger"]);

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut event = String::new();
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(&marker) {
            if s.contains("commit") {
                event = s;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&repo).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(event.contains("commit"), "hook did not get a typed commit event; got: {event:?}");
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
