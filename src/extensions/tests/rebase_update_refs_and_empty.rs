//! Four rebase surfaces that used to be accepted at exit 0 while producing the
//! wrong data, plus the `am` mailbox split that failed a mailbox stock applies.
//!
//! Every expectation here was measured from stock git 2.55.0 on the same
//! fixture before it was written:
//!
//! * `--update-refs` generated no `update-ref` instruction at all, so every
//!   branch pointing into the rebased range was silently left on the old,
//!   now-unreachable commit.
//! * `--empty=keep` dropped the commit it is named for, and `--empty=stop` did
//!   not stop.
//! * `-X<option>` / `--ignore-whitespace` were parsed and then never reached the
//!   merge, so a rebase conflicted where stock resolved cleanly, and neither
//!   `$state_dir/strategy` nor `$state_dir/strategy_opts` was written for
//!   `--continue` to read back.
//! * `git am` never stripped CRLF, so a CRLF mailbox — the default `mailsplit`
//!   converts — failed at 128.
//! * The `pre-rebase` hook was handed two arguments where git passes one, and
//!   its refusal printed no diagnostic.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn empty(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rbur-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f
    }

    /// `main` = base c1 c2 c3 c4, with `base`/`t1`/`t2` parked inside the range
    /// and `up` a sibling of c1 that `main` will be rebased onto.
    fn branched(tag: &str) -> Self {
        let f = Fixture::empty(tag);
        for i in 1..=4 {
            f.append("f.txt", &format!("l{i}\n"));
            f.git(&["add", "f.txt"]);
            f.git(&["commit", "-q", "-m", &format!("c{i}")]);
        }
        f.git(&["branch", "b1", "HEAD~2"]);
        f.git(&["branch", "b2", "HEAD~1"]);
        f.git(&["checkout", "-q", "-b", "up", "HEAD~3"]);
        f.write("g.txt", "u\n");
        f.git(&["add", "g.txt"]);
        f.git(&["commit", "-q", "-m", "u1"]);
        f.git(&["checkout", "-q", "main"]);
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
    }

    fn append(&self, name: &str, body: &str) {
        let p = self.work.join(name);
        let mut old = std::fs::read_to_string(&p).unwrap_or_default();
        old.push_str(body);
        std::fs::write(p, old).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_SEQUENCE_EDITOR", ":")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
            .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn oid(&self, rev: &str) -> String {
        self.stdout(&["rev-parse", rev]).trim().to_string()
    }

    fn state(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.work.join(".git/rebase-merge").join(name)).ok()
    }

    /// A POSIX `sh` sequence editor that rewrites the sheet in place.
    fn editor(&self, name: &str, body: &str) -> String {
        let p = self.root.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&p, perms).unwrap();
        p.display().to_string()
    }
}

/// `--update-refs` must move every `refs/heads/*` that pointed into the rebased
/// range onto the commit that replaced it, with the reflog message
/// `rewritten during rebase` (`do_update_refs()`, sequencer.c:4597).
///
/// The regression it catches: the flag was accepted, the sheet was generated
/// with no `update-ref` line, and the run exited 0 leaving `b1`/`b2` on the old
/// commits — no diagnostic, no report.
#[test]
fn update_refs_moves_branches_inside_the_range() {
    let f = Fixture::branched("ur");
    let old_b1 = f.oid("b1");
    let old_b2 = f.oid("b2");

    let (ok, out, err) = f.run(&["rebase", "--update-refs", "up"]);
    assert!(ok, "rebase failed: {out}{err}");

    // Stock prints the report on stderr, one tab-indented refname per line, in
    // the sorted order `write_update_refs_state()` keeps.
    assert!(
        err.contains("Updated the following refs with --update-refs:\n\trefs/heads/b1\n\trefs/heads/b2\n"),
        "missing update report: {err}"
    );

    let new_b1 = f.oid("b1");
    let new_b2 = f.oid("b2");
    assert_ne!(new_b1, old_b1, "b1 was left on the pre-rebase commit");
    assert_ne!(new_b2, old_b2, "b2 was left on the pre-rebase commit");

    // Each branch has to land on the *rewritten* commit with the same subject,
    // i.e. it stays an ancestor of the rebased tip rather than dangling.
    assert_eq!(f.stdout(&["log", "-1", "--format=%s", "b1"]).trim(), "c2");
    assert_eq!(f.stdout(&["log", "-1", "--format=%s", "b2"]).trim(), "c3");
    let ancestry = f.stdout(&["rev-list", "main"]);
    assert!(ancestry.contains(&new_b1), "b1 not reachable from main: {ancestry}");
    assert!(ancestry.contains(&new_b2), "b2 not reachable from main: {ancestry}");

    assert_eq!(
        f.stdout(&["reflog", "show", "b1", "--format=%gs"]).lines().next(),
        Some("rewritten during rebase"),
    );
}

