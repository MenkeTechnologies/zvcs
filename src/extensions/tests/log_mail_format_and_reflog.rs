//! The mail pretty formats, the `--reflog` pseudo-option, and `get_oid_basic()`'s
//! reflog-reach diagnostics.
//!
//! `CMIT_FMT_EMAIL`/`CMIT_FMT_MBOXRD` are `pretty.c`'s, but the two commands that
//! render them disagree about what they look like, and the disagreement is not a
//! detail: `log-tree.c`'s `show_log()` fills in `ctx.rev` and
//! `ctx.print_email_subject` and prints the magic `From <oid> Mon Sep 17 00:00:00
//! 2001` line itself, while `builtin/rev-list.c` builds a zeroed
//! `pretty_print_context` and prints `commit <oid>` instead. So `git log
//! --pretty=email` gets `Subject: [PATCH] …` and RFC2047-encoded headers where
//! `git rev-list --pretty=email` gets a bare `Subject:` and raw UTF-8 — from the
//! same commit and the same format name.
//!
//! `--reflog` is `add_reflogs_to_pending()`: the old and the new id of every entry
//! of every reflog become pending tips, which is how a commit no ref points at any
//! more is still reachable.
//!
//! `<ref>@{<n>}` past the end of a log is a `die()` inside `get_oid()`, while
//! `<ref>@{<date>}` older than the whole log is a *warning* and the operand still
//! resolves — two outcomes that are not interchangeable, and the second of which
//! is easy to drop silently because nothing about the command's exit status or
//! stdout shows it is missing.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `show_date(1700000000, 0, DATE_MODE(RFC2822))`, the one date format the mail
/// headers hardcode regardless of `--date=`/`log.date`.
const RFC2822: &str = "Tue, 14 Nov 2023 22:13:20 +0000";

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-mailfmt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        Fixture { root, repo }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            // The identity has to come from this fixture's own `git config`
            // lines, and an ambient GIT_AUTHOR_* / GIT_COMMITTER_* beats repo
            // config. CI exports all four (`Configure git identity` in
            // ci.yml), so every `From:` line rendered `zvcs ci
            // <ci@zvcs.test>` there while passing on a developer machine that
            // sets none of them. Removed rather than pinned, so the config
            // lines below stay the thing under test; the one commit that wants
            // a different author still sets GIT_AUTHOR_* explicitly.
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL")
            .env_remove("EMAIL");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// stdout of a command that must succeed.
    fn out(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(out.status.code(), Some(0), "`git {args:?}`: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    /// Two commits: an ASCII one with a body whose second line an mbox reader
    /// would mistake for a message separator, and a non-ASCII one on top.
    fn two_commits(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.repo.join("f.txt"), "i\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&[
            "commit",
            "-q",
            "-m",
            "plain subject\n\nfirst body line\nFrom here be dragons\nlast line",
        ]);
        std::fs::write(f.repo.join("f.txt"), "j\n").unwrap();
        f.git(&["add", "-A"]);
        let out = f
            .cmd(&["commit", "-q", "-m", "sübject"])
            .env("GIT_AUTHOR_NAME", "Ünïcode")
            .env("GIT_AUTHOR_EMAIL", "u@e.co")
            .output()
            .unwrap();
        assert!(out.status.success(), "setup commit failed: {out:?}");
        f
    }
}

/// Replace every 40-character hex run with `<oid>`.
fn blank_hashes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(|c: char| c.is_ascii_hexdigit()) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let run = rest
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(rest.len());
        if run == 40 {
            out.push_str("<oid>");
        } else {
            out.push_str(&rest[..run]);
        }
        rest = &rest[run..];
    }
    out.push_str(rest);
    out
}

