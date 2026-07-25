//! End-to-end test of the async `zcommit` queue: submitting to the daemon
//! returns a job number immediately, the daemon executes the commit off the
//! client's path, and the ledger records the finished job.

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new(BIN)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=zvcs-test"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "zvcs-test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "zvcs-test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

fn zvcs(cwd: &Path, args: &[&str]) -> (String, String) {
    // Pinned global config: the developer's own `[zvcs]` switches would autostart
    // a SECOND daemon beside the one this test starts on purpose, and the two
    // then race for the same socket.
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "zvcs-test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "zvcs-test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn zcommit_is_queued_executed_and_recorded() {
    let root = std::env::temp_dir().join(format!("zvcs-queue-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    // Isolate socket + ledger to this process (single test in this file).
    std::env::set_var("ZVCS_SOCK", root.join("sock"));
    std::env::set_var("ZVCS_HOME", root.join("home"));

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "root"]);
    std::fs::write(repo.join("foo.txt"), b"hi\n").unwrap();

    // Bring up the daemon (handles SUBMIT regardless of [zvcs] autonomy).
    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "zvcs-test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "zvcs-test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .spawn()
        .unwrap();
    wait_for(&root.join("sock"), Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(300));

    // Submit the commit — returns a job number on stderr, immediately.
    let (_, err) = zvcs(&repo, &["zcommit", "foo.txt", "-m", "add foo"]);
    assert!(
        err.contains("queued job #"),
        "zcommit did not report a queued job; stderr:\n{err}"
    );

    // Poll until the daemon has executed it: foo.txt committed on main.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut committed = false;
    while Instant::now() < deadline {
        let log = zvcs(&repo, &["log", "--oneline", "-3"]).0;
        let tracked = zvcs(&repo, &["ls-files"]).0;
        if log.contains("add foo") && tracked.contains("foo.txt") {
            committed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    // The ledger records THIS job as done. The id comes from what `zcommit`
    // reported, not a hardcoded #1: the daemon queues jobs of its own (the status
    // maintainer is a long-lived one), so the first id is not necessarily ours.
    let id = err
        .split("queued job #")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .filter(|s| !s.is_empty())
        .expect("zcommit reported a job id")
        .to_string();
    let job = zvcs(&repo, &["zjob", &id]).0;

    let _ = Command::new(BIN)
        .args(["zdaemon", "stop"])
        .current_dir(&repo)
        .status();
    let _ = daemon.kill();
    let _ = daemon.wait();

    let final_log = zvcs(&repo, &["log", "--oneline", "-3"]).0;
    let _ = std::fs::remove_dir_all(&root);

    assert!(committed, "zcommit job never landed; log:\n{final_log}");
    assert!(
        job.contains("done"),
        "ledger did not record job #{id} as done; zjob {id}:\n{job}"
    );
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
