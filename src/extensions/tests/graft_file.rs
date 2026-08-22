//! `<GIT_DIR>/info/grafts` — git's commit graft table (`commit.c:249-330`).
//!
//! A graft line replaces a commit's parent list for every walk, without touching
//! the commit object, so `log`, `rev-list`, `merge-base`, `describe` and `rev-parse
//! <rev>^` all follow the substituted parents while `cat-file -p` still prints the
//! recorded `parent` header. `parse_commit_buffer()` (commit.c:554-590) is where
//! git makes that split, and this file pins zvcs to the same one.
//!
//! Every expectation below is the output of stock git 2.55.0 over the fixture
//! [`Fixture::new`] builds — four commits `c1..c4` with a `side` branch at `c1`,
//! pinned to one identity and one timestamp so the object ids are fixed. The ids
//! are asserted first, so a fixture that ever stops matching stock fails loudly
//! instead of silently re-baselining everything after it.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Object ids stock git 2.55.0 writes for [`Fixture::new`]'s four commits.
const C1: &str = "8beb863f66fc41030653e5c9609612dbc32b5441";
const C2: &str = "3bc88635fdb3470eb1f358d613dfce2314b3119a";
const C3: &str = "864f8e9bf13dde4cce0db8ab66be06e3a4017e73";
const C4: &str = "2a07f7aa502f1ce3b0084b7f36bbc92431ffe179";

