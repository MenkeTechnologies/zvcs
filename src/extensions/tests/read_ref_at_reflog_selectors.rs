//! `read_ref_at()` (`refs.c:1173-1218`), the walk behind every `<ref>@{<n>}` and
//! `<ref>@{<date>}` operand.
//!
//! The rule that makes this worth its own suite is that `read_ref_at_ent()`
//! inspects `cb->ooid`/`cb->noid` *before* it stores the record it is looking at,
//! so those fields still hold the entry one **newer** than the one selected:
//!
//! ```c
//! if (timestamp <= cb->at_time || cb->cnt == 0) {
//!         set_read_ref_cutoffs(cb, timestamp, tz, message);
//!         if (!is_null_oid(&cb->ooid)) {
//!                 oidcpy(cb->oid, noid);
//!                 if (!oideq(&cb->ooid, noid))
//!                         warning(_("log for ref %s has gap after %s"), …);
//!         }
//!         else if (cb->date == cb->at_time)
//!                 oidcpy(cb->oid, noid);
//!         else if (!oideq(noid, cb->oid))
//!                 warning(_("log for ref %s unexpectedly ended on %s"), …);
//! ```
//!
//! `<ref>@{<n>}` is therefore *not* "entry `n`'s new id". It is entry `n`'s new id
//! only while the entry one newer has a non-null old id. When that entry is a
//! creation the answer is left at the value the caller pre-seeded — the ref's
//! current value — and git warns instead. A `git branch -m` round trip writes
//! exactly such a pair (a delete, `<id>` → null, and a create, null → `<id>`), so
//! `git rev-parse HEAD@{1}` after renaming a branch away and back answers the
//! current tip where a naive "entry `n`'s new id" answers the **null id** — an
//! object no repository has, which then broke `log`, `show`, `rev-list` and `diff`
//! outright rather than visibly.
//!
//! Neither warning has a `flags` gate: `refs.c:1135` and `refs.c:1141` call
//! `warning()` outright, so `--quiet` does not silence them, and they are printed
//! once per `get_oid_basic()` call — twice for `git diff <spec> <spec>`, twice for
//! `git checkout <spec>` (whose `setup_branch_path()` resolves a second time when
//! the operand is not itself a ref name, `builtin/checkout.c:804-806`).
//!
//! Every expectation below was measured against stock git 2.55.0
//! (`/opt/homebrew/bin/git`) on the same fixture before being written down.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const NULL_ID: &str = "0000000000000000000000000000000000000000";

/// The four commit timestamps the fixtures below use, one minute apart, written
/// out as the epoch seconds the hand-built reflogs carry. `+0200` is recorded in
/// the log line itself, and `show_date(…, DATE_MODE(RFC2822))` renders in *that*
/// zone, so the warning text is the same on any machine.
const T1: i64 = 1_112_904_673;
const T2: i64 = 1_112_904_733;
const T3: i64 = 1_112_904_793;
const T4: i64 = 1_112_904_853;

