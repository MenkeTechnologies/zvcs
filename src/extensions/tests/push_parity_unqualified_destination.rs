//! An explicit `<src>:<dst>` whose `<dst>` is not `refs/`-qualified, in `push`
//! and `send-pack`.
//!
//! `match_explicit()` (remote.c:1268-1349) runs `count_refspec_match()` over the
//! remote's advertisement first, so `main:side` lands on the `side` the remote
//! has and `main:dup` against a remote holding `refs/heads/dup` and
//! `refs/tags/dup` is "dst refspec dup matches more than one". Only a name the
//! remote lacks is guessed, by `guess_ref()` (remote.c:1158-1179): the namespace
//! the source resolves to, `refs/heads/` or `refs/tags/`, `HEAD` followed to its
//! branch. A source that resolves to neither (an object name such as `main~1`, a
//! remote-tracking ref) is `show_push_unqualified_ref_name_error()`
//! (remote.c:1206-1266): its `error()`, then the `advise()` chosen by the type of
//! the object pushed, unless `advice.pushUnqualifiedRefName` is off.
//!
//! `match_explicit_refs()` walks the refspecs once (remote.c:1351-1358), so
//! `send-pack`, which matches sources and destinations after connecting
//! (builtin/send-pack.c:279-312), prints its source errors and its destination
//! errors in refspec order.
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

/// `src` has two commits on `main`, a branch `side`, a lightweight tag `lt`, an
/// annotated tag `at` and the remote-tracking ref `refs/remotes/origin/rt` at
/// `main~1`. `srv/s.git` carries `main`, `side`, `lt`, `at`, and `dup` as both a
/// branch and a tag, and `refs/remotes/o/w`, which a bare `o/w` matches only
/// weakly.
fn fixture(stock: &str, tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-pushunq-{tag}-{}", std::process::id()));
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
    for i in 1..=2 {
        std::fs::write(src.join("f"), format!("{i}\n")).unwrap();
        ok(&src, &["add", "f"]);
        ok(&src, &["commit", "-qm", &format!("c{i}")]);
    }
    ok(&src, &["branch", "side"]);
    ok(&src, &["tag", "lt"]);
    ok(&src, &["tag", "-a", "-m", "t", "at"]);
    ok(&src, &["update-ref", "refs/remotes/origin/rt", "main~1"]);
    ok(
        &src,
        &[
            "push", "-q", "../srv/s.git", "main", "side", "lt", "at", "main:refs/heads/dup", "main:refs/tags/dup",
            "main:refs/remotes/o/w",
        ],
    );
    Fixture { root }
}

#[derive(Debug, PartialEq)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    refs: String,
}

/// `args` run in `src` of a fresh fixture by `bin`. `{url}` is the destination
/// `dest` names; `%hex`, `%tag`, `%tree` and `%blob` are full object names of
/// `main~1`, `at`, `main^{tree}` and `main:f`.
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
    let oid = |rev: &str| String::from_utf8(git(stock, &src, root, &["rev-parse", rev]).stdout).unwrap().trim().to_owned();
    let names = [("%hex", oid("main~1")), ("%tag", oid("at")), ("%tree", oid("main^{tree}")), ("%blob", oid("main:f"))];
    let args: Vec<String> = args
        .iter()
        .map(|a| names.iter().fold(a.replace("{url}", &url), |a, (k, v)| a.replace(k, v)))
        .collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git(bin, &src, root, &args);
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).replace(root.to_str().unwrap(), "ROOT"),
        // The port is the server's own, a fresh one each run.
        stderr: String::from_utf8_lossy(&out.stderr).replace(&url, "URL").replace(root.to_str().unwrap(), "ROOT"),
        refs: String::from_utf8_lossy(&git(stock, &root.join("srv/s.git"), root, &["for-each-ref"]).stdout).into_owned(),
    }
}

fn compare(stock: &str, tag: &str, dest: &str, args: &[&str]) -> Run {
    let want = push(stock, stock, &format!("{tag}-s"), dest, args);
    let got = push(stock, ZVCS, &format!("{tag}-z"), dest, args);
    assert_eq!(got, want, "{tag} {dest} {args:?}: zvcs (left) and stock git (right) differ");
    want
}

/// Every refspec in `push` over a path, file:// and smart HTTP, and in
/// `send-pack` over a path and file://, with `advice.pushUnqualifiedRefName`
/// on and off.
fn compare_everywhere(stock: &str, tag: &str, refspecs: &[&str]) -> Vec<Run> {
    let mut runs = Vec::new();
    for advice in ["true", "false"] {
        let advice = format!("advice.pushUnqualifiedRefName={advice}");
        for dest in ["path", "file", "http"] {
            let mut args = vec!["-c", &advice, "push", "{url}"];
            args.extend_from_slice(refspecs);
            runs.push(compare(stock, &format!("{tag}-push-{dest}"), dest, &args));
            if dest != "http" {
                let mut args = vec!["-c", &advice, "send-pack", "{url}"];
                args.extend_from_slice(refspecs);
                runs.push(compare(stock, &format!("{tag}-sp-{dest}"), dest, &args));
            }
        }
    }
    runs
}

#[test]
fn push_parity_unqualified_destination_of_an_object_name_is_refused() {
    let Some(stock) = stock_git() else { return };
    for (tag, spec) in [
        ("tilde", "main~1:newb"),
        ("hex", "%hex:newb"),
        ("tree", "main^{tree}:newt"),
        ("tagobj", "%tag:newt"),
        ("blob", "%blob:newt"),
        ("tracking", "origin/rt:newr"),
        ("tracking-full", "refs/remotes/origin/rt:newr"),
    ] {
        for run in compare_everywhere(stock, tag, &[spec]) {
            assert_ne!(run.code, Some(0), "{tag}: {run:?}");
            assert!(run.stderr.contains("You must fully qualify the ref."), "{tag}: {run:?}");
        }
    }
}

#[test]
fn push_parity_unqualified_destination_is_matched_then_guessed() {
    let Some(stock) = stock_git() else { return };
    for (tag, spec) in [
        ("branch", "main:newb"),
        ("light-tag", "lt:newt"),
        ("ann-tag", "at:newt"),
        ("head", "HEAD:newh"),
        ("advertised", "main~1:side"),
        ("forced-advertised", "+main~1:side"),
        ("weak", "main~1:o/w"),
    ] {
        compare_everywhere(stock, tag, &[spec]);
    }
}

#[test]
fn push_parity_unqualified_destination_matching_two_remote_refs() {
    let Some(stock) = stock_git() else { return };
    for (tag, spec) in [("update", "main~1:dup"), ("delete", ":dup")] {
        for run in compare_everywhere(stock, tag, &[spec]) {
            assert!(run.stderr.contains("error: dst refspec dup matches more than one"), "{tag}: {run:?}");
        }
    }
}

#[test]
fn push_parity_match_errors_come_out_in_refspec_order() {
    let Some(stock) = stock_git() else { return };
    compare_everywhere(stock, "mixed-dst", &["main~1:a", "main:dup", ":nonex", "lt:b"]);
    // `send-pack` matches its sources after connecting too, so a source that
    // names nothing sits between the destination errors around it.
    for dest in ["path", "file"] {
        compare(stock, &format!("order-{dest}"), dest, &["send-pack", "{url}", ":nonex", "nosuch:x", ":nonex2", "alsonot:y"]);
        compare(stock, &format!("order2-{dest}"), dest, &["send-pack", "{url}", "nosuch:x", "main~1:a", "main:dup", "nope:z"]);
    }
}
