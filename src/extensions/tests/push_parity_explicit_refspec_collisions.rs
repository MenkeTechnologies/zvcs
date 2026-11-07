//! Two refspecs landing on one remote ref, and a full-length hex source naming
//! an object this repository does not have, in `push` and `send-pack`.
//!
//! `match_explicit_refs()` places every explicit refspec before
//! `match_push_refs()` picks the remainder (remote.c:1590-1660). An explicit
//! refspec whose destination already has a source is
//! `dst ref %s receives from more than one src` (remote.c:1337-1341), the same
//! source twice included, and the destination is matched against the
//! advertisement *and* the refs earlier explicit refspecs linked, so `main:x
//! side:x` and `main:refs/heads/y main:y` collide on a ref the remote lacks. A
//! pattern or the matching refspec skips a destination an explicit refspec has
//! claimed (remote.c:1619-1622) rather than colliding with it, and each local ref
//! goes to the first pattern that matches it, `--tags`'s `refs/tags/*` ahead of
//! the command line's (remote.c:1376-1395, builtin/push.c:864-867).
//!
//! `try_explicit_object_name()` resolves a source no local ref matches with
//! `repo_get_oid()`, which takes a full-length hex without asking the object
//! store, so an absent object is still a source: an unqualified destination is
//! then `show_push_unqualified_ref_name_error()` with its "object ID that
//! doesn't exist" advice (remote.c:1206-1266), and a qualified one reaches
//! `pack-objects`, which dies `bad object`, and the receiving end reports the
//! pack it never got (send-pack.c:713-735).
//!
//! The server side is stock `receive-pack` (or stock `http-backend`) for both
//! clients. Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

#[path = "support/cgi_server.rs"]
mod cgi_server;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

const MISSING: &str = "1111111111111111111111111111111111111111";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
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

/// `src` has two commits on `main`, a branch `side` forked from the first with
/// a commit of its own, and an annotated tag `lt` on `side`. `srv/s.git`
/// carries `main`, `side` and `lt`.
fn fixture(stock: &str, tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-pushdup-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("srv")).unwrap();
    let root = root.canonicalize().unwrap();
    let src = root.join("src");
    let ok = |cwd: &Path, args: &[&str]| {
        let out = git(stock, cwd, &root, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    ok(&root, &["init", "-q", "-b", "main", "src"]);
    ok(&root, &["init", "-q", "--bare", "-b", "main", "srv/s.git"]);
    ok(&root.join("srv/s.git"), &["config", "http.receivepack", "true"]);
    std::fs::write(src.join("f"), "1\n").unwrap();
    ok(&src, &["add", "f"]);
    ok(&src, &["commit", "-qm", "c1"]);
    ok(&src, &["branch", "side"]);
    std::fs::write(src.join("f"), "2\n").unwrap();
    ok(&src, &["commit", "-qam", "c2"]);
    ok(&src, &["checkout", "-q", "side"]);
    std::fs::write(src.join("g"), "3\n").unwrap();
    ok(&src, &["add", "g"]);
    ok(&src, &["commit", "-qm", "c3"]);
    ok(&src, &["checkout", "-q", "main"]);
    ok(&src, &["tag", "-a", "-m", "t", "lt", "side"]);
    ok(&src, &["push", "-q", "../srv/s.git", "main", "side", "lt"]);
    Fixture { root }
}

#[derive(Debug, PartialEq)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    refs: String,
    fsck: bool,
}

/// `args` run in `src` of a fresh fixture by `bin`, `{url}` standing for the
/// destination `dest` names.
fn push(stock: &str, bin: &str, tag: &str, dest: &str, args: &[&str]) -> Run {
    let fx = fixture(stock, tag);
    let root = &fx.root;
    let src = root.join("src");
    let url = match dest {
        "path" => "../srv/s.git".to_owned(),
        "file" => format!("file://{}", root.join("srv/s.git").display()),
        _ => {
            let (port, _) = cgi_server::spawn(stock, root.join("srv"), cgi_server::Fault::NONE);
            format!("http://127.0.0.1:{port}/s.git")
        }
    };
    let args: Vec<String> = args.iter().map(|a| a.replace("{url}", &url)).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git(bin, &src, root, &args);
    let srv = root.join("srv/s.git");
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).replace(root.to_str().unwrap(), "ROOT"),
        // The port is the server's own, a fresh one each run.
        stderr: String::from_utf8_lossy(&out.stderr).replace(&url, "URL").replace(root.to_str().unwrap(), "ROOT"),
        refs: String::from_utf8_lossy(&git(stock, &srv, root, &["for-each-ref"]).stdout).into_owned(),
        fsck: git(stock, &srv, root, &["fsck", "--no-progress"]).status.success(),
    }
}

