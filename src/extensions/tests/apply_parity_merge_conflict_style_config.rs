//! `git_apply_config()` ends in `git_xmerge_config()` (apply.c:54), so `merge.conflictStyle`
//! is judged when `git apply` reads its configuration — ahead of the command line, `-h`
//! included — and an unusable value is `error: unknown style '<v>' given for
//! 'merge.conflictstyle'` plus `fatal: unable to parse … from command-line config`, 128.
//! `git am` links `apply` in: it has printed `Applying: <subject>` by then, the `die()` ends
//! `am` itself, and the half-made `.git/rebase-apply` stays behind.
//!
//! zvcs never read the key for either; `am` went on to call the patch failed.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A repository on its first commit with `one.patch` (the second commit's change) beside it.
fn fixture(stock: &str, label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-apply-style-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let run = |args: &[&str]| {
        let out = git(stock, &dir, args);
        assert_eq!(out.0, 0, "{args:?}: {out:?}");
        out.1
    };
    run(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    run(&["add", "f.txt"]);
    run(&["commit", "-q", "-m", "first"]);
    std::fs::write(dir.join("f.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    run(&["commit", "-q", "-am", "second"]);
    let patch = run(&["format-patch", "--stdout", "-1"]);
    std::fs::write(dir.parent().unwrap().join("one.patch"), patch).unwrap();
    run(&["reset", "-q", "--hard", "HEAD~1"]);
    dir
}

fn same(label: &str, build: impl Fn(&Path) -> Vec<String>) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let dir = fixture(stock, &format!("{label}-{side}"));
        let args = build(&dir.parent().unwrap().join("one.patch"));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, stdout, stderr) = git(bin, &dir, &args);
        let root = dir.parent().unwrap().to_string_lossy().into_owned();
        let state = |name: &str| dir.join(".git").join(name).exists();
        seen.push((
            code,
            stdout.replace(&root, "<root>"),
            stderr.replace(&root, "<root>"),
            state("rebase-apply"),
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            git(stock, &dir, &["rev-parse", "HEAD"]).1,
        ));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[1], seen[0], "{label}: left is zvcs, right is stock");
}

fn argv(parts: &[&str], patch: &Path) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).chain([patch.to_string_lossy().into_owned()]).collect()
}

#[test]
fn apply_refuses_a_bad_style_before_reading_the_patch() {
    same("apply", |p| argv(&["-c", "merge.conflictStyle=bogus", "apply"], p));
    same("apply-check", |p| argv(&["-c", "merge.conflictStyle=bogus", "apply", "--check"], p));
    same("apply-3way", |p| argv(&["-c", "merge.conflictStyle=bogus", "apply", "--3way"], p));
    same("apply-h", |_| vec!["-c".into(), "merge.conflictStyle=bogus".into(), "apply".into(), "-h".into()]);
}

#[test]
fn am_dies_after_announcing_the_patch_and_leaves_its_state() {
    same("am", |p| argv(&["-c", "merge.conflictStyle=bogus", "am"], p));
    same("am-3", |p| argv(&["-c", "merge.conflictStyle=bogus", "am", "-3"], p));
}

#[test]
fn a_valid_style_applies_as_before() {
    same("good-apply", |p| argv(&["-c", "merge.conflictStyle=diff3", "apply"], p));
    same("good-am", |p| argv(&["-c", "merge.conflictStyle=zdiff3", "am"], p));
}
