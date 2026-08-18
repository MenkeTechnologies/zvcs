//! `git show` renders its commit header with `git log`'s formatter.
//!
//! `cmd_show` runs the same `cmd_log_init` as `cmd_log` and prints every record
//! through the same `show_log()`/`pretty_print_commit()` pair, so there is exactly
//! one set of pretty formats, one placeholder table, and one separator rule across
//! the two commands. These tests pin the cases where a second, private
//! implementation used to answer differently — `show --pretty=fuller` was refused
//! with git's own `fatal: invalid --pretty format` wording, `%cn`/`%ad`/`%D` were
//! rejected under `show` while `log` expanded them, and a `format:` (separator)
//! format terminated its last record instead of separating.
//!
//! Every expectation below was measured against stock git 2.55.0 on the fixture
//! this file builds, then pasted in verbatim. The fixture pins author/committer
//! identity and both timestamps, so the object ids and the rendered dates are
//! fixed and the assertions are byte-exact rather than shape-exact.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `1136214245 +0000` is `Mon Jan 2 15:04:05 2006 +0000`; the second commit is a
/// day later. Both are written in UTC, so the rendered dates carry no dependence
/// on the machine's zone.
const T0: &str = "1136214245 +0000";
const T1: &str = "1136300645 +0000";

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", T1)
        .env("GIT_COMMITTER_DATE", T1)
        .output()
        .unwrap()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn setup(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-showpretty-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    (repo, home)
}

/// Two commits touching one file, an annotated tag on the tip, and a note on it.
///
/// Object ids are deterministic given the pinned identities and timestamps:
/// `9928d08…` (first) and `a141364…` (second), which is what lets the assertions
/// below quote whole records.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (repo, home) = setup(tag);
    let git = |args: &[&str], date: &str| {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&repo)
            .env("HOME", &home)
            .env("ZVCS_HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .output()
            .unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {}",
            String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q", "-b", "main", "."], T0);
    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    git(&["add", "a.txt"], T0);
    git(
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "first commit\n\nBody line one."],
        T0,
    );
    std::fs::write(repo.join("a.txt"), "one\n2\nthree\n").unwrap();
    git(&["add", "a.txt"], T1);
    git(
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "second commit\n\nSecond body."],
        T1,
    );
    git(&["tag", "-a", "v1", "-m", "tag message"], T1);
    git(&["notes", "add", "-m", "a note here", "HEAD"], T1);
    (repo, home)
}

const HEAD_OID: &str = "a1413646a2aea391b106f94719e040ed426233ff";
const PARENT_OID: &str = "9928d08ff1fde8e440ba8b022afda6392d6265c2";

/// The five built-in formats `show` used to reject outright with
/// `fatal: invalid --pretty format: <name>` — git's own wording for a value git
/// itself accepts, which read as user error rather than as a porting gap.
#[test]
fn show_renders_every_builtin_pretty_name() {
    let (repo, home) = fixture("names");

    assert_eq!(
        ok(&repo, &home, &["show", "--no-patch", "--pretty=short", "HEAD"]),
        "commit a1413646a2aea391b106f94719e040ed426233ff\n\
         Author: A U Thor <author@example.com>\n\
         \n    second commit\n"
    );
    assert_eq!(
        ok(&repo, &home, &["show", "--no-patch", "--pretty=full", "HEAD"]),
        "commit a1413646a2aea391b106f94719e040ed426233ff\n\
         Author: A U Thor <author@example.com>\n\
         Commit: C O Mitter <committer@example.com>\n\
         \n    second commit\n    \n    Second body.\n"
    );
    // `fuller` is the one that also proves the identity columns are padded to the
    // `AuthorDate:`/`CommitDate:` width, which `medium` never exercises.
    assert_eq!(
        ok(&repo, &home, &["show", "--no-patch", "--pretty=fuller", "HEAD"]),
        "commit a1413646a2aea391b106f94719e040ed426233ff\n\
         Author:     A U Thor <author@example.com>\n\
         AuthorDate: Tue Jan 3 15:04:05 2006 +0000\n\
         Commit:     C O Mitter <committer@example.com>\n\
         CommitDate: Tue Jan 3 15:04:05 2006 +0000\n\
         \n    second commit\n    \n    Second body.\n"
    );
    // `raw` copies the object's own header lines through, so it also pins that the
    // `parent`/`author`/`committer` lines come from the stored bytes.
    assert_eq!(
        ok(&repo, &home, &["show", "--no-patch", "--pretty=raw", "HEAD"]),
        format!(
            "commit {HEAD_OID}\n\
             tree dd519e9d1c7b0cf2698a38a449a25ea626e68657\n\
             parent {PARENT_OID}\n\
             author A U Thor <author@example.com> 1136300645 +0000\n\
             committer C O Mitter <committer@example.com> 1136300645 +0000\n\
             \n    second commit\n    \n    Second body.\n"
        )
    );
    // `reference` is `%C(auto)%h (%s, %ad)` with `--date=short` forced on.
    assert_eq!(
        ok(&repo, &home, &["show", "--no-patch", "--pretty=reference", "HEAD"]),
        "a141364 (second commit, 2006-01-03)\n"
    );
}

