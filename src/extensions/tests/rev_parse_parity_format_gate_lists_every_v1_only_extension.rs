//! `verify_repository_format()` lists every v1-only extension a version-0 repository
//! carries. `rev-parse` (a format-gate verb) took `gix`'s answer, which stops at the
//! first one it knows, so `refstorage` beside `objectformat` was dropped.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn world(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    Some(twin_repo::pair(label, stock))
}

#[test]
fn rev_parse_lists_refstorage_and_objectformat_together() {
    let Some((s, z)) = world("rev-parse-v1-only") else { return };
    for side in [&s, &z] {
        side.write(
            ".git/config",
            "[core]\n\trepositoryFormatVersion = 0\n[extensions]\n\trefstorage = reftable\n\tobjectFormat = sha256\n",
        );
    }
    for args in [
        &["rev-parse", "--show-object-format"][..],
        &["rev-parse", "HEAD"],
        &["init", "-q"],
    ] {
        let want = s.git(args);
        assert_eq!(want.code, 128, "{args:?}: {want:?}");
        assert_eq!(z.git(args), want, "{args:?}");
    }
    let want = s.git(&["rev-parse", "--show-object-format"]);
    assert_eq!(
        want.stderr,
        "fatal: repo version is 0, but v1-only extensions found:\n\trefstorage\n\tobjectformat\n"
    );
}
