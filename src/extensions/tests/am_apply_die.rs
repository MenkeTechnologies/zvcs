//! git links `apply` into `am` (builtin/am.c:1539), so a `die()` inside it — a
//! bad `--attr-source` (attr.c:1226), a bad `-p` (apply.c:5043) — ends `git am`
//! at 128 on the spot: no `Patch failed at`, no advice. An error that
//! `apply_all_patches()` only returns (apply.c:5194), such as
//! `--whitespace=error` refusing the patch, leaves `git apply` at 128 as well but
//! reaches `am_run()`'s failure branch (builtin/am.c:1909-1916) instead. This port
//! runs `apply` as a child, so the two have to stay distinguishable; each case
//! here pins zvcs to stock git on exit code, stdout, stderr and the session
//! directory left behind.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo at `c0`, a mailbox holding the patch that adds `payload` to `f.txt`,
/// and an index whose stat data vouches for `f.txt` — so `am`'s own refresh
/// (builtin/am.c:1819) never hashes it and the run reaches `run_apply()`.
fn fixture(tag: &str, payload: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-amdie-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f.txt"), "line1\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("f.txt"), format!("line1\n{payload}\n")).unwrap();
    git(&repo, &["commit", "-q", "-am", "c1"]);
    let mbox = Command::new(BIN)
        .args(["format-patch", "-1", "--stdout"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(mbox.status.success(), "format-patch failed");
    std::fs::write(root.join("patch.mbox"), &mbox.stdout).unwrap();
    git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);

    // Age the file so the index written by the refresh is strictly newer than
    // it: the entry is then neither changed nor racily clean.
    let old = SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(repo.join("f.txt"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    git(&repo, &["update-index", "-q", "--really-refresh"]);
    (repo, home)
}

struct AmRun {
    out: Output,
    session: bool,
}

fn run_am(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> AmRun {
    let ra = repo.join(".git/rebase-apply");
    let _ = std::fs::remove_dir_all(&ra);
    let mbox = repo.parent().unwrap().join("patch.mbox");
    let out = Command::new(bin)
        .args(args)
        .arg(&mbox)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    let session = ra.is_dir();
    let _ = std::fs::remove_dir_all(&ra);
    AmRun { out, session }
}

fn assert_match(tag: &str, payload: &str, args: &[&str]) {
    let (repo, home) = fixture(tag, payload);
    let z = run_am(BIN, &repo, &home, args);
    let g = run_am("git", &repo, &home, args);
    let what = format!("{args:?}");
    assert_eq!(z.out.status.code(), g.out.status.code(), "{what}: exit code");
    assert_eq!(
        String::from_utf8_lossy(&z.out.stdout),
        String::from_utf8_lossy(&g.out.stdout),
        "{what}: stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&z.out.stderr),
        String::from_utf8_lossy(&g.out.stderr),
        "{what}: stderr"
    );
    assert_eq!(z.session, g.session, "{what}: session directory survives");
}

#[test]
fn bad_attr_source_dies_in_apply_without_failure_advice() {
    assert_match("attr", "line2", &["--attr-source=nosuch", "am"]);
}

#[test]
fn bad_attr_source_dies_in_the_silenced_threeway_attempt() {
    // Under `--3way` the first apply mutes only error and warning routines
    // (builtin/am.c:1531-1532, apply.c:183-188); the `fatal:` still prints and
    // no fallback is attempted.
    assert_match("attr3", "line2", &["--attr-source=nosuch", "am", "-3"]);
}

#[test]
fn bad_p_value_dies_in_apply_without_failure_advice() {
    assert_match("pval", "line2", &["am", "-p", "x"]);
}

#[test]
fn returned_apply_error_still_reports_the_failed_patch() {
    // `--whitespace=error` is `res = -128` inside `apply_all_patches()` (apply.c:5151-5157):
    // returned, not died, so `am` prints `Patch failed at` and its advice.
    assert_match("ws", "line2  ", &["am", "--whitespace=error"]);
}
