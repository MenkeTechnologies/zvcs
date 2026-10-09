//! A ref update whose lock cannot be taken fails, it does not loop.
//!
//! `git commit` on a branch whose `refs/heads/<name>.lock` is held by someone else dies
//! `fatal: cannot lock ref 'HEAD': Unable to create '<path>/refs/heads/main.lock': File
//! exists.` and exits 128, naming `HEAD` — the ref the caller asked to update — and the lock
//! path of the branch it resolved to. The transaction code walked the chain from the branch
//! edit back to its `HEAD` parent to find that name, and never advanced the cursor once it
//! reached the root, so the process held `HEAD.lock` and spun at full CPU until killed.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_held_branch_lock_refuses_the_ref_update_with_the_exit_status_stock_gives() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("held-branch-lock", stock);
    for side in [&s, &z] {
        std::fs::write(side.repo().join(".git/refs/heads/main.lock"), "").unwrap();
        side.write("a", "1\n2\n3\nedited\n");
    }
    for args in [
        &["commit", "-q", "-am", "x"][..],
        &["commit", "-q", "--amend", "--no-edit", "-a"],
    ] {
        let (want, got) = (s.git(args), z.git(args));
        assert_ne!(want.code, 0, "{args:?}: {want:?}");
        assert_eq!(got.code, want.code, "{args:?}: {got:?} vs {want:?}");
        let refusal = want.stderr.lines().find(|l| l.contains("cannot lock ref")).unwrap_or_else(|| panic!("{args:?}: stock names the ref: {want:?}"));
        // A writer that finds the repository locked is queued, and the queue relays the job's
        // report on its own stream, so the refusal is looked for in both.
        let reported = format!("{}{}", got.stdout, got.stderr);
        assert!(reported.contains(refusal), "{args:?}: {got:?} vs {want:?}");
        assert!(!z.repo().join(".git/HEAD.lock").exists(), "{args:?}: HEAD.lock left behind");
    }
}
