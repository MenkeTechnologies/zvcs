//! The `reference-transaction` hook, run from every ref transaction.
//!
//! git runs it from the generic ref layer, `run_transaction_hook()` (refs.c:2663-2678):
//! at `preparing` over the updates as queued, at `prepared` over them as the backend
//! split them, at `committed`, and at `aborted` (refs.c:2700-2786), each update a line
//! `<old> <new> <refname>` with the null id for a value the update does not carry and
//! `ref:<target>` for a symbolic one. The files backend gives a deletion a nested
//! `packed-refs` transaction that runs the hook too — prepared when `packed-refs` holds
//! the reference, aborted at once otherwise (refs/files-backend.c:3036-3118) — and a
//! refusal at `preparing` or `prepared` aborts and dies with "in '<phase>' phase, update
//! aborted by the reference-transaction hook". zvcs ran no hook at all.
//!
//! Each step runs through both binaries with a hook that appends its phase and stdin to
//! a log; the step's output, the log, and the refs stock reads afterwards must agree.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Stdio};
use twin_repo::Side;

const HOOK: &str = "#!/bin/sh\n\
    printf '== %s\\n' \"$1\" >>\"$HOOKLOG\"\n\
    cat >>\"$HOOKLOG\"\n\
    if test \"$1\" = \"$HOOKFAIL\"; then exit 1; fi\n\
    exit 0\n";

fn install_hook(side: &Side) {
    let path = side.repo().join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, HOOK).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Run `args` in `side`'s repository with `bin`, `input` on stdin, `HOOKFAIL=fail`,
/// and return the exit code, stdout, stderr and what the hook logged.
fn step(side: &Side, bin: &str, args: &[&str], input: &str, fail: &str) -> String {
    let log = side.root.join("hook.log");
    let _ = std::fs::remove_file(&log);
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
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
        .env("HOOKLOG", &log)
        .env("HOOKFAIL", fail)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let root = side.root.to_string_lossy().into_owned();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
    format!(
        "exit={}\n--- stdout\n{}--- stderr\n{}--- hook\n{}",
        out.status.code().unwrap_or(-1),
        text(&out.stdout),
        text(&out.stderr),
        std::fs::read_to_string(&log).unwrap_or_default()
    )
}

/// One command, run with each side's own binary: its arguments, its stdin and `HOOKFAIL`.
struct Step {
    args: &'static [&'static str],
    input: &'static str,
    fail: &'static str,
}

const fn s(args: &'static [&'static str]) -> Step {
    Step { args, input: "", fail: "" }
}

fn compare(format: &str, label: &str, setup: &[&[&str]], steps: &[Step]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s_side, z_side) = twin_repo::pair(&format!("rth-{format}-{label}"), stock);
    for side in [&s_side, &z_side] {
        if format == "reftable" {
            let out = side.run_in(&side.repo(), &[], &["refs", "migrate", "--ref-format=reftable"]);
            assert_eq!(out.code, 0, "{out:?}");
        }
        for args in setup {
            let out = step(side, stock, args, "", "");
            assert!(out.starts_with("exit=0\n"), "{args:?}: {out}");
        }
        install_hook(side);
    }
    for (n, st) in steps.iter().enumerate() {
        let want = step(&s_side, stock, st.args, st.input, st.fail);
        let got = step(&z_side, &z_side.bin, st.args, st.input, st.fail);
        assert_eq!(got, want, "{format} {label} step {n}: {:?}", st.args);
        let refs = ["for-each-ref", "--format=%(refname) %(objectname) %(symref)"];
        let want = step(&s_side, stock, &refs, "", "");
        let got = step(&z_side, stock, &refs, "", "");
        assert_eq!(got, want, "{format} {label} refs after step {n}: {:?}", st.args);
    }
}

fn both(label: &str, setup: &[&[&str]], steps: &[Step]) {
    for format in ["files", "reftable"] {
        compare(format, label, setup, steps);
    }
}

#[test]
fn update_ref_command_line() {
    both(
        "cmdline",
        &[&["branch", "packed"], &["pack-refs", "--all"], &["branch", "loose"]],
        &[
            s(&["update-ref", "refs/heads/new", "HEAD"]),
            s(&["update-ref", "refs/heads/side", "HEAD", "side"]),
            s(&["update-ref", "refs/heads/side", "HEAD", "HEAD~2"]),
            s(&["update-ref", "-d", "refs/heads/packed"]),
            s(&["update-ref", "-d", "refs/heads/loose", "main"]),
            s(&["update-ref", "HEAD", "HEAD~1"]),
            s(&["update-ref", "--no-deref", "HEAD", "main"]),
            s(&["symbolic-ref", "HEAD", "refs/heads/main"]),
        ],
    );
}

