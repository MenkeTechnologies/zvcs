//! Notes under the mail formats, against stock git, across records.
//!
//! `show_log()` fences a mail record's notes with `next_commentary_block()`, which
//! writes `---` unless `opt->shown_dashes` is up and raises it either way
//! (log-tree.c:685-693, :893-898). Only `log_tree_diff_flush()` lowers it again
//! (log-tree.c:929), and `log_tree_diff()` reaches that flush for a root under
//! `show_root_diff`, a non-merge when a diff is wanted, and a merge only through the
//! per-parent loop — never through `do_diff_combined()` (log-tree.c:1188-1268). So a
//! record that is not diffed, after one that showed notes, gets a blank line where
//! the `---` would be. zvcs wrote `---` in front of every record's notes.
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

/// `main`: a, b, a merge of `side` (s1), an empty commit; notes on every commit
/// but `s1`, the merge's two-paragraph.
fn repo(stock: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("zvcs-mail-notes-dashes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut date = 1_000_000_000;
    let mut git = |args: &[&str]| {
        date += 100;
        let (_, err, code) = run(stock, &dir, date, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    git(&["init", "-q", "-b", "main"]);
    for name in ["a", "b"] {
        std::fs::write(dir.join(name), format!("{name}\n")).unwrap();
        git(&["add", name]);
        git(&["commit", "-q", "-m", name]);
        git(&["notes", "add", "-m", &format!("note {name}"), "HEAD"]);
    }
    git(&["checkout", "-q", "-b", "side", "HEAD~1"]);
    std::fs::write(dir.join("s"), "s\n").unwrap();
    git(&["add", "s"]);
    git(&["commit", "-q", "-m", "s1"]);
    git(&["checkout", "-q", "main"]);
    git(&["merge", "-q", "--no-ff", "side", "-m", "merge"]);
    git(&["notes", "add", "-m", "merge note", "-m", "para2", "HEAD"]);
    git(&["commit", "-q", "--allow-empty", "-m", "empty"]);
    git(&["notes", "add", "-m", "empty note", "HEAD"]);
    Repo(dir)
}

#[test]
fn an_undiffed_record_inherits_the_previous_fence() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock);
    for args in [
        &["log", "--pretty=email", "--notes"][..],
        &["log", "--pretty=email", "--notes", "-s"],
        &["log", "--pretty=email", "--notes", "--stat"],
        &["log", "--pretty=email", "--notes", "-p"],
        &["log", "--pretty=email", "--notes", "--raw", "-p"],
        &["log", "--pretty=email", "--notes", "--stat", "-p", "--graph"],
        &["log", "--pretty=email", "--notes", "--cc", "-p"],
        &["log", "--pretty=email", "--notes", "-m", "-p"],
        &["log", "--pretty=email", "--notes", "--first-parent", "-p"],
        &["log", "--pretty=email", "--notes", "--reverse"],
        &["log", "--pretty=email", "--notes", "--log-size"],
        &["log", "--pretty=mboxrd", "--notes", "--graph"],
        &["show", "--pretty=email", "--notes", "HEAD", "HEAD~1"],
        &["show", "--pretty=email", "--notes", "HEAD~1", "HEAD"],
        &["show", "--pretty=email", "--notes", "-m", "HEAD", "HEAD~1"],
        &["show", "--pretty=email", "--notes", "--stat", "HEAD", "HEAD~1", "HEAD~2"],
        &["show", "--pretty=email", "--notes", "HEAD~3..HEAD"],
        &["format-patch", "--stdout", "--notes", "-2"],
    ] {
        assert_eq!(run(BIN, &r.0, 0, args), run(stock, &r.0, 0, args), "{args:?}");
    }
}
