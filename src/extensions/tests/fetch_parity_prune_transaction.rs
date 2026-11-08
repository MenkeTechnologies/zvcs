//! `fetch --prune` and `remote prune` as their ref transactions run, seen through the
//! `reference-transaction` hook.
//!
//! * `do_fetch()` prunes before a single object is asked for (builtin/fetch.c:2014-2028):
//!   `prune_refs()` deletes the stale refs in one `refs_delete_refs()` transaction, or under
//!   `--atomic` as the first entries of the transaction the updates join (:1478-1491), and
//!   prints its rows padded to its own summary width (:1493-1501).
//! * `update_local_ref()` queues nothing for a ref already at the remote's value
//!   (:906-911), and `store_updated_refs()` queues the rest one `fetch_head_status` pass at a
//!   time (:1241-1249), so the merge candidate comes first.
//! * An `--atomic` fetch with a refused update aborts its transaction, and the hook sees
//!   `aborted` over what it held (refs.c:2732-2756).
//! * A dry run still begins and commits an empty transaction, and `set_head()` runs as it
//!   does for any fetch; its create-only attempt on an existing `refs/remotes/<r>/HEAD` runs
//!   the hook at `preparing` before the backend refuses it (refs.c:2532-2538).
//! * `--prune-tags`' tag refspec maps every remote tag like any refspec, so `-v` lists the
//!   tags that are up to date and FETCH_HEAD records them.
//! * `remote prune` deletes its stale refs in one transaction (builtin/remote.c:1646-1648).
//! * Both report a symbolic ref the deletions leave dangling after their rows
//!   (`refs_warn_dangling_symrefs()`, refs.c:490-534).
//!
//! Each case builds the same fixture with stock git (an upstream, and a clone holding packed
//! and loose stale tracking refs and a stale tag), runs one command through each binary, and
//! compares the output, the hook log, the refs, the reflogs, `packed-refs`, the loose refs and
//! FETCH_HEAD, on both ref formats.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const HOOK: &str = "#!/bin/sh\n\
    printf '== %s\\n' \"$1\" >>\"$HOOKLOG\"\n\
    cat >>\"$HOOKLOG\"\n\
    exit 0\n";

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, dir: &Path, root: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(["-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
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
        .env("HOOKLOG", root.join("hook.log"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&*root.to_string_lossy(), "<root>");
    (out.status.code().unwrap_or(-1), text(&out.stdout), text(&out.stderr))
}

/// `up` has `main`, `b3`, `b4` and tags `t2`, `t3` (annotated); `b1`, `b2` and `t1` are
/// gone from it, and `main` and `b3` moved on. `cl` cloned it before, packed its refs (files
/// only), and holds `origin/loose` and `origin/b4` as loose refs.
fn fixture(root: &Path, stock: &str, format: &str) {
    let ok = |dir: &Path, args: &[&str]| {
        let (code, _, err) = run(stock, dir, root, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    let up = root.join("up");
    let cl = root.join("cl");
    ok(root, &["init", "-q", "-b", "main", &format!("--ref-format={format}"), "up"]);
    ok(&up, &["commit", "-q", "--allow-empty", "-m", "one"]);
    for b in ["b1", "b2", "b3", "b4"] {
        ok(&up, &["branch", b]);
    }
    ok(&up, &["tag", "t1"]);
    ok(&up, &["tag", "t2"]);
    ok(&up, &["tag", "-a", "-m", "an", "t3"]);
    ok(root, &["clone", "-q", &format!("--ref-format={format}"), "up", "cl"]);
    if format == "files" {
        ok(&cl, &["pack-refs", "--all"]);
    }
    ok(&cl, &["update-ref", "refs/remotes/origin/b4", "HEAD"]);
    ok(&cl, &["update-ref", "refs/remotes/origin/loose", "HEAD"]);
    ok(&up, &["branch", "-q", "-D", "b1", "b2"]);
    ok(&up, &["tag", "-d", "t1"]);
    ok(&up, &["commit", "-q", "--allow-empty", "-m", "two"]);
    ok(&up, &["branch", "-f", "b3", "HEAD"]);
    let hook = cl.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, HOOK).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Make `up`'s `b4` a new root commit, so updating `origin/b4` is not a fast-forward.
fn diverge_b4(root: &Path, stock: &str) {
    let up = root.join("up");
    for args in [
        &["checkout", "-q", "--orphan", "x"][..],
        &["commit", "-q", "--allow-empty", "-m", "root"],
        &["branch", "-f", "b4", "x"],
    ] {
        let (code, _, err) = run(stock, &up, root, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    }
}

/// Run `args` in the clone with `bin`, then read everything back with stock.
fn report(root: &Path, stock: &str, bin: &str, args: &[&str]) -> String {
    let cl = root.join("cl");
    let _ = std::fs::remove_file(root.join("hook.log"));
    let (code, out, err) = run(bin, &cl, root, args);
    let hook = std::fs::read_to_string(root.join("hook.log")).unwrap_or_default();
    let mut r = format!("exit={code}\n--- stdout\n{out}--- stderr\n{err}--- hook\n{hook}");
    let (_, refs, _) = run(stock, &cl, root, &["for-each-ref", "--format=%(refname) %(objectname) %(symref)"]);
    r += &format!("--- refs\n{refs}");
    let (_, logs, _) = run(stock, &cl, root, &["reflog", "list"]);
    for name in logs.lines() {
        let (_, log, _) = run(stock, &cl, root, &["reflog", "show", "--format=%H %gs", name]);
        r += &format!("--- reflog {name}\n{log}");
    }
    let read = |rel: &str| std::fs::read_to_string(cl.join(rel)).unwrap_or_else(|_| "<none>\n".into());
    r += &format!("--- packed-refs\n{}", read(".git/packed-refs"));
    r += &format!("--- FETCH_HEAD\n{}", read(".git/FETCH_HEAD").replace(&*root.to_string_lossy(), "<root>"));
    let mut loose = Vec::new();
    let mut dirs = vec![cl.join(".git/refs")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                loose.push(path.strip_prefix(cl.join(".git")).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    loose.sort();
    r += &format!("--- loose\n{}\n", loose.join("\n"));
    r
}

fn compare(label: &str, prepare: fn(&Path, &str), args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else { return };
    for format in ["files", "reftable"] {
        let mut reports = Vec::new();
        for (side, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = std::env::temp_dir().join(format!(
                "zvcs-fetch-prune-tx-{label}-{format}-{side}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let root = Root(dir.canonicalize().unwrap());
            fixture(&root.0, stock, format);
            prepare(&root.0, stock);
            reports.push(report(&root.0, stock, bin, args));
        }
        assert_eq!(reports[1], reports[0], "{label} on {format}: zvcs (left) vs stock (right)");
    }
}

fn nothing(_: &Path, _: &str) {}

#[test]
fn prune_deletes_packed_and_loose_refs_in_one_transaction_before_the_updates() {
    compare("prune", nothing, &["fetch", "--prune"]);
    compare("fetch-prune-config", nothing, &["-c", "fetch.prune=true", "fetch"]);
    compare("remote-update", nothing, &["remote", "update", "--prune"]);
}

#[test]
fn atomic_prune_joins_the_update_transaction() {
    compare("atomic", nothing, &["fetch", "--atomic", "--prune"]);
}

#[test]
fn prune_tags_lists_and_records_the_tags_that_are_up_to_date() {
    compare("prune-tags", nothing, &["fetch", "--prune", "--prune-tags", "-v"]);
    compare("prune-tags-atomic", nothing, &["fetch", "--atomic", "--prune", "--prune-tags"]);
}

#[test]
fn a_refused_update_aborts_the_atomic_transaction_deletions_included() {
    compare("atomic-refused", diverge_b4, &["fetch", "--atomic", "--prune", "origin", "refs/heads/*:refs/remotes/origin/*"]);
    compare("refused", diverge_b4, &["fetch", "--prune", "origin", "refs/heads/*:refs/remotes/origin/*"]);
}

#[test]
fn a_dry_run_commits_an_empty_transaction_and_still_sets_the_remote_head() {
    compare("dry-run", nothing, &["fetch", "--prune", "--dry-run"]);
    compare(
        "dry-run-set-head",
        |root, stock| {
            let (code, _, err) = run(stock, &root.join("cl"), root, &["remote", "set-head", "origin", "-d"]);
            assert_eq!(code, 0, "{err}");
        },
        &["fetch", "--dry-run"],
    );
}

#[test]
fn remote_prune_deletes_in_one_transaction() {
    compare("remote-prune", nothing, &["remote", "prune", "origin"]);
}

/// Point `origin/HEAD` at the stale `origin/b1`.
fn head_on_stale(root: &Path, stock: &str) {
    let (code, _, err) =
        run(stock, &root.join("cl"), root, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/b1"]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn a_symref_left_dangling_is_reported_after_the_pruned_rows() {
    compare("dangling", head_on_stale, &["fetch", "--prune"]);
    compare("dangling-dry-run", head_on_stale, &["fetch", "--prune", "--dry-run"]);
    compare("dangling-remote", head_on_stale, &["remote", "prune", "origin"]);
    compare("dangling-remote-dry-run", head_on_stale, &["remote", "prune", "--dry-run", "origin"]);
}
