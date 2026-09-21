//! `git commit`'s message-source options: how they conflict, and how they resolve
//! the commit they name.
//!
//! Three separate things are pinned here, all of them measured against stock git
//! 2.55.0.
//!
//! **`-C` and `-c` are two slots, not one.** git declares them separately
//!
//! ```c
//! static char *use_message_buffer;
//! static const char *use_message, *edit_message, *logfile;
//! ```
//!
//! (builtin/commit.c:121-123) and weighs all four message sources against each
//! other before any of them is resolved:
//!
//! ```c
//! die_for_incompatible_opt4(!!use_message, "-C",
//!                           !!edit_message, "-c",
//!                           !!logfile, "-F",
//!                           !!fixup_message, "--fixup");
//! ```
//!
//! (builtin/commit.c:1341-1344.) Only afterwards are they folded together, by
//! `if (edit_message) use_message = edit_message;` (:1351-1352). Collapsing them
//! into one variable at parse time loses two behaviours: `-C x -c y` stops being a
//! conflict, and `--no-reedit-message` starts discarding a `-C` (and
//! `--no-reuse-message` a `-c`), because each `OPT_STRING`'s unset arm writes NULL
//! over *its own* slot only (parse-options.c:200-202).
//!
//! **`--fixup` and `--squash` resolve their argument the same way `-C` does.** All
//! three go through `lookup_commit_reference_by_name()` (commit.c:109-126) and die
//! with the same sentence:
//!
//! ```c
//! commit = lookup_commit_reference_by_name(fixup_commit);
//! if (!commit)
//!         die(_("could not lookup commit '%s'"), fixup_commit);
//! ```
//!
//! (builtin/commit.c:829-831; :794-796 for `--squash`.) That function peels tags,
//! so `--fixup=<annotated tag>` names the tagged commit, and it runs
//! `lookup_commit_reference_gently()` with `quiet = 0`, so an object that is not a
//! commit gets `object_as_type()`'s `error: object %s is a %s, not a %s` line
//! *before* the `fatal:`. An internal resolver error printed in its place is a
//! different sentence, a different exit code, and a leaked source path.
//!
//! **`--long` has no NUL-delimited spelling.** `finalize_deferred_config()` turns
//! an unset format into porcelain under `-z` but refuses an explicitly long one:
//!
//! ```c
//! if (s->null_termination) {
//!         if (status_format == STATUS_FORMAT_NONE ||
//!             status_format == STATUS_FORMAT_UNSPECIFIED)
//!                 status_format = STATUS_FORMAT_PORCELAIN;
//!         else if (status_format == STATUS_FORMAT_LONG)
//!                 die(_("options '%s' and '%s' cannot be used together"), "--long", "-z");
//! }
//! ```
//!
//! (builtin/commit.c:1262-1268.)
//!
//! Every expectation is written out literally, so the file is a real check on a
//! binary with no stock git present. Nothing needs gpg, a network, or an editor
//! the user can see: the one editor case is a shell script.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A filesystem- and shell-safe stand-in for the thread id, so the scratch
/// repository path holds no parentheses — the editor scripts below are invoked
/// through `sh -c`, where an unquoted `(` is a syntax error.
fn thread_slug() -> String {
    format!("{:?}", std::thread::current().id())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_remove("GIT_REFLOG_ACTION")
        .env_remove("EDITOR")
        .env_remove("VISUAL")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn ok(cwd: &Path, args: &[&str]) -> String {
    let out = run(cwd, args);
    assert!(
        out.status.success(),
        "`git {args:?}` failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// A repository with one commit whose message has a subject and a body, so a
/// reused message is distinguishable from a rewritten one.
fn repo(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "zvcs-commitmsgsrc-{tag}-{}-{}",
        std::process::id(),
        thread_slug()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    let p = p.canonicalize().unwrap();
    ok(&p, &["init", "-q", "--initial-branch=main", "."]);
    std::fs::write(p.join("a.t"), "a\n").unwrap();
    ok(&p, &["add", "a.t"]);
    ok(&p, &["commit", "-q", "-m", "base subject\n\nbase body"]);
    p
}

/// A `GIT_EDITOR` script that copies the buffer it was handed to `seen.txt` and
/// then replaces it with `text`, so both what git seeded and what came back can
/// be checked. No interactive editor is ever involved.
fn editor_writing(repo: &Path, text: &str) -> String {
    let path = repo.join("ed.sh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ncat \"$1\" > \"$(dirname \"$0\")/seen.txt\"\nprintf '{text}\\n' > \"$1\"\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.display().to_string()
}

// ---------------------------------------------------------------------------
// -C and -c are separate slots
// ---------------------------------------------------------------------------

/// `-C` together with `-c` is the conflict `die_for_incompatible_opt4()` names
/// first, in the order the table gives them — so the sentence says `'-C' and
/// '-c'` whichever order they were typed in.
#[test]
fn reuse_message_and_reedit_message_together_are_fatal() {
    let r = repo("both");
    for args in [
        ["commit", "--allow-empty", "-C", "HEAD", "-c", "HEAD"],
        ["commit", "--allow-empty", "-c", "HEAD", "-C", "HEAD"],
    ] {
        let out = run(&r, &args);
        assert_eq!(
            stderr(&out),
            "fatal: options '-C' and '-c' cannot be used together\n",
            "{args:?}"
        );
        assert_eq!(code(&out), 128, "{args:?}");
    }
    assert_eq!(ok(&r, &["rev-list", "--count", "HEAD"]), "1");
}

/// `--no-reedit-message` unsets `edit_message`; it must leave a `-C` alone, which
/// is then still a conflict with `-m`.
#[test]
fn no_reedit_message_does_not_clear_reuse_message() {
    let r = repo("nore");
    let out = run(
        &r,
        &[
            "commit",
            "--allow-empty",
            "-C",
            "HEAD",
            "--no-reedit-message",
            "-m",
            "ignored",
        ],
    );
    assert_eq!(
        stderr(&out),
        "fatal: options '-m' and '-C' cannot be used together\n"
    );
    assert_eq!(code(&out), 128);
}

/// The mirror image: `--no-reuse-message` must leave a `-c` alone, so the editor
/// still opens on the reused message.
#[test]
fn no_reuse_message_does_not_clear_reedit_message() {
    let r = repo("noc");
    let ed = editor_writing(&r, "reworded by editor");
    let out = Command::new(BIN)
        .args([
            "commit",
            "--allow-empty",
            "-c",
            "HEAD",
            "--no-reuse-message",
        ])
        .current_dir(&r)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_EDITOR", &ed)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    // The `-c` survived, so the editor opened on HEAD's message. Checking only
    // what came *back* would pass either way — the editor writes the same thing
    // whether or not it was seeded — so the seeded buffer is what is asserted.
    let seeded = std::fs::read_to_string(r.join("seen.txt")).unwrap();
    assert!(
        seeded.starts_with("base subject\n\nbase body\n"),
        "editor was not seeded with the reused message: {seeded:?}"
    );
    assert_eq!(ok(&r, &["log", "-1", "--format=%B"]), "reworded by editor");
    assert_eq!(ok(&r, &["rev-list", "--count", "HEAD"]), "2");
}

/// `--no-reuse-message` after `-C` really does clear it: with nothing else to say,
/// the commit falls back to the editor rather than to HEAD's message.
#[test]
fn no_reuse_message_clears_its_own_slot() {
    let r = repo("clear");
    let ed = editor_writing(&r, "typed fresh");
    let out = Command::new(BIN)
        .args(["commit", "--allow-empty", "-C", "HEAD", "--no-reuse-message"])
        .current_dir(&r)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_EDITOR", &ed)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let seeded = std::fs::read_to_string(r.join("seen.txt")).unwrap();
    assert!(
        !seeded.contains("base subject"),
        "--no-reuse-message left the -C message in the buffer: {seeded:?}"
    );
    assert_eq!(ok(&r, &["log", "-1", "--format=%B"]), "typed fresh");
}

// ---------------------------------------------------------------------------
// --fixup / --squash resolve like -C
// ---------------------------------------------------------------------------

/// A name that resolves to nothing: one `fatal:` sentence, exit 128, and nothing
/// about the resolver that failed.
#[test]
fn fixup_and_squash_report_an_unresolvable_name_like_git() {
    let r = repo("nosuch");
    for (flag, spelling) in [
        ("--fixup=nosuch", "nosuch"),
        ("--squash=nosuch", "nosuch"),
        ("--fixup=amend:nosuch", "nosuch"),
        ("--fixup=reword:nosuch", "nosuch"),
    ] {
        let out = run(&r, &["commit", "--allow-empty", flag]);
        assert_eq!(
            stderr(&out),
            format!("fatal: could not lookup commit '{spelling}'\n"),
            "{flag}"
        );
        assert_eq!(code(&out), 128, "{flag}");
    }
}

/// An object that exists but is not a commit: `object_as_type()` speaks first,
/// then the `die()`. Two lines, in that order, exit 128.
#[test]
fn fixup_and_squash_report_a_non_commit_object_in_two_lines() {
    let r = repo("blob");
    let blob = ok(&r, &["rev-parse", "HEAD:a.t"]);
    for flag in [format!("--fixup={blob}"), format!("--squash={blob}")] {
        let out = run(&r, &["commit", "--allow-empty", &flag]);
        assert_eq!(
            stderr(&out),
            format!(
                "error: object {blob} is a blob, not a commit\n\
                 fatal: could not lookup commit '{blob}'\n"
            ),
            "{flag}"
        );
        assert_eq!(code(&out), 128, "{flag}");
    }
}

/// The lookup peels, so an annotated tag names the commit it points at and the
/// autosquash subject is that commit's.
#[test]
fn fixup_peels_an_annotated_tag_to_its_commit() {
    let r = repo("peel");
    ok(&r, &["tag", "-a", "-m", "tag message", "v1", "HEAD"]);
    let out = run(&r, &["commit", "--allow-empty", "--fixup=v1"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(
        ok(&r, &["log", "-1", "--format=%B"]),
        "fixup! base subject"
    );
    let out = run(&r, &["commit", "--allow-empty", "--squash=v1"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(
        ok(&r, &["log", "-1", "--format=%s"]),
        "squash! base subject"
    );
}

// ---------------------------------------------------------------------------
// --long has no -z spelling
// ---------------------------------------------------------------------------

/// `--long` with `-z` is fatal; `--short` and `--porcelain` with `-z` are not, and
/// a bare `-z` promotes the unset format to porcelain rather than dying.
#[test]
fn long_with_nul_termination_is_fatal_while_the_others_pass() {
    let r = repo("z");
    std::fs::write(r.join("b.t"), "b\n").unwrap();
    ok(&r, &["add", "b.t"]);

    for args in [
        ["commit", "--dry-run", "--long", "-z"],
        ["commit", "--dry-run", "-z", "--long"],
    ] {
        let out = run(&r, &args);
        assert_eq!(
            stderr(&out),
            "fatal: options '--long' and '-z' cannot be used together\n",
            "{args:?}"
        );
        assert_eq!(code(&out), 128, "{args:?}");
    }

    // `-z` alone: porcelain records, NUL-terminated, exit 0.
    let out = run(&r, &["commit", "--dry-run", "-z"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(out.stdout, b"A  b.t\0");

    let out = run(&r, &["commit", "--dry-run", "-z", "--porcelain"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(out.stdout, b"A  b.t\0");

    let out = run(&r, &["commit", "--dry-run", "-z", "--short"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(out.stdout, b"A  b.t\0");
}
