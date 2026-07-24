//! Two round-8 fixes, proven together via a real async job:
//!  1. an async `zcommit --push` whose commit lands but push fails (send-pack is
//!     unimplemented) must still record the commit's sha (state=failed but sha
//!     present) — so a bot can tell a landed commit from a failed one.
//!  2. the job's child `git` must use the SUBMITTER's GIT_AUTHOR/COMMITTER env,
//!     not the daemon's — the daemon here has no identity, so a commit authored as
//!     `agentX` proves the env was carried into the spec and applied.

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(["-c", "user.email=repo@e.x", "-c", "user.name=repoDefault"])
            .args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn zjob1(home: &Path, sock: &Path, cwd: &Path) -> String {
    String::from_utf8_lossy(
        &Command::new(BIN).args(["zjob", "1"]).current_dir(cwd)
            .env("ZVCS_HOME", home).env("ZVCS_SOCK", sock).output().unwrap().stdout,
    ).into_owned()
}

#[test]
fn async_commit_carries_identity_and_records_sha_on_push_failure() {
    let root = std::env::temp_dir().join(format!("zvcs-asyncid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let sock = root.join("zvcs-test.sock");

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    let head0 = String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&repo).output().unwrap().stdout).unwrap().trim().to_string();

    // Daemon started with NO GIT_AUTHOR/COMMITTER identity in its environment.
    let mut daemon: Child = Command::new(BIN).args(["zdaemon", "start", "--foreground"]).current_dir(&repo)
        .env("ZVCS_HOME", &home).env("ZVCS_SOCK", &sock)
        .env_remove("GIT_AUTHOR_NAME").env_remove("GIT_COMMITTER_NAME")
        .spawn().unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) && !sock.exists() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(sock.exists(), "daemon socket never appeared");

    // Submit an async commit+push as agentX. --push will fail (send-pack stub).
    std::fs::write(repo.join("f.txt"), b"work\n").unwrap();
    let submit = Command::new(BIN)
        .args(["zcommit", "--push", "-m", "async work", "f.txt"])
        .current_dir(&repo)
        .env("ZVCS_HOME", &home).env("ZVCS_SOCK", &sock)
        .env("GIT_AUTHOR_NAME", "agentX").env("GIT_AUTHOR_EMAIL", "x@e.x")
        .env("GIT_COMMITTER_NAME", "agentX").env("GIT_COMMITTER_EMAIL", "x@e.x")
        .output().unwrap();
    let submitted = String::from_utf8_lossy(&submit.stderr);
    assert!(submitted.contains("queued job"), "expected an async queued job (daemon path): {submitted}");

    // Poll the job to completion.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut job = String::new();
    while Instant::now() < deadline {
        job = zjob1(&home, &sock, &repo);
        if job.contains("state:  done") || job.contains("state:  failed") {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let head_now = String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&repo).output().unwrap().stdout).unwrap().trim().to_string();
    let author = String::from_utf8(Command::new("git").args(["log", "-1", "--format=%an"]).current_dir(&repo).output().unwrap().stdout).unwrap().trim().to_string();

    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&repo).env("ZVCS_HOME", &home).env("ZVCS_SOCK", &sock).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);

    // The commit landed (HEAD moved) despite the push failing...
    assert_ne!(head_now, head0, "commit should have landed even though push fails");
    // ...the job is failed (push) but its sha was still recorded (Bug 1 fix)...
    assert!(job.contains("state:  failed"), "push failure → job failed:\n{job}");
    assert!(job.contains("sha:"), "the landed commit's sha must be recorded even on push failure:\n{job}");
    // ...and the commit is attributed to the submitter, not the identity-less daemon.
    assert_eq!(author, "agentX", "async commit must use the submitter's carried identity, got {author:?}");
}
