//! `git log --graph` over a history whose parents are not all shown, pinned
//! against stock git 2.55.0.
//!
//! The graph draws a lane per *interesting* parent — `graph_is_interesting()`
//! (graph.c:457), which asks `get_commit_action()` the same question the walk
//! asks of every commit it prints. A parent dropped by `--merges`, `--no-merges`,
//! a `--grep`, a date limit or a `^rev` exclusion is therefore not drawn, and the
//! merge naming it renders as an ordinary commit even though its `Merge:` header
//! and `%P` still list the parent.
//!
//! Two more shapes belong to the same machinery and are covered here:
//!
//!   * `--first-parent` makes `next_interesting_parent()` return nothing, so a
//!     merge whose *first* parent was filtered out draws no lane at all;
//!   * a commit the walk reached but printed nothing for — a `-S`/`-G` miss, or a
//!     `whatchanged` record with an empty diff — still moves the columns on,
//!     because git runs `graph_update()` from `get_revision()` and draws the rows
//!     from `log_tree_commit()`. The gap shows up as the `...` skip row.
//!
//! Every expectation was captured from stock `git log --graph` on the very
//! repository each test builds. git pads each graph row to the width of the
//! widest row that commit can produce, so trailing spaces are part of the output;
//! they are written as `·` and substituted back in [`rows`].

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

fn scratch(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-graphint-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    git(&repo, &home, "2005-04-01T00:00:00 +0000", &["init", "-q", "-b", "main"]);
    (root, repo, home)
}

/// `base` ← `s1` on `side` and `m1` on `main`, joined by a two-parent `mrg`, then
/// `tip`. Every commit carries a distinct subject and a distinct date, so any
/// single one of them can be filtered out by name or by date.
fn merge_repo(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (root, repo, home) = scratch(tag);
    commit(&repo, &home, "2005-04-01T00:00:00 +0000", "base");
    git(&repo, &home, "2005-04-02T00:00:00 +0000", &["checkout", "-q", "-b", "side"]);
    commit(&repo, &home, "2005-04-02T00:00:00 +0000", "s1");
    git(&repo, &home, "2005-04-03T00:00:00 +0000", &["checkout", "-q", "main"]);
    commit(&repo, &home, "2005-04-03T00:00:00 +0000", "m1");
    git(
        &repo,
        &home,
        "2005-04-04T00:00:00 +0000",
        &["merge", "-q", "--no-ff", "--no-edit", "-m", "mrg", "side"],
    );
    commit(&repo, &home, "2005-04-05T00:00:00 +0000", "tip");
    (root, repo, home)
}

/// `base` ← `b1`, `b2`, `b3`, joined by one octopus merge, then `mtip`.
fn octopus_repo(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (root, repo, home) = scratch(tag);
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
        &["merge", "-q", "--no-edit", "-m", "octo", "b1", "b2", "b3"],
    );
    commit(&repo, &home, "2005-04-10T00:00:00 +0000", "mtip");
    (root, repo, home)
}

