//! `git add` against stock git for the gitignored-path report under pathspec magic:
//! `dir.ignored` is what `fill_directory()` collects under `DIR_COLLECT_IGNORED`, and
//! `exclude_matches_pathspec()` reads each item's `match` alone, so `:(prefix:0)`,
//! `:(top)`, `:(literal)`, `:(exclude)` and `:(attr:…)` elements name an ignored path
//! as a plain one does, an excluded directory is reported as itself, and an untracked
//! directory whose entries are all ignored is reported not at all. `stash push -u` hands
//! its pathspec to `add` as `:(prefix:0)<path>`, which is how the same gap reached it.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::path::Path;
use std::process::Command;

/// What stock git reports about `repo`, so both sides are judged by the same reader.
fn stock_view(stock: &str, repo: &Path, args: &[&str]) -> String {
    let out = Command::new(stock)
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn fixture(side: &twin_repo::Side) {
    side.write(".gitignore", "ign\n*.log\nlogs/\n");
    side.write(".gitattributes", "*.t text\n");
    side.git(&["add", ".gitignore", ".gitattributes"]);
    side.git(&["commit", "-q", "-m", "ignore rules"]);
    side.write("g", "g\n");
    side.write("ign", "ign\n");
    side.write("x.log", "x\n");
    side.write("sub/s.log", "s\n");
    side.write("logs/l", "l\n");
}

#[test]
fn ignored_report_follows_the_walk_for_every_magic() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("add-magic-ignored", stock);
    fixture(&s);
    fixture(&z);
    for args in [
        &["add", "--", ":(prefix:0)g", ":(prefix:0)ign"][..],
        &["add", "-n", "--", ":(prefix:0)g", ":(prefix:0)ign"],
        &["add", "--", "g", ":(top)ign"],
        &["add", "--", "g", ":(literal)ign"],
        &["add", "--", "g", ":(top,literal)ign"],
        &["add", "--", "g", ":!ign"],
        &["add", "-n", "--", "g", ":(exclude)logs/l"],
        &["add", "--", "g", ":(attr:text)ign"],
        &["add", "--", "g", ":(prefix:0)logs"],
        &["add", "--", "g", ":(top)logs/l"],
        &["add", "--", "g", "logs/l"],
        &["add", "--", "g", "sub/s.log"],
        &["add", "--", "g", ":(top)sub/s.log"],
        &["add", "--", "g", "sub"],
        &["add", "--", "g", "x*"],
        &["add", "-n", "--ignore-missing", "--", "g", ":(glob)ign"],
        &["add", "-n", "--ignore-missing", "--", "g", ":(icase)IGN"],
        &["add", "-n", "--ignore-missing", "--", "g", ":(top)sub/s.log"],
        &["add", "-f", "-n", "--", "g", ":(prefix:0)ign"],
    ] {
        for side in [&s, &z] {
            side.git(&["reset", "-q"]);
        }
        assert_eq!(z.git(args), s.git(args), "{args:?}");
        assert_eq!(
            stock_view(stock, &z.repo(), &["ls-files", "--stage"]),
            stock_view(stock, &s.repo(), &["ls-files", "--stage"]),
            "index after {args:?}"
        );
    }
}

#[test]
fn stash_push_u_dies_on_an_ignored_pathspec_with_the_rest_staged() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("stash-u-ignored", stock);
    fixture(&s);
    fixture(&z);
    for args in [&["stash", "push", "-u", "--", "g", "ign"][..], &["stash", "push", "-u", "--", "ign"]] {
        for side in [&s, &z] {
            side.git(&["reset", "-q"]);
        }
        assert_eq!(z.git(args), s.git(args), "{args:?}");
        for view in [&["status", "--porcelain"][..], &["stash", "list"], &["for-each-ref"], &["fsck"]] {
            assert_eq!(
                stock_view(stock, &z.repo(), view),
                stock_view(stock, &s.repo(), view),
                "{view:?} after {args:?}"
            );
        }
    }
}
