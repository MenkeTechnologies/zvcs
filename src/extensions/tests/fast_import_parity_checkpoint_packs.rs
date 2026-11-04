//! A `checkpoint` ends the current packfile and registers it with the object
//! store before the next object is written (`end_packfile()`,
//! builtin/fast-import.c:923-926), and git's store has no fixed table of packs:
//! `packfile_store_add_pack()` appends to a list (packfile.c:778-786). So one
//! `fast-import` process can write far more packs than the store held when it
//! was opened and still read every one of them back: for the fast-forward check
//! of the next `commit`, for `cat-blob` and for `get-mark`.
//!
//! zvcs's object store used to size its slot map once, at open time, to at
//! least 32 entries, and the 33rd pack failed with "The slotmap turned out to
//! be too small": the branch update reported `branch refs/heads/main is missing
//! commits.` and the run exited non-zero. Stock git is the oracle
//! (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `bin <args>` in `dir` with `stdin`, isolated from the user's configuration.
fn run(bin: &str, dir: &Path, args: &[&str], stdin: &str) -> (String, String, i32) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir.parent().unwrap())
        .env("ZVCS_HOME", dir.parent().unwrap().join("home"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// One commit per `i` in `range`, each on top of the last through its mark and
/// followed by a `checkpoint`; then `progress`, `get-mark` of the commit before
/// it and `cat-blob` of a blob written several packs earlier.
fn stream(range: std::ops::RangeInclusive<u32>) -> String {
    let mut s = String::new();
    for i in range {
        let blob = format!("file {i}\n");
        let msg = format!("c{i}");
        s += &format!("blob\nmark :{}\ndata {}\n{blob}\n", 2 * i - 1, blob.len());
        s += &format!(
            "commit refs/heads/main\nmark :{}\ncommitter A U Thor <a@example.com> {} +0000\ndata {}\n{msg}\n",
            2 * i,
            1_700_000_000 + i,
            msg.len()
        );
        if i > 1 {
            s += &format!("from :{}\n", 2 * i - 2);
        }
        s += &format!("M 100644 :{} f{i}\n\ncheckpoint\n\nprogress p{i}\n", 2 * i - 1);
        if i > 1 {
            s += &format!("get-mark :{}\n", 2 * i - 2);
        }
        s += &format!("cat-blob :{}\n", 2 * i.div_ceil(2) - 1);
    }
    s
}

/// What stock git sees in the repository afterwards.
fn post_state(stock: &str, repo: &Path) -> String {
    let packs = std::fs::read_dir(repo.join(".git/objects/pack"))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().ends_with(".pack"))
                .count()
        })
        .unwrap_or(0);
    let mut state = format!("packs {packs}\n");
    for args in [
        &["for-each-ref"][..],
        &["fsck"],
        &["count-objects", "-v"],
        &["log", "--oneline"],
        &["reflog", "list"],
    ] {
        let (out, err, code) = run(stock, repo, args, "");
        let out: String = out.lines().filter(|l| !l.starts_with("size")).map(|l| format!("{l}\n")).collect();
        state += &format!("$ git {}\n{out}{err}exit {code}\n", args.join(" "));
    }
    state
}

/// A fresh `stock` and `zvcs` repository pair under one temp dir.
fn twins(tag: &str, stock: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-fi-checkpoint-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for side in ["stock", "zvcs"] {
        let repo = root.join(side).join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let (_, err, code) = run(stock, &repo, &["init", "-q", "-b", "main"], "");
        assert_eq!(code, 0, "{err}");
    }
    root
}

/// Import `stream` on both sides and require identical output, exit code,
/// marks file and repository state.
fn import_both(root: &Path, stock: &str, args: &[&str], stream: &str) -> String {
    let mut seen = Vec::new();
    for (side, bin) in [("stock", stock), ("zvcs", BIN)] {
        let repo = root.join(side).join("repo");
        let (out, err, code) = run(bin, &repo, args, stream);
        let marks = std::fs::read_to_string(root.join(side).join("marks")).unwrap_or_default();
        seen.push((out, err, code, marks, post_state(stock, &repo)));
    }
    assert_eq!(seen[0], seen[1], "stock (left) vs zvcs (right) for {args:?}");
    seen.remove(0).4
}

#[test]
fn forty_checkpointed_packs_are_all_readable_in_the_same_run() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = twins("forty", stock);
    let post = import_both(
        &root,
        stock,
        &["-c", "fastimport.unpackLimit=0", "fast-import", "--quiet", "--export-marks=../marks"],
        &stream(1..=40),
    );
    assert!(post.starts_with("packs 40\n"), "{post}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The second run opens a store that already holds 40 packs, so the slots it
/// sizes from disk are again exhausted partway through its own 40.
#[test]
fn a_run_over_imported_marks_adds_as_many_packs_again() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = twins("marks", stock);
    let args = ["-c", "fastimport.unpackLimit=0", "fast-import", "--quiet", "--export-marks=../marks"];
    import_both(&root, stock, &args, &stream(1..=40));
    let cont = stream(41..=80);
    let post = import_both(
        &root,
        stock,
        &[
            "-c",
            "fastimport.unpackLimit=0",
            "fast-import",
            "--quiet",
            "--import-marks=../marks",
            "--export-marks=../marks",
        ],
        &cont,
    );
    assert!(post.starts_with("packs 80\n"), "{post}");
    assert!(post.contains("exit 0\n$ git count-objects"), "fsck must pass: {post}");
    let _ = std::fs::remove_dir_all(&root);
}
