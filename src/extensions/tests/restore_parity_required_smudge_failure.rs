//! A smudge filter that fails, checked against stock on twin fixtures.
//!
//! * A `required` driver that fails is `convert_to_working_tree_ca_internal()`'s
//!   `die("%s: smudge filter %s failed")` (convert.c:1516-1517), reached from
//!   `write_entry()` after `checkout_entry_ca()` has unlinked the old file
//!   (entry.c:565-577): `fatal:` at 128 with the path gone, for `restore`,
//!   `checkout -- <path>`, `checkout -f <branch>` and `reset --hard` alike, and a
//!   clone leaves the repository with `remove_junk()`'s warning. zvcs said
//!   `zvcs: <cmd>: …` at 1 and left the old file in place.
//! * A command that will not start is `start_command()`'s `cannot exec` /
//!   `cannot run` (run-command.c:378-405, 757-761), followed by
//!   `filter_buffer_or_fd()`'s `cannot fork to run external filter`
//!   (convert.c:665-668) or `subprocess_start()`'s `cannot fork to run
//!   subprocess` (sub-process.c:93-96).
//! * A `process` filter whose handshake fails is reported step by step
//!   (sub-process.c:111-205), then `initialization for subprocess … failed`, then
//!   `subprocess_stop()`'s `SIGTERM` and `wait_or_whine()`'s `died of signal`.
//!   zvcs printed none of these.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
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
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A directory both sides share, so the filter scripts have one path in every message.
fn scripts() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-smudge-fail-bin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let write = |name: &str, body: &str| {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    // Stdout closed before a greeting, stdin held open until the filter is stopped.
    write("eofgreet", "#!/bin/sh\nexec >&-\nexec cat >/dev/null\n");
    write("badgreet", "#!/bin/sh\nprintf '0016git-filter-xerver\\n'\nexec cat >/dev/null\n");
    dir
}

/// `f` (filtered by `bad`) and `g` at `main`, both changed on `two`, then `f`
/// modified in the worktree, with `filter.bad.<config>` set.
fn fixture(stock: &str, label: &str, config: &[(&str, &str)], dirty: bool) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-smudge-fail-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let ok = |args: &[&str]| {
        let args = [&["-c", "maintenance.auto=false"], args].concat();
        assert_eq!(git(stock, &dir, &args).0, 0, "{args:?}");
    };
    ok(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("f"), "x\n").unwrap();
    std::fs::write(dir.join("g"), "g\n").unwrap();
    std::fs::write(dir.join(".gitattributes"), "f filter=bad\n").unwrap();
    ok(&["add", "."]);
    ok(&["commit", "-q", "-m", "one"]);
    ok(&["checkout", "-q", "-b", "two"]);
    std::fs::write(dir.join("f"), "y\n").unwrap();
    std::fs::write(dir.join("g"), "h\n").unwrap();
    ok(&["commit", "-q", "-am", "two"]);
    ok(&["checkout", "-q", "main"]);
    for (key, value) in config {
        ok(&["config", &format!("filter.bad.{key}"), value]);
    }
    if dirty {
        std::fs::write(dir.join("f"), "mod\n").unwrap();
    }
    dir
}

fn same(label: &str, config: &[(&str, &str)], dirty: bool, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(ZVCS, "zvcs"), (stock, "stock")] {
        let dir = fixture(stock, &format!("{label}-{side}"), config, dirty);
        let out = git(bin, &dir, args);
        let index = git(stock, &dir, &["ls-files", "-s"]);
        let head = git(stock, &dir, &["rev-parse", "HEAD"]);
        let files: Vec<Option<String>> = ["f", "g"].iter().map(|f| std::fs::read_to_string(dir.join(f)).ok()).collect();
        seen.push((out, index, head, files));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[0], seen[1], "git {args:?} under {config:?}: left is zvcs, right is stock");
}

const REQUIRED_FALSE: &[(&str, &str)] = &[("smudge", "false"), ("required", "true")];

#[test]
fn a_required_smudge_failure_dies_with_the_path_unlinked() {
    same("restore", REQUIRED_FALSE, true, &["restore", "f"]);
    same("checkout-path", REQUIRED_FALSE, true, &["checkout", "--", "f"]);
    same("checkout-force", REQUIRED_FALSE, false, &["checkout", "-f", "two"]);
    same("reset-hard", REQUIRED_FALSE, true, &["reset", "--hard", "two"]);
    same("optional", &[("smudge", "false")], true, &["restore", "--source=two", "f"]);
}

#[test]
fn a_filter_that_will_not_start_is_reported() {
    let missing = "/nonexistent/zvcs-filter";
    same("exec-smudge", &[("smudge", missing)], false, &["restore", "--source=two", "f"]);
    same("run-smudge", &[("smudge", "zvcs-no-such-filter"), ("required", "true")], false, &["restore", "--source=two", "f"]);
    same("exec-process", &[("process", missing)], false, &["restore", "--source=two", "f"]);
}

#[test]
fn a_failed_process_handshake_is_reported_and_the_filter_stopped() {
    let bin = scripts();
    let eof = bin.join("eofgreet");
    let bad = bin.join("badgreet");
    same("eof", &[("process", eof.to_str().unwrap())], false, &["restore", "--source=two", "f"]);
    same("greeting", &[("process", bad.to_str().unwrap()), ("required", "true")], false, &["restore", "--source=two", "f"]);
    let _ = std::fs::remove_dir_all(bin);
}

#[test]
fn a_clone_keeps_the_repository_when_a_required_smudge_fails() {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(ZVCS, "zvcs"), (stock, "stock")] {
        let dir = fixture(stock, &format!("clone-{side}"), &[], false);
        let args = ["-c", "filter.bad.smudge=false", "-c", "filter.bad.required=true", "-c", "maintenance.auto=false"];
        let out = git(bin, &dir, &[&args[..], &["clone", "-q", ".", "../c"]].concat());
        let clone = dir.parent().unwrap().join("c");
        let mut names: Vec<String> = std::fs::read_dir(&clone)
            .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort();
        let index_exists = clone.join(".git/index").exists() || clone.join(".git/index.lock").exists();
        seen.push((out, names, index_exists));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[0], seen[1], "left is zvcs, right is stock");
}
