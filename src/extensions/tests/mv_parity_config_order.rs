//! `git mv` against stock git: the order its configuration is read in.
//!
//! `cmd_mv()` runs `git_default_config` first, so `core.createObject` and friends
//! refuse every invocation, usage errors included. `prepare_repo_settings()` is
//! reached only at `repo_read_index()`, after the option parse and the operand
//! count, so `core.packedGitLimit`, `core.commitGraph` and the rest of the
//! settings block lose to a usage error or an unknown option and win over every
//! later diagnostic (`bad source`, `destination exists`, …).
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
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

fn fixture(stock: &str, root: &Path) {
    std::fs::create_dir_all(root.join("d")).unwrap();
    run(stock, root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("a"), "a\n").unwrap();
    std::fs::write(root.join("b"), "b\n").unwrap();
    run(stock, root, &["add", "."]);
    run(stock, root, &["commit", "-qm", "one"]);
}

#[test]
fn settings_follow_the_usage_checks_and_precede_the_rest() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-mv-config-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("stock"), base.join("zvcs"));
    let keys = ["core.createObject=bogus", "core.packedGitLimit=bogus", "core.commitGraph=bogus"];
    let operands: &[&[&str]] = &[
        &["mv"],
        &["mv", "a"],
        &["mv", "-k"],
        &["mv", "--bogus"],
        &["mv", "nonexist", "dest"],
        &["mv", "a", "b"],
        &["mv", "a", "b", "c"],
        &["mv", "-n", "a", "d"],
        &["mv", "a", "e"],
    ];
    for key in keys {
        for ops in operands {
            for root in [&s, &z] {
                let _ = std::fs::remove_dir_all(root);
                fixture(stock, root);
            }
            let mut args = vec!["-c", key];
            args.extend_from_slice(ops);
            let want = run(stock, &s, &args);
            assert_eq!(run(BIN, &z, &args), want, "{args:?}");
            assert_eq!(
                run(BIN, &z, &["status", "--porcelain"]),
                run(stock, &s, &["status", "--porcelain"]),
                "status after {args:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