/// The whole `--pretty=email` stream: the magic `From` line, the identity headers,
/// the `[PATCH]`-prefixed subject, the MIME block a non-ASCII body forces, and the
/// two-newline ending `strbuf_rtrim()` plus `sb->len <= beginning_of_body` leaves a
/// bodyless record with.
#[test]
fn log_pretty_email_renders_the_mailbox_record() {
    let f = Fixture::two_commits("email");
    let got = blank_hashes(&f.out(&["log", "--pretty=email"]));
    assert_eq!(
        got,
        format!(
            "From <oid> Mon Sep 17 00:00:00 2001\n\
             From: =?UTF-8?q?=C3=9Cn=C3=AFcode?= <u@e.co>\n\
             Date: {RFC2822}\n\
             Subject: [PATCH] =?UTF-8?q?s=C3=BCbject?=\n\
             MIME-Version: 1.0\n\
             Content-Type: text/plain; charset=UTF-8\n\
             Content-Transfer-Encoding: 8bit\n\
             \n\
             \n\
             From <oid> Mon Sep 17 00:00:00 2001\n\
             From: t <t@e.co>\n\
             Date: {RFC2822}\n\
             Subject: [PATCH] plain subject\n\
             \n\
             first body line\n\
             From here be dragons\n\
             last line\n"
        )
    );
}

/// `rev-list`'s zeroed `pretty_print_context` reaches neither
/// `fmt_output_email_subject()` nor the RFC2047 encoding, and its own
/// `commit <oid>` header stands where `log`'s magic `From` line does.
#[test]
fn rev_list_pretty_email_drops_the_patch_prefix_and_the_encoding() {
    let f = Fixture::two_commits("revlist-email");
    let got = blank_hashes(&f.out(&["rev-list", "--pretty=email", "--max-count=1", "HEAD"]));
    assert_eq!(
        got,
        format!(
            "commit <oid>\n\
             From: Ünïcode <u@e.co>\n\
             Date: {RFC2822}\n\
             Subject: sübject\n\
             MIME-Version: 1.0\n\
             Content-Type: text/plain; charset=UTF-8\n\
             Content-Transfer-Encoding: 8bit\n\
             \n\
             \n"
        )
    );
}

/// `CMIT_FMT_MBOXRD` differs from `CMIT_FMT_EMAIL` in exactly one place — the
/// `/^>*From /` escape `pp_remainder()` applies — so the headers, whose `From:` is
/// not a body line, are untouched.
#[test]
fn mboxrd_escapes_only_the_body_from_line() {
    let f = Fixture::two_commits("mboxrd");
    let got = blank_hashes(&f.out(&["log", "--pretty=mboxrd", "-1", "HEAD~1"]));
    assert_eq!(
        got,
        format!(
            "From <oid> Mon Sep 17 00:00:00 2001\n\
             From: t <t@e.co>\n\
             Date: {RFC2822}\n\
             Subject: [PATCH] plain subject\n\
             \n\
             first body line\n\
             >From here be dragons\n\
             last line\n"
        )
    );
    // `email` leaves the same line alone, which is the whole difference.
    let plain = f.out(&["log", "--pretty=email", "-1", "HEAD~1"]);
    assert!(plain.contains("\nFrom here be dragons\n"), "{plain}");
}

/// `git_log_config()` reads `format.subjectPrefix` and `format.encodeEmailHeaders`
/// (builtin/log.c:560-561, 566-569) even though the options that set them belong to
/// `format-patch`, and `--[no-]encode-email-headers` is `setup_revisions()`'s, so
/// the last spelling on the command line wins over the config.
#[test]
fn email_headers_follow_format_config_and_the_revision_option() {
    let f = Fixture::two_commits("email-cfg");
    let subject = |args: &[&str]| -> String {
        f.out(args)
            .lines()
            .find(|l| l.starts_with("Subject:"))
            .unwrap_or_default()
            .to_owned()
    };
    let from = |args: &[&str]| -> String {
        f.out(args)
            .lines()
            .find(|l| l.starts_with("From: "))
            .unwrap_or_default()
            .to_owned()
    };

    assert_eq!(
        subject(&["log", "--pretty=email", "-1"]),
        "Subject: [PATCH] =?UTF-8?q?s=C3=BCbject?="
    );
    assert_eq!(
        subject(&["-c", "format.subjectPrefix=RFC", "log", "--pretty=email", "-1"]),
        "Subject: [RFC] =?UTF-8?q?s=C3=BCbject?="
    );
    // `fmt_output_email_subject()`'s `*opt->subject_prefix` test: an empty prefix
    // drops the brackets rather than printing `[] `.
    assert_eq!(
        subject(&["-c", "format.subjectPrefix=", "log", "--pretty=email", "-1"]),
        "Subject: =?UTF-8?q?s=C3=BCbject?="
    );
    assert_eq!(
        from(&["log", "--pretty=email", "-1", "--no-encode-email-headers"]),
        "From: Ünïcode <u@e.co>"
    );
    assert_eq!(
        from(&["-c", "format.encodeEmailHeaders=false", "log", "--pretty=email", "-1"]),
        "From: Ünïcode <u@e.co>"
    );
    // The command line is read after the config, and the last spelling wins.
    assert_eq!(
        from(&[
            "-c",
            "format.encodeEmailHeaders=false",
            "log",
            "--pretty=email",
            "--no-encode-email-headers",
            "--encode-email-headers",
            "-1",
        ]),
        "From: =?UTF-8?q?=C3=9Cn=C3=AFcode?= <u@e.co>"
    );
    // `rev-list` never reads either, whatever the config says.
    assert_eq!(
        subject(&["-c", "format.subjectPrefix=RFC", "rev-list", "--pretty=email", "--max-count=1", "HEAD"]),
        "Subject: sübject"
    );
}

