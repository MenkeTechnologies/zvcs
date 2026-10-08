//! `git rebase --signoff` against stock git: `write_file()` completes the line, so
//! `rebase-merge/signoff` holds `--signoff\n`.
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
fn rebase_signoff_state_file_ends_in_a_newline() {
    let Some((s, z)) = world("rebase-signoff") else { return };
    let args = ["rebase", "--signoff", "-i", "--exec", "false", "main~2"];
    assert_eq!(z.git(&args), s.git(&args));
    assert_eq!(z.read(".git/rebase-merge/signoff"), s.read(".git/rebase-merge/signoff"));
    assert_eq!(s.read(".git/rebase-merge/signoff").as_deref(), Some(&b"--signoff\n"[..]));
}
