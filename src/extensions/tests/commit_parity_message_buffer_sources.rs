//! Four places `git commit` shapes its message differently from what zvcs did.
//!
//! * `--squash` only *prefixes* the buffer (builtin/commit.c:784-799). Without
//!   `-m`/`-F`/`-c`/`-C`/`--amend` the `else if` chain below it still runs, so a
//!   concluded merge or cherry-pick keeps its `MERGE_MSG` (with its `# Conflicts:`
//!   block in the editor) and a configured template follows the subject
//!   (:855-887). zvcs dropped all three.
//! * `--trailer` goes through `amend_file_with_trailers()` (trailer.c:1339-1364),
//!   which reads `COMMIT_EDITMSG` with no `strbuf_complete_line()`: a verbatim
//!   `-F` message with no final newline gets its trailer on the very next line,
//!   not after a blank one as the `interpret-trailers` builtin would put it. `git
//!   tag` newline-terminates its message first (builtin/tag.c:330-333), so there
//!   the blank line stays.
//! * `--allow-empty-message` picks the editor hints that do not promise an abort
//!   on an empty message (builtin/commit.c:946-959).
//! * `--date=<approxidate>` is `@<seconds>` with no zone (`parse_force_date()`,
//!   builtin/commit.c:613-627), which `fmt_ident()` stamps with the local offset
//!   of that moment (date.c:933-943); zvcs stamped `+0000`.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical
//! repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

const HEAD_RAW: &[&str] = &["cat-file", "commit", "HEAD"];

/// Copy the editor's buffer to `<root>/editbuf`, leaving the file as it was.
fn dump_editor(side: &twin_repo::Side) -> String {
    let script = side.root.join("dump-editor");
    std::fs::write(&script, format!("#!/bin/sh\ncat \"$1\" > '{}/editbuf'\n", side.root.display())).unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    script.to_string_lossy().into_owned()
}

fn editbuf(side: &twin_repo::Side) -> String {
    std::fs::read_to_string(side.root.join("editbuf")).unwrap_or_default()
}

#[test]
fn squash_keeps_the_merge_and_cherry_pick_messages_and_the_template() {
    let Some(stock) = stock_git::stock_git() else { return };
    for (label, setup) in [
        ("squash-merge", &["merge", "side"][..]),
        ("squash-pick", &["cherry-pick", "side"][..]),
        ("squash-template", &["config", "commit.template", "tmpl"][..]),
    ] {
        let (s, z) = twin_repo::pair(label, stock);
        for side in [&s, &z] {
            side.write("tmpl", "template line\n\n# template comment\n");
            side.git(setup);
            side.write("a", "resolved\n");
            side.git(&["add", "a"]);
        }
        for args in [&["commit", "--squash=HEAD~1"][..], &["commit", "--squash=HEAD~1", "--cleanup=verbatim"]] {
            let (ws, wz) = (dump_editor(&s), dump_editor(&z));
            let want = s.git_env(&[("GIT_EDITOR", &ws)], args);
            assert_eq!(z.git_env(&[("GIT_EDITOR", &wz)], args), want, "{label} {args:?}");
            assert_eq!(editbuf(&z), editbuf(&s), "{label} {args:?}");
            assert_eq!(z.git(HEAD_RAW), s.git(HEAD_RAW), "{label} {args:?}");
            s.git(&["reset", "-q", "--soft", "HEAD~1"]);
            z.git(&["reset", "-q", "--soft", "HEAD~1"]);
        }
    }
}

#[test]
fn a_trailer_follows_an_unterminated_verbatim_message_directly() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("trailer-unterminated", stock);
    for side in [&s, &z] {
        side.write("nonl", "subject only");
    }
    let commit = ["commit", "--allow-empty", "-F", "nonl", "--cleanup=verbatim", "--trailer", "X: y"];
    assert_eq!(z.git(&commit), s.git(&commit));
    assert_eq!(z.git(HEAD_RAW), s.git(HEAD_RAW));
    let tag = ["tag", "-a", "-F", "nonl", "--trailer", "X: y", "t1"];
    assert_eq!(z.git(&tag), s.git(&tag));
    let show = ["cat-file", "tag", "t1"];
    assert_eq!(z.git(&show), s.git(&show));
}

#[test]
fn allow_empty_message_drops_the_abort_promise_from_the_hints() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("allow-empty-message-hint", stock);
    for cleanup in ["--cleanup=strip", "--cleanup=whitespace", "--cleanup=verbatim"] {
        let args = ["commit", "--allow-empty", "--allow-empty-message", cleanup];
        let (ws, wz) = (dump_editor(&s), dump_editor(&z));
        let want = s.git_env(&[("GIT_EDITOR", &ws)], &args);
        assert_eq!(z.git_env(&[("GIT_EDITOR", &wz)], &args), want, "{cleanup}");
        assert_eq!(editbuf(&z), editbuf(&s), "{cleanup}");
        assert_eq!(z.git(HEAD_RAW), s.git(HEAD_RAW), "{cleanup}");
    }
}

#[test]
fn an_approxidate_date_takes_the_local_offset() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("force-date-offset", stock);
    for tz in ["Asia/Tokyo", "America/New_York", "UTC"] {
        let env = [("TZ", tz), ("GIT_TEST_DATE_NOW", "1700000000")];
        for date in ["1 day ago", "yesterday noon", "@1600000000"] {
            let args = ["commit", "--allow-empty", "-q", "-m", "x", "--date", date];
            assert_eq!(z.git_env(&env, &args), s.git_env(&env, &args), "{tz} {date}");
            assert_eq!(z.git(HEAD_RAW), s.git(HEAD_RAW), "{tz} {date}");
        }
    }
}
