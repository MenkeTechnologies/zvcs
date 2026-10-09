//! `transaction_refname_valid()` (refs.c): `FETCH_HEAD` and `MERGE_HEAD` are never
//! written through a transaction, and a null new value (a deletion or an update to the
//! all-zero id) only needs a "safe" name, so `origin/main` is refused as a bad name
//! instead of deleting `refs/.../origin/main`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

const ZERO: &str = "0000000000000000000000000000000000000000";

#[test]
fn pseudorefs_and_unsafe_names_with_a_null_value_are_refused_like_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("update-ref-null-names", stock);
    for args in [
        &["update-ref", "FETCH_HEAD", "main"][..],
        &["update-ref", "MERGE_HEAD", "main"],
        &["update-ref", "-d", "FETCH_HEAD"],
        &["update-ref", "--create-reflog", "origin/main", ZERO],
        &["update-ref", "origin/main", ZERO],
        &["update-ref", "main", ZERO],
        &["update-ref", "-d", "origin/main"],
        &["update-ref", "refs/heads/side", ZERO],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
        assert_eq!(
            z.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            s.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            "refs after {args:?}"
        );
        assert_eq!(z.read(".git/FETCH_HEAD"), s.read(".git/FETCH_HEAD"), "FETCH_HEAD after {args:?}");
    }
}
