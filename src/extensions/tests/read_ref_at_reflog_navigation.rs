//! What `repo_get_oid()` does to a `<ref>@{…}` operand *around* the reflog read:
//! the reduction that decides which name `get_oid_basic()` is finally handed, and
//! the routing that decides which commands hear what it says.
//!
//! Sibling of `read_ref_at_reflog_selectors.rs`, which pins the walk itself. This
//! file pins the three things that surround it.
//!
//! **The reduction.** `repo_get_oid()` never hands `get_oid_basic()` the operand
//! as typed. `get_oid_with_context_1()` cuts a `<rev>:<path>` at the first colon
//! that is not inside an `@{…}`/`^{…}` group, `get_oid_1()` strips one
//! `~<n>`/`^<n>` and recurses, and `peel_onion()` strips a trailing `^{<type>}`
//! — each ending at one `get_oid_basic()` call on what is left, with the suffix
//! then applied to the *object* that came back:
//!
//! ```c
//! if (get_oid_1(r, name, sp - name - 2, &outer, lookup_flags))
//!         return -1;
//! o = parse_object(r, &outer);
//! ```
//!
//! (`object-name.c:959-962`.) So `HEAD@{1}^{tree}` is `HEAD@{1}`'s commit
//! peeled to its tree, never a reflog operand whose selector is `1}^{tree`.
//!
//! **The flags.** `get_parent()` and `get_nth_ancestor()` are the only steps that
//! do not pass `lookup_flags` down — they hand the recursion a literal
//! `GET_OID_COMMITTISH` (`object-name.c:828-834`, `object-name.c:858-867`) — so a
//! `~<n>`/`^<n>` anywhere in the reduction *loses* `GET_OID_QUIETLY`. That is why
//! `git rev-parse --quiet --verify 'HEAD@{<old date>}^'` warns although `--quiet`
//! was given, and why the second resolution `die_verify_filename()` performs
//! (`GET_OID_ONLY_TO_DIE | GET_OID_QUIETLY`, `object-name.c:1886`) repeats the
//! warning for `~<n>`/`^<n>` and stays quiet for `^{…}` and `:<path>`.
//!
//! **The routing.** All of it is raised from inside `get_oid_basic()`, so it
//! reaches every command that resolves an argv operand and no command that does
//! not — including the `die()`, which ends the process below `repo_get_oid()`'s
//! only return path and therefore cannot be declined by a caller.
//!
//! Every expectation below was measured against stock git 2.55.0
//! (`/opt/homebrew/bin/git`) on the same fixture before being written down.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The three commit timestamps the fixtures use, one minute apart. `+0200` is
/// recorded in the reflog line itself and `show_date(…, DATE_MODE(RFC2822))`
/// renders in *that* zone, so the warning text is the same on any machine.
const T1: &str = "1112904673 +0200";
const T2: &str = "1112904733 +0200";
const T3: &str = "1112904793 +0200";

/// The oldest entry of [`Fx::three_commits`], as the two reach-diagnostics date it.
const OLDEST: &str = "Thu, 7 Apr 2005 22:11:13 +0200";

