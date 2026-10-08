//! `git checkout` and `git checkout HEAD`, against stock git: no ref moves, but
//! `update_refs_for_switch()` still ends in `report_tracking()` for the current branch — the
//! condition is `new_branch_info->path || !strcmp(new_branch_info->name, "HEAD")` — so a branch
//! with an upstream gets its ahead/behind summary after the local-changes listing. `-q`
//! silences it, a detached `HEAD` has none to report.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// `main` is one commit ahead of `origin/main`, with a modified tracked file.
fn fixture(stock: &str, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let git = |args: &[&str]| run(stock, root, args);
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("f"), "a\n").unwrap();
    git(&["add", "f"]);
    git(&["commit", "-qm", "one"]);
    git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&["config", "branch.main.remote", "origin"]);
    git(&["config", "branch.main.merge", "refs/heads/main"]);
    git(&["config", "remote.origin.url", "/nonexistent"]);
    git(&["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
    std::fs::write(root.join("g"), "g\n").unwrap();
    git(&["add", "g"]);
    git(&["commit", "-qm", "two"]);
    std::fs::write(root.join("g"), "g2\n").unwrap();
}

#[test]
fn checking_out_the_current_branch_in_place_reports_tracking() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let base = std::env::temp_dir().join(format!("zvcs-checkout-track-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let cases: [(&[&str], bool); 6] = [
        (&["checkout"], false),
        (&["checkout", "HEAD"], false),
        (&["checkout", "--guess"], false),
        (&["checkout", "-q"], false),
        (&["checkout", "main"], false),
        (&["checkout"], true),
    ];
    for (args, detached) in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let root = base.join(who);
            let _ = std::fs::remove_dir_all(&root);
            fixture(stock, &root);
            if detached {
                run(stock, &root, &["checkout", "-q", "--detach"]);
            }
            seen.push(run(bin, &root, args));
        }
        assert_eq!(seen[1], seen[0], "{args:?} detached={detached}");
        if args == ["checkout"] && !detached {
            assert!(
                seen[0].0.contains("Your branch is ahead of 'origin/main' by 1 commit."),
                "{:?}",
                seen[0]
            );
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