/// `add_reflogs_to_pending()` pends the old and the new id of every reflog entry,
/// so a commit the branch has been moved off is still walked — which is the only
/// thing `--reflog` is for.
#[test]
fn reflog_pseudo_option_reaches_a_commit_no_ref_points_at() {
    let f = Fixture::two_commits("reflog-opt");
    let tip = f.out(&["rev-parse", "HEAD"]).trim().to_owned();
    f.git(&["update-ref", "refs/heads/main", "HEAD~1"]);

    let plain = f.out(&["log", "--format=%s"]);
    assert_eq!(plain, "plain subject\n", "the branch was moved back: {plain}");

    let with_reflog = f.out(&["log", "--reflog", "--format=%s"]);
    assert!(with_reflog.contains("sübject"), "{with_reflog}");
    assert!(with_reflog.contains("plain subject"), "{with_reflog}");
    // The id really came out of a log, not out of a ref.
    let refs = f.out(&["for-each-ref", "--format=%(objectname)"]);
    assert!(!refs.contains(&tip), "the moved-off commit still has a ref: {refs}");
}

/// `read_ref_at()`'s two endings. A selector past the end of the log is
/// `die("log for '%.*s' only has %d entries")`; a date older than the whole log is
/// `warning("log for '%.*s' only goes back to %s")` and the operand goes on to
/// resolve, so the command succeeds with full output and the only trace is on
/// stderr.
#[test]
fn reflog_selectors_out_of_reach_die_or_warn() {
    let f = Fixture::two_commits("reach");

    let fatal = f.run(&["log", "--format=%s", "HEAD@{99}"]);
    assert_eq!(fatal.status.code(), Some(128), "{fatal:?}");
    assert_eq!(
        String::from_utf8_lossy(&fatal.stderr),
        "fatal: log for 'HEAD' only has 2 entries\n"
    );
    assert!(fatal.stdout.is_empty(), "{fatal:?}");

    let warned = f.run(&["log", "--format=%s", "HEAD@{2005-01-01}"]);
    assert_eq!(warned.status.code(), Some(0), "{warned:?}");
    assert_eq!(
        String::from_utf8_lossy(&warned.stderr),
        format!("warning: log for 'HEAD' only goes back to {RFC2822}\n")
    );
    // The operand resolved to the oldest entry, and the walk ran normally.
    assert_eq!(String::from_utf8_lossy(&warned.stdout), "plain subject\n");

    // A selector the log does reach says nothing at all.
    let quiet = f.run(&["log", "--format=%s", "HEAD@{2030-01-01}"]);
    assert_eq!(quiet.status.code(), Some(0), "{quiet:?}");
    assert!(quiet.stderr.is_empty(), "{quiet:?}");

    // A bare `@{…}` is diagnosed under the branch's name, not `HEAD`:
    // `get_oid_basic()` resolves it with `repo_dwim_ref(r, "HEAD", …)`, which
    // reports the symref's target.
    let bare = f.run(&["rev-parse", "@{2005-01-01}"]);
    assert_eq!(bare.status.code(), Some(0), "{bare:?}");
    assert_eq!(
        String::from_utf8_lossy(&bare.stderr),
        format!("warning: log for 'main' only goes back to {RFC2822}\n")
    );

    // `--quiet` is `GET_OID_QUIETLY`, the one switch that silences it.
    let quiet_flag = f.run(&["rev-parse", "-q", "@{2005-01-01}"]);
    assert!(quiet_flag.stderr.is_empty(), "{quiet_flag:?}");
}

