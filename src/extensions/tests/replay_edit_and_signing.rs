//! Two things the sequencer and the commit machinery used to get wrong in ways
//! that changed committed data without saying so.
//!
//! **`-e`/`--edit`.** `git revert -e` and `git cherry-pick -e` do not merely
//! choose a cleanup mode: `do_commit()` refuses the in-process path whenever
//! `EDIT_MSG` is set (sequencer.c:1728) and hands the whole commit to a real
//! `git commit -e` (sequencer.c:1750-1754). `revert -e` used to be accepted and
//! ignored — no editor, exit 0, whatever message the sequencer generated
//! committed as-is — and `cherry-pick -e` was refused outright with a sentence
//! git never prints. The delegation is observable well beyond the message text,
//! so this file pins the side effects too: the reflog wording, and the ` Date:`
//! line that the sequencer's own summary always carries but `git commit`'s only
//! adds when the author date is interesting.
//!
//! **`gpg.format`.** `git commit -S` resolved `gpg.program` and ran `gpg -bsa`
//! whatever the configured format was, so `gpg.format = ssh` fed an ssh *public
//! key* to gpg and died with `gpg: skipped …: No secret key`. The ssh backend is
//! `ssh-keygen -Y sign -n git -f <key>` and produces an `SSH SIGNATURE` block.
//!
//! Everything asserted here was measured against git 2.55.0 first. The editors
//! are scripts and stdin is `/dev/null`, so nothing is interactive; the ssh test
//! generates its own throwaway key and skips loudly when `ssh-keygen -Y` is
//! unavailable.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `base` → `third commit` on `main`, with a hermetic `HOME` so the
    /// developer's own `core.commentChar` / `core.editor` cannot reach the test.
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("zvcs-replayedit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(home.join("gitconfig"), "").unwrap();
        let f = Fixture {
            repo: repo.canonicalize().unwrap(),
            home: home.canonicalize().unwrap(),
            root: root.canonicalize().unwrap(),
        };
        f.ok(&["init", "-q", "-b", "main"]);
        std::fs::write(f.repo.join("f"), "one\n").unwrap();
        f.ok(&["add", "f"]);
        f.ok(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.repo.join("g"), "three\n").unwrap();
        f.ok(&["add", "g"]);
        f.ok(&["commit", "-q", "-m", "third commit"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            // `HOME` alone is not enough to fence off the developer's config:
            // pin the global and system files too, or a stray
            // `core.commentChar` silently rewrites every message assertion here.
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("ZVCS_HOME", &self.home)
            .env_remove("GIT_EDITOR")
            .env_remove("EDITOR")
            .env_remove("VISUAL")
            .env_remove("GIT_REFLOG_ACTION")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
            .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
            .stdin(std::process::Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Run with `GIT_EDITOR` bound to a shell snippet that gets the message path
    /// as `$1` — the same shape `launch_editor()` invokes a real editor with.
    fn run_edited(&self, editor: &str, args: &[&str]) -> Output {
        self.cmd(args).env("GIT_EDITOR", editor).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "git {args:?} failed ({}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn message(&self) -> String {
        self.ok(&["log", "-1", "--format=%B"])
    }

    fn subject(&self) -> String {
        self.ok(&["log", "-1", "--format=%s"]).trim().to_string()
    }

    fn head(&self) -> String {
        self.ok(&["rev-parse", "HEAD"]).trim().to_string()
    }
}

/// An editor that appends `marker` and succeeds.
fn appender(marker: &str) -> String {
    format!(r#"sh -c 'printf "{marker}\n" >> "$1"' _"#)
}

// ---------------------------------------------------------------------------
// -e / --edit
// ---------------------------------------------------------------------------

/// The core of the accepted-but-ignored bug: with `-e` the editor's edit has to
/// reach the commit object, and without it the editor must not run at all.
///
/// Stock, stdin redirected: `git revert -e HEAD` commits
/// `Revert "third commit"\n\nThis reverts commit <oid>.\n\nEDITED\n`, while
/// `git revert HEAD` commits the same message without the marker — `should_edit()`
/// (sequencer.c:2203-2212) returns 0 for an unspecified `--edit` when stdin is
/// not a tty.
#[test]
fn revert_edit_reaches_the_commit_message_and_no_edit_does_not() {
    let f = Fixture::new("revert-edit");

    let out = f.run_edited(&appender("EDITED"), &["revert", "-e", "HEAD"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let edited = f.message();
    assert!(
        edited.contains("EDITED"),
        "`revert -e` did not run the editor; message was:\n{edited}"
    );
    assert!(edited.starts_with("Revert \"third commit\""), "message: {edited}");

    f.ok(&["reset", "-q", "--hard", "HEAD~1"]);

    // Same editor, no `-e`: git's default for a revert is "edit only at a tty",
    // and this process has stdin on /dev/null.
    let out = f.run_edited(&appender("EDITED"), &["revert", "HEAD"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        !f.message().contains("EDITED"),
        "`revert` without -e ran the editor:\n{}",
        f.message()
    );

    f.ok(&["reset", "-q", "--hard", "HEAD~1"]);

    // `--no-edit` is the explicit form of the same answer.
    let out = f.run_edited(&appender("EDITED"), &["revert", "--no-edit", "HEAD"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!f.message().contains("EDITED"), "`--no-edit` ran the editor");
}

/// `cherry-pick -e` used to be a hard refusal ("`-e`/`--edit` (editor mode) is
/// not supported"), which is a sentence git never prints. It edits, and — unlike
/// revert — its *default* is never to edit, at a tty or not
/// (`opts->action == REPLAY_REVERT && isatty(0)`).
#[test]
fn cherry_pick_edit_is_accepted_and_edits() {
    let f = Fixture::new("cp-edit");
    let pick = f.head();
    f.ok(&["checkout", "-q", "-b", "side", "HEAD~1"]);

    let out = f.run_edited(&appender("CP-EDIT"), &["cherry-pick", "-e", &pick]);
    assert!(
        out.status.success(),
        "cherry-pick -e failed ({}):\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let msg = f.message();
    assert!(msg.starts_with("third commit"), "message: {msg}");
    assert!(msg.contains("CP-EDIT"), "cherry-pick -e did not edit:\n{msg}");

    f.ok(&["reset", "-q", "--hard", "HEAD~1"]);
    let out = f.run_edited(&appender("CP-EDIT"), &["cherry-pick", &pick]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        !f.message().contains("CP-EDIT"),
        "cherry-pick without -e ran the editor:\n{}",
        f.message()
    );
}

/// An editor that fails must abort the revert, leaving HEAD where it was. Stock
/// prints `error: there was a problem with the editor '<cmd>'` followed by
/// `Please supply the message using either -m or -F option.` and exits 1 —
/// `error()` plus `exit(1)` (editor.c:116, builtin/commit.c:1124-1127), not
/// `die()`, so it is neither `fatal:` nor 128.
#[test]
fn revert_edit_with_failing_editor_aborts_at_one() {
    let f = Fixture::new("revert-badeditor");
    let before = f.head();

    let out = f.run_edited("sh -c 'exit 3' _", &["revert", "-e", "HEAD"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("error: there was a problem with the editor"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("Please supply the message using either -m or -F option."),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("fatal:"), "editor failure must not wear die()'s voice: {stderr}");
    assert_eq!(f.head(), before, "a failed editor still moved HEAD");
}

/// An editor that empties the buffer aborts too: `EDIT_MSG` suppresses the
/// `--allow-empty-message` that `run_git_commit()` would otherwise pass
/// (sequencer.c:1177-1178).
#[test]
fn revert_edit_with_emptied_message_aborts() {
    let f = Fixture::new("revert-emptymsg");
    let before = f.head();

    let out = f.run_edited(r#"sh -c ': > "$1"' _"#, &["revert", "-e", "HEAD"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("Aborting commit due to empty commit message."),
        "stderr: {stderr}"
    );
    assert_eq!(f.head(), before, "an emptied message still moved HEAD");
}

/// The delegation is visible in two places that have nothing to do with the
/// message, and both are easy to lose by re-implementing the editor in-process
/// instead of calling `git commit`:
///
///   * the reflog still says `revert:`, because the child inherits
///     `GIT_REFLOG_ACTION=revert` (sequencer.c:1141) and `builtin/commit.c`
///     reads it before every fallback (builtin/commit.c:1850);
///   * the summary loses its ` Date:` line, because the sequencer's own
///     `print_commit_summary()` always passes `SUMMARY_SHOW_AUTHOR_DATE` while
///     `git commit`'s only prints it when `author_date_is_interesting()`, and a
///     revert reuses no author.
#[test]
fn revert_edit_keeps_the_reflog_action_and_drops_the_date_line() {
    let f = Fixture::new("revert-sideeffects");

    let edited = f.run_edited("true", &["revert", "-e", "HEAD"]);
    assert!(edited.status.success(), "stderr: {}", String::from_utf8_lossy(&edited.stderr));
    let edited_summary = String::from_utf8_lossy(&edited.stdout).into_owned();
    let reflog = f.ok(&["log", "-g", "-1", "--format=%gs"]).trim().to_string();
    assert_eq!(reflog, "revert: Revert \"third commit\"");
    assert!(
        !edited_summary.contains("\n Date:"),
        "`revert -e` summary must not carry the sequencer's Date line:\n{edited_summary}"
    );
    assert!(edited_summary.contains("] Revert \"third commit\""), "{edited_summary}");

    f.ok(&["reset", "-q", "--hard", "HEAD~1"]);

    let plain = f.run(&["revert", "HEAD"]);
    assert!(plain.status.success(), "stderr: {}", String::from_utf8_lossy(&plain.stderr));
    let plain_summary = String::from_utf8_lossy(&plain.stdout).into_owned();
    assert!(
        plain_summary.contains("\n Date:"),
        "a non-edited revert keeps the sequencer's Date line:\n{plain_summary}"
    );
}

/// `--reference` builds a message whose title is a *comment*, and whether that
/// comment survives is decided by the cleanup mode, not by `--reference`:
///
///   * bare `--reference` leaves it (nothing sets a cleanup mode, so the message
///     is committed verbatim);
///   * `--reference -e` drops it, because the delegated `git commit -e` resolves
///     its own `default` cleanup to `strip`;
///   * `--cleanup=default --reference` drops it with no editor at all, because
///     `get_cleanup_mode(cleanup_arg, 1)` passes the literal `1`
///     (builtin/revert.c:189).
///
/// The prefix is `comment_line_str`, so `core.commentChar` moves it.
#[test]
fn reference_title_follows_cleanup_and_comment_char() {
    let f = Fixture::new("revert-reference");
    const TITLE: &str = "*** SAY WHY WE ARE REVERTING ON THE TITLE LINE ***";

    f.ok(&["revert", "--reference", "HEAD"]);
    assert!(
        f.message().starts_with(&format!("# {TITLE}")),
        "bare --reference must keep the commented title:\n{}",
        f.message()
    );
    f.ok(&["reset", "-q", "--hard", "HEAD~1"]);

    let out = f.run_edited("true", &["revert", "--reference", "-e", "HEAD"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        f.message().starts_with("This reverts commit "),
        "--reference -e must strip the commented title:\n{}",
        f.message()
    );
    f.ok(&["reset", "-q", "--hard", "HEAD~1"]);

    f.ok(&["revert", "--cleanup=default", "--reference", "HEAD"]);
    assert!(
        f.message().starts_with("This reverts commit "),
        "--cleanup=default must strip the commented title without an editor:\n{}",
        f.message()
    );
    f.ok(&["reset", "-q", "--hard", "HEAD~1"]);

    f.ok(&["-c", "core.commentChar=%", "revert", "--reference", "HEAD"]);
    assert!(
        f.message().starts_with(&format!("% {TITLE}")),
        "the title prefix is comment_line_str, not a literal '#':\n{}",
        f.message()
    );
}

/// The same `get_cleanup_mode(arg, 1)` literal governs `cherry-pick`, where the
/// message being cleaned is the picked commit's own — so a body line starting
/// with the comment prefix is a case that really occurs.
///
/// Stock: picking a commit whose message is `subject line\n\n# a hash line\nbody\n`
/// keeps the hash line under `--cleanup=whitespace` and `--cleanup=verbatim`,
/// drops it under `--cleanup=strip` *and* `--cleanup=default`, and keeps it when
/// no `--cleanup` is given at all (nothing sets a mode, so the message is carried
/// across untouched).
#[test]
fn cherry_pick_cleanup_default_strips_comment_lines() {
    let f = Fixture::new("cp-cleanup");
    std::fs::write(f.repo.join("h"), "two\n").unwrap();
    f.ok(&["add", "h"]);
    let msg = f.root.join("msg");
    std::fs::write(&msg, "subject line\n\n# a hash line\nbody\n").unwrap();
    f.ok(&["commit", "-q", "--cleanup=verbatim", "-F", msg.to_str().unwrap()]);
    let pick = f.head();

    let case = |spec: Option<&str>| -> String {
        f.ok(&["checkout", "-q", "-B", "side", "main~1"]);
        let mut args = vec!["cherry-pick"];
        if let Some(s) = spec {
            args.push(s);
        }
        args.push(&pick);
        f.ok(&args);
        f.message()
    };

    assert!(case(None).contains("# a hash line"), "no --cleanup must carry the message across");
    assert!(
        case(Some("--cleanup=whitespace")).contains("# a hash line"),
        "whitespace must keep comment lines"
    );
    assert!(
        case(Some("--cleanup=verbatim")).contains("# a hash line"),
        "verbatim must keep comment lines"
    );
    assert!(
        !case(Some("--cleanup=strip")).contains("# a hash line"),
        "strip must drop comment lines"
    );
    assert!(
        !case(Some("--cleanup=default")).contains("# a hash line"),
        "default resolves through get_cleanup_mode(arg, 1) and must strip too"
    );
}

// ---------------------------------------------------------------------------
// core.commentChar = auto
// ---------------------------------------------------------------------------

/// `adjust_comment_line_char()` (builtin/commit.c:700-736) re-picks the comment
/// character against the message body under `core.commentChar = auto`, so a body
/// line that starts with `#` is text rather than a comment. Skipping it is silent
/// data loss: the `strip` cleanup an editor implies deletes the line and the
/// commit succeeds.
///
/// Measured against git 2.55.0 with `HOME`/`GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`
/// pinned — this machine's own `~/.gitconfig` sets `core.commentChar = |`, which
/// would otherwise stand in for the default and hide the whole bug.
#[test]
fn comment_char_auto_keeps_hash_body_lines() {
    let f = Fixture::new("commentchar-auto");
    let msg = f.root.join("msg");
    std::fs::write(&msg, "subject\n\n# note line\nreal tail\n").unwrap();
    let msg = msg.to_str().unwrap().to_string();

    let commit = |name: &str, cfg: &[&str]| {
        std::fs::write(f.repo.join(name), "x\n").unwrap();
        f.ok(&["add", name]);
        let mut args: Vec<&str> = cfg.to_vec();
        args.extend_from_slice(&["commit", "-q", "-F", &msg, "-e"]);
        let out = f.cmd(&args).env("GIT_EDITOR", "true").output().unwrap();
        assert!(
            out.status.success(),
            "commit failed ({}):\nstdout: {}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        f.message()
    };

    assert_eq!(
        commit("p1", &["-c", "core.commentChar=auto"]).trim_end().to_string(),
        "subject\n\n# note line\nreal tail",
        "auto must move the comment character off '#' and keep the body line"
    );
    // The default `#` really is a comment character, so the same body loses the
    // line — the contrast is what proves `auto` did something.
    assert_eq!(
        commit("p2", &[]).trim_end().to_string(),
        "subject\n\nreal tail",
        "with the default '#' the line is a comment and is stripped"
    );

    // The template the editor sees is commented with the *adjusted* character.
    std::fs::write(f.repo.join("f"), "seen\n").unwrap();
    f.ok(&["add", "f"]);
    let cap = f.root.join("cap");
    let ed = f.root.join("cap.sh");
    std::fs::write(&ed, format!("#!/bin/sh\ncp \"$1\" {}\n", cap.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&ed, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = f
        .cmd(&["-c", "core.commentChar=auto", "commit", "-q", "-F", &msg, "-e"])
        .env("GIT_EDITOR", ed.to_str().unwrap())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let seen = std::fs::read_to_string(&cap).unwrap();
    assert!(
        seen.contains("; Please enter the commit message"),
        "the template must use the adjusted character:\n{seen}"
    );
    assert!(seen.contains("\n# note line\n"), "the body line must survive into the template");
}

/// When the body starts a line with *every* candidate, git has nowhere to move
/// to and dies rather than eating one of them:
/// `die(_("unable to select a comment character that is not used\nin the current
/// commit message"))` (builtin/commit.c:732-733) — `fatal:` at 128.
#[test]
fn comment_char_auto_dies_when_no_candidate_is_free() {
    let f = Fixture::new("commentchar-exhausted");
    let msg = f.root.join("msg");
    // One line per candidate in `char candidates[] = "#;@!$%^&|:"`.
    std::fs::write(&msg, "subj\n#a\n;b\n@c\n!d\n$e\n%f\n^g\n&h\n|i\n:j\ntail\n").unwrap();
    std::fs::write(f.repo.join("f"), "x\n").unwrap();
    f.ok(&["add", "f"]);

    let before = f.head();
    let out = f
        .cmd(&["-c", "core.commentChar=auto", "commit", "-F", msg.to_str().unwrap(), "-e"])
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(128), "stderr: {err}");
    assert!(
        err.contains(
            "fatal: unable to select a comment character that is not used\n\
             in the current commit message"
        ),
        "stderr: {err}"
    );
    assert_eq!(f.head(), before, "the refusal must not have committed");
}

// ---------------------------------------------------------------------------
// git_editor()
// ---------------------------------------------------------------------------

/// `git_editor()` (editor.c:27-46) decides which editor `-e` launches, and three
/// of its rules were wrong here. All expectations measured against git 2.55.0
/// with `HOME`, `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` pinned.
///
///   * `getenv("GIT_EDITOR")` is non-NULL for an *empty* variable, so `GIT_EDITOR=`
///     selects the empty editor and fails at the exec instead of falling through
///     to `core.editor`;
///   * `$VISUAL` is consulted only when `TERM` is not dumb, while `$EDITOR` is
///     consulted either way;
///   * a dumb `TERM` is the only thing that makes git give up — whether stdin is
///     a terminal never enters into it, and this port used to refuse on that.
#[test]
fn git_editor_resolution_order() {
    let f = Fixture::new("editor-order");
    let ed = f.root.join("ed.sh");
    std::fs::write(&ed, "#!/bin/sh\nprintf 'EDITED\\n' >> \"$1\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&ed, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ed = ed.to_str().unwrap().to_string();

    let stage = |name: &str, body: &str| {
        std::fs::write(f.repo.join(name), body).unwrap();
        f.ok(&["add", name]);
    };

    // A dumb terminal with nothing configured is git's only give-up path.
    stage("a", "a\n");
    let out = f.cmd(&["commit"]).env("TERM", "dumb").output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("error: Terminal is dumb, but EDITOR unset"), "{err}");
    assert!(err.contains("Please supply the message using either -m or -F option."), "{err}");

    // `$VISUAL` is skipped on a dumb terminal, so this is the same refusal.
    let out = f.cmd(&["commit"]).env("TERM", "dumb").env("VISUAL", &ed).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "VISUAL must not be used on a dumb terminal: {err}");
    assert!(err.contains("Terminal is dumb, but EDITOR unset"), "{err}");

    // `$EDITOR` is not skipped on a dumb terminal.
    let out = f.cmd(&["commit", "-q"]).env("TERM", "dumb").env("EDITOR", &ed).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(f.subject(), "EDITED");

    // An empty `GIT_EDITOR` wins over `core.editor` and then fails to exec.
    stage("b", "b\n");
    f.ok(&["config", "core.editor", &ed]);
    let out = f
        .cmd(&["commit"])
        .env("TERM", "xterm")
        .env("GIT_EDITOR", "")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("error: cannot run : No such file or directory"), "{err}");
    assert!(err.contains("error: unable to start editor ''"), "{err}");

    // Same repo state, `GIT_EDITOR` gone: `core.editor` is used.
    let out = f.cmd(&["commit", "-q"]).env("TERM", "xterm").output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(f.subject(), "EDITED");
}

// ---------------------------------------------------------------------------
// hook ordering
// ---------------------------------------------------------------------------

/// ```c
/// run_commit_hook(use_editor, repo_get_index_file(...), NULL, "post-commit", NULL);
/// if (amend && !no_post_rewrite)
///         commit_post_rewrite(the_repository, current_head, &oid);
/// ```
///
/// (builtin/commit.c:1966-1970.) `post-commit` first, `post-rewrite amend`
/// second — this port ran them the other way round. Both hooks can observe the
/// repository the other left behind, so the order is part of the contract, and a
/// single append-only log file is enough to pin it.
#[test]
fn amend_runs_post_commit_before_post_rewrite() {
    let f = Fixture::new("hookorder");
    let hooks = f.repo.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for (name, body) in [
        ("post-commit", "#!/bin/sh\necho post-commit >> \"$PWD/hooklog\"\n"),
        (
            "post-rewrite",
            "#!/bin/sh\necho \"post-rewrite $1 [$(cat)]\" >> \"$PWD/hooklog\"\n",
        ),
    ] {
        let p = hooks.join(name);
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let log = f.repo.join("hooklog");

    let old = f.head();
    f.ok(&["commit", "-q", "--amend", "-m", "amended"]);
    let new = f.head();
    let seen = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        seen,
        format!("post-commit\npost-rewrite amend [{old} {new}]\n"),
        "hook log was:\n{seen}"
    );

    // `--no-post-rewrite` suppresses the second one and nothing else.
    std::fs::write(&log, "").unwrap();
    f.ok(&["commit", "-q", "--amend", "--no-post-rewrite", "-m", "again"]);
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "post-commit\n");
}

// ---------------------------------------------------------------------------
// gpg.format = ssh
// ---------------------------------------------------------------------------

/// `ssh-keygen -Y sign` arrived in OpenSSH 8.2p1; older builds print a usage
/// block instead. Probing beats version parsing.
fn ssh_signing_available() -> bool {
    let Ok(out) = Command::new("ssh-keygen").arg("-Y").output() else {
        return false;
    };
    // Without a sub-command `-Y` reports "missing namespace argument"-style
    // usage; an OpenSSH too old to know `-Y` reports "illegal option".
    let text = String::from_utf8_lossy(&out.stderr).to_lowercase();
    !text.contains("illegal option") && !text.contains("invalid option")
}

/// `git commit -S` under `gpg.format = ssh` must sign with `ssh-keygen`, not with
/// `gpg`. The regression it guards is silent in the worst way at the config
/// level and loud at the wrong one at runtime: `gpg.program` was resolved for
/// every format, so an ssh public key was handed to `gpg -bsa` and the commit
/// died with `gpg: skipped …: No secret key`.
///
/// The signature is verified with `ssh-keygen -Y verify` against an
/// `allowed_signers` file built from the same throwaway key, so nothing outside
/// the fixture is trusted or consulted.
#[test]
fn commit_dash_s_uses_the_ssh_backend_under_gpg_format_ssh() {
    if !ssh_signing_available() {
        eprintln!("SKIP commit_dash_s_uses_the_ssh_backend_under_gpg_format_ssh: \
                   ssh-keygen has no `-Y sign` (OpenSSH < 8.2p1) or is absent");
        return;
    }
    let f = Fixture::new("ssh-sign");
    let key = f.root.join("id");
    let keygen = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "zvcs-test", "-f"])
        .arg(&key)
        .status()
        .unwrap();
    if !keygen.success() {
        eprintln!("SKIP commit_dash_s_uses_the_ssh_backend_under_gpg_format_ssh: \
                   ssh-keygen could not generate an ed25519 key");
        return;
    }
    let pubkey = std::fs::read_to_string(f.root.join("id.pub")).unwrap();

    f.ok(&["config", "gpg.format", "ssh"]);
    f.ok(&["config", "user.signingKey", key.with_extension("pub").to_str().unwrap()]);

    std::fs::write(f.repo.join("h"), "signed\n").unwrap();
    f.ok(&["add", "h"]);
    let out = f.run(&["commit", "-S", "-m", "signed one"]);
    assert!(
        out.status.success(),
        "commit -S under gpg.format=ssh failed ({}):\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(f.subject(), "signed one");

    let raw = f.ok(&["cat-file", "commit", "HEAD"]);
    assert!(
        raw.contains("gpgsig -----BEGIN SSH SIGNATURE-----"),
        "expected an SSH SIGNATURE gpgsig header, got:\n{raw}"
    );
    assert!(
        !raw.contains("BEGIN PGP SIGNATURE"),
        "gpg.format=ssh must not produce a PGP signature:\n{raw}"
    );

    // A well-formed block is not enough — it has to verify over the payload git
    // signs, which is the commit object with the `gpgsig` header removed and its
    // continuation lines unfolded. `verify-commit` reconstructs exactly that and
    // hands it to `ssh-keygen -Y verify`; the principal file is built from the
    // same throwaway key, so nothing outside the fixture is trusted.
    let allowed = f.root.join("allowed_signers");
    std::fs::write(&allowed, format!("a@e.x {pubkey}")).unwrap();
    f.ok(&["config", "gpg.ssh.allowedSignersFile", allowed.to_str().unwrap()]);

    let verified = f.run(&["verify-commit", "HEAD"]);
    let report = String::from_utf8_lossy(&verified.stderr).into_owned();
    assert!(
        verified.status.success() && report.contains("Good \"git\" signature for a@e.x"),
        "the ssh signature did not verify ({}):\n{report}",
        verified.status
    );
    assert_eq!(f.ok(&["log", "-1", "--format=%G?"]).trim(), "G");
}
