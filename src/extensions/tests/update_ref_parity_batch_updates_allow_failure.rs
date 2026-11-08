//! `update-ref --stdin --batch-updates` begins its transaction with
//! `REF_TRANSACTION_ALLOW_FAILURE` (builtin/update-ref.c:706-707, :829-830), so a
//! `prepare` — the explicit command or the one `commit` and the end of input run —
//! records an update the files backend refuses instead of failing: deleting a ref
//! that is not there with an old value is `reference does not exist`
//! (`lock_raw_ref()`, refs/files-backend.c:885-897), where zvcs died with gitoxide's
//! "for deletion did not exist" whenever a `reference-transaction` hook was
//! installed. A refused deletion takes no part in the nested `packed-refs`
//! transaction (:3021-3058), so the hook sees no `aborted` run for it.
//!
//! And `write_with_updates()` (refs/packed-backend.c:1407-1418) always writes the
//! `packed-refs` header: deleting the last packed ref leaves the file holding the
//! header alone, where zvcs removed it.
//!
//! Each case runs through both binaries in identical fixtures built at the same
//! path; the output, the hook's log, the refs, the reflogs and `packed-refs` must
//! agree. Stock git is the oracle (`support/stock_git.rs`).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, cwd: &Path, root: &Path, args: &[&str], input: &str) -> std::process::Output {
    let mut child = Command::new(bin)
        .args(["-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn ok(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> String {
    let out = git(bin, cwd, root, args, "");
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository with `main` at `two` and `side` at `one`, packed when `pack`,
/// with a `reference-transaction` hook logging every phase. `$A` and `$B` in
/// `input` stand for `one` and `two`. Run `args` with `bin` and report what it
/// printed and the state it left.
fn run(stock: &str, root: &Path, bin: &str, pack: bool, args: &[&str], input: &str) -> String {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    ok(stock, root, root, &["init", "-q", "-b", "main", "r"]);
    let repo = root.join("r");
    ok(stock, &repo, root, &["commit", "-q", "--allow-empty", "-m", "one"]);
    ok(stock, &repo, root, &["commit", "-q", "--allow-empty", "-m", "two"]);
    let a = ok(stock, &repo, root, &["rev-parse", "HEAD~1"]);
    let b = ok(stock, &repo, root, &["rev-parse", "HEAD"]);
    ok(stock, &repo, root, &["branch", "side", &a]);
    if pack {
        ok(stock, &repo, root, &["pack-refs", "--all"]);
    }
    let log = root.join("hook.log");
    let hook = repo.join(".git/hooks/reference-transaction");
    std::fs::write(&hook, format!("#!/bin/sh\necho \"== $1\" >> '{0}'\ncat >> '{0}'\n", log.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

    let input = input.replace("$A", &a).replace("$B", &b);
    let out = git(bin, &repo, root, args, &input);
    let refs = git(stock, &repo, root, &["for-each-ref"], "");
    let mut logs = String::new();
    for r in ["HEAD", "refs/heads/main", "refs/heads/side"] {
        if let Ok(raw) = std::fs::read_to_string(repo.join(".git/logs").join(r)) {
            logs += &format!("{r}:\n{raw}");
        }
    }
    format!(
        "exit {:?}\n--- stdout\n{}--- stderr\n{}--- hook\n{}--- refs\n{}--- logs\n{logs}--- packed-refs\n{:?}\n",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(&log).unwrap_or_default(),
        String::from_utf8_lossy(&refs.stdout),
        std::fs::read(repo.join(".git/packed-refs")).ok().map(|b| String::from_utf8_lossy(&b).into_owned()),
    )
}

fn compare(name: &str, args: &[&str], input: &str) {
    let Some(stock) = stock_git() else { return };
    for pack in [false, true] {
        let root = std::env::temp_dir().join(format!("zvcs-update-ref-allow-failure-{name}-{pack}-{}", std::process::id()));
        let fx = Fixture { root };
        let want = run(stock, &fx.root, stock, pack, args, input);
        let got = run(stock, &fx.root, ZVCS, pack, args, input);
        assert_eq!(got, want, "{name} (packed: {pack})");
    }
}

const BATCH: &[&str] = &["update-ref", "--stdin", "--batch-updates"];

#[test]
fn deleting_a_missing_ref_is_rejected_not_fatal() {
    compare("delete-missing", BATCH, "delete refs/heads/nonexist $A\ncreate refs/heads/n2 $A\n");
    compare(
        "delete-missing-prepare",
        BATCH,
        "start\ndelete refs/heads/nonexist $A\ncreate refs/heads/n2 $A\nprepare\ncommit\n",
    );
}

#[test]
fn a_refused_deletion_takes_no_part_in_the_packed_refs_transaction() {
    compare(
        "delete-stale",
        BATCH,
        "delete refs/heads/side $B\ndelete refs/heads/main\nupdate refs/heads/n4 $A\n",
    );
}

#[test]
fn other_refusals_leave_the_rest_of_the_batch() {
    compare("stale", BATCH, "update refs/heads/side $B $B\ncreate refs/heads/n2 $A\n");
    compare("name-conflict", BATCH, "create refs/heads/side/y $A\ncreate refs/heads/n3 $B\n");
}

#[test]
fn deleting_the_last_packed_ref_keeps_the_header() {
    compare("delete-every-ref", &["update-ref", "--stdin"], "delete refs/heads/main\ndelete refs/heads/side\n");
}
