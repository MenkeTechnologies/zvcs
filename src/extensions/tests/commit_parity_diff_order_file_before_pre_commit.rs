//! `git commit -a` / `-i` against stock git: `add_files_to_cache()`, which runs before the
//! `pre-commit` hook, ends in `diffcore_order()` — a changed path makes `diff.orderFile`
//! readable-or-fatal ahead of the hook.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn both(label: &str, setup: impl Fn(&Side)) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    let (s, z) = twin_repo::pair(label, stock);
    setup(&s);
    setup(&z);
    Some((s, z))
}

#[test]
fn diff_order_file_is_read_before_the_pre_commit_hook_when_the_queue_is_not_empty() {
    let Some((s, z)) = both("commit-orderfile", |side| {
        side.git(&["config", "diff.orderFile", "no-such-order"]);
        side.write(".git/hooks/pre-commit", "#!/bin/sh\necho hook ran >&2\nexit 1\n");
        let hook = side.repo().join(".git/hooks/pre-commit");
        std::process::Command::new("chmod").arg("+x").arg(hook).status().unwrap();
    }) else {
        return;
    };
    for side in [&s, &z] {
        side.write("a", "1\n2\n3\nchanged\n");
    }
    for args in [
        &["commit", "-a", "-m", "x"][..],
        &["commit", "-i", "-m", "x", "a"],
        &["commit", "-m", "x", "a"],
        &["commit", "--dry-run", "-a"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
