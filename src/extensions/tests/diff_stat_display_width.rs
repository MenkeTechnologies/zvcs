//! The diffstat's geometry, which used to be nine hand-copied ports of
//! `diff.c`'s `show_stats()` giving three different answers, and the two
//! `diff-tree` exit paths that went with them.
//!
//! Every expectation here was read off stock git 2.55.0 on this fixture before
//! it was written down. The guards, in order:
//!
//!   * the name column is measured in **display columns** (`utf8_strwidth()`),
//!     not bytes and not Unicode scalars — a `café` name is one column narrower
//!     than its byte length and a CJK name is one column *wider* per glyph;
//!   * `--stat-count` truncates the listing, prints ` ...`, and narrows the
//!     columns because the geometry scan stops at the same place;
//!   * the total width is `term_columns()` for the porcelain (`diff`, `log`,
//!     `show`) and a flat 80 for the plumbing (`diff-tree`), because only
//!     `builtin/diff.c` and `builtin/log.c` call `init_diffstat_widths()`;
//!   * `$COLUMNS` is read with C's `atoi`, so a `+` sign is accepted;
//!   * `-I<regex>` drops a pair from the **raw** format too, not just from
//!     `--stat` (`diff_flush()`'s `diff_from_contents` gate);
//!   * `diff-tree`'s routed render propagates `diff-pairs`' exit code, and
//!     `--quiet` stops the tree walk at the first changed path the way
//!     `diff_can_quit_early()` does.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A long ASCII path, chosen so the name column overflows an 80-column budget
/// and not a 100-column one — that difference is what makes `$COLUMNS` visible.
const LONG: &str = "deep/deeper/deepest/a-long-ascii-file-name-for-the-column-budget.txt";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Four files whose names span the three width regimes — pure ASCII, Latin-1
/// multibyte (bytes > columns), and CJK (columns > scalars) — plus one path
/// long enough to be elided at 80 columns and not at 100.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-statwidth-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(repo.join("deep/deeper/deepest")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));

    git(&repo, &["init", "-q", "-b", "main"]);
    for f in ["café-naïve.txt", "日本語.txt", "plain.txt", LONG] {
        std::fs::write(repo.join(f), "a\n").unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);

    std::fs::write(repo.join("café-naïve.txt"), "a\nb\nc\n").unwrap();
    std::fs::write(repo.join("日本語.txt"), "a\nb\n").unwrap();
    std::fs::write(
        repo.join("plain.txt"),
        "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\no\np\nq\nr\ns\nt\n",
    )
    .unwrap();
    std::fs::write(repo.join(LONG), "a\nb\nc\nd\ne\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "c1"]);
    (repo, home)
}

