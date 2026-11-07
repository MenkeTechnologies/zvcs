//! `git am --signoff`: the message bytes it signs, the trailer it re-adds, and
//! a `--signoff` typed on the command that resumes a session.
//!
//! * `am_append_signoff()` (builtin/am.c:1189-1196) attaches the raw message to
//!   a strbuf, so a Latin-1 body keeps its bytes in `rebase-apply/final-commit`.
//!   zvcs decoded it lossily and wrote U+FFFD.
//! * Its `append_signoff(&sb, 0, 0)` passes no APPEND_SIGNOFF_DEDUP, so a
//!   matching sign-off that is not the last trailer is added again. zvcs
//!   de-duplicated it.
//! * `if (state.signoff == SIGNOFF_EXPLICIT) am_append_signoff(&state);`
//!   (builtin/am.c:2489-2490) signs the resumed patch's message, so
//!   `am --continue --signoff` commits it signed; `am_resolve()` and `am_skip()`
//!   then `am_load()` the session again (:1987-1989, :2143-2145), so the later
//!   patches follow the `sign` file. zvcs ignored the resuming `--signoff`.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in identical
//! repositories.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

type Out = (Vec<u8>, Vec<u8>, i32);

fn git(bin: &str, dir: &Path, args: &[&str]) -> Out {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
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
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (out.stdout, out.stderr, out.status.code().expect("no signal"))
}

/// `a.txt` is `line 1` .. `line 30`, committed on `main`.
fn repo(label: &str, stock: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-am-signoff-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(&work).unwrap();
    git(stock, &work, &["init", "-q", "-b", "main", "."]);
    let a: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    std::fs::write(work.join("a.txt"), a).unwrap();
    git(stock, &work, &["add", "."]);
    git(stock, &work, &["commit", "-q", "-m", "initial"]);
    work
}

/// A mail changing `old` to `new` in `a.txt`, with `msg` as subject and body.
fn mail(msg: &[u8], old: &str, new: &str, at: usize) -> Vec<u8> {
    let mut m = b"From 0000000000000000000000000000000000000000 Mon Sep 17 00:00:00 2001\n\
From: P <p@example.com>\nDate: Tue, 14 Nov 2023 22:13:20 +0000\nSubject: "
        .to_vec();
    m.extend_from_slice(msg);
    let ctx = |i: usize| format!(" line {i}\n");
    m.extend_from_slice(
        format!(
            "\n---\ndiff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -{},7 +{},7 @@\n{}{}{}-{old}\n+{new}\n{}{}{}",
            at - 3,
            at - 3,
            ctx(at - 3),
            ctx(at - 2),
            ctx(at - 1),
            ctx(at + 1),
            ctx(at + 2),
            ctx(at + 3)
        )
        .as_bytes(),
    );
    m
}

enum Step<'a> {
    /// A git command; `am` runs under the binary being tested, the rest under stock.
    Git(&'a [&'a str]),
    /// Overwrite a worktree file.
    Write(&'a str, &'a str),
}

/// Run `steps` in twin repositories with `mbox` at `../in.mbox`; compare every
/// command's output, then `log`, the refs, the status and `final-commit`.
fn same(label: &str, mbox: &[u8], steps: &[Step]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let s = repo(&format!("{label}-stock"), stock);
    let z = repo(&format!("{label}-zvcs"), stock);
    for dir in [&s, &z] {
        std::fs::write(dir.parent().unwrap().join("in.mbox"), mbox).unwrap();
    }
    for step in steps {
        match step {
            Step::Write(path, content) => {
                for dir in [&s, &z] {
                    std::fs::write(dir.join(path), content).unwrap();
                }
            }
            Step::Git(args) => {
                let args: Vec<&str> = args.iter().map(|a| if *a == "MBOX" { "../in.mbox" } else { a }).collect();
                let tested = if args[0] == "am" { ZVCS } else { stock };
                let want = git(stock, &s, &args);
                let got = git(tested, &z, &args);
                assert_eq!(got, want, "git {args:?}: left is zvcs, right is stock");
            }
        }
    }
    for probe in [&["log", "--format=%H%n%B", "-3"][..], &["for-each-ref"], &["status", "--porcelain"]] {
        assert_eq!(git(stock, &z, probe), git(stock, &s, probe), "{probe:?} (left is zvcs)");
    }
    let fc = |d: &Path| std::fs::read(d.join(".git/rebase-apply/final-commit")).ok();
    assert_eq!(fc(&z), fc(&s), "final-commit (left is zvcs)");
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
}

#[test]
fn a_latin1_body_keeps_its_bytes_when_signed() {
    // No patch, so am stops with the signed message in final-commit.
    let latin = b"From 1 Mon Sep 17 00:00:00 2001\nFrom: \xe9mile <e@example.com>\n\
Subject: caf\xe9\nDate: Tue, 14 Nov 2023 22:13:20 +0000\n\nlatin \xe9 body\n";
    same("latin", latin, &[Step::Git(&["am", "--signoff", "MBOX"])]);
    same("latin-k", latin, &[Step::Git(&["am", "-s", "-k", "MBOX"])]);
}

#[test]
fn a_sign_off_that_is_not_the_last_trailer_is_added_again() {
    let msg = b"change five\n\nbody\n\nSigned-off-by: C <c@example.com>\nAcked-by: Y <y@example.com>\n";
    same("dup", &mail(msg, "line 5", "line five", 5), &[Step::Git(&["am", "--signoff", "MBOX"])]);
    let msg = b"change five\n\nbody\n\nSigned-off-by: C <c@example.com>\n";
    same("last", &mail(msg, "line 5", "line five", 5), &[Step::Git(&["am", "--signoff", "MBOX"])]);
}

/// Two mails: the first no longer matches `line 3`, the second applies.
fn conflicting_series() -> Vec<u8> {
    let first = String::from_utf8(mail(b"side one\n", "line 4", "line FOUR", 4))
        .unwrap()
        .replace(" line 3\n", " line THREE\n");
    let mut mbox = first.into_bytes();
    mbox.push(b'\n');
    mbox.extend_from_slice(&mail(b"side two\n", "line 20", "line twenty", 20));
    mbox
}

fn resolved() -> String {
    (1..=30).map(|i| if i == 4 { "line FOUR\n".to_string() } else { format!("line {i}\n") }).collect()
}

#[test]
fn continue_with_signoff_signs_only_the_resolved_patch() {
    let a = resolved();
    same(
        "continue",
        &conflicting_series(),
        &[
            Step::Git(&["am", "MBOX"]),
            Step::Write("a.txt", &a),
            Step::Git(&["add", "a.txt"]),
            Step::Git(&["am", "--continue", "--signoff"]),
        ],
    );
    same(
        "resolved-s",
        &conflicting_series(),
        &[
            Step::Git(&["am", "MBOX"]),
            Step::Write("a.txt", &a),
            Step::Git(&["add", "a.txt"]),
            Step::Git(&["am", "-s", "--resolved"]),
        ],
    );
}

#[test]
fn skip_with_signoff_leaves_the_next_patch_unsigned() {
    same("skip", &conflicting_series(), &[Step::Git(&["am", "MBOX"]), Step::Git(&["am", "--signoff", "--skip"])]);
    // The session was started signed; a resuming --no-signoff does not unsign
    // the patches am_skip() reloads.
    same(
        "skip-no",
        &conflicting_series(),
        &[Step::Git(&["am", "-s", "MBOX"]), Step::Git(&["am", "--no-signoff", "--skip"])],
    );
}
