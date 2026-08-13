//! `git log --graph` over merges with three or more parents, pinned against stock
//! git 2.55.0.
//!
//! Every expectation below was captured from stock `git log --graph` run on the
//! very repository each test builds — same commits, same dates, same order — so a
//! drift here is a drift from git.
//!
//! The rows an octopus adds over a two-way merge are all covered:
//!
//!   * the commit row's horizontal `-`…`.` run reaching the parents past the
//!     second (`graph_draw_octopus_merge`);
//!   * the `|\ \ \` post-merge row, one character per parent
//!     (`graph_output_post_merge_line`);
//!   * the expansion rows that open space around a merge with a branch line to its
//!     right (`graph_output_pre_commit_line`) — only drawn when the merge is not
//!     the rightmost column, hence the `--date-order` case, since the default
//!     `--graph` ordering finishes one lane before switching to another;
//!   * the padding row git prints in front of the blank line separating two
//!     records (`graph_padding_line`), which widens the octopus's own column to
//!     cover the dashes its commit row is about to carry.
//!
//! git pads every graph row to the width of the widest row that commit can
//! produce, so trailing spaces are part of the output. They are written here as
//! `·` and substituted back in [`rows`], where no editor or lint can eat them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Join expected graph rows, turning each `·` back into the space git padded with.
fn rows(lines: &[&str]) -> String {
    lines.join("\n").replace('·', " ")
}

fn run(dir: &Path, home: &Path, date: &str, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@e")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@e")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .expect("run binary")
}

