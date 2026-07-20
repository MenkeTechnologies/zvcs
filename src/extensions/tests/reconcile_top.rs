//! `autoreconcile` is documented as "this one + submodules": the daemon's
//! converge pass must fast-forward the TOP-LEVEL repo too, not only submodules.
//! Here the top repo is left behind its (already-fetched) origin/main and the
//! daemon must ff it on startup.

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn head(dir: &Path) -> String {
    String::from_utf8(git(dir, &["rev-parse", "HEAD"]).stdout).unwrap().trim().to_string()
}

#[test]
fn daemon_converge_fast_forwards_top_level_repo() {
    let root = std::env::temp_dir().join(format!("zvcs-rectop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    let bare = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "top"]);
    let top = root.join("top");
    git(&top, &["checkout", "-q", "-B", "main"]);
    git(&top, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    git(&top, &["push", "-q", "origin", "main"]);
    let c0 = head(&top);

    // Advance origin/main to c1 from a second clone.
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "other"]);
    let other = root.join("other");
    git(&other, &["checkout", "-q", "main"]);
    git(&other, &["commit", "--allow-empty", "-q", "-m", "c1"]);
    git(&other, &["push", "-q", "origin", "main"]);
    let c1 = head(&other);
    assert_ne!(c0, c1);

    // Fetch into `top` so refs/remotes/origin/main = c1 while HEAD stays at c0 —
    // the clean "behind" state the daemon reconciles (fetch-free).
    git(&top, &["fetch", "-q", "origin"]);
    assert_eq!(head(&top), c0, "precondition: top still at c0 after fetch");

    git(&top, &["config", "zvcs.autoreconcile", "true"]);
    git(&top, &["config", "zvcs.interval", "1"]);

    let sock = root.join("zvcs-test.sock");
    std::env::set_var("ZVCS_SOCK", &sock);
    let mut daemon: Child = Command::new(BIN).args(["zdaemon", "start"]).current_dir(&top).spawn().expect("spawn zdaemon");

    // The startup converge (react) must ff the top-level to c1.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut reconciled = false;
    while Instant::now() < deadline {
        if head(&top) == c1 {
            reconciled = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&top).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let final_head = head(&top);
    let _ = std::fs::remove_dir_all(&root);

    assert!(reconciled, "daemon must fast-forward the TOP-LEVEL repo to origin/main (still at {final_head}, wanted {c1})");
}
