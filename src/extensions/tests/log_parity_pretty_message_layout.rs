//! How the built-in formats and `%b`/`%f` lay out a commit message whose shape
//! is not the usual subject, blank line, body.
//!
//! `pretty_print_commit()` (pretty.c:2350-2367) runs `skip_blank_lines()`, then
//! `pp_remainder()`, then `strbuf_rtrim()` and one `'\n'`. `pp_remainder()`
//! (pretty.c:2254-2296) has no notion of a subject, so a subject paragraph of
//! several lines keeps one line each under `medium`/`full`/`fuller`/`raw`, and
//! `short` prints the whole first paragraph; every line goes through
//! `is_blank_line()`, which right-trims it; and the final `strbuf_rtrim()` takes
//! the blank line under the header away when the message is empty. zvcs folded
//! the subject onto one line, kept trailing whitespace, and printed a blank line
//! for an empty message.
//!
//! `%f` sanitizes only the subject's first line (pretty.c:1765-1767); `%b`
//! starts after every blank line that follows the subject, a whitespace-only
//! line included (pretty.c:1000-1010). `rev-list --pretty=oneline` ends the
//! line of a commit with an empty message (builtin/rev-list.c:373-374). A
//! placeholder `format_commit_one()` does not know — `%ax`, `%gx`, `%Gx`, a
//! trailing `%a` — prints as typed (pretty.c:856-866, :1649, :1707-1708), and
//! `%GF` is `sigc->fingerprint`, empty for an unsigned commit. `rev-list
//! --no-abbrev` makes `%h`/`%p`/`%t` print whole ids.
//!
//! Every expectation is stock git's output for the same command on the same
//! fixture.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, root: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("ZVCS_HOME", root.join(".zvcs"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap()
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    stock: &'static str,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// One empty commit per message shape, committed verbatim, plus a side
    /// branch merged back so `--graph` has a second lane to pad.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git()?;
        let root = std::env::temp_dir().join(format!("zvcs-pretty-layout-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work, stock };
        fx.stock(&["init", "-q", "-b", "main", "."]);
        let messages: [&[u8]; 9] = [
            b"subject line one\nsubject line two\n\nbody after\n",
            b"  indented subject  \n\nsecond line   \n",
            b"subj\n   \nbody behind a whitespace-only line\n",
            b"subj\n\n\n\nbody behind three blank lines\n\n\n",
            b"\n\n  \nsubject after blank lines\n\nbody\n",
            b"tab\tin\x01subject\n \t\nbody\twith\ttabs\n",
            b"a..b...c... sanitized\nsecond.line\n\nx\n",
            b"",
            b"only subject   \n   \n",
        ];
        for (i, msg) in messages.iter().enumerate() {
            let file = fx.root.join(format!("msg{i}"));
            std::fs::write(&file, msg).unwrap();
            fx.stock(&[
                "commit",
                "-q",
                "--allow-empty",
                "--allow-empty-message",
                "--cleanup=verbatim",
                "-F",
                file.to_str().unwrap(),
            ]);
            if i == 3 {
                fx.stock(&["branch", "side"]);
            }
        }
        fx.stock(&["checkout", "-q", "side"]);
        fx.stock(&["commit", "-q", "--allow-empty", "--allow-empty-message", "-m", ""]);
        fx.stock(&["checkout", "-q", "main"]);
        fx.stock(&["merge", "-q", "--no-ff", "--no-edit", "side"]);
        Some(fx)
    }

    fn stock(&self, args: &[&str]) {
        let out = run(self.stock, &self.work, &self.root, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn same(&self, args: &[&str]) {
        let want = run(self.stock, &self.work, &self.root, args);
        let got = run(BIN, &self.work, &self.root, args);
        assert_eq!(
            (String::from_utf8_lossy(&got.stdout), String::from_utf8_lossy(&got.stderr), got.status.code()),
            (String::from_utf8_lossy(&want.stdout), String::from_utf8_lossy(&want.stderr), want.status.code()),
            "{args:?}"
        );
    }
}

/// `log`, `show` and `rev-list` print the message through the same
/// `pp_remainder()`, under each indented format, with and without a graph.
#[test]
fn builtin_formats_keep_the_message_lines_as_pp_remainder_does() {
    let Some(fx) = Fixture::new("builtin") else { return };
    for pretty in ["--pretty=medium", "--pretty=short", "--pretty=full", "--pretty=fuller", "--pretty=raw"] {
        fx.same(&["log", pretty]);
        fx.same(&["log", "--graph", pretty]);
        fx.same(&["log", "-z", pretty]);
        fx.same(&["log", "--log-size", pretty]);
        fx.same(&["rev-list", "--all", pretty]);
        fx.same(&["show", "-s", pretty, "HEAD~1", "HEAD~3", "side"]);
    }
    fx.same(&["log", "--expand-tabs=4", "--pretty=medium"]);
    fx.same(&["log", "--no-expand-tabs", "--pretty=fuller"]);
    fx.same(&["log", "--expand-tabs", "--pretty=short"]);
    fx.same(&["log", "--notes", "--pretty=medium"]);
}

/// `%f` reads the subject's first line only, and `%b` starts after every blank
/// line that follows the subject.
#[test]
fn sanitized_subject_and_body_offsets_follow_parse_commit_message() {
    let Some(fx) = Fixture::new("offsets") else { return };
    fx.same(&["log", "--format=[%s]%n[%f]%n[%b]"]);
    fx.same(&["log", "--format=%B|"]);
    fx.same(&["rev-list", "--all", "--format=%f|%b"]);
}

/// An empty message under `rev-list --pretty=oneline` still ends its line.
#[test]
fn rev_list_oneline_ends_an_empty_message() {
    let Some(fx) = Fixture::new("oneline") else { return };
    fx.same(&["rev-list", "--all", "--pretty=oneline"]);
    fx.same(&["rev-list", "--all", "--oneline"]);
    fx.same(&["log", "--pretty=oneline"]);
}

/// Unknown person, reflog and signature letters print as typed; `%GF` is the
/// (empty) fingerprint of an unsigned commit.
#[test]
fn unknown_placeholders_print_as_typed() {
    let Some(fx) = Fixture::new("unknown") else { return };
    for format in ["%ax|%cz|%gx|%Gx|%GF|", "%h %a", "%h %c", "%h %g", "%h %G", "%<(8)%ax|"] {
        fx.same(&["log", &format!("--format={format}")]);
        fx.same(&["rev-list", "--all", &format!("--format={format}")]);
    }
    fx.same(&["log", "-g", "--format=%gx|%gd"]);
}

/// `rev-list --no-abbrev` zeroes `revs->abbrev`, which `%h`, `%p` and `%t` read as
/// `ctx.abbrev` (builtin/rev-list.c:333): `odb_find_abbrev_len()` answers the
/// whole id for a zero length (odb.c:952-955).
#[test]
fn rev_list_no_abbrev_reaches_the_placeholders() {
    let Some(fx) = Fixture::new("noabbrev") else { return };
    fx.same(&["rev-list", "--all", "--no-abbrev", "--format=%h %p %t"]);
    fx.same(&["rev-list", "--all", "--abbrev=9", "--no-abbrev", "--format=%h"]);
    fx.same(&["rev-list", "--all", "--no-abbrev", "--abbrev=9", "--format=%h"]);
    fx.same(&["rev-list", "--all", "--no-abbrev", "--abbrev-commit", "--oneline"]);
}
