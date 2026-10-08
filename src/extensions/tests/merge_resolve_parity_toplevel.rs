//! `git merge-resolve` and `git merge -s resolve` against stock git.
//!
//! `git-merge-resolve` refuses to run anywhere but the top of the work tree, and a submodule's
//! `.git/modules/<name>` — whose `core.worktree` puts the work tree elsewhere — is not the top
//! even though it is a git directory. `git merge` has already moved to the top by the time it
//! spawns the strategy, so a merge typed in a subdirectory reaches `read-tree` and reports
//! *its* complaint about `-Xbogus`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn merge_resolve_in_a_module_git_dir_with_a_named_work_tree_is_refused() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("merge-resolve-module", stock);
    for side in [&s, &z] {
        let work = side.root.join("elsewhere");
        std::fs::create_dir_all(&work).unwrap();
        let module = side.repo().join(".git");
        let cfg = format!("{}/config", module.display());
        let mut body = std::fs::read_to_string(&cfg).unwrap();
        body.push_str(&format!("[core]\n\tworktree = {}\n", work.display()));
        std::fs::write(cfg, body).unwrap();
    }
    for args in [&["merge-resolve", "--"][..], &["merge-resolve", "main", "--", "HEAD", "side"]] {
        let run = |side: &twin_repo::Side| side.run_in(&side.repo().join(".git"), &[], args);
        assert_eq!(run(&z), run(&s), "{args:?}");
    }
}

#[test]
fn merge_s_resolve_from_a_subdirectory_runs_the_strategy_at_the_top() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("merge-resolve-subdir", stock);
    for side in [&s, &z] {
        side.write("sub/f", "f\n");
        side.git(&["add", "sub/f"]);
        side.git(&["commit", "-q", "-m", "sub"]);
        side.git(&["checkout", "-q", "-b", "other", "main~1"]);
        side.write("other.txt", "o\n");
        side.git(&["add", "other.txt"]);
        side.git(&["commit", "-q", "-m", "other"]);
        side.git(&["checkout", "-q", "main"]);
    }
    let run = |side: &twin_repo::Side| {
        let sub = side.repo().join("sub");
        let merged = side.run_in(&sub, &[], &["merge", "-Xbogus", "-sresolve", "other"]);
        let state = side.git(&["status", "--porcelain=v1", "--branch"]);
        (merged, state, side.read(".git/MERGE_HEAD").is_some())
    };
    assert_eq!(run(&z), run(&s));
}