/// `advise()`'s eight lines for `ADVICE_GRAFT_FILE_DEPRECATED` (commit.c:293-302),
/// byte for byte, including the bare `hint:` for each blank line.
const DEPRECATION_ADVICE: &str = "\
hint: Support for <GIT_DIR>/info/grafts is deprecated
hint: and will be removed in a future Git version.
hint:
hint: Please use \"git replace --convert-graft-file\"
hint: to convert the grafts into replace refs.
hint:
hint: Turn this message off by running
hint: \"git config set advice.graftFileDeprecated false\"
";

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Fixture {
    /// Four commits on `main` and a `side` branch at the root commit, with the
    /// identity and both timestamps pinned so every object id is reproducible.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-graftfile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo, home };

        f.ok(&["init", "-q", "-b", "main"]);
        f.ok(&["config", "user.name", "A"]);
        f.ok(&["config", "user.email", "a@e"]);
        // Every expectation below spells an abbreviated id, so the width has to be
        // a property of the fixture rather than of whoever runs it. `core.abbrev`
        // defaults to `auto`, which git scales with the object count, and the
        // developer capturing these strings had `abbrev = 10` in their own
        // `~/.gitconfig` — which this fixture then isolates away with `HOME`, so
        // the captured widths were ten and the observed ones seven. Pinning it
        // makes the two agree and keeps them agreeing as the fixture grows.
        f.ok(&["config", "core.abbrev", "10"]);
        for i in 1..=4 {
            std::fs::write(f.repo.join("f"), format!("{i}\n")).unwrap();
            f.ok(&["add", "f"]);
            f.ok(&["commit", "-q", "-m", &format!("c{i}")]);
        }
        f.ok(&["branch", "side", "main~3"]);

        // If these drift, the whole file's expectations are about a different
        // history and nothing below can be trusted.
        assert_eq!(f.ok(&["rev-parse", "main"]).trim_end(), C4, "fixture c4");
        assert_eq!(f.ok(&["rev-parse", "main~1"]).trim_end(), C3, "fixture c3");
        assert_eq!(f.ok(&["rev-parse", "main~2"]).trim_end(), C2, "fixture c2");
        assert_eq!(f.ok(&["rev-parse", "side"]).trim_end(), C1, "fixture c1");
        f
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_GRAFT_FILE")
            .env_remove("GIT_NO_REPLACE_OBJECTS")
            .env_remove("GIT_ADVICE")
            // The identity and both dates are what make the object ids above fixed.
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@e")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@e")
            .env("GIT_AUTHOR_DATE", "1112904793 +0200")
            .env("GIT_COMMITTER_DATE", "1112904793 +0200");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        let out = self.command(args).output().expect("run zvcs");
        Output {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Run and require success, returning stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(out.code, 0, "git {args:?} failed ({}):\n{}", out.code, out.stderr);
        out.stdout
    }

    /// Run with the deprecation advice switched off, so stdout comparisons are not
    /// entangled with the hint. Returns the full [`Output`].
    fn quiet(&self, args: &[&str]) -> Output {
        let mut full = vec!["-c", "advice.graftFileDeprecated=false"];
        full.extend_from_slice(args);
        self.run(&full)
    }

    fn write_grafts(&self, contents: &str) {
        let info = self.repo.join(".git").join("info");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::write(info.join("grafts"), contents).unwrap();
    }

    fn path(&self) -> &Path {
        &self.repo
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A graft line with no parents makes the commit a root: `nr_parent == 0`, so
/// `parse_commit_buffer()` drops every decoded `parent` header (commit.c:569) and
/// appends nothing (commit.c:578-590).
///
/// The commit object is untouched, which is why `cat-file -p` still shows the real
/// parent and `<commit>^` no longer resolves.
#[test]
fn a_parentless_graft_line_truncates_every_walk() {
    let f = Fixture::new("truncate");
    f.write_grafts(&format!("{C3}\n"));

    let log = f.quiet(&["log", "--oneline"]);
    assert_eq!(log.code, 0, "stderr: {}", log.stderr);
    assert_eq!(log.stdout, "2a07f7aa50 c4\n864f8e9bf1 c3\n");

    assert_eq!(f.quiet(&["rev-list", "--count", "HEAD"]).stdout, "2\n");
    assert_eq!(
        f.quiet(&["log", "--graph", "--oneline"]).stdout,
        "* 2a07f7aa50 c4\n* 864f8e9bf1 c3\n"
    );

    // `cat-file -p` prints the object, and the object still has its parent:
    // grafts live in the parsed commit, not in the odb.
    let raw = f.quiet(&["cat-file", "-p", C3]).stdout;
    assert!(
        raw.contains(&format!("parent {C2}\n")),
        "the graft must not rewrite the object; got:\n{raw}"
    );

    // `get_parent()` walks `commit->parents`, which is now empty, so the name is
    // unresolvable — git's `verify_filename()` fallback message.
    let caret = f.quiet(&["rev-parse", &format!("{C3}^")]);
    assert_eq!(caret.code, 128, "stdout {:?} stderr {:?}", caret.stdout, caret.stderr);
    assert!(
        caret.stderr.starts_with(&format!(
            "fatal: ambiguous argument '{C3}^': unknown revision or path not in the working tree.\n"
        )),
        "got: {:?}",
        caret.stderr
    );
}

/// A graft line with parents substitutes them: the walk crosses from `c3` straight
/// to `c1`, skipping `c2` entirely, and everything that reads a parent list agrees.
#[test]
fn a_graft_line_substitutes_the_parent_list() {
    let f = Fixture::new("substitute");
    f.write_grafts(&format!("{C3} {C1}\n"));

    assert_eq!(
        f.quiet(&["log", "--oneline"]).stdout,
        "2a07f7aa50 c4\n864f8e9bf1 c3\n8beb863f66 c1\n"
    );
    assert_eq!(f.quiet(&["rev-list", "--count", "HEAD"]).stdout, "3\n");

    // `%p`/`%P` render `commit->parents`, so they show the graft, not the header.
    assert_eq!(
        f.quiet(&["log", "--format=%h %p %P"]).stdout,
        format!("2a07f7aa50 864f8e9bf1 {C3}\n864f8e9bf1 8beb863f66 {C1}\n8beb863f66  \n")
    );
    assert_eq!(
        f.quiet(&["rev-list", "--parents", "HEAD"]).stdout,
        format!("{C4} {C3}\n{C3} {C1}\n{C1}\n")
    );

    // `<rev>^` is `get_parent()` over the same list.
    assert_eq!(f.quiet(&["rev-parse", &format!("{C3}^")]).stdout, format!("{C1}\n"));

    // `c1` is `side`, and with the graft it is also an ancestor of `main`, so it is
    // the merge base of the two.
    assert_eq!(f.quiet(&["merge-base", "HEAD", "side"]).stdout, format!("{C1}\n"));
}

/// `git describe` counts the commits between the tag and `HEAD` over the same
/// parent lists, so a graft shortens the count it reports.
#[test]
fn describe_counts_over_the_grafted_history() {
    let f = Fixture::new("describe");
    f.ok(&["tag", "-m", "t", "v1", C1]);

    // Ungrafted: c1 -> c2 -> c3 -> c4 is three commits past the tag.
    assert_eq!(f.quiet(&["describe", "HEAD"]).stdout, "v1-3-g2a07f7aa50\n");

    f.write_grafts(&format!("{C3} {C1}\n"));
    assert_eq!(f.quiet(&["describe", "HEAD"]).stdout, "v1-2-g2a07f7aa50\n");
}

/// `read_graft_line()` (commit.c:249-285) skips blank lines and `#` comments after
/// `strbuf_rtrim()`, which is also what makes a CRLF file and trailing blanks work.
/// A line it cannot parse is `error("bad graft data: %s", line->buf)` — reported,
/// not fatal, with every other line still registered.
#[test]
fn unparsable_lines_are_reported_and_skipped() {
    let f = Fixture::new("badline");
    f.write_grafts(&format!("# a comment\n\n   \ngarbage\n{C3} {C1}   \r\n"));

    let out = f.quiet(&["log", "--oneline"]);
    assert_eq!(out.code, 0, "a bad line must not fail the command");
    assert_eq!(out.stderr, "error: bad graft data: garbage\n");
    assert_eq!(
        out.stdout,
        "2a07f7aa50 c4\n864f8e9bf1 c3\n8beb863f66 c1\n",
        "the comment, the blanks and the CRLF line must all be handled"
    );
}

/// `register_commit_graft(r, graft, 1)` (commit.c:220-246, called from
/// commit.c:308) keeps the entry already registered and answers "duplicate", so the
/// *first* line naming a commit wins and the later one is only reported.
#[test]
fn a_second_line_for_one_commit_is_a_duplicate() {
    let f = Fixture::new("duplicate");
    f.write_grafts(&format!("{C3} {C1}\n{C3}\n"));

    let out = f.quiet(&["log", "--oneline"]);
    assert_eq!(out.code, 0);
    assert_eq!(out.stderr, format!("error: duplicate graft data: {C3}\n"));
    assert_eq!(
        out.stdout,
        "2a07f7aa50 c4\n864f8e9bf1 c3\n8beb863f66 c1\n",
        "the first line stands; the parentless one is discarded"
    );
}

/// The `advice.graftFileDeprecated` hint (commit.c:293-302) fires on the read that
/// builds the table — once, for a command that parses a commit at all — and is
/// switched off by its own slot or by `GIT_ADVICE`.
#[test]
fn the_deprecation_advice_is_printed_once_and_can_be_switched_off() {
    let f = Fixture::new("advice");
    f.write_grafts(&format!("{C3} {C1}\n"));

    let out = f.run(&["log", "--oneline"]);
    assert_eq!(out.code, 0);
    assert_eq!(out.stderr, DEPRECATION_ADVICE, "the hint, byte for byte");
    assert_eq!(
        out.stderr.matches("Support for").count(),
        1,
        "`commit_graft_prepared` latches, so one command prints one hint"
    );

    assert_eq!(
        f.run(&["-c", "advice.graftFileDeprecated=false", "log", "--oneline"]).stderr,
        "",
        "advice.graftFileDeprecated=false silences it"
    );
    let quiet_env = f
        .command(&["log", "--oneline"])
        .env("GIT_ADVICE", "0")
        .output()
        .expect("run zvcs");
    assert_eq!(
        String::from_utf8_lossy(&quiet_env.stderr),
        "",
        "GIT_ADVICE=0 silences every hint"
    );

    // A command that never asks for a parent list never reads the table, which is
    // why `git cat-file -p <commit>` is silent in git too.
    assert_eq!(f.run(&["cat-file", "-t", C3]).stderr, "");
}

/// Grafts and replace refs are separate mechanisms with separate switches.
///
/// `disable_replace_refs()`/`replace_refs_enabled()` (replace-object.c:83-108) gate
/// `lookup_replace_object()` only; `lookup_commit_graft()` (commit.c:332-340)
/// consults none of them. Measured against git 2.55.0: all three ways of turning
/// replacement off leave a grafted walk exactly as truncated as an ungated one.
#[test]
fn grafts_are_not_gated_by_the_replace_object_switches() {
    let f = Fixture::new("gating");
    f.write_grafts(&format!("{C3}\n"));

    let expected = "2a07f7aa50 c4\n864f8e9bf1 c3\n";
    assert_eq!(f.quiet(&["log", "--oneline"]).stdout, expected, "baseline");
    assert_eq!(
        f.quiet(&["--no-replace-objects", "log", "--oneline"]).stdout,
        expected,
        "--no-replace-objects does not reach the graft table"
    );
    assert_eq!(
        f.quiet(&["-c", "core.useReplaceRefs=false", "log", "--oneline"]).stdout,
        expected,
        "core.useReplaceRefs=false does not reach the graft table"
    );
    let env = f
        .command(&["-c", "advice.graftFileDeprecated=false", "log", "--oneline"])
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .output()
        .expect("run zvcs");
    assert_eq!(
        String::from_utf8_lossy(&env.stdout),
        expected,
        "GIT_NO_REPLACE_OBJECTS does not reach the graft table"
    );
}

/// `repo_get_graft_file()` (repository.c:139-144) reads `$GIT_GRAFT_FILE` in
/// preference to `info/grafts`, and the default lives under the *common* directory
/// so linked worktrees share it.
#[test]
fn the_graft_file_path_follows_git_graft_file() {
    let f = Fixture::new("path");
    let elsewhere = f.path().join("grafts-elsewhere");
    std::fs::write(&elsewhere, format!("{C3}\n")).unwrap();

    // Untouched `info/grafts` is what the default path would have found.
    f.write_grafts(&format!("{C3} {C1}\n"));

    let out = f
        .command(&["-c", "advice.graftFileDeprecated=false", "log", "--oneline"])
        .env("GIT_GRAFT_FILE", &elsewhere)
        .output()
        .expect("run zvcs");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "2a07f7aa50 c4\n864f8e9bf1 c3\n",
        "$GIT_GRAFT_FILE wins over info/grafts"
    );
}

