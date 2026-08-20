//! When a revert may leave `.git/AUTO_MERGE` behind, and when it may not.
//!
//! `merge_switch_to_result()` (merge-ort.c:4927) does three things in order:
//! `checkout(opt, head, result->tree)` (4936), `record_conflicted_index_entries()`
//! (4947), then the `write_auto_merge` region that records the merged tree as
//! `AUTO_MERGE` (4959-4971). Only the third writes the file, and each of the
//! first two `return`s outright on failure:
//!
//! ```text
//! if (checkout(opt, head, result->tree)) {
//!         /* failure to function */
//!         result->clean = -1;
//!         merge_finalize(opt, result);
//!         trace2_region_leave("merge", "checkout", opt->repo);
//!         return;
//! }
//! ```
//!
//! `checkout()` is an `unpack_trees()` with `setup_unpack_trees_porcelain(…,
//! "merge")` (merge-ort.c:4618), so it is exactly the step that refuses with
//! `error: Your local changes to the following files would be overwritten by
//! merge:` — and a revert that hits that refusal writes no `AUTO_MERGE` and, since
//! `merge_display_update_messages()` runs later still (4973), reports no
//! `Auto-merging` lines either.
//!
//! A revert that *does* check out leaves the file behind even after committing:
//! only `remove_merge_branch_state()` and `sequencer_post_commit_cleanup()` delete
//! it. Both halves matter — a missing `AUTO_MERGE` breaks `git diff AUTO_MERGE`
//! on a stopped revert, and a stale one makes every later `git status` read a
//! merge that never happened.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A STOCK git to compare against, or `None` on a machine without one.
///
/// Resolved explicitly rather than through `PATH`: on a machine where zvcs
/// shadows `git`, a `PATH` lookup makes the oracle the thing under test.
fn stock_git() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return Path::new(&p).exists().then_some(p);
    }
    ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .map(str::to_owned)
}

const DATE: &str = "1112911993 +0000"; // 2005-04-07 in UTC

fn run(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .output()
        .unwrap()
}

