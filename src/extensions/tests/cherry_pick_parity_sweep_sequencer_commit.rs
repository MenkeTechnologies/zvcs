//! `cherry-pick` / `revert` against stock git where the sequencer hands a
//! message to the commit, and where a stopped pick is concluded.
//!
//! * `MERGE_MSG` is `ctx->message` as built (sequencer.c:2450): `-x`/`-s` on an
//!   empty message leave their leading blank lines there, and only
//!   `try_to_commit()`'s cleanup (sequencer.c:1622-1630) strips the committed
//!   copy. That cleanup never cuts at a scissors line, and `commit.cleanup` from
//!   `git_sequencer_config()` (sequencer.c:274-298) is an explicit mode, with a
//!   warning for any value outside the four it knows.
//! * `--keep-redundant-commits` is tested before `--empty=drop`
//!   (sequencer.c:1813-1818).
//! * A root commit never reaches the `-m` checks (sequencer.c:2333).
//! * A signature gpg refuses is `error: failed to write commit object` and
//!   status 1 (sequencer.c:1687-1690, :2570), on the in-process and the `-e`
//!   path alike.
//! * `continue_single_pick()` (sequencer.c:5232-5256) is one plain `git commit`
//!   for both commands: the author comes from `CHERRY_PICK_HEAD` alone, and the
//!   `--continue` command line's `-e` reaches it and the resumed picks.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("VISUAL")
        .env_remove("EDITOR")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700009999 +0000")
        .env("GIT_COMMITTER_DATE", "1700009999 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

/// Build the shared history once with stock git:
///
/// * `main`: `c1` (a.txt), `c2` (a.txt line 2), `c3` (a.txt line 9)
/// * `side` from `c1`: `same` (the `c2` change, redundant on `main`), `bump`
///   (line 2 again, conflicts with `c2`), `nomsg` (empty message), `cut` (a
///   message with a comment line and a scissors line)
fn template(stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-cp-sweep-seq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let t = dir.join("tmpl");
    std::fs::create_dir_all(&t).unwrap();
    let mut tick = 1_700_000_000u64;
    let commit = |args: &[&str], tick: &mut u64| {
        *tick += 60;
        let date = format!("{tick} +0000");
        let mut full = vec!["-c", "maintenance.auto=false", "commit", "-q"];
        full.extend_from_slice(args);
        let out = Command::new(stock)
            .args(&full)
            .current_dir(&t)
            .env("HOME", &dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    let lines = |two: &str, nine: &str| format!("1\n{two}\n3\n4\n5\n6\n7\n8\n{nine}\n10\n");
    git(stock, &t, &["init", "-q", "-b", "main"]);
    std::fs::write(t.join("a.txt"), lines("2", "9")).unwrap();
    git(stock, &t, &["add", "a.txt"]);
    commit(&["-m", "c1"], &mut tick);
    git(stock, &t, &["tag", "c1"]);
    std::fs::write(t.join("a.txt"), lines("TWO", "9")).unwrap();
    commit(&["-am", "c2"], &mut tick);
    git(stock, &t, &["tag", "c2"]);
    std::fs::write(t.join("a.txt"), lines("TWO", "NINE")).unwrap();
    commit(&["-am", "c3"], &mut tick);
    git(stock, &t, &["checkout", "-q", "-b", "side", "c1"]);
    std::fs::write(t.join("a.txt"), lines("TWO", "9")).unwrap();
    commit(&["-am", "same"], &mut tick);
    git(stock, &t, &["tag", "same"]);
    std::fs::write(t.join("a.txt"), lines("BUMP", "9")).unwrap();
    commit(&["-am", "bump"], &mut tick);
    git(stock, &t, &["tag", "bump"]);
    commit(&["--allow-empty", "--allow-empty-message", "-m", ""], &mut tick);
    git(stock, &t, &["tag", "nomsg"]);
    std::fs::write(t.join("b.txt"), "b\n").unwrap();
    git(stock, &t, &["add", "b.txt"]);
    commit(
        &["--cleanup=verbatim", "-m", "cut\n\n# a comment\nbody\n# ------------------------ >8 ------------------------\nbelow\n"],
        &mut tick,
    );
    git(stock, &t, &["tag", "cut"]);
    git(stock, &t, &["checkout", "-q", "main"]);
    dir
}

/// Everything a step leaves behind that the comparison covers.
fn state(stock: &str, dir: &Path) -> String {
    let mut s = String::new();
    for args in [&["log", "--format=%H %an <%ae> %ad%n%B", "-2"][..], &["status", "--porcelain=v2"]] {
        s.push_str(&git(stock, dir, args).0);
    }
    for f in ["MERGE_MSG", "CHERRY_PICK_HEAD", "REVERT_HEAD", "COMMIT_EDITMSG", "sequencer/todo"] {
        if let Ok(text) = std::fs::read_to_string(dir.join(".git").join(f)) {
            s.push_str(&format!("-- {f}\n{text}"));
        }
    }
    s
}

/// Run `steps` in a fresh copy of the template under each binary and compare
/// every step's output and the final state.
fn compare(stock: &str, root: &Path, steps: &[&[&str]]) {
    let mut seen = Vec::new();
    for bin in [stock, BIN] {
        let work = root.join("work");
        let _ = std::fs::remove_dir_all(&work);
        let copied = Command::new("cp").arg("-R").arg(root.join("tmpl")).arg(&work).status().unwrap();
        assert!(copied.success());
        let outputs: Vec<_> = steps.iter().map(|args| git(bin, &work, args)).collect();
        seen.push((outputs, state(stock, &work)));
    }
    assert_eq!(seen[1], seen[0], "{steps:?}");
}

#[test]
fn sequencer_messages_empty_rules_and_continue_match_stock() {
    let Some(stock) = stock_git() else { return };
    let root = template(stock);
    let cases: &[&[&[&str]]] = &[
        // `MERGE_MSG` keeps the blank lines `-x`/`-s` put ahead of an empty message.
        &[&["cherry-pick", "-x", "-s", "nomsg"]],
        // Neither `--cleanup=scissors` nor `-x`'s whitespace cleanup cuts at the
        // scissors line; `commit.cleanup=strip` drops the comment line.
        &[&["cherry-pick", "--cleanup=scissors", "cut"]],
        &[&["-c", "commit.cleanup=strip", "cherry-pick", "-x", "cut"]],
        &[&["-c", "commit.cleanup=default", "-c", "commit.cleanup=bogus", "cherry-pick", "--quit"]],
        // `--keep-redundant-commits` outranks `--empty=drop`.
        &[&["cherry-pick", "--keep-redundant-commits", "--empty=drop", "same"]],
        // `-m` is never consulted for a root commit.
        &[&["revert", "-m", "2", "c1"]],
        // A refused signature stops at 1 with the merge in place.
        &[&["-c", "gpg.program=false", "cherry-pick", "-S", "same"]],
        &[&["-c", "gpg.program=false", "revert", "-S", "c3"]],
        &[&["-c", "gpg.program=false", "cherry-pick", "-S", "-e", "same"]],
        // One `git commit` concludes either stop: a stopped revert commits as the
        // current user, a stopped pick as the picked author.
        &[&["revert", "c2"], &["add", "a.txt"], &["cherry-pick", "--continue"]],
        &[&["cherry-pick", "bump"], &["add", "a.txt"], &["revert", "--continue"]],
        // `-e` on `--continue` reaches the child commit and the resumed picks.
        &[&["cherry-pick", "bump", "nomsg"], &["add", "a.txt"], &["cherry-pick", "--continue", "-e"]],
    ];
    for steps in cases {
        compare(stock, &root, steps);
    }
    let _ = std::fs::remove_dir_all(&root);
}
