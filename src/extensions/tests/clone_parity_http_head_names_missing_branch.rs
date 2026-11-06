//! `git clone` over smart HTTP of a repository whose `HEAD` names a branch it
//! does not have, compared with stock git.
//!
//! Protocol v0/v1 advertises no `HEAD` then, so `cmd_clone()` falls back to
//! `repo_default_branch_name(the_repository, 0)`, whose advice is printed when
//! `init.defaultBranch` is unset, checks out the remote's branch of that name
//! if there is one and otherwise leaves `HEAD` unborn on it
//! (builtin/clone.c:1554-1586, refs.c:689-718). Protocol v2's `ls-refs`
//! reports the branch as `unborn` and the clone takes that name instead.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

#[path = "support/cgi_server.rs"]
mod cgi_server;

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[derive(Clone, Copy, Debug)]
enum Remote {
    /// No commit at all.
    Empty,
    /// `main` only, `HEAD` naming `missing`.
    Missing,
    /// `main` and `master`, `HEAD` naming `missing`.
    MissingWithMaster,
}

#[derive(Debug, PartialEq)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    state: String,
}

fn run(stock: &str, bin: &str, tag: &str, remote: Remote, protocol: u8, args: &[&str]) -> Run {
    let root: PathBuf = std::env::temp_dir().join(format!("zvcs-clonehead-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("srv")).unwrap();
    let git = |cwd: &Path, args: &[&str]| {
        let out = Command::new(stock)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
    };
    let bare = root.join("srv/r.git");
    git(&root, &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
    if !matches!(remote, Remote::Empty) {
        let src = root.join("src");
        git(&root, &["init", "-q", "-b", "main", src.to_str().unwrap()]);
        std::fs::write(src.join("f"), "a\n").unwrap();
        git(&src, &["add", "f"]);
        git(&src, &["commit", "-qm", "a"]);
        git(&src, &["push", "-q", bare.to_str().unwrap(), "main"]);
        if matches!(remote, Remote::MissingWithMaster) {
            git(&src, &["push", "-q", bare.to_str().unwrap(), "main:master"]);
        }
        git(&bare, &["symbolic-ref", "HEAD", "refs/heads/missing"]);
    }
    let (port, _) = cgi_server::spawn(stock, root.join("srv"), cgi_server::Fault::NONE);
    let url = format!("http://127.0.0.1:{port}/r.git");

    let protocol = format!("protocol.version={protocol}");
    let out = Command::new(bin)
        .args(["-c", protocol.as_str(), "clone"])
        .args(args)
        .args([url.as_str(), "c"])
        .current_dir(&root)
        .env("HOME", &root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let clone = root.join("c");
    let mut state = String::new();
    for query in [
        &["for-each-ref"][..],
        &["symbolic-ref", "HEAD"],
        &["status", "--porcelain"],
        &["config", "list", "--local"],
        &["reflog", "list"],
        &["ls-files"],
    ] {
        state += &format!("$ git {}\n{}", query.join(" "), git(&clone, query));
    }
    let host = format!("127.0.0.1:{port}");
    let result = Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).replace(&host, "HOST"),
        stderr: String::from_utf8_lossy(&out.stderr).replace(&host, "HOST"),
        state: state.replace(&host, "HOST"),
    };
    let _ = std::fs::remove_dir_all(&root);
    result
}

fn check(tag: &str, remote: Remote, protocol: u8, args: &[&str], advised: bool) {
    let Some(stock) = stock_git() else { return };
    let want = run(stock, stock, &format!("{tag}-s"), remote, protocol, args);
    let got = run(stock, BIN, &format!("{tag}-z"), remote, protocol, args);
    assert_eq!(got, want, "{tag}: zvcs and stock git differ");
    assert_eq!(
        want.stderr.contains("hint: Using 'master' as the name for the initial branch."),
        advised,
        "{tag}: stock's advice:\n{}",
        want.stderr
    );
}

#[test]
fn clone_parity_v0_head_naming_a_missing_branch_falls_back_to_the_default() {
    for protocol in [0, 1] {
        for remote in [Remote::Empty, Remote::Missing, Remote::MissingWithMaster] {
            let tag = format!("{remote:?}-v{protocol}");
            check(&tag, remote, protocol, &[], true);
            check(&format!("{tag}-bare"), remote, protocol, &["--bare"], true);
            let configured = ["-c", "init.defaultBranch=main"];
            check(&format!("{tag}-configured"), remote, protocol, &configured, false);
        }
    }
}

#[test]
fn clone_parity_v2_head_naming_a_missing_branch_is_unborn() {
    for remote in [Remote::Empty, Remote::Missing, Remote::MissingWithMaster] {
        check(&format!("{remote:?}-v2"), remote, 2, &[], false);
    }
}
