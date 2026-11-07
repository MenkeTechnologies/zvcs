//! `git merge` against stock git on how heads are named, what a merged annotated
//! tag contributes, and the state a refused commit leaves behind.
//!
//! * `merge_name()` (builtin/merge.c:546-635) names a head by the operand as
//!   `copy_branchname()` left it, so `heads/x` and `refs/heads/x` stay as typed,
//!   and an operand naming a tag object by its id is `tag '<id>'`.
//! * `fmt_merge_msg_sigs()` (fmt-merge-msg.c:523-589) appends each merged tag's
//!   body, and `write_merge_heads()` (builtin/merge.c:1123-1147) records the tag
//!   object, not its commit, in MERGE_HEAD.
//! * `abort_commit()` (builtin/merge.c:900-908) prints the notice and rewrites
//!   MERGE_MSG from `merge_msg` for every refusal on the commit path, a vetoing
//!   `pre-merge-commit` hook and an emptied editor buffer included.
//! * `--abort`, `--quit` and `--continue` are independent booleans tested in that
//!   order (builtin/merge.c:1417-1466).
//! * `merge -` is `@{-1}` (:1526-1527); an upstream on the remote `.` is the
//!   local branch itself (`set_merge()`, remote.c:1798-1808).
//! * An operand that peels to a tree or a blob is reported by
//!   `repo_peel_to_type()` (object.c:880-902) ahead of the refusal.
//! * The diffstat counts CR bytes (diff.c:4192-4235), and a rename is never
//!   `(new)` under `--compact-summary` (diff.c:4135-4158).
//! * `-X renormalize` converts without a round-trip check (convert.c:1542-1556).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
    state: String,
}

fn git(bin: &str, dir: &Path, root: &Path, args: &[&str]) -> (i32, String, String) {
    git_env(bin, dir, root, &[], args)
}

