//! notify-on-next-command: a headless autonomous-op failure recorded in the
//! ledger must be surfaced on the operator's next `git` invocation (stderr),
//! exactly once. Async/daemon failures carry no exit code back, so this is their
//! only delivery channel.
//!
//! Single test in its own file so the process-global `ZVCS_HOME` it sets is not
//! raced by a sibling test.

use std::path::Path;
use std::process::Command;

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

/// stderr of the zvcs `git` binary run in `cwd`.
fn zvcs_stderr(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).current_dir(cwd).output().unwrap();
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn failure_is_surfaced_once_on_next_command() {
    let root = std::env::temp_dir().join(format!("zvcs-notify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    // Isolate ledger + socket to this process.
    let home = root.join("home");
    std::env::set_var("ZVCS_HOME", &home);

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "root"]);

    // Record a headless failure for this repo (as the daemon would).
    let gix_repo = gix::discover(&repo).unwrap();
    zvcs::db::record_failure(gix_repo.git_dir(), "autobump", "not a fast-forward").unwrap();

    // Next `git` command in the repo surfaces it once, on stderr.
    let first = zvcs_stderr(&repo, &["status"]);
    assert!(
        first.contains("zvcs: autobump failed: not a fast-forward"),
        "failure not surfaced on next command; stderr was:\n{first}"
    );

    // A subsequent command does NOT re-surface it (marked notified).
    let second = zvcs_stderr(&repo, &["status"]);
    assert!(
        !second.contains("autobump failed"),
        "failure was surfaced twice; stderr was:\n{second}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
