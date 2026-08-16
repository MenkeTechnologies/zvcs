//! The pending list `setup_revisions()` builds, and what the three commands that
//! read it do with the order.
//!
//! Four behaviours that used to differ from stock git meet in one fixture:
//!
//! 1. **`git show <range>` walked breadth-first.** `cmd_show` hands its pending
//!    list to `cmd_log_walk`, so the traversal is `git log`'s commit-date-ordered
//!    frontier. gitoxide's `rev_walk` defaults to `Sorting::BreadthFirst`, which
//!    orders a merge's two lanes by graph distance instead of by clock.
//! 2. **`--no-walk` painted UNINTERESTING over the whole history.**
//!    `prepare_revision_walk()` returns before `limit_list()` when `no_walk`
//!    survived, so nothing paints the flag: only what `mark_parents_uninteresting()`
//!    already reached is dropped, and that walk stops at a commit whose parents are
//!    not loaded yet (revision.c:262-269).
//! 3. **`--branches`/`--tags`/`--remotes`/`--glob`/`--exclude` and `<rev>^@`/
//!    `<rev>^!`** were unserved by one or more of `log`, `rev-list`, `show` and
//!    `bundle create`, all of which reach the same `setup_revisions()`.
//! 4. **`rev-list --abbrev-commit --abbrev=<n>`** was a usage error.
//!
//! Every expectation below was read off stock git 2.55.0 on this exact fixture
//! before it was written down. Commit dates are pinned and deliberately *not*
//! monotonic with the graph, because a fixture where date order and breadth-first
//! order agree cannot tell the two apart.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// ```text
    /// *   m    1000000400  (main, HEAD)
    /// |\
    /// | * s1   1000000350  (side)
    /// * | c1   1000000300  (v1)
    /// |/
    /// * c0     1000000100
    /// ```
    ///
    /// `refs/remotes/origin/main` mirrors `main`. The point of the dates is that
    /// `s1` is *newer* than `c1` while sitting one step further from `m` along the
    /// second parent, so commit-date order (`m s1 c1`) and breadth-first order
    /// (`m c1 s1`) disagree.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-pending-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.git_at(&["init", "-q", "-b", "main", "."], 1_000_000_100);
        f.git_at(&["config", "user.email", "t@e.co"], 1_000_000_100);
        f.git_at(&["config", "user.name", "t"], 1_000_000_100);
        f.commit("c0", "f", 1_000_000_100);
        f.commit("c1", "f", 1_000_000_300);
        f.git_at(&["tag", "v1"], 1_000_000_300);
        f.git_at(&["checkout", "-q", "-b", "side", "HEAD~1"], 1_000_000_350);
        f.commit("s1", "g", 1_000_000_350);
        f.git_at(&["checkout", "-q", "main"], 1_000_000_400);
        f.git_at(&["merge", "-q", "--no-ff", "-m", "m", "side"], 1_000_000_400);
        f.git_at(&["update-ref", "refs/remotes/origin/main", "refs/heads/main"], 1_000_000_400);
        f
    }

    fn cmd(&self, args: &[&str], at: i64) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", format!("{at} +0000"))
            .env("GIT_COMMITTER_DATE", format!("{at} +0000"));
        c
    }

    fn git_at(&self, args: &[&str], at: i64) {
        let out = self.cmd(args, at).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn commit(&self, msg: &str, path: &str, at: i64) {
        std::fs::write(self.repo.join(path), format!("{msg}\n")).unwrap();
        self.git_at(&["add", path], at);
        self.git_at(&["commit", "-q", "-m", msg], at);
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.cmd(args, 1_000_000_400).output().unwrap()
    }

    /// stdout of a successful run, split into lines.
    fn lines(&self, args: &[&str]) -> Vec<String> {
        let out = self.run(args);
        assert_eq!(out.status.code(), Some(0), "`git {args:?}`: {out:?}");
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    }

    /// The `%s` subjects a rev-listing command prints, `rev-list --format`'s
    /// `commit <oid>` header lines dropped.
    fn subjects(&self, args: &[&str]) -> Vec<String> {
        self.lines(args).into_iter().filter(|l| !l.starts_with("commit ")).collect()
    }

    fn bundle_path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// The `<oid> <ref>` lines of a bundle header.
    fn bundle_heads(&self, file: &Path) -> Vec<String> {
        self.lines(&["bundle", "list-heads", file.to_str().unwrap()])
    }
}