/// The placeholders `log` expanded and `show` rejected. `%cn`/`%cd` were the whole
/// committer family, `%d`/`%D` the decorations, and `%C(...)` the colour requests
/// that render as nothing when output is not a terminal.
#[test]
fn show_expands_the_placeholders_log_expands() {
    let (repo, home) = fixture("placeholders");
    let got = ok(
        &repo,
        &home,
        &["show", "--no-patch", "--pretty=format:%h|%an|%cn|%ad|%cd|%D|%s", "HEAD"],
    );
    assert_eq!(
        got,
        "a141364|A U Thor|C O Mitter|Tue Jan 3 15:04:05 2006 +0000\
         |Tue Jan 3 15:04:05 2006 +0000|HEAD -> main, tag: v1|second commit"
    );

    // The two commands must agree byte for byte, since they now share the engine.
    let via_log = ok(
        &repo,
        &home,
        &["log", "--no-walk", "--no-patch", "--pretty=format:%h|%an|%cn|%ad|%cd|%D|%s", "HEAD"],
    );
    assert_eq!(got, via_log);
}

/// `get_commit_format` answers `show_log()`'s `use_terminator` as well as the
/// format: `format:` *separates* records and `tformat:`/`--format=` *terminates*
/// them (log-tree.c:776-793, 915-919). `show` used to terminate both, so a
/// `format:` series carried a trailing newline stock does not write.
#[test]
fn separator_and_terminator_formats_end_records_differently() {
    let (repo, home) = fixture("terminator");
    assert_eq!(
        ok(&repo, &home, &["show", "--no-patch", "--pretty=format:%H", "HEAD", "HEAD~1"]),
        format!("{HEAD_OID}\n{PARENT_OID}")
    );
    assert_eq!(
        ok(&repo, &home, &["show", "--no-patch", "--pretty=tformat:%H", "HEAD", "HEAD~1"]),
        format!("{HEAD_OID}\n{PARENT_OID}\n")
    );
}

/// `show_tag_object()` writes the object's remainder verbatim and then sets
/// `rev.shown_one`, so the blank line between a tag and the object it points at is
/// the *record separator* — which a terminator format does not have. Adding one
/// unconditionally left `git show --pretty=oneline <tag>` with a stray blank line.
///
/// `fuller` additionally pins `pp_user_info()`'s `Tagger:` padding and the
/// `%sDate: ` arm, whose `what` is `Tagger` (pretty.c:591-593, 614-617).
#[test]
fn annotated_tag_header_follows_the_pretty_format() {
    let (repo, home) = fixture("tag");
    assert_eq!(
        ok(&repo, &home, &["show", "--no-patch", "--pretty=oneline", "v1"]),
        format!("tag v1\n\ntag message\n{HEAD_OID} second commit\n")
    );
    assert_eq!(
        ok(&repo, &home, &["show", "--no-patch", "--pretty=fuller", "v1"]),
        "tag v1\n\
         Tagger:     C O Mitter <committer@example.com>\n\
         TaggerDate: Tue Jan 3 15:04:05 2006 +0000\n\
         \n\
         tag message\n\
         \n\
         commit a1413646a2aea391b106f94719e040ed426233ff\n\
         Author:     A U Thor <author@example.com>\n\
         AuthorDate: Tue Jan 3 15:04:05 2006 +0000\n\
         Commit:     C O Mitter <committer@example.com>\n\
         CommitDate: Tue Jan 3 15:04:05 2006 +0000\n\
         \n    second commit\n    \n    Second body.\n"
    );
}