fn compare(stock: &str, tag: &str, dest: &str, args: &[&str]) -> Run {
    let mut want = push(stock, stock, &format!("{tag}-s"), dest, args);
    let mut got = push(stock, ZVCS, &format!("{tag}-z"), dest, args);
    // When send-pack dies under remote-curl, its last `fatal:` and the parent's
    // `failed to push some refs` race for stderr in stock git, so over http a
    // push that ends that way is compared line for line in any order.
    if dest == "http" && want.stderr.contains("unexpected disconnect while reading sideband packet") {
        for run in [&mut want, &mut got] {
            let mut lines: Vec<&str> = run.stderr.lines().collect();
            lines.sort_unstable();
            run.stderr = lines.join("\n");
        }
    }
    assert_eq!(got, want, "{tag} {dest} {args:?}: zvcs (left) and stock git (right) differ");
    want
}

/// `refspecs` in `push` over a path, file:// and smart HTTP, and in `send-pack`
/// over a path and file:// unless it starts with a `push` option.
fn compare_everywhere(stock: &str, tag: &str, refspecs: &[&str]) -> Vec<Run> {
    let mut runs = Vec::new();
    for dest in ["path", "file", "http"] {
        let mut args = vec!["push", "{url}"];
        args.extend_from_slice(refspecs);
        runs.push(compare(stock, &format!("{tag}-push-{dest}"), dest, &args));
        if dest != "http" && !refspecs[0].starts_with("--") {
            let mut args = vec!["send-pack", "{url}"];
            args.extend_from_slice(refspecs);
            runs.push(compare(stock, &format!("{tag}-sp-{dest}"), dest, &args));
        }
    }
    runs
}

#[test]
fn push_parity_explicit_refspecs_on_one_destination_are_refused() {
    let Some(stock) = stock_git() else { return };
    for (tag, refspecs) in [
        ("tag-twice", &["main:lt", "main:refs/tags/lt"][..]),
        ("tag-two-srcs", &["side:lt", "main:refs/tags/lt"]),
        ("bare-twice", &["side", "main", "main"]),
        ("full-twice", &["main:refs/heads/x", "main:refs/heads/x"]),
        ("guessed-twice", &["main:x", "side:x"]),
        ("linked-then-short", &["main:refs/heads/y", "main:y"]),
        ("delete-linked", &[":refs/heads/zz", ":zz"]),
    ] {
        for run in compare_everywhere(stock, tag, refspecs) {
            assert!(run.stderr.contains("receives from more than one src"), "{tag}: {run:?}");
        }
    }
}

#[test]
fn push_parity_pattern_and_matching_skip_an_explicitly_claimed_destination() {
    let Some(stock) = stock_git() else { return };
    for (tag, refspecs) in [
        ("pattern-first", &["refs/heads/*:refs/heads/*", "+main:side"][..]),
        ("pattern-last", &["+main:side", "refs/heads/*:refs/heads/*"]),
        ("matching", &["+main:side", ":"]),
        ("created-order", &["refs/heads/*:refs/heads/n/*", "main:new"]),
        ("two-patterns", &["refs/heads/*:refs/heads/a/*", "refs/heads/*:refs/heads/b/*"]),
        ("pattern-then-matching", &["refs/heads/*:refs/heads/a/*", ":"]),
        ("tags-first", &["--tags", "refs/tags/*:refs/heads/t/*"]),
        ("tags-claimed", &["--tags", "+main:refs/tags/lt"]),
    ] {
        for run in compare_everywhere(stock, tag, refspecs) {
            assert_eq!(run.code, Some(0), "{tag}: {run:?}");
        }
    }
}

#[test]
fn push_parity_full_hex_source_without_its_object() {
    let Some(stock) = stock_git() else { return };
    let unqualified = format!("{MISSING}:x");
    let qualified = format!("{MISSING}:refs/heads/x");
    for (tag, refspecs) in [
        ("unqualified", vec![unqualified.as_str()]),
        ("qualified", vec![qualified.as_str()]),
        ("qualified-with-main", vec!["main", qualified.as_str()]),
        ("porcelain", vec!["--porcelain", qualified.as_str()]),
    ] {
        for run in compare_everywhere(stock, tag, &refspecs) {
            assert_ne!(run.code, Some(0), "{tag}: {run:?}");
            assert!(run.fsck, "{tag}: {run:?}");
        }
    }
}
