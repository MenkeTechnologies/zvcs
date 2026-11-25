//! `git symbolic-ref` against stock git where the files backend decides:
//!
//! * `-d` runs `check_symref()` before it looks at the name
//!   (builtin/symbolic-ref.c:70-77), so a detached `HEAD` is `Cannot delete
//!   HEAD, not a symbolic ref`; zvcs refused every `HEAD` as `deleting 'HEAD' is
//!   not allowed`.
//! * `core.preferSymlinkRefs` is loaded lazily by `files_transaction_finish()`
//!   (refs/files-backend.c:136-157, :3327): a read never meets a bad value, a
//!   write dies over it only once its lock is held, and a lock failure is
//!   reported first. zvcs read it on every form, ahead of everything.
//! * A transaction the backend refuses is `error: <reason>` at 1, where zvcs said
//!   `zvcs: symbolic-ref: <reason>`.
//! * `parse_loose_ref_contents()` takes `ref:` and any whitespace after it
//!   (refs/files-backend.c:674-676), so `ref:refs/heads/main` is a symref; and a
//!   symref to a one-level `ref: x` is followed to `x` and deleted by `-d`, where
//!   zvcs answered `No such ref` and failed the delete.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use stock_git::stock_git_at_least;
use twin_repo::Side;

/// The loose refs and reflogs under `refs/heads`, so a write or
/// delete that should not have happened shows.
fn refs(side: &Side) -> Vec<String> {
    let mut names: Vec<String> = ["refs/heads", "logs/refs/heads"]
        .iter()
        .flat_map(|dir| {
            std::fs::read_dir(side.repo().join(".git").join(dir))
                .unwrap()
                .map(|e| format!("{dir}/{}", e.unwrap().file_name().to_string_lossy()))
                .collect::<Vec<_>>()
        })
        .collect();
    names.sort();
    names
}

/// Run each command on both sides, after `setup` wrote the same files into each,
/// and compare every answer and the refs left behind.
fn same(s: &Side, z: &Side, setup: &[(&str, &str)], commands: &[&[&str]]) {
    for side in [s, z] {
        for (path, body) in setup {
            side.write(path, body);
        }
    }
    for args in commands {
        assert_eq!(z.git(args), s.git(args), "{setup:?} {args:?}");
    }
    assert_eq!(refs(z), refs(s), "{setup:?} {commands:?}");
}

#[test]
fn delete_checks_the_ref_before_the_name() {
    // The lazy `core.preferSymlinkRefs` read is 2.56's; 2.55 read it up front.
    let Some(stock) = stock_git_at_least((2, 56, 0)) else { return };
    let (s, z) = twin_repo::pair("symref-delete-order", stock);
    for side in [&s, &z] {
        side.git(&["checkout", "-q", "--detach"]);
    }
    same(&s, &z, &[], &[&["symbolic-ref", "-d", "HEAD"], &["symbolic-ref", "-q", "-d", "HEAD"]]);
}

#[test]
fn prefer_symlink_refs_is_read_by_the_write_only() {
    let Some(stock) = stock_git_at_least((2, 56, 0)) else { return };
    let (s, z) = twin_repo::pair("symref-lazy-config", stock);
    let bogus = ["-c", "core.preferSymlinkRefs=bogus"];
    let with = |rest: &[&'static str]| -> Vec<&'static str> { bogus.iter().chain(rest).copied().collect() };
    for side in [&s, &z] {
        side.git(&["symbolic-ref", "refs/heads/sym", "refs/heads/side"]);
    }
    let commands = [
        with(&["symbolic-ref", "HEAD"]),
        with(&["symbolic-ref", "HEAD", "refs/heads/side"]),
        with(&["symbolic-ref", "refs/heads/new", "refs/heads/main"]),
        with(&["symbolic-ref", "-d", "refs/heads/sym"]),
        // The lock comes first: a directory/file conflict is the transaction's
        // `error:` at 1, not the config's `fatal:`.
        with(&["symbolic-ref", "refs/heads/main/b", "refs/heads/main"]),
        vec!["symbolic-ref", "refs/heads/main/b", "refs/heads/main"],
        vec!["symbolic-ref", "HEAD"],
    ];
    let commands: Vec<&[&str]> = commands.iter().map(Vec::as_slice).collect();
    same(&s, &z, &[], &commands);
}

#[test]
fn loose_symref_contents_parse_as_git_parses_them() {
    let Some(stock) = stock_git_at_least((2, 56, 0)) else { return };
    let (s, z) = twin_repo::pair("symref-loose-contents", stock);
    same(
        &s,
        &z,
        &[
            (".git/refs/heads/nospace", "ref:refs/heads/side\n"),
            (".git/refs/heads/tab", "ref:\trefs/heads/main\n"),
            (".git/refs/heads/lower", "ref: x\n"),
            (".git/refs/heads/garbage", "garbage\n"),
        ],
        &[
            &["symbolic-ref", "refs/heads/nospace"],
            &["symbolic-ref", "refs/heads/tab"],
            &["rev-parse", "refs/heads/nospace"],
            &["symbolic-ref", "refs/heads/lower"],
            &["symbolic-ref", "--no-recurse", "refs/heads/lower"],
            &["symbolic-ref", "-d", "refs/heads/lower"],
            &["symbolic-ref", "-d", "refs/heads/garbage"],
            &["symbolic-ref", "-d", "refs/heads/nospace"],
        ],
    );
}
