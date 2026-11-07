//! `log`, `show` and `--left-right` against stock git when committer dates are skewed.
//!
//! A walk with an UNINTERESTING commit runs `limit_list()` (revision.c:1446-1516),
//! which stops `SLOP` uninteresting pops after nothing interesting is queued and the
//! queue has fallen behind the last interesting date (`still_interesting()`,
//! revision.c:1306-1327). A hidden side whose commits are dated before the history
//! it shares with the shown side therefore stops short, and git lists what it never
//! reached. `SYMMETRIC_LEFT` travels only through the commits that walk popped
//! (revision.c:1182-1219), and for `A...B` the parent lists
//! `mark_parents_uninteresting()` can follow are the ones `get_merge_bases()`
//! parsed (commit-reach.c). zvcs hid the whole ancestry of every negative tip.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

/// `commits` is `(branch, parents by branch, committer date)`; each commit adds a file
/// named after its branch. Built with stock `fast-import` so the dates are exact.
fn repo(stock: &str, tag: &str, commits: &[(&str, &[&str], i64)]) -> Repo {
    let dir = std::env::temp_dir().join(format!("zvcs-log-slop-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(run(stock, &dir, &["init", "-q", "-b", "main"]).2, 0);
    let mut stream = String::new();
    for (i, (name, parents, date)) in commits.iter().enumerate() {
        stream += &format!("commit refs/heads/{name}\nmark :{}\n", i + 1);
        stream += &format!("committer C <c@example.com> {date} +0000\ndata {}\n{name}\n", name.len());
        for (k, p) in parents.iter().enumerate() {
            let mark = commits.iter().position(|c| c.0 == *p).unwrap() + 1;
            stream += &format!("{} :{mark}\n", if k == 0 { "from" } else { "merge" });
        }
        stream += &format!("M 644 inline {name}.t\ndata {}\n{name}\n\n", name.len() + 1);
    }
    let mut child = Command::new(stock)
        .args(["fast-import", "--quiet"])
        .current_dir(&dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    Repo(dir)
}

fn same(stock: &str, repo: &Repo, cases: &[&[&str]]) {
    for args in cases {
        assert_eq!(run(BIN, &repo.0, args), run(stock, &repo.0, args), "{args:?}");
    }
}

/// `c1..c5` dated forward; `y` builds on `c5` later still; `x` builds on `c5` with
/// seven commits dated before `c1`, under a tip dated after everything.
const SKEWED: &[(&str, &[&str], i64)] = &[
    ("c1", &[], 1_000_000_010),
    ("c2", &["c1"], 1_000_000_020),
    ("c3", &["c2"], 1_000_000_030),
    ("c4", &["c3"], 1_000_000_040),
    ("c5", &["c4"], 1_000_000_050),
    ("y1", &["c5"], 1_000_000_600),
    ("y2", &["y1"], 1_000_000_650),
    ("x1", &["c5"], 999_000_043),
    ("x2", &["x1"], 999_000_044),
    ("x3", &["x2"], 999_000_045),
    ("x4", &["x3"], 999_000_046),
    ("x5", &["x4"], 999_000_047),
    ("x6", &["x5"], 999_000_048),
    ("x7", &["x6"], 999_000_049),
    ("x8", &["x7"], 1_000_000_700),
];

#[test]
fn a_hidden_side_dated_behind_the_shown_one_stops_after_slop() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "slop", SKEWED);
    // Stock lists c5..c1 for every one of these: the walk from `x8` gave up
    // before `x1` could paint `c5`.
    same(
        stock,
        &r,
        &[
            &["log", "--format=%h %s", "x8..y2"],
            &["log", "--format=%h %s", "^x8", "y2"],
            &["log", "--format=%m%h %s", "--left-right", "^x8", "y2"],
            &["log", "--format=%h %s", "--boundary", "x8..y2"],
            &["log", "--format=%h %s", "--topo-order", "x8..y2"],
            &["log", "--format=%h %s", "--graph", "x8..y2"],
            &["log", "--format=%h %s", "--reverse", "x8..y2"],
            &["log", "--format=%h %s", "-n", "3", "x8..y2"],
            &["log", "--format=%h %s", "--first-parent", "x8..y2"],
            &["log", "--format=%h %s", "--exclude-first-parent-only", "^x8", "y2"],
            &["log", "--format=%h %s", "--since=1000000030", "x8..y2"],
            &["log", "--format=%h %s", "x8..y2", "--", "c3.t"],
            &["show", "-s", "--format=%h %s", "x8..y2"],
            &["show", "--format=%h %s", "^x8", "y2"],
            &["log", "--format=%m%h %s", "--left-right", "x8...y2"],
        ],
    );
}

#[test]
fn symmetric_sides_follow_what_the_merge_base_paint_parsed() {
    let Some(stock) = stock_git::stock_git() else { return };
    // `m` is the merge base of `l` and `r`; `l` reaches it through a merge whose
    // second parent is an old-dated chain, so the limit_list() paint from the base
    // stops before the chain's bottom, and only the commits get_merge_bases() read
    // let the mark go further.
    let r = repo(
        stock,
        "symmetric",
        &[
            ("b1", &[], 1_000_000_100),
            ("b2", &["b1"], 1_000_000_200),
            ("b3", &["b2"], 1_000_000_300),
            ("m", &["b3"], 1_000_000_400),
            ("o1", &["b1"], 999_000_001),
            ("o2", &["o1"], 999_000_002),
            ("o3", &["o2"], 999_000_003),
            ("o4", &["o3"], 999_000_004),
            ("o5", &["o4"], 999_000_005),
            ("o6", &["o5"], 999_000_006),
            ("o7", &["o6"], 999_000_007),
            ("mo", &["m", "o7"], 1_000_000_500),
            ("l", &["mo"], 1_000_000_600),
            ("r1", &["m"], 1_000_000_700),
            ("r", &["r1", "o3"], 1_000_000_800),
        ],
    );
    same(
        stock,
        &r,
        &[
            &["log", "--format=%m%h %s", "--left-right", "l...r"],
            &["log", "--format=%m%h %s", "--left-right", "r...l"],
            &["log", "--format=%m%h %s", "--left-right", "--boundary", "l...r"],
            &["log", "--format=%m%h %s", "--left-right", "--cherry-mark", "l...r"],
            &["log", "--format=%m%h %s", "--left-only", "l...r"],
            &["show", "-s", "--format=%m%h %s", "l...r"],
            &["log", "--format=%h %s", "^l", "r"],
        ],
    );
}
