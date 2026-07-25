//! `autoreconcile` is documented as "this one + submodules": the daemon's
//! converge pass must fast-forward the TOP-LEVEL repo too, not only submodules.
//! Here the top repo is left behind its (already-fetched) origin/main and the
//! daemon must ff it on startup.
//!
//! Everything runs against a private `ZVCS_HOME`. Sharing the developer's real
//! one made this test depend on whether their own daemon happened to be running:
//! the singleton lock lives in the home, so a live daemon there meant the test's
//! `zdaemon start` bailed with "already running", nothing converged, and the
//! failure looked like a reconcile bug. It also wrote test rows into the
//! developer's ledger. The socket lives under a short `/tmp` root because a unix
//! socket path is capped near 104 bytes and the temp root is already most of it.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The isolated state this test runs against: its own home and socket, and a
/// pinned empty git config so the developer's `[zvcs]` switches cannot autostart
/// a second daemon beside the one under test.
struct Sandbox {
    home: PathBuf,
    sock: PathBuf,
}

impl Sandbox {
    fn apply(&self, cmd: &mut Command) {
        cmd.env("ZVCS_HOME", &self.home)
            .env("ZVCS_SOCK", &self.sock)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null");
    }
}

fn git(sb: &Sandbox, dir: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(BIN);
    cmd.args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
        .args(args)
        .current_dir(dir);
    sb.apply(&mut cmd);
    let out = cmd.output().unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn head(sb: &Sandbox, dir: &Path) -> String {
    String::from_utf8(git(sb, dir, &["rev-parse", "HEAD"]).stdout).unwrap().trim().to_string()
}

#[test]
fn daemon_converge_fast_forwards_top_level_repo() {
    let root = std::env::temp_dir().join(format!("zvcs-rectop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    // Short socket root: `root` itself is already close to the sun_path cap.
    let sock_root = PathBuf::from(format!("/tmp/zvcs-rectop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sock_root);
    std::fs::create_dir_all(&sock_root).unwrap();
    let sb = Sandbox { home: root.join("home"), sock: sock_root.join("s") };
    std::fs::create_dir_all(&sb.home).unwrap();

    let bare = root.join("remote.git");
    git(&sb, &root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    git(&sb, &root, &["clone", "-q", bare.to_str().unwrap(), "top"]);
    let top = root.join("top");
    git(&sb, &top, &["checkout", "-q", "-B", "main"]);
    git(&sb, &top, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    git(&sb, &top, &["push", "-q", "origin", "main"]);
    let c0 = head(&sb, &top);

    // Advance origin/main to c1 from a second clone.
    git(&sb, &root, &["clone", "-q", bare.to_str().unwrap(), "other"]);
    let other = root.join("other");
    git(&sb, &other, &["checkout", "-q", "main"]);
    git(&sb, &other, &["commit", "--allow-empty", "-q", "-m", "c1"]);
    git(&sb, &other, &["push", "-q", "origin", "main"]);
    let c1 = head(&sb, &other);
    assert_ne!(c0, c1);

    // Fetch into `top` so refs/remotes/origin/main = c1 while HEAD stays at c0 —
    // the clean "behind" state the daemon reconciles (fetch-free).
    git(&sb, &top, &["fetch", "-q", "origin"]);
    assert_eq!(head(&sb, &top), c0, "precondition: top still at c0 after fetch");

    git(&sb, &top, &["config", "zvcs.autoreconcile", "true"]);
    git(&sb, &top, &["config", "zvcs.interval", "1"]);

    let mut daemon_cmd = Command::new(BIN);
    daemon_cmd.args(["zdaemon", "start", "--foreground"]).current_dir(&top);
    sb.apply(&mut daemon_cmd);
    let mut daemon: Child = daemon_cmd.spawn().expect("spawn zdaemon");

    // The startup converge (react) must ff the top-level to c1.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut reconciled = false;
    while Instant::now() < deadline {
        if head(&sb, &top) == c1 {
            reconciled = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let mut stop = Command::new(BIN);
    stop.args(["zdaemon", "stop"]).current_dir(&top);
    sb.apply(&mut stop);
    let _ = stop.status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let final_head = head(&sb, &top);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&sock_root);

    assert!(reconciled, "daemon must fast-forward the TOP-LEVEL repo to origin/main (still at {final_head}, wanted {c1})");
}
