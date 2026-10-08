//! `--deepen` is sent next to `--shallow-since` / `--shallow-exclude`, and the server refuses the pair.
//!
//! `cmd_fetch()` folds `--deepen=<n>` into the same `depth` string `--depth` fills
//! (builtin/fetch.c:2666-2670) and hands it to the transport together with `deepen_since` and
//! `deepen_not`; `upload-pack.c:send_shallow_list()` then dies with `deepen and deepen-since (or
//! deepen-not) cannot be used together`, which the client follows with `the remote end hung up
//! unexpectedly`, exit 128. `--depth` already behaved that way in zvcs. `--deepen` was dropped as soon
//! as a rev-list selector was named, so the fetch succeeded and moved the shallow boundary.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let root = dir.ancestors().find(|p| p.ends_with("work")).and_then(Path::parent).unwrap_or(dir);
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).replace(root.to_str().unwrap(), "<root>"),
        out.status.code().expect("no signal"),
    )
}

/// `root/work` is a depth-1 clone of `root/up`, which has gained two commits since.
fn world(label: &str, stock: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-fetch-deepen-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let run = |dir: &Path, args: &[&str]| {
        let out = git(stock, dir, args);
        assert_eq!(out.2, 0, "{args:?}: {out:?}");
    };
    run(&root, &["init", "-q", "-b", "main", "up"]);
    let up = root.join("up");
    let commit = |n: u32| {
        std::fs::write(up.join("f"), format!("{n}\n")).unwrap();
        run(&up, &["add", "f"]);
        run(&up, &["commit", "-q", "-m", &format!("c{n}")]);
    };
    commit(0);
    commit(1);
    run(&root, &["clone", "-q", "--depth=1", &format!("file://{}", up.display()), "work"]);
    commit(2);
    root.join("work")
}

fn same(label: &str, args: &[&str]) -> (String, String, i32) {
    let stock = stock_git::stock_git().expect("checked by caller");
    let s = world(&format!("{label}-stock"), stock);
    let z = world(&format!("{label}-zvcs"), stock);
    let want = git(stock, &s, args);
    let got = git(ZVCS, &z, args);
    let shallow = |w: &Path| std::fs::read_to_string(w.join(".git/shallow")).ok().map(|s| s.lines().count());
    let (want_shallow, got_shallow) = (shallow(&s), shallow(&z));
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
    assert_eq!(got, want, "git {args:?}: left is zvcs, right is stock");
    assert_eq!(got_shallow, want_shallow, "git {args:?}: .git/shallow");
    want
}

#[test]
fn deepen_beside_shallow_since_is_refused_by_the_server() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let want = same("since", &["fetch", "--shallow-since=1979-01-01", "--deepen=1"]);
    assert_eq!(want.2, 128, "{want:?}");
    assert!(want.1.contains("deepen and deepen-since (or deepen-not) cannot be used together"), "{want:?}");
}

#[test]
fn deepen_beside_shallow_exclude_is_refused_by_the_server() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let want = same("exclude", &["fetch", "--shallow-exclude=main", "--deepen=1"]);
    assert_eq!(want.2, 128, "{want:?}");
}

#[test]
fn deepen_alone_and_a_zero_deepen_are_unchanged() {
    if stock_git::stock_git().is_none() {
        return;
    }
    same("alone", &["fetch", "--deepen=1"]);
    same("zero", &["fetch", "--deepen=0", "--shallow-since=1979-01-01"]);
}