/// `<GIT_DIR>/shallow` feeds the same table through `register_shallow()`
/// (shallow.c:32-45), which registers with `ignore_dups = 0` — so a commit named by
/// both files ends up shallow, and the graft-file line for it is replaced.
///
/// The visible difference is only that both make the commit parentless here; what
/// the test pins is that the two files compose rather than one shadowing the other.
#[test]
fn the_shallow_file_feeds_the_same_table() {
    let f = Fixture::new("shallow");
    std::fs::write(f.path().join(".git").join("shallow"), format!("{C3}\n")).unwrap();

    assert_eq!(
        f.quiet(&["log", "--oneline"]).stdout,
        "2a07f7aa50 c4\n864f8e9bf1 c3\n",
        "a shallow boundary is a graft with no parents"
    );

    // Now add a graft line for a *different* commit: both tables are in effect.
    f.write_grafts(&format!("{C4} {C1}\n"));
    let out = f.quiet(&["log", "--oneline"]);
    assert_eq!(
        out.stdout, "2a07f7aa50 c4\n8beb863f66 c1\n",
        "the graft moves c4's parent to c1, and c3 is no longer reachable"
    );
}

/// The object id no line of this fixture's history carries, used as a graft's
/// parent so the walk is sent at something the repository does not have.
const ABSENT: &str = "0123456789012345678901234567890123456789";

