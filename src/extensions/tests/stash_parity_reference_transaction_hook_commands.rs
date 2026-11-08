//! The `reference-transaction` hook as the porcelain's own transactions run it.
//!
//! Every ref a command writes goes through a transaction of its own shape, and the hook
//! sees each one (refs.c:2663-2786). These steps are the commands whose transactions
//! differed from stock's:
//!
//! * `stash push` runs `reset --hard`, whose `reset_refs()` moves `ORIG_HEAD` and then
//!   `HEAD` to itself with the old values it read (builtin/reset.c:308-322), and `stash
//!   drop`/`clear` delete `refs/stash` with the value `repo_get_oid()` read
//!   (builtin/stash.c:307-315);
//! * `merge --abort` runs `reset --merge` the same way, then `remove_branch_state()`
//!   (branch.c:863-879), and the autostash ref is deleted with its old value
//!   (sequencer.c:4939-4940);
//! * a rebase's checkout of `<onto>` passes the `HEAD` it read (reset.c:65-68), each pick
//!   writes `CHERRY_PICK_HEAD` before it commits and deletes it once the commit lands
//!   (sequencer.c:2500-2511, :1741-1744), the commit passes the `HEAD` it was made on
//!   (sequencer.c:1261-1297), and the apply backend's branch update carries no old value
//!   (builtin/rebase.c:592-619);
//! * a revert that commits deletes `CHERRY_PICK_HEAD`, not `REVERT_HEAD`
//!   (sequencer.c:1741-1744);
//! * `worktree add` writes the new `HEAD` through the worktree's ref store
//!   (builtin/worktree.c:556-563) and its `reset --hard` child runs `reset_refs()` and
//!   `remove_branch_state()` there;
//! * `bisect` writes `refs/bisect/*` and `BISECT_EXPECTED_REV` as ref transactions in the
//!   files backend too.
//!
//! Each step runs through both binaries with a hook that appends its phase and stdin to a
//! log; the step's output, the log, the refs and the `HEAD` reflog stock reads afterwards
//! must agree, on both ref formats.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Stdio};
use twin_repo::Side;

const HOOK: &str = "#!/bin/sh\n\
    printf '== %s\\n' \"$1\" >>\"$HOOKLOG\"\n\
    cat >>\"$HOOKLOG\"\n\
    exit 0\n";

fn install_hook(side: &Side) {
    let path = side.repo().join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, HOOK).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Run `args` in `side`'s repository with `bin` and return the exit code, stdout, stderr
/// and what the hook logged.
fn step(side: &Side, bin: &str, args: &[&str]) -> String {
    let log = side.root.join("hook.log");
    let _ = std::fs::remove_file(&log);
    let out = Command::new(bin)
        .args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("HOOKLOG", &log)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
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

/// One command: the files to write into the work tree first, then its arguments, and
/// whether its hook log is compared.
struct Step {
    write: &'static [(&'static str, &'static str)],
    args: &'static [&'static str],
    hook: bool,
}

const fn s(args: &'static [&'static str]) -> Step {
    Step { write: &[], args, hook: true }
}

/// A step whose output and resulting refs are compared but whose hook log is not.
const fn unhooked(args: &'static [&'static str]) -> Step {
    Step { write: &[], args, hook: false }
}

/// [`step`]'s report without the hook log.
fn without_hook(report: String) -> String {
    match report.find("--- hook\n") {
        Some(at) => report[..at].to_owned(),
        None => report,
    }
}

/// What stock reads back after a step, with stock: the refs, the `HEAD` reflog, and the
/// reflog of every worktree's `HEAD` named in `worktrees`.
fn state(side: &Side, stock: &str, worktrees: &[&str]) -> String {
    let mut out = step(side, stock, &["for-each-ref", "--format=%(refname) %(objectname) %(symref)"]);
    out += &step(side, stock, &["reflog", "show", "--format=%gD %H %gs", "HEAD"]);
    for wt in worktrees {
        let dir = format!("../{wt}");
        if side.repo().join(&dir).exists() {
            out += &step(side, stock, &["-C", &dir, "reflog", "show", "--format=%gD %H %gs", "HEAD"]);
            out += &step(side, stock, &["-C", &dir, "rev-parse", "-q", "--verify", "ORIG_HEAD"]);
        }
    }
    out
}

fn compare(format: &str, label: &str, setup: &[Step], steps: &[Step], worktrees: &[&str]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s_side, z_side) = twin_repo::pair(&format!("rthc-{format}-{label}"), stock);
    for side in [&s_side, &z_side] {
        if format == "reftable" {
            let out = side.run_in(&side.repo(), &[], &["refs", "migrate", "--ref-format=reftable"]);
            assert_eq!(out.code, 0, "{out:?}");
        }
        for st in setup {
            for (path, body) in st.write {
                side.write(path, body);
            }
            let out = step(side, stock, st.args);
            assert!(out.starts_with("exit=0\n"), "{:?}: {out}", st.args);
        }
        install_hook(side);
    }
    for (n, st) in steps.iter().enumerate() {
        for side in [&s_side, &z_side] {
            for (path, body) in st.write {
                side.write(path, body);
            }
        }
        let mut want = step(&s_side, stock, st.args);
        let mut got = step(&z_side, &z_side.bin, st.args);
        if !st.hook {
            (want, got) = (without_hook(want), without_hook(got));
        }
        assert_eq!(got, want, "{format} {label} step {n}: {:?}", st.args);
        let want = state(&s_side, stock, worktrees);
        let got = state(&z_side, stock, worktrees);
        assert_eq!(got, want, "{format} {label} refs after step {n}: {:?}", st.args);
    }
}