/// Run `args` with `$COLUMNS` set to `columns` (or unset when `None`).
fn run(repo: &Path, home: &Path, columns: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("COLUMNS");
    if let Some(c) = columns {
        cmd.env("COLUMNS", c);
    }
    cmd.output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Byte-for-byte stock output for `-c core.quotePath=false diff --stat HEAD~1 HEAD`
/// at 80 columns. `café-naïve.txt` is 15 bytes and 14 columns; `日本語.txt` is 13
/// bytes, 6 scalars and 10 columns. Counting bytes moves the first `|` right by
/// one, counting scalars moves it left by three — only display columns line all
/// four rows up on the same `|`.
const STAT_80: &str = "\
\x20café-naïve.txt                                        |  2 ++
 .../a-long-ascii-file-name-for-the-column-budget.txt  |  4 ++++
 plain.txt                                             | 19 +++++++++++++++++++
 日本語.txt                                            |  1 +
 4 files changed, 26 insertions(+)
";

/// The same diff at 100 columns: the long path now fits whole, so nothing is
/// elided and the column moves right.
const STAT_100: &str = "\
\x20café-naïve.txt                                                       |  2 ++
 deep/deeper/deepest/a-long-ascii-file-name-for-the-column-budget.txt |  4 ++++
 plain.txt                                                            | 19 +++++++++++++++++++
 日本語.txt                                                           |  1 +
 4 files changed, 26 insertions(+)
";

#[test]
fn stat_name_column_is_display_columns() {
    let (repo, home) = fixture("cols");
    let o = run(
        &repo,
        &home,
        Some("80"),
        &["-c", "core.quotePath=false", "diff", "--stat", "HEAD~1", "HEAD"],
    );
    assert_eq!(stdout(&o), STAT_80);

    // The same geometry reaches `log` and `show`, which share the port.
    let l = run(
        &repo,
        &home,
        Some("80"),
        &["-c", "core.quotePath=false", "log", "--format=", "--stat", "-1"],
    );
    assert_eq!(stdout(&l), STAT_80);
    let s = run(
        &repo,
        &home,
        Some("80"),
        &["-c", "core.quotePath=false", "show", "--format=", "--stat", "HEAD"],
    );
    assert_eq!(stdout(&s), STAT_80);
}

#[test]
fn porcelain_scales_to_columns_and_plumbing_does_not() {
    let (repo, home) = fixture("term");
    let stat = ["-c", "core.quotePath=false", "diff", "--stat", "HEAD~1", "HEAD"];
    assert_eq!(stdout(&run(&repo, &home, Some("100"), &stat)), STAT_100);

    // `atoi("+100") == 100`. A reader that takes only the leading digit run
    // rejects the sign and silently falls back to 80.
    assert_eq!(stdout(&run(&repo, &home, Some("+100"), &stat)), STAT_100);
    // …and `atoi` ignores trailing junk, so this is 100 as well.
    assert_eq!(stdout(&run(&repo, &home, Some("100junk"), &stat)), STAT_100);
    // A non-positive or unparseable value is the 80-column default.
    assert_eq!(stdout(&run(&repo, &home, Some("-5"), &stat)), STAT_80);
    assert_eq!(stdout(&run(&repo, &home, Some("nope"), &stat)), STAT_80);

    // `diff-tree` never calls `init_diffstat_widths()`, so it renders at a flat
    // 80 columns whatever `$COLUMNS` says.
    let tree = ["-c", "core.quotePath=false", "diff-tree", "-r", "--stat", "HEAD~1", "HEAD"];
    assert_eq!(stdout(&run(&repo, &home, Some("100"), &tree)), STAT_80);
    assert_eq!(stdout(&run(&repo, &home, Some("40"), &tree)), STAT_80);
}

#[test]
fn stat_count_truncates_and_narrows_the_scan() {
    let (repo, home) = fixture("count");
    let o = run(
        &repo,
        &home,
        Some("80"),
        &["-c", "core.quotePath=false", "diff", "--stat-count=2", "--stat", "HEAD~1", "HEAD"],
    );
    // Only the first two rows are listed and ` ...` marks the cut — but the
    // *geometry* scan stopped there too, so the long path is no longer elided
    // (its 68 columns now fit the budget the two rows have to share) and the
    // number column narrowed from 2 to 1. The summary still counts all four.
    assert_eq!(
        stdout(&o),
        "\
\x20café-naïve.txt                                                       | 2 ++
 deep/deeper/deepest/a-long-ascii-file-name-for-the-column-budget.txt | 4 ++++
 ...
 4 files changed, 26 insertions(+)
"
    );
}

#[test]
fn stat_width_flag_overrides_the_terminal() {
    let (repo, home) = fixture("width");
    let o = run(
        &repo,
        &home,
        Some("100"),
        &["-c", "core.quotePath=false", "diff", "--stat-width=40", "--stat", "HEAD~1", "HEAD"],
    );
    // The elision walks glyphs off the front until the remaining *display*
    // width fits, then resumes at the first `/` inside what is left.
    assert_eq!(
        stdout(&o),
        "\
\x20café-naïve.txt            |  2 +
 ...-the-column-budget.txt |  4 ++
 plain.txt                 | 19 +++++++
 日本語.txt                |  1 +
 4 files changed, 26 insertions(+)
"
    );
}

#[test]
fn ignore_matching_lines_drops_pairs_from_the_raw_format() {
    let (repo, home) = fixture("ignore");
    // Every line these four pairs add matches, so `diff_from_contents` makes
    // `diff_flush()`'s raw loop drop all of them and print nothing at all. The
    // `--stat` path already did this; the raw path did not, so the two formats
    // disagreed with each other.
    let o = run(
        &repo,
        &home,
        None,
        &["diff-tree", "-r", "--name-only", "-I", "^[b-t]$", "HEAD~1", "HEAD"],
    );
    assert_eq!(stdout(&o), "");
    assert_eq!(o.status.code(), Some(0));

    // A pattern that leaves something unmatched keeps every pair.
    let kept = run(
        &repo,
        &home,
        None,
        &["diff-tree", "-r", "--name-only", "-I", "^zzz$", "HEAD~1", "HEAD"],
    );
    assert_eq!(stdout(&kept).lines().count(), 4);
}

#[test]
fn invalid_ignore_regex_is_parse_options_error_129() {
    let (repo, home) = fixture("badre");
    let o = run(&repo, &home, None, &["diff-tree", "-r", "-I", "[", "HEAD~1", "HEAD"]);
    // `diff_opt_ignore_regex()` quotes the *pattern*, not the engine's complaint,
    // and answers parse-options' 129 rather than dying.
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "error: invalid regex given to -I: '['\n"
    );
    assert_eq!(o.status.code(), Some(129));
}