/// `error("Could not read %s")` (commit.c:641-645) followed by
/// `die("Failed to traverse parents of commit %s")` (revision.c:4467-4471).
///
/// Two ids, and they are different ones: the first names the parent that could
/// not be read, the second the commit whose parent list named it.
fn traverse_failure() -> String {
    format!("error: Could not read {ABSENT}\nfatal: Failed to traverse parents of commit {C3}\n")
}

/// A graft that names a parent this repository does not have.
///
/// `lookup_commit_graft()` (commit.c:332-340) substitutes whatever the file said
/// without checking it, so `process_parents()` (revision.c:1189-1194) reaches a
/// parent `repo_parse_commit_gently(r, p, 0)` cannot read. That prints
/// `error("Could not read %s")` (commit.c:641-645) and returns -1, which
/// `get_revision_1()` turns into `die("Failed to traverse parents of commit %s")`
/// (revision.c:4467-4471) naming the *commit*, not the parent.
///
/// The walk streams, so every commit popped before the failure has already been
/// printed: `git log --oneline` shows `c4` and only then dies. What must not
/// happen is following the *real* parent, which would silently show history the
/// graft removed.
#[test]
fn a_graft_to_a_missing_parent_does_not_fall_back_to_the_real_one() {
    let f = Fixture::new("missing");
    f.write_grafts(&format!("{C3} {ABSENT}\n"));

    let out = f.quiet(&["log", "--oneline"]);
    assert!(
        !out.stdout.contains("c2"),
        "c2 is only reachable through the header the graft replaced; got:\n{}",
        out.stdout
    );
    assert!(
        out.stdout.starts_with("2a07f7aa50 c4\n"),
        "the walk still starts at HEAD; got:\n{}",
        out.stdout
    );
    assert_eq!(out.stdout, "2a07f7aa50 c4\n", "and stops before the grafted commit");
    assert_eq!(out.stderr, traverse_failure());
    assert_eq!(out.code, 128);
}

