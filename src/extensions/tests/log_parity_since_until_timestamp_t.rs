//! `--since`/`--until` and their spellings as `struct rev_info` holds them.
//!
//! `max_age`, `min_age` and `max_age_as_filter` are `timestamp_t` (revision.h:325-327),
//! which is `uintmax_t`, and so is the `approxidate()` that fills them. A date before
//! the epoch (`100.years.ago`) wraps to a bound above every commit date: `--since`
//! of it shows nothing and `--until` of it shows everything. `--max-age` and
//! `--min-age` take any `parse_age()` number, `-5` and `2^64-2` included.
//!
//! `--since` is a walk cut, not a filter. A limited walk (`limit_list()`,
//! revision.c:1474-1483) paints a commit below the bound UNINTERESTING together with
//! its whole ancestry; a streaming one (`get_revision_1()`, revision.c:4472-4476)
//! drops the commit before `process_parents()`, so its parents are reached only
//! through some other child. A reflog walk measures the entry's own date
//! (`comparison_date()`, revision.c:4211-4217). `blame` applies the test to the
//! working-tree commit too, whose date is the current time (blame.c:209, 2614-2616),
//! and `bundle create` keeps a tag only when its tagger date lies strictly inside the
//! window (bundle.c:303-331).
//!
//! Each case runs stock git and zvcs over the same fixture with the dates pinned and
//! compares stdout, stderr and the exit status.

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn stock_available() -> bool {
    Path::new(STOCK).exists()
}

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str], date: &str) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("GIT_TEST_DATE_NOW", "1790000000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-age-timestamp-t-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("repo");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."], 1_000_000_000);
        f
    }

    fn git(&self, args: &[&str], date: i64) {
        let (_, err, code) = run(STOCK, &self.work, &self.root, args, &format!("{date} +0000"));
        assert_eq!(code, 0, "fixture step {args:?} failed: {err}");
    }

    fn commit(&self, message: &str, date: i64) {
        std::fs::write(self.work.join("f"), format!("{message}\n")).unwrap();
        self.git(&["add", "f"], date);
        self.git(&["commit", "-q", "--allow-empty", "-m", message], date);
    }

    /// `c1`..`c3` a day apart in September 2001, a `v1` tag made a day after
    /// `c3`, and a `HEAD` reflog whose last two entries are dated 2020 by a
    /// `reset` away and back.
    fn linear(tag: &str) -> Self {
        let f = Fixture::new(&format!("linear-{tag}"));
        for i in 1..=3 {
            f.commit(&format!("c{i}"), 1_000_000_000 + i * 86_400);
        }
        f.git(&["tag", "-a", "-m", "t", "v1"], 1_000_345_600);
        f.git(&["reset", "-q", "--hard", "HEAD~1"], 1_600_000_000);
        f.git(&["reset", "-q", "--hard", "ORIG_HEAD"], 1_600_000_100);
        f
    }

    /// A first-parent chain whose second commit is dated before the first.
    fn skewed() -> Self {
        let f = Fixture::new("skewed");
        for (i, date) in [1_000_300_000, 1_000_100_000, 1_000_400_000, 1_000_500_000].iter().enumerate() {
            f.commit(&format!("s{i}"), *date);
        }
        f
    }

    /// `R` (+250) with children `A` (+50) on `main` and `B` (+200) on `side`,
    /// merged as `M` (+300): with `--since=+150` a streaming walk still reaches
    /// `R` through `B`, while a limited one paints it through `A`.
    fn merge() -> Self {
        let f = Fixture::new("merge");
        let empty = |message: &str, date: i64| f.git(&["commit", "-q", "--allow-empty", "-m", message], date);
        empty("R", 1_000_000_250);
        f.git(&["branch", "side"], 1_000_000_250);
        empty("A", 1_000_000_050);
        f.git(&["checkout", "-q", "side"], 1_000_000_250);
        empty("B", 1_000_000_200);
        f.git(&["checkout", "-q", "main"], 1_000_000_250);
        f.git(&["merge", "-q", "--no-ff", "-m", "M", "side"], 1_000_000_300);
        f
    }

    fn check(&self, args: &[&str]) {
        let date = "1000900000 +0000";
        let stock = run(STOCK, &self.work, &self.root, args, date);
        let zvcs = run(ZVCS, &self.work, &self.root, args, date);
        assert_eq!(zvcs, stock, "git {args:?}: (stdout, stderr, status) differ");
    }
}

/// Every spelling of the three bounds, at the dates the unsigned compare decides.
const BOUNDS: &[&str] = &[
    "--since=100.years.ago",
    "--after=100.years.ago",
    "--until=100.years.ago",
    "--before=100.years.ago",
    "--since-as-filter=100.years.ago",
    "--since=@0",
    "--until=@0",
    "--since=0",
    "--until=0",
    "--since=@4000000000",
    "--until=@4000000000",
    "--since=@1000172800",
    "--until=@1000172800",
    "--max-age=-5",
    "--min-age=-5",
    "--max-age=-1",
    "--min-age=-1",
    "--max-age=18446744073709551614",
    "--min-age=18446744073709551614",
    "--max-age=9223372036854775808",
    "--min-age=9223372036854775808",
];

