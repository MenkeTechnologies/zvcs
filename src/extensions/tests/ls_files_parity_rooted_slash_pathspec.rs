//! Pathspecs whose path is rooted at '/' or ends in one, run from a subdirectory
//! and from the top.
//!
//! `init_pathspec_item()` copies a `:(top)` element's path verbatim and keeps the
//! trailing '/' of any element in `item->match` (pathspec.c:481-506), so
//! `:(top)/` is the one-byte match "/" that no index entry starts with, and a
//! bare `/` reaches `prefix_path_gently()` as an absolute path outside the
//! worktree. gix's parse dropped that '/' and left the empty path, which matched
//! everything. The diagnostics quote `copyfrom` as typed (`'//'`, `'../x/'`), and
//! `add` asks `file_exists()` about the match with its '/', so `add x/` on a
//! file dies. A walk from "/" dies in `prep_exclude()` (dir.c:1713-1715), and a
//! walk whose common prefix starts with '/' stops at the empty first component
//! (dir.c:2862-2865) instead of descending the filesystem.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

/// `x`, `top`, `sub/x` and `sub/deep/y` committed, then `x` and `sub/x`
/// modified and `n`, `sub/n` left untracked.
fn fixture(side: &Side) {
    for (path, body) in [("x", "a\n"), ("top", "d\n"), ("sub/x", "b\n"), ("sub/deep/y", "c\n")] {
        side.write(path, body);
    }
    assert_eq!(side.git(&["add", "."]).code, 0);
    assert_eq!(side.git(&["commit", "-q", "-m", "files"]).code, 0);
    for (path, body) in [("x", "a\ne\n"), ("sub/x", "b\nf\n"), ("n", "new\n"), ("sub/n", "new\n")] {
        side.write(path, body);
    }
}

fn pair(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    let (s, z) = twin_repo::pair(label, stock);
    fixture(&s);
    fixture(&z);
    Some((s, z))
}

fn compare(s: &Side, z: &Side, dir: &str, args: &[&str]) {
    let (sd, zd) = (s.repo().join(dir), z.repo().join(dir));
    assert_eq!(z.run_in(&zd, &[], args), s.run_in(&sd, &[], args), "in '{dir}': {args:?}");
}

#[test]
fn a_slash_only_path_matches_nothing_or_is_outside() {
    let Some((s, z)) = pair("rooted-slash-only") else { return };
    for dir in ["sub", ""] {
        for spec in ["/", "//", ":(top)/", ":(top)//", "://", ":(literal,top)/", ":(icase,top)/", ":(glob,top)/", ":(exclude,top)/"] {
            for cmd in [&["ls-files", "--"][..], &["grep", "-n", ".", "--"], &["diff", "--name-only", "--"], &["log", "--oneline", "--"]] {
                let mut args = cmd.to_vec();
                args.push(spec);
                compare(&s, &z, dir, &args);
            }
        }
    }
}

#[test]
fn the_outside_repository_fatal_quotes_the_trailing_slash() {
    let Some((s, z)) = pair("rooted-slash-quote") else { return };
    for spec in ["../x/", "../../", "//"] {
        compare(&s, &z, "sub", &["ls-files", "--", spec]);
        compare(&s, &z, "sub", &["add", "-n", "--", spec]);
        compare(&s, &z, "sub", &["log", "--oneline", "--", spec]);
    }
}

#[test]
fn a_rooted_absolute_spec_is_a_literal_match() {
    let Some((s, z)) = pair("rooted-absolute") else { return };
    for cmd in [&["ls-files", "--"][..], &["ls-files", "-o", "--"], &["add", "-n", "--"], &["grep", "-n", ".", "--"]] {
        for spec in [":(top)/x", ":(top)/nope/x", ":(top)/x/"] {
            let mut args = cmd.to_vec();
            args.push(spec);
            compare(&s, &z, "sub", &args);
        }
        // The worktree's own absolute path under `:(top)`: no entry starts with
        // '/', and `add`'s `file_exists()` finds the file, so it says nothing.
        let (sp, zp) = (format!(":(top){}/x", s.repo().display()), format!(":(top){}/x", z.repo().display()));
        let (mut sa, mut za) = (cmd.to_vec(), cmd.to_vec());
        sa.push(&sp);
        za.push(&zp);
        assert_eq!(z.run_in(&z.repo().join("sub"), &[], &za), s.run_in(&s.repo().join("sub"), &[], &sa), "{cmd:?} :(top)<repo>/x");
    }
}

#[test]
fn a_walk_from_the_filesystem_root_dies_in_prep_exclude() {
    let Some((s, z)) = pair("rooted-prep-exclude") else { return };
    for dir in ["sub", ""] {
        compare(&s, &z, dir, &["add", "-n", "--", ":(top)/"]);
        compare(&s, &z, dir, &["ls-files", "-o", "--", ":(top)/"]);
    }
    assert!(!z.repo().join(".git/index.lock").exists(), "die() leaves no index.lock behind");
}

#[test]
fn add_of_a_file_spelled_as_a_directory_does_not_match() {
    let Some((s, z)) = pair("rooted-file-slash") else { return };
    compare(&s, &z, "", &["add", "-n", "--", "x/"]);
    compare(&s, &z, "", &["add", "-n", "--", "sub/"]);
    compare(&s, &z, "sub", &["add", "-n", "--", "deep/"]);
}