/// `--max-count` spends itself before the walk reaches the unreadable parent, so
/// `get_revision()` is never asked for the commit that would have died on it.
///
/// This is the one shape of the failure that is not a failure at all: git exits 0
/// with no diagnostic, because `cmd_log_walk()`'s loop ended on the cap.
#[test]
fn a_cap_reached_before_the_missing_parent_is_not_an_error() {
    let f = Fixture::new("missing-capped");
    f.write_grafts(&format!("{C3} {ABSENT}\n"));

    for args in [
        &["log", "--oneline", "-n", "1"][..],
        &["rev-list", "--max-count=1", "HEAD"][..],
    ] {
        let out = f.quiet(args);
        assert_eq!(out.code, 0, "{args:?} stderr: {}", out.stderr);
        assert_eq!(out.stderr, "", "{args:?}");
    }

    // One more than the cap does reach it.
    let out = f.quiet(&["log", "--oneline", "-n", "2"]);
    assert_eq!(out.stdout, "2a07f7aa50 c4\n");
    assert_eq!(out.stderr, traverse_failure());
    assert_eq!(out.code, 128);
}

/// Every verb that shares `get_revision()` dies the same way over the same
/// repository, and the ones that summarise rather than stream print nothing.
///
/// `cmd_rev_list()` prints each commit inside the walk loop, so its prefix stands;
/// `--count`, `--quiet` and `cmd_shortlog()` only print once the loop ends, which
/// a `die()` inside the loop never lets happen.
#[test]
fn every_verb_over_the_same_walk_dies_the_same_way() {
    let f = Fixture::new("missing-siblings");
    f.write_grafts(&format!("{C3} {ABSENT}\n"));

    // Streaming: the commits popped before the failure are already out.
    for (args, stdout) in [
        (&["rev-list", "HEAD"][..], format!("{C4}\n")),
        (&["rev-list", "--parents", "HEAD"][..], format!("{C4} {C3}\n")),
        (&["rev-list", "--all"][..], format!("{C4}\n{C1}\n")),
        (&["log", "--oneline", "--all"][..], "2a07f7aa50 c4\n8beb863f66 c1\n".to_string()),
    ] {
        let out = f.quiet(args);
        assert_eq!(out.stdout, stdout, "{args:?}");
        assert_eq!(out.stderr, traverse_failure(), "{args:?}");
        assert_eq!(out.code, 128, "{args:?}");
    }

    // Summarising: nothing is printed at all.
    for args in [
        &["rev-list", "--count", "HEAD"][..],
        &["rev-list", "--quiet", "HEAD"][..],
        &["rev-list", "--reverse", "HEAD"][..],
        &["shortlog", "HEAD"][..],
        &["log", "--oneline", "--reverse"][..],
    ] {
        let out = f.quiet(args);
        assert_eq!(out.stdout, "", "{args:?}");
        assert_eq!(out.stderr, traverse_failure(), "{args:?}");
        assert_eq!(out.code, 128, "{args:?}");
    }
}

