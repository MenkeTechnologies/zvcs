//! `send-pack`'s explicit refspecs and what reaches the wire when the push is
//! abandoned.
//!
//! `match_explicit_lhs()` (remote.c:1181-1204) takes a source that matches one
//! local ref by `count_refspec_match()`, falls back to `try_explicit_object_name()`
//! for none, so `main~1` or a full object name is pushed under the name written,
//! and refuses more than one; a matched ref carries its own oid, so an annotated
//! tag is pushed as the tag. `cmd_send_pack()` matches only after `git_connect()`
//! and the advertisement (builtin/send-pack.c:279-312), and a failed match closes
//! the connection without writing anything, so the receive-pack on the other end
//! reads EOF and says `the remote end hung up unexpectedly` after the `error:`
//! lines. The capability refusals in `send_pack()` (send-pack.c:564-580) are
//! `die()`s with the same half-open close: the request the far end receives is
//! empty, where a flush packet used to follow.
//!
//! The far end is stock `receive-pack` behind a `tee` that records the request.
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

/// `src` holds three commits on `main`, a branch `other` and an annotated tag
/// `v1` on the second; `dst.git` has `main` at the first.
fn fixture(stock: &str, tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-sendpackobj-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let ok = |cwd: &Path, args: &[&str]| {
        let out = run(stock, cwd, &root, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    ok(&root, &["init", "-q", "-b", "main", "src"]);
    let src = root.join("src");
    for i in 1..=3 {
        std::fs::write(src.join(format!("f{i}")), format!("{i}\n")).unwrap();
        ok(&src, &["add", "."]);
        ok(&src, &["commit", "-qm", &format!("c{i}")]);
    }
    ok(&src, &["branch", "other"]);
    ok(&src, &["tag", "-a", "-m", "t", "v1", "main~1"]);
    ok(&root, &["init", "-q", "--bare", "-b", "main", "dst.git"]);
    ok(&src, &["push", "-q", "../dst.git", "HEAD~2:refs/heads/main"]);
    Fixture { root }
}

/// What one run left behind: stdout, stderr, exit code, the server's refs and
/// the bytes the client wrote after the advertisement.
type Run = (String, String, Option<i32>, String, Vec<u8>);

/// Run `args` (each `{hex}` replaced by `main`'s id) in a fresh fixture with each
/// binary, `send-pack --receive-pack=<tee | stock receive-pack>` first, and
/// compare everything.
fn check(tag: &str, verb: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut results: Vec<Run> = Vec::new();
    for bin in [stock, ZVCS] {
        let fx = fixture(stock, tag);
        let src = fx.root.join("src");
        let request = fx.root.join("request.bin");
        let receive_pack = format!("--receive-pack=tee {} | {stock} receive-pack", request.display());
        let hex = String::from_utf8(run(stock, &src, &fx.root, &["rev-parse", "main"]).stdout).unwrap();
        let args: Vec<String> = args.iter().map(|a| a.replace("{hex}", hex.trim())).collect();
        let mut full = vec![verb, receive_pack.as_str()];
        full.extend(args.iter().map(String::as_str));
        let out = run(bin, &src, &fx.root, &full);
        let refs = run(stock, &fx.root.join("dst.git"), &fx.root, &["for-each-ref"]);
        results.push((
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).replace(fx.root.to_str().unwrap(), "ROOT"),
            out.status.code(),
            String::from_utf8_lossy(&refs.stdout).into_owned(),
            std::fs::read(&request).unwrap_or_default(),
        ));
    }
    let (stock_run, zvcs_run) = (&results[0], &results[1]);
    assert_eq!(
        (&stock_run.0, &stock_run.1, stock_run.2, &stock_run.3),
        (&zvcs_run.0, &zvcs_run.1, zvcs_run.2, &zvcs_run.3),
        "{verb} {args:?}: stock (left) vs zvcs (right)"
    );
    // The request carries the client's agent string, so only its absence and
    // whether it ends in a flush packet are compared: a refused push writes nothing,
    // and a pack is the last thing a successful one writes.
    assert_eq!(stock_run.4.is_empty(), zvcs_run.4.is_empty(), "{verb} {args:?}: request written");
    assert_eq!(
        stock_run.4.ends_with(b"0000"),
        zvcs_run.4.ends_with(b"0000"),
        "{verb} {args:?}: request tail"
    );
}

#[test]
fn send_pack_parity_source_may_be_any_object_name() {
    check("tilde", "send-pack", &["../dst.git", "main~1:refs/heads/main"]);
    check("hex", "send-pack", &["../dst.git", "{hex}:refs/heads/x"]);
    check("at", "send-pack", &["../dst.git", "@:refs/heads/at"]);
}

#[test]
fn send_pack_parity_matched_tag_is_pushed_unpeeled() {
    check("tag", "send-pack", &["../dst.git", "v1:refs/tags/v1"]);
}

#[test]
fn send_pack_parity_failed_match_closes_without_writing() {
    check("nosuch", "send-pack", &["../dst.git", "nosuch:refs/heads/x"]);
    check("mixed", "send-pack", &["../dst.git", "nosuch:refs/heads/x", "other:refs/heads/y", ":gone"]);
    check("gone", "send-pack", &["../dst.git", ":gone"]);
}

#[test]
fn send_pack_parity_refspec_without_destination_is_parsed_first() {
    check("bare", "send-pack", &["../dst.git", "main~1"]);
}

#[test]
fn push_parity_refused_capability_writes_nothing_more() {
    check("pushopt", "push", &["-o", "x", "../dst.git", "main"]);
    check("signed", "push", &["--signed", "../dst.git", "main"]);
    check("sendsigned", "send-pack", &["--signed", "../dst.git", "main"]);
}
