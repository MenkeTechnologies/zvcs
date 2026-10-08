//! `git rebase` (merge backend) against stock git: `get_replay_opts()` adopts the
//! sequencer's `default_strategy` — `pull.twohead`, cut at its first space — as the
//! rebase's strategy when no `-s` was given, and `$state_dir/strategy` records it.
//! `-s` wins over the config, and `-X` alone implies `ort` before the default is
//! looked at.
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
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
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
    for i in 1..=4 {
        std::fs::write(root.join(format!("f{i}")), format!("{i}\n")).unwrap();
        run(stock, root, &["add", "."]);
        run(stock, root, &["commit", "-qm", &format!("c{i}")]);
    }
}

/// `rebase-merge/strategy` and `rebase-merge/strategy_opts`, `None` when absent.
fn recorded(root: &Path) -> [Option<String>; 2] {
    ["strategy", "strategy_opts"].map(|n| std::fs::read_to_string(root.join(".git/rebase-merge").join(n)).ok())
}

#[test]
fn pull_twohead_is_the_rebase_strategy_when_none_was_given() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-rebase-defstrat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let cases: &[&[&str]] = &[
        &["-c", "pull.twohead=all", "rebase", "-i", "--exec", "false", "HEAD~2"],
        &["-c", "pull.twohead=all", "rebase", "-m", "--exec", "false", "HEAD~2"],
        &["-c", "pull.twohead=ort extra", "rebase", "-i", "--exec", "false", "HEAD~2"],
        &["-c", "pull.twohead=all", "rebase", "-s", "ours", "-i", "--exec", "false", "HEAD~2"],
        &["-c", "pull.twohead=all", "rebase", "-Xtheirs", "-i", "--exec", "false", "HEAD~2"],
        &["rebase", "-i", "--exec", "false", "HEAD~2"],
    ];
    for args in cases {
        let (s, z) = (base.join("stock"), base.join("zvcs"));
        for root in [&s, &z] {
            let _ = std::fs::remove_dir_all(root);
            fixture(stock, root);
        }
        let want = run(stock, &s, args);
        let got = run(BIN, &z, args);
        assert_eq!(got, want, "{args:?}");
        assert_eq!(recorded(&z), recorded(&s), "state files after {args:?}");
        assert_eq!(
            run(BIN, &z, &["status", "--porcelain=v2", "--branch"]),
            run(stock, &s, &["status", "--porcelain=v2", "--branch"]),
            "status after {args:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// `do_pick_commit()` takes the fast-forward arm before it looks at the strategy, so
/// the picks of a continued rebase fast-forward — and never run a `merge-all` child,
/// which does not exist — however the strategy was chosen.
#[test]
fn a_fast_forwarding_pick_never_runs_the_strategy_child() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-rebase-defstrat-ff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("stock"), base.join("zvcs"));
    for (strategy_args, label) in [
        (&["-c", "pull.twohead=all"][..], "configured"),
        (&[][..], "none"),
    ] {
        for root in [&s, &z] {
            let _ = std::fs::remove_dir_all(root);
            fixture(stock, root);
        }
        let start: Vec<&str> =
            strategy_args.iter().copied().chain(["rebase", "-i", "--exec", "false", "HEAD~2"]).collect();
        let cont: Vec<&str> = strategy_args.iter().copied().chain(["rebase", "--continue"]).collect();
        for args in [&start, &cont] {
            assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{label}: {args:?}");
        }
        for probe in [
            &["log", "--format=%H %s", "--all"][..],
            &["reflog", "--format=%gs"][..],
            &["status", "--porcelain=v2", "--branch"][..],
        ] {
            assert_eq!(run(BIN, &z, probe), run(stock, &s, probe), "{label}: {probe:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
