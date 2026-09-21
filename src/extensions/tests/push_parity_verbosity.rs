//! `git push` verbosity: what `-q` hides, what `-v` adds, and how the two
//! combine.
//!
//! Three separate rules in the C, all measured against stock git 2.55.0 over a
//! bare repo on disk (no network):
//!
//! * `transport_push()` gates the whole `To <url>` status block on
//!   `if (!quiet || err)` (transport.c:1545) and the `Everything up-to-date`
//!   summary on `!quiet && !ret && !transport_refs_pushed()` (transport.c:1562),
//!   so a quiet push that succeeded prints nothing at all — while a quiet push
//!   that had a ref rejected still lists it.
//! * `push_with_options()` prints `Pushing to <url>` when `verbosity > 0`
//!   (builtin/push.c:386) and `update_one_tracking_ref()` prints
//!   `updating local tracking ref '<ref>'` (transport.c:585); the latter is
//!   written *before* the closing summary, because the tracking refs are updated
//!   at transport.c:1557 and the summary at transport.c:1561-1564.
//! * `-v` and `-q` share one counter through `OPT__VERBOSITY()`, and
//!   `parse_opt_verbosity_cb()` (parse-options-cb.c:75-85) *resets* across zero
//!   rather than cancelling, so `-q -v` is verbose, `-v -q` is quiet and
//!   `-q -q -v` is verbose again.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn zvcs(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run zvcs git")
}

fn err_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn out_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A bare remote plus a work tree with one commit and `origin` pointing at it.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-pushverb-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let bare = root.join("bare");
    let work = root.join("work");
    std::fs::create_dir_all(&home).expect("mkdir home");

    assert!(
        zvcs(&root, &home, &["init", "-q", "--bare", "-b", "main", bare.to_str().expect("utf-8")])
            .status
            .success(),
        "init bare"
    );
    assert!(
        zvcs(&root, &home, &["init", "-q", "-b", "main", work.to_str().expect("utf-8")])
            .status
            .success(),
        "init work"
    );
    assert!(
        zvcs(&work, &home, &["remote", "add", "origin", "../bare"]).status.success(),
        "remote add"
    );
    assert!(
        zvcs(&work, &home, &["commit", "--allow-empty", "-q", "-m", "c0"]).status.success(),
        "commit"
    );
    (root, work, home)
}

/// Add one commit so the next push has something to send.
fn commit(work: &Path, home: &Path, msg: &str) {
    assert!(
        zvcs(work, home, &["commit", "--allow-empty", "-q", "-m", msg]).status.success(),
        "commit {msg}"
    );
}