fn both(label: &str, setup: &[Step], steps: &[Step], worktrees: &[&str]) {
    for format in ["files", "reftable"] {
        compare(format, label, setup, steps, worktrees);
    }
}

/// A second tracked file, so that a dirty change can be stashed beside `a`'s conflicts.
const ADD_B: &[Step] = &[
    Step { write: &[("b", "b\n")], args: &["add", "b"], hook: true },
    Step { write: &[], args: &["commit", "-q", "-m", "b"], hook: true },
];

#[test]
fn stash_push_drop_and_clear() {
    both(
        "stash",
        ADD_B,
        &[
            Step { write: &[("b", "x\n")], args: &["stash", "-q"], hook: true },
            Step { write: &[("b", "y\n")], args: &["stash", "-q"], hook: true },
            s(&["stash", "pop", "-q"]),
            s(&["stash", "drop", "-q"]),
            Step { write: &[("b", "z\n")], args: &["stash", "-q"], hook: true },
            s(&["stash", "clear"]),
            s(&["checkout", "-q", "--detach"]),
            Step { write: &[("b", "w\n")], args: &["stash", "-q"], hook: true },
            s(&["stash", "drop", "-q"]),
        ],
        &[],
    );
}

#[test]
fn merge_autostash_abort_reset_and_commit() {
    both(
        "merge-autostash",
        ADD_B,
        &[
            Step { write: &[("b", "x\n")], args: &["merge", "-q", "--autostash", "side", "-m", "m"], hook: true },
            s(&["merge", "--abort"]),
            Step { write: &[("b", "y\n")], args: &["merge", "-q", "--autostash", "side", "-m", "m"], hook: true },
            s(&["reset", "-q", "--hard"]),
            Step { write: &[("b", "z\n")], args: &["merge", "-q", "--autostash", "side", "-m", "m"], hook: true },
            Step { write: &[("a", "1\n2\n3\n")], args: &["add", "a"], hook: true },
            s(&["commit", "-q", "-m", "done"]),
        ],
        &[],
    );
}

#[test]
fn rebase_picks_and_branch_updates() {
    let setup: &[Step] = &[
        s(&["checkout", "-q", "-b", "topic", "main~1"]),
        Step { write: &[("t", "t\n")], args: &["add", "t"], hook: true },
        s(&["commit", "-q", "-m", "t"]),
        Step { write: &[("u", "u\n")], args: &["add", "u"], hook: true },
        s(&["commit", "-q", "-m", "u"]),
        s(&["tag", "orig"]),
    ];
    both(
        "rebase",
        setup,
        &[
            s(&["rebase", "-q", "main"]),
            s(&["reset", "-q", "--hard", "orig"]),
            s(&["rebase", "-q", "--apply", "main"]),
            s(&["reset", "-q", "--hard", "orig"]),
            s(&["rebase", "-q", "-i", "--onto", "side", "main~1"]),
            s(&["reset", "-q", "--hard", "orig"]),
            s(&["rebase", "-q", "-x", "true", "main"]),
        ],
        &[],
    );
}

#[test]
fn revert_commits_and_stops() {
    both(
        "revert",
        &[],
        &[
            s(&["revert", "--no-edit", "HEAD"]),
            s(&["revert", "-n", "HEAD"]),
            s(&["revert", "--abort"]),
            s(&["revert", "--no-edit", "HEAD~2"]),
            s(&["revert", "--abort"]),
        ],
        &[],
    );
}

#[test]
fn worktree_add_ref_stores() {
    both(
        "worktree",
        &[],
        &[
            s(&["worktree", "add", "-q", "../wt"]),
            s(&["worktree", "add", "-q", "--detach", "../wt2"]),
            s(&["worktree", "add", "-q", "-b", "nb", "../wt3", "HEAD~1"]),
            s(&["worktree", "add", "-q", "--no-checkout", "../wt4"]),
            s(&["-c", "core.logAllRefUpdates=always", "worktree", "add", "-q", "../wt5"]),
            s(&["worktree", "add", "-q", "--orphan", "-b", "orph", "../wt6"]),
            s(&["worktree", "remove", "../wt"]),
        ],
        &["wt", "wt2", "wt3", "wt4", "wt5", "wt6"],
    );
}

#[test]
fn bisect_marks_and_reset() {
    both(
        "bisect",
        &[],
        // `bisect_clean_state()`'s deletion of the session's refs (bisect.c:1194-1210) is
        // not this test's: `start` and `reset` are compared without their hook logs.
        &[
            unhooked(&["bisect", "start"]),
            s(&["bisect", "bad"]),
            s(&["bisect", "good", "main~2"]),
            s(&["bisect", "skip"]),
            unhooked(&["bisect", "reset"]),
            unhooked(&["-c", "core.logAllRefUpdates=always", "bisect", "start", "--no-checkout", "main"]),
            s(&["-c", "core.logAllRefUpdates=always", "bisect", "good", "main~2"]),
            unhooked(&["bisect", "reset"]),
        ],
        &[],
    );
}
