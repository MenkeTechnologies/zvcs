//! Hooks receive a *typed* event: a commit fires the hook with
//! `ZVCS_EVENT=commit` (and old/new SHA), enabling cross-repo reactive rules.

use std::path::Path;
use std::process::{Child, Command, Stdio};
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

    // Index the repo into the ledger BEFORE configuring the hook. Every `git`
    // invocation autostarts the singleton daemon when `[zvcs]` autonomy/hooks are
    // configured (lib::run → autostart, which fires *before* the subcommand runs).
    // If `zvcs.hook` were already set, `zreindex` would autostart a daemon that
    // races its own ledger write: the daemon does a ONE-SHOT `build_targets` at
    // start, so if it wins the race it watches zero repos and never re-scans, and
    // the explicit `zdaemon start` below then bails "already running" — nothing
    // ever watches the repo and the hook never fires. Locally the ledger write
    // wins; on CI the daemon wins, deterministically. Indexing first (no hook set
    // yet ⇒ no autostart) makes the explicit start below the sole daemon, and it
    // reads a fully-committed ledger. Do NOT hoist the hook config above this line.
    assert!(Command::new(BIN).args(["zreindex", "--sync", repo.to_str().unwrap()]).current_dir(&repo).status().unwrap().success());

    let marker = root.join("event.txt");
    let hook = format!("printf '%s %s' \"$ZVCS_EVENT\" \"$ZVCS_NEW_SHA\" > {}", marker.display());
    git(&repo, &["config", "zvcs.hook", &hook]);
    git(&repo, &["config", "zvcs.interval", "1"]);

    // Start the daemon with its output captured so we can wait for it to actually
    // begin watching. `notify` does not replay events that predate the watch, so
    // the trigger commit MUST happen after the watcher is established — a fixed
    // sleep is a race on a slow runner.
    let daemon_log = root.join("daemon.log");
    let logf = std::fs::File::create(&daemon_log).unwrap();
    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&repo)
        .stdout(Stdio::from(logf.try_clone().unwrap()))
        .stderr(Stdio::from(logf))
        .spawn()
        .unwrap();
    wait_for(&root.join("sock"), Duration::from_secs(5));
    // Block until the watch loop has set up its watches (printed only after every
    // `watcher.watch()` call in superset::watch::run). Best-effort: on timeout we
    // proceed and let the 20s event poll below be the real assertion.
    wait_for_log(&daemon_log, "[zvcs watch] watching", Duration::from_secs(10));

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

/// Poll `log` until it contains `needle`, or `timeout` elapses. Best-effort: it
/// does not panic on timeout — the caller's downstream assertion is the real gate.
/// Used to confirm the daemon's watcher is established before we mutate refs.
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
