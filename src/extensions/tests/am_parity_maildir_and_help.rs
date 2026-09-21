//! Two measured divergences between this port and stock git 2.55.0 in `git am`.
//!
//! **A lone `-h`/`--help-all` is answered before anything else.** git.c:474-476
//! demotes the `RUN_SETUP` of that exact form ("demote to GENTLY to allow
//! 'git cmd -h' outside repo"), and builtin/am.c:2448-2450 puts
//! `show_usage_with_options_if_asked()` ahead of `repo_config()`. So neither a
//! missing repository nor a malformed `am.threeWay` can preempt the usage block.
//! This port ran `discover()` and then the `am.*` config first, so `git am -h`
//! outside a repository answered `fatal: not a git repository` (128) and, inside
//! one, `am.threeWay=bogus` answered the config fatal (128) — both where git
//! prints usage and exits 129.
//!
//! **A Maildir is enumerated and split the way `git mailsplit` does it.**
//! `populate_maildir_list()` (builtin/mailsplit.c:114-149) scans `cur` then
//! `new` into one `string_list` ordered by `maildir_filename_cmp()` — a byte
//! comparison in which digit runs present on both sides compare as integers —
//! and `split_maildir()` (:172-217) hands each file to `split_one(f, name, 1)`,
//! which rewrites `\r\n` to `\n` unless `--keep-cr`, un-escapes mboxrd `>From `,
//! and stops at a second `From ` postmark only when the first line was one.
//! This port instead read `new` then `cur`, sorted each directory on its own by
//! plain byte order, and copied every file verbatim — so it applied patches in
//! the wrong order and left CRLF line endings in the mail it handed to
//! `mailinfo`.
//!
//! Every expectation below is stock git 2.55.0's own observed output; the
//! fixtures are built with this binary, so the tests need no second git.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// git's usage line for `am`, the first line of both `-h` and `--help-all`.
const AM_USAGE_FIRST_LINE: &str = "usage: git am [<options>] [(<mbox> | <Maildir>)...]";

