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

/// A graft that names a parent this repository does not have.
///
/// git's `lookup_commit()` pre-creates the node and the walk then dies with
/// `error: Could not read <oid>` / `fatal: Failed to traverse parents of commit
/// <oid>` (commit.c:644). zvcs stops the walk at the missing parent instead of
/// dying; what must not happen either way is following the *real* parent, which
/// would silently show history the graft removed.
#[test]
fn a_graft_to_a_missing_parent_does_not_fall_back_to_the_real_one() {
    let f = Fixture::new("missing");
    let absent = "0123456789012345678901234567890123456789";
    f.write_grafts(&format!("{C3} {absent}\n"));

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
}
