//! `execute_commands_non_atomic()` (builtin/receive-pack.c:1868-1966) runs each
//! phase, deletions first, as one ref transaction that may fail partially,
//! `REF_TRANSACTION_ALLOW_FAILURE`:
//!
//! * the `reference-transaction` hook sees a phase's updates together, refused
//!   ones included (`transaction_hook_feed_stdin()`, refs.c:2602-2640), and runs
//!   with no lines for a phase whose every update `update()` refused;
//! * an update refused by the backend (`ref_transaction_maybe_set_rejected()`,
//!   refs.c:1276-1312) is reported alone and the rest are written: a stale old
//!   value, a ref created meanwhile, one deleted meanwhile, and a D/F conflict
//!   with a loose ref, a directory of refs or a packed ref
//!   (`lock_raw_ref()`, `refs_verify_refnames_available()`);
//! * a refusal is keyed by the name of the ref refused, which for an update
//!   through a symref is its referent, so the command naming the symref is
//!   reported `ok` and the symref and `HEAD` still get their log-only entries;
//! * `--atomic` runs its transaction, so the hook sees it at `preparing`;
//! * deleting the last packed ref leaves `packed-refs` with its header only
//!   (`write_with_updates()`, refs/packed-backend.c).
//!
//! The ref that goes stale is moved by the `update` hook, which runs inside
//! `update()`, after the client read the advertisement and before the
//! transaction is committed. Every case pushes with stock `git push` and swaps
//! only the receiving end (`--receive-pack`), into identical fixtures built at
//! the same path, on the files and the reftable backend. Stock git is the oracle
//! (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(["-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn ok(bin: &str, cwd: &Path, root: &Path, args: &[&str]) {
    let out = git(bin, cwd, root, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn write_hook(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Build `bare.git` (format `format`) holding `main`, `side` and `old` at commit
/// `one`, `other` at `three` and the symref `alias` -> `main`, and the client
/// `work` at `two`; run `setup` in `bare.git`, install a `reference-transaction`
/// hook that logs every phase and an `update` hook running `update`, then push
/// `push_args` from `work` with `server` as receive-pack. Report the push and
/// the state it leaves.
fn push(stock: &str, root: &Path, server: &str, format: &str, setup: &[&[&str]], update: &str, push_args: &[&str]) -> String {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    let bare = root.join("bare.git");
    let work = root.join("work");
    let ref_format = format!("--ref-format={format}");
    ok(stock, root, root, &["init", "-q", "--bare", "-b", "main", &ref_format, "bare.git"]);
    ok(stock, root, root, &["init", "-q", "-b", "main", &ref_format, "work"]);
    std::fs::write(work.join("f"), "a\n").unwrap();
    ok(stock, &work, root, &["add", "f"]);
    ok(stock, &work, root, &["commit", "-q", "-m", "one"]);
    ok(stock, &work, root, &["push", "-q", "../bare.git", "main", "main:side", "main:old"]);
    std::fs::write(work.join("f"), "b\n").unwrap();
    ok(stock, &work, root, &["commit", "-q", "-am", "two"]);
    ok(stock, &work, root, &["branch", "-f", "side"]);
    std::fs::write(work.join("f"), "c\n").unwrap();
    ok(stock, &work, root, &["commit", "-q", "-am", "three"]);
    ok(stock, &work, root, &["push", "-q", "../bare.git", "HEAD:refs/heads/other"]);
    ok(stock, &work, root, &["reset", "-q", "--hard", "HEAD~1"]);
    ok(stock, &bare, root, &["symbolic-ref", "refs/heads/alias", "refs/heads/main"]);
    ok(stock, &bare, root, &["config", "core.logAllRefUpdates", "true"]);
    for args in setup {
        ok(stock, &bare, root, args);
    }
    let log = root.join("hook.log");
    write_hook(
        &bare.join("hooks/reference-transaction"),
        &format!("echo \"== $1\" >> '{}'\ncat >> '{}'\n", log.display(), log.display()),
    );
    // The hook's own ref writes run stock git with no hooks, so they log nothing.
    let quiet = format!("{stock} -c core.hooksPath=/nonexistent");
    write_hook(&bare.join("hooks/update"), &format!("GIT=\"{quiet}\"\n{update}\nexit 0\n"));

    let receive_pack = format!("--receive-pack={server} receive-pack");
    let mut args = vec!["push", receive_pack.as_str(), "../bare.git"];
    args.extend(push_args);
    let out = git(stock, &work, root, &args);
    let refs = git(stock, &bare, root, &["for-each-ref", "--format=%(refname) %(objectname) %(symref)"]);
    let fsck = git(stock, &bare, root, &["fsck"]);
    let mut logs = String::new();
    for r in ["HEAD", "refs/heads/main", "refs/heads/alias", "refs/heads/side", "refs/heads/old"] {
        let reflog = git(stock, &bare, root, &["reflog", "show", "--format=%H %gs", r, "--"]);
        logs += &format!("{r}:\n{}{}", String::from_utf8_lossy(&reflog.stdout), String::from_utf8_lossy(&reflog.stderr));
        if let Ok(raw) = std::fs::read_to_string(bare.join("logs").join(r)) {
            logs += &format!("raw:\n{raw}");
        }
    }
    format!(
        "exit {:?}\n--- stdout\n{}--- stderr\n{}--- hook\n{}--- refs\n{}--- packed-refs\n{:?}\n--- logs\n{logs}--- fsck\n{}{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(&log).unwrap_or_default(),
        String::from_utf8_lossy(&refs.stdout),
        std::fs::read(bare.join("packed-refs")).ok().map(|b| String::from_utf8_lossy(&b).into_owned()),
        String::from_utf8_lossy(&fsck.stdout),
        String::from_utf8_lossy(&fsck.stderr),
    )
}

fn compare(name: &str, setup: &[&[&str]], update: &str, push_args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    for format in ["files", "reftable"] {
        let base = std::env::temp_dir().join(format!("zvcs-receive-allow-failure-{name}-{format}-{}", std::process::id()));
        let fx = Fixture { root: base };
        let want = push(stock, &fx.root, stock, format, setup, update, push_args);
        let got = push(stock, &fx.root, ZVCS, format, setup, update, push_args);
        assert_eq!(got, want, "{name} ({format})");
    }
}

/// Moves `main` on to `other` when the update named in `$1` is checked.
fn race_main(names: &str) -> String {
    format!("case \"$1\" in {names}) $GIT update-ref -m race refs/heads/main $($GIT rev-parse refs/heads/other) ;; esac")
}

#[test]
fn a_phase_is_one_transaction() {
    compare("one-transaction", &[], "", &["main", "side"]);
    compare("deletions-together", &[], "", &[":old", ":side", "main"]);
}

#[test]
fn a_phase_whose_updates_were_all_declined_still_runs_the_hook() {
    compare("declined", &[], "[ \"$1\" = refs/heads/side ] && exit 1", &["main", "side"]);
    compare("declined-all", &[], "exit 1", &["main", "side", ":old"]);
}

#[test]
fn a_stale_old_value_through_a_symref_is_keyed_by_its_referent() {
    compare("alias-update", &[], &race_main("refs/heads/alias"), &["HEAD:refs/heads/alias"]);
    compare("alias-delete", &[], &race_main("refs/heads/alias"), &[":refs/heads/alias"]);
    compare(
        "alias-delete-packed",
        &[&["pack-refs", "--all"]],
        &race_main("refs/heads/alias"),
        &[":refs/heads/alias", "side"],
    );
}

#[test]
fn a_refused_update_leaves_the_rest_of_its_phase() {
    compare("stale-main", &[], &race_main("refs/heads/main"), &["HEAD:refs/heads/main", "side", ":old"]);
    compare(
        "created-meanwhile",
        &[],
        "[ \"$1\" = refs/heads/newb ] && $GIT update-ref refs/heads/newb refs/heads/other",
        &["HEAD:refs/heads/newb", "side"],
    );
    compare(
        "deleted-meanwhile",
        &[],
        "[ \"$1\" = refs/heads/side ] && $GIT update-ref -d refs/heads/side",
        &["main", "side", ":old"],
    );
    compare(
        "deleted-meanwhile-delete",
        &[],
        "[ \"$1\" = refs/heads/old ] && $GIT update-ref -d refs/heads/old",
        &[":old", ":side", "main"],
    );
}

#[test]
fn a_name_conflict_refuses_that_update_alone() {
    compare("loose-file", &[], "", &["HEAD:refs/heads/side/x", "HEAD:refs/heads/new"]);
    compare("packed-file", &[&["pack-refs", "--all"]], "", &["HEAD:refs/heads/side/x", "HEAD:refs/heads/new", ":old"]);
    compare("deleted-first", &[], "", &["HEAD:refs/heads/old/x", ":old", "HEAD:refs/heads/side/y"]);
    compare(
        "loose-directory",
        &[&["update-ref", "refs/heads/deep/x", "refs/heads/side"]],
        "",
        &["HEAD:refs/heads/deep", "HEAD:refs/heads/new"],
    );
    compare(
        "packed-directory",
        &[&["update-ref", "refs/heads/deep/x", "refs/heads/side"], &["pack-refs", "--all"]],
        "",
        &["HEAD:refs/heads/deep", "HEAD:refs/heads/new"],
    );
    compare("same-push", &[], "", &["HEAD:refs/heads/x", "HEAD:refs/heads/x/y"]);
}

#[test]
fn atomic_runs_its_transaction() {
    compare("atomic", &[], &race_main("refs/heads/alias"), &["--atomic", "HEAD:refs/heads/alias", "side"]);
}

#[test]
fn deleting_every_packed_ref_keeps_the_packed_refs_header() {
    compare(
        "delete-packed",
        &[&["pack-refs", "--all"], &["symbolic-ref", "HEAD", "refs/heads/none"], &["symbolic-ref", "-d", "refs/heads/alias"], &["update-ref", "-d", "refs/heads/other"]],
        "",
        &[":old", ":side", ":main"],
    );
}
