//! A repository indexed AFTER the daemon started must still get watched.
//!
//! The watch set used to be built once, at daemon startup, and never revisited.
//! Every repo discovered later — by the background crawler, by `zreindex`, by a
//! clone an hour into the session — was invisible until the daemon was
//! restarted, and its hooks simply never fired. Nothing reported that: the
//! daemon logged "watching 0 path(s)" once and then sat there looking healthy,
//! and the repo's hook was configured, correct, and dead.
//!
//! This test reproduces that exact order — daemon first, repo second — and
//! requires the hook to fire.
//!
//! The socket path lives under a SHORT temp root on purpose: a unix socket path
//! is capped near 104 bytes, and the usual `zvcs-<test>-<pid>` naming under
//! macOS's private temp dir is long enough to blow that cap.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run a zvcs command in `dir` with the test's isolated home and a pinned,
/// empty git config (so the developer's own `[zvcs]` switches cannot autostart a
/// second daemon or lend this test an identity it did not set).
fn zvcs(dir: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ZVCS_HOME", root.join("home"))
        .env("ZVCS_SOCK", root.join("s"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "zvcs-test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "zvcs-test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("run zvcs git")
}

/// Wait for `pred` to hold, polling until `budget` runs out. Returns whether it
/// ever held, so the caller can assert with its own message.
fn wait_until(budget: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn log_contains(log: &Path, needle: &str) -> bool {
    std::fs::read_to_string(log).is_ok_and(|t| t.contains(needle))
}

#[test]
fn a_repo_indexed_after_startup_is_picked_up_and_its_hook_fires() {
    let root = PathBuf::from(format!("/tmp/zvcs-rescan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    std::fs::create_dir_all(root.join("home")).expect("mkdir home");

    assert!(zvcs(&repo, &root, &["init", "-q", "-b", "main"]).status.success(), "init");
    assert!(
        zvcs(&repo, &root, &["commit", "--allow-empty", "-q", "-m", "root"]).status.success(),
        "root commit"
    );

    let marker = root.join("event.txt");
    let hook = format!("printf '%s' \"$ZVCS_EVENT\" > {}", marker.display());
    assert!(zvcs(&repo, &root, &["config", "zvcs.hook", &hook]).status.success(), "set hook");

    // The daemon comes up while the ledger is still empty, so it starts with an
    // empty watch set — the state that used to be permanent.
    let daemon_log = root.join("daemon.log");
    let logf = std::fs::File::create(&daemon_log).expect("create daemon log");
    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&repo)
        .env("ZVCS_HOME", root.join("home"))
        .env("ZVCS_SOCK", root.join("s"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdout(Stdio::from(logf.try_clone().expect("clone log handle")))
        .stderr(Stdio::from(logf))
        .spawn()
        .expect("spawn daemon");

    let up = wait_until(Duration::from_secs(20), || log_contains(&daemon_log, "[zvcs watch] watching"));
    assert!(up, "daemon never reported a watch set:\n{}", std::fs::read_to_string(&daemon_log).unwrap_or_default());

    // Now index the repo, exactly as the crawler or an explicit `zreindex` does
    // at any point in a daemon's life.
    assert!(
        zvcs(&repo, &root, &["zreindex", "--sync", repo.to_str().expect("utf-8 path")])
            .status
            .success(),
        "zreindex"
    );

    // The rescan must adopt it. Without one this waits out the full budget.
    let adopted = wait_until(Duration::from_secs(30), || log_contains(&daemon_log, "picked up"));

    // Only now commit: `notify` does not replay events that predate a watch, so
    // a commit made before the repo was adopted proves nothing either way.
    let fired = adopted
        && zvcs(&repo, &root, &["commit", "--allow-empty", "-q", "-m", "trigger"]).status.success()
        && wait_until(Duration::from_secs(20), || {
            std::fs::read_to_string(&marker).is_ok_and(|t| t.contains("commit"))
        });

    let log = std::fs::read_to_string(&daemon_log).unwrap_or_default();
    let _ = Command::new(BIN)
        .args(["zdaemon", "stop"])
        .current_dir(&repo)
        .env("ZVCS_HOME", root.join("home"))
        .env("ZVCS_SOCK", root.join("s"))
        .status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(adopted, "the daemon never picked up the repo indexed after it started:\n{log}");
    assert!(fired, "the adopted repo's hook did not fire on a commit:\n{log}");
}
