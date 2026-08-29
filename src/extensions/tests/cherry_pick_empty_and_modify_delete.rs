//! Two things a cherry-pick reports rather than resolves: a path one side
//! modified and the other deleted, and a pick that turns out to be empty.
//!
//! The empty one is a sequence rather than a single command — the pick stops,
//! `--continue` refuses again because `continue_single_pick()` is
//! `git commit --no-edit --cleanup=strip` and that command will not record an
//! empty commit, and only `--skip` (or `--allow-empty`) moves past it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .expect("run binary")
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let out = run(dir, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-cpe-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir fixture");
    ok(&root, &["init", "-q", "-b", "main"]);
    root
}

/// The short id and subject of `rev`, which is how the sequencer names a commit
/// in its messages.
fn label(dir: &Path, rev: &str) -> String {
    let line = stdout_of(&ok(dir, &["log", "-1", "--format=%h (%s)", rev]));
    line.trim_end().to_string()
}

#[test]
fn a_modify_delete_conflict_names_both_sides_and_the_version_left_in_tree() {
    // HEAD deleted the file, the picked commit modified it.
    let dir = scratch("md");
    std::fs::write(dir.join("f.txt"), "base\n").expect("write");
    ok(&dir, &["add", "f.txt"]);
    ok(&dir, &["commit", "-qm", "base"]);
    ok(&dir, &["checkout", "-q", "-b", "side"]);
    std::fs::write(dir.join("f.txt"), "changed\n").expect("write");
    ok(&dir, &["commit", "-qam", "modify"]);
    ok(&dir, &["checkout", "-q", "main"]);
    ok(&dir, &["rm", "-q", "f.txt"]);
    ok(&dir, &["commit", "-qm", "delete"]);

    let side = label(&dir, "side");
    let out = run(&dir, &["cherry-pick", "side"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stdout_of(&out),
        format!(
            "CONFLICT (modify/delete): f.txt deleted in HEAD and modified in {side}.  \
             Version {side} of f.txt left in tree.\n"
        )
    );
    ok(&dir, &["cherry-pick", "--abort"]);

    // The other way round: HEAD modified it and the picked commit deleted it.
    let dir = scratch("dm");
    std::fs::write(dir.join("f.txt"), "base\n").expect("write");
    ok(&dir, &["add", "f.txt"]);
    ok(&dir, &["commit", "-qm", "base"]);
    ok(&dir, &["checkout", "-q", "-b", "side"]);
    ok(&dir, &["rm", "-q", "f.txt"]);
    ok(&dir, &["commit", "-qm", "delete"]);
    ok(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("f.txt"), "changed\n").expect("write");
    ok(&dir, &["commit", "-qam", "modify"]);

    let side = label(&dir, "side");
    let out = run(&dir, &["cherry-pick", "side"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stdout_of(&out),
        format!(
            "CONFLICT (modify/delete): f.txt deleted in {side} and modified in HEAD.  \
             Version HEAD of f.txt left in tree.\n"
        )
    );
}

/// A commit whose change is already in HEAD picks to nothing. The stop, the
/// refused `--continue` and the `--skip` are one story, so they are one test.
#[test]
fn an_empty_pick_stops_refuses_to_continue_and_is_skipped() {
    let dir = scratch("empty");
    std::fs::write(dir.join("f.txt"), "a\n").expect("write");
    ok(&dir, &["add", "f.txt"]);
    ok(&dir, &["commit", "-qm", "base"]);
    ok(&dir, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(dir.join("f.txt"), "b\n").expect("write");
    ok(&dir, &["commit", "-qam", "same on topic"]);
    ok(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("f.txt"), "b\n").expect("write");
    ok(&dir, &["commit", "-qam", "same on main"]);
    ok(&dir, &["checkout", "-q", "topic"]);

    let empty_advice = "The previous cherry-pick is now empty, possibly due to conflict resolution.\n\
                        If you wish to commit it anyway, use:\n\
                        \n    git commit --allow-empty\n\
                        \nOtherwise, please use 'git cherry-pick --skip'\n";

    let stop = run(&dir, &["--no-advice", "cherry-pick", "main"]);
    assert_eq!(stop.status.code(), Some(1));
    assert_eq!(stderr_of(&stop), empty_advice);
    // `--no-advice` takes the three status hints with it and leaves the state
    // line, which is `wt_status_prepare()`'s `s->hints`.
    assert_eq!(
        stdout_of(&stop),
        "On branch topic\nYou are currently cherry-picking commit \
         ".to_string()
            + &stdout_of(&ok(&dir, &["rev-parse", "--short", "main"])).trim_end().to_string()
            + ".\n\nnothing to commit, working tree clean\n"
    );

    // `--continue` is `git commit`, which will not record an empty commit.
    let cont = run(&dir, &["--no-advice", "cherry-pick", "--continue"]);
    assert_eq!(cont.status.code(), Some(1));
    assert_eq!(stderr_of(&cont), empty_advice);
    let before = stdout_of(&ok(&dir, &["rev-parse", "HEAD"]));

    ok(&dir, &["cherry-pick", "--skip"]);
    assert_eq!(stdout_of(&ok(&dir, &["rev-parse", "HEAD"])), before, "the skip records nothing");
    assert!(!dir.join(".git").join("CHERRY_PICK_HEAD").exists(), "the pick is over");
}
