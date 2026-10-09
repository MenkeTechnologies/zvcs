//! `git bisect start` against a HEAD that is a symref resolving to nothing, or to a ref
//! outside `refs/heads/`.
//!
//! `bisect_start()` (builtin/bisect.c) records the start label from `HEAD` in three arms:
//!
//! ```c
//! if (!repo_get_oid(the_repository, head, &head_oid) &&
//!     !starts_with(head, "refs/heads/")) {
//!         strbuf_addstr(&start_head, oid_to_hex(&head_oid));
//! } else if (!repo_get_oid(the_repository, head, &head_oid) &&
//!            skip_prefix(head, "refs/heads/", &head)) {
//!         strbuf_addstr(&start_head, head);
//! } else {
//!         return error(_("bad HEAD - strange symbolic ref"));
//! }
//! ```
//!
//! An unborn branch or a dangling symref reaches the last arm: `error:` and exit 1, where
//! zvcs died `fatal: cannot bisect: HEAD does not point at a commit yet` at 128. A symref
//! that resolves outside `refs/heads/` is recorded as the commit id, not a shortened name.
//! Expectations come from stock git (`support/stock_git.rs`) in an identical repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn unborn_and_dangling_head_is_a_strange_symbolic_ref() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("bisect-strange", stock);
    for side in [&s, &z] {
        side.git(&["symbolic-ref", "HEAD", "refs/heads/unborn"]);
    }
    for args in [&["bisect", "start"][..], &["bisect", "start", "main", "side"]] {
        let (want, got) = (s.git(args), z.git(args));
        assert_eq!(want.code, 1, "{args:?}: {want:?}");
        assert_eq!(got, want, "{args:?}");
        assert!(!z.repo().join(".git/BISECT_START").exists(), "{args:?}");
    }
    for side in [&s, &z] {
        side.git(&["symbolic-ref", "HEAD", "refs/foo/bar"]);
    }
    let (want, got) = (s.git(&["bisect", "start"]), z.git(&["bisect", "start"]));
    assert_eq!(got, want);
}

#[test]
fn symref_outside_heads_records_the_commit_id() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("bisect-outside-heads", stock);
    for side in [&s, &z] {
        side.git(&["update-ref", "refs/foo/bar", "main"]);
        side.git(&["symbolic-ref", "HEAD", "refs/foo/bar"]);
    }
    let (want, got) = (s.git(&["bisect", "start"]), z.git(&["bisect", "start"]));
    assert_eq!(got, want);
    assert_eq!(z.read(".git/BISECT_START"), s.read(".git/BISECT_START"));
    assert_eq!(z.read(".git/BISECT_START").map(|b| b.len()), Some(41));
}
