//! `git bundle create <file> <rev-args>`: the options `setup_revisions()` reads after
//! the `<file>` operand (builtin/bundle.c:104 stops option parsing there, so every
//! later word is a revision, a pathspec or a rev-list option).
//!
//! Three rules are pinned, each measured on git 2.56.0 first:
//!
//! 1. **A word nothing claims** is `error: unrecognized argument: <word>` followed by
//!    SIGABRT (bundle.c:513-516 `goto out` reaches `object_array_clear(&revs_copy)` on
//!    a `revs_copy` initialised only at :551). It is reported only after the whole
//!    command line was read, so a bad revision later on the line dies first with 128.
//! 2. **A limiting option** (`--max-count`, `--skip`, `--since`, `--no-merges`,
//!    `--first-parent`, a pathspec, ...) shrinks the set `get_revision()` shows. The
//!    commits it stops short of become the bundle's prerequisites, a tip that fell out
//!    of the walk is `warning: ref '<name>' is excluded by the rev-list options` and
//!    leaves the header, and no tip left is `Refusing to create empty bundle.`.
//! 3. **An output-only option** (`--oneline`, `--stat`, `--graph`, `-p`, ...) is
//!    accepted and changes nothing, because `bundle create` prints no log and runs no
//!    diff.
//!
//! The expectations that do not need a second git are asserted unconditionally; the
//! matrix against stock additionally compares exit status or signal, stderr and every
//! byte of the bundle written.
#![cfg(unix)]

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// 2020-01-01 00:00:00 UTC; commit `n` is dated `DAY0 + n * 86400`.
const DAY0: i64 = 1_577_836_800;

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// ```text
    /// * d      (main)        day 5
    /// *   m                  day 6, merge of c and s
    /// |\
    /// | * s    (side)        day 3
    /// * | c                  day 4
    /// * | b    (dev, tag v1) day 2, v1 tagged on day 10
    /// |/
    /// * a                    day 1
    /// ```
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-bundlerev-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.git(1, &["init", "-q", "-b", "main"]);
        f.commit("a", 1);
        f.commit("b", 2);
        f.git(2, &["branch", "dev"]);
        f.git(10, &["tag", "-a", "-m", "v1", "v1"]);
        f.git(3, &["checkout", "-q", "-b", "side", "HEAD~1"]);
        f.commit("s", 3);
        f.git(4, &["checkout", "-q", "main"]);
        f.commit("c", 4);
        f.git(6, &["merge", "-q", "--no-ff", "-m", "m", "side"]);
        f.commit("d", 5);
        f
    }

    fn commit(&self, name: &str, day: i64) {
        std::fs::write(self.root.join(name), format!("{name}\n")).unwrap();
        self.git(day, &["add", name]);
        self.git(day, &["commit", "-q", "-m", name]);
    }

    fn git(&self, day: i64, args: &[&str]) {
        let out = self.run(BIN, day, args);
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn run(&self, bin: &str, day: i64, args: &[&str]) -> Output {
        let date = format!("@{} +0000", DAY0 + (day - 1) * 86_400);
        Command::new(bin)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("run {bin} {args:?}: {e}"))
    }

    /// `git bundle create <out> <args...>`, returning the verdict and the bundle bytes.
    fn create(&self, bin: &str, out: &str, args: &[&str]) -> Verdict {
        let path = self.root.join(out);
        let _ = std::fs::remove_file(&path);
        let mut argv = vec!["bundle", "create", out];
        argv.extend_from_slice(args);
        let o = self.run(bin, 20, &argv);
        Verdict {
            code: o.status.code(),
            signal: o.status.signal(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            bundle: std::fs::read(&path).ok(),
        }
    }
}

#[derive(PartialEq, Eq, Debug)]
struct Verdict {
    code: Option<i32>,
    signal: Option<i32>,
    stderr: String,
    bundle: Option<Vec<u8>>,
}

impl Verdict {
    /// The header: everything up to the blank line that precedes the pack.
    fn header(&self) -> String {
        let bytes = self.bundle.as_deref().expect("a bundle was written");
        let end = bytes.windows(2).position(|w| w == b"\n\n").expect("header end") + 1;
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }
}

