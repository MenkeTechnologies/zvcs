//! `rev-parse <arg>` that names no revision falls back to `verify_filename()`, which `lstat()`s
//! the operand from the directory git is standing in. `setup_bare_git_dir()` changes back to the
//! directory the command was started in, so inside a bare repository's `refs/` the file `HEAD`
//! is not found - the operand is ambiguous and the command dies - while at the top of the bare
//! repository `HEAD` is a file there and passes.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn an_unborn_head_is_ambiguous_below_the_top_of_a_bare_repository_only() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("rev-parse-bare-subdir", stock);
    for side in [&s, &z] {
        let init = side.git(&["init", "-q", "--bare", "bare.git"]);
        assert_eq!(init.code, 0, "{init:?}");
        let bare = side.repo().join("bare.git");
        side.run_in(&bare, &[], &["symbolic-ref", "HEAD", "refs/heads/nonexist"]);
    }
    for sub in ["bare.git", "bare.git/refs", "bare.git/refs/heads", "bare.git/objects"] {
        for args in [&["rev-parse", "HEAD"][..], &["rev-parse", "--abbrev-ref", "HEAD"]] {
            let want = s.run_in(&s.repo().join(sub), &[], args);
            let got = z.run_in(&z.repo().join(sub), &[], args);
            assert_eq!(got, want, "{sub}: {args:?}");
        }
    }
    let at_top = s.run_in(&s.repo().join("bare.git"), &[], &["rev-parse", "HEAD"]);
    let below = s.run_in(&s.repo().join("bare.git/refs"), &[], &["rev-parse", "HEAD"]);
    assert_eq!((at_top.code, below.code), (0, 128), "{at_top:?} {below:?}");
}
