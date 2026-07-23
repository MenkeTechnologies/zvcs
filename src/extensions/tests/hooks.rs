//! End-to-end hook mechanism: with `[zvcs] hook` set and repos indexed in the
//! db, the daemon watches them and runs the per-repo hook on a ref change — no
//! `.git/hooks` files installed. Here the hook writes a marker containing
//! `$ZVCS_REPO`; committing in the watched repo must produce it.

use std::path::Path;
use std::process::{Child, Command, Stdio};
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

    // Index the repo BEFORE configuring the hook. Every `git` invocation
    // autostarts the singleton daemon when `[zvcs]` autonomy/hooks are configured
    // (lib::run → autostart, which fires *before* the subcommand runs). If
    // `zvcs.hook` were already set, `zreindex` would autostart a daemon that races
    // its own ledger write: the daemon does a ONE-SHOT `build_targets` at start, so
    // if it wins the race it watches zero repos and never re-scans, and the
    // explicit `zdaemon start` below then bails "already running" — nothing watches
    // the repo and the hook never fires. Locally the ledger write wins; on CI the
    // daemon wins, deterministically. Indexing first (no hook set yet ⇒ no
    // autostart) makes the explicit start below the sole daemon, reading a
    // fully-committed ledger. Do NOT hoist the hook config above this line.
    let ok = Command::new(BIN)
        .args(["zreindex", "--sync", repo.to_str().unwrap()])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success();
    assert!(ok, "zreindex failed");

    // Hook writes the repo path into a marker; short debounce for the test.
    let marker = root.join("hook-ran.txt");
    let hook_cmd = format!("printf '%s' \"$ZVCS_REPO\" > {}", marker.display());
    git(&repo, &["config", "zvcs.hook", &hook_cmd]);
    git(&repo, &["config", "zvcs.interval", "1"]);

    // Start the daemon with its output captured so we can wait for it to actually
    // begin watching. `notify` does not replay events that predate the watch, so
    // the trigger commit MUST happen after the watcher is established — a fixed
    // sleep is a race on a slow runner.
    let daemon_log = root.join("daemon.log");
    let logf = std::fs::File::create(&daemon_log).unwrap();
    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start"])
        .current_dir(&repo)
        .stdout(Stdio::from(logf.try_clone().unwrap()))
        .stderr(Stdio::from(logf))
        .spawn()
        .unwrap();
    wait_for(&root.join("sock"), Duration::from_secs(5));
    // Block until the watch loop has set up its watches (printed only after every
    // `watcher.watch()` call in superset::watch::run). Best-effort: on timeout we
    // proceed and let the 20s marker poll below be the real assertion.
    wait_for_log(&daemon_log, "[zvcs watch] watching", Duration::from_secs(10));

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