fn rev(root: &Path, name: &str) -> String {
    let o = Command::new(BIN)
        .args(["rev-parse", name])
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// Runs both gits on `args` and fails with the whole picture when they differ.
fn assert_same(f: &Fixture, stock: &str, args: &[&str]) {
    let want = f.create(stock, "stock.bundle", args);
    let got = f.create(BIN, "zvcs.bundle", args);
    assert_eq!(got, want, "git bundle create <file> {args:?}");
}

#[test]
fn a_limited_walk_turns_the_parents_it_stops_at_into_prerequisites() {
    let f = Fixture::new("limit");
    let v = f.create(BIN, "b.bundle", &["main", "--max-count=1"]);
    assert_eq!(v.code, Some(0));
    assert_eq!(v.stderr, "");
    let (m, d) = (rev(&f.root, "main~1"), rev(&f.root, "main"));
    assert_eq!(v.header(), format!("# v2 git bundle\n-{m} m\n{d} refs/heads/main\n"));
}

#[test]
fn a_tip_the_walk_never_shows_is_warned_about_and_left_out() {
    let f = Fixture::new("excluded");
    let v = f.create(BIN, "b.bundle", &["main", "dev", "--max-count=1"]);
    assert_eq!(v.code, Some(0));
    assert_eq!(v.stderr, "warning: ref 'dev' is excluded by the rev-list options\n");
    assert!(!v.header().contains("refs/heads/dev"));

    let v = f.create(BIN, "b.bundle", &["main", "--skip=1"]);
    assert_eq!(v.code, Some(128));
    assert_eq!(
        v.stderr,
        "warning: ref 'main' is excluded by the rev-list options\nfatal: Refusing to create empty bundle.\n"
    );
    assert!(v.bundle.is_none());
}

#[test]
fn an_unclaimed_word_is_reported_after_the_whole_line_and_aborts() {
    let f = Fixture::new("unknown");
    let v = f.create(BIN, "b.bundle", &["main", "--foo"]);
    assert_eq!(v.stderr, "error: unrecognized argument: --foo\n");
    assert_eq!(v.signal, Some(6), "SIGABRT, as stock");
    assert!(v.bundle.is_none());

    // The first unclaimed word is the one named, whatever follows it.
    let v = f.create(BIN, "b.bundle", &["main", "--foo", "--max-count=1", "--bar"]);
    assert_eq!(v.stderr, "error: unrecognized argument: --foo\n");

    // A bad revision later on the line is fatal while `setup_revisions()` is still
    // reading, which is before the leftover is looked at.
    let v = f.create(BIN, "b.bundle", &["main", "--foo", "nonexistent"]);
    assert_eq!(v.code, Some(128));
    assert!(v.stderr.starts_with("fatal: ambiguous argument 'nonexistent'"), "{}", v.stderr);
}

#[test]
fn output_only_options_change_nothing() {
    let f = Fixture::new("inert");
    let plain = f.create(BIN, "b.bundle", &["main"]);
    for opt in ["--oneline", "--stat", "-p", "--graph", "--parents", "--quiet", "--abbrev=3", "--date=short"] {
        let v = f.create(BIN, "b.bundle", &["main", opt]);
        assert_eq!(v, plain, "{opt}");
    }
}

#[test]
fn bundle_options_after_the_file_are_not_bundle_options() {
    let f = Fixture::new("late");
    for word in ["-q", "--progress", "--all-progress", "--version=3", "-h", "--help-all"] {
        let v = f.create(BIN, "b.bundle", &["main", word]);
        assert_eq!(v.stderr, format!("error: unrecognized argument: {word}\n"), "{word}");
        assert_eq!(v.signal, Some(6), "{word}");
    }
}

/// Every case here was run against stock git 2.56.0 and agreed byte for byte.
#[test]
fn limiting_options_agree_with_stock() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new("matrix");
    let t = |day: i64| format!("@{}", DAY0 + (day - 1) * 86_400);
    let (d4, d3, d5) = (t(4), t(3), t(5));
    let cases: Vec<Vec<String>> = [
        vec!["main", "--max-count=1"],
        vec!["main", "--max-count=2"],
        vec!["main", "--max-count=2", "dev"],
        vec!["main", "dev", "--max-count=1"],
        vec!["--max-count=2", "main"],
        vec!["--all", "--max-count=1"],
        vec!["--all", "--max-count=2"],
        vec!["--all", "--skip=1"],
        vec!["--branches", "--max-count=1"],
        vec!["--tags", "--max-count=1"],
        vec!["main", "-2"],
        vec!["main", "-n2"],
        vec!["main", "-n", "2"],
        vec!["main", "--max-count", "2"],
        vec!["main", "--max-count=0"],
        vec!["main", "--max-count=-1"],
        vec!["main", "--skip=1"],
        vec!["main", "--skip", "1"],
        vec!["main", "--first-parent"],
        vec!["main", "--first-parent", "--max-count=2"],
        vec!["main", "--no-merges"],
        vec!["main", "--merges"],
        vec!["main", "--min-parents=2"],
        vec!["main", "--max-parents=1"],
        vec!["main", "--reverse"],
        vec!["main", "--reverse", "--max-count=2"],
        vec!["main", "--reverse", "--skip=1"],
        vec!["main", "--no-walk"],
        vec!["main", "dev", "--no-walk"],
        vec!["main", "--topo-order", "--max-count=3"],
        vec!["main", "--date-order", "--max-count=3"],
        vec!["main", "--author-date-order", "--max-count=3"],
        vec!["main", "--author=nobody"],
        vec!["main", "--committer=Mitter"],
        vec!["main", "--grep=c"],
        vec!["main", "--grep=C", "-i"],
        vec!["main", "--grep=c|d", "-E"],
        vec!["main", "--grep=c", "--invert-grep", "--max-count=1"],
        vec!["main", "--simplify-by-decoration", "--max-count=2"],
        vec!["main", "--", "a"],
        vec!["main", "--max-count=1", "--", "a"],
        vec!["main", "--full-history", "--", "c"],
        vec!["main", "--follow", "--", "a"],
        vec!["main", "a"],
        vec!["main", "a", "b"],
        vec!["main", "a", "nonexistent"],
        vec!["main", "a", "--max-count=1"],
        vec!["a"],
        vec!["main", "dev..main", "--max-count=1"],
        vec!["dev..main", "--max-count=1"],
        vec!["main...side", "--cherry-pick"],
        vec!["main...side", "--cherry"],
        vec!["main...side", "--left-only"],
        vec!["main...side", "--right-only"],
        vec!["side...main", "--left-only"],
        vec!["main", "--left-only", "--max-count=1"],
        vec!["main", "--cherry"],
        vec!["main", "^dev", "--max-count=1"],
        vec!["main", "--not", "dev", "--max-count=1"],
        vec!["main", "--all", "--not", "--max-count=1"],
        vec!["v1", "--max-count=1"],
        vec!["v1", "--skip=1"],
        vec!["v1", "--merges"],
        vec!["v1", "main", "--max-count=1"],
        vec!["main", "--default", "dev"],
        vec!["--default", "dev"],
    ]
    .into_iter()
    .map(|c| c.into_iter().map(String::from).collect())
    .chain([
        vec!["main".to_string(), format!("--since={d4}")],
        vec!["main".to_string(), "--since".to_string(), d4.clone()],
        vec!["main".to_string(), format!("--after={d4}")],
        vec!["main".to_string(), format!("--until={d3}")],
        vec!["main".to_string(), format!("--before={d3}")],
        vec!["main".to_string(), format!("--since={d4}"), format!("--until={d5}")],
        vec!["main".to_string(), format!("--since={d4}"), "--all".to_string()],
        vec!["main".to_string(), format!("--max-age={}", DAY0 + 3 * 86_400)],
        vec!["main".to_string(), format!("--min-age={}", DAY0 + 3 * 86_400)],
        // A tag is judged by its own date (day 10), not by its commit's (day 2).
        vec!["v1".to_string(), format!("--since={d4}")],
        vec!["v1".to_string(), format!("--since={}", t(11))],
        vec!["v1".to_string(), format!("--until={}", t(9))],
        vec!["v1".to_string(), "main".to_string(), format!("--since={}", t(11))],
    ])
    .collect();
    for case in &cases {
        let args: Vec<&str> = case.iter().map(String::as_str).collect();
        assert_same(&f, stock, &args);
    }
}