/// `--max-age`/`--min-age` set the same `revs->max_age`/`revs->min_age` as
/// `--since`/`--until`, and only their value parser differs: `parse_age()` reads a
/// raw epoch with `strtoumax` and dies on anything left over.
#[test]
fn max_and_min_age_take_a_raw_epoch() {
    let f = Fixture::two_commits("ages");
    // Both commits sit exactly on 1700000000, so a bound one second later keeps
    // them and one second earlier drops them.
    assert_eq!(f.out(&["log", "--format=%s", "--max-age=1699999999"]).lines().count(), 2);
    assert_eq!(f.out(&["log", "--format=%s", "--min-age=1700000001"]).lines().count(), 2);
    assert_eq!(f.out(&["log", "--format=%s", "--max-age=1700000001"]), "");
    assert_eq!(f.out(&["log", "--format=%s", "--min-age=1699999999"]), "");
    // The value may stand as the next argv element (`parse_long_opt()`).
    assert_eq!(f.out(&["log", "--format=%s", "--max-age", "1699999999"]).lines().count(), 2);

    let bad = f.run(&["log", "--format=%s", "--max-age=bogus"]);
    assert_eq!(bad.status.code(), Some(128), "{bad:?}");
    assert_eq!(
        String::from_utf8_lossy(&bad.stderr),
        "fatal: 'bogus': not a number of seconds since epoch\n"
    );
    // `--max-age=-1` wraps to `UINTMAX_MAX`, which is `repo_init_revisions()`'s
    // own sentinel — so it parses and then does nothing.
    assert_eq!(f.out(&["log", "--format=%s", "--max-age=-1"]).lines().count(), 2);
}

/// `cmd_show` runs the same `cmd_log_init` as `cmd_log`, so its mail record is
/// `log`'s and not `rev-list`'s: the magic `From <oid>` line, the `[PATCH]`
/// subject prefix and the RFC2047 encoding all reach it, because `show_log()`
/// fills in `ctx.rev`/`ctx.print_email_subject` for both (log-tree.c:697-705).
///
/// `email` also separates its records rather than terminating them, so a second
/// commit is preceded by a blank line.
#[test]
fn show_pretty_email_is_logs_mailbox_record() {
    let f = Fixture::two_commits("show-email");
    let one = blank_hashes(&f.out(&["show", "--pretty=email", "--no-patch", "HEAD"]));
    assert_eq!(
        one,
        format!(
            "From <oid> Mon Sep 17 00:00:00 2001\n\
             From: =?UTF-8?q?=C3=9Cn=C3=AFcode?= <u@e.co>\n\
             Date: {RFC2822}\n\
             Subject: [PATCH] =?UTF-8?q?s=C3=BCbject?=\n\
             MIME-Version: 1.0\n\
             Content-Type: text/plain; charset=UTF-8\n\
             Content-Transfer-Encoding: 8bit\n\
             \n"
        )
    );

    let two = blank_hashes(&f.out(&["show", "--pretty=email", "--no-patch", "HEAD", "HEAD~1"]));
    assert_eq!(
        two,
        format!(
            "From <oid> Mon Sep 17 00:00:00 2001\n\
             From: =?UTF-8?q?=C3=9Cn=C3=AFcode?= <u@e.co>\n\
             Date: {RFC2822}\n\
             Subject: [PATCH] =?UTF-8?q?s=C3=BCbject?=\n\
             MIME-Version: 1.0\n\
             Content-Type: text/plain; charset=UTF-8\n\
             Content-Transfer-Encoding: 8bit\n\
             \n\
             \n\
             From <oid> Mon Sep 17 00:00:00 2001\n\
             From: t <t@e.co>\n\
             Date: {RFC2822}\n\
             Subject: [PATCH] plain subject\n\
             \n\
             first body line\n\
             From here be dragons\n\
             last line\n"
        )
    );

    // `--no-encode-email-headers` is `setup_revisions()`'s option and reaches
    // `show` as well, leaving the identity and subject raw.
    let raw = f.out(&[
        "show",
        "--pretty=email",
        "--no-encode-email-headers",
        "--no-patch",
        "HEAD",
    ]);
    assert!(raw.contains("From: Ünïcode <u@e.co>\n"), "{raw}");
    assert!(raw.contains("Subject: [PATCH] sübject\n"), "{raw}");

    // `mboxrd` differs from `email` only in the body escape.
    let mboxrd = f.out(&["show", "--pretty=mboxrd", "--no-patch", "HEAD~1"]);
    assert!(mboxrd.contains("\n>From here be dragons\n"), "{mboxrd}");
}