fn git(dir: &Path, args: &[&str]) {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// One invocation under a pinned, isolated environment: no system or global
/// config, a fixed locale and time zone, and fixed identities and dates so the
/// commits a successful `am` writes are reproducible.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
        .output()
        .unwrap()
}

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-am-md-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// A repo on `base` plus two mailbox files, `second.mbox` and `third.mbox`,
/// which only apply in that order: `second` rewrites line 2, `third` rewrites it
/// back and appends a line. Applying `third` first therefore fails, which is
/// what makes enumeration order observable rather than merely cosmetic.
fn fixture(tag: &str) -> PathBuf {
    let repo = scratch(tag);
    git(&repo, &["init", "-q", "-b", "main", "."]);
    std::fs::write(repo.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["tag", "base"]);

    std::fs::write(repo.join("f.txt"), "one\nTWO\nthree\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "second"]);
    let second = run(&repo, &["format-patch", "-1", "--stdout"]);
    assert!(second.status.success());
    std::fs::write(repo.join("second.mbox"), &second.stdout).unwrap();

    std::fs::write(repo.join("f.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "third"]);
    let third = run(&repo, &["format-patch", "-1", "--stdout"]);
    assert!(third.status.success());
    std::fs::write(repo.join("third.mbox"), &third.stdout).unwrap();

    git(&repo, &["reset", "-q", "--hard", "base"]);
    repo
}

/// Place `mbox` into `<repo>/md/<sub>/<name>`, creating the Maildir as needed.
fn deliver(repo: &Path, sub: &str, name: &str, mbox: &str) {
    let dir = repo.join("md").join(sub);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(repo.join(mbox), dir.join(name)).unwrap();
}

/// The subject lines `base..HEAD` carries, oldest first — i.e. the order the
/// Maildir's messages were applied in.
fn applied(repo: &Path) -> Vec<String> {
    let out = run(repo, &["log", "--reverse", "--format=%s", "base..HEAD"]);
    assert!(out.status.success(), "log failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn am_lone_h_answers_usage_without_a_repository() {
    // A plain directory: no repository anywhere above it inside the scratch root.
    let dir = scratch("h-norepo");
    for flag in ["-h", "--help-all"] {
        let out = run(&dir, &["am", flag]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(129), "`am {flag}` exit code");
        assert_eq!(
            stdout.lines().next(),
            Some(AM_USAGE_FIRST_LINE),
            "`am {flag}` first stdout line"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "",
            "`am {flag}` must say nothing on stderr outside a repository"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn am_lone_h_answers_usage_before_reading_am_config() {
    let repo = fixture("h-badconfig");
    // `git_am_config` would reject this boolean, but git never reaches it: the
    // usage block is printed by show_usage_with_options_if_asked() two lines
    // earlier (builtin/am.c:2448 vs :2450).
    git(&repo, &["config", "am.threeWay", "notabool"]);
    for flag in ["-h", "--help-all"] {
        let out = run(&repo, &["am", flag]);
        assert_eq!(out.status.code(), Some(129), "`am {flag}` exit code");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).lines().next(),
            Some(AM_USAGE_FIRST_LINE),
            "`am {flag}` first stdout line"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "",
            "`am {flag}` must not report the malformed am.threeWay"
        );
    }
    // The same malformed value is still fatal for a real invocation, so the
    // early return above is scoped to the lone help form and nothing else.
    let out = run(&repo, &["am", "second.mbox"]);
    assert_eq!(out.status.code(), Some(128), "real run still hits the config");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: bad boolean config value 'notabool' for 'am.threeway'\n",
        "config fatal still reported for a non-help invocation"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn am_maildir_reads_cur_before_new() {
    let repo = fixture("cur-before-new");
    // Byte order alone would put `new/aaa` first; git's list is keyed on the
    // whole `<sub>/<name>` string, so every `cur/` name beginning with 'c'
    // precedes a `new/` name beginning with 'n'.
    deliver(&repo, "cur", "zzz", "second.mbox");
    deliver(&repo, "new", "aaa", "third.mbox");
    let out = run(&repo, &["am", "md"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "both patches must apply cleanly in cur-then-new order"
    );
    assert_eq!(out.status.code(), Some(0), "exit code");
    assert_eq!(applied(&repo), vec!["second", "third"], "applied order");
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn am_maildir_compares_digit_runs_as_numbers() {
    let repo = fixture("numeric-order");
    // `maildir_filename_cmp` runs `strtol` over both digit runs, so 9 < 10;
    // plain byte order would sort "10" before "9" and apply `third` first.
    deliver(&repo, "cur", "9", "second.mbox");
    deliver(&repo, "cur", "10", "third.mbox");
    let out = run(&repo, &["am", "md"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "cur/9 must precede cur/10"
    );
    assert_eq!(out.status.code(), Some(0), "exit code");
    assert_eq!(applied(&repo), vec!["second", "third"], "applied order");
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn am_maildir_rewrites_crlf_unless_keep_cr() {
    let repo = fixture("maildir-crlf");
    let crlf: Vec<u8> = std::fs::read(repo.join("second.mbox"))
        .unwrap()
        .split_inclusive(|b| *b == b'\n')
        .flat_map(|l| match l.strip_suffix(b"\n") {
            Some(body) => [body, b"\r\n"].concat(),
            None => l.to_vec(),
        })
        .collect();
    std::fs::create_dir_all(repo.join("md/cur")).unwrap();
    std::fs::write(repo.join("md/cur/1"), &crlf).unwrap();

    // split_one() strips the CR before mailinfo ever sees the mail, so a CRLF
    // Maildir message applies exactly as its LF twin does.
    let out = run(&repo, &["am", "md"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "CRLF Maildir mail must apply cleanly"
    );
    assert_eq!(out.status.code(), Some(0), "exit code");
    assert_eq!(applied(&repo), vec!["second"], "applied the message");
    assert_eq!(
        std::fs::read(repo.join("f.txt")).unwrap(),
        b"one\nTWO\nthree\n",
        "worktree content after applying the CRLF mail"
    );

    // `--keep-cr` suppresses exactly that rewrite, so the patch no longer
    // matches the worktree and the session stops with the quoted-CR warning.
    git(&repo, &["reset", "-q", "--hard", "base"]);
    let out = run(&repo, &["am", "--keep-cr", "md"]);
    assert_eq!(out.status.code(), Some(128), "--keep-cr exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: quoted CRLF detected"),
        "--keep-cr must leave the CRs in the mail: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn am_maildir_bare_message_keeps_an_embedded_from_line() {
    let repo = fixture("maildir-bare");
    // split_one() takes `is_bare` from the *first* line only. This mail does not
    // open with a postmark, so the `From ` line inside its commit message is
    // body text and must survive into the commit, not truncate the message.
    let mail = b"From: A U Thor <author@example.com>\n\
                 Date: Thu, 7 Apr 2005 15:13:13 -0700\n\
                 Subject: [PATCH] second\n\
                 \n\
                 From nobody Mon Sep 17 00:00:00 2001\n\
                 ---\n\
                 diff --git a/f.txt b/f.txt\n\
                 index 814f4a4..5f1a0b1 100644\n\
                 --- a/f.txt\n\
                 +++ b/f.txt\n\
                 @@ -1,3 +1,3 @@\n\
                 \x20one\n\
                 -two\n\
                 +TWO\n\
                 \x20three\n";
    std::fs::create_dir_all(repo.join("md/cur")).unwrap();
    std::fs::write(repo.join("md/cur/1"), mail).unwrap();

    let out = run(&repo, &["am", "md"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "a bare Maildir mail is copied whole, diff included"
    );
    assert_eq!(out.status.code(), Some(0), "exit code");
    let body = run(&repo, &["log", "--format=%B", "-1"]);
    assert_eq!(
        String::from_utf8_lossy(&body.stdout),
        "second\n\nFrom nobody Mon Sep 17 00:00:00 2001\n\n",
        "the embedded postmark stays in the commit message"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn am_maildir_unreadable_message_reports_the_split_failure_too() {
    let repo = fixture("maildir-empty-mail");
    // An empty file is `strbuf_getwholeline()` hitting EOF straight away, which
    // split_maildir() reports through error_errno() with errno untouched, and
    // cmd_mailsplit then adds its own line (builtin/mailsplit.c:366-369) before
    // `am` dies. All three lines are part of the contract.
    std::fs::create_dir_all(repo.join("md/cur")).unwrap();
    std::fs::write(repo.join("md/cur/1"), b"").unwrap();
    std::fs::copy(repo.join("second.mbox"), repo.join("md/cur/2")).unwrap();

    let out = run(&repo, &["am", "md"]);
    assert_eq!(out.status.code(), Some(128), "exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut lines = stderr.lines();
    assert!(
        lines.next().is_some_and(|l| l.starts_with("error: cannot read mail md/cur/1: ")),
        "per-mail diagnostic: {stderr}"
    );
    assert_eq!(
        lines.next(),
        Some("error: cannot split patches from md"),
        "cmd_mailsplit's own line: {stderr}"
    );
    assert_eq!(
        lines.next(),
        Some("fatal: Failed to split patches."),
        "am's die(): {stderr}"
    );
    assert!(
        !repo.join(".git/rebase-apply").exists(),
        "am_destroy() leaves no session behind"
    );
    let _ = std::fs::remove_dir_all(&repo);
}
