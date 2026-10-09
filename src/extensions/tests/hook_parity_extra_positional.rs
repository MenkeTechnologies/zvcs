//! `git hook run <event> <extra>` without `--`.
//!
//! `cmd_hook_run()` parses with `PARSE_OPT_KEEP_DASHDASH` and then, when `argv[1]`
//! is neither `--` nor `--end-of-options`, takes the `usage:` exit — the full
//! `git hook run` block on stderr and status 129, before any configuration is
//! read or the event is looked up. zvcs died with its own
//! `fatal: unexpected extra argument` (128). An option that follows the extra
//! word is still diagnosed first, because the whole argv is parsed before that
//! check.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::PathBuf;
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &PathBuf, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

#[test]
fn a_second_word_before_the_dashdash_is_the_usage_error() {
    let Some(stock) = stock_git() else { return };
    let dir = std::env::temp_dir().join(format!("zvcs-hook-extra-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(Command::new(BIN).args(["init", "-q"]).current_dir(&dir).status().unwrap().success());

    for args in [
        &["hook", "run", "pre-commit", "x"][..],
        &["hook", "run", "no-such-event", "x"],
        &["hook", "run", "x", "--allow-unknown-hook-name", "y"],
        &["hook", "run", "x", "--bogus", "y"],
        &["hook", "run", "pre-commit", "--", "x"],
    ] {
        let want = run(stock, &dir, args);
        let got = run(BIN, &dir, args);
        assert_eq!(got, want, "args {args:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
