//! `git cherry-pick` against stock git: a pick that turns out empty stops with the merge's
//! result index written, so `index.recordEndOfIndexEntries` reaches the index the stop leaves
//! behind.
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
fn an_empty_pick_stop_rewrites_the_index_under_the_index_write_settings() {
    let Some((s, z)) = world("pick-empty-eoie") else { return };
    for side in [&s, &z] {
        side.git(&["checkout", "-q", "-b", "twice", "main~2"]);
        side.write("a", "1\n2\n");
        side.git(&["commit", "-q", "-am", "two-twice"]);
    }
    let args = ["-c", "index.recordEndOfIndexEntries=true", "cherry-pick", "main~1"];
    assert_eq!(z.git(&args), s.git(&args));
    let ext = |side: &Side| twin_repo::index_extension(&side.read(".git/index").unwrap(), b"EOIE");
    assert!(ext(&s).is_some(), "stock writes EOIE");
    assert_eq!(ext(&z), ext(&s));
}