/// The generated sheet, and the `update-refs` state file a stopped rebase
/// leaves behind.
///
/// Both shapes are load-bearing and both were absent: the `update-ref` line
/// carries a trailing newline inside its argument (sequencer.c:6489-6498), so
/// the sheet has a blank line after each one, and the state file holds three
/// lines per record — refname, the id the ref held, and the id reached so far
/// (null until the instruction runs).
#[test]
fn update_refs_writes_the_sheet_and_the_state_file() {
    let f = Fixture::branched("urstate");
    let before_b1 = f.oid("b1");
    let editor = f.editor(
        "ed.sh",
        // Insert a `break` before the first pick so the run stops with the
        // sheet still holding both `update-ref` instructions.
        r#"printf 'break\n' > "$1.new"; cat "$1" >> "$1.new"; mv "$1.new" "$1""#,
    );

    let (ok, out, err) = f
        .cmd(&["rebase", "-i", "--update-refs", "up"])
        .env("GIT_SEQUENCE_EDITOR", &editor)
        .output()
        .map(|o| {
            (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
            )
        })
        .unwrap();
    assert!(ok, "rebase stopped unexpectedly: {out}{err}");

    let todo = f.state("git-rebase-todo").expect("no git-rebase-todo");
    assert!(
        todo.contains("update-ref refs/heads/b1\n\n"),
        "no update-ref line (with git's trailing blank) for b1: {todo:?}"
    );
    assert!(
        todo.contains("update-ref refs/heads/b2\n\n"),
        "no update-ref line (with git's trailing blank) for b2: {todo:?}"
    );

    let recorded = f.state("update-refs").expect("no update-refs state file");
    let lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(lines.len(), 6, "expected two three-line records: {recorded:?}");
    assert_eq!(lines[0], "refs/heads/b1");
    assert_eq!(lines[1], before_b1, "`before` is not the ref's pre-rebase id");
    assert_eq!(lines[2], "0".repeat(before_b1.len()), "`after` should start null");
    assert_eq!(lines[3], "refs/heads/b2");
}

