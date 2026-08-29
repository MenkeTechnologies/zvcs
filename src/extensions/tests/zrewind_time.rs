//! `git zrewind <duration>` — restore the tree to a wall-clock time.
//!
//! The third destructive verb in the superset, and the one whose default points
//! the *opposite* way to its closest neighbour:
//!
//!     git zrewind 2h              resets --hard, now
//!     git zrewind 2h --dry-run    previews
//!     git zrollback               previews
//!     git zrollback --apply       resets --hard
//!
//! Two verbs that both end in `reset --hard` across a tree, with inverted
//! defaults. Nothing enforces that split but the code, so it is written down
//! here and in `zfleet_dry_run.rs` from the other side.
//!
//! The cases control the clock rather than waiting on it: each commit is made
//! with `GIT_COMMITTER_DATE` set to a chosen offset from now, which is what
//! stamps the reflog entries `zrewind` reads. That makes "the HEAD this repo
//! had an hour ago" an exact, reproducible question instead of a timing race.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Run with the clock pinned at `now - ago` seconds, so the reflog entry this
/// command writes carries that timestamp.
fn at(dir: &Path, home: &Path, ago: u64, args: &[&str]) -> Output {
    let stamp = format!("{} +0000", now() - ago);
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", &stamp)
        .env("GIT_COMMITTER_DATE", &stamp)
        .output()
        .unwrap()
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    at(dir, home, 0, args)
}

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn both(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

fn subject(repo: &Path, home: &Path) -> String {
    ok(&run(repo, home, &["log", "-1", "--format=%s"]), "log").trim().to_string()
}

/// A repository with one commit three hours old and one five minutes old.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zrewind-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&run(&repo, &home, &["init", "-q", "-b", "main", "."]), "init");
    std::fs::write(repo.join("a.txt"), b"old\n").unwrap();
    ok(&at(&repo, &home, 10_800, &["add", "a.txt"]), "add");
    ok(&at(&repo, &home, 10_800, &["commit", "-q", "-m", "three hours ago"]), "old commit");
    std::fs::write(repo.join("a.txt"), b"new\n").unwrap();
    ok(&at(&repo, &home, 300, &["commit", "-qam", "five minutes ago"]), "recent commit");
    (repo, home)
}

#[test]
fn dry_run_names_the_target_and_moves_nothing() {
    let (repo, home) = fixture("dry");
    let before = subject(&repo, &home);
    assert_eq!(before, "five minutes ago");

    let out = both(&run(&repo, &home, &["zrewind", "1h", "--dry-run"]));
    assert!(out.contains("would rewind"), "the preview does not say what it would do:\n{out}");
    assert_eq!(subject(&repo, &home), before, "a dry run rewound the repository");
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "new\n", "a dry run touched the worktree");

    // `-n` is the same switch.
    let _ = run(&repo, &home, &["zrewind", "1h", "-n"]);
    assert_eq!(subject(&repo, &home), before, "-n rewound the repository");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_rewind_lands_on_the_head_the_repo_had_at_that_time() {
    let (repo, home) = fixture("apply");
    // An hour ago the repo was still at the three-hours-ago commit.
    let out = both(&run(&repo, &home, &["zrewind", "1h"]));
    assert!(out.contains("rewound"), "{out}");
    assert_eq!(subject(&repo, &home), "three hours ago", "landed on the wrong commit");
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "old\n",
        "the worktree was not restored with the ref"
    );

    // The rewind is itself in the reflog, so it can be undone in turn — the
    // property that makes a destructive verb recoverable rather than final.
    let reflog = ok(&run(&repo, &home, &["reflog"]), "reflog");
    assert!(reflog.contains("reset"), "the rewind left no reflog entry to undo:\n{reflog}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_dirty_repository_is_refused_and_keeps_its_uncommitted_work() {
    // "Refuses a dirty repo — uncommitted work is never clobbered" is the whole
    // safety story of a verb that ends in `reset --hard`, so the assertion is on
    // the bytes in the file, not on the message.
    let (repo, home) = fixture("dirty");
    std::fs::write(repo.join("a.txt"), b"uncommitted\n").unwrap();
    let before = subject(&repo, &home);

    let out = both(&run(&repo, &home, &["zrewind", "1h"]));
    assert_eq!(subject(&repo, &home), before, "a dirty repository was rewound");
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "uncommitted\n",
        "uncommitted work was discarded"
    );
    assert!(
        out.contains("skip") || out.contains("dirty") || out.contains("0 rewound"),
        "the refusal was not reported:\n{out}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_time_before_the_reflog_begins_is_reported_rather_than_guessed() {
    // The reflog only reaches back three hours here. Asked for ten days, the
    // verb has no entry to land on: it must say so rather than pick the oldest
    // entry and call it the answer.
    let (repo, home) = fixture("far");
    let before = subject(&repo, &home);
    let out = both(&run(&repo, &home, &["zrewind", "10d"]));
    assert_eq!(subject(&repo, &home), before, "a request past the reflog moved HEAD anyway");
    assert!(
        out.contains("skip") || out.contains("0 rewound") || out.contains("no reflog"),
        "an out-of-range request was not reported:\n{out}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
