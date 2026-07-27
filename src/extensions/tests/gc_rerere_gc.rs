//! `gc` and `maintenance run --task=rerere-gc` must actually collect `rr-cache`.
//!
//! Both delegate to the `rerere` port, which is handed the arguments its verb was
//! dispatched with — the verb is not one of them. Passing a leading "rerere" makes
//! the first positional read as an unknown subcommand, so the run printed git's
//! `usage: git rerere […]` block and collected nothing, while `gc` reported
//! success. git prints nothing here and prunes the cache.
//!
//! A preimage with no postimage dates an unresolved conflict, which
//! `gc.rerereUnresolved` expires after 15 days; the fixture backdates one well
//! past that, so a run that reaches the collector must delete it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new(BIN).args(args).current_dir(dir).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A one-commit repository with rerere on and a single `rr-cache` entry whose
/// preimage is 100 days old — expired under any default.
fn fixture(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-rrgc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    git(&repo, &["config", "rerere.enabled", "true"]);
    std::fs::write(repo.join("f"), "v1\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c1"]);

    let entry = stale_entry(&repo);
    std::fs::create_dir_all(&entry).unwrap();
    let preimage = entry.join("preimage");
    std::fs::write(&preimage, "<<<<<<<\nours\n=======\ntheirs\n>>>>>>>\n").unwrap();
    let old = SystemTime::now() - Duration::from_secs(100 * 86400);
    std::fs::File::options()
        .write(true)
        .open(&preimage)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_accessed(old).set_modified(old))
        .unwrap();
    repo
}

fn stale_entry(repo: &Path) -> PathBuf {
    repo.join(".git/rr-cache/0123456789abcdef0123456789abcdef01234567")
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(BIN).args(args).current_dir(repo).output().unwrap()
}

/// The usage block is the tell: it only appears when the delegate rejected its
/// arguments, and it means nothing was collected.
fn assert_collected(repo: &Path, out: &Output, what: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stderr.contains("usage: git rerere") && !stdout.contains("usage: git rerere"),
        "{what} printed rerere's usage block, so its arguments were rejected:\n{stderr}{stdout}"
    );
    assert!(
        !stale_entry(repo).exists(),
        "{what} left the expired rr-cache entry in place, so the collector never ran"
    );
}

#[test]
fn gc_collects_the_rerere_cache() {
    let repo = fixture("gc");
    let out = run(&repo, &["gc", "-q"]);
    assert!(out.status.success(), "gc failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_collected(&repo, &out, "gc");
}

#[test]
fn maintenance_rerere_gc_collects_the_rerere_cache() {
    let repo = fixture("maint");
    let out = run(&repo, &["maintenance", "run", "--task=rerere-gc"]);
    assert!(
        out.status.success(),
        "maintenance run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_collected(&repo, &out, "maintenance run --task=rerere-gc");
}