/// `show_tag_object()` sends the `tagger` line through `pp_user_info()`, so the
/// identity block an annotated tag prints is the *selected format's*:
/// `oneline` prints none at all, the mail formats print `From:` with an RFC2822
/// `Date:`, a user format prints `Tagger:` with no date line, and only `medium`
/// prints both halves (pretty.c:516-595).
#[test]
fn show_tag_identity_block_follows_the_pretty_format() {
    let f = Fixture::two_commits("show-tag");
    f.git(&["tag", "-a", "v1", "-m", "tag body", "HEAD"]);

    let medium = f.out(&["show", "--pretty=medium", "--no-patch", "v1"]);
    assert!(medium.starts_with("tag v1\nTagger: t <t@e.co>\nDate:   "), "{medium}");

    // `if (pp->fmt == CMIT_FMT_ONELINE) return;` — the whole block is skipped.
    let oneline = f.out(&["show", "--pretty=oneline", "--no-patch", "v1"]);
    assert!(oneline.starts_with("tag v1\n\ntag body\n"), "{oneline}");

    // The `switch (pp->fmt)` that writes `Date:` has no `CMIT_FMT_USERFORMAT`
    // arm, so a user format gets the identity and no date.
    let user = f.out(&["show", "--pretty=format:%H", "--no-patch", "v1"]);
    assert!(user.starts_with("tag v1\nTagger: t <t@e.co>\n\ntag body\n"), "{user}");

    // The mail formats take `From:` and the hardcoded RFC2822 date, which
    // `--date=` does not reach.
    let email = f.out(&["show", "--pretty=email", "--date=short", "--no-patch", "v1"]);
    assert!(
        email.starts_with(&format!("tag v1\nFrom: t <t@e.co>\nDate: {RFC2822}\n\ntag body\n")),
        "{email}"
    );
}

/// `rev-list` runs the same `setup_revisions()` as `log`, so the options that
/// live there are not `log`'s alone. `--max-age`/`--min-age` take
/// `parse_age()`'s raw epoch and die on anything else; `-<n>` is
/// `revision.c`'s `(*arg == '-' && isdigit(arg[1]))` branch;
/// `--[no-]encode-email-headers` is accepted but — with
/// `builtin/rev-list.c`'s zeroed `pretty_print_context` — changes nothing it
/// prints.
#[test]
fn rev_list_takes_the_setup_revisions_options_log_has() {
    let f = Fixture::two_commits("revlist-opts");

    assert_eq!(f.out(&["rev-list", "--max-age=1699999999", "HEAD"]).lines().count(), 2);
    assert_eq!(f.out(&["rev-list", "--max-age=1700000001", "HEAD"]), "");
    assert_eq!(f.out(&["rev-list", "--min-age=1700000001", "HEAD"]).lines().count(), 2);
    assert_eq!(f.out(&["rev-list", "--min-age=1699999999", "HEAD"]), "");
    // Detached value, which is `parse_long_opt()`.
    assert_eq!(f.out(&["rev-list", "--min-age", "1700000001", "HEAD"]).lines().count(), 2);
    // `--max-age=-1` wraps onto `repo_init_revisions()`'s own sentinel.
    assert_eq!(f.out(&["rev-list", "--max-age=-1", "HEAD"]).lines().count(), 2);

    let bad = f.run(&["rev-list", "--min-age=bogus", "HEAD"]);
    assert_eq!(bad.status.code(), Some(128), "{bad:?}");
    assert_eq!(
        String::from_utf8_lossy(&bad.stderr),
        "fatal: 'bogus': not a number of seconds since epoch\n"
    );
    let missing = f.run(&["rev-list", "--max-age"]);
    assert_eq!(missing.status.code(), Some(128), "{missing:?}");
    assert_eq!(
        String::from_utf8_lossy(&missing.stderr),
        "fatal: Option '--max-age' requires a value\n"
    );

    assert_eq!(f.out(&["rev-list", "-1", "HEAD"]).lines().count(), 1);
    assert_eq!(f.out(&["rev-list", "-2", "HEAD"]).lines().count(), 2);

    // Accepted, and inert: the same bytes as without it.
    let plain = f.out(&["rev-list", "--pretty=email", "--max-count=1", "HEAD"]);
    let flagged = f.out(&[
        "rev-list",
        "--encode-email-headers",
        "--pretty=email",
        "--max-count=1",
        "HEAD",
    ]);
    assert_eq!(plain, flagged);
    assert!(plain.contains("Subject: sübject\n"), "{plain}");
}