/// `git show <range>` orders the walk by commit date, not by graph distance.
///
/// `s1` is newer than `c1` and further from `m`, so a breadth-first walk prints
/// `m c1 s1` where git prints `m s1 c1` — and `git log` over the same range has
/// always printed the latter, which is the drift this closes.
#[test]
fn show_walks_a_range_in_commit_date_order() {
    let f = Fixture::new("order");

    assert_eq!(f.subjects(&["show", "-s", "--format=%s", "main~2..main"]), ["m", "s1", "c1"]);
    // The same walk, spelled with an exclusion rather than a range.
    assert_eq!(f.subjects(&["show", "-s", "--format=%s", "main", "^main~2"]), ["m", "s1", "c1"]);
    // …and it agrees with `git log`, which is the whole point.
    assert_eq!(
        f.subjects(&["log", "--format=%s", "main~2..main"]),
        f.subjects(&["show", "-s", "--format=%s", "main~2..main"])
    );
    // `--topo-order` and `--date-order` reach `show` too; on this history both
    // agree with the plain walk, which is what stock prints.
    assert_eq!(
        f.subjects(&["show", "-s", "--format=%s", "--topo-order", "main~2..main"]),
        ["m", "s1", "c1"]
    );
    assert_eq!(
        f.subjects(&["show", "-s", "--format=%s", "--date-order", "main~2..main"]),
        ["m", "s1", "c1"]
    );
    // `--reverse` reverses what the walk produced.
    assert_eq!(
        f.subjects(&["show", "-s", "--format=%s", "--reverse", "main~2..main"]),
        ["c1", "s1", "m"]
    );
}

/// `--no-walk` is positional against every spelling that excludes, and once it
/// survives, nothing paints UNINTERESTING over the history.
#[test]
fn no_walk_survives_a_range_written_before_it() {
    let f = Fixture::new("nowalk");

    // Flag first: the range's excluded endpoint clears `no_walk`, so this walks.
    assert_eq!(
        f.subjects(&["rev-list", "--format=%s", "--no-walk", "main~2..main"]),
        ["m", "s1", "c1"]
    );
    // Flag last: `no_walk` stands, and only the positive endpoint is pended.
    assert_eq!(f.subjects(&["rev-list", "--format=%s", "main~2..main", "--no-walk"]), ["m"]);
    assert_eq!(f.subjects(&["show", "-s", "--format=%s", "main", "^main~2", "--no-walk"]), ["m"]);

    // `<a>...<b>` pends the merge bases *first*, so an endpoint that is itself a
    // merge base is already SEEN — and UNINTERESTING — by the time it is named
    // again. `side` is the merge base of `main...side`, so only `main` survives.
    assert_eq!(
        f.subjects(&["rev-list", "--format=%s", "main...side", "--no-walk=unsorted"]),
        ["m"]
    );
    assert_eq!(f.subjects(&["rev-list", "--format=%s", "main...side", "--no-walk"]), ["m"]);

    // `unsorted` keeps the pending order; the default sorts by commit date, and
    // that sort is stable, so a tie would keep the pending order too.
    assert_eq!(
        f.subjects(&["rev-list", "--format=%s", "--no-walk=unsorted", "side", "main", "v1"]),
        ["s1", "m", "c1"]
    );
    assert_eq!(
        f.subjects(&["rev-list", "--format=%s", "--no-walk=sorted", "side", "main", "v1"]),
        ["m", "s1", "c1"]
    );
}

