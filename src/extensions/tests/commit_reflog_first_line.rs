//! What a commit writes into the reflog, as opposed to what it shows as `%s`.
//!
//! git builds the two from the same message by two different rules, and only a
//! *continuation* line — one newline with no blank line after it — separates them:
//!
//! ```c
//! nl = strchr(msg->buf, '\n');
//! if (nl) {
//!         strbuf_add(&sb, msg->buf, nl + 1 - msg->buf);
//! } else {
//!         strbuf_addbuf(&sb, msg);
//!         strbuf_addch(&sb, '\n');
//! }
//! ```
//!
//! (`update_head_with_reflog()`, sequencer.c:1259-1295, which is what
//! builtin/commit.c:1945 calls for every commit it records.) So the reflog gets the
//! **first line**, while `print_commit_summary()`'s `%s` is `format_subject(sb,
//! msg, " ")` — the whole first paragraph folded onto one line. `line one\nline
//! two` is therefore `line one line two` as a subject and `commit: line one` in the
//! reflog, and reusing the folded subject for both put the second line in every
//! reflog entry.
//!
//! The composed string then goes through `copy_reflog_msg()` (refs.c:1031-1045),
//! which `ref_transaction_add_update()` applies to *every* reflog message
//! (refs.c:1342): each run of whitespace becomes one space and both ends are
//! trimmed. That is what turns a tabbed subject into single spaces, and what leaves
//! a bare `commit:` — with no trailing space — for a message with no first line.
//!
//! Every expectation below is written out literally, so the file fails on a broken
//! binary with no stock git present; where stock git *is* present, one test
//! additionally cross-checks it. Nothing here needs a daemon or a network.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

// ---------------------------------------------------------------------------
// process plumbing
// ---------------------------------------------------------------------------

/// Run `bin` in `cwd` with an isolated, deterministic environment, plus `env`.
fn run_env(bin: &str, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .env_remove("GIT_REFLOG_ACTION")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "zvcs-test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "zvcs-test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00+0000")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00+0000")
        .env("LC_ALL", "C");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap_or_else(|e| panic!("{bin} {args:?}: {e}"))
}

fn run(bin: &str, cwd: &Path, args: &[&str]) -> Output {
    run_env(bin, cwd, args, &[])
}