/// A date older than every entry. `parse_date_basic()` rejects it — there is no
/// time of day — so `approxidate_careful()` falls through to `approxidate_str()`,
/// which is the arm every `@{<date>}` selector in this file exercises.
const OLD: &str = "2005-01-01";

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
        let root = std::env::temp_dir().join(format!("zvcs-refnav-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("work");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fx { root, repo };
        f.ok(&["init", "-q", "-b", "main", "."]);
        f
    }

    /// Three commits one minute apart, so `logs/HEAD` holds three entries and the
    /// oldest is dated [`OLDEST`].
    fn three_commits(tag: &str) -> Self {
        let f = Fx::new(tag);
        f.commit("1\n", "c1", T1);
        f.commit("1\n2\n", "c2", T2);
        f.commit("1\n2\n3\n", "c3", T3);
        f
    }

    /// [`Fx::three_commits`] with both logs emptied — the file exists and holds
    /// nothing, which is `read_ref_at()`'s `!cb.reccnt` with a non-zero `cnt`.
    fn empty_log(tag: &str) -> Self {
        let f = Fx::three_commits(tag);
        std::fs::write(f.repo.join(".git/logs/HEAD"), "").unwrap();
        std::fs::write(f.repo.join(".git/logs/refs/heads/main"), "").unwrap();
        f
    }

    /// A `git branch -m` round trip on two commits: `main` → `other` → `main`.
    /// Each rename logs a delete and a create, so `HEAD@{1}` selects a record
    /// whose new id is the null id and `read_ref_at()` answers with the ref's own
    /// value instead — the shape that makes the reduction observable.
    fn renamed(tag: &str) -> Self {
        let f = Fx::new(tag);
        f.commit("hi\n", "one", T3);
        f.commit("hi\ntwo\n", "two", T3);
        f.ok(&["branch", "-m", "main", "other"]);
        f.ok(&["branch", "-m", "other", "main"]);
        f
    }

    fn run(&self, args: &[&str]) -> Output {
        run_in(&self.repo, &self.root, T3, args)
    }

    fn ok(&self, args: &[&str]) {
        let out = self.run(args);
        assert!(out.status.success(), "git {args:?} failed: {}", err(&out));
    }

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

/// How many times `needle` appears in `out`'s stderr. The point of most of this
/// file is the *count*, not merely the presence.
fn times(out: &Output, needle: &str) -> usize {
    err(out).matches(needle).count()
}

const REACH: &str = "warning: log for 'HEAD' only goes back to";
const ENDED: &str = "warning: log for ref HEAD unexpectedly ended on";

// ---------------------------------------------------------------------------
// The reduction
// ---------------------------------------------------------------------------

/// `peel_onion()` resolves `sp - name - 2` characters and peels *that* object, so
/// a `^{<type>}` over a reflog operand is the peel of the entry's commit — not a
/// reflog read whose selector happens to end in `}`.
///
/// Routing the whole operand into the reader instead did not fail loudly: the
/// selector `1}^{tree` is not rejected by `approxidate_careful()`, which sets
/// `*error_ret` only when nothing in the string was `isdigit()` or `isalpha()`
/// (`date.c:1409-1410`), so the reader answered with the newest entry and every
/// `^{…}` spelling silently returned the commit the operand started from.
#[test]
fn a_peel_over_a_reflog_base_peels_the_entrys_object() {
    let f = Fx::three_commits("peel");
    let (c2, c3) = (f.rev("HEAD~1"), f.rev("HEAD"));
    let tree_of_c2 = f.rev("HEAD~1^{tree}");
    assert_ne!(c2, c3);
    assert_ne!(tree_of_c2, c2);

    for (spec, want) in [
        ("HEAD@{1}", &c2),
        ("HEAD@{1}^{commit}", &c2),
        ("HEAD@{1}^{}", &c2),
        ("HEAD@{1}^{object}", &c2),
        ("HEAD@{1}^{tree}", &tree_of_c2),
    ] {
        let out = f.run(&["rev-parse", spec]);
        assert_eq!(code(&out), 0, "{spec}: {}", err(&out));
        assert_eq!(out_text(&out), format!("{want}\n"), "{spec}");
        assert_eq!(err(&out), "", "{spec}");
    }
}

/// The other two reductions, on the fixture that tells them apart: after a
/// `git branch -m` round trip `HEAD@{1}` is *not* the entry's new id, so a suffix
/// resolved by gitoxide's own reflog reader walks from the null id and fails
/// outright where git answers.
#[test]
fn every_suffix_family_navigates_from_read_ref_ats_answer() {
    let f = Fx::renamed("suffix");
    let (first, tip) = (f.rev("HEAD~1"), f.rev("HEAD"));
    let tree_of_tip = f.rev("HEAD^{tree}");
    let blob_of_tip = f.rev("HEAD:f");

    // `HEAD@{1}` is the delete half of the rename pair; `read_ref_at()` answers
    // with the ref's current value and warns. Each suffix then works on *that*.
    for (spec, want) in [
        ("HEAD@{1}", &tip),
        ("HEAD@{1}^{commit}", &tip),
        ("HEAD@{1}^{tree}", &tree_of_tip),
        ("HEAD@{1}~1", &first),
        ("HEAD@{1}^", &first),
        ("HEAD@{1}^1", &first),
        ("HEAD@{1}:f", &blob_of_tip),
    ] {
        let out = f.run(&["rev-parse", spec]);
        assert_eq!(code(&out), 0, "{spec}: {}", err(&out));
        assert_eq!(out_text(&out), format!("{want}\n"), "{spec}");
        // The warning names the reflog and is raised once per resolution, so the
        // suffix neither silences it nor doubles it.
        assert_eq!(times(&out, ENDED), 1, "{spec}: {}", err(&out));
    }
}

/// The reduction reaches the verbs that resolve an operand for a walk, too — each
/// of which had its own copy of "is this a reflog operand?" keyed on the whole
/// spec.
#[test]
fn the_walkers_and_readers_navigate_from_the_same_answer() {
    let f = Fx::renamed("walkers");
    let tip = f.rev("HEAD");
    let tree_of_tip = f.rev("HEAD^{tree}");

    let out = f.run(&["log", "--format=%H", "-1", "HEAD@{1}^{commit}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{tip}\n"));

    let out = f.run(&["rev-list", "--count", "HEAD@{1}^{commit}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), "2\n");

    let out = f.run(&["show", "-s", "--format=%H", "HEAD@{1}^{commit}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{tip}\n"));

    let out = f.run(&["cat-file", "-t", "HEAD@{1}^{tree}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), "tree\n");

    let out = f.run(&["rev-parse", "HEAD@{1}^{tree}"]);
    assert_eq!(out_text(&out), format!("{tree_of_tip}\n"));
}

/// `get_oid_with_context_1()` counts bracket depth rather than taking the first
/// colon (`object-name.c:1821-1830`), so the clock in a date selector belongs to
/// the selector. Cutting at the first colon handed the reflog readers
/// `HEAD@{2005-01-01T00` — no trailing `}`, so not a reflog operand at all — and
/// every `@{<date>}` spelling carrying a time went undiagnosed.
#[test]
fn a_colon_inside_the_selector_is_part_of_the_selector() {
    let f = Fx::three_commits("colon");
    let c1 = f.rev("HEAD~2");

    for sel in [
        "2005-01-01",
        "2005-01-01T00:00:00+0000",
        "2005-01-01T00:00:00Z",
        "2005-01-01 00:00:00",
        "2005-01-01 00:00:00 +0000",
        "2005-04-07 22:10:00 +0200",
    ] {
        let spec = format!("HEAD@{{{sel}}}");
        let out = f.run(&["rev-parse", &spec]);
        assert_eq!(code(&out), 0, "{spec}: {}", err(&out));
        assert_eq!(out_text(&out), format!("{c1}\n"), "{spec}");
        assert_eq!(err(&out), format!("{REACH} {OLDEST}\n"), "{spec}");
    }

    // And the selector is not merely *reaching* the reader intact — it is read as
    // the same instant git reads it, which is what says the vendored
    // `approxidate_careful()` was never the problem. The three entries are one
    // minute apart, so a selector between two of them picks one side or the other.
    let (c1, c2, c3) = (f.rev("HEAD~2"), f.rev("HEAD~1"), f.rev("HEAD"));
    for (sel, want) in [
        ("2005-04-07 22:11:30 +0200", &c1),
        ("2005-04-07 22:12:30 +0200", &c2),
        ("2005-04-07T22:12:30+0200", &c2),
        ("2005-04-07 20:12:30 +0000", &c2),
        ("2005-04-07 22:13:30 +0200", &c3),
    ] {
        let spec = format!("HEAD@{{{sel}}}");
        let out = f.run(&["rev-parse", &spec]);
        assert_eq!(code(&out), 0, "{spec}: {}", err(&out));
        assert_eq!(out_text(&out), format!("{want}\n"), "{spec}");
        assert_eq!(err(&out), "", "{spec} is in range");
    }

    // Same operand, same one warning, through the shared resolver rather than
    // through `rev-parse`'s own.
    let spec = "HEAD@{2005-01-01T00:00:00+0000}";
    for args in [
        vec!["cat-file", "-t", spec],
        vec!["merge-base", spec, "HEAD"],
        vec!["log", "--format=%H", "-1", spec],
    ] {
        let out = f.run(&args);
        assert_eq!(code(&out), 0, "{args:?}: {}", err(&out));
        assert_eq!(err(&out), format!("{REACH} {OLDEST}\n"), "{args:?}");
    }
}

// ---------------------------------------------------------------------------
// The die(), and where it is raised from
// ---------------------------------------------------------------------------

/// `read_ref_at()` dies *inside* `get_oid_basic()`, which the reduction reaches
/// before any suffix is applied — so a selector past the end of the log is the
/// same fatal whatever follows it, and stdout stays empty because `show_file()`
/// never runs.
#[test]
fn an_out_of_range_selector_dies_before_the_suffix_is_applied() {
    let f = Fx::three_commits("outofrange");

    for spec in [
        "HEAD@{99}",
        "HEAD@{99}^",
        "HEAD@{99}~1",
        "HEAD@{99}^{commit}",
        "HEAD@{99}:f",
    ] {
        let out = f.run(&["rev-parse", spec]);
        assert_eq!(code(&out), 128, "{spec}: {}", err(&out));
        assert_eq!(out_text(&out), "", "{spec} must not reach show_file()");
        assert_eq!(err(&out), "fatal: log for 'HEAD' only has 3 entries\n", "{spec}");
    }
}

/// `if (flags & GET_OID_QUIETLY) exit(128); else die(…)` (`refs.c:1207-1210` and
/// `object-name.c:810-815`): `--quiet` takes the message away and leaves the exit
/// code — except where the reduction has already dropped the flag.
#[test]
fn quiet_takes_the_message_and_leaves_the_exit_code() {
    let f = Fx::three_commits("quietdie");

    for args in [
        vec!["rev-parse", "--quiet", "--verify", "HEAD@{99}"],
        vec!["rev-parse", "--quiet", "HEAD@{99}"],
    ] {
        let out = f.run(&args);
        assert_eq!(code(&out), 128, "{args:?}");
        assert_eq!(err(&out), "", "{args:?} must exit(128) rather than die()");
    }

    // `get_nth_ancestor()` hands the recursion a bare `GET_OID_COMMITTISH`, so
    // `GET_OID_QUIETLY` never reaches `get_oid_basic()` and the `die()` is loud.
    let out = f.run(&["rev-parse", "--quiet", "--verify", "HEAD@{99}~1"]);
    assert_eq!(code(&out), 128);
    assert_eq!(err(&out), "fatal: log for 'HEAD' only has 3 entries\n");

    // The same asymmetry on the warning, which is gated by the same flag.
    let reach = format!("HEAD@{{{OLD}}}");
    let out = f.run(&["rev-parse", "--quiet", "--verify", &reach]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), "");
    let out = f.run(&["rev-parse", "--quiet", "--verify", &format!("{reach}^")]);
    assert_eq!(code(&out), 1, "{}", err(&out));
    assert_eq!(err(&out), format!("{REACH} {OLDEST}\n"));
}

/// The `die()` is raised below `repo_get_oid()`'s only return path, so no caller
/// can decline it: every verb that resolves an argv operand ends here, with the
/// same bytes and the same exit code, rather than with its own "not a valid
/// object name".
#[test]
fn the_empty_log_fatal_reaches_every_verb_that_resolves_an_operand() {
    let f = Fx::empty_log("emptyroute");
    const FATAL: &str = "fatal: log for HEAD is empty\n";
    let tree = f.rev("HEAD^{tree}");

    for args in [
        vec!["rev-parse", "HEAD@{1}"],
        vec!["rev-parse", "--verify", "HEAD@{1}"],
        vec!["cat-file", "-t", "HEAD@{1}"],
        vec!["cat-file", "-p", "HEAD@{1}"],
        vec!["merge-base", "HEAD@{1}", "HEAD"],
        vec!["merge-base", "--is-ancestor", "HEAD@{1}", "HEAD"],
        vec!["branch", "--contains", "HEAD@{1}"],
        vec!["tag", "--contains", "HEAD@{1}"],
        vec!["for-each-ref", "--contains", "HEAD@{1}"],
        vec!["log", "--format=%H", "-1", "HEAD@{1}"],
        vec!["rev-list", "--count", "HEAD@{1}"],
        vec!["show", "-s", "--format=%H", "HEAD@{1}"],
        vec!["diff", "--name-only", "HEAD@{1}", "HEAD"],
        vec!["name-rev", "HEAD@{1}"],
        vec!["describe", "--always", "HEAD@{1}"],
        vec!["ls-tree", "--name-only", "HEAD@{1}"],
        vec!["grep", "-e", "1", "HEAD@{1}"],
        vec!["blame", "-L1,1", "--porcelain", "f", "HEAD@{1}"],
        vec!["ls-files", "--with-tree=HEAD@{1}"],
        vec!["read-tree", "HEAD@{1}"],
        vec!["commit-tree", "-p", "HEAD@{1}", "-m", "x", &tree],
        vec!["show-branch", "HEAD@{1}"],
        vec!["cherry", "HEAD", "HEAD@{1}"],
        vec!["verify-tag", "HEAD@{1}"],
    ] {
        let out = f.run(&args);
        assert_eq!(code(&out), 128, "{args:?}: {}", err(&out));
        assert_eq!(err(&out), FATAL, "{args:?}");
    }

    // `@{0}` is the one selector an empty log still answers: `read_ref_at()`
    // returns 1 with the value the caller pre-seeded and `co_cnt == 0`, which
    // `get_oid_basic()`'s `nth == co_cnt` arm accepts in silence.
    let out = f.run(&["cat-file", "-t", "HEAD@{0}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), "");
}

// ---------------------------------------------------------------------------
// The warning, and where it is raised from
// ---------------------------------------------------------------------------

/// `warning: log for '<ref>' only goes back to …` comes out of the same
/// `get_oid_basic()` call, so it reaches the same set of verbs — once each.
///
/// The negative half matters as much: `git describe` without `--always` dies at
/// `No names found, cannot describe anything.` before it ever resolves the
/// operand, so stock is silent there, and a warning from a verb git is silent in
/// is as wrong as a missing one.
#[test]
fn the_reach_warning_reaches_every_verb_that_resolves_an_operand() {
    let f = Fx::three_commits("reachroute");
    let spec = format!("HEAD@{{{OLD}}}");
    let spec = spec.as_str();
    let expected = format!("{REACH} {OLDEST}\n");
    let tree = f.rev("HEAD^{tree}");

    for args in [
        vec!["rev-parse", spec],
        vec!["rev-parse", "--verify", spec],
        vec!["cat-file", "-t", spec],
        vec!["cat-file", "-p", spec],
        vec!["merge-base", spec, "HEAD"],
        vec!["merge-base", "--is-ancestor", spec, "HEAD"],
        vec!["branch", "--contains", spec],
        vec!["branch", "--merged", spec],
        vec!["tag", "--contains", spec],
        vec!["for-each-ref", "--contains", spec],
        vec!["log", "--format=%H", "-1", spec],
        vec!["rev-list", "--count", spec],
        vec!["show", "-s", "--format=%H", spec],
        vec!["diff", "--name-only", spec, "HEAD"],
        vec!["name-rev", spec],
        vec!["describe", "--always", spec],
        vec!["ls-tree", "--name-only", spec],
        vec!["grep", "-e", "1", spec],
        vec!["ls-files", &format!("--with-tree={spec}")],
        vec!["commit-tree", "-p", spec, "-m", "x", &tree],
        vec!["cherry", "HEAD", spec],
    ] {
        let out = f.run(&args);
        assert_eq!(code(&out), 0, "{args:?}: {}", err(&out));
        assert_eq!(err(&out), expected, "{args:?}");
    }

    // Two resolutions, two warnings — `blame` resolves the operand and then the
    // start commit, `show-branch` once per rev it collects.
    for args in [
        vec!["blame", "-L1,1", "--porcelain", "f", spec],
        vec!["show-branch", spec],
    ] {
        let out = f.run(&args);
        assert_eq!(code(&out), 0, "{args:?}: {}", err(&out));
        assert_eq!(times(&out, REACH), 2, "{args:?}: {}", err(&out));
    }

    // Silent in stock, and therefore silent here: the operand is never resolved.
    let out = f.run(&["describe", spec]);
    assert_eq!(code(&out), 128);
    assert_eq!(err(&out), "fatal: No names found, cannot describe anything.\n");
    for args in [vec!["symbolic-ref", "HEAD"], vec!["status", "--short"]] {
        let out = f.run(&args);
        assert_eq!(times(&out, REACH), 0, "{args:?}");
    }
}

/// `try_difference()` cuts at the `..` and resolves each side with
/// `repo_get_oid_committish()` (`builtin/rev-parse.c:269-326`) — never the range
/// as a unit — so an endpoint the reflog reader owns is an ordinary resolution
/// and the range grammar is not consulted about it at all.
///
/// Handing `HEAD@{1}..HEAD` to gitoxide's range parser instead put the *null id*
/// on stdout as `^0000000000000000000000000000000000000000` after a
/// `git branch -m` round trip, silently, with exit 0.
#[test]
fn a_range_endpoint_is_an_ordinary_resolution() {
    let f = Fx::renamed("range");
    let tip = f.rev("HEAD");

    let out = f.run(&["rev-parse", "HEAD@{1}..HEAD"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{tip}\n^{tip}\n"));
    assert_eq!(times(&out, ENDED), 1);

    // Symmetric: both endpoints, then the merge base.
    let out = f.run(&["rev-parse", "HEAD@{1}...HEAD"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{tip}\n{tip}\n^{tip}\n"));

    // The right-hand side goes through the same resolver, and so does a suffixed
    // endpoint.
    for spec in ["HEAD..HEAD@{1}", "HEAD@{1}^{commit}..HEAD"] {
        let out = f.run(&["rev-parse", spec]);
        assert_eq!(code(&out), 0, "{spec}: {}", err(&out));
        assert_eq!(out_text(&out), format!("{tip}\n^{tip}\n"), "{spec}");
    }

    // And the `die()` reaches an endpoint too, before anything is echoed.
    let g = Fx::three_commits("rangedie");
    for spec in ["HEAD@{99}..HEAD", "HEAD..HEAD@{99}"] {
        let out = g.run(&["rev-parse", spec]);
        assert_eq!(code(&out), 128, "{spec}: {}", err(&out));
        assert_eq!(out_text(&out), "", "{spec}");
        assert_eq!(err(&out), "fatal: log for 'HEAD' only has 3 entries\n", "{spec}");
    }
}

/// `batch_one_object()` reaches `get_oid_with_context()` once per line
/// (`builtin/cat-file.c`), so a batch is not an exception to any of it: the
/// warnings come out per line, and the `die()` ends the whole batch with whatever
/// was already on stdout still on stdout — `exit()` flushes the stdio buffer.
///
/// The `warn_on_object_refname_ambiguity` bracket `batch_objects()` holds takes
/// the *full-hex* ambiguity warning out and nothing else; none of the reflog
/// diagnostics consults that switch.
#[test]
fn a_batch_hears_all_of_it_line_by_line() {
    let f = Fx::three_commits("batch");
    let tip = f.rev("HEAD");
    let c1 = f.rev("HEAD~2");

    let run_batch = |stdin: &str| -> Output {
        let mut child = Command::new(BIN)
            .args(["cat-file", "--batch-check"])
            .current_dir(&f.repo)
            .env("HOME", &f.root)
            .env("ZVCS_HOME", &f.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    };

    // The reach warning, per line, with the line still answered.
    let out = run_batch(&format!("HEAD@{{{OLD}}}\n"));
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(out_text(&out).starts_with(&format!("{c1} commit ")), "{}", out_text(&out));
    assert_eq!(err(&out), format!("{REACH} {OLDEST}\n"));

    // The `die()` ends the batch, and the lines already emitted survive it.
    let out = run_batch("HEAD\nHEAD@{99}\nHEAD\n");
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert!(out_text(&out).starts_with(&format!("{tip} commit ")), "{}", out_text(&out));
    assert_eq!(out_text(&out).lines().count(), 1, "the batch stops at the die()");
    assert_eq!(err(&out), "fatal: log for 'HEAD' only has 3 entries\n");

    // `get_oid_1()` has no case for `^!`, so a batch reports it missing rather
    // than resolving it through the wider revspec grammar.
    let out = run_batch("HEAD^!\n");
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), "HEAD^! missing\n");
    assert_eq!(err(&out), "");
}

// ---------------------------------------------------------------------------
// die_verify_filename()'s second resolution
// ---------------------------------------------------------------------------

/// `die_verify_filename()` resolves the operand once more before it dies:
///
/// ```c
/// get_oid_with_context_1(r, name, GET_OID_ONLY_TO_DIE | GET_OID_QUIETLY,
///                        prefix, &oid, &oc);
/// ```
///
/// (`object-name.c:1886`.) So an operand that failed hears `get_oid_basic()`
/// twice, and each diagnostic repeats or not according to its own gate:
/// `read_ref_at()`'s `warning()` has none and always repeats, while
/// `only goes back to` answers to `GET_OID_QUIETLY` — which the second pass sets,
/// and which only a `~<n>`/`^<n>` step takes back off.
#[test]
fn the_failure_path_resolves_twice_and_repeats_what_it_is_not_gated_out_of() {
    let f = Fx::three_commits("twice");
    let reach = format!("HEAD@{{{OLD}}}");

    // `GET_OID_QUIETLY` dropped by `get_parent()`/`get_nth_ancestor()`: twice.
    for spec in [format!("{reach}^"), format!("{reach}~99")] {
        let out = f.run(&["rev-parse", &spec]);
        assert_eq!(code(&out), 128, "{spec}: {}", err(&out));
        assert_eq!(times(&out, REACH), 2, "{spec}: {}", err(&out));
    }

    // `get_oid_with_context_1()`'s path arm keeps the caller's flags, so the
    // second pass is quiet: once.
    let out = f.run(&["rev-parse", &format!("{reach}:nosuch")]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(times(&out, REACH), 1, "{}", err(&out));
    assert!(err(&out).ends_with(&format!("fatal: path 'nosuch' does not exist in '{reach}'\n")));

    // Resolving is not failing: an operand that answers is resolved once.
    let out = f.run(&["rev-parse", &reach]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(times(&out, REACH), 1);

    // `--verify` dies at `die_no_single_rev()` instead, which performs no second
    // resolution at all.
    let out = f.run(&["rev-parse", "--verify", &format!("{reach}^")]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(times(&out, REACH), 1, "{}", err(&out));
}

/// `read_ref_at()`'s own `warning()` has no `flags` gate at all — `refs.c:1135`
/// and `refs.c:1141` call `warning()` outright — so the second resolution repeats
/// it whatever the suffix was, and `--quiet` does not take it away.
#[test]
fn the_ungated_warning_repeats_on_every_resolution() {
    let f = Fx::renamed("twiceungated");

    for spec in ["HEAD@{1}~99", "HEAD@{1}:nosuch"] {
        let out = f.run(&["rev-parse", spec]);
        assert_eq!(code(&out), 128, "{spec}: {}", err(&out));
        assert_eq!(times(&out, ENDED), 2, "{spec}: {}", err(&out));
    }

    // `cmd_rev_parse()` advances past the exclusion mark before resolving but
    // `die_verify_filename()` is handed `arg`, whose leading `^` stops
    // `get_oid_basic()` from finding the operand at all — so the second pass says
    // nothing and the count drops back to one.
    let out = f.run(&["rev-parse", "^HEAD@{1}~99"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(times(&out, ENDED), 1, "{}", err(&out));

    // Once, not twice, for an operand that resolves.
    let out = f.run(&["rev-parse", "HEAD@{1}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(times(&out, ENDED), 1);
}

/// The second resolution carries `get_oid_basic()`'s *other* warning too, under
/// the gates that branch answers to: the full-hex one tests
/// `GET_OID_SKIP_AMBIGUITY_CHECK` and so repeats unconditionally, while the
/// plain-name one tests `GET_OID_QUIETLY` and repeats only when the reduction
/// dropped it.
#[test]
fn the_ambiguity_warning_follows_the_same_two_pass_rule() {
    let f = Fx::three_commits("ambigtwice");
    f.ok(&["branch", "dup"]);
    f.ok(&["tag", "dup"]);
    let hex = f.rev("HEAD");
    f.ok(&["update-ref", &format!("refs/heads/{hex}"), "HEAD"]);
    let plain = "warning: refname 'dup' is ambiguous.";
    let full = format!("warning: refname '{hex}' is ambiguous.");

    // Plain name: `GET_OID_QUIETLY` survives `^{…}` and `:<path>`, so once.
    for spec in ["dup:nosuch", "dup^{blob}"] {
        let out = f.run(&["rev-parse", spec]);
        assert_eq!(code(&out), 128, "{spec}: {}", err(&out));
        assert_eq!(times(&out, plain), 1, "{spec}: {}", err(&out));
    }
    // …and does not survive `~<n>`/`^<n>`, so twice.
    for spec in ["dup~99", "dup^99"] {
        let out = f.run(&["rev-parse", spec]);
        assert_eq!(code(&out), 128, "{spec}: {}", err(&out));
        assert_eq!(times(&out, plain), 2, "{spec}: {}", err(&out));
    }
    // Which `--quiet` cannot put back: the flag is gone before the warning.
    let out = f.run(&["rev-parse", "--quiet", "--verify", "dup~99"]);
    assert_eq!(code(&out), 1, "{}", err(&out));
    assert_eq!(times(&out, plain), 1, "{}", err(&out));
    let out = f.run(&["rev-parse", "--quiet", "--verify", "dup"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(times(&out, plain), 0);

    // Full hex: no `GET_OID_QUIETLY` gate anywhere, so both passes say it —
    // including for the bracket-aware colon, which a plain `strchr` split had
    // been cutting at.
    for spec in [format!("{hex}:nosuch"), format!("{hex}~99"), format!("{hex}^{{/x:y}}")] {
        let out = f.run(&["rev-parse", &spec]);
        assert_eq!(code(&out), 128, "{spec}: {}", err(&out));
        assert_eq!(times(&out, &full), 2, "{spec}: {}", err(&out));
    }
    let out = f.run(&["rev-parse", &format!("{hex}^{{commit}}")]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(times(&out, &full), 1, "{}", err(&out));
}
