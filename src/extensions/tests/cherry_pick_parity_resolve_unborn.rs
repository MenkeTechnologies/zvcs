//! `cherry-pick --strategy=resolve` and `git merge-resolve` on a branch yet to be born.
//!
//! `do_pick_commit()` hands `try_merge_command()` the empty tree for `HEAD`, and
//! `git-merge-resolve.sh` opens with `git diff-index --quiet --cached HEAD --`, which dies
//! with `fatal: bad revision 'HEAD'` there; `if !` takes that for local changes, the
//! `--name-only` child dies the same way, and the script exits 2 (git-merge-resolve.sh:10-15),
//! which the sequencer folds into `cherry-pick failed` (sequencer.c:2488-2494). zvcs bailed
//! with an `unsupported` error.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// `main` with two commits, checked out on the orphan `new`; `empty` clears its index and
/// worktree, otherwise both keep `main`'s files.
fn prepare(stock: &str, r: &Path, empty: bool) {
    let ok = |args: &[&str]| assert_eq!(run(stock, r, args).2, 0, "{args:?}");
    ok(&["init", "-q", "-b", "main", "."]);
    std::fs::write(r.join("a"), "a\n").unwrap();
    ok(&["add", "a"]);
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
    std::fs::write(r.join("c"), "c\n").unwrap();
    ok(&["add", "c"]);
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "two"]);
    ok(&["checkout", "-q", "--orphan", "new"]);
    if empty {
        ok(&["rm", "-rqf", "."]);
    }
}

fn compare(tag: &str, empty: bool, args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-cp-resolve-unborn-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let r = base.join(side);
        std::fs::create_dir_all(&r).unwrap();
        prepare(stock, &r, empty);
        let out = run(bin, &r, args);
        let mut state = run(stock, &r, &["status", "--porcelain"]).0;
        state.push_str(&run(stock, &r, &["for-each-ref"]).0);
        for f in ["MERGE_MSG", "CHERRY_PICK_HEAD", "AUTO_MERGE"] {
            if let Ok(text) = std::fs::read_to_string(r.join(".git").join(f)) {
                state.push_str(&format!("-- {f}\n{text}"));
            }
        }
        outputs.push((out, state));
    }
    assert_eq!(outputs[1], outputs[0], "{tag}: {args:?}");
}

#[test]
fn the_resolve_strategy_refuses_an_unborn_head_as_its_script_does() {
    compare("pick", true, &["cherry-pick", "--strategy=resolve", "main"]);
    compare("pick-xopt", true, &["cherry-pick", "--strategy=resolve", "-Xours", "main"]);
    compare("script", true, &["merge-resolve", "HEAD", "--", "HEAD", "main"]);
}
