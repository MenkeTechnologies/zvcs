//! The daemon's background status maintainer must keep `repo_status` warm for
//! every indexed repo on its own — without `[zvcs] autostatus` — so `zdashboard`
//! and `zstatus --all` are instant *and* accurate. This starts a daemon over a
//! two-repo index and asserts the cache fills in on its own.

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").args(args).current_dir(dir).status().unwrap().success(), "git {args:?}");
}

fn zvcs(home: &Path, sock: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).env("ZVCS_HOME", home).env("ZVCS_SOCK", sock).output().unwrap();
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

#[test]
fn daemon_keeps_status_cache_warm() {
    let root = std::env::temp_dir().join(format!("zvcs-statusd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let home = root.join("home");
    // Short socket path: a long one exceeds the unix-socket SUN_LEN limit.
    let sock = std::env::temp_dir().join(format!("zsd-{}.sock", std::process::id()));

    for name in ["alpha", "beta"] {
        let repo = work.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("f"), "hi\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["-c", "user.email=t@e.x", "-c", "user.name=t", "commit", "-qm", "c1"]);
    }
    std::fs::write(work.join("beta/f"), "hi\ndirty\n").unwrap(); // beta tracked-dirty

    assert!(zvcs(&home, &sock, &["zreindex", "--sync", work.to_str().unwrap()]).contains("indexed 2"));

    // Start the daemon (spawns the status maintainer, no autostatus needed). A
    // daemon may also autostart from a global `[zvcs] hook`; either way one runs
    // its statusd — a duplicate start just bails "already running", harmlessly.
    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start"])
        .env("ZVCS_HOME", &home)
        .env("ZVCS_SOCK", &sock)
        .spawn()
        .unwrap();

    // The maintainer should fill the cache on its own within a few seconds.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut warmed = false;
    while Instant::now() < deadline {
        let all = zvcs(&home, &sock, &["zstatus", "--all"]);
        if all.contains("alpha") && all.contains("beta") {
            // beta must be reported dirty — proof it computed real status.
            assert!(all.lines().any(|l| l.contains("beta") && l.contains("dirty")), "beta dirty in cache:\n{all}");
            warmed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let _ = zvcs(&home, &sock, &["zdaemon", "stop"]);
    let _ = daemon.wait();
    let _ = daemon.kill();
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&sock);

    assert!(warmed, "the daemon's status maintainer never populated the cache");
}
