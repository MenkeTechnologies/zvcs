//! The `@@@` header of a combined hunk names the section it sits in.
//!
//! `dump_sline()` remembers the last unmarked result line that `hunk_comment_line()`
//! accepts — one opening with an ASCII letter, `_` or `$` — on its way to each hunk,
//! and prints up to 40 bytes of it, stopping short of the last non-space byte
//! (combine-diff.c:730-738, 771-833, v2.56.0). The diff driver's `funcname` and
//! `xfuncname` play no part. With color the comment is `c_reset c_context " "
//! c_reset c_func <comment>`, and a body line's trailing CR follows its `c_reset`
//! (`show_line_to_eol()`, combine-diff.c:740-750).
//!
//! The port printed a bare `@@@ ... @@@` from `git diff`, `show` and `log`, and
//! `diff-files` sent its combined sections through the two-way painter, which
//! split the closing `@@@` and recolored the sign columns.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// The result file: plain context lines, with hunk-comment candidates of every
/// shape `hunk_comment_line()` and the 40-byte scan distinguish.
fn base_text() -> String {
    let mut s = String::new();
    for i in 1..=120 {
        let line = match i {
            5 => "x".to_owned(),
            15 => format!("_under {i}"),
            25 => format!("${i} dollar\r"),
            35 => "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ long".to_owned(),
            45 => "tab\there \t ".to_owned(),
            55 => "a           ".to_owned(),
            65 => "ab\x0cc\x0c".to_owned(),
            75 => "func foo() {".to_owned(),
            85 => "1digit".to_owned(),
            _ => format!("  line {i}"),
        };
        s.push_str(&line);
        s.push('\n');
    }
    s
}

/// Rewrite every tenth plain line (`n % 10 == step`) the way one side does.
fn edit(text: &str, step: usize, tag: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(n, l)| match (n + 1) % 10 == step && l.starts_with("  line ") {
            true => format!("  {tag} {}\n", &l["  line ".len()..]),
            false => format!("{l}\n"),
        })
        .collect()
}

/// `f` and `g` conflict in every tenth line; `g` goes through a driver whose
/// `xfuncname`/`funcname` would pick other lines if combine-diff consulted it.
/// `merged` holds the same conflict committed as a resolved merge.
fn fixture(stock: &str, tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-cc-hunk-comment-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (work, merged) = (root.join("work"), root.join("merged"));
    for dir in [&work, &merged] {
        std::fs::create_dir_all(dir).unwrap();
        let git = |args: &[&str]| run(stock, dir, args);
        git(&["init", "-q", "-b", "main", "."]);
        git(&["config", "core.autocrlf", "false"]);
        git(&["config", "diff.drv.xfuncname", "^  line (.*)$"]);
        git(&["config", "diff.fn.funcname", "^  line"]);
        std::fs::write(dir.join(".gitattributes"), "g diff=drv\nh diff=fn\n").unwrap();
        for f in ["f", "g", "h"] {
            std::fs::write(dir.join(f), base_text()).unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "base"]);
        git(&["checkout", "-q", "-b", "side"]);
        for f in ["f", "g", "h"] {
            std::fs::write(dir.join(f), edit(&base_text(), 2, "side")).unwrap();
        }
        git(&["commit", "-q", "-am", "side"]);
        git(&["checkout", "-q", "main"]);
        for f in ["f", "g", "h"] {
            std::fs::write(dir.join(f), edit(&base_text(), 2, "main")).unwrap();
        }
        git(&["commit", "-q", "-am", "main"]);
        git(&["merge", "-q", "side"]);
    }
    // Resolve `merged` by keeping both sides' lines, then commit the merge.
    for f in ["f", "g", "h"] {
        let text = std::fs::read_to_string(merged.join(f)).unwrap();
        let kept: String = text
            .lines()
            .filter(|l| !l.starts_with("<<<<<<<") && !l.starts_with("=======") && !l.starts_with(">>>>>>>"))
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(merged.join(f), kept).unwrap();
    }
    run(stock, &merged, &["add", "f", "g", "h"]);
    run(stock, &merged, &["commit", "-q", "-m", "merge"]);
    (root, work)
}

const COLORS: [&str; 6] = [
    "-c",
    "color.diff.func=yellow",
    "-c",
    "color.diff.context=blue",
    "-c",
    "color.diff.frag=magenta bold",
];

#[test]
fn combined_hunk_headers_carry_the_hunk_comment() {
    let Some(stock) = stock_git() else { return };
    let (root, work) = fixture(stock, "a");
    let merged = root.join("merged");
    let worktree: &[&[&str]] = &[
        &["diff"],
        &["diff", "-c"],
        &["diff", "--cc", "-U1"],
        &["diff", "--cc", "-U0"],
        &["diff", "-W"],
        &["diff", "--color=always"],
        &["diff", "--color=always", "--line-prefix=LP"],
        &["diff", "HEAD", "--cc"],
        &["diff-files", "-c"],
        &["diff-files", "--cc", "--color=always"],
        &["diff-files", "--cc", "--color=always", "--line-prefix=LP"],
        &["diff-files", "--cc", "--word-diff"],
        &["diff-files", "--cc", "--stat", "-p"],
        &["diff-files", "--cc", "--color-moved", "--color=always"],
    ];
    let history: &[&[&str]] = &[
        &["show"],
        &["show", "-c"],
        &["show", "--cc", "-U0"],
        &["show", "--cc", "--color=always"],
        &["show", "-c", "--color=always", "--line-prefix=LP"],
        &["log", "-p", "-c", "-1"],
        &["log", "--cc", "-1", "--format=%s"],
        &["diff-tree", "-c", "-p", "HEAD"],
        &["diff", "--cc", "HEAD", "HEAD^1", "HEAD^2"],
    ];
    for (dir, vectors) in [(&work, worktree), (&merged, history)] {
        for args in vectors.iter() {
            let want = run(stock, dir, args);
            assert!(want.0.contains("@@@ "), "{args:?}: no combined hunk in {:?}", want.0);
            assert_eq!(run(BIN, dir, args), want, "{args:?}");
            let colored: Vec<&str> = COLORS.iter().copied().chain(args.iter().copied()).collect();
            assert_eq!(run(BIN, dir, &colored), run(stock, dir, &colored), "{colored:?}");
        }
    }
    let _ = std::fs::remove_dir_all(root);
}
