//! `git filter-branch --state-branch <b>` loads its map through `git show`.
//!
//! ```sh
//! git show "$state_commit:filter.map" >"$tempdir"/filter-map ||
//!         die "Unable to load state from $state_branch:filter.map"
//! ```
//!
//! (`git-filter-branch.sh`, line 299.) Only stdout is redirected, so when the
//! branch carries no `filter.map` the child's own
//! `fatal: path 'filter.map' does not exist in '<commit>'` reaches the terminal
//! ahead of the script's `Unable to load state` line. Reading the blob
//! in-process instead printed only the second line — measured against stock git
//! 2.55.0, which prints both.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
        .env("FILTER_BRANCH_SQUELCH_WARNING", "1")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fixture() -> PathBuf {
    let repo =
        std::env::temp_dir().join(format!("zvcs-fb-state-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    for n in ["one", "two"] {
        std::fs::write(repo.join("f"), format!("{n}\n")).unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-q", "-m", n]);
    }
    repo
}

/// `main` is a real branch with no `filter.map` in its tree, so the `git show`
/// child fails and the script dies — with both diagnostics.
#[test]
fn a_state_branch_without_filter_map_reports_the_show_failure() {
    let repo = fixture();
    let head = git(&repo, &["rev-parse", "main"]).trim().to_owned();

    let out = run(
        &repo,
        &["filter-branch", "-f", "--state-branch", "main", "--msg-filter", "cat", "HEAD"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        stderr.contains(&format!("Populating map from main ({head})")),
        "the loader never ran:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("fatal: path 'filter.map' does not exist in '{head}'")),
        "git show's own diagnostic was swallowed:\n{stderr}"
    );
    assert!(
        stderr.contains("Unable to load state from main:filter.map"),
        "the script's die message is missing:\n{stderr}"
    );
    assert_eq!(out.status.code(), Some(1), "die_with_status 1 is the script's exit");
}