#[test]
fn quiet_push_prints_nothing_when_it_succeeds() {
    let (root, work, home) = fixture("quiet-ok");

    // A brand new branch: the loud form would print `To ../bare` plus
    // ` * [new branch]      main -> main`.
    let new = zvcs(&work, &home, &["push", "-q", "origin", "main"]);
    assert!(new.status.success(), "push -q: {}", err_text(&new));
    assert_eq!(err_text(&new), "", "quiet push of a new branch must be silent");

    // A fast-forward, which would print the ` <old>..<new>  main -> main` row.
    commit(&work, &home, "c1");
    let ff = zvcs(&work, &home, &["push", "--quiet", "origin", "main"]);
    assert!(ff.status.success(), "push --quiet ff: {}", err_text(&ff));
    assert_eq!(err_text(&ff), "", "quiet fast-forward must be silent");

    // Nothing to do, which would print `Everything up-to-date`.
    let noop = zvcs(&work, &home, &["push", "-q", "origin", "main"]);
    assert!(noop.status.success(), "push -q noop: {}", err_text(&noop));
    assert_eq!(err_text(&noop), "", "quiet no-op must not say Everything up-to-date");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn quiet_push_still_reports_a_rejected_ref() {
    let (root, work, home) = fixture("quiet-reject");
    commit(&work, &home, "c1");
    assert!(zvcs(&work, &home, &["push", "-q", "origin", "main"]).status.success(), "seed");

    // Rewind and diverge, so the next push is a non-fast-forward.
    assert!(zvcs(&work, &home, &["reset", "-q", "--hard", "HEAD~1"]).status.success(), "reset");
    commit(&work, &home, "other");

    let out = zvcs(&work, &home, &["push", "-q", "origin", "main"]);
    assert!(!out.status.success(), "a non-fast-forward push must fail");
    let err = err_text(&out);
    // `err` is set, so transport.c:1545 prints the block despite `-q`.
    assert!(err.contains("To ../bare\n"), "quiet rejected push keeps the To header: {err}");
    assert!(
        err.contains(" ! [rejected]        main -> main (non-fast-forward)\n"),
        "quiet rejected push keeps the rejection row: {err}"
    );
    assert!(
        err.contains("error: failed to push some refs to '../bare'\n"),
        "the failure trailer is not quiet-gated: {err}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn verbose_push_announces_destination_and_tracking_ref() {
    let (root, work, home) = fixture("verbose");

    let out = zvcs(&work, &home, &["push", "-v", "origin", "main"]);
    assert!(out.status.success(), "push -v: {}", err_text(&out));
    let err = err_text(&out);
    assert!(
        err.starts_with("Pushing to ../bare\n"),
        "`Pushing to` leads, before the transport says anything: {err}"
    );
    assert!(
        err.contains("updating local tracking ref 'refs/remotes/origin/main'\n"),
        "-v names the tracking ref it writes: {err}"
    );
    assert!(
        err.contains(" * [new branch]      main -> main\n"),
        "the ordinary status row is still there under -v: {err}"
    );

    // Up to date under -v: the `= [up to date]` row, then the tracking-ref
    // notice, and only then the summary — transport.c writes the tracking refs
    // at :1557 and the summary at :1562.
    let noop = zvcs(&work, &home, &["push", "-v", "origin", "main"]);
    assert!(noop.status.success(), "push -v noop: {}", err_text(&noop));
    let err = err_text(&noop);
    let tracking = err
        .find("updating local tracking ref 'refs/remotes/origin/main'")
        .unwrap_or_else(|| panic!("no tracking notice: {err}"));
    let summary = err
        .find("Everything up-to-date")
        .unwrap_or_else(|| panic!("no summary: {err}"));
    assert!(tracking < summary, "the tracking notice precedes the summary: {err}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn verbosity_counter_resets_across_zero() {
    let (root, work, home) = fixture("counter");
    assert!(zvcs(&work, &home, &["push", "-q", "origin", "main"]).status.success(), "seed");

    // `-q` then `-v`: the counter goes -1, then resets to 1. Verbose.
    commit(&work, &home, "c1");
    let qv = zvcs(&work, &home, &["push", "-q", "-v", "origin", "main"]);
    assert!(qv.status.success(), "push -q -v: {}", err_text(&qv));
    assert!(
        err_text(&qv).starts_with("Pushing to ../bare\n"),
        "-q -v is verbose: {}",
        err_text(&qv)
    );

    // `-v` then `-q`: 1, then resets to -1. Quiet.
    commit(&work, &home, "c2");
    let vq = zvcs(&work, &home, &["push", "-v", "-q", "origin", "main"]);
    assert!(vq.status.success(), "push -v -q: {}", err_text(&vq));
    assert_eq!(err_text(&vq), "", "-v -q is quiet");

    // `-q -q -v`: -1, -2, then resets to 1. Verbose again — a counter that
    // merely incremented would land on -1 and stay quiet.
    commit(&work, &home, "c3");
    let qqv = zvcs(&work, &home, &["push", "-q", "-q", "-v", "origin", "main"]);
    assert!(qqv.status.success(), "push -q -q -v: {}", err_text(&qqv));
    assert!(
        err_text(&qqv).starts_with("Pushing to ../bare\n"),
        "-q -q -v is verbose: {}",
        err_text(&qqv)
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn quiet_porcelain_push_prints_only_done() {
    let (root, work, home) = fixture("porcelain-quiet");
    assert!(zvcs(&work, &home, &["push", "-q", "origin", "main"]).status.success(), "seed");

    // The status block is gated on `!quiet || err` whatever the output format
    // is, but `Done` is printed outside that gate (transport.c:1560).
    let out = zvcs(&work, &home, &["push", "--porcelain", "-q", "origin", "main"]);
    assert!(out.status.success(), "push --porcelain -q: {}", err_text(&out));
    assert_eq!(out_text(&out), "Done\n", "quiet porcelain prints the trailer alone");
    assert_eq!(err_text(&out), "", "and nothing on stderr");

    // Loud, for contrast: the same push lists the up-to-date ref.
    let loud = zvcs(&work, &home, &["push", "--porcelain", "origin", "main"]);
    assert!(loud.status.success(), "push --porcelain: {}", err_text(&loud));
    assert_eq!(
        out_text(&loud),
        "To ../bare\n=\trefs/heads/main:refs/heads/main\t[up to date]\nDone\n",
        "the loud porcelain block is unchanged"
    );

    let _ = std::fs::remove_dir_all(&root);
}
