//! `<ref>@{<date>}` for a date before the epoch, and the lookups `stash` repeats.
//!
//! `approxidate_careful()` returns `update_tm()`'s `time_t` cast to `timestamp_t`
//! (date.c:1410), which is `uintmax_t`. A date before 1970 — `100.years.ago`,
//! `100.years.from.now` (read as "100 years, then now"), a `mktime()` failure —
//! wraps to a value no reflog entry is newer than, so `read_ref_at_ent()`'s
//! `timestamp <= cb->at_time` (refs.c:1126) holds for the newest entry and
//! `stash@{100.years.ago}` is `stash@{0}` with no `only goes back to` warning.
//! `1000.years.ago` is different again: the year goes negative, which
//! `update_tm()` reads as unset and replaces with this year, so it is *now*.
//! The same unsigned comparisons decide `get_reflog_recno_by_time()`
//! (reflog-walk.c:110) for `log -g` and `count_reflog_ent()` (reflog.c:515) for
//! `reflog delete` and `stash drop`, and `rev-parse --since` prints the wrap.
//!
//! `get_stash_info()` resolves the revision once and then six more times through
//! `get_oidf()` (builtin/stash.c:217-224, 281), so every warning `get_oid_basic()`
//! raises for it is printed per lookup; and `repo_dwim_ref()` finding a branch
//! named `stash` beside `refs/stash` fails the lookup silently for `show`,
//! `apply` and `branch`, and as "not a stash reference" for `drop` and `pop`.
//!
//! Each case runs stock git and zvcs on identical fixtures, with the clock pinned
//! through `GIT_TEST_DATE_NOW`, and compares stdout, stderr, the exit status and
//! the stash reflog afterwards.

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
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Two identical repositories, `stock/` and `zvcs/`, each with one commit and
    /// three stash entries an hour apart in November 2023. `stash_branch` adds a
    /// branch named `stash`, which makes `stash` ambiguous. Stock git builds both.
    fn new(tag: &str, stash_branch: bool) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-approxidate-overflow-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        for side in ["stock", "zvcs"] {
            let work = f.root.join(side);
            std::fs::create_dir_all(&work).unwrap();
            f.git(&work, &["init", "-q", "-b", "main", "."], "1700000000 +0000");
            std::fs::write(work.join("f"), "a\n").unwrap();
            f.git(&work, &["add", "f"], "1700000000 +0000");
            f.git(&work, &["commit", "-q", "-m", "base"], "1700000000 +0000");
            for i in 1..=3 {
                std::fs::write(work.join("f"), format!("{i}\n")).unwrap();
                f.git(&work, &["stash", "-q"], &format!("{} +0000", 1_700_000_000 + i * 3600));
            }
            if stash_branch {
                f.git(&work, &["branch", "stash"], "1700000000 +0000");
            }
        }
        f
    }

    fn git(&self, dir: &Path, args: &[&str], date: &str) {
        let (_, err, code) = run(STOCK, dir, &self.root, args, date);
        assert_eq!(code, 0, "fixture step {args:?} failed: {err}");
    }

    /// The stash reflog and every ref, read by stock git.
    fn post_state(&self, side: &str) -> String {
        let dir = self.root.join(side);
        let (log, _, _) = run(STOCK, &dir, &self.root, &["reflog", "show", "--format=%H %gs", "refs/stash"], "1700000000 +0000");
        let (refs, _, _) = run(STOCK, &dir, &self.root, &["for-each-ref"], "1700000000 +0000");
        format!("{log}--\n{refs}")
    }
}

/// Run `args` through both binaries on a fresh fixture and compare everything.
fn check(tag: &str, stash_branch: bool, args: &[&str]) {
    let f = Fixture::new(tag, stash_branch);
    let date = "1700100000 +0000";
    let stock = run(STOCK, &f.root.join("stock"), &f.root, args, date);
    let zvcs = run(ZVCS, &f.root.join("zvcs"), &f.root, args, date);
    assert_eq!(zvcs, stock, "git {args:?}: (stdout, stderr, status) differ");
    assert_eq!(f.post_state("zvcs"), f.post_state("stock"), "git {args:?}: post-state differs");
}

const SPECS: &[&str] = &[
    "stash@{100.years.ago}",
    "stash@{1000.years.ago}",
    "stash@{10000.years.ago}",
    "stash@{100000.years.ago}",
    "stash@{1900-01-01}",
    "stash@{100.years.from.now}",
    "stash@{2099-01-01}",
    "stash@{3.years.ago}",
    "stash@{1700005000}",
    "stash@{1699999999}",
];

#[test]
fn rev_parse_resolves_wrapped_dates_to_the_newest_entry() {
    if !stock_available() {
        return;
    }
    for (i, spec) in SPECS.iter().enumerate() {
        check(&format!("rp{i}"), false, &["rev-parse", spec]);
        check(&format!("rpq{i}"), false, &["rev-parse", "--verify", "-q", spec]);
    }
    for spec in ["HEAD@{100.years.ago}", "@{100.years.ago}", "main@{100.years.ago}", "stash@{0}", "stash@{3}", "stash@{4}"] {
        check("rp-other", false, &["rev-parse", spec]);
    }
}

#[test]
fn rev_parse_since_prints_the_unsigned_wrap() {
    if !stock_available() {
        return;
    }
    for date in ["100.years.ago", "57.years.ago", "1000.years.ago", "1900-01-01", "3.years.ago"] {
        check("since", false, &["rev-parse", &format!("--since={date}")]);
        check("until", false, &["rev-parse", &format!("--until={date}"), "HEAD"]);
    }
}

#[test]
fn log_walk_and_show_start_at_the_newest_entry() {
    if !stock_available() {
        return;
    }
    for (i, spec) in SPECS.iter().enumerate() {
        check(&format!("lg{i}"), false, &["log", "-g", "--format=%H %gd", spec]);
        check(&format!("sh{i}"), false, &["show", "-s", "--format=%H", spec]);
    }
}

#[test]
fn stash_commands_take_the_entry_and_repeat_the_warning_per_lookup() {
    if !stock_available() {
        return;
    }
    for (i, spec) in SPECS.iter().enumerate() {
        for verb in [&["stash", "show"][..], &["stash", "list"], &["stash", "apply"], &["stash", "drop"], &["stash", "pop"], &["stash", "export", "--print"], &["stash", "branch", "nb"], &["reflog", "delete"]] {
            let mut args = verb.to_vec();
            args.push(spec);
            check(&format!("st{i}"), false, &args);
        }
    }
}

#[test]
fn stash_refuses_an_ambiguous_stash_name_as_get_stash_info_does() {
    if !stock_available() {
        return;
    }
    for args in [
        &["stash", "show", "stash@{1}"][..],
        &["stash", "show", "stash@{3.years.ago}"],
        &["stash", "show"],
        &["stash", "show", "1"],
        &["stash", "apply", "stash@{1}"],
        &["stash", "apply", "refs/stash@{1}"],
        &["stash", "branch", "nb", "stash@{1}"],
        &["stash", "drop", "stash@{1}"],
        &["stash", "drop", "refs/stash@{1}"],
        &["stash", "pop", "stash@{1}"],
        &["stash", "pop"],
    ] {
        check("amb", true, args);
    }
}