struct Fx {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run_in(repo: &Path, home: &Path, date: &str, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

impl Fx {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-readrefat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("work");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fx { root, repo };
        f.ok(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn run(&self, args: &[&str]) -> Output {
        run_in(&self.repo, &self.root, "1112904793 +0200", args)
    }

    fn ok(&self, args: &[&str]) {
        let out = self.run(args);
        assert!(out.status.success(), "git {args:?} failed: {}", err(&out));
    }

    /// One commit at `date`, so a fixture's entries carry distinct timestamps.
    fn commit(&self, body: &str, message: &str, date: &str) {
        std::fs::write(self.repo.join("f"), body).unwrap();
        let add = run_in(&self.repo, &self.root, date, &["add", "f"]);
        assert!(add.status.success(), "add failed: {}", err(&add));
        let commit = run_in(&self.repo, &self.root, date, &["commit", "-q", "-m", message]);
        assert!(commit.status.success(), "commit failed: {}", err(&commit));
    }

    fn rev(&self, spec: &str) -> String {
        let out = self.run(&["rev-parse", spec]);
        assert!(out.status.success(), "rev-parse {spec} failed: {}", err(&out));
        out_text(&out).trim().to_string()
    }

    fn write_head_log(&self, lines: &[(String, String, i64, &str)]) {
        let text: String = lines
            .iter()
            .map(|(old, new, at, message)| {
                format!("{old} {new} A <a@example.com> {at} +0200\t{message}\n")
            })
            .collect();
        std::fs::write(self.repo.join(".git/logs/HEAD"), text).unwrap();
    }
}

fn out_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn err(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// Four commits, one minute apart, on `main`.
fn four_commits(tag: &str) -> (Fx, String, String, String, String) {
    let f = Fx::new(tag);
    f.commit("1\n", "c1", "1112904673 +0200");
    f.commit("1\n2\n", "c2", "1112904733 +0200");
    f.commit("1\n2\n3\n", "c3", "1112904793 +0200");
    f.commit("1\n2\n3\n4\n", "c4", "1112904853 +0200");
    let (c1, c2, c3, c4) =
        (f.rev("HEAD~3"), f.rev("HEAD~2"), f.rev("HEAD~1"), f.rev("HEAD"));
    (f, c1, c2, c3, c4)
}

/// A `git branch -m` round trip: two commits, then `main` → `other` → `main`.
/// Both renames log a delete and a create into `logs/HEAD`, which is the shape
/// that leaves a creation record one *newer* than the entry `@{1}` selects.
fn renamed_round_trip(tag: &str) -> (Fx, String, String) {
    let f = Fx::new(tag);
    f.commit("hi\n", "one", "1112904793 +0200");
    f.commit("hi\ntwo\n", "two", "1112904793 +0200");
    let (first, tip) = (f.rev("HEAD~1"), f.rev("HEAD"));
    f.ok(&["branch", "-m", "main", "other"]);
    f.ok(&["branch", "-m", "other", "main"]);
    (f, first, tip)
}

const ENDED_AT_2213: &str =
    "warning: log for ref HEAD unexpectedly ended on Thu, 7 Apr 2005 22:13:13 +0200\n";
const GAP_AT_2211: &str =
    "warning: log for ref HEAD has gap after Thu, 7 Apr 2005 22:11:13 +0200\n";

/// The reported defect: `HEAD@{1}` and `HEAD@{3}` select the *delete* half of a
/// rename pair, whose new id is the null id. git never hands that back — the
/// record one newer is the matching create, so `cb->ooid` is null, nothing
/// overwrites the pre-seeded value, and the current tip is the answer.
#[test]
fn a_selector_landing_after_a_creation_answers_the_ref_and_warns() {
    let (f, first, tip) = renamed_round_trip("creation");

    for nth in ["HEAD@{1}", "HEAD@{3}"] {
        let out = f.run(&["rev-parse", nth]);
        assert_eq!(code(&out), 0, "{nth}: {}", err(&out));
        assert_eq!(out_text(&out), format!("{tip}\n"), "{nth} must not be the null id");
        assert_ne!(out_text(&out).trim(), NULL_ID);
        assert_eq!(err(&out), ENDED_AT_2213, "{nth}");
    }

    // The entries whose *newer* neighbour is an ordinary record answer from the
    // log and say nothing, so the warning is not a blanket one for this fixture.
    for nth in ["HEAD@{0}", "HEAD@{2}", "HEAD@{4}"] {
        let out = f.run(&["rev-parse", nth]);
        assert_eq!(out_text(&out), format!("{tip}\n"), "{nth}");
        assert_eq!(err(&out), "", "{nth} has an ordinary predecessor");
    }
    let out = f.run(&["rev-parse", "HEAD@{5}"]);
    assert_eq!(out_text(&out), format!("{first}\n"));
    assert_eq!(err(&out), "");
}

/// `!oideq(&cb->ooid, noid)`: the entry one newer starts where this one did not
/// end, so the log has a hole. The date in the message is the *stopped-on*
/// entry's, not the newer one's.
#[test]
fn a_hole_in_the_chain_is_reported_as_a_gap() {
    let (f, c1, c2, c3, c4) = four_commits("gap");
    // `c2`'s record removed, leaving `c3`'s old id pointing at a record that is
    // no longer there — what `git reflog delete` without `--rewrite` leaves.
    f.write_head_log(&[
        (NULL_ID.to_string(), c1.clone(), T1, "commit (initial): c1"),
        (c2, c3.clone(), T3, "commit: c3"),
        (c3.clone(), c4.clone(), T4, "commit: c4"),
    ]);

    let out = f.run(&["rev-parse", "HEAD@{2}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{c1}\n"));
    assert_eq!(err(&out), GAP_AT_2211);

    // The two entries before the hole are contiguous and silent.
    assert_eq!(err(&f.run(&["rev-parse", "HEAD@{0}"])), "");
    assert_eq!(out_text(&f.run(&["rev-parse", "HEAD@{0}"])), format!("{c4}\n"));
    assert_eq!(err(&f.run(&["rev-parse", "HEAD@{1}"])), "");
    assert_eq!(out_text(&f.run(&["rev-parse", "HEAD@{1}"])), format!("{c3}\n"));
}

/// The warning names `real_ref` — the ref `repo_dwim_log()` found, spelled in
/// full — and not the operand. A bare `@{<n>}` goes through `repo_dwim_ref("HEAD")`
/// instead, which reports HEAD's *target*, so it reads `main`'s log and names it.
#[test]
fn the_warning_names_the_full_ref_the_log_belongs_to() {
    let (f, c1, c2, c3, c4) = four_commits("names");
    let gapped = format!(
        "{NULL_ID} {c1} A <a@example.com> {T1} +0200\tcommit (initial): c1\n\
         {c2} {c3} A <a@example.com> {T3} +0200\tcommit: c3\n\
         {c3} {c4} A <a@example.com> {T4} +0200\tcommit: c4\n"
    );
    std::fs::write(f.repo.join(".git/logs/refs/heads/main"), &gapped).unwrap();
    // Leave `logs/HEAD` intact so the two operands cannot be reading the same file.

    for spec in ["main@{2}", "heads/main@{2}", "refs/heads/main@{2}", "@{2}"] {
        let out = f.run(&["rev-parse", spec]);
        assert_eq!(out_text(&out), format!("{c1}\n"), "{spec}");
        assert_eq!(
            err(&out),
            "warning: log for ref refs/heads/main has gap after Thu, 7 Apr 2005 22:11:13 +0200\n",
            "{spec}"
        );
    }
}

/// `@{0}` has no newer record at all, so `cb->ooid` is the zeroed one `memset()`
/// left and the answer is always the ref's own value — with
/// `!oideq(noid, cb->oid)` warning when the newest entry disagrees with it.
#[test]
fn selector_zero_answers_the_ref_and_reports_a_log_that_fell_behind() {
    let (f, c1, c2, c3, c4) = four_commits("desync");
    // The log stops at `c3` while `HEAD` is at `c4` — what a crashed writer, or a
    // ref updated with the log disabled, leaves behind.
    f.write_head_log(&[
        (c1, c2.clone(), T2, "commit: c2"),
        (c2.clone(), c3, T3, "commit: c3"),
    ]);

    let out = f.run(&["rev-parse", "HEAD@{0}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{c4}\n"), "@{{0}} is the ref, not the log");
    assert_eq!(err(&out), ENDED_AT_2213);

    // `@{1}` has `@{0}`'s non-null old id in front of it and takes the ordinary path.
    let out = f.run(&["rev-parse", "HEAD@{1}"]);
    assert_eq!(out_text(&out), format!("{c2}\n"));
    assert_eq!(err(&out), "");
}

/// `read_ref_at_ent_oldest()` and the `nth == co_cnt` arm above it: one past the
/// end is still answerable from the oldest entry's *old* id, and only a selector
/// beyond that — or an oldest entry that is itself a creation — is the `die()`.
#[test]
fn one_past_the_end_comes_from_the_oldest_entrys_old_id() {
    let (f, c1, c2, c3, c4) = four_commits("oldest");
    f.write_head_log(&[
        (c1.clone(), c2.clone(), T2, "commit: c2"),
        (c2, c3.clone(), T3, "commit: c3"),
        (c3, c4, T4, "commit: c4"),
    ]);

    let out = f.run(&["rev-parse", "HEAD@{3}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{c1}\n"));
    assert_eq!(err(&out), "");

    for nth in ["HEAD@{4}", "HEAD@{7}"] {
        let out = f.run(&["rev-parse", nth]);
        assert_eq!(code(&out), 128, "{nth}");
        assert_eq!(out_text(&out), "", "{nth}");
        assert_eq!(err(&out), "fatal: log for 'HEAD' only has 3 entries\n", "{nth}");
    }
}

/// `if (!cb.reccnt)`: a log file that exists but holds nothing. `@{0}` falls back
/// to the pre-seeded ref value in silence; anything else is a different `die()`
/// that names the **full** ref rather than the operand.
#[test]
fn an_empty_log_answers_zero_and_dies_for_everything_else() {
    let (f, _c1, _c2, _c3, c4) = four_commits("emptylog");
    std::fs::write(f.repo.join(".git/logs/HEAD"), "").unwrap();
    std::fs::write(f.repo.join(".git/logs/refs/heads/main"), "").unwrap();

    let out = f.run(&["rev-parse", "HEAD@{0}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{c4}\n"));
    assert_eq!(err(&out), "");

    // `nth` and the date form both pass a non-zero `cnt` (`-1` for a date), so both
    // take the `die()` — and it is spelled without quotes around the ref.
    for spec in ["HEAD@{1}", "HEAD@{2005-01-01}"] {
        let out = f.run(&["rev-parse", spec]);
        assert_eq!(code(&out), 128, "{spec}");
        assert_eq!(err(&out), "fatal: log for HEAD is empty\n", "{spec}");
    }
    let out = f.run(&["rev-parse", "main@{1}"]);
    assert_eq!(code(&out), 128);
    assert_eq!(err(&out), "fatal: log for refs/heads/main is empty\n");
}

/// A missing log is not an empty one: `repo_dwim_log()` skips the spelling
/// entirely, and with no rule left the operand simply does not resolve.
#[test]
fn a_missing_log_falls_through_to_the_ordinary_unknown_revision_text() {
    let (f, _c1, _c2, c3, _c4) = four_commits("nolog");
    // `logs/HEAD` gone but `logs/refs/heads/main` present is git's `else if
    // (strcmp(ref, path.buf) && refs_reflog_exists(refs, ref))` arm: HEAD's own
    // log is missing, so its *target's* log answers.
    std::fs::remove_file(f.repo.join(".git/logs/HEAD")).unwrap();
    let out = f.run(&["rev-parse", "HEAD@{1}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{c3}\n"));

    std::fs::remove_dir_all(f.repo.join(".git/logs")).unwrap();
    let out = f.run(&["rev-parse", "HEAD@{1}"]);
    assert_eq!(code(&out), 128);
    assert_eq!(out_text(&out), "HEAD@{1}\n");
    assert!(
        err(&out).starts_with("fatal: ambiguous argument 'HEAD@{1}': unknown revision"),
        "{}",
        err(&out)
    );
}

/// The date form shares the walk, so it shares the warnings — and adds
/// `get_oid_basic()`'s own when nothing is old enough (`object-name.c:795-800`).
#[test]
fn the_date_form_shares_the_walk_and_its_warnings() {
    let (f, c1, c2, c3, c4) = four_commits("dates");
    f.write_head_log(&[
        (NULL_ID.to_string(), c1.clone(), T1, "commit (initial): c1"),
        (c2, c3.clone(), T3, "commit: c3"),
        (c3, c4.clone(), T4, "commit: c4"),
    ]);

    // A date between the hole's two sides stops on the same entry `@{2}` does.
    let out = f.run(&["rev-parse", &format!("HEAD@{{{}}}", T2)]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{c1}\n"));
    assert_eq!(err(&out), GAP_AT_2211);

    // Older than every entry: `read_ref_at_ent_oldest()` answers with the oldest
    // record's *new* id, because its old id is the null id and `at_time` is set.
    let out = f.run(&["rev-parse", "HEAD@{2005-01-01}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{c1}\n"));
    assert_eq!(
        err(&out),
        "warning: log for 'HEAD' only goes back to Thu, 7 Apr 2005 22:11:13 +0200\n"
    );

    // Newer than every entry: the newest record matches, has no newer neighbour,
    // and agrees with the ref — silent.
    let out = f.run(&["rev-parse", &format!("HEAD@{{{}}}", T4 + 600)]);
    assert_eq!(out_text(&out), format!("{c4}\n"));
    assert_eq!(err(&out), "");
}


/// Every verb that resolves the operand prints the warning, once per resolution —
/// which is twice for the two operands of a `diff`, and twice for one `checkout`.
/// `--quiet` changes none of it.
#[test]
fn each_verb_repeats_the_warning_exactly_as_often_as_it_resolves() {
    let (f, _first, tip) = renamed_round_trip("verbs");

    for args in [
        vec!["rev-parse", "HEAD@{1}"],
        vec!["rev-parse", "--quiet", "--verify", "HEAD@{1}"],
        vec!["cat-file", "-t", "HEAD@{1}"],
        vec!["branch", "--contains", "HEAD@{1}"],
        vec!["merge-base", "HEAD@{1}", "HEAD"],
        vec!["rev-list", "--count", "HEAD@{1}"],
        vec!["log", "--format=%H", "-1", "HEAD@{1}"],
        vec!["show", "-s", "--format=%H", "HEAD@{1}"],
        vec!["diff", "--name-only", "HEAD@{1}", "HEAD"],
    ] {
        let out = f.run(&args);
        assert_eq!(code(&out), 0, "{args:?}: {}", err(&out));
        assert_eq!(err(&out), ENDED_AT_2213, "{args:?}");
    }

    // Two operands, two resolutions, two warnings.
    let out = f.run(&["diff", "--name-only", "HEAD@{1}", "HEAD@{1}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), format!("{ENDED_AT_2213}{ENDED_AT_2213}"));

    // The verbs that used to answer with the null id must now name a real commit.
    for args in [
        vec!["rev-parse", "HEAD@{1}"],
        vec!["show", "-s", "--format=%H", "HEAD@{1}"],
        vec!["log", "--format=%H", "-1", "HEAD@{1}"],
    ] {
        assert_eq!(out_text(&f.run(&args)), format!("{tip}\n"), "{args:?}");
    }
    assert_eq!(out_text(&f.run(&["rev-list", "--count", "HEAD@{1}"])), "2\n");
}

/// `git log -g <ref>@{<n>}` needs the operand to resolve to a *commit* before
/// `add_pending_object_with_path()` will queue the reflog walk at all
/// (`revision.c:306-314`), so the null id silently walked nothing.
#[test]
fn the_reflog_walk_starts_where_the_selector_resolved() {
    let (f, first, tip) = renamed_round_trip("walk");
    // Full ids rather than `--oneline`: the abbreviation length is `core.abbrev`'s
    // business and would make this assertion depend on the runner's config.
    let out = f.run(&["log", "-g", "--format=%H %gd: %gs", "HEAD@{1}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), ENDED_AT_2213);

    // The walk starts at the *resolved* commit, so it opens at `HEAD@{2}` — the
    // newest entry whose new id is that commit — and skips the two records whose
    // new id is the null id, which name no commit at all.
    assert_eq!(
        out_text(&out),
        format!(
            "{tip} HEAD@{{2}}: Branch: renamed refs/heads/main to refs/heads/other\n\
             {tip} HEAD@{{4}}: commit: two\n\
             {first} HEAD@{{5}}: commit (initial): one\n"
        )
    );
}

/// `setup_branch_path()` resolves the operand a second time whenever
/// `repo_dwim_ref()` did not claim it (`builtin/checkout.c:804-806`), and
/// `<ref>@{<n>}` is never a ref name — so a checkout warns twice where an
/// ambiguous plain name warns once.
#[test]
fn checkout_resolves_a_reflog_operand_twice() {
    let (f, _first, tip) = renamed_round_trip("checkout");
    let out = f.run(&["checkout", "-q", "--detach", "HEAD@{1}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), format!("{ENDED_AT_2213}{ENDED_AT_2213}"));
    assert_eq!(f.rev("HEAD"), tip);
}

/// `git reset` reaches `get_oid()` twice for its operand too, and both calls warn.
#[test]
fn reset_resolves_a_reflog_operand_twice() {
    let (f, _first, tip) = renamed_round_trip("reset");
    let out = f.run(&["reset", "--soft", "HEAD@{1}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), format!("{ENDED_AT_2213}{ENDED_AT_2213}"));
    assert_eq!(f.rev("HEAD"), tip);
}
