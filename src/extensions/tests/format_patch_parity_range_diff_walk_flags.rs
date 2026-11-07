//! `format-patch --range-diff=<prev>` against stock git when the series' own walk
//! has already flagged `<prev>`'s endpoints.
//!
//! `infer_range_diff_ranges()` runs after the walk (builtin/log.c:2395-2400), and
//! `is_range_diff_range()` (range-diff.c:616-641) counts a pending object as
//! negative by its `UNINTERESTING` bit, which lives on the object: a `<prev>` tip
//! the series excluded makes `<prev>` no range, so it becomes `<head>..<prev>`.
//! Neither caller looks at `show_range_diff()`'s result (builtin/log.c:1484,
//! log-tree.c:736), so a range the inner `git log` cannot read leaves the block
//! empty, says so on stderr and the series goes on at exit 0. zvcs classified
//! `<prev>` by its spelling, refused `^<rev>` as a bad revision and stopped the
//! whole command on the inner failure.
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

/// `main`: base, base2, upstream; `v1` forks from base2 (one, two, three); `v2`
/// rebuilds the series on upstream (one, a changed two, four).
fn repo(stock: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("zvcs-fp-range-diff-flags-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut date = 1_000_000_000;
    let mut git = |args: &[&str]| {
        date += 100;
        let (_, err, code) = run(stock, &dir, date, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    git(&["init", "-q", "-b", "main"]);
    let commit = |git: &mut dyn FnMut(&[&str]), file: &str, line: &str, msg: &str| {
        let path = dir.join(file);
        let mut body = std::fs::read_to_string(&path).unwrap_or_default();
        body += &format!("{line}\n");
        std::fs::write(&path, body).unwrap();
        git(&["add", file]);
        git(&["commit", "-q", "-m", msg]);
    };
    commit(&mut git, "base.t", "base", "base");
    commit(&mut git, "base.t", "base2", "base2");
    git(&["checkout", "-q", "-b", "v1"]);
    commit(&mut git, "a.t", "one", "feat one");
    commit(&mut git, "b.t", "two", "feat two");
    commit(&mut git, "c.t", "three", "feat three");
    git(&["checkout", "-q", "main"]);
    commit(&mut git, "up.t", "up", "upstream");
    git(&["checkout", "-q", "-b", "v2"]);
    commit(&mut git, "a.t", "one", "feat one");
    commit(&mut git, "b.t", "TWO", "feat two");
    commit(&mut git, "d.t", "four", "feat four");
    Repo(dir)
}

#[test]
fn prev_is_classified_with_the_flags_the_walk_left() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock);
    for args in [
        // `main` is UNINTERESTING after the walk, so neither is a range.
        &["format-patch", "--stdout", "--range-diff=v1..main", "main..v2"][..],
        &["format-patch", "--stdout", "--range-diff=v1..main", "-1"],
        &["format-patch", "--stdout", "--range-diff=v1...main", "main..v2"],
        &["format-patch", "--stdout", "--range-diff=main~1..main", "main..v2"],
        // A negative-only `<prev>` resolves, is no range, and its inner log fails.
        &["format-patch", "--stdout", "--range-diff=^main", "main..v2"],
        &["format-patch", "--stdout", "--range-diff=^main", "v1", "main..v2"],
        &["format-patch", "--stdout", "--range-diff=^v1", "-1"],
        &["format-patch", "--stdout", "--range-diff=v1..v1", "main..v2"],
        // `add_parents_only()`'s spellings.
        &["format-patch", "--stdout", "--range-diff=v1^!", "main..v2"],
        &["format-patch", "--stdout", "--range-diff=v1^-1", "main..v2"],
        // Unchanged: ranges and tips the walk left alone.
        &["format-patch", "--stdout", "--range-diff=main..v1", "main..v2"],
        &["format-patch", "--stdout", "--range-diff=main...v1", "main..v2"],
        &["format-patch", "--stdout", "--range-diff=v1", "main..v2"],
        &["format-patch", "--stdout", "--range-diff=nosuch", "main..v2"],
        &["format-patch", "--stdout", "--range-diff=nosuch..v1", "main..v2"],
    ] {
        assert_eq!(run(BIN, &r.0, 1_100_000_000, args), run(stock, &r.0, 1_100_000_000, args), "{args:?}");
    }
}
