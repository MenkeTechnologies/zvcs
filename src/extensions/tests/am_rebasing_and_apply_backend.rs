//! `git am --rebasing` — the mode `git rebase --apply` drives — and the pieces
//! of `am` that only that mode exercises.
//!
//! Under `--rebasing`, `parse_mail_rebase()` (builtin/am.c:1464) reads **only**
//! each message's `From <oid>` postmark and then rebuilds everything from the
//! commit that names: `get_commit_info()` takes the authorship and message off
//! the commit object, and `write_commit_patch()` regenerates the diff. The mail
//! body is never consulted. That is the property most of these tests pin,
//! because a port that quietly fell back to `git mailinfo` would still pass a
//! naive "does it apply" check while silently rewriting authorship — and
//! authorship is exactly what a rebase must not touch.
//!
//! Every expectation below was measured against stock git 2.55.0 and is written
//! as a literal or a structural invariant rather than compared against whatever
//! `git` happens to be on `PATH`: in this project `git` on `PATH` is zvcs itself
//! and reports `git version 2.55.0`, so a live comparison would compare the
//! binary with itself and pass vacuously.
//!
//! Hooks and editors are written as POSIX `sh` so the suite runs headless on
//! Linux CI with no interactive terminal.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The commit the fixtures replay. Pinned so the author line is a fixed string
/// that cannot coincide with the committer identity — a port that rewrote the
/// author would produce `C O Mitter`, which every assertion here would catch.
const A_NAME: &str = "Orig Author";
const A_EMAIL: &str = "orig@example.com";
const A_DATE: &str = "2001-02-03T04:05:06+0700";
/// `show_ident_date(&id, DATE_MODE(NORMAL))` for [`A_DATE`] — the spelling
/// `get_commit_info()` puts in `author-script`, measured from stock.
const A_DATE_NORMAL: &str = "Sat Feb 3 04:05:06 2001 +0700";
const A_DATE_ISO: &str = "2001-02-03 04:05:06 +0700";

struct Fx {
    repo: PathBuf,
    home: PathBuf,
}

