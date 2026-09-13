//! `get_oid_1()` parses a `~<n>`/`^<n>` count *before* it recurses, and a count
//! above `INT_MAX` returns `MISSING_OBJECT` on the spot (`object-name.c:1105-1119`).
//! Nothing inside that frame runs: no `get_oid_basic()` and no `get_short_oid()`,
//! so none of their diagnostics are printed for the name the frame encloses —
//! not the short-object-id ambiguity report, not the refname ambiguity warning,
//! not the `@{u}` die, and not an inner `peel_onion()` error.
//!
//! The fixture holds two blobs whose ids share the prefix `a366`, one commit,
//! and `dup` as both a branch and a tag. Every expectation was measured against
//! stock git 2.55.0 (`/opt/homebrew/bin/git`) on the same fixture before being
//! written down; each overflow case is paired with a control one count below
//! the limit that does print.

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
        .env("GIT_AUTHOR_NAME", "zvcs test")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "zvcs test")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
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
    let dir = std::env::temp_dir().join(format!("zvcs-objname-overflow-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(run(&dir, &["init", "-q", "-b", "main"], None).status.success());
    for (file, body) in [("a.txt", "collide 105\n"), ("b.txt", "collide 215\n")] {
        std::fs::write(dir.join(file), body).unwrap();
        let out = run(&dir, &["hash-object", "-w", file], None);
        assert!(out.status.success());
        assert!(out.stdout.starts_with(b"a366"), "premise: {file} hashes under a366");
    }
    assert!(run(&dir, &["add", "a.txt"], None).status.success());
    assert!(run(&dir, &["commit", "-q", "-m", "one"], None).status.success());
    assert!(run(&dir, &["branch", "dup"], None).status.success());
    assert!(run(&dir, &["tag", "dup"], None).status.success());
    dir
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const REPORT: &str = "error: short object ID a366 is ambiguous\n";

#[test]
fn overflowing_count_prints_no_short_oid_report() {
    let dir = fixture("report");
    // Past `UINT_MAX` (the multiply/add overflow returns) and past `INT_MAX`
    // alone, as the outermost suffix, beneath a peel, above a peel and as the
    // revision half of `<rev>:<path>`.
    for name in [
        "a366~99999999999",
        "a366^99999999999",
        "a366~4294967296",
        "a366~2147483648",
        "a366~99999999999^",
        "a366~99999999999^{commit}",
        "a366^{commit}~99999999999",
    ] {
        let out = run(&dir, &["cat-file", "-t", name], None);
        assert_eq!(stderr_of(&out), format!("fatal: Not a valid object name {name}\n"), "cat-file -t {name}");
        let out = run(&dir, &["rev-parse", "--verify", name], None);
        assert_eq!(stderr_of(&out), "fatal: Needed a single revision\n", "rev-parse --verify {name}");
        let out = run(&dir, &["cat-file", "--batch-check"], Some(&format!("{name}\n")));
        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{name} missing\n"), "--batch-check {name}");
        assert_eq!(stderr_of(&out), "", "--batch-check {name}");
    }
    let out = run(&dir, &["cat-file", "-t", "a366~99999999999:a.txt"], None);
    assert_eq!(stderr_of(&out), "fatal: invalid object name 'a366~99999999999'.\n");

    // `INT_MAX` itself recurses, so `get_short_oid()` runs and reports.
    for name in ["a366~2147483647", "a366^0", "a366^{commit}~2147483647"] {
        let out = run(&dir, &["cat-file", "-t", name], None);
        assert!(stderr_of(&out).starts_with(REPORT), "cat-file -t {name}: {}", stderr_of(&out));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overflowing_count_skips_get_oid_basic_diagnostics() {
    let dir = fixture("basic");
    let warn = "warning: refname 'dup' is ambiguous.\n";
    let out = run(&dir, &["cat-file", "-t", "dup~99999999999"], None);
    assert_eq!(stderr_of(&out), "fatal: Not a valid object name dup~99999999999\n");
    let out = run(&dir, &["cat-file", "-t", "dup^99999999999^{commit}"], None);
    assert_eq!(stderr_of(&out), "fatal: Not a valid object name dup^99999999999^{commit}\n");
    let out = run(&dir, &["cat-file", "-t", "dup~0"], None);
    assert_eq!(stderr_of(&out), warn);

    // `interpret_branch_mark()`'s die sits inside `get_oid_basic()` as well.
    let out = run(&dir, &["cat-file", "-t", "@{u}~99999999999"], None);
    assert_eq!(stderr_of(&out), "fatal: Not a valid object name @{u}~99999999999\n");
    let out = run(&dir, &["cat-file", "-t", "@{u}~1"], None);
    assert_eq!(stderr_of(&out), "fatal: no upstream configured for branch 'main'\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overflowing_count_hides_an_enclosed_peel_error() {
    let dir = fixture("peel");
    let error = "error: HEAD^{blob}: expected blob type, but the object dereferences to tree type\n";
    for name in ["HEAD^{blob}~99999999999^{tree}", "HEAD^{blob}^99999999999"] {
        let out = run(&dir, &["cat-file", "-t", name], None);
        assert_eq!(stderr_of(&out), format!("fatal: Not a valid object name {name}\n"), "{name}");
    }
    let out = run(&dir, &["cat-file", "-t", "HEAD^{blob}~1^{tree}"], None);
    assert_eq!(stderr_of(&out), format!("{error}fatal: Not a valid object name HEAD^{{blob}}~1^{{tree}}\n"));
    let _ = std::fs::remove_dir_all(&dir);
}
