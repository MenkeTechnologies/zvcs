//! `batch_one_object()` reports `get_oid_with_context()`'s `SHORT_NAME_AMBIGUOUS`
//! as `<name> ambiguous`, apart from `MISSING_OBJECT` (`builtin/cat-file.c:589-595`).
//!
//! The fixture holds two blobs whose ids share the prefix `a366`. Every
//! expectation was measured against stock git 2.55.0 (`/opt/homebrew/bin/git`)
//! on the same fixture before being written down.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(repo: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .env("ZVCS_HOME", repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match stdin {
        Some(_) => cmd.stdin(Stdio::piped()),
        None => cmd.stdin(Stdio::null()),
    };
    let mut child = cmd.spawn().unwrap();
    if let Some(text) = stdin {
        use std::io::Write as _;
        child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-cat-file-ambiguous-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(run(&dir, &["init", "-q"], None).status.success());
    for (file, body) in [("a.txt", "collide 105\n"), ("b.txt", "collide 215\n")] {
        std::fs::write(dir.join(file), body).unwrap();
        let out = run(&dir, &["hash-object", "-w", file], None);
        assert!(out.status.success());
        assert!(out.stdout.starts_with(b"a366"), "premise: {file} hashes under a366");
    }
    dir
}

#[test]
fn ambiguous_is_reported_only_where_get_oid_1_returns_it() {
    let dir = fixture("status");
    // `^`/`~<n>` hand the inner `get_short_oid()` result back through
    // `get_parent()`/`get_nth_ancestor()`; an overflowing count, a peel, a
    // `<rev>:<path>` and an index path all end at `-1`.
    let input = "a366\na366^\na366~2\na366^^\nA366\na366~99999999999\na366^{tree}\na366:a.txt\n:a366\na36\n";
    let want = "a366 ambiguous\na366^ ambiguous\na366~2 ambiguous\na366^^ ambiguous\nA366 ambiguous\n\
                a366~99999999999 missing\na366^{tree} missing\na366:a.txt missing\n:a366 missing\na36 missing\n";
    for mode in ["--batch-check", "--batch", "--batch-check=%(objectname) %(objecttype)"] {
        let out = run(&dir, &["cat-file", mode], Some(input));
        assert!(out.status.success(), "{mode}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{mode}");
    }
    let out = run(&dir, &["cat-file", "--batch-command"], Some("info a366\ncontents a366~1\n"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a366 ambiguous\na366~1 ambiguous\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ambiguity_report_names_the_lowercased_prefix() {
    let dir = fixture("case");
    let out = run(&dir, &["cat-file", "--batch-check"], Some("A366\n"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("error: short object ID a366 is ambiguous\n"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
