//! `git show-branch --reflog` walks the log once per row through `read_ref_at()`,
//! and reads `showbranch.default` through the config callback.
//!
//! **One `read_ref_at()` per row.** The loop (builtin/show-branch.c:798-826)
//! hands every call the same `oid`, which `read_ref_at_ent()` (refs.c:1115-1155)
//! reads as well as writes:
//!
//! ```c
//! if (!is_null_oid(&cb->ooid)) {
//!         oidcpy(cb->oid, noid);
//!         …
//! }
//! else if (cb->date == cb->at_time)
//!         oidcpy(cb->oid, noid);
//! else if (!oideq(noid, cb->oid))
//!         warning(_("log for ref %s unexpectedly ended on %s"), …);
//! ```
//!
//! An entry whose newer neighbour is a creation (an orphan's initial commit in
//! `HEAD`'s log) therefore warns there, and the name `HEAD@{n}` warns once more
//! when `repo_get_oid()` resolves it for the row.
//!
//! **A date base.** `--reflog=<n>,<date>` runs `read_ref_at(at, -1, …, &base)`
//! first (builtin/show-branch.c:784-796). `base` is `cutoff_cnt`, the matched
//! entry's position — or, when no entry is that old, the length of the log
//! (`read_ref_at_ent_oldest()` sets it from `cb->reccnt`), which leaves no rows
//! and prints `No revs to be shown.`. An empty log is `die("log for %s is
//! empty")` because the count passed is -1, not 0.
//!
//! **`showbranch.default`.** `git_show_branch_config()` pushes every value
//! once; a `-c` value seen twice turns a glob's second copy into nothing but
//! duplicates and so into `error: no matching refs with <glob>`. A valueless key
//! is `config_error_nonbool()`, and the walk dies at the first bad value of
//! either key it owns, in configuration order.
//!
//! **`--end-of-options`** ends the options like `--` (parse-options.c:1119-1124).
//!
//! Every comparison runs stock git and zvcs on twin fixtures built by stock git
//! with pinned dates.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, dir: &Path, date: &str, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("TERM", "dumb")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// `one`, `two` on `main`; an orphan `root2`; back to `main`; then HEAD
/// detached at `main~1`. Each step a minute after the last, so the log has
/// distinct dates to cut at. Branches `b1`, `b10`, `b2` exist for the glob.
fn prepare(stock: &str, r: &Path) {
    let mut minute = 0;
    let mut ok = |args: &[&str]| {
        minute += 1;
        let date = format!("{} +0000", 1_700_000_000 + 60 * minute);
        let (_, err, code) = run(stock, r, &date, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    ok(&["init", "-q", "-b", "main", "."]);
    for (file, msg) in [("a", "one"), ("b", "two")] {
        std::fs::write(r.join(file), msg).unwrap();
        ok(&["add", file]);
        ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", msg]);
    }
    ok(&["checkout", "-q", "--orphan", "root2"]);
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "root"]);
    ok(&["checkout", "-q", "main"]);
    for b in ["b1", "b10", "b2"] {
        ok(&["branch", b, "main~1"]);
    }
    ok(&["update-ref", "refs/heads/nolog", "main"]);
    std::fs::remove_file(r.join(".git/logs/refs/heads/nolog")).unwrap();
    ok(&["checkout", "-q", "--detach", "main~1"]);
}

fn compare(tag: &str, args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-sb-rra-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let r = base.join(side);
        std::fs::create_dir_all(&r).unwrap();
        prepare(stock, &r);
        outputs.push(run(bin, &r, "1700099999 +0000", args));
    }
    assert_eq!(outputs[1], outputs[0], "{tag}: {args:?}");
}

#[test]
fn every_row_walks_the_log_and_warns_where_read_ref_at_does() {
    // HEAD@{3} sits under the orphan's creation entry: the walk warns, and so does
    // resolving the row's name.
    compare("detached-4", &["show-branch", "--reflog=4"]);
    compare("head-6", &["show-branch", "--reflog=6", "HEAD"]);
    compare("head-from-2", &["show-branch", "--reflog=3,2", "HEAD"]);
}

#[test]
fn a_date_base_is_the_matched_position_or_the_end_of_the_log() {
    // Between the second and third HEAD entries (minutes 9 and 10 from the end).
    compare("date-mid", &["show-branch", "--reflog=2,2023-11-14 22:20:30 +0000", "HEAD"]);
    // Older than every entry: base is the log's length, nothing is shown.
    compare("date-old", &["show-branch", "--reflog=2,2000-01-01", "main"]);
    // A log with no entries is fatal for a date base, as for any count but 0.
    compare("date-nolog", &["show-branch", "--reflog=2,2000-01-01", "nolog"]);
    compare("count-nolog", &["show-branch", "--reflog=2", "nolog"]);
}

#[test]
fn showbranch_default_is_read_once_per_value() {
    compare("default-glob", &["-c", "showbranch.default=b1*", "show-branch"]);
    compare("default-two", &["-c", "showbranch.default=b2", "-c", "showbranch.default=b1?", "show-branch"]);
    compare("default-nonbool", &["-c", "showbranch.default", "show-branch"]);
    // The callback meets the values in order and dies at the first bad one, even
    // when a later value of the same key would read.
    compare("color-bad-first", &["-c", "color.showbranch=bogus", "-c", "color.showbranch=always", "show-branch", "b1"]);
    compare("nonbool-before-color", &["-c", "showbranch.default", "-c", "color.showbranch=bogus", "show-branch"]);
}

#[test]
fn end_of_options_ends_the_options() {
    compare("end-of-options", &["show-branch", "--end-of-options", "b1", "b2"]);
}