impl Fx {
    /// Run the binary under a fully pinned environment. `~/.gitconfig` on a
    /// developer box may set `core.commentChar`, and an unpinned `HOME` would
    /// let it reach the commit messages `am` writes, so `HOME`,
    /// `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM` and `GIT_CONFIG_NOSYSTEM` are
    /// all closed off.
    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.home.join("gitconfig-system"))
            .env("GIT_AUTHOR_NAME", "C O Mitter")
            .env("GIT_AUTHOR_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-0700")
            .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-0700")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        for (k, v) in extra {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// `git log -1 --format=<fmt>` on the current HEAD, trimmed.
    fn show(&self, fmt: &str) -> String {
        // `--date=iso` pins `%ad`/`%cd`: the default renderer is git's NORMAL
        // format, which would make these assertions compare spellings, not times.
        let out = self.ok(&["log", "-1", "--date=iso", &format!("--format={fmt}")]);
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    fn rev(&self, spec: &str) -> String {
        let out = self.ok(&["rev-parse", spec]);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn git_path(&self, rel: &str) -> PathBuf {
        self.repo.join(".git").join(rel)
    }

    fn read_state(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.git_path(rel)).ok()
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.repo.join(rel), body).unwrap();
    }

    /// Write an executable POSIX `sh` hook.
    fn hook(&self, name: &str, body: &str) {
        let dir = self.git_path("hooks");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

/// `main` holds `base`; `topic` adds one commit authored by [`A_NAME`].
/// `<tag>` keeps concurrent tests out of each other's directories.
fn fixture(tag: &str) -> Fx {
    let root = std::env::temp_dir().join(format!("zvcs-amreb-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(home.join("gitconfig"), "").unwrap();
    std::fs::write(home.join("gitconfig-system"), "").unwrap();
    let fx = Fx { repo, home };
    fx.ok(&["init", "-q", "-b", "main"]);
    fx.write("f.txt", "base\n");
    fx.ok(&["add", "f.txt"]);
    fx.ok(&["commit", "-q", "-m", "base"]);
    fx.ok(&["checkout", "-q", "-b", "topic"]);
    fx.write("f.txt", "REAL\n");
    fx.ok(&["add", "f.txt"]);
    fx.run_env(
        &["commit", "-q", "-m", "TOPIC"],
        &[
            ("GIT_AUTHOR_NAME", A_NAME),
            ("GIT_AUTHOR_EMAIL", A_EMAIL),
            ("GIT_AUTHOR_DATE", A_DATE),
        ],
    );
    fx
}

/// `format-patch` the topic commit, then hand the mailbox back.
fn mailbox(fx: &Fx) -> Vec<u8> {
    let out = fx.ok(&[
        "format-patch",
        "-k",
        "--stdout",
        "--full-index",
        "--no-renames",
        "--no-cover-letter",
        "main..topic",
    ]);
    out.stdout
}

/// Feed `mbox` to `git am <args>` on stdin.
fn am_stdin(fx: &Fx, args: &[&str], mbox: &[u8]) -> Output {
    use std::io::Write;
    let mut all = vec!["am"];
    all.extend_from_slice(args);
    let mut c = Command::new(BIN);
    c.args(&all)
        .current_dir(&fx.repo)
        .env("HOME", &fx.home)
        .env("ZVCS_HOME", &fx.home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", fx.home.join("gitconfig"))
        .env("GIT_CONFIG_SYSTEM", fx.home.join("gitconfig-system"))
        .env("GIT_AUTHOR_NAME", "C O Mitter")
        .env("GIT_AUTHOR_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-0700")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut ch = c.spawn().unwrap();
    ch.stdin.take().unwrap().write_all(mbox).unwrap();
    ch.wait_with_output().unwrap()
}

/// The mail body is a lie; the `From <oid>` postmark is the truth.
///
/// `write_commit_patch()` regenerates the diff from the commit, so replacing the
/// hunk's `+REAL` with `+LIE` must change nothing about the result. Measured
/// against stock: the replayed commit's tree equals the original's, `f.txt`
/// holds `REAL`, and the author survives verbatim. A port that applied the mail
/// body instead would write `LIE` and a different tree.
#[test]
fn rebasing_replays_the_commit_not_the_mail_body() {
    let fx = fixture("body");
    let topic = fx.rev("topic");
    let tree = fx.rev("topic^{tree}");
    let mbox = mailbox(&fx);
    let corrupt = String::from_utf8_lossy(&mbox).replace("+REAL", "+LIE");
    fx.ok(&["checkout", "-q", "main"]);

    let out = am_stdin(&fx, &["--rebasing"], corrupt.as_bytes());
    assert!(out.status.success(), "am --rebasing: {out:?}");

    assert_eq!(fx.rev("HEAD^{tree}"), tree, "tree comes from the commit");
    assert_eq!(
        std::fs::read_to_string(fx.repo.join("f.txt")).unwrap(),
        "REAL\n",
        "worktree holds the commit's content, not the mail body's"
    );
    // Authorship is carried over whole: name, email and date.
    assert_eq!(fx.show("%an|%ae|%ad|%cn"), format!("{A_NAME}|{A_EMAIL}|{A_DATE_ISO}|C O Mitter"));

    // `--rebasing` records what it replayed, and leaves housekeeping to its
    // caller: `am_run` skips `am_destroy` in this mode (builtin/am.c:1937).
    let rewritten = fx.read_state("rebase-apply/rewritten").expect("rewritten written");
    assert_eq!(rewritten, format!("{topic} {}\n", fx.rev("HEAD")));
    assert!(fx.git_path("rebase-apply").is_dir(), "session survives under --rebasing");
}

/// `git rebase --apply`'s resume path: the authorship of a commit whose patch
/// conflicted must survive the stop, the manual resolution, and `--continue`.
///
/// `parse_mail_rebase()` writes `author-script` from the *commit*, and
/// `am_resolve()` reads it back, so the replayed commit keeps [`A_NAME`]. This
/// is the misattribution trap: a port that let `--continue` fall back to the
/// ambient identity would commit as `C O Mitter` and still exit 0.
#[test]
fn rebasing_keeps_authorship_across_conflict_and_continue() {
    let fx = fixture("continue");
    let topic = fx.rev("topic");
    let mbox = mailbox(&fx);
    fx.ok(&["checkout", "-q", "main"]);
    // Make `main` conflict with the patch's pre-image.
    fx.write("f.txt", "main\n");
    fx.ok(&["add", "f.txt"]);
    fx.ok(&["commit", "-q", "-m", "MAIN"]);

    let out = am_stdin(&fx, &["--rebasing"], &mbox);
    assert_eq!(out.status.code(), Some(128), "a conflicting patch stops with 128");

    // The stop is fully described on disk: which commit, and its authorship.
    assert_eq!(
        fx.read_state("rebase-apply/original-commit").unwrap().trim(),
        topic
    );
    assert_eq!(fx.rev("REBASE_HEAD"), topic, "REBASE_HEAD names the replayed commit");
    let script = fx.read_state("rebase-apply/author-script").unwrap();
    assert_eq!(
        script,
        format!(
            "GIT_AUTHOR_NAME='{A_NAME}'\nGIT_AUTHOR_EMAIL='{A_EMAIL}'\nGIT_AUTHOR_DATE='{A_DATE_NORMAL}'\n"
        ),
        "author-script holds the replayed commit's author in git's NORMAL date spelling"
    );

    fx.write("f.txt", "resolved\n");
    fx.ok(&["add", "f.txt"]);
    let out = fx.run(&["am", "--continue"]);
    assert!(out.status.success(), "am --continue: {out:?}");
    assert_eq!(
        fx.show("%an|%ae|%ad"),
        format!("{A_NAME}|{A_EMAIL}|{A_DATE_ISO}"),
        "--continue must not reattribute the commit to the committer"
    );
    assert_eq!(
        fx.read_state("rebase-apply/rewritten").unwrap(),
        format!("{topic} {}\n", fx.rev("HEAD"))
    );
}

/// `am --skip` inside a live `--rebasing` session.
///
/// `am_skip()` maps the skipped commit to the *current* `HEAD` in `rewritten`
/// (builtin/am.c:2134) — that is how a dropped commit is reported to the
/// `post-rewrite` hook — and its `clean_index()` moves no ref. Measured against
/// stock: no new `HEAD` reflog entry, no `ORIG_HEAD`, and the `AUTO_MERGE` left
/// by the three-way attempt is gone, because `clean_index()` ends in
/// `remove_branch_state()`.
#[test]
fn skip_records_head_and_leaves_no_merge_state() {
    let fx = fixture("skip");
    let topic = fx.rev("topic");
    let mbox = mailbox(&fx);
    fx.ok(&["checkout", "-q", "main"]);
    fx.write("f.txt", "main\n");
    fx.ok(&["add", "f.txt"]);
    fx.ok(&["commit", "-q", "-m", "MAIN"]);

    let out = am_stdin(&fx, &["--rebasing"], &mbox);
    assert_eq!(out.status.code(), Some(128));
    // The three-way fallback ran and recorded its result.
    assert!(fx.git_path("AUTO_MERGE").exists(), "3-way fallback records AUTO_MERGE");

    let head_before = fx.rev("HEAD");
    let reflog_before = fx.ok(&["reflog", "show", "--format=%gd", "HEAD"]).stdout.len();

    let out = fx.run(&["am", "--skip"]);
    assert!(out.status.success(), "am --skip: {out:?}");

    assert_eq!(fx.rev("HEAD"), head_before, "--skip moves no ref");
    assert_eq!(
        fx.ok(&["reflog", "show", "--format=%gd", "HEAD"]).stdout.len(),
        reflog_before,
        "clean_index() writes no HEAD reflog entry (reset --hard would)"
    );
    assert!(!fx.git_path("ORIG_HEAD").exists(), "clean_index() writes no ORIG_HEAD");
    assert!(
        !fx.git_path("AUTO_MERGE").exists(),
        "remove_branch_state() drops AUTO_MERGE with the discarded conflict"
    );
    assert_eq!(
        fx.read_state("rebase-apply/rewritten").unwrap(),
        format!("{topic} {head_before}\n"),
        "a skipped commit is rewritten to the current HEAD"
    );
}

/// `--resolvemsg=<text>` replaces git's whole `--continue`/`--skip`/`--abort`
/// hint block rather than adding to it (builtin/am.c:1161-1184). `git rebase
/// --apply` relies on this: it is why a conflicted apply-backend rebase talks
/// about `git rebase` and never mentions `git am`.
#[test]
fn resolvemsg_replaces_the_hint_block() {
    let fx = fixture("resolvemsg");
    let mbox = mailbox(&fx);
    fx.ok(&["checkout", "-q", "main"]);
    fx.write("f.txt", "main\n");
    fx.ok(&["add", "f.txt"]);
    fx.ok(&["commit", "-q", "-m", "MAIN"]);

    let out = am_stdin(&fx, &["--rebasing", "--resolvemsg=FIRST LINE\nSECOND LINE"], &mbox);
    assert_eq!(out.status.code(), Some(128));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("hint: FIRST LINE\n"), "each line is hinted: {err}");
    assert!(err.contains("hint: SECOND LINE\n"), "{err}");
    assert!(
        !err.contains("git am --continue"),
        "the built-in block must be replaced, not appended to: {err}"
    );
    assert!(
        err.contains("hint: Disable this message with \"git config set advice.mergeConflict false\""),
        "the advice trailer still applies: {err}"
    );
}

/// `-n`/`--no-verify` suppresses `applypatch-msg` and `pre-applypatch` and
/// nothing else: `post-applypatch` is run unconditionally (builtin/am.c:1729),
/// and its exit status is discarded. Measured against stock.
#[test]
fn no_verify_suppresses_only_the_two_verifying_hooks() {
    let fx = fixture("hooks");
    let mbox = mailbox(&fx);
    fx.ok(&["checkout", "-q", "main"]);
    for h in ["applypatch-msg", "pre-applypatch", "post-applypatch"] {
        fx.hook(h, &format!("echo {h} >> \"$GIT_DIR/../ran.log\"\nexit 0"));
    }

    let out = am_stdin(&fx, &[], &mbox);
    assert!(out.status.success(), "am: {out:?}");
    let log = std::fs::read_to_string(fx.repo.join("ran.log")).unwrap();
    assert_eq!(
        log, "applypatch-msg\npre-applypatch\npost-applypatch\n",
        "all three hooks fire in git's order"
    );

    // Second run with --no-verify, from the same base.
    let fx2 = fixture("hooks-nv");
    let mbox2 = mailbox(&fx2);
    fx2.ok(&["checkout", "-q", "main"]);
    for h in ["applypatch-msg", "pre-applypatch", "post-applypatch"] {
        fx2.hook(h, &format!("echo {h} >> \"$GIT_DIR/../ran.log\"\nexit 0"));
    }
    let out = am_stdin(&fx2, &["--no-verify"], &mbox2);
    assert!(out.status.success(), "am --no-verify: {out:?}");
    assert_eq!(
        std::fs::read_to_string(fx2.repo.join("ran.log")).unwrap(),
        "post-applypatch\n",
        "--no-verify skips the two verifying hooks but not post-applypatch"
    );
}

/// A rejecting `applypatch-msg` stops `am` with exit 1 before anything is
/// committed, and a hook that edits `final-commit` in place changes the message
/// that gets committed — `run_applypatch_msg_hook()` re-reads the file
/// afterwards (builtin/am.c:1478-1497).
#[test]
fn applypatch_msg_hook_can_rewrite_or_reject() {
    let fx = fixture("apmsg");
    let mbox = mailbox(&fx);
    fx.ok(&["checkout", "-q", "main"]);
    fx.hook("applypatch-msg", "printf 'HOOKED\\n' >> \"$1\"\nexit 0");
    let out = am_stdin(&fx, &["--rebasing"], &mbox);
    assert!(out.status.success(), "am: {out:?}");
    assert!(
        fx.show("%B").contains("HOOKED"),
        "the rewritten final-commit is what gets committed: {:?}",
        fx.show("%B")
    );

    let fx2 = fixture("apmsg-reject");
    let mbox2 = mailbox(&fx2);
    fx2.ok(&["checkout", "-q", "main"]);
    let before = fx2.rev("HEAD");
    fx2.hook("applypatch-msg", "exit 1");
    let out = am_stdin(&fx2, &["--rebasing"], &mbox2);
    assert_eq!(out.status.code(), Some(1), "a rejecting hook exits 1");
    assert_eq!(fx2.rev("HEAD"), before, "and commits nothing");
}

/// `am -3`: when the patch does not apply, `fall_back_threeway()` rebuilds the
/// pre-image tree from the patch's own `index` lines and merges. Here the
/// context is shifted by a line prepended upstream, so the straight apply fails
/// and the fallback must succeed cleanly.
///
/// The three progress lines and their order were measured from stock.
#[test]
fn threeway_fallback_reconstructs_the_base_and_merges() {
    let root = std::env::temp_dir().join(format!("zvcs-amreb-3way-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(home.join("gitconfig"), "").unwrap();
    std::fs::write(home.join("gitconfig-system"), "").unwrap();
    let fx = Fx { repo, home };
    fx.ok(&["init", "-q", "-b", "main"]);
    fx.write("f.txt", "1\n2\n3\n4\n5\n");
    fx.ok(&["add", "f.txt"]);
    fx.ok(&["commit", "-q", "-m", "base"]);
    fx.ok(&["checkout", "-q", "-b", "topic"]);
    fx.write("f.txt", "1\n2\nTHREE\n4\n5\n");
    fx.ok(&["add", "f.txt"]);
    fx.ok(&["commit", "-q", "-m", "change"]);
    let mbox = fx.ok(&["format-patch", "--full-index", "--stdout", "-1"]).stdout;
    fx.ok(&["checkout", "-q", "main"]);
    // Shift every hunk line down by one so the patch's offsets no longer match.
    fx.write("f.txt", "zero\n1\n2\n3\n4\n5\n");
    fx.ok(&["add", "f.txt"]);
    fx.ok(&["commit", "-q", "-m", "shift"]);

    let out = am_stdin(&fx, &["-3"], &mbox);
    assert!(out.status.success(), "am -3: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Using index info to reconstruct a base tree...\n"), "{stdout}");
    assert!(stdout.contains("Falling back to patching base and 3-way merge...\n"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(fx.repo.join("f.txt")).unwrap(),
        "zero\n1\n2\nTHREE\n4\n5\n",
        "the merge keeps the upstream line and applies the patch's change"
    );
    // A clean fallback commits and leaves no session behind.
    assert!(!fx.git_path("rebase-apply").exists(), "clean run destroys the session");
}

/// `git rebase --reset-author-date` (and its hidden `--ignore-date` spelling)
/// restamps the **author** date of every replayed commit and leaves the
/// committer alone — `fmt_ident(..., NULL, ...)` reads the wall clock and
/// ignores `GIT_COMMITTER_DATE`, so a pinned committer date must not leak into
/// the author line (sequencer.c:1636-1682).
///
/// This guards the bug where the option was accepted and silently dropped by the
/// merge backend: the replayed commits kept their original 2001 author date.
#[test]
fn rebase_reset_author_date_restamps_author_not_committer() {
    let fx = fixture("resetdate");
    // `topic` is one commit ahead of `base`; give `main` a commit so the rebase
    // has real work and cannot preemptively fast-forward.
    fx.ok(&["checkout", "-q", "main"]);
    fx.write("other.txt", "o\n");
    fx.ok(&["add", "other.txt"]);
    fx.ok(&["commit", "-q", "-m", "MAIN"]);
    fx.ok(&["checkout", "-q", "topic"]);

    let out = fx.run(&["rebase", "--reset-author-date", "main"]);
    assert!(out.status.success(), "rebase: {out:?}");

    // The author identity is untouched; only the date moved.
    assert_eq!(fx.show("%an|%ae"), format!("{A_NAME}|{A_EMAIL}"));
    assert_ne!(
        fx.show("%ad"),
        A_DATE_ISO,
        "--reset-author-date must not keep the original author date"
    );
    // The committer date stays where the environment pinned it, which also
    // proves the restamp did not simply copy the committer's time.
    assert_eq!(fx.show("%cd"), "2005-04-07 15:13:13 -0700");
    assert_ne!(fx.show("%ad"), fx.show("%cd"), "author was restamped to now, committer was not");
}

/// `--committer-date-is-author-date` copies the surviving author date onto the
/// committer. Combined with `--reset-author-date` the two compose: git calls
/// `reset_ident_date()` and dates both identities by the same fresh value.
#[test]
fn rebase_committer_date_is_author_date_composes_with_reset() {
    let fx = fixture("cdate");
    fx.ok(&["checkout", "-q", "main"]);
    fx.write("other.txt", "o\n");
    fx.ok(&["add", "other.txt"]);
    fx.ok(&["commit", "-q", "-m", "MAIN"]);
    fx.ok(&["checkout", "-q", "topic"]);

    let out = fx.run(&["rebase", "--committer-date-is-author-date", "main"]);
    assert!(out.status.success(), "rebase: {out:?}");
    assert_eq!(
        fx.show("%cd"),
        A_DATE_ISO,
        "the committer takes the replayed commit's author date"
    );
    assert_eq!(fx.show("%ad"), A_DATE_ISO, "and the author date is untouched");

    // Now both together: the author is restamped first, then copied across.
    let fx2 = fixture("cdate-both");
    fx2.ok(&["checkout", "-q", "main"]);
    fx2.write("other.txt", "o\n");
    fx2.ok(&["add", "other.txt"]);
    fx2.ok(&["commit", "-q", "-m", "MAIN"]);
    fx2.ok(&["checkout", "-q", "topic"]);
    let out = fx2.run(&[
        "rebase",
        "--committer-date-is-author-date",
        "--reset-author-date",
        "main",
    ]);
    assert!(out.status.success(), "rebase: {out:?}");
    assert_ne!(fx2.show("%ad"), A_DATE_ISO, "author restamped");
    assert_eq!(fx2.show("%ad"), fx2.show("%cd"), "committer follows the restamped author");
}

/// A stopped merge-backend rebase must record `--reset-author-date` in its state
/// directory, because `--continue` re-reads it (sequencer.c:3239-3242) and also
/// re-clears `allow_ff` from it. Without the marker a resumed rebase would leave
/// the remaining commits with their original dates.
///
/// The other half is that `allow_ff` itself is *not* recorded. `read_populate_opts()`'s
/// rebase arm clears it from exactly three flag files — `signoff`, `cdate_is_adate`,
/// `ignore_date` — and `-f`/`--no-ff` is persisted nowhere (`save_opts()`'s
/// `options.allow-ff` at sequencer.c:3698 is cherry-pick/revert state, in
/// `$GIT_DIR/sequencer/opts`). Measured against stock: `rebase --reset-author-date`,
/// `rebase -f` and `rebase --signoff` all stop with no `no-ff` file, and a
/// `--continue` after `rebase -f` logs `rebase: fast-forward` again.
#[test]
fn reset_author_date_survives_into_the_state_directory() {
    let fx = fixture("statefile");
    fx.ok(&["checkout", "-q", "main"]);
    fx.write("other.txt", "o\n");
    fx.ok(&["add", "other.txt"]);
    fx.ok(&["commit", "-q", "-m", "MAIN"]);
    fx.ok(&["checkout", "-q", "topic"]);

    // `--exec false` stops the rebase after the pick, leaving the state dir.
    let out = fx.run(&["rebase", "--reset-author-date", "--exec", "false", "main"]);
    assert!(!out.status.success(), "the failing exec stops the rebase");
    assert!(
        fx.git_path("rebase-merge/ignore_date").exists(),
        "the option is recorded for --continue"
    );
    assert!(
        !fx.git_path("rebase-merge/no-ff").exists(),
        "git persists allow_ff for cherry-pick/revert only, never for a rebase"
    );
}

/// `am`'s date options, including the two shapes where the *absence* of a date
/// matters. `do_commit` reaches `fmt_ident` directly in git, so a NULL or empty
/// date argument means `ident_default_date()` — the wall clock — and never
/// `$GIT_AUTHOR_DATE`. This port drives `commit-tree`, which does read the
/// environment, so both arms must clear it explicitly.
///
/// The ambient `GIT_AUTHOR_DATE` here is 2001, the mail's is 2003, and "now" is
/// neither, so a leak from the environment is unambiguous. All four expectations
/// were measured against stock git 2.55.0.
#[test]
fn am_date_options_never_fall_through_to_the_environment() {
    // (extra am args, mail carries a Date: header, expect author == mail date,
    //  expect committer == mail date)
    let cases: &[(&[&str], bool, bool, bool)] = &[
        (&[], true, true, false),
        (&["--ignore-date"], true, false, false),
        (&["--committer-date-is-author-date"], true, true, true),
        (&["--ignore-date", "--committer-date-is-author-date"], true, false, false),
        // No `Date:` at all: the author date is "now" even without --ignore-date.
        (&[], false, false, false),
        (&["--committer-date-is-author-date"], false, false, false),
    ];
    const MAIL_DATE: &str = "2003-03-03 03:03:03 +0000";
    const AMBIENT: &str = "2001-01-01T00:00:00+0000";

    for (n, (extra, with_date, author_is_mail, committer_is_mail)) in cases.iter().enumerate() {
        let fx = fixture(&format!("dates{n}"));
        // Re-author `topic` with the mail date so it is distinct from both the
        // ambient value and "now".
        fx.ok(&["checkout", "-q", "topic"]);
        let mut mbox = String::from_utf8(mailbox(&fx)).unwrap();
        mbox = mbox.replace(
            &format!("Date: {}", mbox.lines().find(|l| l.starts_with("Date: ")).unwrap().trim_start_matches("Date: ")),
            "Date: Mon, 3 Mar 2003 03:03:03 +0000",
        );
        if !with_date {
            mbox = mbox.lines().filter(|l| !l.starts_with("Date: ")).collect::<Vec<_>>().join("\n") + "\n";
        }
        fx.ok(&["checkout", "-q", "main"]);

        let mut args = vec!["am"];
        args.extend_from_slice(extra);
        let mut c = Command::new(BIN);
        c.args(&args)
            .current_dir(&fx.repo)
            .env("HOME", &fx.home)
            .env("ZVCS_HOME", &fx.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", fx.home.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", fx.home.join("gitconfig-system"))
            .env("GIT_AUTHOR_NAME", "C O Mitter")
            .env("GIT_AUTHOR_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            // The trap: an ambient author date that is neither the mail's nor now.
            .env("GIT_AUTHOR_DATE", AMBIENT)
            .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-0700")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut ch = c.spawn().unwrap();
        use std::io::Write;
        ch.stdin.take().unwrap().write_all(mbox.as_bytes()).unwrap();
        let out = ch.wait_with_output().unwrap();
        assert!(out.status.success(), "case {n} ({extra:?}): {out:?}");

        let ad = fx.show("%ad");
        let cd = fx.show("%cd");
        assert!(
            !ad.starts_with("2001-01-01"),
            "case {n} ({extra:?}): the ambient GIT_AUTHOR_DATE leaked into the author line: {ad}"
        );
        if *author_is_mail {
            assert_eq!(ad, MAIL_DATE, "case {n} ({extra:?}): author date");
        } else {
            assert_ne!(ad, MAIL_DATE, "case {n} ({extra:?}): author date should be restamped");
        }
        if *committer_is_mail {
            assert_eq!(cd, MAIL_DATE, "case {n} ({extra:?}): committer date");
        }
        // `--committer-date-is-author-date` always ties the two together.
        if extra.contains(&"--committer-date-is-author-date") {
            assert_eq!(ad, cd, "case {n} ({extra:?}): committer must follow the author");
        } else {
            assert_eq!(
                cd, "2005-04-07 15:13:13 -0700",
                "case {n} ({extra:?}): committer date is left to the environment"
            );
        }
    }
}

/// `-b`/`--binary` does nothing, but giving it still prints a deprecation
/// notice on stderr (builtin/am.c:2461-2464). Measured verbatim from stock.
#[test]
fn binary_option_still_warns() {
    let fx = fixture("bwarn");
    let mbox = mailbox(&fx);
    fx.ok(&["checkout", "-q", "main"]);
    let out = am_stdin(&fx, &["-b"], &mbox);
    assert!(out.status.success(), "am -b: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "The -b/--binary option has been a no-op for long time, and\n\
         it will be removed. Please do not use it anymore.\n"
    );
}

/// `git rebase --exec` must leave no `AUTO_MERGE` behind, while a plain rebase
/// does. `pick_commits()` clears the previous instruction's records at the top
/// of every instruction (sequencer.c:5043-5048), so the trailing `exec` wipes
/// the last pick's `AUTO_MERGE` and writes no merge of its own. Both halves are
/// asserted, because only the contrast pins the mechanism.
#[test]
fn exec_clears_the_previous_picks_auto_merge() {
    let plain = fixture("automerge-plain");
    plain.ok(&["checkout", "-q", "main"]);
    plain.write("other.txt", "o\n");
    plain.ok(&["add", "other.txt"]);
    plain.ok(&["commit", "-q", "-m", "MAIN"]);
    plain.ok(&["checkout", "-q", "topic"]);
    let out = plain.run(&["rebase", "main"]);
    assert!(out.status.success(), "rebase: {out:?}");
    assert!(
        plain.git_path("AUTO_MERGE").exists(),
        "a plain rebase leaves the last pick's AUTO_MERGE"
    );

    let exec = fixture("automerge-exec");
    exec.ok(&["checkout", "-q", "main"]);
    exec.write("other.txt", "o\n");
    exec.ok(&["add", "other.txt"]);
    exec.ok(&["commit", "-q", "-m", "MAIN"]);
    exec.ok(&["checkout", "-q", "topic"]);
    let out = exec.run_env(&["rebase", "--exec", "true", "main"], &[("GIT_SEQUENCE_EDITOR", ":")]);
    assert!(out.status.success(), "rebase --exec: {out:?}");
    assert!(
        !exec.git_path("AUTO_MERGE").exists(),
        "the trailing exec clears it"
    );
}
