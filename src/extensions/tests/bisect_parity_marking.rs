//! What `git bisect good|bad <rev>` leaves on disk *before* it finds out the
//! marking cannot be bisected, pinned against stock git 2.55.0.
//!
//! `bisect_state()` (builtin/bisect.c:1004-1016) writes each marking — the
//! `refs/bisect/…` ref and both `BISECT_LOG` lines — and only then calls
//! `bisect_auto_next()`, which is where a commit that now sits on both sides of
//! the search is noticed (`<oid> was both '<good>' and '<bad>'`, bisect.c:1093).
//! A port that checks for the clash first answers with the same line and the same
//! exit code while silently dropping the ref and the log lines, so the session it
//! leaves behind is not the one stock git leaves and `git bisect log` replays
//! differently.
//!
//! The same loop carries git's `verify_expected` invalidation: marking anything
//! other than the commit the last step asked for removes `BISECT_EXPECTED_REV`
//! *and* `BISECT_ANCESTORS_OK`, so the next step re-runs the merge-base checks
//! instead of trusting a flag file written before the marking.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run_at(dir: &Path, home: &Path, args: &[&str], date: Option<&str>) -> Output {
    let stamp = date.unwrap_or("1700000000 +0000");
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", stamp)
        .env("GIT_COMMITTER_DATE", stamp)
        .output()
        .expect("run binary")
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    run_at(dir, home, args, None)
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let o = run(dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// A linear history with strictly increasing commit dates, so the revision walk
/// has no ties to break and every step is reproducible.
fn fixture(tag: &str, n: usize) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-bpm-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    for i in 1..=n {
        std::fs::write(repo.join(format!("f{i}")), "x\n").unwrap();
        let date = format!("{} +0000", 1_700_000_000 + i * 60);
        git(&repo, &home, &["add", "-A"]);
        let msg = format!("c{i}");
        let o = run_at(&repo, &home, &["commit", "-q", "-m", &msg], Some(&date));
        assert!(o.status.success(), "commit {msg}: {}", String::from_utf8_lossy(&o.stderr));
        git(&repo, &home, &["tag", &msg]);
    }
    (root, repo, home)
}

fn rev(dir: &Path, home: &Path, spec: &str) -> String {
    String::from_utf8_lossy(&run(dir, home, &["rev-parse", spec]).stdout)
        .trim()
        .to_string()
}

/// Marking a commit that is already on the other side is refused — but the ref
/// and the two log lines are written first, so `git bisect log` still shows the
/// marking that caused the refusal.
#[test]
fn a_contradictory_marking_is_logged_before_it_is_refused() {
    let (root, repo, home) = fixture("clash", 15);
    let c10 = rev(&repo, &home, "c10");

    git(&repo, &home, &["bisect", "start", "c15", "c1"]);
    git(&repo, &home, &["bisect", "good", "c10"]);

    let bad = run(&repo, &home, &["bisect", "bad", "c10"]);
    assert_eq!(bad.status.code(), Some(1), "{:?}", bad.status);
    // The verdict is on stdout, and it names the commit that ended up on both
    // sides rather than the one the step was about to test.
    assert_eq!(
        String::from_utf8_lossy(&bad.stdout),
        format!("{c10} was both 'good' and 'bad'\n")
    );

    // `bisect_write()` ran before `bisect_auto_next()` noticed anything: the bad
    // ref moved and the log grew by the pair of lines.
    assert_eq!(
        std::fs::read_to_string(repo.join(".git/refs/bisect/bad")).unwrap().trim(),
        c10
    );
    let log = std::fs::read_to_string(repo.join(".git/BISECT_LOG")).unwrap();
    assert!(log.ends_with(&format!("# bad: [{c10}] c10\ngit bisect bad {c10}\n")), "{log}");

    let _ = std::fs::remove_dir_all(&root);
}

/// Marking a commit other than the one the last step asked for drops both cached
/// answers, which is what lets the next step notice that the marks have become
/// inconsistent instead of bisecting on a stale `BISECT_ANCESTORS_OK`.
#[test]
fn marking_off_the_expected_rev_clears_the_cached_answers() {
    let (root, repo, home) = fixture("expected", 15);

    git(&repo, &home, &["bisect", "start", "c15", "c1"]);
    // The first step wrote both files: the commit it asked for, and the note that
    // the goods were checked against the bad end.
    assert_eq!(
        std::fs::read_to_string(repo.join(".git/BISECT_EXPECTED_REV")).unwrap().trim(),
        rev(&repo, &home, "c8")
    );
    assert!(repo.join(".git/BISECT_ANCESTORS_OK").exists());

    // c10 is not the commit under test, so both go away — and the step that
    // follows writes a fresh pair for the commit *it* picked.
    git(&repo, &home, &["bisect", "good", "c10"]);
    assert_eq!(
        std::fs::read_to_string(repo.join(".git/BISECT_EXPECTED_REV")).unwrap().trim(),
        rev(&repo, &home, "c12")
    );
    assert!(repo.join(".git/BISECT_ANCESTORS_OK").exists());

    // Marking the already-good c1 as bad invalidates them again, and this time
    // the merge-base check that follows refuses — so nothing re-creates them, and
    // the refusal is the ancestry one rather than the empty-candidate-set one.
    let refusal = run(&repo, &home, &["bisect", "bad", "c1"]);
    assert_eq!(refusal.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&refusal.stderr),
        "Some 'good' revs are not ancestors of the 'bad' rev.\n\
         git bisect cannot work properly in this case.\n\
         Maybe you mistook 'good' and 'bad' revs?\n"
    );
    assert!(!repo.join(".git/BISECT_EXPECTED_REV").exists());
    assert!(!repo.join(".git/BISECT_ANCESTORS_OK").exists());

    let _ = std::fs::remove_dir_all(&root);
}