fn git(dir: &Path, home: &Path, date: &str, args: &[&str]) {
    let o = run(dir, home, date, args);
    assert!(
        o.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}

fn out(dir: &Path, home: &Path, args: &[&str]) -> String {
    let o = run(dir, home, "2005-04-20T00:00:00 +0000", args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn commit(repo: &Path, home: &Path, date: &str, msg: &str) {
    std::fs::write(repo.join(format!("f{msg}")), format!("{msg}\n")).unwrap();
    git(repo, home, date, &["add", &format!("f{msg}")]);
    git(repo, home, date, &["commit", "-q", "-m", msg]);
}

/// `base` ← `b1`, `b2`, `b3`, joined by one octopus merge on `main`, then a tip
/// commit and a branch rooted back at `base` whose date falls between the two —
/// which is what keeps a lane open to the right of the merge under `--date-order`.
fn octopus_repo(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-graphoct-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));

    git(&repo, &home, "2005-04-01T00:00:00 +0000", &["init", "-q", "-b", "main"]);
    commit(&repo, &home, "2005-04-01T00:00:00 +0000", "base");
    for b in ["b1", "b2", "b3"] {
        git(&repo, &home, "2005-04-02T00:00:00 +0000", &["checkout", "-q", "-b", b, "main"]);
        commit(&repo, &home, "2005-04-02T00:00:00 +0000", b);
    }
    git(&repo, &home, "2005-04-07T00:00:00 +0000", &["checkout", "-q", "main"]);
    git(
        &repo,
        &home,
        "2005-04-07T00:00:00 +0000",
        &["merge", "-q", "--no-edit", "b1", "b2", "b3", "-m", "octo"],
    );
    commit(&repo, &home, "2005-04-10T00:00:00 +0000", "mtip");
    let base = out(&repo, &home, &["rev-list", "--max-parents=0", "main"]).trim().to_string();
    git(&repo, &home, "2005-04-09T00:00:00 +0000", &["checkout", "-q", "-b", "t1", &base]);
    commit(&repo, &home, "2005-04-09T00:00:00 +0000", "t1");
    (root, repo, home)
}

#[test]
fn octopus_commit_row_and_post_merge_row_match_git() {
    let (root, repo, home) = octopus_repo("plain");

    // The merge row carries `*-.` — one dash pair per parent past the second — and
    // the row under it fans out `|\ \`, one character per parent.
    let got = out(&repo, &home, &["log", "--graph", "--pretty=format:%s", "--all"]);
    let want = rows(&[
        "* mtip",
        "*-.   octo",
        "|\\ \\··",
        "| | * b3",
        "| * | b2",
        "| |/··",
        "* / b1",
        "|/··",
        "| * t1",
        "|/··",
        "* base",
    ]);
    assert_eq!(got, want, "octopus graph drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn octopus_expansion_rows_match_git() {
    let (root, repo, home) = octopus_repo("expand");

    // With `t1`'s lane still open to the right of the merge, git widens the space
    // around it first: two expansion rows per dashed parent, each drawn on a line
    // of its own before the commit row.
    let got =
        out(&repo, &home, &["log", "--graph", "--date-order", "--pretty=format:%s", "--all"]);
    let want = rows(&[
        "* mtip",
        "| * t1",
        "| |·····",
        "|  \\····",
        "*-. \\   octo",
        "|\\ \\ \\··",
        "* | | | b1",
        "| |_|/··",
        "|/| |···",
        "| * | b2",
        "|/ /··",
        "| * b3",
        "|/··",
        "* base",
    ]);
    assert_eq!(got, want, "octopus expansion rows drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn octopus_under_first_parent_draws_one_lane() {
    let (root, repo, home) = octopus_repo("firstparent");

    // `--first-parent` makes every parent but the first uninteresting
    // (`next_interesting_parent()` returns nothing at all), so the octopus draws as
    // an ordinary single-lane commit: no dashes, no fan-out row.
    let got =
        out(&repo, &home, &["log", "--graph", "--first-parent", "--pretty=format:%s", "main"]);
    let want = rows(&["* mtip", "* octo", "* b1", "* base"]);
    assert_eq!(got, want, "--first-parent octopus graph drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn terminator_formats_close_the_record_after_the_graph_rows() {
    let (root, repo, home) = octopus_repo("terminator");

    // A terminator format ends each record *after* the commit's remaining graph
    // rows (log-tree.c:915-919). Under `-z` the terminator is a NUL, which makes
    // the ordering visible: `octo` is followed by its `|\ \` row and only then by
    // the NUL that closes the record.
    let got = out(&repo, &home, &["log", "--graph", "-z", "--pretty=tformat:%s", "--all"]);
    let want = "* mtip\0*-.   octo\n|\\ \\  \0| | * b3\0| * | b2\n| |/  \0\
                * / b1\n|/  \0| * t1\n|/  \0* base\0";
    assert_eq!(got, want, "-z terminator placement drifted: {got:?}");

    // When the record's own text ends in a newline, git puts a padding row in
    // front of the terminator so the blank line does not read as a hole.
    let got = out(&repo, &home, &["log", "--graph", "--pretty=tformat:%s%n", "--all"]);
    let want = rows(&[
        "* mtip",
        "|·",
        "*-.   octo",
        "|\\ \\··",
        "| | |·",
        "| | * b3",
        "| | |·",
        "| * | b2",
        "| |/··",
        "| |···",
        "* / b1",
        "|/··",
        "|···",
        "| * t1",
        "|/··",
        "|···",
        "* base",
        "··",
        "",
    ]);
    assert_eq!(got, want, "padding row before the terminator drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn octopus_record_separator_row_matches_git() {
    let (root, repo, home) = octopus_repo("multiline");

    // A separator format puts a blank line between records; git prints that line
    // only after the *next* commit has been through `graph_update()`, and puts its
    // padding row in front of it. For an octopus that row widens the merge's own
    // column by two spaces per dashed parent, so the row above `*-.   commit …` is
    // `|` plus five spaces, not the `|` plus one the previous commit's own rows use.
    let got = out(&repo, &home, &["log", "--graph", "--pretty=medium", "--all"]);
    let lines: Vec<&str> = got.lines().collect();
    let merge_row = lines
        .iter()
        .position(|l| l.starts_with("*-.   commit "))
        .expect(&format!("no octopus commit row:\n{got}"));
    assert!(merge_row > 0, "octopus commit row is first:\n{got}");
    assert_eq!(lines[merge_row - 1], "|     ", "separator row drifted:\n{got}");
    assert!(lines[merge_row + 1].starts_with("|\\ \\  Merge: "), "no merge row:\n{got}");
    // The commit's own body rows stay one column wide.
    assert!(lines.contains(&"|     mtip"), "mtip body row drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}