/// How far `mark_parents_uninteresting()` reaches under `--no-walk` depends on
/// which commits the command line already caused to be parsed.
///
/// `^main main~2` drops `main~2`: walking to it parsed the commit in between, so
/// the marking could recurse past `main`'s direct parents. `^main side` names the
/// same generation through a ref, parses nothing on the way — but here `side` *is*
/// a direct parent of `main`, so it is marked at the first level anyway. The pair
/// that separates the two rules is `main..side`, whose positive endpoint is an
/// ancestor two levels down.
#[test]
fn no_walk_marks_only_as_far_as_the_parsed_commits_reach() {
    let f = Fixture::new("mark");

    // Direct parent of the excluded commit: marked, whichever way it is named.
    assert!(f.subjects(&["log", "--format=%s", "^main", "side", "--no-walk=unsorted"]).is_empty());
    assert!(f.subjects(&["log", "--format=%s", "main..side", "--no-walk=unsorted"]).is_empty());
    // Two levels down, reached by navigation, so the marking followed it there.
    assert!(f.subjects(&["log", "--format=%s", "^main", "main~2", "--no-walk=unsorted"]).is_empty());
    // A full ancestor closure would also have dropped the merge itself here; it is
    // kept, because nothing marked it.
    assert_eq!(f.subjects(&["log", "--format=%s", "^main~2", "main", "--no-walk=unsorted"]), ["m"]);
}

/// `cmd_show` reuses one `rev_info` across its pending loop, so the first commit
/// it walks consumes `--reverse` (`revs->reverse = 0; revs->reverse_output_stage
/// = 1`, revision.c:4683) and every commit after that is popped straight off the
/// list — past the `commit_ignore` check that would have dropped it.
#[test]
fn show_reverse_leaks_the_excluded_pending_objects() {
    let f = Fixture::new("revstage");

    // `main~2` is `c0`, pended UNINTERESTING and normally invisible…
    assert_eq!(f.subjects(&["show", "-s", "--format=%s", "main", "^main~2", "--no-walk"]), ["m"]);
    // …but with `--reverse` the second pending entry escapes the check.
    assert_eq!(
        f.subjects(&["show", "-s", "--format=%s", "main", "^main~2", "--no-walk", "--reverse"]),
        ["m", "c0"]
    );
}