#[test]
fn routed_diff_tree_propagates_the_render_exit_code() {
    let (repo, home) = fixture("route");
    // `-G` is rendered by the routed `diff-pairs`, and only it can report the
    // unusable pattern. The status has to travel back: this exited 0 while
    // printing the fatal, because the routed code was being discarded.
    let o = run(&repo, &home, None, &["diff-tree", "-r", "-G", "[", "HEAD~1", "HEAD"]);
    assert_eq!(o.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&o.stderr).starts_with("fatal: invalid regex:"),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn quiet_stops_the_walk_at_the_first_changed_path() {
    let (repo, home) = fixture("quick");
    let quiet = |args: &[&str]| {
        let mut v = vec!["diff-tree", "-r"];
        v.extend_from_slice(args);
        v.extend_from_slice(&["HEAD~1", "HEAD"]);
        run(&repo, &home, None, &v).status.code()
    };

    // `café-naïve.txt` sorts first and gains `b` and `c`; `q` only appears in
    // `plain.txt`, further along. Under `--quiet` the walk stops after the first
    // queued pair, so the pickaxe never sees `plain.txt` and the answer is 0.
    assert_eq!(quiet(&["--quiet", "-Sb"]), Some(1));
    assert_eq!(quiet(&["--quiet", "-Sq"]), Some(0));
    // A string in no file at all is 0 either way — the case that used to answer
    // 1 because the exit status came from the walk rather than from the queue
    // the pickaxe left behind.
    assert_eq!(quiet(&["--quiet", "-Szzz"]), Some(0));

    // `-s --exit-code` is not `--quiet`: `opt->flags.quick` stays clear, the
    // whole tree is walked, and `plain.txt` is reached.
    assert_eq!(quiet(&["-s", "--exit-code", "-Sq"]), Some(1));
    // `diff_can_quit_early()` also requires no `--diff-filter` …
    assert_eq!(quiet(&["--quiet", "--diff-filter=M", "-Sq"]), Some(1));
    // … and no `diff_from_contents`, which `-w` turns on.
    assert_eq!(quiet(&["--quiet", "-w", "-Sq"]), Some(1));

    // With no pickaxe, `--quiet` still reports the difference.
    assert_eq!(quiet(&["--quiet"]), Some(1));
}
