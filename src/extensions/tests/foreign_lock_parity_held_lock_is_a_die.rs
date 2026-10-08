//! A lock another process holds is a `die()` in stock git: `fatal: Unable to create
//! '<absolute>.lock': File exists.`, the holder paragraph, exit 128 (`unable_to_lock_die()`).
//! The port takes the same locks through gitoxide, whose acquisition error used to surface as
//! `zvcs: <verb>: Could not acquire lock …` at exit 1.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        // A contended zvcs verb queues itself as a job and exits 0; a queued re-run is the
        // one that reports the failure, which is the behaviour compared here.
        .env("ZVCS_QUEUED", "1")
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code())
}

fn fixture(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    run(BIN, root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("a"), "a\n").unwrap();
    run(BIN, root, &["add", "a"]);
    run(BIN, root, &["commit", "-qm", "one"]);
    std::fs::write(root.join("a"), "changed\n").unwrap();
    std::fs::write(root.join("b"), "b\n").unwrap();
}

#[test]
fn a_held_index_lock_is_fatal_at_128() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-held-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("s").join("repo"), base.join("z").join("repo"));
    fixture(&s);
    fixture(&z);
    let (s, z) = (s.canonicalize().unwrap(), z.canonicalize().unwrap());
    for root in [&s, &z] {
        std::fs::write(root.join(".git/index.lock"), "").unwrap();
    }
    let norm = |o: Outcome, root: &Path| (o.0.replace(root.to_str().unwrap(), "<root>"), o.1);
    for args in [
        &["add", "b"][..],
        &["rm", "--cached", "-q", "a"],
        &["reset", "-q", "--hard"],
        &["commit", "-q", "-a", "-m", "x"],
    ] {
        let want = norm(run(stock, &s, args), &s);
        let got = norm(run(BIN, &z, args), &z);
        assert_eq!(got.1, Some(128), "{args:?}: {got:?}");
        assert_eq!(got, want, "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
