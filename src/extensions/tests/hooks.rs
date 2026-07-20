//! End-to-end hook mechanism: with `[zvcs] hook` set and repos indexed in the
//! db, the daemon watches them and runs the per-repo hook on a ref change — no
//! `.git/hooks` files installed. Here the hook writes a marker containing
//! `$ZVCS_REPO`; committing in the watched repo must produce it.

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

#[test]
fn hook_fires_on_ref_change_in_watched_repo() {
    let root = std::env::temp_dir().join(format!("zvcs-hook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::env::set_var("ZVCS_SOCK", root.join("sock"));
    std::env::set_var("ZVCS_HOME", root.join("home"));

    let repo = root.join("watched");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "root"]);

    // Hook writes the repo path into a marker; short debounce for the test.
    let marker = root.join("hook-ran.txt");
    let hook_cmd = format!("printf '%s' \"$ZVCS_REPO\" > {}", marker.display());
    git(&repo, &["config", "zvcs.hook", &hook_cmd]);
    git(&repo, &["config", "zvcs.interval", "1"]);

    // Index the repo so the daemon watches it.
    let ok = Command::new(BIN)
        .args(["zreindex", repo.to_str().unwrap()])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success();
    assert!(ok, "zreindex failed");

    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start"])
        .current_dir(&repo)
        .spawn()
        .unwrap();
    wait_for(&root.join("sock"), Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(800));

    // A commit moves HEAD → the daemon fires the hook.
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "trigger"]);

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut fired = false;
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(&marker) {
            if s.contains("watched") {
                fired = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&repo).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(fired, "hook did not run on ref change (marker not written)");
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
