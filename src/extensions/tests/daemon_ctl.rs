//! Daemon control verbs: ping / info / restart / stop.

use std::io::Write;
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
        .args(["zdaemon", "start", "--foreground"])
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

/// A bare `git zdaemon start` must daemonize — spawn the worker detached and
/// RETURN, never holding the terminal. `ctl` uses `Command::output`, which blocks
/// until the process exits, so if `start` ran the event loop in the foreground
/// this test would hang forever. It also proves the detached daemon actually came
/// up and can be stopped (no leak).
#[test]
fn bare_start_daemonizes_and_returns() {
    let root = std::env::temp_dir().join(format!("zvcs-daemonize-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let sock = root.join("sock");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&repo).status().unwrap().success());

    // Returns promptly (does not hang), and a daemon is now up.
    let (_out, ok) = ctl(&home, &sock, &repo, &["zdaemon", "start"]);
    assert!(ok, "bare `zdaemon start` should return success");
    assert!(wait_for(&sock, true, Duration::from_secs(5)), "detached daemon should be up");
    assert!(ctl(&home, &sock, &repo, &["zdaemon", "ping"]).1, "ping the detached daemon");

    // No Child handle (it is detached) — stop it via the socket so it doesn't leak.
    let _ = ctl(&home, &sock, &repo, &["zdaemon", "stop"]);
    assert!(wait_for(&sock, false, Duration::from_secs(5)), "detached daemon should stop");
    let _ = std::fs::remove_dir_all(&root);
}

/// A manual `git zdaemon stop` must STAY stopped: with `[zvcs]` autonomy enabled,
/// every `git` command runs autostart, so without a sticky disable a stop is
/// undone by the very next command (across up to 16 concurrent instances). The
/// stop sets a marker autostart honors; `start` clears it. Regression guard.
#[test]
fn manual_stop_disables_autostart_until_start() {
    let root = std::env::temp_dir().join(format!("zvcs-autostart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let sock = root.join("sock");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&repo).status().unwrap().success());
    // Enable autonomy directly in the repo config so a bare git command would
    // normally autostart the daemon. Written to the file to avoid depending on a
    // `git config` writer.
    let mut cfg = std::fs::OpenOptions::new().append(true).open(repo.join(".git/config")).unwrap();
    writeln!(cfg, "[zvcs]\n\tautostatus = true").unwrap();
    drop(cfg);

    let marker = home.join("zdaemon.disabled");

    // A bare git command autostarts the daemon.
    let _ = ctl(&home, &sock, &repo, &["rev-parse", "HEAD"]);
    assert!(wait_for(&sock, true, Duration::from_secs(5)), "autostart should bring the daemon up");

    // stop → daemon down, marker set, status reflects it.
    let _ = ctl(&home, &sock, &repo, &["zdaemon", "stop"]);
    assert!(wait_for(&sock, false, Duration::from_secs(5)), "daemon should stop");
    assert!(marker.exists(), "stop must set the autostart-disable marker");
    let (st, _) = ctl(&home, &sock, &repo, &["zdaemon", "status"]);
    assert!(st.contains("autostart disabled"), "status should note disabled: {st}");

    // THE REGRESSION: a git command after stop must NOT resurrect the daemon.
    let _ = ctl(&home, &sock, &repo, &["rev-parse", "HEAD"]);
    assert!(
        !wait_for(&sock, true, Duration::from_secs(2)),
        "stop must stick — autostart must not respawn the daemon while disabled"
    );

    // An explicit start clears the marker and brings the daemon back up.
    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&repo)
        .env("ZVCS_HOME", &home)
        .env("ZVCS_SOCK", &sock)
        .spawn()
        .unwrap();
    assert!(wait_for(&sock, true, Duration::from_secs(5)), "explicit start should bring the daemon up");
    assert!(!marker.exists(), "start must clear the disable marker");

    let _ = ctl(&home, &sock, &repo, &["zdaemon", "stop"]);
    let _ = daemon.wait();
    let _ = daemon.kill();
    let _ = std::fs::remove_dir_all(&root);
}
