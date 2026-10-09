//! A reftable stack written for sha1 in a repository whose config says `sha256`: the stack
//! fails to open (`reftable_merged_table_new()` refuses the hash mismatch), `refs->err` stays
//! set, and git reads that as "no value" rather than a corrupt table. `HEAD` does not resolve
//! (`your current branch appears to be broken`, `failed to resolve HEAD as a valid ref`,
//! `No such ref: HEAD`) and the reference listings come out empty with status 0.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn readers_see_no_references_when_the_stack_hash_does_not_match_the_config() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("reftable-foreign-hash", stock);
    for side in [&s, &z] {
        let migrated = side.git(&["refs", "migrate", "--ref-format=reftable"]);
        assert_eq!(migrated.code, 0, "{migrated:?}");
        side.git(&["config", "core.repositoryFormatVersion", "1"]);
        side.git(&["config", "extensions.objectFormat", "sha256"]);
    }
    for args in [
        &["log", "--oneline"][..],
        &["log", "-1", "--format=%H"],
        &["log", "--all"],
        &["for-each-ref"],
        &["for-each-ref", "--format=%(refname) %(objectname)"],
        &["for-each-ref", "refs/tags"],
        &["branch", "--list"],
        &["branch", "-a", "-v"],
        &["branch", "--show-current"],
        &["branch", "-d", "nosuch"],
        &["branch", "newb"],
        &["symbolic-ref", "HEAD"],
        &["symbolic-ref", "-q", "HEAD"],
        &["symbolic-ref", "--short", "HEAD"],
        &["rev-list", "--all"],
        &["rev-list", "HEAD"],
        &["reflog"],
        &["show-ref"],
        &["rev-parse", "HEAD"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
