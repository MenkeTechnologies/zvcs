//! `git rev-list`: the pre-`setup_revisions()` scan refuses `--exclude-promisor-objects`
//! next to any recognised `--missing=` action but `error`, ahead of every other check.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn exclude_promisor_objects_conflicts_with_a_non_error_missing_action() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("rev-list-promisor-missing", stock);
    for args in [
        &["rev-list", "--exclude-promisor-objects", "--missing=print"][..],
        &["rev-list", "--missing=print", "--exclude-promisor-objects"],
        &["rev-list", "--missing=allow-any", "--exclude-promisor-objects", "HEAD"],
        &["rev-list", "--exclude-promisor-objects", "--missing=allow-promisor"],
        &["rev-list", "--missing-only", "--exclude-promisor-objects", "--missing=allow-any"],
        &["rev-list", "--bisect-vars", "--children", "--exclude-promisor-objects", "--missing=print"],
        &["rev-list", "--exclude-promisor-objects", "--missing=print", "nosuchrev"],
        &["rev-list", "--missing=print", "--missing=error", "--exclude-promisor-objects", "HEAD"],
        &["rev-list", "--missing=print", "--missing=bogus", "--exclude-promisor-objects", "HEAD"],
        &["rev-list", "--exclude-promisor-objects", "--missing=error", "HEAD"],
        &["rev-list", "--exclude-promisor-objects", "--missing=bogus", "HEAD"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
