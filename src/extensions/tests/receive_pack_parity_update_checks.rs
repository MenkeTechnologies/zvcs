//! `receive-pack`'s `update()` checks (builtin/receive-pack.c:1477-1660) and the
//! phases `execute_commands_non_atomic()` runs them in (:1886-1966):
//!
//! * a tag object written to `refs/heads/*` is `ref_transaction_update()`'s
//!   `trying to write non-commit object <oid> to branch '<ref>'`, reported as
//!   `invalid new value provided` (refs.c:1433-1448), and under `--atomic` the
//!   rest of the push fails with it;
//! * a delete may name a one-level ref (`REFNAME_ALLOW_ONELEVEL`), so
//!   `:refs/foo` is `deleting a non-existent ref`, not a funny refname;
//! * `receive.denyNonFastForwards` over a non-commit is `bad sha1 objects` on
//!   receive-pack's own stderr and `bad ref`;
//! * deletions are processed in a phase of their own ahead of the other
//!   updates, which the `update` hook's order shows;
//! * `find_shared_symref()` walks every worktree, so a branch a linked worktree
//!   has checked out, or one a detached worktree is rebasing, is the current
//!   branch, and `updateInstead` updates the linked worktree.
//!
//! Every case pushes with stock `git push` and swaps only the receiving end
//! (`--receive-pack`), into identical fixtures built at the same path. Stock
//! git is the oracle (`support/stock_git.rs`).

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
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn ok(bin: &str, cwd: &Path, root: &Path, args: &[&str]) {
    let out = git(bin, cwd, root, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// Build `src` (commits `one` and `two` on `main`, branch `other` and annotated
/// tag `lt` at `one`) and its clone `dst` — bare unless `nonbare` — then run
/// `setup` in `dst`, push `refspecs` from `src` into `dst` served by `server`,
/// and report the push's exit code and output with the state `dst` is left in.
fn push(
    stock: &str,
    root: &Path,
    server: &str,
    nonbare: bool,
    setup: &[&[&str]],
    push_args: &[&str],
) -> String {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    let src = root.join("src");
    ok(stock, root, root, &["init", "-q", "-b", "main", "src"]);
    std::fs::write(src.join("f"), "a\n").unwrap();
    ok(stock, &src, root, &["add", "f"]);
    ok(stock, &src, root, &["commit", "-q", "-m", "one"]);
    ok(stock, &src, root, &["branch", "other"]);
    ok(stock, &src, root, &["tag", "-a", "-m", "t", "lt"]);
    let mut clone = vec!["clone", "-q", "--no-local"];
    if !nonbare {
        clone.push("--bare");
    }
    clone.extend(["src", "dst"]);
    ok(stock, root, root, &clone);
    std::fs::write(src.join("f"), "a\nb\n").unwrap();
    ok(stock, &src, root, &["commit", "-q", "-am", "two"]);
    let dst = root.join("dst");
    for args in setup {
        ok(stock, &dst, root, args);
    }
    let hooks = if nonbare { dst.join(".git/hooks") } else { dst.join("hooks") };
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("update");
    std::fs::write(&hook, "#!/bin/sh\necho update \"$@\" >&2\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

    let receive_pack = format!("--receive-pack={server} receive-pack");
    let mut args = vec!["push", receive_pack.as_str(), "../dst"];
    args.extend(push_args);
    let out = git(stock, &src, root, &args);
    let refs = git(stock, &dst, root, &["for-each-ref"]);
    let status = git(stock, &dst, root, &["status", "--porcelain"]);
    let fsck = git(stock, &dst, root, &["fsck"]);
    let mut worktrees = String::new();
    for wt in ["wt", "wt2"] {
        if let Ok(text) = std::fs::read_to_string(root.join(wt).join("f")) {
            worktrees += &format!("{wt}/f: {text}");
        }
    }
    format!(
        "exit {:?}\n--- stdout\n{}--- stderr\n{}--- refs\n{}--- status\n{}--- fsck\n{}{}--- worktrees\n{worktrees}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&refs.stdout),
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&fsck.stdout),
        String::from_utf8_lossy(&fsck.stderr),
    )
}

fn compare(name: &str, nonbare: bool, setup: &[&[&str]], push_args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-receive-update-checks-{name}-{}", std::process::id()));
    let fx = Fixture { root: base };
    let want = push(stock, &fx.root, stock, nonbare, setup, push_args);
    let got = push(stock, &fx.root, ZVCS, nonbare, setup, push_args);
    assert_eq!(got, want, "{name}");
}

#[test]
fn a_tag_written_to_a_branch_is_an_invalid_new_value() {
    compare("tag", false, &[], &["refs/tags/*:refs/heads/t/*", "main:refs/heads/x"]);
}

#[test]
fn a_tag_written_to_a_branch_fails_an_atomic_push() {
    compare("tag-atomic", false, &[], &["--atomic", "refs/tags/*:refs/heads/t/*", "main:refs/heads/x"]);
}

#[test]
fn a_tree_written_to_a_branch_is_an_invalid_new_value() {
    compare("tree", false, &[], &["main^{tree}:refs/heads/tree"]);
}

#[test]
fn a_one_level_ref_may_be_deleted_but_not_written() {
    compare("onelevel", false, &[], &["main:refs/foo", ":refs/foo"]);
}

#[test]
fn deny_non_fast_forwards_over_a_tag_is_a_bad_ref() {
    compare(
        "nff-tag",
        false,
        &[&["config", "receive.denyNonFastForwards", "true"]],
        &["-f", "lt:refs/heads/other"],
    );
}

#[test]
fn deletions_run_before_the_other_updates() {
    compare("phases", false, &[], &["main", "main:refs/heads/new", ":other"]);
}

#[test]
fn a_branch_checked_out_in_a_linked_worktree_is_the_current_branch() {
    compare(
        "linked",
        true,
        &[&["worktree", "add", "-q", "../wt", "other"], &["checkout", "-q", "--detach"]],
        &["-f", "main:other", ":other"],
    );
}

#[test]
fn update_instead_updates_a_linked_worktree() {
    compare(
        "linked-update-instead",
        true,
        &[
            &["worktree", "add", "-q", "../wt2", "other"],
            &["config", "receive.denyCurrentBranch", "updateInstead"],
        ],
        &["-f", "main:other"],
    );
}

#[test]
fn a_branch_a_detached_worktree_is_rebasing_is_the_current_branch() {
    let Some(stock) = stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-receive-update-checks-rebasing-{}", std::process::id()));
    let fx = Fixture { root: base };
    let mut results = Vec::new();
    for server in [stock, ZVCS] {
        // The rebase state is planted after the clone, by hand: `push()` builds
        // the fixture, so run it once with a no-op push to get `dst`, plant the
        // state, then push for real.
        push(stock, &fx.root, server, true, &[&["checkout", "-q", "--detach"]], &["--dry-run", "main"]);
        let state = fx.root.join("dst/.git/rebase-merge");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("head-name"), "refs/heads/other\n").unwrap();
        let out = git(
            stock,
            &fx.root.join("src"),
            &fx.root,
            &["push", &format!("--receive-pack={server} receive-pack"), "../dst", "-f", "main:other"],
        );
        let refs = git(stock, &fx.root.join("dst"), &fx.root, &["for-each-ref"]);
        results.push(format!(
            "{:?}\n{}{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&refs.stdout)
        ));
    }
    assert!(results[0].contains("branch is currently checked out"), "{}", results[0]);
    assert_eq!(results[1], results[0]);
}
