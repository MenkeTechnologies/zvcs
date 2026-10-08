//! `git push --signed=<value>` against stock git.
//!
//! `option_parse_push_signed()` (builtin/push.c) tries `git_parse_maybe_bool()`
//! first — the boolean words in any case, the empty string, and any integer, so
//! `-0` is "no" and `2` is "yes" — then `if-asked` case-insensitively, and only
//! then dies with `bad signed argument`. zvcs matched a handful of lowercase
//! spellings and refused the rest, so `--signed=-0` died where stock pushed.
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
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// `work` with one commit on `main` and a bare `peer.git` as `origin`.
fn fixture(stock: &str, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    run(stock, root, &["init", "-q", "-b", "main", "work"]);
    run(stock, &root.join("work"), &["commit", "-q", "--allow-empty", "-m", "a"]);
    run(stock, root, &["init", "-q", "--bare", "-b", "main", "peer.git"]);
    run(stock, &root.join("work"), &["remote", "add", "origin", "../peer.git"]);
}

#[test]
fn signed_value_follows_maybe_bool_then_if_asked() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-push-signed-grammar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("stock"), base.join("zvcs"));
    fixture(stock, &s);
    fixture(stock, &z);
    for value in ["-0", "0", "00", "2", "0x10", "1k", "TRUE", "Yes", "ON", "no", "OFF", "", "If-Asked", "if-asked", "bogus", "1.5"] {
        let arg = format!("--signed={value}");
        let args = ["push", arg.as_str(), "origin", "main"];
        let want = run(stock, &s.join("work"), &args);
        let got = run(BIN, &z.join("work"), &args);
        assert_eq!(got, want, "{arg}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