/// An order that has to see the whole history first fails during *setup*.
///
/// `--topo-order`, `--date-order` and `--graph` (which implies the first) make
/// `prepare_revision_walk()` run `limit_list()`/`sort_in_topological_order()`
/// before it returns (revision.c:4033-4039), so the read failure happens before
/// the first commit is printed and `builtin/log.c`'s `die(_("revision walk setup
/// failed"))` is what the user sees.
#[test]
fn a_whole_history_order_fails_during_setup_instead() {
    let f = Fixture::new("missing-ordered");
    f.write_grafts(&format!("{C3} {ABSENT}\n"));

    let expected = format!("error: Could not read {ABSENT}\nfatal: revision walk setup failed\n");
    for args in [
        &["log", "--graph", "--oneline"][..],
        &["log", "--oneline", "--topo-order"][..],
        &["log", "--oneline", "--date-order"][..],
        &["rev-list", "--topo-order", "HEAD"][..],
        &["rev-list", "--date-order", "HEAD"][..],
    ] {
        let out = f.quiet(args);
        assert_eq!(out.stdout, "", "{args:?}");
        assert_eq!(out.stderr, expected, "{args:?}");
        assert_eq!(out.code, 128, "{args:?}");
    }
}

/// `merge-base` does not stream, and it does not stop at the parent either.
///
/// `paint_down_to_common()` parses every parent it is about to queue and returns
/// `error(_("could not parse commit %s"))` when one cannot be read
/// (commit-reach.c:171-186), on top of the `error("Could not read %s")` the parse
/// itself printed. Neither is a `die()`, so the exit code is whatever the mode's
/// handler returns — and the five modes disagree: the default one propagates
/// `show_merge_base()`'s -1 (255), `--octopus`, `--is-ancestor` and
/// `--fork-point` end at 128, and `--independent` reaches its ordinary "nothing
/// to show" 1 because `reduce_heads_replace()` discards the failure.
#[test]
fn merge_base_reports_the_parent_it_could_not_read() {
    let f = Fixture::new("missing-mergebase");
    f.write_grafts(&format!("{C3} {ABSENT}\n"));

    let expected =
        format!("error: Could not read {ABSENT}\nerror: could not parse commit {ABSENT}\n");
    for (args, code) in [
        (&["merge-base", "HEAD", "side"][..], 255),
        (&["merge-base", "main", "main~1"][..], 255),
        (&["merge-base", "--octopus", "HEAD", "side"][..], 128),
        (&["merge-base", "--is-ancestor", "side", "HEAD"][..], 128),
        (&["merge-base", "--fork-point", "side", "main"][..], 128),
        (&["merge-base", "--independent", "main", "side"][..], 1),
    ] {
        let out = f.quiet(args);
        assert_eq!(out.stdout, "", "{args:?}");
        assert_eq!(out.stderr, expected, "{args:?}");
        assert_eq!(out.code, code, "{args:?}");
    }

    // `merge_bases_many()` short-circuits when an operand *is* the other
    // (commit-reach.c:206-215), so the walk never starts and nothing is reported.
    let out = f.quiet(&["merge-base", "main", "main"]);
    assert_eq!(out.stdout, format!("{C4}\n"));
    assert_eq!(out.stderr, "");
    assert_eq!(out.code, 0);
}

/// A pathspec puts `try_to_simplify_commit()` (revision.c:1182) ahead of the
/// parent loop, and its tree diff hits the unreadable parent first — so the
/// ending is `die("cannot simplify commit %s (because of %s)")`
/// (revision.c:1034-1037), which names both ids in one line.
#[test]
fn a_pathspec_dies_in_the_simplification_instead() {
    let f = Fixture::new("missing-pathspec");
    f.write_grafts(&format!("{C3} {ABSENT}\n"));

    let expected = format!(
        "error: Could not read {ABSENT}\nfatal: cannot simplify commit {C3} (because of {ABSENT})\n"
    );
    for (args, stdout) in [
        (&["log", "--oneline", "--", "f"][..], "2a07f7aa50 c4\n"),
        (&["rev-list", "HEAD", "--", "f"][..], "2a07f7aa502f1ce3b0084b7f36bbc92431ffe179\n"),
        (&["shortlog", "HEAD", "--", "f"][..], ""),
    ] {
        let out = f.quiet(args);
        assert_eq!(out.stdout, stdout, "{args:?}");
        assert_eq!(out.stderr, expected, "{args:?}");
        assert_eq!(out.code, 128, "{args:?}");
    }
}
