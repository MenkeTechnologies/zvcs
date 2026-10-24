//! `git bisect reset` under a configuration value only its child `git checkout` parses.
//!
//! `bisect_reset()` (builtin/bisect.c:244-279) checks the original head out with a child
//! `git checkout --ignore-other-worktrees <branch> --`. The child's
//! `repo_config(the_repository, git_checkout_config, opts)` (builtin/checkout.c:1901)
//! reaches `git_default_config()`, where `core.safecrlf=abc` dies in `git_config_bool()`
//! (environment.c:430-438). bisect never parses that key itself, so `bisect start`
//! succeeds and `bisect reset` ends with the child's fatal, `could not check out original
//! HEAD`, exit 1 and the session left in place. zvcs ran the in-process checkout without
//! the child's configuration pass and answered `Already on 'main'` with 0.
//! Expectations come from stock git (`support/stock_git.rs`) in an identical repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn reset_reports_the_child_checkouts_config_fatal() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("bisect-reset-child-config", stock);
    for side in [&s, &z] {
        side.git(&["config", "core.safecrlf", "abc"]);
    }
    let (want, got) = (s.git(&["bisect", "start"]), z.git(&["bisect", "start"]));
    assert_eq!(want.code, 0, "{want:?}");
    assert_eq!(got, want);

    let (want, got) = (s.git(&["bisect", "reset"]), z.git(&["bisect", "reset"]));
    assert_eq!(want.code, 1, "{want:?}");
    assert_eq!(got, want);
    assert_eq!(z.read(".git/BISECT_START"), s.read(".git/BISECT_START"));
    assert!(z.read(".git/BISECT_START").is_some());
    assert_eq!(z.read(".git/logs/HEAD"), s.read(".git/logs/HEAD"));
}