/// `--sparse` is `revs->dense = 0`, and it does two separable things to a
/// path-limited walk: `try_to_simplify_commit()`'s early return
/// (`if (!revs->dense && !commit->parents->next) return;`, revision.c:996) stops
/// a non-merge from ever being compared, and the `revs->prune && revs->dense`
/// display gate (revision.c:4221) no longer drops what *is* TREESAME. The
/// in-place parent prune survives both, so a merge that took one side leaves the
/// other side unwalked — which is why `--sparse` is not simply "every commit".
#[test]
fn rev_list_sparse_keeps_treesame_commits_but_not_the_pruned_side() {
    let f = Fixture::new("revlist-sparse");
    std::fs::create_dir_all(&f.repo).unwrap();
    f.git(&["init", "-q", "-b", "main", "."]);
    f.git(&["config", "user.email", "t@e.co"]);
    f.git(&["config", "user.name", "t"]);
    // Every commit gets its own second: with one shared timestamp the output
    // order is a queue tie rather than a date, and this asserts on order.
    let clock = std::cell::Cell::new(1_700_000_000u64);
    // Runs a history-writing command on the next second of that clock.
    let tick = |args: &[&str]| {
        clock.set(clock.get() + 60);
        let stamp = format!("{} +0000", clock.get());
        let out = f
            .cmd(args)
            .env("GIT_AUTHOR_DATE", &stamp)
            .env("GIT_COMMITTER_DATE", &stamp)
            .env("GIT_MERGE_AUTOEDIT", "no")
            .output()
            .unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    };
    let commit = |_: &Fixture, msg: &str| tick(&["commit", "-q", "-m", msg]);

    std::fs::write(f.repo.join("base.txt"), "base\n").unwrap();
    f.git(&["add", "-A"]);
    commit(&f, "root");
    std::fs::write(f.repo.join("other.txt"), "o1\n").unwrap();
    f.git(&["add", "-A"]);
    commit(&f, "other1");
    f.git(&["checkout", "-q", "-b", "side", "main"]);
    std::fs::write(f.repo.join("tracked.txt"), "s1\n").unwrap();
    f.git(&["add", "-A"]);
    commit(&f, "side1");
    std::fs::write(f.repo.join("tracked.txt"), "s1\ns2\n").unwrap();
    f.git(&["add", "-A"]);
    commit(&f, "side2");
    f.git(&["checkout", "-q", "main"]);
    std::fs::write(f.repo.join("other.txt"), "o1\no2\n").unwrap();
    f.git(&["add", "-A"]);
    commit(&f, "other2");
    tick(&["merge", "-q", "--no-ff", "-m", "merge", "side"]);
    std::fs::write(f.repo.join("tracked.txt"), "s1\ns2\nm3\n").unwrap();
    f.git(&["add", "-A"]);
    commit(&f, "main3");

    let subjects = |args: &[&str]| -> Vec<String> {
        f.out(args).lines().map(str::to_owned).collect()
    };

    assert_eq!(
        subjects(&["rev-list", "--format=%s", "--no-commit-header", "main"]),
        ["main3", "merge", "other2", "side2", "side1", "other1", "root"]
    );
    // Dense: every TREESAME commit is dropped.
    assert_eq!(
        subjects(&["rev-list", "--format=%s", "--no-commit-header", "main", "--", "tracked.txt"]),
        ["main3", "side2", "side1"]
    );
    // Sparse: nothing is dropped for being TREESAME — but `other2` is gone all
    // the same, because the merge was pruned in place to the parent it was
    // TREESAME to and the walk never reached the other side.
    assert_eq!(
        subjects(&[
            "rev-list",
            "--format=%s",
            "--no-commit-header",
            "--sparse",
            "main",
            "--",
            "tracked.txt",
        ]),
        ["main3", "merge", "side2", "side1", "other1", "root"]
    );
    // `--dense` is only ever an undo of an earlier `--sparse`.
    assert_eq!(
        subjects(&[
            "rev-list",
            "--format=%s",
            "--no-commit-header",
            "--sparse",
            "--dense",
            "main",
            "--",
            "tracked.txt",
        ]),
        ["main3", "side2", "side1"]
    );
    // `simplify_commit()` reaches `rewrite_parents()` only under
    // `revs->prune && revs->dense`, so `--sparse --parents` prints the pruned
    // ancestry rather than a rewritten one: the merge shows a single parent.
    let parents = f.out(&["rev-list", "--parents", "--sparse", "main", "--", "tracked.txt"]);
    for line in parents.lines() {
        assert!(
            line.split_whitespace().count() <= 2,
            "a pruned merge should print one parent: {line}"
        );
    }

    // Without a pathspec there is nothing to simplify and `--sparse` says nothing.
    assert_eq!(
        f.out(&["rev-list", "--sparse", "main"]),
        f.out(&["rev-list", "main"])
    );
}

