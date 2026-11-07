//! `git clean` deletes what `fill_directory()` lists under the `dir.flags`
//! `cmd_clean()` sets (builtin/clean.c:959-1011), corrected by
//! `correct_untracked_entries()` (builtin/clean.c:887-917).
//!
//! * A walk from the "/" that `:(top)/` names dies "oops in prep_exclude"
//!   (dir.c:1713-1715) with nothing removed.
//! * `treat_path()` skips the pathspec test for an excluded path (dir.c:2490-2500), so
//!   `-X` removes an ignored directory a `:(glob)` or `:(attr:…)` pathspec never names.
//! * `core.ignoreCase` plays no part in matching the pathspec: `upper` does not name
//!   `UPPER`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::path::Path;
use twin_repo::Side;

/// Untracked files at every depth, an untracked directory, ignored files and an
/// ignored directory, and a nested repository.
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
}

/// Every path under `dir` but the `.git` directories, sorted.
fn worktree(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            out.push(path.strip_prefix(root).unwrap().to_string_lossy().into_owned());
            if path.symlink_metadata().unwrap().is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

const MODES: [&[&str]; 9] = [
    &["clean", "-n"],
    &["clean", "-nd"],
    &["clean", "-ndx"],
    &["clean", "-nX"],
    &["clean", "-ndX"],
    &["clean", "-ndx", "-e", "untracked"],
    &["clean", "-fd"],
    &["clean", "-ffdx"],
    &["clean", "-fX"],
];

/// The stock and zvcs twins, populated, with `core.ignoreCase` set when `icase`.
fn twins(label: &str, icase: bool) -> Option<(Side, Side)> {
    let (s, z) = twin_repo::pair(label, stock_git::stock_git()?);
    for side in [&s, &z] {
        populate(side);
        if icase {
            side.git(&["config", "core.ignoreCase", "true"]);
        }
    }
    Some((s, z))
}

/// Run every mode with `spec` from `dir`, comparing the output and the worktree
/// left. The dry runs share `dry`; each mode that removes gets fresh twins.
fn compare(label: &str, dry: &(Side, Side), dir: &str, spec: &str, icase: bool) {
    for mode in MODES {
        let args: Vec<&str> = mode.iter().copied().chain(["--", spec]).collect();
        let fresh;
        let (s, z) = if mode[1].starts_with("-n") {
            (&dry.0, &dry.1)
        } else {
            let Some(pair) = twins(label, icase) else { return };
            fresh = pair;
            (&fresh.0, &fresh.1)
        };
        let (sd, zd) = (s.repo().join(dir), z.repo().join(dir));
        assert_eq!(z.run_in(&zd, &[], &args), s.run_in(&sd, &[], &args), "{dir}: {args:?}");
        assert_eq!(worktree(&z.repo()), worktree(&s.repo()), "{dir}: worktree after {args:?}");
    }
}

#[test]
fn a_walk_from_the_root_dies_in_prep_exclude() {
    let Some(dry) = twins("clean-dir-walk-top-n", false) else { return };
    for dir in [".", "sub"] {
        compare("clean-dir-walk-top", &dry, dir, ":(top)/", false);
    }
    let died = dry.0.git(&["clean", "-fd", "--", ":(top)/"]);
    assert_eq!((died.code, died.stderr.as_str()), (128, "fatal: oops in prep_exclude\n"));
}

#[test]
fn an_ignored_directory_is_removed_without_a_pathspec_test() {
    let Some(dry) = twins("clean-dir-walk-ignored-n", false) else { return };
    for spec in [":(glob)**/untracked", ":(attr:-text)"] {
        compare("clean-dir-walk-ignored", &dry, ".", spec, false);
    }
    let out = dry.0.git(&["clean", "-nX", "--", ":(attr:-text)"]);
    assert_eq!(out.stdout, "Would remove ign_dir/\n");
}

#[test]
fn core_ignore_case_does_not_fold_the_pathspec() {
    let Some(dry) = twins("clean-dir-walk-icase-n", true) else { return };
    for spec in ["upper", "u*", ":(icase)upper"] {
        compare("clean-dir-walk-icase", &dry, ".", spec, true);
    }
}

#[test]
fn magic_pathspecs_from_the_top_and_from_a_subdirectory() {
    let Some(dry) = twins("clean-dir-walk-magic-n", false) else { return };
    let specs = [":(top)", ":(top)sub", ":/", ":(exclude)sub", ":(top)nested", ":(top)ign_dir", "sub/", "nested/"];
    for dir in [".", "sub"] {
        for spec in specs {
            compare("clean-dir-walk-magic", &dry, dir, spec, false);
        }
    }
}
