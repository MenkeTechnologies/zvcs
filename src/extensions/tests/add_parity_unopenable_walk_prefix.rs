//! `cmd_add()` runs `fill_directory()` once (builtin/add.c:504-511). When the pathspec's common
//! prefix leads through a directory to a file, `sub/x/` with `sub/x` a file, that one walk's
//! `opendir()` fails and `warning: could not open directory 'sub/x/': Not a directory` is printed
//! once (dir.c:2585-2587), ahead of the "did not match any files" fatal. Without `-f` the walk is
//! the `DIR_COLLECT_IGNORED` one that collects `dir.ignored`; with `-f` it is the same walk
//! without that flag.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

/// `t`, `sub/f` and `.gitignore` committed, then `sub/x`, `untr` and an ignored `ign.o` left
/// untracked.
fn fixture(side: &Side) {
    for (path, body) in [("t", "a\n"), ("sub/f", "b\n"), (".gitignore", "*.o\n")] {
        side.write(path, body);
    }
    assert_eq!(side.git(&["add", "."]).code, 0);
    assert_eq!(side.git(&["commit", "-q", "-m", "files"]).code, 0);
    for (path, body) in [("sub/x", "c\n"), ("untr", "u\n"), ("ign.o", "i\n")] {
        side.write(path, body);
    }
}

fn compare(s: &Side, z: &Side, dir: &str, args: &[&str]) {
    let (sd, zd) = (s.repo().join(dir), z.repo().join(dir));
    assert_eq!(z.run_in(&zd, &[], args), s.run_in(&sd, &[], args), "in '{dir}': {args:?}");
}

#[test]
fn a_file_under_a_directory_spelled_as_a_directory_warns_once() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("add-unopenable-prefix", stock);
    fixture(&s);
    fixture(&z);
    let out = s.run_in(&s.repo(), &[], &["add", "-n", "--", "sub/x/"]);
    assert_eq!(out.stderr.matches("could not open directory").count(), 1, "{out:?}");
    for args in [
        &["add", "-n", "--", "sub/x/"][..],
        &["add", "--", "sub/x/"],
        &["add", "-n", "--", "sub/f/"],
        &["add", "-n", "-f", "--", "sub/x/"],
        &["add", "-n", "--ignore-missing", "--", "sub/x/"],
        &["add", "-n", "--", "sub/x/a", "sub/x/b"],
        &["add", "-n", "--", "sub/x/", "untr/"],
        &["add", "-n", "--", "sub/x/", "ign.o"],
    ] {
        compare(&s, &z, "", args);
    }
    compare(&s, &z, "sub", &["add", "-n", "--", "x/"]);
    compare(&s, &z, "sub", &["add", "-n", "--", "f/"]);
}
