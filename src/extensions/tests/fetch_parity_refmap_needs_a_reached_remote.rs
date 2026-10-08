//! `git fetch --refmap` without a command-line refspec against stock git.
//!
//! The refusal ("--refmap option is only meaningful with command-line
//! refspec(s)") lives in `get_ref_map()` (builtin/fetch.c:544-545), which runs on
//! the advertisement of a remote that was actually reached. With no remote to
//! fetch from (`fetch`, `--all` and `--multiple` over zero remotes), or with a
//! remote that cannot be reached, stock never gets there. zvcs refused straight
//! out of option parsing, so those exited 128 where stock exits 0 or dies on the
//! connection.
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

/// `up` has one commit on `main`; `lone` has no remote; `work` is a clone of `up`
/// whose `FETCH_HEAD` already holds a row.
fn fixture(stock: &str, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    run(stock, root, &["init", "-q", "-b", "main", "up"]);
    run(stock, &root.join("up"), &["commit", "-q", "--allow-empty", "-m", "a"]);
    run(stock, root, &["init", "-q", "-b", "main", "lone"]);
    run(stock, root, &["clone", "-q", "up", "work"]);
    run(stock, &root.join("work"), &["fetch", "-q", "origin", "main"]);
}

#[test]
fn refmap_is_refused_only_once_the_remote_is_reached() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-fetch-refmap-reach-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("stock"), base.join("zvcs"));
    fixture(stock, &s);
    fixture(stock, &z);
    let cases: &[(&str, &[&str])] = &[
        ("lone", &["fetch", "--refmap=x"]),
        ("lone", &["fetch", "--refmap=x", "--depth=1"]),
        ("lone", &["fetch", "--refmap=x", "--all"]),
        ("lone", &["fetch", "--refmap=x", "--multiple"]),
        ("lone", &["fetch", "--refmap=x", "nosuch"]),
        ("lone", &["fetch", "--refmap=x", "--stdin"]),
        ("work", &["fetch", "--refmap=x"]),
        ("work", &["fetch", "--refmap=x", "origin"]),
        ("work", &["fetch", "--refmap=x", "--all"]),
        ("work", &["fetch", "--refmap=x", "origin", "main"]),
        ("work", &["fetch", "--refmap=", "origin", "main"]),
    ];
    for (repo, args) in cases {
        let scrub = |o: Outcome, root: &Path| {
            let r = root.display().to_string();
            (o.0.replace(&r, "R"), o.1.replace(&r, "R"), o.2)
        };
        let want = scrub(run(stock, &s.join(repo), args), &s);
        let got = scrub(run(BIN, &z.join(repo), args), &z);
        assert_eq!(got, want, "{repo}: {args:?}");
        let fetch_head = |root: &Path| {
            std::fs::read_to_string(root.join(repo).join(".git/FETCH_HEAD"))
                .ok()
                .map(|t| t.replace(&root.display().to_string(), "R"))
        };
        assert_eq!(fetch_head(&z), fetch_head(&s), "{repo}: {args:?} FETCH_HEAD");
    }
    let _ = std::fs::remove_dir_all(&base);
}
