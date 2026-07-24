//! `[zvcs] autocrawl` runs a background repo crawl on daemon start, populating
//! the index without an explicit `git zreindex`.

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
fn autocrawl_on_start_indexes_configured_roots() {
    let root = std::env::temp_dir().join(format!("zvcs-autocrawl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::env::set_var("ZVCS_SOCK", root.join("sock"));
    std::env::set_var("ZVCS_HOME", root.join("home"));

    // A separate tree to crawl, with two repos.
    let crawl = root.join("crawl");
    for name in ["alpha", "beta"] {
        let r = crawl.join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
    }

    // The daemon's own repo, configured to auto-crawl the tree above.
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "zvcs.autocrawl", "true"]);
    git(&repo, &["config", "zvcs.crawlroots", crawl.to_str().unwrap()]);

    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&repo)
        .spawn()
        .unwrap();
    wait_for(&root.join("sock"), Duration::from_secs(5));

    // Poll zrepos until the crawl has recorded both repos.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut found = false;
    while Instant::now() < deadline {
        let out = Command::new(BIN)
            .args(["zrepos"])
            .current_dir(&repo)
            .output()
            .unwrap();
        let s = String::from_utf8_lossy(&out.stdout);
        if s.contains("alpha") && s.contains("beta") {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&repo).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(found, "autocrawl did not index the configured roots");
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