/// `--reflog` is `add_reflogs_to_pending()` in `rev-list` exactly as it is in
/// `log`: the ids come from `$GIT_DIR/logs`, not from the ref store, so a commit
/// no ref points at any more is still listed. `*flags` is what `--not` holds, so
/// `--not --reflog` excludes them instead.
#[test]
fn rev_list_reflog_pends_ids_from_the_logs() {
    let f = Fixture::two_commits("revlist-reflog");
    let tip = f.out(&["rev-parse", "HEAD"]).trim().to_owned();
    f.git(&["update-ref", "refs/heads/main", "HEAD~1"]);

    assert!(!f.out(&["rev-list", "main"]).contains(&tip));
    assert!(f.out(&["rev-list", "--reflog"]).contains(&tip));
    // An excluded pending object is what clears `revs->no_walk`, and `--not`
    // puts UNINTERESTING on every id `--reflog` pends.
    assert_eq!(f.out(&["rev-list", "--not", "--reflog", "--all"]), "");
}

/// `handle_dotdot_1()` joins its two `repo_get_oid_with_context()` calls with
/// `||`, so a left endpoint that does not resolve means the right one is never
/// looked at — and never earns the `warning: refname … is ambiguous.` its own
/// resolution would have printed. Both halves of `get_oid_basic()`'s stderr are
/// on that rule: the ambiguity block and the reflog-reach warning.
#[test]
fn range_endpoints_warn_only_up_to_the_one_that_failed() {
    let f = Fixture::two_commits("range-warn");
    let tip = f.out(&["rev-parse", "HEAD"]).trim().to_owned();
    // A ref whose *name* is 40 hex characters is what the warning is about.
    f.git(&["update-ref", &format!("refs/heads/{tip}"), &tip]);

    let warnings = |args: &[&str]| -> usize {
        let out = f.run(args);
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter(|l| l.starts_with("warning: refname "))
            .count()
    };

    for cmd in ["shortlog", "rev-list", "log", "show"] {
        // Both endpoints resolve: one warning each.
        assert_eq!(warnings(&[cmd, &format!("{tip}..{tip}")]), 2, "{cmd}");
        assert_eq!(warnings(&[cmd, &format!("{tip}...{tip}")]), 2, "{cmd}");
        // The right endpoint fails, so the left has already warned.
        assert_eq!(warnings(&[cmd, &format!("{tip}..nosuch")]), 1, "{cmd}");
        // The left endpoint fails, so the right is never resolved: silence.
        assert_eq!(warnings(&[cmd, &format!("nosuch..{tip}")]), 0, "{cmd}");
        assert_eq!(warnings(&[cmd, &format!("nosuch...{tip}")]), 0, "{cmd}");
    }
}

