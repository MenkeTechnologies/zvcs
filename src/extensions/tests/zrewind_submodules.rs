//! `git zrewind <duration>` across a submodule tree.
//!
//! Two things made the verb do nothing on the tree shape it advertises
//! ("the repo at the cwd and every nested submodule"):
//!
//!   * it collected only the *first* level of submodules, so a submodule that
//!     has submodules of its own left the deepest repos at their current HEAD
//!     while their parents moved — a tree rewound part of the way down;
//!   * it refused any repo `is_dirty()` called dirty, and a superproject reads
//!     as dirty the moment a submodule moves. That is the exact state a
//!     tree-wide rewind exists to undo, so on a real superproject the verb
//!     skipped every repo and rewound nothing (measured: 0 of 3).
//!
//! The refusal itself is a real guarantee and is pinned here too: uncommitted
//! work in a repo's own tracked files still stops that repo from being reset.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Run with the clock pinned `ago` seconds back, so the reflog entry the
/// command writes carries that timestamp — `zrewind` reads reflog times.
fn at(dir: &Path, home: &Path, ago: u64, args: &[&str]) -> Output {
    let stamp = format!("{} +0000", now() - ago);
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example")
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
        "{what} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn subject(repo: &Path, home: &Path) -> String {
    ok(&run(repo, home, &["log", "-1", "--format=%s"]), "log").trim().to_string()
}

const OLD: u64 = 10_800; // three hours back
const RECENT: u64 = 300; // five minutes back

/// `super` → `sub` → `deep`, every repo carrying an old commit and a recent
/// one, and every step of the wiring stamped old so each repo's reflog reaches
/// back past the rewind target.
fn nested(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-rwsub-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();

    // Standalone origins for the two submodules.
    for name in ["origin-deep", "origin-sub"] {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        ok(&run(&d, &home, &["init", "-q", "-b", "main", "."]), "init");
        std::fs::write(d.join("f.txt"), b"old\n").unwrap();
        ok(&at(&d, &home, OLD, &["add", "f.txt"]), "add");
        ok(&at(&d, &home, OLD, &["commit", "-q", "-m", "old"]), "commit");
    }
    let allow = ["-c", "protocol.file.allow=always"];
    let sub_origin = root.join("origin-sub");
    ok(&at(&sub_origin, &home, OLD,
        &[&allow[..], &["submodule", "add", "-q", root.join("origin-deep").to_str().unwrap(), "deep"]].concat()), "add deep");
    ok(&at(&sub_origin, &home, OLD, &["commit", "-q", "-m", "old-add-deep"]), "commit deep");

    let top = root.join("super");
    std::fs::create_dir_all(&top).unwrap();
    ok(&run(&top, &home, &["init", "-q", "-b", "main", "."]), "init super");
    std::fs::write(top.join("p.txt"), b"old\n").unwrap();
    ok(&at(&top, &home, OLD, &["add", "p.txt"]), "add p");
    ok(&at(&top, &home, OLD, &["commit", "-q", "-m", "old"]), "commit super");
    ok(&at(&top, &home, OLD,
        &[&allow[..], &["submodule", "add", "-q", sub_origin.to_str().unwrap(), "sub"]].concat()), "add sub");
    ok(&at(&top, &home, OLD, &["commit", "-q", "-m", "old-add-sub"]), "commit sub");
    ok(&at(&top, &home, OLD, &[&allow[..], &["submodule", "update", "--init", "--recursive", "-q"]].concat()), "sm update");

    (root, home, top)
}

/// A recent commit in each of the three repos, deepest first so each parent's
/// gitlink really moves.
fn advance_all(home: &Path, top: &Path) {
    for rel in ["sub/deep", "sub", "."] {
        let repo = top.join(rel);
        let file = if rel == "." { "p.txt" } else { "f.txt" };
        std::fs::write(repo.join(file), b"recent\n").unwrap();
        ok(&at(&repo, home, RECENT, &["commit", "-q", "-am", "recent"]), "recent commit");
    }
}

