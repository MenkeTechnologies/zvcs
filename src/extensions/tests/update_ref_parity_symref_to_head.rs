//! `git update-ref --stdin` updating a branch and, in the same transaction, a
//! symref that points at `HEAD` (`refs/heads/sym -> HEAD -> refs/heads/main`).
//!
//! On the files backend `lock_ref_for_update()` gives the branch update its
//! log-only `HEAD` split (`split_head_update()`), which takes `HEAD`'s lock, and
//! moves the symref's update onto `HEAD` (`split_symref_update()`). That second
//! `HEAD` update finds the lock already taken, reuses it without
//! `lock_raw_ref()` setting its type, takes it for a regular ref and writes its
//! new value into a lock file the first user already closed: `cannot update
//! ref 'HEAD': couldn't write '<gitdir>/HEAD.lock'`, a generic error that dies
//! at 128 with `--batch-updates` too, and under `prepare` (refs/files-backend.c:
//! 2499-2610, :2687-2900, :2056-2076). zvcs said `Edit preprocessing failed
//! ... has multiple edits`, and applied the update under `--batch-updates`. The
//! reftable backend refuses the pair as a name conflict, which already agreed.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, dir: &Path, root: &Path, args: &[&str], stdin: Option<&str>) -> (String, String, i32) {
    let mut child = Command::new(bin)
        .args(["-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(dir)
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
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.unwrap_or("").as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// A repository with `main` at `B` (its parent `A`) and `refs/heads/sym ->
/// HEAD`, built by stock git at `root/repo`; `bin` then runs `update-ref
/// --stdin <flags>` over `script`, with `A` and `B` substituted, and the result
/// is reported with the refs, `HEAD` and the reflogs left behind.
fn scenario(stock: &str, bin: &str, root: &Path, format: &str, flags: &[&str], script: &str) -> String {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    run(stock, root, root, &["init", "-q", "-b", "main", &format!("--ref-format={format}"), "repo"], None);
    let repo = root.join("repo");
    for text in ["a", "b"] {
        std::fs::write(repo.join("f"), text).unwrap();
        run(stock, &repo, root, &["add", "f"], None);
        run(stock, &repo, root, &["commit", "-q", "-m", text], None);
    }
    run(stock, &repo, root, &["symbolic-ref", "refs/heads/sym", "HEAD"], None);
    let a = run(stock, &repo, root, &["rev-parse", "HEAD~1"], None).0.trim().to_owned();
    let b = run(stock, &repo, root, &["rev-parse", "HEAD"], None).0.trim().to_owned();
    let script = script.replace("<A>", &a).replace("<B>", &b);
    let mut args = vec!["update-ref", "--stdin"];
    args.extend(flags);
    let (out, err, code) = run(bin, &repo, root, &args, Some(&script));
    let refs = run(stock, &repo, root, &["for-each-ref", "--format=%(refname) %(objectname) %(symref)"], None).0;
    let head = run(stock, &repo, root, &["symbolic-ref", "HEAD"], None).0;
    let mut logs = String::new();
    for name in ["HEAD", "refs/heads/main", "refs/heads/sym"] {
        logs += &run(stock, &repo, root, &["reflog", "show", "--format=%H %gs", name], None).0;
    }
    let locks: Vec<_> = std::fs::read_dir(repo.join(".git"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".lock"))
        .collect();
    format!("{code}\n{out}--\n{err}--\n{refs}--\n{head}--\n{logs}--\n{locks:?}\n")
}

fn compare(label: &str, flags: &[&str], script: &str) {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-update-ref-symref-to-head-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let root = Root(std::fs::canonicalize(&base).unwrap());
    for format in ["files", "reftable"] {
        let want = scenario(stock, stock, &root.0, format, flags, script);
        let got = scenario(stock, ZVCS, &root.0, format, flags, script);
        assert_eq!(got, want, "{label} ({format})");
    }
}

#[test]
fn a_branch_and_a_symref_to_head_die_on_heads_reused_lock() {
    let script = "update refs/heads/main <A>\nupdate refs/heads/sym <A>\n";
    compare("plain", &[], script);
}

#[test]
fn batch_updates_dies_on_heads_reused_lock_too() {
    let script = "update refs/heads/main <A>\nupdate refs/heads/sym <A>\n";
    compare("batch", &["--batch-updates"], script);
}

#[test]
fn prepare_reports_the_failed_write() {
    let script = "start\nupdate refs/heads/main <A>\nupdate refs/heads/sym <A>\nprepare\ncommit\n";
    compare("prepare", &[], script);
}

#[test]
fn the_symref_alone_goes_through() {
    compare("alone", &[], "update refs/heads/sym <A>\n");
}

#[test]
fn a_rejected_old_value_comes_first() {
    let script = "update refs/heads/main <A> <A>\nupdate refs/heads/sym <A>\n";
    compare("old", &[], script);
}
