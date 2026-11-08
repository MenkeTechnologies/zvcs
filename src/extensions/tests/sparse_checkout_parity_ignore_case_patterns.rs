//! Sparse patterns under `core.ignoreCase`, against stock git.
//!
//! A non-cone list is matched by `path_matches_pattern_list()` through `match_basename()`
//! and `match_pathname()`, whose literal comparisons are `fspathcmp()`/`fspathncmp()` —
//! `strcasecmp()`/`strncasecmp()` while `repo_ignore_case()` holds (dir.c:127-140) — and
//! whose wildmatch gains `WM_CASEFOLD` in `fnmatch_icase_mem()` (dir.c:205-206). A cone
//! list compares its hashmap names with `fspathncmp()` the same way. So with
//! `core.ignoreCase=true` a `/P/` line keeps `p/`, for `sparse-checkout set/add/reapply`,
//! `read-tree -mu`, `checkout` and `check-rules` alike, a `--rules-file` included. zvcs
//! matched non-cone lists, and the cone lists `unpack_trees()` and `check-rules
//! --rules-file` read, case-sensitively.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::io::Write;
use std::process::{Command, Stdio};

use twin_repo::{Out, Side};

fn world(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        for (path, body) in [("p/f", "1\n"), ("p/b/g", "2\n"), ("c/h", "3\n"), ("D/x", "4\n"), ("top", "5\n"), ("Readme", "6\n")] {
            side.write(path, body);
        }
        side.git(&["add", "."]);
        let commit = side.git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "tree"]);
        assert_eq!(commit.code, 0, "{commit:?}");
        side.git(&["config", "core.ignoreCase", "true"]);
    }
    Some((s, z))
}

/// `git <args>` in the repository with `input` on stdin, in the environment
/// [`Side::run_in`] sets up.
fn piped(side: &Side, args: &[&str], input: &str) -> Out {
    let mut child = Command::new(&side.bin)
        .args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
        .env("GIT_CEILING_DIRECTORIES", side.root.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let root = side.root.to_string_lossy().into_owned();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
    Out { code: out.status.code().unwrap_or(-1), stdout: text(&out.stdout), stderr: text(&out.stderr) }
}

/// Run `args` on both sides and compare the answer and the skip-worktree bits it left.
#[track_caller]
fn same(s: &Side, z: &Side, args: &[&str]) {
    assert_eq!(z.git(args), s.git(args), "git {args:?}: left is zvcs, right is stock");
    let flags = ["ls-files", "-t"];
    assert_eq!(z.git(&flags), s.git(&flags), "ls-files -t after git {args:?}");
}

#[test]
fn non_cone_lines_fold_case_for_every_reader() {
    let Some((s, z)) = world("sc-icase-noncone") else { return };
    same(&s, &z, &["sparse-checkout", "set", "--no-cone", "/P/"]);
    assert!(s.git(&["ls-files", "-t"]).stdout.contains("H p/f\n"));
    same(&s, &z, &["sparse-checkout", "add", "/D/"]);
    same(&s, &z, &["sparse-checkout", "add", "readme"]);

    let paths = "p/f\nP/F\nd/x\nREADME\ntop\nc/h\n";
    let args = ["sparse-checkout", "check-rules"];
    assert_eq!(piped(&z, &args, paths), piped(&s, &args, paths));
    for side in [&s, &z] {
        side.write("../rules", "/p/B/\nTOP\n");
    }
    let args = ["sparse-checkout", "check-rules", "--no-cone", "--rules-file", "../rules"];
    assert_eq!(piped(&z, &args, "p/b/g\nP/f\ntop\n"), piped(&s, &args, "p/b/g\nP/f\ntop\n"));

    // A negation folds case as well: `!c/H` takes `c/h` back out of `/C/`.
    for side in [&s, &z] {
        side.write(".git/info/sparse-checkout", "/C/\n!c/H\n/TOP\n");
    }
    same(&s, &z, &["sparse-checkout", "reapply"]);
    for side in [&s, &z] {
        side.write(".git/info/sparse-checkout", "/P/\n*.TXT\n/README\n");
    }
    same(&s, &z, &["read-tree", "-mu", "HEAD"]);
    for side in [&s, &z] {
        side.write(".git/info/sparse-checkout", "/D/\n");
    }
    same(&s, &z, &["checkout", "-f", "HEAD"]);

    // Without `core.ignoreCase` the same lines match nothing.
    for side in [&s, &z] {
        side.git(&["config", "core.ignoreCase", "false"]);
        side.write(".git/info/sparse-checkout", "/P/\n/TOP\n");
    }
    same(&s, &z, &["sparse-checkout", "reapply"]);
}

#[test]
fn cone_lists_from_the_file_and_a_rules_file_fold_case() {
    let Some((s, z)) = world("sc-icase-cone") else { return };
    same(&s, &z, &["sparse-checkout", "set", "--cone", "p"]);
    for side in [&s, &z] {
        side.write(".git/info/sparse-checkout", "/*\n!/*/\n/P/\n/d/\n");
    }
    same(&s, &z, &["read-tree", "-mu", "HEAD"]);
    same(&s, &z, &["sparse-checkout", "reapply"]);

    for side in [&s, &z] {
        side.write("../rules", "P\nd/X\n");
    }
    let paths = "p/f\nP/b/g\nD/x/y\nd/X/q\nc/h\n";
    let args = ["sparse-checkout", "check-rules", "--rules-file", "../rules"];
    let stock = piped(&s, &args, paths);
    assert_eq!(stock.stdout, "p/f\nP/b/g\nD/x/y\nd/X/q\n");
    assert_eq!(piped(&z, &args, paths), stock);
}
