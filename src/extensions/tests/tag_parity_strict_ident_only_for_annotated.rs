//! `git tag` asks for a strict committer identity only where it writes a tag object:
//! `create_tag()` formats the `tagger` header with `git_committer_info(IDENT_STRICT)`
//! (builtin/tag.c:315-323), after `cmd_tag()` has validated the name and refused an
//! existing tag and after `create_tag()` has typed the object (builtin/tag.c:307-309).
//! Listing, `-n`, `-d`, `-v` and a lightweight tag never format a strict ident, and the
//! reflog a lightweight tag writes takes the non-strict default (ident.c:474-485).
//!
//! zvcs refused every `git tag` up front, so a clone with no `user.email` could not
//! even list its tags on a host whose name has no domain.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// `git <args>` in `dir` with no identity in the environment beyond `extra`.
fn run(bin: &str, dir: &Path, home: &Path, extra: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .envs(extra.iter().copied())
        .output()
        .expect("run git");
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.code().unwrap_or(-1), text(&out.stdout), text(&out.stderr))
}

const WHO: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "A U Thor"),
    ("GIT_AUTHOR_EMAIL", "author@example.com"),
    ("GIT_COMMITTER_NAME", "C O Mitter"),
    ("GIT_COMMITTER_EMAIL", "committer@example.com"),
];

/// A repository with one commit and one annotated tag, then `user.useConfigOnly` and
/// no identity, so a strict lookup refuses on every host.
fn fixture(stock: &str, root: &Path) -> PathBuf {
    let home = root.join("home");
    let repo = root.join("r");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q"][..],
        &["commit", "-q", "--allow-empty", "-m", "c"],
        &["tag", "-a", "-m", "old", "old"],
        &["config", "user.useConfigOnly", "true"],
        &["config", "core.logAllRefUpdates", "always"],
    ] {
        assert_eq!(run(stock, &repo, &home, &WHO, args).0, 0, "{args:?}");
    }
    repo
}

#[test]
fn only_an_annotated_tag_needs_a_strict_identity() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-tag-strict-ident-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let sides = [(stock, base.join("s")), (ZVCS, base.join("z"))];
    let repos: Vec<PathBuf> = sides.iter().map(|(_, root)| fixture(stock, root)).collect();

    let cases: &[(&[(&str, &str)], &[&str])] = &[
        (&[], &["tag"]),
        (&[], &["tag", "-l", "-n"]),
        (&[], &["tag", "light"]),
        (&[], &["tag", "-d", "light"]),
        (&[], &["tag", "-v", "old"]),
        // Refused before `create_tag()` asks for the tagger.
        (&[], &["tag", "-a", "-m", "m", "HEAD"]),
        (&[], &["tag", "-a", "-m", "m", "old"]),
        (&[], &["tag", "-a", "-m", "m", "x", "0123456789012345678901234567890123456789"]),
        // Refused where it asks.
        (&[], &["tag", "-a", "-m", "m", "ann"]),
        (&[], &["tag", "-m", "m", "ann"]),
        (&[], &["tag", "-f", "-a", "-m", "m", "old"]),
        (&[("GIT_COMMITTER_EMAIL", "c@x")], &["tag", "-a", "-m", "m", "ann"]),
        (&[("GIT_COMMITTER_EMAIL", "c@x"), ("GIT_COMMITTER_NAME", "")], &["tag", "-a", "-m", "m", "ann"]),
        (
            &[("GIT_COMMITTER_EMAIL", "c@x"), ("GIT_COMMITTER_NAME", "c"), ("GIT_COMMITTER_DATE", "bogus")],
            &["tag", "-a", "-m", "m", "ann"],
        ),
        (&[("GIT_COMMITTER_EMAIL", "c@x"), ("GIT_COMMITTER_NAME", "c")], &["tag", "-a", "-m", "m", "ann"]),
        (&[], &["tag", "light2"]),
    ];
    for (env, args) in cases {
        let home = |i: usize| sides[i].1.join("home");
        let want = run(stock, &repos[0], &home(0), env, args);
        let got = run(ZVCS, &repos[1], &home(1), env, args);
        assert_eq!(got, want, "{env:?} {args:?}");
    }
    assert_eq!(run(stock, &repos[0], &base, &[], &["tag"]).1, "ann\nlight2\nold\n");

    // The refs, and the reflog the lightweight tag wrote with the non-strict default.
    for what in [&["for-each-ref"][..], &["reflog", "list"], &["reflog", "show", "refs/tags/light2"]] {
        let s = run(stock, &repos[0], &base, &[], what);
        let z = run(stock, &repos[1], &base, &[], what);
        assert_eq!(z, s, "{what:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
