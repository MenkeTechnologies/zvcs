//! `git update-ref --stdin` without `--batch-updates`, in both ref formats.
//!
//! files_transaction_prepare() is the files backend's one prepare path: it locks the
//! updates in order, splitting a symref or `HEAD` update as lock_ref_for_update()
//! reaches it, and the first refusal fails the transaction with its own text
//! (refs/files-backend.c:2687-2905, :3020-3033). So a stale symref update queued
//! before a missing reference is reported first, a missing reference before a name
//! conflict met later, a loose reference in the way before a pair of new names the
//! packed-refs check catches once every lock is held (:3064-3082), and a verification
//! of absence beside a create of the same name is ref_update_reject_duplicates()'s
//! (refs.c:2708-2710). zvcs checked the whole batch up front in its own order.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

const HOOK: &str = "#!/bin/sh\necho \"== $1\" >>\"$GIT_DIR/../hook.log\"\ncat >>\"$GIT_DIR/../hook.log\"\n";

/// Each case: whether it runs in a reftable repository too, whether the
/// `reference-transaction` hook is installed, the arguments after `update-ref`, and
/// the input, with `{A}` and `{B}` standing for `main~1` and `main` and `{Z}` for the null id.
const CASES: &[(bool, bool, &[&str], &str)] = &[
    (
        true,
        false,
        &["--stdin"],
        "update refs/heads/alias {B} {B}\nupdate refs/heads/stale {B} {B}\ncreate refs/heads/side/x {B}\nverify refs/heads/nope {B}\n",
    ),
    (true, false, &["--stdin"], "verify refs/heads/nope {B}\ncreate refs/heads/side/y {B}\n"),
    (
        true,
        true,
        &["--stdin"],
        "start\nverify refs/heads/alias {A}\ncreate refs/heads/side/y {B}\ndelete refs/heads/packed\nupdate refs/heads/nope {B} {A}\nprepare\ncommit\n",
    ),
    (true, true, &["--stdin"], "delete refs/heads/packed\nupdate refs/heads/stale {B} {B}\n"),
    (true, false, &["--stdin"], "create refs/heads/q {B}\ncreate refs/heads/loose/s {B}\ncreate refs/heads/q/r {B}\n"),
    (true, false, &["--stdin"], "verify refs/heads/main {Z}\ncreate refs/heads/main {B}\n"),
    (
        true,
        false,
        &["--stdin", "--batch-updates"],
        "option no-deref\nsymref-update refs/heads/alias refs/heads/main ref refs/heads/main\nsymref-create refs/heads/side refs/heads/main\n",
    ),
];

fn run(bin: &str, dir: &Path, args: &[&str], stdin: &str) -> (String, String, i32) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code().unwrap_or(-1))
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// `main` at `a` then `b`; `side`, `packed` and the tag `t1` at `main~1`, packed;
/// `loose` at `main`; `alias` pointing at `side` and `stale` at a missing branch.
fn template(stock: &str, root: &Path, format: &str) -> PathBuf {
    let repo = root.join(format!("template-{format}"));
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let (_, err, code) = run(stock, &repo, args, "");
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    git(&["init", "-q", "-b", "main", &format!("--ref-format={format}"), "."]);
    for name in ["a", "b"] {
        std::fs::write(repo.join(name), format!("{name}\n")).unwrap();
        git(&["add", name]);
        git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", name]);
    }
    git(&["branch", "side", "main~1"]);
    git(&["branch", "packed", "main~1"]);
    git(&["tag", "t1", "main~1"]);
    git(&["pack-refs", "--all"]);
    git(&["branch", "loose"]);
    git(&["symbolic-ref", "refs/heads/alias", "refs/heads/side"]);
    git(&["symbolic-ref", "refs/heads/stale", "refs/heads/missing"]);
    repo
}

/// A `reference-transaction` hook appending each phase and its input to `hook.log`.
fn install_hook(repo: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let hook = repo.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, HOOK).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The command's answer, then the repository as stock reads it back.
fn outcome(stock: &str, bin: &str, repo: &Path, args: &[&str], input: &str) -> Vec<(String, String, i32)> {
    let mut command = vec!["update-ref"];
    command.extend(args);
    let mut seen = vec![run(bin, repo, &command, input)];
    seen.push(run(stock, repo, &["for-each-ref", "--format=%(refname) %(objectname) %(symref)"], ""));
    // A reftable repository keeps its logs in the tables, which the reftable cases
    // leave to the hook's view.
    for log in ["HEAD", "refs/heads/main", "refs/heads/alias", "refs/heads/side", "refs/heads/stale"] {
        let raw = std::fs::read(repo.join(".git/logs").join(log)).unwrap_or_default();
        seen.push((String::from_utf8_lossy(&raw).into_owned(), String::new(), 0));
    }
    let hook = std::fs::read(repo.join("hook.log")).unwrap_or_default();
    seen.push((String::from_utf8_lossy(&hook).into_owned(), String::new(), 0));
    seen
}

fn check(format: &str) {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-update-ref-prepare-in-order-{format}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let template = template(stock, &root, format);
    let ids = run(stock, &template, &["rev-parse", "main~1", "main"], "").0;
    let (a, b) = ids.trim().split_once('\n').expect("two ids");
    for (n, &(reftable, hook, args, input)) in CASES.iter().enumerate() {
        if format == "reftable" && !reftable {
            continue;
        }
        let input = input.replace("{A}", &a).replace("{B}", &b).replace("{Z}", &"0".repeat(a.len()));
        let mut seen = Vec::new();
        for (side, bin) in [("stock", stock), ("zvcs", ZVCS)] {
            let repo = root.join(format!("{format}-{n}-{side}"));
            copy_dir(&template, &repo);
            if hook {
                install_hook(&repo);
            }
            seen.push(outcome(stock, bin, &repo, args, &input));
        }
        assert_eq!(seen[1], seen[0], "{format}-{n}: update-ref {args:?}\n{input}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_files_transaction_fails_on_the_first_update_it_locks_and_refuses() {
    check("files");
}

#[test]
fn a_reftable_transaction_refuses_the_same_updates() {
    check("reftable");
}