fn git_env(bin: &str, dir: &Path, root: &Path, env: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// One fixture built by stock git: `main` and a `side` that merge cleanly, a
/// `conf` that conflicts, an annotated tag `vside` on `side`, a branch `crlf`
/// that only converts `a` to CRLF, and a rename `ren`.
fn fixture(stock: &str, root: &Path) -> PathBuf {
    let work = root.join("w");
    std::fs::create_dir_all(&work).unwrap();
    let g = |args: &[&str]| {
        let (code, _, err) = git(stock, &work, root, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    let write = |rel: &str, body: &str| std::fs::write(work.join(rel), body).unwrap();
    g(&["init", "-q", "-b", "main", "."]);
    write("a", "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
    write("c", "c1\nc2\nc3\nc4\nc5\nc6\n");
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "base"]);
    g(&["tag", "base"]);
    g(&["checkout", "-q", "-b", "side"]);
    write("s", "s\n");
    g(&["add", "s"]);
    g(&["commit", "-q", "-m", "side one"]);
    g(&["tag", "-a", "-m", "side tag subject", "-m", "side tag body", "vside"]);
    g(&["checkout", "-q", "-b", "conf", "base"]);
    write("a", "conf\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
    g(&["commit", "-q", "-am", "conf"]);
    g(&["checkout", "-q", "-b", "crlf", "base"]);
    write("a", "1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9\r\n10\r\n");
    g(&["commit", "-q", "-am", "crlf"]);
    g(&["checkout", "-q", "-b", "ren", "base"]);
    g(&["mv", "c", "c2"]);
    g(&["commit", "-q", "-m", "ren"]);
    g(&["checkout", "-q", "main"]);
    write("a", "main\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
    g(&["commit", "-q", "-am", "main"]);
    work
}

/// Build the fixture under `label` for both binaries, run `pre` with stock and
/// `cmd` with each binary, and return both runs with their post-state.
fn both(label: &str, pre: &[&[&str]], env: &[(&str, &str)], cmd: &[&str]) -> Option<(Run, Run)> {
    let stock = stock_git::stock_git()?;
    let base = std::env::temp_dir().join(format!("zvcs-merge-names-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let run = |bin: &str, side: &str| -> Run {
        let root = base.join(side);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let work = fixture(stock, &root);
        for args in pre {
            // `["sh", script]` is a shell step in the work tree; anything else
            // is a stock git invocation.
            match args {
                ["sh", script] => {
                    Command::new("/bin/sh").args(["-c", script]).current_dir(&work).status().unwrap();
                }
                _ => {
                    git(stock, &work, &root, args);
                }
            }
        }
        let (code, stdout, stderr) = git_env(bin, &work, &root, env, cmd);
        let mut state = String::new();
        for args in [
            &["for-each-ref", "--format=%(refname) %(objectname)"][..],
            &["log", "--all", "--format=%H %P%n%B"],
            &["status", "--porcelain=v2", "--branch"],
            &["ls-files", "-s"],
        ] {
            state.push_str(&git(stock, &work, &root, args).1);
        }
        for f in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "ORIG_HEAD", "logs/HEAD"] {
            let body = std::fs::read(work.join(".git").join(f)).unwrap_or_default();
            state.push_str(&format!("== {f}\n{}\n", String::from_utf8_lossy(&body)));
        }
        Run { code, stdout, stderr, state }
    };
    let s = run(stock, "stock");
    let z = run(ZVCS, "zvcs");
    let _ = std::fs::remove_dir_all(&base);
    Some((s, z))
}

fn assert_same(label: &str, pre: &[&[&str]], env: &[(&str, &str)], cmd: &[&str]) {
    let Some((s, z)) = both(label, pre, env, cmd) else { return };
    assert_eq!(z.stderr, s.stderr, "{cmd:?}: stderr");
    assert_eq!(z.stdout, s.stdout, "{cmd:?}: stdout");
    assert_eq!(z.code, s.code, "{cmd:?}: exit code");
    assert_eq!(z.state, s.state, "{cmd:?}: post-state");
}

#[test]
fn operands_are_named_as_typed() {
    for (label, spec) in [
        ("heads", "heads/side"),
        ("refs-heads", "refs/heads/side"),
        ("refs-tags", "refs/tags/vside"),
        ("prior", "@{-1}"),
    ] {
        assert_same(
            label,
            &[&["checkout", "-q", "side"], &["checkout", "-q", "main"]],
            &[],
            &["merge", "--no-ff", "--log", spec],
        );
    }
}

#[test]
fn a_tag_object_id_is_merged_as_a_tag() {
    let Some(stock) = stock_git::stock_git() else { return };
    let probe = std::env::temp_dir().join(format!("zvcs-merge-names-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&probe);
    std::fs::create_dir_all(&probe).unwrap();
    let work = fixture(stock, &probe);
    let tag = git(stock, &work, &probe, &["rev-parse", "vside"]).1.trim().to_string();
    let _ = std::fs::remove_dir_all(&probe);
    assert_same("tag-id", &[], &[], &["merge", "--no-ff", "--log", &tag]);
}

#[test]
fn a_merged_tag_brings_its_body_and_is_recorded_in_merge_head() {
    assert_same("tag-body", &[], &[], &["merge", "vside"]);
    assert_same("tag-nocommit", &[], &[], &["merge", "--no-commit", "vside"]);
    assert_same("tag-m-log", &[], &[], &["merge", "-m", "hello", "--log", "vside"]);
    assert_same("tag-fetch", &[&["fetch", "-q", ".", "tag", "vside"]], &[], &["merge", "FETCH_HEAD"]);
}

#[test]
fn a_refused_commit_rewrites_merge_msg_from_the_message() {
    let hook: &[&str] = &["config", "core.hooksPath", "hooks"];
    // An editor that empties the buffer, and one that fails.
    assert_same("edit-empty", &[], &[("GIT_EDITOR", ": >")], &["merge", "--edit", "side"]);
    assert_same("edit-fail", &[], &[("GIT_EDITOR", "false")], &["merge", "--signoff", "--edit", "side"]);
    // A vetoing hook, kept out of `.git/hooks` by `core.hooksPath`.
    for name in ["pre-merge-commit", "commit-msg", "prepare-commit-msg"] {
        let script = format!(
            "mkdir -p hooks && printf '#!/bin/sh\\nexit 1\\n' > hooks/{name} && chmod +x hooks/{name}"
        );
        assert_same(&format!("hook-{name}"), &[hook, &["sh", &script]], &[], &["merge", "--log", "side"]);
    }
}

#[test]
fn abort_quit_and_continue_are_tested_in_that_order() {
    let conflicted: &[&[&str]] = &[&["merge", "conf"]];
    assert_same("abort-quit", conflicted, &[], &["merge", "--quit", "--abort"]);
    assert_same("continue-quit", conflicted, &[], &["merge", "--continue", "--quit"]);
    assert_same("abort-noquit", conflicted, &[], &["merge", "--abort", "--no-quit"]);
}

#[test]
fn dash_and_a_local_upstream_name_the_branch() {
    let prior: &[&[&str]] = &[&["checkout", "-q", "side"], &["checkout", "-q", "main"]];
    assert_same("dash", prior, &[], &["merge", "-"]);
    assert_same("dash-squash", prior, &[], &["merge", "--squash", "-"]);
    assert_same("upstream-dot", &[&["branch", "-u", "side"]], &[], &["merge"]);
    assert_same(
        "upstream-dot-tag",
        &[&["config", "branch.main.remote", "."], &["config", "branch.main.merge", "refs/tags/vside"]],
        &[],
        &["merge"],
    );
}

#[test]
fn a_non_commit_operand_is_reported_before_the_refusal() {
    assert_same("blob", &[], &[], &["merge", "side:s"]);
    assert_same("tree", &[], &[], &["merge", "side^{tree}"]);
}

#[test]
fn the_diffstat_counts_line_ending_changes_and_renames_are_not_new() {
    let at_base: &[&[&str]] = &[&["checkout", "-q", "-b", "t", "base"]];
    assert_same("crlf-ff", at_base, &[], &["merge", "crlf"]);
    assert_same("crlf-theirs", &[], &[], &["merge", "-X", "theirs", "crlf"]);
    assert_same("compact-rename", at_base, &[], &["merge", "--compact-summary", "ren"]);
}

#[test]
fn renormalize_does_not_warn_about_round_trips() {
    let attrs: &[&[&str]] = &[&["sh", "printf '* text=auto\\n' > .git/info/attributes"]];
    assert_same("renormalize", attrs, &[], &["merge", "-X", "renormalize", "crlf"]);
}