fn ok(bin: &str, repo: &Path, home: &Path, args: &[&str]) {
    let out = run(bin, repo, home, args);
    assert!(
        out.status.success(),
        "{bin} {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn work_area(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-revam-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    (repo, home)
}

/// The sorted names directly under `.git`, which is how the two implementations'
/// leftover state is compared.
fn git_dir_listing(repo: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo.join(".git"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `c1` seeds `x.txt`/`y.txt`; `c2` touches both; `c3` touches a distant region of
/// `x.txt`. Reverting `c2` from `c3` therefore needs a real content merge of
/// `x.txt` — which is what would emit an `Auto-merging x.txt` line — while the
/// uncommitted edit to `y.txt` is what makes the checkout refuse.
fn fixture(git: &str, tag: &str) -> (PathBuf, PathBuf) {
    let (repo, home) = work_area(tag);
    ok(git, &repo, &home, &["init", "-q", "-b", "main", "."]);
    ok(git, &repo, &home, &["config", "user.name", "A U Thor"]);
    ok(git, &repo, &home, &["config", "user.email", "author@example.com"]);

    let base: String = (1..=20).map(|i| format!("x{i}\n")).collect();
    std::fs::write(repo.join("x.txt"), &base).unwrap();
    std::fs::write(repo.join("y.txt"), "y1\n").unwrap();
    ok(git, &repo, &home, &["add", "."]);
    ok(git, &repo, &home, &["commit", "-q", "-m", "c1"]);

    std::fs::write(repo.join("x.txt"), base.replacen("x1\n", "X1\n", 1)).unwrap();
    std::fs::write(repo.join("y.txt"), "y1\ny2\n").unwrap();
    ok(git, &repo, &home, &["add", "."]);
    ok(git, &repo, &home, &["commit", "-q", "-m", "c2"]);

    let c2 = std::fs::read_to_string(repo.join("x.txt")).unwrap();
    std::fs::write(repo.join("x.txt"), c2.replace("x20\n", "X20\n")).unwrap();
    ok(git, &repo, &home, &["add", "."]);
    ok(git, &repo, &home, &["commit", "-q", "-m", "c3"]);
    (repo, home)
}

/// A revert the checkout refuses must leave the repository exactly as stock does:
/// no `AUTO_MERGE`, and no `Auto-merging` line from a merge whose result was never
/// applied. `--no-commit` takes the same path and must behave the same.
#[test]
fn a_refused_revert_writes_no_auto_merge_and_no_merge_messages() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    for (tag, extra) in [("commit", vec![]), ("nocommit", vec!["--no-commit"])] {
        let (zrepo, zhome) = fixture(&git, &format!("z-{tag}"));
        let (grepo, ghome) = fixture(&git, &format!("g-{tag}"));
        // The uncommitted `y.txt` edit is the path `c2`'s revert would overwrite.
        for repo in [&zrepo, &grepo] {
            std::fs::write(repo.join("y.txt"), "y1\ny2\nlocal\n").unwrap();
        }
        let mut args = vec!["revert"];
        args.extend(extra.iter().copied());
        args.push("HEAD~1");

        let z = run(BIN, &zrepo, &zhome, &args);
        let g = run(&git, &grepo, &ghome, &args);

        assert_eq!(
            g.status.code(),
            z.status.code(),
            "`git {}` exit code differs from stock",
            args.join(" ")
        );
        assert_eq!(
            String::from_utf8_lossy(&g.stderr),
            String::from_utf8_lossy(&z.stderr),
            "`git {}` stderr differs from stock",
            args.join(" ")
        );
        assert_eq!(
            String::from_utf8_lossy(&g.stdout),
            String::from_utf8_lossy(&z.stdout),
            "`git {}` stdout differs from stock — a refused checkout reports no \
             `Auto-merging` lines (merge-ort.c:4936 returns before 4973)",
            args.join(" ")
        );
        assert!(
            String::from_utf8_lossy(&z.stdout).is_empty(),
            "nothing at all is printed on stdout by a refused revert"
        );
        assert_eq!(
            git_dir_listing(&grepo),
            git_dir_listing(&zrepo),
            "`git {}` left different state in .git than stock",
            args.join(" ")
        );
        assert!(
            !zrepo.join(".git/AUTO_MERGE").exists(),
            "a refused checkout returns before merge-ort.c's write_auto_merge region"
        );

        let _ = std::fs::remove_dir_all(zrepo.parent().unwrap());
        let _ = std::fs::remove_dir_all(grepo.parent().unwrap());
    }
}

/// The other half: a revert that really checks out its result records
/// `AUTO_MERGE` and — because only `remove_merge_branch_state()` deletes it —
/// leaves it there afterwards, exactly as stock does. Without this, "fix the
/// leak" degenerates into never writing the file at all.
#[test]
fn a_revert_that_applies_still_records_auto_merge() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    let (zrepo, zhome) = fixture(&git, "z-clean");
    let (grepo, ghome) = fixture(&git, "g-clean");
    // No local edits this time, so the checkout succeeds.
    let args = ["revert", "--no-edit", "HEAD~1"];
    let z = run(BIN, &zrepo, &zhome, &args);
    let g = run(&git, &grepo, &ghome, &args);

    assert!(
        z.status.success(),
        "zvcs revert failed: {}",
        String::from_utf8_lossy(&z.stderr)
    );
    assert_eq!(g.status.code(), z.status.code(), "exit codes differ");
    assert_eq!(
        git_dir_listing(&grepo),
        git_dir_listing(&zrepo),
        "a successful revert must leave the same .git state as stock"
    );
    let auto = std::fs::read_to_string(zrepo.join(".git/AUTO_MERGE")).unwrap();
    let tree = run(BIN, &zrepo, &zhome, &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(
        auto.trim(),
        String::from_utf8_lossy(&tree.stdout).trim(),
        "AUTO_MERGE holds the merged tree the revert checked out"
    );

    let _ = std::fs::remove_dir_all(zrepo.parent().unwrap());
    let _ = std::fs::remove_dir_all(grepo.parent().unwrap());
}

/// The sibling sequencer/merge paths, which reach the same merge-ort region from
/// their own gates. They already agreed with stock — including stock's own habit
/// of leaving `AUTO_MERGE` behind after a `merge` whose checkout was refused — and
/// this pins that agreement so the revert fix cannot be over-applied to them.
#[test]
fn sibling_commands_leave_the_same_git_dir_as_stock() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    for (tag, args) in [
        ("cherry-pick", vec!["cherry-pick", "HEAD~1"]),
        ("merge", vec!["merge", "--no-ff", "-m", "m", "HEAD~1"]),
        ("rebase", vec!["rebase", "HEAD~2"]),
    ] {
        let (zrepo, zhome) = fixture(&git, &format!("z-sib-{tag}"));
        let (grepo, ghome) = fixture(&git, &format!("g-sib-{tag}"));
        for repo in [&zrepo, &grepo] {
            std::fs::write(repo.join("y.txt"), "y1\ny2\nlocal\n").unwrap();
        }
        let z = run(BIN, &zrepo, &zhome, &args);
        let g = run(&git, &grepo, &ghome, &args);
        assert_eq!(
            g.status.code(),
            z.status.code(),
            "`git {}` exit code differs from stock: {}",
            args.join(" "),
            String::from_utf8_lossy(&z.stderr)
        );
        assert_eq!(
            git_dir_listing(&grepo),
            git_dir_listing(&zrepo),
            "`git {}` left different state in .git than stock",
            args.join(" ")
        );
        let _ = std::fs::remove_dir_all(zrepo.parent().unwrap());
        let _ = std::fs::remove_dir_all(grepo.parent().unwrap());
    }
}