/// [`run`] asserting success and returning trimmed stdout.
fn ok(bin: &str, cwd: &Path, args: &[&str]) -> String {
    let out = run(bin, cwd, args);
    assert!(
        out.status.success(),
        "`{bin} {args:?}` failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// A fresh repository named after `tag`, initialized by `bin`.
fn repo(bin: &str, tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "zvcs-reflogline-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    let p = p.canonicalize().unwrap();
    ok(bin, &p, &["init", "-q", "--initial-branch=main", "."]);
    p
}

// ---------------------------------------------------------------------------
// what the repository actually recorded
// ---------------------------------------------------------------------------

/// The message field of the last `.git/logs/HEAD` line — the raw reflog record,
/// read off disk rather than through a porcelain, so nothing about how `git
/// reflog` renders can hide a wrong byte. `None` when no reflog line exists.
///
/// A reflog line is `<old> <new> <committer>\t<message>`, and the message field is
/// omitted entirely when it is empty (`log_ref_write_fd()`,
/// refs/files-backend.c:1944-1947), which reads back as `""`.
fn reflog_last(repo: &Path) -> Option<String> {
    let log = std::fs::read_to_string(repo.join(".git/logs/HEAD")).ok()?;
    let line = log.lines().next_back()?;
    Some(match line.split_once('\t') {
        Some((_, msg)) => msg.to_string(),
        None => String::new(),
    })
}

/// `git log -1 --format=%s`: the folded subject, which must not change.
fn subject(bin: &str, repo: &Path) -> String {
    ok(bin, repo, &["log", "-1", "--format=%s"])
}

/// Commit `message` as an empty commit and report `(%s, reflog message)`.
fn commit_and_read(bin: &str, repo: &Path, extra: &[&str], message: &str) -> (String, String) {
    let mut args = vec!["commit", "-q", "--allow-empty"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["-m", message]);
    let out = run(bin, repo, &args);
    assert!(
        out.status.success(),
        "`commit {extra:?}` failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    (subject(bin, repo), reflog_last(repo).expect("commit wrote a reflog entry"))
}

// ---------------------------------------------------------------------------
// the defect: the folded subject where git writes the first line
// ---------------------------------------------------------------------------

/// A subject continued on a second line: `%s` folds it, the reflog does not.
#[test]
fn continuation_line_is_cut_from_the_reflog_but_not_from_the_subject() {
    let repo = repo(BIN, "cont");
    let (subject, reflog) = commit_and_read(BIN, &repo, &[], "line one\nline two");
    assert_eq!(subject, "line one line two", "%s is the folded first paragraph");
    assert_eq!(reflog, "commit (initial): line one", "the reflog is the first line only");
}

/// Three lines and a body: still only the first line, and the body never appears.
#[test]
fn only_the_first_line_reaches_the_reflog() {
    let repo = repo(BIN, "three");
    ok(BIN, &repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let (subject, reflog) = commit_and_read(BIN, &repo, &[], "one\ntwo\nthree\n\nbody");
    assert_eq!(subject, "one two three");
    assert_eq!(reflog, "commit: one");
}

/// An ordinary two-paragraph message is identical either way — which is why a
/// single-line-message test suite never caught the defect above.
#[test]
fn a_paragraph_break_hides_the_difference() {
    let repo = repo(BIN, "para");
    let out = run(BIN, &repo, &["commit", "-q", "--allow-empty", "-m", "subj", "-m", "body"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(BIN, &repo), "subj");
    assert_eq!(reflog_last(&repo).unwrap(), "commit (initial): subj");
}

// ---------------------------------------------------------------------------
// `copy_reflog_msg()`: whitespace runs, and the ends
// ---------------------------------------------------------------------------

/// Tabs and multiple spaces survive in `%s` and collapse in the reflog. A tab in
/// particular *must* go: it is the reflog's own field separator.
#[test]
fn interior_whitespace_collapses_in_the_reflog_only() {
    let repo = repo(BIN, "space");
    let (subject, reflog) = commit_and_read(BIN, &repo, &[], "a\t\tb   c");
    assert_eq!(subject, "a\t\tb   c", "%s keeps the message's own spacing");
    assert_eq!(reflog, "commit (initial): a b c");
    let raw = std::fs::read_to_string(repo.join(".git/logs/HEAD")).unwrap();
    assert_eq!(
        raw.lines().next_back().unwrap().matches('\t').count(),
        1,
        "exactly the one tab that separates the committer from the message: {raw:?}"
    );
}

/// A `verbatim` message that opens with a newline has an empty first line, so the
/// reflog keeps the action and nothing else — `strbuf_rtrim()` takes the space
/// after the colon with it.
#[test]
fn an_empty_first_line_leaves_a_bare_action() {
    let repo = repo(BIN, "leadnl");
    let (subject, reflog) =
        commit_and_read(BIN, &repo, &["--cleanup=verbatim"], "\nfoo\nbar\n");
    assert_eq!(subject, "foo bar", "%s skips the leading blank line");
    assert_eq!(reflog, "commit (initial):", "no trailing space after the colon");
}

/// The same for a message that is empty altogether.
#[test]
fn an_empty_message_leaves_a_bare_action() {
    let repo = repo(BIN, "emptymsg");
    let (subject, reflog) = commit_and_read(BIN, &repo, &["--allow-empty-message"], "");
    assert_eq!(subject, "");
    assert_eq!(reflog, "commit (initial):");
}

/// The cut is the first newline, not a length limit: a 300-character first line
/// is recorded whole.
#[test]
fn a_long_first_line_is_not_truncated() {
    let repo = repo(BIN, "long");
    let head = "x".repeat(300);
    let (_, reflog) = commit_and_read(BIN, &repo, &[], &format!("{head}\nsecond line"));
    assert_eq!(reflog, format!("commit (initial): {head}"));
}

// ---------------------------------------------------------------------------
// the other writers that name a commit
// ---------------------------------------------------------------------------

/// `--amend` takes `commit (amend)` (builtin/commit.c:1854-1856) and the same cut.
#[test]
fn amend_records_the_first_line() {
    let repo = repo(BIN, "amend");
    ok(BIN, &repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let (subject, reflog) =
        commit_and_read(BIN, &repo, &["--amend"], "am one\nam two");
    assert_eq!(subject, "am one am two");
    assert_eq!(reflog, "commit (amend): am one");
}

/// `GIT_REFLOG_ACTION` replaces the wording and nothing else — `reflog_msg =
/// getenv(...)` is read before every fallback (builtin/commit.c:1850).
#[test]
fn a_reflog_action_override_keeps_the_cut() {
    let repo = repo(BIN, "action");
    ok(BIN, &repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let out = run_env(
        BIN,
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "ra one\nra two"],
        &[("GIT_REFLOG_ACTION", "zzz")],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(BIN, &repo), "ra one ra two");
    assert_eq!(reflog_last(&repo).unwrap(), "zzz: ra one");
}

/// A cherry-pick concludes through the same `update_head_with_reflog()`
/// (sequencer.c:1691), so a picked commit's continuation line is cut too.
#[test]
fn cherry_pick_records_the_first_line() {
    let repo = repo(BIN, "cherry");
    ok(BIN, &repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    ok(BIN, &repo, &["branch", "side"]);
    std::fs::write(repo.join("m.txt"), "m\n").unwrap();
    ok(BIN, &repo, &["add", "m.txt"]);
    ok(BIN, &repo, &["commit", "-q", "-m", "main"]);
    ok(BIN, &repo, &["checkout", "-q", "side"]);
    std::fs::write(repo.join("s.txt"), "s\n").unwrap();
    ok(BIN, &repo, &["add", "s.txt"]);
    ok(BIN, &repo, &["commit", "-q", "-m", "pick one\npick two"]);
    ok(BIN, &repo, &["checkout", "-q", "main"]);
    ok(BIN, &repo, &["cherry-pick", "side"]);
    assert_eq!(subject(BIN, &repo), "pick one pick two");
    assert_eq!(reflog_last(&repo).unwrap(), "cherry-pick: pick one");
}

/// The normalization is not commit's: `git update-ref -m` is handed to the same
/// `ref_transaction_add_update()` and comes back with its whitespace collapsed.
#[test]
fn update_ref_messages_are_normalized_too() {
    let repo = repo(BIN, "updateref");
    ok(BIN, &repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
    ok(BIN, &repo, &["update-ref", "-m", "msg\ta   b  ", "HEAD", "HEAD"]);
    assert_eq!(reflog_last(&repo).unwrap(), "msg a b");
}

// ---------------------------------------------------------------------------
// cross-check against stock git when the machine has one
// ---------------------------------------------------------------------------

/// A stock git that is definitely *not* this binary, or `None` to skip.
///
/// `zjobs` is a zvcs-only verb: stock git fails on it, this binary succeeds. The
/// probe runs with an **empty `PATH`** because git resolves an unknown verb by
/// looking for `git-<verb>` on `PATH` (`execv_dashed_external()`), and a machine
/// with the shadow binary installed has a `git-zjobs` symlink sitting there — so
/// with the ambient `PATH`, stock git would dispatch into zvcs and the probe would
/// mistake it for the binary under test.
fn stock_git() -> Option<String> {
    fn on_path(name: &str) -> Option<String> {
        if name.contains('/') {
            return std::fs::metadata(name).is_ok().then(|| name.to_string());
        }
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|c| c.is_file())
                .map(|c| c.to_string_lossy().into_owned())
        })
    }

    for cand in ["/opt/homebrew/bin/git", "/usr/bin/git", "/usr/local/bin/git", "git"] {
        let Some(cand) = on_path(cand) else { continue };
        let Ok(version) = Command::new(&cand).arg("--version").output() else { continue };
        if !version.status.success() || !version.stdout.starts_with(b"git version") {
            continue;
        }
        match Command::new(&cand).arg("zjobs").env("PATH", "").output() {
            Ok(out) if !out.status.success() => return Some(cand),
            _ => continue,
        }
    }
    None
}

/// Every shape above, run side by side with stock git. Skipped, not failed, on a
/// runner that has no stock git — the literal expectations above still hold there.
#[test]
fn stock_git_agrees_on_every_shape() {
    let Some(stock) = stock_git() else {
        eprintln!("no stock git on this machine; skipping the comparison");
        return;
    };
    // (tag, extra commit options, message)
    let cases: [(&str, &[&str], &str); 6] = [
        ("cont", &[], "line one\nline two"),
        ("three", &[], "one\ntwo\nthree\n\nbody"),
        ("space", &[], "a\t\tb   c"),
        ("leadnl", &["--cleanup=verbatim"], "\nfoo\nbar\n"),
        ("empty", &["--allow-empty-message"], ""),
        ("crlf", &["--cleanup=verbatim"], "one\r\ntwo\r\n"),
    ];
    for (tag, extra, message) in cases {
        let ours = repo(BIN, &format!("x-{tag}"));
        let theirs = repo(&stock, &format!("s-{tag}"));
        let (our_subject, our_reflog) = commit_and_read(BIN, &ours, extra, message);
        let (their_subject, their_reflog) = commit_and_read(&stock, &theirs, extra, message);
        assert_eq!(our_subject, their_subject, "%s for {tag}");
        assert_eq!(our_reflog, their_reflog, "reflog message for {tag}");
    }
}
