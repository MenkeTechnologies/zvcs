//! `git ls-remote` with no operand and no remote to default to.
//!
//! `remote_get(NULL)` finds neither a branch remote nor `origin`, and
//! `cmd_ls_remote()` answers `No remote configured to list refs from.` at 128.
//! Inside a repository zvcs reported the vendored lookup's own sentence instead.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn no_remote_configured() {
    let Some(t) = Twin::new("ls-remote-none") else { return };
    t.same_in("up", &["ls-remote"]);
    t.same_in("up", &["ls-remote", "-q"]);
    t.same_in("up", &["ls-remote", "--tags", "--", "v*"]);
}

#[test]
fn origin_is_the_default_when_it_exists() {
    let Some(t) = Twin::new("ls-remote-origin") else { return };
    t.same(&["ls-remote"]);
}
