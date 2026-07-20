//! Daemon control verbs: ping / info / restart / stop.

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn ctl(home: &Path, sock: &Path, cwd: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("ZVCS_HOME", home)
        .env("ZVCS_SOCK", sock)
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.success())
}

fn wait_for(sock: &Path, up: bool, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if sock.exists() == up {
            return true;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    false
}

#[test]
fn daemon_control_lifecycle() {
    let root = std::env::temp_dir().join(format!("zvcs-ctl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let sock = root.join("sock");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&repo).status().unwrap().success());

    // ping before start → not running (non-zero).
    let (out, ok) = ctl(&home, &sock, &repo, &["zdaemon", "ping"]);
    assert!(!ok && out.contains("not running"), "ping before start: {out}");

    // start (detached child), wait for the socket.
    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start"])
        .current_dir(&repo)
        .env("ZVCS_HOME", &home)
        .env("ZVCS_SOCK", &sock)
        .spawn()
        .unwrap();
    assert!(wait_for(&sock, true, Duration::from_secs(5)), "socket never appeared");

    // ping → running; info → shows running + pid.
    let (p, pok) = ctl(&home, &sock, &repo, &["zdaemon", "ping"]);
    assert!(pok && p.contains("running"), "ping after start: {p}");
    let (info, _) = ctl(&home, &sock, &repo, &["zdaemon", "info"]);
    assert!(info.contains("running: true"), "info running: {info}");
    assert!(info.contains("pid:"), "info pid: {info}");

    // restart → stops the old daemon and brings up a fresh one.
    let (r, rok) = ctl(&home, &sock, &repo, &["zdaemon", "restart"]);
    assert!(rok && r.contains("restarted"), "restart: {r}");
    let _ = daemon.wait(); // old child was STOP'd
    assert!(wait_for(&sock, true, Duration::from_secs(5)), "no daemon after restart");
    assert!(ctl(&home, &sock, &repo, &["zdaemon", "ping"]).1, "ping after restart");

    // stop → gone.
    let (_s, _) = ctl(&home, &sock, &repo, &["zdaemon", "stop"]);
    assert!(wait_for(&sock, false, Duration::from_secs(5)), "socket must be gone after stop");
    assert!(!ctl(&home, &sock, &repo, &["zdaemon", "ping"]).1, "ping after stop must be non-zero");

    let _ = daemon.kill();
    let _ = std::fs::remove_dir_all(&root);
}
