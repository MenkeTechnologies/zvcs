//! `git show` against stock git when a range clears `no_walk`.
//!
//! `cmd_show()` starts with `rev.no_walk = 1` and, once a range or `^<rev>` has
//! cleared it, hands everything to `cmd_log_walk()` (builtin/log.c:696-701), with
//! `show_setup_revisions_tweak()` (builtin/log.c:651-659) making merges default to
//! the dense combined diff and the patch the format when none was asked for. So
//! `--graph`, `--left-right` and the rest of `git log`'s walk options draw what
//! `git log` draws. zvcs refused `--graph` with a range and every walk option its
//! own parser did not know.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, dir: &Path, date: i64, args: &[&str]) -> (String, String, i32) {
    let date = format!("{date} +0000");
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// `main`: a, b, m1, m2, a merge of `side` (s1, s2), m3; `skew` grows from `s2`
/// with dates running backwards; notes on m3.
fn repo(stock: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("zvcs-show-graph-range-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut date = 1_000_000_000;
    let mut git = |args: &[&str]| {
        date += 100;
        let (_, err, code) = run(stock, &dir, date, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    git(&["init", "-q", "-b", "main"]);
    let commit = |git: &mut dyn FnMut(&[&str]), name: &str| {
        std::fs::write(dir.join(format!("{name}.t")), format!("{name}\n")).unwrap();
        git(&["add", &format!("{name}.t")]);
        git(&["commit", "-q", "-m", name]);
    };
    commit(&mut git, "a");
    commit(&mut git, "b");
    git(&["checkout", "-q", "-b", "side"]);
    commit(&mut git, "s1");
    commit(&mut git, "s2");
    git(&["checkout", "-q", "main"]);
    commit(&mut git, "m1");
    commit(&mut git, "m2");
    git(&["merge", "-q", "--no-ff", "side", "-m", "merge"]);
    commit(&mut git, "m3");
    git(&["checkout", "-q", "-b", "skew", "side"]);
    commit(&mut git, "k1");
    commit(&mut git, "k2");
    git(&["checkout", "-q", "main"]);
    git(&["notes", "add", "-m", "note for m3", "HEAD"]);
    Repo(dir)
}

#[test]
fn a_range_under_show_walks_as_log_does() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock);
    for args in [
        &["show", "--graph", "main~1..main"][..],
        &["show", "--graph", "main~3..main"],
        &["show", "--graph", "--oneline", "main~2..main"],
        &["show", "--graph", "--stat", "main~3..main"],
        &["show", "--graph", "-s", "main~3..main"],
        &["show", "--graph", "-c", "main~3..main"],
        &["show", "--graph", "--first-parent", "main~3..main"],
        &["show", "--graph", "--raw", "main~3..main"],
        &["show", "--graph", "--boundary", "main~2..main"],
        &["show", "--graph", "main..skew", "--reverse"],
        &["show", "--graph", "--format=%s", "main..skew", "main~1"],
        &["show", "--graph", "^main~2", "main"],
        &["show", "--graph", "--left-right", "main...skew"],
        &["show", "--left-right", "--oneline", "main...skew"],
        &["show", "--cherry-mark", "main...skew"],
        &["show", "--graph", "--no-walk", "main~3..main"],
        &["show", "--graph", "main~3..main", "--no-walk"],
    ] {
        assert_eq!(run(BIN, &r.0, 0, args), run(stock, &r.0, 0, args), "{args:?}");
    }
}