/// The branch `HEAD` is on is excluded from `--update-refs`: the rebase's own
/// finish already moves it, and listing it would make `do_update_refs()` fight
/// that update.
#[test]
fn update_refs_skips_the_branch_being_rebased() {
    let f = Fixture::branched("urhead");
    let editor = f.editor("ed2.sh", r#"printf 'break\n' > "$1.new"; cat "$1" >> "$1.new"; mv "$1.new" "$1""#);
    let out = f
        .cmd(&["rebase", "-i", "--update-refs", "up"])
        .env("GIT_SEQUENCE_EDITOR", &editor)
        .output()
        .unwrap();
    assert!(out.status.success(), "rebase failed: {out:?}");
    let recorded = f.state("update-refs").unwrap_or_default();
    assert!(
        !recorded.contains("refs/heads/main"),
        "the rebased branch must not be tracked: {recorded:?}"
    );
}

/// A commit whose pick comes out empty without conflicting, for the `--empty`
/// family.
///
/// `topic sets X` changes line 2; the upstream changes line 2 the same way
/// *and* line 8, far enough apart that the merge is clean. The pick therefore
/// leaves the index identical to `HEAD` — but the upstream commit's patch has
/// two hunks, so its patch id differs and `--cherry-mark` does not remove the
/// topic commit from the sheet. `allow_empty()` is what decides, which is the
/// whole point of `--empty`.
fn empty_fixture(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.write("f.txt", "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.git(&["checkout", "-q", "-b", "topic"]);
    f.write("f.txt", "l1\nX\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "topic sets X"]);
    f.write("f.txt", "l1\nX\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nZ\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "topic sets Z"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("f.txt", "l1\nX\nl3\nl4\nl5\nl6\nl7\nY\nl9\nl10\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "upstream sets X and Y"]);
    f.git(&["checkout", "-q", "topic"]);
    f
}

/// `--empty=keep` keeps the commit whose pick came out empty
/// (`allow_empty()` == 1, sequencer.c:1812-1813).
///
/// The regression: the pick loop dropped every empty pick unconditionally, so
/// `--empty=keep` silently lost a commit at exit 0.
#[test]
fn empty_keep_keeps_the_emptied_commit() {
    let f = empty_fixture("keep");
    let (ok, out, err) = f.run(&["rebase", "--empty=keep", "main"]);
    assert!(ok, "rebase failed: {out}{err}");
    let subjects: Vec<String> =
        f.stdout(&["log", "--format=%s"]).lines().map(str::to_owned).collect();
    assert_eq!(
        subjects,
        ["topic sets Z", "topic sets X", "upstream sets X and Y", "base"],
        "an emptied commit was dropped under --empty=keep",
    );
    assert!(
        !err.contains("dropping "),
        "--empty=keep must not report a drop: {err}"
    );
}

/// `--empty=drop` reports the drop by full object name and keeps going
/// (`allow_empty()` == 2, sequencer.c:2502-2511).
#[test]
fn empty_drop_reports_and_continues() {
    let f = empty_fixture("drop");
    let dropped = f.oid("HEAD~1");
    let (ok, out, err) = f.run(&["rebase", "--empty=drop", "main"]);
    assert!(ok, "rebase failed: {out}{err}");
    assert!(
        err.contains(&format!(
            "dropping {dropped} topic sets X -- patch contents already upstream"
        )),
        "missing drop report: {err}"
    );
    let subjects: Vec<String> =
        f.stdout(&["log", "--format=%s"]).lines().map(str::to_owned).collect();
    assert_eq!(subjects, ["topic sets Z", "upstream sets X and Y", "base"]);
}

/// `--empty=stop` halts the rebase on the emptied pick (`allow_empty()` == 0),
/// which git implements by re-entering a real `git commit` that refuses the
/// empty commit. Everything that refusal prints arrives on **stderr**, because
/// the child's stdout is a pipe folded into it.
///
/// The regression: nothing stopped — the commit was dropped and the rebase
/// exited 0, so `--empty=stop` was indistinguishable from `--empty=drop`.
#[test]
fn empty_stop_halts_with_a_resumable_state() {
    let f = empty_fixture("stop");
    let stopped = f.oid("HEAD~1");
    let (ok, out, err) = f.run(&["rebase", "--empty=stop", "main"]);
    assert!(!ok, "--empty=stop should not succeed: {out}{err}");
    assert!(out.is_empty(), "the refusal belongs on stderr, not stdout: {out:?}");
    assert!(
        err.contains("The previous cherry-pick is now empty, possibly due to conflict resolution."),
        "missing git commit refusal: {err}"
    );
    assert!(
        err.contains("Otherwise, please use 'git rebase --skip'"),
        "missing the rebase-specific half of the advice: {err}"
    );
    assert!(err.contains("Could not apply "), "missing error_with_patch line: {err}");

    // The stop is a normal sequencer stop: resumable, with the pick recorded.
    assert_eq!(f.state("stopped-sha").unwrap().trim(), stopped);
    assert_eq!(
        std::fs::read_to_string(f.work.join(".git/REBASE_HEAD")).unwrap().trim(),
        stopped,
    );
    assert_eq!(f.state("msgnum").unwrap().trim(), "1");
    assert!(f.state("patch").is_some(), "no patch recorded for --show-current-patch");

    // `--continue` moves past it and finishes.
    let (ok, out, err) = f.run(&["rebase", "--continue"]);
    assert!(ok, "--continue failed: {out}{err}");
    let subjects: Vec<String> =
        f.stdout(&["log", "--format=%s"]).lines().map(str::to_owned).collect();
    assert_eq!(subjects, ["topic sets Z", "upstream sets X and Y", "base"]);
}

/// `--empty` is recorded as the pair of marker files the sequencer reads back,
/// so a stopped rebase resumes with the same policy (sequencer.c:3344-3347).
/// `stop` writes neither, which is exactly what makes `allow_empty()` return 0.
#[test]
fn empty_is_recorded_as_marker_files() {
    let f = empty_fixture("markers");
    let (_, _, _) = f.run(&["rebase", "--empty=stop", "main"]);
    assert!(f.state("drop_redundant_commits").is_none());
    assert!(f.state("keep_redundant_commits").is_none());
    f.run(&["rebase", "--abort"]);

    let f = Fixture::empty("markers2");
    f.write("f.txt", "a\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.git(&["checkout", "-q", "-b", "up"]);
    f.write("u.txt", "u\n");
    f.git(&["add", "u.txt"]);
    f.git(&["commit", "-q", "-m", "u1"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("f.txt", "conflicting\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "m1"]);
    f.write("f.txt", "a\nlater\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "m2"]);
    let editor = f.editor("brk.sh", r#"printf 'break\n' > "$1.new"; cat "$1" >> "$1.new"; mv "$1.new" "$1""#);
    let out = f
        .cmd(&["rebase", "-i", "--empty=drop", "up"])
        .env("GIT_SEQUENCE_EDITOR", &editor)
        .output()
        .unwrap();
    assert!(out.status.success(), "rebase failed: {out:?}");
    assert!(f.state("drop_redundant_commits").is_some(), "--empty=drop wrote no marker");
    assert!(f.state("keep_redundant_commits").is_none());
}

/// An upstream whose only change to the contested line is the *run of spaces
/// inside it*, so a plain rebase conflicts and `-Xignore-space-change` — which
/// is what `--ignore-whitespace` becomes on the merge backend
/// (builtin/rebase.c:1546-1548) — does not.
fn whitespace_fixture(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.write("f.txt", "a\nb c\nd\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.git(&["checkout", "-q", "-b", "topic"]);
    f.write("f.txt", "a\nTOPIC\nd\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "topic rewrites the line"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("f.txt", "a\nb    c\nd\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "upstream respaces the line"]);
    f.git(&["checkout", "-q", "topic"]);
    f
}

/// Without any option the pick conflicts; that is the control for the two below.
#[test]
fn a_whitespace_only_upstream_change_conflicts_by_default() {
    let f = whitespace_fixture("ws-control");
    let (ok, _out, err) = f.run(&["rebase", "main"]);
    assert!(!ok, "expected a conflict");
    assert!(err.contains("could not apply"), "{err}");
    assert!(std::fs::read_to_string(f.work.join("f.txt")).unwrap().contains("<<<<<<<"));
}

/// `--ignore-whitespace` has to reach the merge. It used to be parsed, turned
/// into `ignore-space-change`, and then dropped, so the rebase conflicted where
/// stock resolved cleanly.
#[test]
fn ignore_whitespace_reaches_the_merge() {
    let f = whitespace_fixture("ws");
    let (ok, out, err) = f.run(&["rebase", "--ignore-whitespace", "main"]);
    assert!(ok, "--ignore-whitespace still conflicted: {out}{err}");
    assert_eq!(
        std::fs::read_to_string(f.work.join("f.txt")).unwrap(),
        "a\nTOPIC\nd\n",
        "the resolved content is not the topic's",
    );
}

/// `-Xtheirs` resolves the same conflict the other way, and — because the merge
/// came out clean — the rebase prints no `Auto-merging`/`CONFLICT` block:
/// `show_output = !is_rebase_i(opts) || !result.clean` (sequencer.c:783).
#[test]
fn strategy_option_theirs_resolves_and_stays_quiet() {
    let f = whitespace_fixture("xtheirs");
    let (ok, out, err) = f.run(&["rebase", "-Xtheirs", "main"]);
    assert!(ok, "-Xtheirs still conflicted: {out}{err}");
    assert_eq!(std::fs::read_to_string(f.work.join("f.txt")).unwrap(), "a\nTOPIC\nd\n");
    assert!(
        !out.contains("Auto-merging") && !err.contains("Auto-merging"),
        "a clean rebase pick must not announce the merge: {out}{err}",
    );
}

/// The strategy and its options are recorded so `--continue` merges the same
/// way (`write_basic_state()`, sequencer.c:3330-3333). `strategy_opts` is one
/// `quote_cmdline()`-quoted word per option on a single line, which is the form
/// `split_cmdline()` reads back.
#[test]
fn strategy_and_options_are_recorded_for_continue() {
    let f = whitespace_fixture("xstate");
    // `-Xours` on this fixture keeps the upstream's line, so the pick empties
    // out and the run stops under the interactive `--empty` default — leaving
    // the state directory in place to inspect.
    let (_ok, _out, _err) = f.run(&["rebase", "-i", "-Xours", "-Xdiff-algorithm=histogram", "main"]);
    assert_eq!(f.state("strategy").as_deref(), Some("ort\n"));
    assert_eq!(
        f.state("strategy_opts").as_deref(),
        Some("\"ours\" \"diff-algorithm=histogram\"\n"),
    );
}

/// `git am` converts CRLF to LF unless `--keep-cr`
/// (`split_one()`, builtin/mailsplit.c:88-92). Without it a CRLF mailbox — the
/// common shape when a patch has been through a Windows mail client — failed at
/// 128 on a patch stock applies.
#[test]
fn am_strips_crlf_by_default() {
    let f = Fixture::empty("keepcr");
    f.write("f.txt", "a\nb\nc\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.write("f.txt", "a\nB\nc\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "change b"]);
    let mbox = f.stdout(&["format-patch", "-1", "--stdout"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);

    let crlf = mbox.replace('\n', "\r\n");
    let path = f.root.join("crlf.mbox");
    std::fs::write(&path, &crlf).unwrap();

    let (ok, out, err) = f.run(&["am", path.to_str().unwrap()]);
    assert!(ok, "a CRLF mailbox must apply: {out}{err}");
    assert_eq!(f.stdout(&["log", "-1", "--format=%s"]).trim(), "change b");
    assert_eq!(
        std::fs::read_to_string(f.work.join("f.txt")).unwrap(),
        "a\nB\nc\n",
        "the applied content kept its CRs",
    );
}

/// `--keep-cr` is not a no-op: the CRs survive into the patch, which then does
/// not apply to an LF worktree. Measured against stock, which also fails here —
/// the point is that the flag changes what the split produces.
#[test]
fn am_keep_cr_preserves_the_carriage_returns() {
    let f = Fixture::empty("keepcr2");
    f.write("f.txt", "a\nb\nc\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.write("f.txt", "a\nB\nc\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "change b"]);
    let mbox = f.stdout(&["format-patch", "-1", "--stdout"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);

    let path = f.root.join("crlf2.mbox");
    std::fs::write(&path, mbox.replace('\n', "\r\n")).unwrap();

    let (ok, _out, _err) = f.run(&["am", "--keep-cr", path.to_str().unwrap()]);
    assert!(!ok, "--keep-cr should have kept the unapplicable CRs");
    let patch = std::fs::read(f.work.join(".git/rebase-apply/patch")).unwrap();
    assert!(
        patch.windows(2).any(|w| w == b"\r\n"),
        "--keep-cr left no CRLF in the split patch",
    );
    f.run(&["am", "--abort"]);
}

/// `am.keepcr` is the configuration `--keep-cr` overrides, and `--no-keep-cr`
/// overrides it back (`OPT_SET_INT` without `PARSE_OPT_NONEG`, builtin/am.c:2352).
#[test]
fn am_keepcr_config_and_its_override() {
    let f = Fixture::empty("keepcr3");
    f.write("f.txt", "a\nb\nc\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.write("f.txt", "a\nB\nc\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "change b"]);
    let mbox = f.stdout(&["format-patch", "-1", "--stdout"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    let path = f.root.join("crlf3.mbox");
    std::fs::write(&path, mbox.replace('\n', "\r\n")).unwrap();

    f.git(&["config", "am.keepcr", "true"]);
    let (ok, _, _) = f.run(&["am", path.to_str().unwrap()]);
    assert!(!ok, "am.keepcr=true should have kept the CRs");
    f.run(&["am", "--abort"]);

    let (ok, out, err) = f.run(&["am", "--no-keep-cr", path.to_str().unwrap()]);
    assert!(ok, "--no-keep-cr must override am.keepcr: {out}{err}");
    assert_eq!(f.stdout(&["log", "-1", "--format=%s"]).trim(), "change b");
}

/// The `pre-rebase` hook receives `<upstream>` alone when no `[<branch>]`
/// operand was given — `run_hooks_l(..., options.upstream_arg, argc ? argv[0] :
/// NULL, NULL)` is NULL-terminated (builtin/rebase.c:1834-1835) — and its
/// refusal prints `error: The pre-rebase hook refused to rebase.` before
/// exiting 1.
#[test]
fn pre_rebase_hook_argv_and_refusal() {
    let f = Fixture::branched("hook");
    let hooks = f.work.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let path = hooks.join("pre-rebase");
    std::fs::write(
        &path,
        "#!/bin/sh\nprintf 'argc=%s\\n' \"$#\" >&2\nfor a in \"$@\"; do printf 'arg=[%s]\\n' \"$a\" >&2; done\nexit ${PRE_REBASE_RC:-0}\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();

    let (ok, out, err) = f.run(&["rebase", "up"]);
    assert!(ok, "rebase failed: {out}{err}");
    assert!(err.contains("argc=1\n"), "hook got the wrong argument count: {err}");
    assert!(err.contains("arg=[up]\n"), "hook did not get <upstream>: {err}");

    // With a `[<branch>]` operand there are two, in git's order.
    let f2 = Fixture::branched("hook2");
    let hooks = f2.work.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::copy(&path, hooks.join("pre-rebase")).unwrap();
    let (_ok, _out, err) = f2.run(&["rebase", "up", "b1"]);
    assert!(err.contains("argc=2\n"), "{err}");
    assert!(err.contains("arg=[up]\n") && err.contains("arg=[b1]\n"), "{err}");

    // A refusing hook stops the rebase with a diagnostic.
    let f3 = Fixture::branched("hook3");
    let hooks = f3.work.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::copy(&path, hooks.join("pre-rebase")).unwrap();
    let before = f3.oid("HEAD");
    let out = f3.cmd(&["rebase", "up"]).env("PRE_REBASE_RC", "1").output().unwrap();
    assert!(!out.status.success(), "a refusing hook must fail the rebase");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("error: The pre-rebase hook refused to rebase."),
        "missing refusal diagnostic: {out:?}"
    );
    assert_eq!(f3.oid("HEAD"), before, "the refused rebase moved HEAD");
}

/// A `fixup` writes its own subject into the reflog, exactly as a `pick` does:
/// `try_to_commit()` ends in the same `update_head_with_reflog()` call. The
/// entry used to be the bare `rebase (fixup)`, with no subject.
#[test]
fn fixup_reflog_entry_carries_the_subject() {
    let f = Fixture::empty("fixup");
    f.write("a", "a\n");
    f.git(&["add", "a"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.write("b", "b\n");
    f.git(&["add", "b"]);
    f.git(&["commit", "-q", "-m", "add b"]);
    f.write("c", "c\n");
    f.git(&["add", "c"]);
    f.git(&["commit", "-q", "-m", "add c"]);
    f.append("b", "more\n");
    f.git(&["add", "b"]);
    f.git(&["commit", "-q", "-m", "fixup! add b"]);

    let (ok, out, err) = f.run(&["rebase", "--autosquash", "-i", "HEAD~3"]);
    assert!(ok, "rebase failed: {out}{err}");
    let log: Vec<String> = f
        .stdout(&["reflog", "show", "HEAD", "--format=%gs"])
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        log.iter().any(|l| l == "rebase (fixup): add b"),
        "no fixup reflog entry with a subject: {log:?}"
    );
}

/// `--cherry-mark` removes a commit whose patch is already in `<upstream>` from
/// the sheet, before any pick runs. It is what keeps `git rebase -i` — whose
/// `--empty` default is `stop` — from halting on a commit the upstream already
/// carries.
#[test]
fn a_previously_applied_commit_is_skipped_before_the_sheet() {
    let f = Fixture::empty("cherry");
    f.write("f.txt", "a\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.git(&["checkout", "-q", "-b", "up"]);
    f.write("x.txt", "X\n");
    f.git(&["add", "x.txt"]);
    f.git(&["commit", "-q", "-m", "adds x"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("x.txt", "X\n");
    f.git(&["add", "x.txt"]);
    f.git(&["commit", "-q", "-m", "adds x too"]);
    f.write("y.txt", "Y\n");
    f.git(&["add", "y.txt"]);
    f.git(&["commit", "-q", "-m", "adds y"]);

    let short = f.stdout(&["rev-parse", "--short", "HEAD~1"]).trim().to_string();
    let (ok, out, err) = f.run(&["rebase", "-i", "up"]);
    assert!(ok, "rebase should not have stopped: {out}{err}");
    assert!(
        err.contains(&format!("warning: skipped previously applied commit {short}")),
        "missing skip warning: {err}"
    );
    assert!(
        err.contains("hint: use --reapply-cherry-picks to include skipped commits"),
        "missing skip advice: {err}"
    );
    let subjects: Vec<String> =
        f.stdout(&["log", "--format=%s"]).lines().map(str::to_owned).collect();
    assert_eq!(subjects, ["adds y", "adds x", "base"]);

    // `--reapply-cherry-picks` turns the mark off; the commit reaches the pick
    // loop and the interactive `--empty=stop` default halts on it.
    let f2 = Fixture::empty("cherry2");
    f2.write("f.txt", "a\n");
    f2.git(&["add", "f.txt"]);
    f2.git(&["commit", "-q", "-m", "base"]);
    f2.git(&["checkout", "-q", "-b", "up"]);
    f2.write("x.txt", "X\n");
    f2.git(&["add", "x.txt"]);
    f2.git(&["commit", "-q", "-m", "adds x"]);
    f2.git(&["checkout", "-q", "main"]);
    f2.write("x.txt", "X\n");
    f2.git(&["add", "x.txt"]);
    f2.git(&["commit", "-q", "-m", "adds x too"]);
    let (ok, out, err) = f2.run(&["rebase", "-i", "--reapply-cherry-picks", "up"]);
    assert!(!ok, "--reapply-cherry-picks + --empty=stop should halt: {out}{err}");
    assert!(err.contains("The previous cherry-pick is now empty"), "{err}");
}

/// `git am --patch-format=hg` re-emits mercurial's `<epoch> <seconds west>`
/// date as RFC2822 (builtin/am.c:891-929). Passing it through unchanged left
/// `mailinfo` with an unparsable date, so every hg patch was committed with the
/// current time.
#[test]
fn hg_patch_format_keeps_the_author_date() {
    let f = Fixture::empty("hg");
    f.write("file", "one\n");
    f.git(&["add", "file"]);
    f.git(&["commit", "-q", "-m", "first"]);
    f.write("file", "one\ntwo\n");
    f.git(&["add", "file"]);
    f.git(&["commit", "-q", "-m", "second"]);
    let body = f.stdout(&["diff-tree", "--no-commit-id", "-p", "HEAD"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);

    // 1112900000 with 25200 seconds *west* of UTC is 2005-04-07T11:53:20-07:00.
    let patch = format!(
        "# HG changeset patch\n# User A <a@e.com>\n# Date 1112900000 25200\nsecond\n\n{body}"
    );
    let path = f.root.join("hg.eml");
    std::fs::write(&path, patch).unwrap();

    let (ok, out, err) = f.run(&["am", "--patch-format=hg", path.to_str().unwrap()]);
    assert!(ok, "hg patch failed: {out}{err}");
    assert_eq!(
        f.stdout(&["log", "-1", "--format=%aI"]).trim(),
        "2005-04-07T11:53:20-07:00",
    );
    assert_eq!(f.stdout(&["log", "-1", "--format=%s"]).trim(), "second");
}

/// A malformed `# Date` aborts the whole split, with the converter's own
/// diagnostic, then `could not parse patch '<path>'`, then
/// `fatal: Failed to split patches.` and exit 128.
#[test]
fn hg_patch_format_rejects_a_malformed_date() {
    let f = Fixture::empty("hgbad");
    f.write("file", "one\n");
    f.git(&["add", "file"]);
    f.git(&["commit", "-q", "-m", "first"]);
    f.write("file", "one\ntwo\n");
    f.git(&["add", "file"]);
    f.git(&["commit", "-q", "-m", "second"]);
    let body = f.stdout(&["diff-tree", "--no-commit-id", "-p", "HEAD"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);

    // No timezone field at all.
    let path = f.root.join("hgbad.eml");
    std::fs::write(
        &path,
        format!("# HG changeset patch\n# User A <a@e.com>\n# Date 1112900000\nsecond\n\n{body}"),
    )
    .unwrap();

    let out = f.cmd(&["am", "--patch-format=hg", path.to_str().unwrap()]).output().unwrap();
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("error: invalid Date line"), "{err}");
    assert!(
        err.contains(&format!("error: could not parse patch '{}'", path.display())),
        "{err}"
    );
    assert!(err.contains("fatal: Failed to split patches."), "{err}");
    assert!(!f.work.join(".git/rebase-apply").exists(), "the session directory survived");
}
