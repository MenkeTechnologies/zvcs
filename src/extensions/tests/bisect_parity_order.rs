//! The order `bisect_next_all()` walks its candidates in, and `bisect next`'s
//! argument count — both pinned against stock git 2.55.0.
//!
//! `bisect_rev_setup()` sets `revs.limited = 1` (bisect.c:1078), so
//! `prepare_revision_walk()` ends in `limit_list()`, whose `prio_queue` is
//! ordered by `compare_commits_by_commit_date`: `revs.commits` comes out newest
//! first. `find_bisection()` then reverses that list while counting
//! (bisect.c:414-430) and `best_bisection()` keeps the *first* commit reaching
//! the largest `min(weight, nr - weight)` (bisect.c:202-205, a strict `>`).
//!
//! On a linear history every order agrees, so the walk order is invisible. On a
//! merge it is not: two candidates can share the best distance, and then the
//! commit that is offered to the user is whichever the walk reached first. A
//! graph-shaped traversal picks the other one, with the same "N revisions left"
//! line in front of it — so only the `[<oid>] <subject>` line gives it away.

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

/// The subject of the commit the step just checked out, from its own
/// `[<oid>] <subject>` line.
fn tested(text: &str) -> String {
    text.lines()
        .find(|l| l.starts_with('['))
        .and_then(|l| l.split_once("] "))
        .map(|(_, s)| s.to_string())
        .unwrap_or_default()
}

/// A history where the candidate set has two equally good members whose commit
/// dates disagree with the shape of the graph:
///
/// ```text
/// a1 ── a2 ───────── m1 ── a3      (main)
///   └─ b1 ─ b2 ─ b3 ─┘             (side)
/// ```
///
/// Bisecting `a3` against `b2` leaves `a3`, `m1`, `b3` and `a2`. `b3` and `a2`
/// both have weight 1, so both are equally good — and `a2` is the older of the
/// two, so a date-ordered walk reversed puts it first.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-bpo-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    git(&repo, &home, &["init", "-q", "-b", "main"]);

    let mut clock = 1_700_000_000;
    let commit = |name: &str, clock: &mut i64| {
        std::fs::write(repo.join(name), "x\n").unwrap();
        *clock += 60;
        git(&repo, &home, &["add", "-A"]);
        let date = format!("{clock} +0000");
        let o = run_at(&repo, &home, &["commit", "-q", "-m", name], Some(&date));
        assert!(o.status.success(), "commit {name}: {}", String::from_utf8_lossy(&o.stderr));
        git(&repo, &home, &["tag", name]);
    };

    commit("a1", &mut clock);
    commit("a2", &mut clock);
    git(&repo, &home, &["checkout", "-q", "-b", "side", "a1"]);
    commit("b1", &mut clock);
    commit("b2", &mut clock);
    commit("b3", &mut clock);
    git(&repo, &home, &["checkout", "-q", "main"]);
    clock += 60;
    let date = format!("{clock} +0000");
    let o = run_at(&repo, &home, &["merge", "-q", "--no-ff", "-m", "m1", "side"], Some(&date));
    assert!(o.status.success(), "merge: {}", String::from_utf8_lossy(&o.stderr));
    git(&repo, &home, &["tag", "m1"]);
    commit("a3", &mut clock);

    (root, repo, home)
}

/// Two candidates are equally good; the one the date-ordered walk reached first
/// is the one that gets tested.
#[test]
fn equally_good_candidates_are_broken_by_the_commit_date_walk() {
    let (root, repo, home) = fixture("tie");

    let start = run(&repo, &home, &["bisect", "start", "a3", "b2"]);
    assert!(start.status.success(), "{}", String::from_utf8_lossy(&start.stderr));
    let text = String::from_utf8_lossy(&start.stdout);
    // Four candidates (a3, m1, b3, a2); the pick reaches one of them, so three
    // minus itself remain.
    assert!(
        text.starts_with("Bisecting: 2 revisions left to test after this (roughly 1 step)\n"),
        "{text}"
    );
    assert_eq!(tested(&text), "a2", "{text}");

    let _ = std::fs::remove_dir_all(&root);
}

/// `cmd_bisect__next` checks its argument count before it reads any state, so an
/// operand is refused whether or not a bisection is running.
#[test]
fn next_takes_no_arguments() {
    let (root, repo, home) = fixture("next");

    for started in [false, true] {
        if started {
            git(&repo, &home, &["bisect", "start", "a3", "a1"]);
        }
        let o = run(&repo, &home, &["bisect", "next", "extra1"]);
        assert_eq!(o.status.code(), Some(1), "started={started}: {:?}", o.status);
        assert_eq!(
            String::from_utf8_lossy(&o.stderr),
            "error: 'git bisect next' requires 0 arguments\n",
            "started={started}"
        );
        assert_eq!(String::from_utf8_lossy(&o.stdout), "", "started={started}");
    }

    let _ = std::fs::remove_dir_all(&root);
}
