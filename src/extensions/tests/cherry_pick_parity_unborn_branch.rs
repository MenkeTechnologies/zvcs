//! `cherry-pick` and `revert` onto a branch yet to be born, against stock git.
//!
//! `do_pick_commit()` reads `HEAD` with `repo_get_oid()` and, when that fails,
//! merges against the empty tree (sequencer.c:2317-2327); `try_to_commit()` then
//! writes a root commit and `update_head_with_reflog()` creates the branch
//! (sequencer.c:1574-1577, :1285-1287). A root commit fast-forwards it under
//! `--ff` (sequencer.c:2366-2371). `rollback_single_pick()` refuses with
//! `cannot abort from a branch yet to be born` (sequencer.c:3564-3565), a
//! sequence of reverts with `can't revert as initial commit` (sequencer.c:5746-5749),
//! and the `git commit` that concludes a stopped pick logs `commit (initial)`
//! (builtin/commit.c:1851-1853). zvcs refused every one of these outright.
//!
//! A *sequence* that stops on an unborn branch is left out: stock's
//! `save_head()` writes an `oid` nothing initialised there (sequencer.c:5746-5751).

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
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
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

/// `main`: `base` (a.txt) then `edit` (a.txt changed); an orphan `roots` branch:
/// `root` (r.txt) then `root2` (s.txt). The checkout is left on `nb`, an orphan
/// branch with an empty index and work tree.
fn template(stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-cp-unborn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let t = dir.join("tmpl");
    std::fs::create_dir_all(&t).unwrap();
    let commit = |file: &str, text: &str, msg: &str| {
        std::fs::write(t.join(file), text).unwrap();
        git(stock, &t, &["add", file]);
        let (_, err, code) = git(stock, &t, &["-c", "maintenance.auto=false", "commit", "-q", "-m", msg]);
        assert_eq!(code, Some(0), "{msg}: {err}");
        git(stock, &t, &["tag", msg]);
    };
    git(stock, &t, &["init", "-q", "-b", "main"]);
    commit("a.txt", "a\n", "base");
    commit("a.txt", "A\n", "edit");
    git(stock, &t, &["checkout", "-q", "--orphan", "roots"]);
    git(stock, &t, &["rm", "-rqf", "."]);
    commit("r.txt", "r\n", "root");
    commit("s.txt", "s\n", "root2");
    git(stock, &t, &["checkout", "-q", "--orphan", "nb"]);
    git(stock, &t, &["rm", "-rqf", "."]);
    dir
}

fn state(stock: &str, dir: &Path) -> String {
    let mut s = String::new();
    for args in [
        &["log", "--format=%H %P %s", "-3", "nb", "--"][..],
        &["reflog", "show", "--format=%H %gs", "nb", "--"],
        &["status", "--porcelain=v2", "--branch"],
    ] {
        let (out, err, _) = git(stock, dir, args);
        s.push_str(&out);
        s.push_str(&err);
    }
    for f in ["MERGE_MSG", "CHERRY_PICK_HEAD", "REVERT_HEAD", "sequencer/todo", "sequencer/abort-safety"] {
        if let Ok(text) = std::fs::read_to_string(dir.join(".git").join(f)) {
            s.push_str(&format!("-- {f}\n{text}"));
        }
    }
    s
}

#[test]
fn picks_and_reverts_onto_an_unborn_branch_match_stock() {
    let Some(stock) = stock_git() else { return };
    let root = template(stock);
    let cases: &[&[&[&str]]] = &[
        // A root commit, alone, under `--ff`, and two of them in a sequence.
        &[&["cherry-pick", "root"]],
        &[&["cherry-pick", "--ff", "root"]],
        &[&["cherry-pick", "root", "root2"]],
        // A modify/delete against the empty tree, then each way out of it.
        &[&["cherry-pick", "edit"], &["cherry-pick", "--abort"]],
        &[&["cherry-pick", "edit"], &["add", "a.txt"], &["cherry-pick", "--continue"]],
        // `--no-commit` never looks at `HEAD`.
        &[&["cherry-pick", "-n", "edit"]],
        // A single revert merges against the empty tree; a sequence is refused.
        &[&["revert", "edit"]],
        &[&["revert", "root"]],
        &[&["revert", "edit", "base"]],
    ];
    for steps in cases {
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
    let _ = std::fs::remove_dir_all(&root);
}