#[test]
fn parent_count_limits_close_the_merges_lanes() {
    let (root, repo, home) = merge_repo("parentcount");

    // `--merges` is `--min-parents=2`, which drops both of `mrg`'s parents. With no
    // interesting parent left the merge draws as an ordinary commit: no `|\` row,
    // and a two-column-wide `* ` rather than the four `*   ` a merge carries.
    let want = rows(&["* mrg"]);
    for args in [
        ["log", "--graph", "--pretty=format:%s", "--merges", "--all"],
        ["log", "--graph", "--pretty=format:%s", "--min-parents=2", "--all"],
    ] {
        let got = out(&repo, &home, &args);
        assert_eq!(got, want, "{args:?} drew a lane for a filtered parent:\n{got}");
    }

    // The merge itself is what `--no-merges` drops, so `tip`'s only parent stops
    // being interesting and its lane closes — `s1` takes the column back over.
    let got = out(&repo, &home, &["log", "--graph", "--pretty=format:%s", "--no-merges", "--all"]);
    let want = rows(&["* tip", "* s1", "| * m1", "|/··", "* base"]);
    assert_eq!(got, want, "--no-merges graph drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn message_and_date_limits_close_the_merges_lanes() {
    let (root, repo, home) = merge_repo("grepdate");

    // `--grep` keeps `mrg` and `s1`; `m1` is filtered, so the merge fans out to one
    // parent and draws no `|\`.
    let got = out(
        &repo,
        &home,
        &["log", "--graph", "--pretty=format:%s", "--grep=s1", "--grep=mrg", "--all"],
    );
    assert_eq!(got, rows(&["* mrg", "* s1"]), "--grep graph drifted:\n{got}");

    // `--since` cuts `s1` and `base` off by date, which leaves `mrg` with only its
    // first parent.
    let got = out(
        &repo,
        &home,
        &["log", "--graph", "--pretty=format:%s", "--since=2005-04-03T00:00:00+0000", "--all"],
    );
    assert_eq!(got, rows(&["* tip", "* mrg", "* m1"]), "--since graph drifted:\n{got}");

    // A `^rev` exclusion marks `s1` UNINTERESTING, which `get_commit_action()`
    // rejects just as flatly.
    let got = out(&repo, &home, &["log", "--graph", "--pretty=format:%s", "^side", "main"]);
    assert_eq!(got, rows(&["* tip", "* mrg", "* m1"]), "^rev graph drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn boundary_commits_keep_the_lane_their_children_marked() {
    let (root, repo, home) = merge_repo("boundary");

    // `--boundary` marks every parent of a commit the walk returned CHILD_SHOWN,
    // and `graph_is_interesting()` accepts that flag on its own — so `base`, hidden
    // by the `side..` exclusion, still gets the lane its `o` row sits in, and the
    // `o s1` above it collapses into that lane rather than closing over an empty
    // column.
    let got =
        out(&repo, &home, &["log", "--graph", "--pretty=format:%s", "--boundary", "side..main"]);
    let want = rows(&["* tip", "*   mrg", "|\\··", "* | m1", "| o s1", "|/··", "o base"]);
    assert_eq!(got, want, "--boundary graph drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn first_parent_draws_nothing_when_that_parent_is_filtered() {
    let (root, repo, home) = merge_repo("firstparent");

    // `--first-parent` leaves `first_interesting_parent()` no fallback: `mrg`'s
    // first parent `m1` is filtered out by the `--grep`, and `next_interesting_parent()`
    // returns nothing under `--first-parent`, so the merge draws no lane at all.
    let got = out(
        &repo,
        &home,
        &[
            "log",
            "--graph",
            "--pretty=format:%s",
            "--first-parent",
            "--grep=mrg",
            "--grep=tip",
            "--all",
        ],
    );
    assert_eq!(got, rows(&["* tip", "* mrg"]), "--first-parent graph drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_octopus_fans_out_only_to_its_interesting_parents() {
    let (root, repo, home) = octopus_repo("partial");

    // Two of the three parents survive the `--grep`, so the octopus loses its
    // horizontal `-`…`.` run and its third `\`: it is drawn as the two-parent merge
    // it now is.
    let got = out(
        &repo,
        &home,
        &[
            "log",
            "--graph",
            "--pretty=format:%s",
            "--grep=octo",
            "--grep=b1",
            "--grep=b3",
            "--grep=mtip",
            "--all",
        ],
    );
    let want = rows(&["* mtip", "*   octo", "|\\··", "| * b3", "* b1"]);
    assert_eq!(got, want, "octopus with two interesting parents drifted:\n{got}");

    // One parent left: no fan-out row at all, and the merge's column is two wide.
    let got = out(
        &repo,
        &home,
        &[
            "log",
            "--graph",
            "--pretty=format:%s",
            "--grep=octo",
            "--grep=b2",
            "--grep=mtip",
            "--all",
        ],
    );
    assert_eq!(got, rows(&["* mtip", "* octo", "* b2"]), "octopus with one parent drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_commit_that_prints_nothing_still_moves_the_columns() {
    let (root, repo, home) = merge_repo("pickaxe");

    // `-S` is a diff filter, not a walk filter: git walks `tip`, `mrg` and `other`,
    // runs `graph_update()` on each and prints none of them, so the graph never
    // reaches padding and the one commit that does print opens with the `...` skip
    // row — in the second column, where the walk had already moved.
    let got =
        out(&repo, &home, &["log", "--graph", "--pretty=format:%s", "-S", "s1", "--all"]);
    assert_eq!(got, rows(&["...·", "| * s1"]), "-S skip row drifted:\n{got}");

    let got =
        out(&repo, &home, &["log", "--graph", "--pretty=format:%s", "-G", "s1", "--all"]);
    assert_eq!(got, rows(&["...·", "| * s1"]), "-G skip row drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn whatchanged_graph_skips_the_row_of_a_commit_with_no_diff() {
    let (root, repo, home) = merge_repo("whatchanged");

    // `whatchanged` prints nothing for a commit whose diff queue came out empty —
    // the merge here — and hands the `--max-count` slot back. The merge still went
    // through `graph_update()`, so the commit below it opens with the `...` row.
    let got = out(
        &repo,
        &home,
        &[
            "whatchanged",
            "--i-still-use-this",
            "--graph",
            "--pretty=format:%s",
            "--name-only",
            "--all",
        ],
    );
    let want = rows(&[
        "* tip|·",
        "| ftip",
        "",
        "...·",
        "| * s1| |·",
        "| | fs1",
        "",
        "* | m1",
        "|/  |···",
        "|   fm1",
        "",
        "* base··",
        "  fbase",
        "",
    ]);
    assert_eq!(got, want, "whatchanged graph drifted:\n{got}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn no_walk_and_graph_are_refused() {
    let (root, repo, home) = merge_repo("nowalk");

    // revision.c:3197 — the graph lays its columns out by following parents into
    // the walk, and `--no-walk` yields the named commits alone.
    let o = run(&repo, &home, "2005-04-20T00:00:00 +0000", &["log", "--graph", "--no-walk", "main"]);
    assert_eq!(o.status.code(), Some(128), "--no-walk --graph exit code");
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "fatal: options '--no-walk' and '--graph' cannot be used together\n"
    );

    let _ = std::fs::remove_dir_all(root);
}