/// The reflog-reach warning is the other half of `get_oid_basic()`'s stderr, and
/// it is not `log`'s alone: every command that reads a revision off argv runs the
/// same `setup_revisions()`.
#[test]
fn reflog_reach_warning_reaches_every_revision_reader() {
    let f = Fixture::two_commits("reach-shared");
    let expected = format!("warning: log for 'main' only goes back to {RFC2822}\n");
    for cmd in ["shortlog", "rev-list", "log"] {
        let out = f.run(&[cmd, "main@{2005-01-01}"]);
        assert_eq!(out.status.code(), Some(0), "{cmd}: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), expected, "{cmd}");
    }
    // A selector past the end of the log is the fatal, not the warning.
    for cmd in ["shortlog", "rev-list", "log"] {
        let out = f.run(&[cmd, "main@{99}"]);
        assert_eq!(out.status.code(), Some(128), "{cmd}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "fatal: log for 'main' only has 2 entries\n",
            "{cmd}"
        );
    }
}

/// `interpret_branch_mark()` resolves an `@{u}`/`@{upstream}` operand inside
/// `get_oid()` and `die()`s there when `branch_get_upstream()` has nothing to
/// return — so the command never gets to report an unresolvable argument, and
/// the "ambiguous argument" block is the wrong answer rather than a differently
/// worded one.
#[test]
fn upstream_mark_dies_inside_get_oid() {
    let f = Fixture::two_commits("upstream");

    for cmd in ["log", "show", "rev-list", "shortlog"] {
        for spec in ["main@{u}", "main@{upstream}", "HEAD@{u}", "@{u}"] {
            let out = f.run(&[cmd, spec]);
            assert_eq!(out.status.code(), Some(128), "{cmd} {spec}: {out:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stderr),
                "fatal: no upstream configured for branch 'main'\n",
                "{cmd} {spec}"
            );
        }
        // `ref_exists(branch->refname)` is tested first, so an unknown branch is
        // reported as such rather than as a missing upstream.
        let unknown = f.run(&[cmd, "nosuch@{u}"]);
        assert_eq!(
            String::from_utf8_lossy(&unknown.stderr),
            "fatal: no such branch: 'nosuch'\n",
            "{cmd}"
        );
    }

    // `upstream_mark()` compares with `strncasecmp` and only requires the mark to
    // *start* at the `@`, so the spelling is case-insensitive and the die still
    // happens with the mark not consuming the whole operand.
    for spec in ["main@{U}", "main@{UpStReAm}", "main@{u}xyz", "main@{u}^", "^main@{u}"] {
        let out = f.run(&["log", spec]);
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "fatal: no upstream configured for branch 'main'\n",
            "{spec}"
        );
    }
    // `interpret_branch_name()` walks the `@` positions left to right.
    let at_in_name = f.run(&["log", "a@b@{u}"]);
    assert_eq!(
        String::from_utf8_lossy(&at_in_name.stderr),
        "fatal: no such branch: 'a@b'\n"
    );

    // `branch_get(NULL)` is NULL on a detached HEAD.
    f.git(&["checkout", "-q", "--detach", "HEAD"]);
    let detached = f.run(&["log", "HEAD@{u}"]);
    assert_eq!(
        String::from_utf8_lossy(&detached.stderr),
        "fatal: HEAD does not point to a branch\n"
    );
}

/// The other half of `branch_get("HEAD")`: once an upstream *is* configured, the
/// mark resolves to it — and it resolves under `HEAD@{u}` as well as under the
/// branch's own name, because git looks up the same branch for both. A resolver
/// that reads `HEAD` as a branch name asks about a nonexistent
/// `branch.HEAD.remote` and fails only for that spelling, which is exactly the
/// shape this pins down.
#[test]
fn head_upstream_mark_resolves_like_the_branch_it_points_at() {
    let f = Fixture::two_commits("upstream-ok");
    let want = f.out(&["rev-parse", "HEAD~1"]).trim().to_owned();
    f.git(&["update-ref", "refs/remotes/origin/main", &want]);
    f.git(&["config", "remote.origin.url", "."]);
    f.git(&["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
    f.git(&["config", "branch.main.remote", "origin"]);
    f.git(&["config", "branch.main.merge", "refs/heads/main"]);

    for spec in ["@{u}", "main@{u}", "HEAD@{u}", "HEAD@{upstream}"] {
        let got = f.out(&["log", "--format=%H", "--max-count=1", spec]);
        assert_eq!(got.trim(), want, "{spec}");
    }
}
