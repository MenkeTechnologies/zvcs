//! `gc.logExpiry`, and the two `gc.repackFilter*` keys `gc` forwards to its
//! `repack`.
//!
//! # `gc.logExpiry`
//!
//! Two separate behaviours, both asserted here:
//!
//! * **Validation.** `gc_config()` reads it through `repo_config_get_expiry()`
//!   (`config.c:2468-2481`), whose test is not "does this parse" but "does this
//!   resolve to a moment strictly in the past", with the literal `now` let
//!   through as a special case. Anything `approxidate()` cannot read resolves to
//!   *now* and is therefore refused — so `bogus`, an empty value, `false` and
//!   `all` are all rejected while `never` and `1.day.ago` are accepted.
//! * **Effect.** `report_last_gc_error()` (`builtin/gc.c:791-831`) reads
//!   `$GIT_DIR/gc.log` and, when it is non-empty and its mtime has not aged past
//!   the expiry, prints it and abandons the run. That whole path is gated on
//!   `opts.detach > 0` (`:962`), which under `--auto` comes from `gc.autoDetach`.
//!
//! # `gc.repackFilter` / `gc.repackFilterTo`
//!
//! These are forwarded verbatim as `--filter=` / `--filter-to=` to the `repack`
//! child (`builtin/gc.c:653-656`), and what is ported is the pair of refusals
//! that child raises — the second pack a valid filter would produce is not
//! written here, which the `gc` module docs state. So the tests cover the
//! refusals and the fact that a valid spec does not become one.
//!
//! Every literal below was captured from git 2.55.0 (`/opt/homebrew/bin/git`);
//! the commands are named in each test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run binary")
}