#[test]
fn unclaimed_words_and_option_errors_agree_with_stock() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new("errors");
    let cases: &[&[&str]] = &[
        &["--foo"],
        &["--foo", "--bar"],
        &["main", "--foo", "nonexistent"],
        &["main", "nonexistent", "--foo"],
        &["main", "--max-count=1", "--foo"],
        &["main", "--no-such"],
        &["main", "-"],
        &["main", "-x"],
        &["main", "-ixyz"],
        &["main", "--no-merges=1"],
        &["main", "--no-walk=bogus"],
        &["main", "--max-count=abc"],
        &["main", "--max-count=abc", "nonexistent"],
        &["main", "nonexistent", "--max-count=abc"],
        &["main", "--max-count"],
        &["main", "--skip=abc"],
        &["main", "-n"],
        &["main", "-nx"],
        &["main", "-n", "x"],
        &["main", "-1x"],
        &["main", "--max-count=1", "--max-count-oldest=1"],
        &["main", "--skip=1", "--max-count-oldest=1"],
        &["main", "--grep"],
        &["main", "--author"],
        &["main", "--glob"],
        &["main", "--exclude"],
        &["main", "--date"],
        &["main", "--default"],
        &["main", "--ancestry-path"],
        &["main", "--ancestry-path=zz"],
        &["main", "--unpacked=x"],
        &["main", "--merge"],
        &["main", "--follow"],
        &["main", "--diff-filter=Z"],
        &["main", "--diff-filter"],
        &["main", "-S"],
        &["main", "--anchored"],
        &["main", "--stat-width=x"],
        &["main", "--color=bogus"],
        &["main", "--end-of-options"],
        &["main", "--end-of-options", "--max-count=1"],
        &["--end-of-options", "main", "--max-count=1"],
        &["a", "--max-count=1"],
        &["main", "a", "--foo"],
        &["main", "^a"],
    ];
    for case in cases {
        assert_same(&f, stock, case);
    }
}