#[test]
fn walks_compare_the_bounds_unsigned() {
    if !stock_available() {
        return;
    }
    let f = Fixture::linear("walks");
    for bound in BOUNDS {
        f.check(&["log", "--format=%s", bound]);
        f.check(&["log", "--format=%s", "--topo-order", bound]);
        f.check(&["log", "--format=%s", "--no-walk", "HEAD", "HEAD~1", bound]);
        f.check(&["log", "--format=%s", "HEAD~1..HEAD", bound]);
        f.check(&["rev-list", "HEAD", bound]);
        f.check(&["rev-list", "--count", "HEAD", bound]);
        f.check(&["rev-list", "--objects", "HEAD", bound]);
        f.check(&["shortlog", "-s", "HEAD", bound]);
        f.check(&["show", "-s", "--format=%s", "HEAD~1", "HEAD", bound]);
        f.check(&["show", "-s", "--format=%s", "HEAD~2..HEAD", bound]);
    }
}

#[test]
fn reflog_walks_measure_the_entry_date() {
    if !stock_available() {
        return;
    }
    let f = Fixture::linear("reflog");
    for bound in BOUNDS.iter().copied().chain([
        "--since=2019-01-01",
        "--until=2019-01-01",
        "--since-as-filter=2019-01-01",
        "--since=@1600000050",
        "--until=@1600000050",
    ]) {
        f.check(&["log", "-g", "--format=%gd %s", bound]);
        f.check(&["rev-list", "-g", "HEAD", bound]);
        f.check(&["show", "-g", "-s", "--format=%gd %s", bound]);
    }
}

#[test]
fn since_cuts_the_walk_rather_than_filtering_it() {
    if !stock_available() {
        return;
    }
    let skewed = Fixture::skewed();
    for bound in ["--since=@1000200000", "--max-age=1000200000", "--since-as-filter=@1000200000"] {
        skewed.check(&["log", "--format=%s", bound]);
        skewed.check(&["log", "--format=%s", "--reverse", bound]);
        skewed.check(&["log", "--format=%s", "--topo-order", bound]);
        skewed.check(&["log", "--graph", "--format=%s", bound]);
        skewed.check(&["rev-list", "HEAD", bound]);
        skewed.check(&["shortlog", "-s", "HEAD", bound]);
    }
    let merge = Fixture::merge();
    for bound in ["--since=@1000000150", "--max-age=1000000150"] {
        merge.check(&["log", "--format=%s", bound]);
        merge.check(&["log", "--format=%s", "--topo-order", bound]);
        merge.check(&["log", "--format=%s", "--date-order", bound]);
        merge.check(&["rev-list", "--format=%s", "HEAD", bound]);
        merge.check(&["rev-list", "--topo-order", "--format=%s", "HEAD", bound]);
        merge.check(&["shortlog", "-s", "HEAD", bound]);
        merge.check(&["shortlog", "-s", "--topo-order", "HEAD", bound]);
    }
}

#[test]
fn blame_stops_at_the_working_tree_commit_and_parses_max_age() {
    if !stock_available() {
        return;
    }
    let f = Fixture::linear("blame");
    // `-s` drops the author and the date, which for the working-tree commit is
    // the wall clock of each run.
    for bound in ["--since=100.years.ago", "--since=@4000000000", "--max-age=-5", "--max-age=18446744073709551614", "--since=@1000172800"] {
        f.check(&["blame", "-s", bound, "f"]);
        f.check(&["blame", "-s", bound, "HEAD", "--", "f"]);
        f.check(&["blame", "--porcelain", bound, "HEAD", "--", "f"]);
        f.check(&["blame", "-s", "--show-stats", bound, "f"]);
    }
    f.check(&["blame", "--max-age=abc", "f"]);
    f.check(&["blame", "--max-age=1x", "HEAD", "--", "f"]);
}

#[test]
fn bundle_keeps_a_tag_strictly_inside_the_window() {
    if !stock_available() {
        return;
    }
    let f = Fixture::linear("bundle");
    for (i, bound) in [
        "--since=@1000345600",
        "--until=@1000345600",
        "--since=@1000345599",
        "--until=@1000345601",
        "--until=100.years.ago",
        "--since=100.years.ago",
        "--min-age=-5",
    ]
    .iter()
    .enumerate()
    {
        let stock_out = f.root.join(format!("stock-{i}.bundle"));
        let zvcs_out = f.root.join(format!("zvcs-{i}.bundle"));
        let date = "1000900000 +0000";
        let stock = run(STOCK, &f.work, &f.root, &["bundle", "create", stock_out.to_str().unwrap(), "main", "v1", bound], date);
        let zvcs = run(ZVCS, &f.work, &f.root, &["bundle", "create", zvcs_out.to_str().unwrap(), "main", "v1", bound], date);
        assert_eq!(zvcs, stock, "bundle create {bound}: (stdout, stderr, status) differ");
        if stock.2 == 0 {
            let heads = |path: &Path| run(STOCK, &f.work, &f.root, &["bundle", "list-heads", path.to_str().unwrap()], date);
            assert_eq!(heads(&zvcs_out), heads(&stock_out), "bundle create {bound}: heads differ");
        }
    }
}