fn ok(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(cwd, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-gclog-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    ok(&repo, &home, &["config", "user.email", "alice@example.com"]);
    ok(&repo, &home, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    ok(&repo, &home, &["add", "f"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

/// A repository holding two packs, so `gc.autoPackLimit=1` makes `gc --auto`
/// decide there is work to do and the `--auto` branch is actually entered.
fn two_pack_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (repo, home) = fixture(tag);
    ok(&repo, &home, &["repack", "-a", "-d", "-q"]);
    std::fs::write(repo.join("g"), "second\n").unwrap();
    ok(&repo, &home, &["add", "g"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c1"]);
    ok(&repo, &home, &["repack", "-q"]);
    let packs = repo
        .join(".git/objects/pack")
        .read_dir()
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".idx"))
        .count();
    assert!(packs >= 2, "fixture must hold at least two packs, has {packs}");
    (repo, home)
}

/// Write a `gc.log` the way a failed detached `gc` would have left one.
fn write_gc_log(repo: &Path, body: &str) {
    std::fs::write(repo.join(".git/gc.log"), body).unwrap();
}

const AUTO_PACK: &[&str] = &["-c", "gc.autoPackLimit=1"];

#[test]
fn log_expiry_rejects_values_that_do_not_resolve_to_the_past() {
    // git 2.55.0:
    //     $ git -c gc.logExpiry=bogus gc
    //     error: Invalid gc.logexpiry: 'bogus'
    //     fatal: unable to parse 'gc.logexpiry' from command-line config
    //     (exit 128)
    // `false` and `all` are in the same set even though `parse_expiry_date` would
    // happily read both: `repo_config_get_expiry` uses `approxidate`, and
    // `approxidate` answers *now* for anything it cannot read.
    let (repo, home) = fixture("reject");
    for value in ["bogus", "", "false", "all", "tomorrow"] {
        let out = run(&repo, &home, &["-c", &format!("gc.logExpiry={value}"), "gc", "-q"]);
        assert_eq!(
            stderr(&out),
            format!(
                "error: Invalid gc.logexpiry: '{value}'\n\
                 fatal: unable to parse 'gc.logexpiry' from command-line config\n"
            ),
            "gc.logExpiry={value:?} must be refused"
        );
        assert_eq!(code(&out), 128, "gc.logExpiry={value:?} exits 128");
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn log_expiry_accepts_now_never_and_any_past_moment() {
    // The other half: `now` is the documented special case, `never` and the dated
    // forms resolve into the past. All four leave the `gc` alone.
    let (repo, home) = fixture("accept");
    for value in ["now", "never", "1.day.ago", "2 weeks ago"] {
        let out = run(&repo, &home, &["-c", &format!("gc.logExpiry={value}"), "gc", "-q"]);
        assert!(
            out.status.success(),
            "gc.logExpiry={value:?} must be accepted, got: {}",
            stderr(&out)
        );
        assert!(
            !stderr(&out).contains("gc.logexpiry"),
            "gc.logExpiry={value:?} must not be diagnosed"
        );
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn log_expiry_from_a_file_names_the_file_and_h_still_wins() {
    // `git_die_config()`'s clause depends on where the value came from, and for a
    // file it names the line as well as the path — which
    // `crate::config::walk_config` supplies by re-parsing the file, since
    // gitoxide's config metadata carries the path but not the line.
    let (repo, home) = fixture("file");
    ok(&repo, &home, &["config", "gc.logExpiry", "bogus"]);
    let line = std::fs::read_to_string(repo.join(".git/config"))
        .unwrap()
        .lines()
        .position(|l| l.trim().to_ascii_lowercase().starts_with("logexpiry"))
        .expect("the value git config wrote is there")
        + 1;
    let out = run(&repo, &home, &["gc", "-q"]);
    assert_eq!(
        stderr(&out),
        format!(
            "error: Invalid gc.logexpiry: 'bogus'\n\
             fatal: bad config variable 'gc.logexpiry' in file '.git/config' at line {line}\n"
        )
    );
    assert_eq!(code(&out), 128);

    // `gc_config()` runs after `show_usage_with_options_if_asked`, so `-h` is
    // reached first and prints the usage block.
    let help = run(&repo, &home, &["gc", "-h"]);
    assert_eq!(code(&help), 129);
    assert!(help.stdout.starts_with(b"usage: git gc"));
    assert!(stderr(&help).is_empty());

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn max_cruft_size_is_diagnosed_before_log_expiry() {
    // `gc_config()` reads `gc.maxCruftSize` before `gc.logexpiry`, so when both
    // are unreadable the cruft size is the one named — in either `-c` order,
    // since the read is not driven by the command line. Verified against git
    // 2.55.0.
    let (repo, home) = fixture("pairorder");
    for order in [
        ["gc.logExpiry=bogus", "gc.maxCruftSize=bogus"],
        ["gc.maxCruftSize=bogus", "gc.logExpiry=bogus"],
    ] {
        let out = run(&repo, &home, &["-c", order[0], "-c", order[1], "gc", "-q"]);
        assert_eq!(
            stderr(&out),
            "fatal: bad numeric config value 'bogus' for 'gc.maxcruftsize': invalid unit\n",
            "gc.maxCruftSize reports first for {order:?}"
        );
        assert_eq!(code(&out), 128);
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The first line of `report_last_gc_error()`'s warning.
const LAST_GC_WARNING: &str =
    "warning: The last gc run reported the following. Please correct the root cause";

#[test]
fn a_recent_gc_log_stops_an_auto_gc_and_is_reported() {
    // git 2.55.0, in a two-pack repository with a fresh `.git/gc.log`:
    //     $ git -c gc.autoPackLimit=1 gc --auto
    //     Auto packing the repository in background for optimum performance.
    //     See "git help gc" for manual housekeeping.
    //     warning: The last gc run reported the following. Please correct the root cause
    //     and remove .git/gc.log
    //     Automatic cleanup will not be performed until the file is removed.
    //
    //     boom: previous gc failed
    //     (exit 0)
    // The two `Auto packing` lines belong to a detached run this port does not
    // perform, so what is asserted is the warning, the exit code, and — the point
    // of the whole path — that the `gc` did not run.
    let (repo, home) = two_pack_fixture("recent-log");
    write_gc_log(&repo, "boom: previous gc failed\n");
    let packs_before = pack_count(&repo);

    let mut args = AUTO_PACK.to_vec();
    args.extend_from_slice(&["gc", "--auto", "-q"]);
    let out = run(&repo, &home, &args);

    assert_eq!(code(&out), 0, "a reported failure is not an error exit");
    let text = stderr(&out);
    assert!(text.contains(LAST_GC_WARNING), "the previous failure must be reported: {text}");
    assert!(
        text.contains("and remove ") && text.contains("gc.log"),
        "the warning names the file to remove: {text}"
    );
    assert!(
        text.contains("boom: previous gc failed"),
        "the log's own contents are echoed: {text}"
    );
    assert_eq!(
        pack_count(&repo),
        packs_before,
        "the gc must be abandoned, leaving the packs alone"
    );
    assert!(repo.join(".git/gc.log").exists(), "the log stays until the user removes it");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn log_expiry_decides_whether_the_log_is_old_enough_to_ignore() {
    // The key's whole job: `if (st.st_mtime < gc_log_expire_time) goto done;`.
    // A `gc.log` written a moment ago is newer than `now`, so `gc.logExpiry=now`
    // skips the report and the gc runs; `2.days.ago` does not, so it still
    // reports. Both verified against git 2.55.0 on this fixture.
    let (repo, home) = two_pack_fixture("expiry-now");
    write_gc_log(&repo, "boom\n");
    let mut args = AUTO_PACK.to_vec();
    args.extend_from_slice(&["-c", "gc.logExpiry=now", "gc", "--auto", "-q"]);
    let skipped = run(&repo, &home, &args);
    assert!(skipped.status.success(), "{}", stderr(&skipped));
    assert!(
        !stderr(&skipped).contains(LAST_GC_WARNING),
        "gc.logExpiry=now ages the log out immediately: {}",
        stderr(&skipped)
    );
    assert_eq!(pack_count(&repo), 1, "the gc ran, so the packs were rewritten into one");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());

    let (repo, home) = two_pack_fixture("expiry-days");
    write_gc_log(&repo, "boom\n");
    let mut args = AUTO_PACK.to_vec();
    args.extend_from_slice(&["-c", "gc.logExpiry=2.days.ago", "gc", "--auto", "-q"]);
    let reported = run(&repo, &home, &args);
    assert!(reported.status.success());
    assert!(
        stderr(&reported).contains(LAST_GC_WARNING),
        "a log younger than the expiry still stops the run: {}",
        stderr(&reported)
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn the_gc_log_is_only_read_on_the_detaching_path() {
    // `if (opts.detach > 0)` is what gates the whole report, and `opts.detach`
    // starts at -1: only `--detach`, or `--auto` with `gc.autoDetach` (default
    // true), makes it positive. Each of these was checked against git 2.55.0 on
    // the same fixture.
    for (tag, extra, expect_warning) in [
        ("detach-auto", vec!["gc", "--auto", "-q"], true),
        ("detach-explicit", vec!["gc", "--detach", "-q"], true),
        ("detach-auto-no", vec!["gc", "--auto", "--no-detach", "-q"], false),
        ("detach-plain", vec!["gc", "-q"], false),
        ("detach-no", vec!["gc", "--no-detach", "-q"], false),
    ] {
        let (repo, home) = two_pack_fixture(tag);
        write_gc_log(&repo, "boom\n");
        let mut args = AUTO_PACK.to_vec();
        args.extend_from_slice(&extra);
        let out = run(&repo, &home, &args);
        assert!(out.status.success(), "{extra:?}: {}", stderr(&out));
        assert_eq!(
            stderr(&out).contains(LAST_GC_WARNING),
            expect_warning,
            "{extra:?} must {}report the previous failure; got: {}",
            if expect_warning { "" } else { "not " },
            stderr(&out)
        );
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    // `gc.autoDetach=false` takes `--auto` off the detaching path too.
    let (repo, home) = two_pack_fixture("detach-config-off");
    write_gc_log(&repo, "boom\n");
    let mut args = AUTO_PACK.to_vec();
    args.extend_from_slice(&["-c", "gc.autoDetach=false", "gc", "--auto", "-q"]);
    let out = run(&repo, &home, &args);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains(LAST_GC_WARNING),
        "gc.autoDetach=false leaves opts.detach at 0: {}",
        stderr(&out)
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn an_empty_gc_log_is_not_a_previous_failure() {
    // `else if (len > 0)`: a zero-length log is skipped and the gc proceeds.
    let (repo, home) = two_pack_fixture("empty-log");
    write_gc_log(&repo, "");
    let mut args = AUTO_PACK.to_vec();
    args.extend_from_slice(&["gc", "--auto", "-q"]);
    let out = run(&repo, &home, &args);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains(LAST_GC_WARNING),
        "an empty gc.log reports nothing: {}",
        stderr(&out)
    );
    assert_eq!(pack_count(&repo), 1, "and the gc still ran");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// How many pack indexes the repository holds — the cheapest signal for "did the
/// repack actually happen".
fn pack_count(repo: &Path) -> usize {
    repo.join(".git/objects/pack")
        .read_dir()
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".idx"))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn repack_filter_config_reaches_the_repack_childs_refusals() {
    // git 2.55.0:
    //     $ git -c gc.repackFilter=bogusfilter gc
    //     fatal: invalid filter-spec 'bogusfilter'
    //     fatal: failed to run repack
    //     (exit 128)
    //     $ git -c gc.repackFilterTo=/tmp/zz gc
    //     fatal: option '--filter-to' can only be used along with '--filter'
    //     fatal: failed to run repack
    //     (exit 128)
    let (repo, home) = fixture("filter-bad");

    let bad_spec = run(&repo, &home, &["-c", "gc.repackFilter=bogusfilter", "gc", "-q"]);
    assert_eq!(
        stderr(&bad_spec),
        "fatal: invalid filter-spec 'bogusfilter'\nfatal: failed to run repack\n"
    );
    assert_eq!(code(&bad_spec), 128);

    let depthless = run(&repo, &home, &["-c", "gc.repackFilter=tree:", "gc", "-q"]);
    assert_eq!(
        stderr(&depthless),
        "fatal: expected 'tree:<depth>'\nfatal: failed to run repack\n",
        "the child's own per-form diagnostics come through too"
    );
    assert_eq!(code(&depthless), 128);

    let orphan_to = run(&repo, &home, &["-c", "gc.repackFilterTo=out", "gc", "-q"]);
    assert_eq!(
        stderr(&orphan_to),
        "fatal: option '--filter-to' can only be used along with '--filter'\n\
         fatal: failed to run repack\n"
    );
    assert_eq!(code(&orphan_to), 128);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_valid_repack_filter_is_accepted_and_an_empty_one_is_not_a_filter_at_all() {
    // A spec the child would accept must not be turned into a refusal, and
    // `--filter-to` alongside it is then legal. An *empty* value is skipped
    // entirely (`if (cfg->repack_filter && *cfg->repack_filter)`), so an empty
    // `gc.repackFilter` next to a set `gc.repackFilterTo` still trips the pairing
    // check — which is the guard against treating "set" as "non-empty".
    let (repo, home) = fixture("filter-good");

    for spec in ["blob:none", "blob:limit=1k", "tree:0", "object:type=blob", "combine:blob:none+tree:0"] {
        let out = run(&repo, &home, &["-c", &format!("gc.repackFilter={spec}"), "gc", "-q"]);
        assert!(out.status.success(), "gc.repackFilter={spec} must be accepted: {}", stderr(&out));
        assert!(
            !stderr(&out).contains("failed to run repack"),
            "gc.repackFilter={spec} must not be refused"
        );
    }

    let with_to = run(
        &repo,
        &home,
        &["-c", "gc.repackFilter=blob:none", "-c", "gc.repackFilterTo=out", "gc", "-q"],
    );
    assert!(with_to.status.success(), "{}", stderr(&with_to));

    let empty_filter = run(
        &repo,
        &home,
        &["-c", "gc.repackFilter=", "-c", "gc.repackFilterTo=out", "gc", "-q"],
    );
    assert_eq!(
        stderr(&empty_filter),
        "fatal: option '--filter-to' can only be used along with '--filter'\n\
         fatal: failed to run repack\n",
        "an empty filter is not forwarded, so --filter-to has nothing to pair with"
    );
    assert_eq!(code(&empty_filter), 128);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_below_threshold_auto_gc_never_reaches_the_repack_filter_check() {
    // `gc --auto` returns from `need_to_gc()` before it builds the repack, so a
    // configuration that would be fatal for a real `gc` is silent there. Verified
    // against git 2.55.0, which also exits 0.
    let (repo, home) = fixture("filter-auto");
    let out = run(&repo, &home, &["-c", "gc.repackFilter=bogusfilter", "gc", "--auto", "-q"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).is_empty(), "nothing is reported: {}", stderr(&out));
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