#[test]
fn a_rewind_reaches_every_depth_of_the_tree() {
    let (root, home, top) = nested("depth");
    advance_all(&home, &top);
    for rel in [".", "sub", "sub/deep"] {
        assert_eq!(subject(&top.join(rel), &home), "recent", "precondition: {rel} is at its recent commit");
    }

    let out = ok(&run(&top, &home, &["zrewind", "1h"]), "zrewind");

    // The nested submodule is the one a first-level walk never saw.
    assert_eq!(subject(&top.join("sub/deep"), &home), "old", "the NESTED submodule was not rewound:\n{out}");
    assert_eq!(subject(&top.join("sub"), &home), "old-add-deep", "the submodule was not rewound:\n{out}");
    assert_eq!(subject(&top, &home), "old-add-sub", "the superproject was not rewound:\n{out}");
    assert!(out.contains("3 rewound"), "all three repos must be counted:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_moved_gitlink_does_not_count_as_uncommitted_work() {
    let (root, home, top) = nested("gitlink");
    advance_all(&home, &top);
    // One more commit in the submodule, *after* the superproject's last commit,
    // so the only thing left in the superproject's worktree is the gitlink its
    // child just moved. (`advance_all` commits the parent last, which sweeps the
    // pointer into that commit and leaves the parent clean.)
    std::fs::write(top.join("sub/f.txt"), b"newer\n").unwrap();
    ok(&at(&top.join("sub"), &home, RECENT, &["commit", "-q", "-am", "newer"]), "child commit");

    // The only difference in the superproject's worktree is the submodule
    // pointer its child just moved. `git status` reports that as ` M sub`, which
    // the blanket dirty check read as work to protect — so nothing was rewound.
    let status = ok(&run(&top, &home, &["status", "--porcelain"]), "status");
    assert!(status.contains(" M sub"), "precondition: the gitlink shows as modified:\n{status}");
    let clean = ok(&run(&top, &home, &["status", "--porcelain", "--ignore-submodules=all"]), "status");
    assert!(clean.trim().is_empty(), "precondition: there is no other local work:\n{clean}");

    let out = ok(&run(&top, &home, &["zrewind", "1h"]), "zrewind");
    assert!(!out.contains("skip .:"), "a moved gitlink must not refuse the superproject:\n{out}");
    assert_eq!(subject(&top, &home), "old-add-sub", "the superproject must be rewound:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn real_uncommitted_work_still_refuses_that_repo() {
    let (root, home, top) = nested("dirty");
    advance_all(&home, &top);

    // An edit to a tracked file of the superproject itself: `reset --hard` would
    // destroy it, so that repo must be skipped — while the submodules below it,
    // which have no local work, are still rewound.
    std::fs::write(top.join("p.txt"), b"work in progress\n").unwrap();

    let out = ok(&run(&top, &home, &["zrewind", "1h"]), "zrewind");
    assert!(out.contains("skip ."), "a repo with uncommitted work must be skipped:\n{out}");
    assert_eq!(std::fs::read_to_string(top.join("p.txt")).unwrap(), "work in progress\n",
        "uncommitted work must survive");
    assert_eq!(subject(&top.join("sub/deep"), &home), "old",
        "a clean submodule must still be rewound when its parent is skipped:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dry_run_names_every_repo_and_moves_none_of_them() {
    let (root, home, top) = nested("dry");
    advance_all(&home, &top);

    let out = ok(&run(&top, &home, &["zrewind", "1h", "--dry-run"]), "dry run");
    assert!(out.contains("would rewind"), "the preview must name what would move:\n{out}");
    assert!(out.contains("3 would rewind"), "the preview must cover the whole tree:\n{out}");
    for rel in [".", "sub", "sub/deep"] {
        assert_eq!(subject(&top.join(rel), &home), "recent", "--dry-run moved {rel}:\n{out}");
    }

    let _ = std::fs::remove_dir_all(&root);
}