/// `show_log()` prints one `commit <name>` line for every non-mail, non-user
/// format (log-tree.c:810-834), `raw` included — so `--abbrev-commit` shortens it
/// and `--decorate` decorates it there too. The renderer used to special-case
/// `raw` into a full, undecorated id.
#[test]
fn raw_header_honours_abbrev_commit_and_decorate() {
    let (repo, home) = fixture("rawhdr");
    let expected = format!(
        "commit a141364 (HEAD -> main, tag: v1)\n\
         tree dd519e9d1c7b0cf2698a38a449a25ea626e68657\n\
         parent {PARENT_OID}\n\
         author A U Thor <author@example.com> 1136300645 +0000\n\
         committer C O Mitter <committer@example.com> 1136300645 +0000\n\
         \n    second commit\n    \n    Second body.\n"
    );
    assert_eq!(
        ok(
            &repo,
            &home,
            &["log", "--no-walk", "--pretty=raw", "--abbrev-commit", "--decorate", "HEAD"]
        ),
        expected
    );
    assert_eq!(
        ok(
            &repo,
            &home,
            &["show", "--no-patch", "--pretty=raw", "--abbrev-commit", "--decorate", "HEAD"]
        ),
        expected
    );
}

/// ```c
/// case 'N':
///         if (c->pretty_ctx->notes_message) { … return 1; }
///         return 0;
/// ```
///
/// (pretty.c:1650-1655.) `show_log()` fills `notes_message` only under
/// `opt->show_notes`, so with notes off `%N` consumes nothing and prints
/// literally — which is a different answer from a commit that simply has no note.
#[test]
fn percent_n_is_literal_when_notes_are_off() {
    let (repo, home) = fixture("notesph");
    assert_eq!(
        ok(&repo, &home, &["log", "--no-walk", "--pretty=format:[%N]", "--no-notes", "HEAD"]),
        "[%N]"
    );
    assert_eq!(
        ok(&repo, &home, &["log", "--no-walk", "--pretty=format:[%N]", "--notes", "HEAD"]),
        "[a note here\n]"
    );
    assert_eq!(
        ok(&repo, &home, &["show", "--no-patch", "--pretty=format:[%N]", "--no-notes", "HEAD"]),
        "[%N]"
    );
}

/// `pretty_print_commit()`'s notes tail runs for every non-user format, and the
/// mail formats fence it with `next_commentary_block()`'s `---` because everything
/// past that line is commentary a patch applier drops (log-tree.c:893-898).
/// Raising `opt->shown_dashes` there is also what stops the `--stat`-plus-`-p`
/// pair from writing a second `---` (log-tree.c:965-968).
#[test]
fn mail_formats_fence_their_notes_block() {
    let (repo, home) = fixture("mailnotes");
    assert_eq!(
        ok(&repo, &home, &["show", "--no-patch", "--pretty=email", "--notes", "HEAD"]),
        format!(
            "From {HEAD_OID} Mon Sep 17 00:00:00 2001\n\
             From: A U Thor <author@example.com>\n\
             Date: Tue, 3 Jan 2006 15:04:05 +0000\n\
             Subject: [PATCH] second commit\n\
             \nSecond body.\n---\n\nNotes:\n    a note here\n"
        )
    );
    assert_eq!(
        ok(
            &repo,
            &home,
            &["show", "--pretty=email", "--notes", "--patch-with-stat", "HEAD"]
        ),
        format!(
            "From {HEAD_OID} Mon Sep 17 00:00:00 2001\n\
             From: A U Thor <author@example.com>\n\
             Date: Tue, 3 Jan 2006 15:04:05 +0000\n\
             Subject: [PATCH] second commit\n\
             \nSecond body.\n---\n\nNotes:\n    a note here\n\
             \n a.txt | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n\n\
             diff --git a/a.txt b/a.txt\n\
             index 4cb29ea..f04eb26 100644\n\
             --- a/a.txt\n+++ b/a.txt\n\
             @@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"
        )
    );
}

