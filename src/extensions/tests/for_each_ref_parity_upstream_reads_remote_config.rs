//! `%(upstream)` / `%(push)` resolve a branch through `branch_get()`, whose first call makes
//! the one `read_config()` pass over `remote.*` (remote.c:630-650); a value `handle_config()`
//! cannot parse dies there (`bad boolean config value ... for 'remote.origin.prune'`, 128).
//! `branch -v` renders the same atoms. zvcs listed the refs without reading the remotes.
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
fn a_bad_remote_value_dies_when_a_tracking_atom_is_rendered() {
    let Some((s, z)) = world("for-each-ref-remote-config") else { return };
    let bad = ["-c", "remote.origin.prune=input"];
    for args in [
        &["for-each-ref", "--format=%(upstream)"][..],
        &["for-each-ref", "--format=%(push:short)", "refs/heads"],
        &["for-each-ref", "--format=%(upstream:track)"],
        &["branch", "-v"],
        &["branch", "-vv"],
        // Nothing asks for a branch's remote, so nothing reads the remotes.
        &["for-each-ref", "--format=%(refname)"],
        &["for-each-ref", "--format=%(upstream)", "refs/tags"],
        &["branch", "--list"],
    ] {
        let argv: Vec<&str> = bad.iter().chain(args).copied().collect();
        let want = s.git(&argv);
        assert_eq!(z.git(&argv), want, "{argv:?}");
    }
    let want = s.git(&["-c", "remote.origin.prune=input", "for-each-ref", "--format=%(upstream)"]);
    assert_eq!(
        (want.code, want.stdout.as_str(), want.stderr.as_str()),
        (128, "", "fatal: bad boolean config value 'input' for 'remote.origin.prune'\n")
    );
}