/// The ref-selecting pseudo-options, in `log`, `rev-list` and `show` alike.
///
/// The pattern is matched against the *whole* refname with `wildmatch(…, 0)`
/// (refs.c:475-490) — no `WM_PATHNAME`, so a `*` crosses `/` — after the
/// namespace prefix is prepended and an implicit `/*` appended to a pattern that
/// holds no `?`, `*` or `[`. The name each pending object carries is the trimmed
/// one for the namespace forms and the full refname for `--all`/`--glob`, which
/// is what `--source` prints.
#[test]
fn ref_selectors_name_their_refs_the_way_git_does() {
    let f = Fixture::new("sel");

    assert_eq!(f.subjects(&["show", "-s", "--format=%s", "--branches"]), ["m", "s1"]);
    assert_eq!(f.subjects(&["show", "-s", "--format=%s", "--glob=refs/tags/*"]), ["c1"]);
    // `--branches=ma*` trims to the short name; `--glob` keeps the full one.
    assert_eq!(
        f.lines(&["log", "--source", "--oneline", "--branches=ma*"])
            .iter()
            .map(|l| l.split('\t').nth(1).unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["main m", "main s1", "main c1", "main c0"]
    );
    assert_eq!(
        f.lines(&["log", "--source", "--oneline", "--glob=refs/*/main"])
            .iter()
            .map(|l| l.split('\t').nth(1).unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["refs/heads/main m", "refs/heads/main s1", "refs/heads/main c1", "refs/heads/main c0"]
    );
    // A `*` crosses `/`: `--remotes=origin` gains the implicit `/*` and reaches
    // `origin/main`, and the pending name keeps the trimmed `origin/main`.
    assert_eq!(
        f.lines(&["log", "--source", "--oneline", "--remotes=origin"])
            .first()
            .and_then(|l| l.split('\t').nth(1))
            .map(str::to_owned),
        Some("origin/main m".to_string())
    );
    // A ref selection is `rev_input_given`, so the implicit `HEAD` is not added
    // on top of what it selected — `--tags` shows the tag's commit alone.
    assert_eq!(f.subjects(&["show", "-s", "--format=%s", "--tags"]), ["c1"]);
}

/// `--exclude` accumulates until a ref-selecting option consumes *and clears* it,
/// and its pattern is matched against the same (trimmed or full) name
/// `handle_one_ref()` was handed — with no implicit `/*` of its own.
#[test]
fn exclude_applies_to_the_next_selection_only() {
    let f = Fixture::new("excl");

    // Short name against `--branches`, full refname against `--all`.
    assert_eq!(f.subjects(&["show", "-s", "--format=%s", "--exclude=side", "--branches"]), ["m"]);
    assert_eq!(
        f.lines(&["log", "--source", "--oneline", "--exclude=refs/heads/side", "--all"])
            .iter()
            .map(|l| l.split('\t').nth(1).unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["refs/heads/main m", "refs/heads/main s1", "refs/tags/v1 c1", "refs/heads/main c0"]
    );
    // The pattern has to match the whole name: `refs/heads/side` never matches the
    // trimmed `side` that `--branches` yields.
    assert_eq!(
        f.subjects(&["show", "-s", "--format=%s", "--exclude=refs/heads/side", "--branches"]),
        ["m", "s1"]
    );
    // Written *after* the selection it never applies to it.
    assert_eq!(
        f.subjects(&["show", "-s", "--format=%s", "--branches", "--exclude=side"]),
        ["m", "s1"]
    );
}

/// `add_parents_only()`: `<rev>^@` pends the parents alone, `<rev>^!` pends them
/// UNINTERESTING and then the commit itself.
#[test]
fn parent_shorthands_reach_log_and_rev_list() {
    let f = Fixture::new("parents");

    // `^!` is a one-commit walk however the walk is spelled.
    assert_eq!(f.subjects(&["rev-list", "--format=%s", "main^!"]), ["m"]);
    assert_eq!(f.subjects(&["rev-list", "--format=%s", "main^!", "--do-walk"]), ["m"]);
    assert_eq!(f.subjects(&["log", "--format=%s", "main^!", "--reverse"]), ["m"]);
    // `^@` leaves `no_walk` alone, so it walks from the parents by default…
    assert_eq!(f.subjects(&["log", "--format=%s", "main^@"]), ["s1", "c1", "c0"]);
    // …and lists just those parents once `--no-walk` is written after it.
    assert_eq!(f.subjects(&["rev-list", "--format=%s", "main^@", "--no-walk"]), ["s1", "c1"]);
}

/// `builtin/rev-list.c:277-282` prints the abbreviated id only when
/// `revs->abbrev_commit` *and* a non-zero `revs->abbrev` are both set.
#[test]
fn rev_list_abbrev_needs_both_switches() {
    let f = Fixture::new("abbrev");

    let full = f.lines(&["rev-list", "main"]);
    assert!(full.iter().all(|l| l.len() == 40), "unabbreviated ids: {full:?}");

    let eight = f.lines(&["rev-list", "--abbrev-commit", "--abbrev=8", "main"]);
    assert_eq!(eight.len(), full.len());
    assert!(eight.iter().all(|l| l.len() == 8), "eight-hex ids: {eight:?}");
    for (short, long) in eight.iter().zip(&full) {
        assert!(long.starts_with(short), "{short} must be a prefix of {long}");
    }

    // `--abbrev` alone never abbreviates rev-list's own id column.
    assert_eq!(f.lines(&["rev-list", "--abbrev=8", "main"]), full);
    // Nor does `--no-abbrev`, which is git's zero, whatever `--abbrev-commit` says.
    assert_eq!(f.lines(&["rev-list", "--abbrev-commit", "--no-abbrev", "main"]), full);
    // `--abbrev=<n>` below `MINIMUM_ABBREV` clamps up to four.
    let four = f.lines(&["rev-list", "--abbrev-commit", "--abbrev=1", "main"]);
    assert!(four.iter().all(|l| l.len() == 4), "clamped ids: {four:?}");
}

/// `create_bundle()` calls `setup_revisions()` (bundle.c:501), so the same
/// pseudo-options select what a bundle carries — and the prerequisite lines come
/// out in the order the commit-date-ordered walk first met each boundary parent.
#[test]
fn bundle_create_serves_the_pseudo_revisions() {
    let f = Fixture::new("bundle");

    let b1 = f.bundle_path("branches.bundle");
    f.run(&["bundle", "create", b1.to_str().unwrap(), "--branches"]);
    assert_eq!(
        f.bundle_heads(&b1).iter().map(|l| l.split(' ').nth(1).unwrap()).collect::<Vec<_>>(),
        ["refs/heads/main", "refs/heads/side"]
    );

    // `--exclude` filters the selection that follows it, `HEAD` included in the
    // `--all` list because `refs_head_ref` pends it under that literal name.
    let b2 = f.bundle_path("excluded.bundle");
    f.run(&["bundle", "create", b2.to_str().unwrap(), "--exclude=refs/heads/side", "--all"]);
    assert_eq!(
        f.bundle_heads(&b2).iter().map(|l| l.split(' ').nth(1).unwrap()).collect::<Vec<_>>(),
        ["refs/heads/main", "refs/remotes/origin/main", "refs/tags/v1", "HEAD"]
    );

    let b3 = f.bundle_path("glob.bundle");
    f.run(&["bundle", "create", b3.to_str().unwrap(), "--glob=refs/tags/*"]);
    assert_eq!(
        f.bundle_heads(&b3).iter().map(|l| l.split(' ').nth(1).unwrap()).collect::<Vec<_>>(),
        ["refs/tags/v1"]
    );

    // Prerequisites: `^main~2` hides `c0`, which every kept commit descends from.
    let b4 = f.bundle_path("prereq.bundle");
    f.run(&["bundle", "create", b4.to_str().unwrap(), "--all", "^main~2"]);
    let header = std::fs::read(&b4).unwrap();
    let prereqs: Vec<String> = String::from_utf8_lossy(&header)
        .lines()
        .take_while(|l| !l.is_empty())
        .filter(|l| l.starts_with('-'))
        .map(str::to_owned)
        .collect();
    assert_eq!(prereqs.len(), 1, "one boundary commit: {prereqs:?}");
    assert!(prereqs[0].ends_with(" c0"), "the boundary is c0: {prereqs:?}");
}

/// `revs->expand_tabs_in_log`: a tab in a commit message was written against the
/// message's own left edge, so reprinting it under a four-space indent would
/// shift every tab stop the author lined up against. git expands the tabs
/// instead — width 8 by default for the indented header formats — and
/// `--expand-tabs[=<n>]` / `--no-expand-tabs` change or disable that.
///
/// `git show` did not expand them at all, which was wrong at zero flags.
#[test]
fn expand_tabs_keeps_a_messages_columns_under_the_indent() {
    let f = Fixture::new("tabs");
    // A message whose body lines the columns up with tabs.
    std::fs::write(f.repo.join("t.txt"), "t\n").unwrap();
    f.git_at(&["add", "t.txt"], 1_000_000_500);
    f.git_at(&["commit", "-q", "-m", "tabby\n\nab\tcd\n\tlead"], 1_000_000_500);

    let body = |args: &[&str]| -> Vec<String> {
        f.lines(args).into_iter().skip_while(|l| !l.contains("tabby")).collect()
    };
    for cmd in [
        vec!["log", "-1", "--no-patch"],
        vec!["show", "-s"],
        // `-s` is `DIFF_FORMAT_NO_OUTPUT`, which satisfies `cmd_whatchanged`'s
        // `if (!rev.diffopt.output_format)` — so the raw listing is dropped and
        // the record is still shown, because its pair queue was not empty.
        vec!["whatchanged", "-1", "--no-patch", "--i-still-use-this"],
    ] {
        let mut with = cmd.clone();
        with.push("--expand-tabs=4");
        let mut without = cmd.clone();
        without.push("--no-expand-tabs");
        assert_eq!(
            body(&cmd),
            ["    tabby", "    ", "    ab      cd", "            lead"],
            "{cmd:?} expands to width 8 by default"
        );
        assert_eq!(
            body(&with),
            ["    tabby", "    ", "    ab  cd", "        lead"],
            "{with:?}"
        );
        assert_eq!(
            body(&without),
            ["    tabby", "    ", "    ab\tcd", "    \tlead"],
            "{without:?}"
        );
    }
    // `--expand-tabs=0` is `--no-expand-tabs`, and a bare `--expand-tabs` is 8.
    assert_eq!(body(&["log", "-1", "--no-patch", "--expand-tabs=0"]), body(&["log", "-1", "--no-patch", "--no-expand-tabs"]));
    assert_eq!(body(&["log", "-1", "--no-patch", "--expand-tabs"]), body(&["log", "-1", "--no-patch"]));

    // `OPT_INTEGER`'s failure here is a fatal, not a usage error.
    for bad in ["--expand-tabs=bogus", "--expand-tabs=-1"] {
        let out = f.run(&["log", bad]);
        assert_eq!(out.status.code(), Some(128), "`log {bad}`: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("not a non-negative integer"),
            "`log {bad}` stderr: {out:?}"
        );
    }
}

/// `--patch-with-stat` and `--patch-with-raw` are `OPT_BITOP`s that set their own
/// format bit *and* `DIFF_FORMAT_PATCH`, so they are exactly `-p --stat` and
/// `-p --raw`.
#[test]
fn patch_with_stat_is_patch_plus_stat() {
    let f = Fixture::new("pws");

    // `git show` has no `-n`, so each command gets its own way of naming one
    // commit — and it has to be a non-merge, since `HEAD` here is the merge and
    // git renders no diff for one by default.
    for cmd in [vec!["log", "-1", "main~1"], vec!["show", "main~1"]] {
        let with = |extra: &[&str]| -> Vec<String> {
            let mut a = cmd.clone();
            a.extend_from_slice(extra);
            f.lines(&a)
        };
        assert_eq!(with(&["--patch-with-stat"]), with(&["-p", "--stat"]), "{cmd:?} --patch-with-stat");
        assert_eq!(with(&["--patch-with-raw"]), with(&["-p", "--raw"]), "{cmd:?} --patch-with-raw");
        // It is a *set*, so it wins over an earlier `-s`, and a later `-s` wins
        // over it.
        assert_eq!(
            with(&["-s", "--patch-with-stat"]),
            with(&["--patch-with-stat"]),
            "{cmd:?} -s then --patch-with-stat"
        );
        assert_eq!(with(&["--patch-with-stat", "-s"]), with(&["-s"]), "{cmd:?} --patch-with-stat then -s");
        // And it really does render both parts.
        let both = with(&["--patch-with-stat"]);
        assert!(both.iter().any(|l| l.contains(" | ")), "a diffstat row: {both:?}");
        assert!(both.iter().any(|l| l.starts_with("diff --git ")), "a patch: {both:?}");
    }
}

/// `diff_opt_dirstat()` dies while the option list is being read, so it precedes
/// the synopsis `diff-tree` prints when no tree-ish was named.
#[test]
fn diff_tree_reports_a_bad_dirstat_value_before_its_usage() {
    let f = Fixture::new("dirstat");

    for arg in ["--dirstat=bogus", "--dirstat==", "-Xbogus", "--dirstat-by-file=bogus"] {
        let out = f.run(&["diff-tree", arg]);
        assert_eq!(out.status.code(), Some(128), "`diff-tree {arg}`: {out:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.starts_with("fatal: Failed to parse --dirstat/-X option parameter:\n"),
            "`diff-tree {arg}` stderr: {err:?}"
        );
        assert!(err.contains("Unknown dirstat parameter"), "`diff-tree {arg}` stderr: {err:?}");
    }
    // A value git accepts still reaches the synopsis, because nothing else was named.
    let out = f.run(&["diff-tree", "--dirstat=files,10"]);
    assert_eq!(out.status.code(), Some(129), "a valid value keeps the usage error: {out:?}");
}
