//! `mailsplit` never opens a repository, so a `core.repositoryFormatVersion` git cannot parse
//! (which `read_repository_format()` dies on for every verb that does set up) leaves it alone:
//! the usage error, the split itself and the unknown-option death all come out as they would
//! outside any repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn an_unparsable_repository_format_version_does_not_reach_mailsplit() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("mailsplit-format-version", stock);
    for side in [&s, &z] {
        side.write(".git/config", "[core]\n\trepositoryFormatVersion = all\n");
    }
    for args in [
        &["mailsplit"][..],
        &["mailsplit", "-o."],
        &["mailsplit", "-x"],
        &["mailsplit", "-o.", "/dev/null"],
        &["mailsplit", "--", "--", "-d4", "--mboxrd", "-o.", "-o.", "--keep-cr"],
    ] {
        let want = s.git(args);
        assert_eq!(z.git(args), want, "{args:?}");
        assert!(!want.stderr.contains("repositoryformatversion"), "{args:?}: {want:?}");
    }
    // The contrast: a verb that does set up still dies on it.
    let want = s.git(&["status"]);
    assert_eq!(want.code, 128, "{want:?}");
    assert_eq!(z.git(&["status"]), want);
}
