//! `format-patch --cover-letter`'s shortlog drops a `[PATCH...]` prefix.
//!
//! `shortlog_add_commit()` hands each `%s` to `insert_one_record()`
//! (builtin/shortlog.c:57-85), which skips leading whitespace, drops a
//! `[PATCH...]` prefix whose `]` is on the first line, skips the blanks after it
//! and folds the rest with `format_subject()`. zvcs listed the subject as is, so
//! a commit titled `[PATCH] fix` showed as `[PATCH] fix` in the cover letter.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in identical
//! repositories.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
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

const SUBJECTS: &[&str] = &[
    "[PATCH] whitespace: add trailing ws",
    "[PATCH v2 3/7] numbered: prefix",
    "[PATCH]",
    "   [PATCH]    indented",
    "[PATCHES are not a prefix",
    "[PATCH no closing bracket",
    "[RFC] not a PATCH prefix",
    "plain subject",
];

fn repo(label: &str, stock: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-fp-cover-patch-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(&work).unwrap();
    git(stock, &work, &["init", "-q", "-b", "main", "."]);
    git(stock, &work, &["commit", "-q", "--allow-empty", "-m", "base"]);
    for (i, s) in SUBJECTS.iter().enumerate() {
        std::fs::write(work.join("f.txt"), format!("{i}\n")).unwrap();
        git(stock, &work, &["add", "f.txt"]);
        git(stock, &work, &["commit", "-q", "--cleanup=verbatim", "-m", s]);
    }
    work
}

fn same(args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let s = repo("stock", stock);
    let z = repo("zvcs", stock);
    let want = git(stock, &s, args);
    let got = git(ZVCS, &z, args);
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
    assert_eq!(got, want, "git {args:?}: left is zvcs, right is stock");
}

#[test]
fn cover_letter_shortlog_drops_the_patch_prefix() {
    same(&["format-patch", "--no-signature", "--cover-letter", "--stdout", "main~8..main"]);
    same(&["format-patch", "--no-signature", "--cover-letter", "-n", "--stdout", "-3"]);
    same(&["format-patch", "--no-signature", "--cover-letter", "--cover-from-description=subject", "--stdout", "-8"]);
}