/// `-S`/`-G`/`-l`/`-O` require a value and `parse_short_opt()` takes it from the
/// next argv slot; `-M`/`-C`/`-B` take an optional *attached* one and leave the
/// next word to `setup_revisions()`. Reading `-S <string>` as a bare flag made
/// `format-patch -S base` die on `base` as if it were a revision.
#[test]
fn format_patch_short_diff_options_take_their_value() {
    let (repo, home) = fixture("fpshort");

    // Missing value: `opterror()` + exit 129, one line and no usage block.
    let out = run(&repo, &home, &["format-patch", "--stdout", "-S"]);
    assert_eq!(out.status.code(), Some(129));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "error: switch `S' requires a value\n");
    let out = run(&repo, &home, &["format-patch", "--stdout", "-l"]);
    assert_eq!(out.status.code(), Some(129));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "error: switch `l' requires a value\n");

    // `-M` does not eat the next word, so git resolves it as a revision and dies.
    let out = run(&repo, &home, &["format-patch", "--stdout", "-M", "5", "-1"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: ambiguous argument '5': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    );

    // `-S` does eat it. The pickaxe itself is not ported, so what must NOT happen
    // is the value being mistaken for a revision; the refusal has to name `-S`.
    let out = run(&repo, &home, &["format-patch", "--stdout", "-S", "base", "-1"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("ambiguous argument 'base'"), "value leaked into revisions: {err}");
    assert!(err.contains("-S"), "refusal should name the option it could not honour: {err}");
}

/// A repo whose second commit renames twelve modified files, so
/// `too_many_rename_candidates()` trips under `diff.renameLimit=1`.
fn rename_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (repo, home) = setup(tag);
    let git = |args: &[&str], date: &str| {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&repo)
            .env("HOME", &home)
            .env("ZVCS_HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .output()
            .unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {}",
            String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q", "-b", "main", "."], T0);
    for i in 1..=12 {
        std::fs::write(
            repo.join(format!("old{i}.txt")),
            format!("content {i}\nsecond {i}\nthird {i}\n"),
        )
        .unwrap();
    }
    git(&["add", "-A"], T0);
    git(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "base"], T0);
    for i in 1..=12 {
        let old = repo.join(format!("old{i}.txt"));
        let new = repo.join(format!("new{i}.txt"));
        std::fs::rename(&old, &new).unwrap();
        let mut body = std::fs::read_to_string(&new).unwrap();
        body.push_str(&format!("extra {i}\n"));
        std::fs::write(&new, body).unwrap();
    }
    git(&["add", "-A"], T1);
    git(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "renames"], T1);
    (repo, home)
}

/// `diff_result_code()` closes every command that returns one with
/// `diff_warn_rename_limit()` (diff.c:7546-7548), so a run whose rename detection
/// was cut short says so on stderr. The pass ran and the patch already matched
/// stock; only the warning was being dropped on the floor.
#[test]
fn rename_limit_warning_reaches_show_and_log() {
    let (repo, home) = rename_fixture("renamewarn");
    let expected = "warning: exhaustive rename detection was skipped due to too many files.\n\
         warning: you may want to set your diff.renameLimit variable to at least 12 \
         and retry the command.\n";

    for args in [
        ["-c", "diff.renameLimit=1", "show", "--stat"].as_slice(),
        ["-c", "diff.renameLimit=1", "log", "--stat"].as_slice(),
        ["-c", "diff.renameLimit=1", "log", "--name-status"].as_slice(),
    ] {
        let out = run(&repo, &home, args);
        assert_eq!(String::from_utf8_lossy(&out.stderr), expected, "for {args:?}");
    }

    // Under the default limit nothing is cut short, so nothing is said.
    let out = run(&repo, &home, &["show", "--stat"]);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}
