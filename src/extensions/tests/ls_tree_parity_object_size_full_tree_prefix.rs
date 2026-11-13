//! Four `git ls-tree` paths where zvcs answered differently.
//!
//! * A blob the object database cannot answer for: `show_tree_long()` prints
//!   `BAD` in the size column and carries on (builtin/ls-tree.c:222-231), while
//!   `%(objectsize)` dies `could not get object info about '<oid>'` from
//!   `expand_objectsize()` (builtin/ls-tree.c:34-38) after the lines already
//!   written. zvcs failed both with its own error and no output.
//! * `--full-tree` sets `prefix = NULL` before `parse_pathspec()`
//!   (builtin/ls-tree.c:387-388, :420-423), so `../x` from a subdirectory is
//!   outside the repository; zvcs checked it against the current directory.
//! * `<rev>:../x` climbing out of the work tree is `prefix_path()`'s die inside
//!   the object-name lookup (object-name.c:1840-1845), where zvcs said
//!   `Not a valid object name`.
//! * `--end-of-options` ends the options as `--` does (parse-options.c:1119-1124).
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical
//! repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

/// Run stock git in `side`'s repository, so both sides are set up byte for byte
/// alike.
fn stock_in(stock: &str, side: &twin_repo::Side, args: &[&str]) -> String {
    let out = std::process::Command::new(stock)
        .args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn missing_blobs_paths_outside_and_end_of_options() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("ls-tree-size-prefix", stock);
    for side in [&s, &z] {
        side.write("sub/f", "f\n");
        side.write("sub/gone", "gone\n");
        stock_in(stock, side, &["add", "sub"]);
        stock_in(stock, side, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "sub"]);
        let gone = stock_in(stock, side, &["rev-parse", "HEAD:sub/gone"]);
        let gone = gone.trim();
        std::fs::remove_file(side.repo().join(".git/objects").join(&gone[..2]).join(&gone[2..])).unwrap();
    }
    let top: &[&[&str]] = &[
        &["ls-tree", "-r", "-l", "HEAD"],
        &["ls-tree", "-r", "--format=%(objectsize:padded) %(path)", "HEAD"],
        &["ls-tree", "HEAD:../a"],
        &["ls-tree", "nosuch:../a"],
        &["ls-tree", "--end-of-options", "HEAD", "a"],
    ];
    for args in top {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
    let sub: &[&[&str]] = &[&["ls-tree", "--full-tree", "HEAD", "../a"], &["ls-tree", "HEAD", "../a"]];
    for args in sub {
        assert_eq!(z.run_in(&z.repo().join("sub"), &[], args), s.run_in(&s.repo().join("sub"), &[], args), "{args:?}");
    }
}
