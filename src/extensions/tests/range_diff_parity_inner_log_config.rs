//! `git range-diff` against stock git: a configuration value only the inner
//! `git log` refuses (`git_log_config()`, `repo_init_revisions()`'s `grep_config`
//! pass, `format.pretty`) is that log's `fatal:` followed by `error: could not
//! parse log for '<range>'` at 255 — and only once `range-diff` has accepted its own
//! operands. The parent reads `git_diff_ui_config()` alone, so a usage error such as
//! `need two commit ranges` is never pre-empted by such a value.
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

fn fixture(stock: &str, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    run(stock, root, &["init", "-q", "-b", "main"]);
    run(stock, root, &["commit", "-q", "--allow-empty", "-m", "one"]);
}

#[test]
fn only_the_inner_log_refuses_its_own_configuration() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-range-diff-logcfg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("stock"), base.join("zvcs"));
    for root in [&s, &z] {
        fixture(stock, root);
    }
    let keys = [
        "log.showRoot=bogus",
        "log.date=bogus",
        "log.abbrevCommit=bogus",
        "log.follow=bogus",
        "grep.patternType=bogus",
        "grep.lineNumber=bogus",
        "format.pretty=bogus",
        "diff.renameLimit=bogus",
        "color.ui=bogus",
    ];
    let operands: &[&[&str]] = &[
        &["range-diff", "main...main"],
        &["range-diff", "main"],
        &["range-diff"],
        &["range-diff", "main...main", "--left-only", "--right-only"],
    ];
    for key in keys {
        for ops in operands {
            let mut args = vec!["-c", key];
            args.extend_from_slice(ops);
            let want = run(stock, &s, &args);
            assert_eq!(run(BIN, &z, &args), want, "{args:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
