//! `git status` lists untracked and ignored paths from `wt_status_collect_untracked()`
//! (wt-status.c:806-850): `fill_directory()` over `s->pathspec` with
//! `DIR_SHOW_OTHER_DIRECTORIES | DIR_HIDE_EMPTY_DIRECTORIES` unless `-uall`, and
//! `DIR_SHOW_IGNORED_TOO[_MODE_MATCHING]` under `--ignored`.
//!
//! * A walk from the "/" that `:(top)/` names cannot be split into directories, and
//!   `prep_exclude()` dies "oops in prep_exclude" (dir.c:1713-1715) before anything is
//!   printed, leaving no `index.lock` behind; `-uno` never walks.
//! * `treat_path()` skips the pathspec test for an excluded path (dir.c:2490-2500), so
//!   `--ignored=matching` reports an ignored directory a `:(glob)` or `:(attr:…)`
//!   pathspec never names.
//! * `core.ignoreCase` plays no part in matching the pathspec: `upper` does not name
//!   `UPPER`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

/// Untracked files at every depth, an untracked directory, ignored files and an
/// ignored directory, a nested repository and a modified tracked file.
fn populate(side: &Side) {
    side.write(".gitignore", "*.ign\nign_dir/\n");
    side.write("sub/tracked", "s\n");
    side.write("sub/deep/tracked", "d\n");
    side.git(&["add", ".gitignore", "sub"]);
    side.git(&["commit", "-q", "-m", "layout"]);
    for path in ["untracked", "sub/untracked", "sub/deep/untracked", "udir/a", "udir/inner/b", "UPPER"] {
        side.write(path, "u\n");
    }
    for path in ["x.ign", "sub/y.ign", "ign_dir/z"] {
        side.write(path, "i\n");
    }
    let nested = side.repo().join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    side.run_in(&nested, &[], &["init", "-q", "-b", "main", "."]);
    side.write("nested/f", "n\n");
    side.write("a", "1\n2\n3\nmodified\n");
}

fn pair(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    let (s, z) = twin_repo::pair(label, stock);
    populate(&s);
    populate(&z);
    Some((s, z))
}

const MODES: [&[&str]; 8] = [
    &["status", "--porcelain"],
    &["status", "--porcelain", "-uall"],
    &["status", "--porcelain", "-uno"],
    &["status", "--porcelain", "--ignored"],
    &["status", "--porcelain", "--ignored=matching"],
    &["status", "--porcelain", "--ignored=traditional", "-uall"],
    &["status", "--porcelain=v2", "--ignored=matching", "-uall"],
    &["status"],
];

fn compare(s: &Side, z: &Side, dir: &str, args: &[&str]) {
    let (sd, zd) = (s.repo().join(dir), z.repo().join(dir));
    assert_eq!(z.run_in(&zd, &[], args), s.run_in(&sd, &[], args), "{dir}: {args:?}");
    assert_eq!(
        z.repo().join(".git/index.lock").exists(),
        s.repo().join(".git/index.lock").exists(),
        "{dir}: index.lock after {args:?}"
    );
}

#[test]
fn a_walk_from_the_root_dies_in_prep_exclude() {
    let Some((s, z)) = pair("status-dir-walk-top") else { return };
    for dir in [".", "sub"] {
        for mode in MODES {
            let args: Vec<&str> = mode.iter().copied().chain(["--", ":(top)/"]).collect();
            compare(&s, &z, dir, &args);
        }
    }
    let died = s.git(&["status", "--porcelain", "--", ":(top)/"]);
    assert_eq!((died.code, died.stderr.as_str()), (128, "fatal: oops in prep_exclude\n"));
}

#[test]
fn an_ignored_directory_is_reported_without_a_pathspec_test() {
    let Some((s, z)) = pair("status-dir-walk-ignored") else { return };
    for spec in [":(glob)**/untracked", ":(attr:-text)", ":(glob)sub/*"] {
        for mode in MODES {
            let args: Vec<&str> = mode.iter().copied().chain(["--", spec]).collect();
            compare(&s, &z, ".", &args);
        }
    }
    let out = s.git(&["status", "--porcelain", "--ignored=matching", "--", ":(attr:-text)"]);
    assert_eq!(out.stdout, "!! ign_dir/\n");
}

#[test]
fn core_ignore_case_does_not_fold_the_pathspec() {
    let Some((s, z)) = pair("status-dir-walk-icase") else { return };
    s.git(&["config", "core.ignoreCase", "true"]);
    z.git(&["config", "core.ignoreCase", "true"]);
    for spec in ["upper", "u*", ":(icase)upper"] {
        for mode in MODES {
            let args: Vec<&str> = mode.iter().copied().chain(["--", spec]).collect();
            compare(&s, &z, ".", &args);
        }
    }
}

#[test]
fn magic_pathspecs_from_the_top_and_from_a_subdirectory() {
    let Some((s, z)) = pair("status-dir-walk-magic") else { return };
    let specs = [":(top)", ":(top)sub", ":/", "/", ":(exclude)sub", ":(top)sub/deep", "udir", ":(top)nested", ":(top)ign_dir", "sub/", "nested/"];
    for dir in [".", "sub"] {
        for spec in specs {
            for mode in MODES {
                let args: Vec<&str> = mode.iter().copied().chain(["--", spec]).collect();
                compare(&s, &z, dir, &args);
            }
        }
    }
}