#[test]
fn hook_refusals_abort_and_die() {
    both(
        "refusal",
        &[&["branch", "packed"], &["pack-refs", "--all"]],
        &[
            Step { args: &["update-ref", "refs/heads/n", "HEAD"], input: "", fail: "preparing" },
            Step { args: &["update-ref", "refs/heads/n", "HEAD"], input: "", fail: "prepared" },
            Step { args: &["update-ref", "refs/heads/n", "HEAD"], input: "", fail: "committed" },
            Step { args: &["update-ref", "-d", "refs/heads/packed"], input: "", fail: "prepared" },
            Step { args: &["branch", "b"], input: "", fail: "prepared" },
            Step {
                args: &["update-ref", "--stdin"],
                input: "start\nupdate refs/heads/a HEAD\nprepare\n",
                fail: "prepared",
            },
        ],
    );
}

#[test]
fn update_ref_stdin_transactions() {
    both(
        "stdin",
        &[&["branch", "packed"], &["pack-refs", "--all"], &["branch", "loose"], &["symbolic-ref", "refs/heads/sym", "refs/heads/side"]],
        &[
            Step {
                args: &["update-ref", "--stdin"],
                input: "update refs/heads/a HEAD\ncreate refs/heads/b HEAD\ndelete refs/heads/loose\nverify refs/heads/side side\nverify refs/heads/none 0000000000000000000000000000000000000000\n",
                fail: "",
            },
            Step {
                args: &["update-ref", "--stdin"],
                input: "start\nupdate refs/heads/c HEAD\nprepare\ncommit\nstart\ncreate refs/heads/d HEAD\nprepare\nabort\nstart\ndelete refs/heads/packed\nabort\n",
                fail: "",
            },
            Step { args: &["update-ref", "--stdin"], input: "", fail: "" },
            Step { args: &["update-ref", "--stdin"], input: "start\nupdate refs/heads/e HEAD\n", fail: "" },
            Step { args: &["update-ref", "--stdin"], input: "update refs/heads/sym HEAD~1\ndelete HEAD\n", fail: "" },
            Step {
                args: &["update-ref", "--stdin", "--no-deref"],
                input: "symref-update HEAD refs/heads/a ref refs/heads/main\nsymref-create refs/heads/sc refs/heads/side\nsymref-delete refs/heads/sc refs/heads/side\n",
                fail: "",
            },
        ],
    );
}

#[test]
fn porcelain_ref_writers() {
    both(
        "porcelain",
        &[&["branch", "packed"], &["pack-refs", "--all"], &["branch", "loose"], &["tag", "t1", "HEAD~1"]],
        &[
            s(&["branch", "nb"]),
            s(&["branch", "-D", "packed"]),
            s(&["branch", "-m", "loose", "renamed"]),
            s(&["branch", "-c", "renamed", "copied"]),
            s(&["tag", "-f", "t1", "HEAD"]),
            s(&["tag", "-d", "t1"]),
            s(&["checkout", "-q", "side"]),
            s(&["checkout", "-q", "-b", "nb2"]),
            s(&["switch", "-q", "main"]),
            s(&["cherry-pick", "side"]),
            s(&["reset", "-q", "--soft", "HEAD~1"]),
            s(&["reset", "-q", "--hard", "HEAD~1"]),
            s(&["commit", "-q", "--allow-empty", "-m", "c"]),
            s(&["pack-refs", "--all"]),
            s(&["pack-refs", "--all"]),
        ],
    );
}

#[test]
fn push_updates_each_tracking_ref() {
    let setup: &[&[&str]] = &[&["init", "-q", "--bare", "../bare"], &["remote", "add", "o", "../bare"]];
    both("push", setup, &[s(&["push", "-q", "o", "main", "side"]), s(&["push", "-q", "o", ":side"])]);
}

#[test]
fn no_hook_and_non_executable_hook() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s_side, z_side) = twin_repo::pair("rth-nohook", stock);
    for side in [&s_side, &z_side] {
        install_hook(side);
        let hook = side.repo().join(".git/hooks/reference-transaction");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    for args in [&["branch", "x"][..], &["update-ref", "-d", "refs/heads/x"]] {
        let want = step(&s_side, stock, args, "", "");
        let got = step(&z_side, &z_side.bin, args, "", "");
        assert_eq!(got, want, "{args:?}");
    }
}
