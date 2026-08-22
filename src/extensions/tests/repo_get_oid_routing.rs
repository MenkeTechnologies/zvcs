//! The rest of `repo_get_oid()`: the diagnostics `get_oid_basic()` and
//! `peel_onion()` raise that are *not* `read_ref_at()`'s, and the commands that
//! were resolving their operands without going through it.
//!
//! Third of the set. `read_ref_at_reflog_selectors.rs` pins the reflog walk,
//! `read_ref_at_reflog_navigation.rs` pins the reduction around it and the
//! routing of its three messages, and this file pins the two diagnostics that
//! ride the same call and the five verbs that were not hearing any of them.
//!
//! **`peel_onion()`'s `error()`.** `repo_peel_to_type()` reports a type it cannot
//! reach through `error()` (`object-name.c:897-903`) and returns NULL;
//! `peel_onion()` then returns -1, and `get_oid_1()` **carries on**:
//!
//! ```c
//! ret = peel_onion(r, name, len, oid, lookup_flags);
//! if (!ret)
//!         return FOUND;
//!
//! ret = get_oid_basic(r, name, len, oid, lookup_flags);
//! ```
//!
//! (`object-name.c:1128-1132`.) So the line is not a failure report: the operand
//! may still resolve, out of a second `get_oid_basic()` call that is handed the
//! name **whole**. For a reflog operand that second call is a second reflog read
//! with a different selector — `approxidate_careful()` accepts `2005-01-01}^{blob`
//! because it sets `*error_ret` only when nothing in the string was `isdigit()`
//! or `isalpha()` (`date.c:1409-1410`) — which is why stock 2.55.0 answers
//! `git rev-parse 'HEAD@{<old date>}^{blob}'` with an id on stdout, exit 0, and
//! the reach warning **twice** around the `error:` line.
//!
//! The frame that reports is not generally the operand's own, either. The
//! reduction cuts `:<path>` and `~<n>`/`^<n>` before `peel_onion()` is entered, so
//! `HEAD^{blob}:f`, `HEAD^{blob}^` and `HEAD^{blob}~1` all name `HEAD^{blob}` in
//! the message.
//!
//! **`interpret_branch_mark()`'s `die()`.** `repo_dwim_ref()` runs
//! `substitute_branch_name()` before it expands anything (`refs.c:795-803`), and
//! an `@{u}`/`@{push}` mark that names no upstream dies in there — inside
//! `get_oid_basic()` (`object-name.c:748`), below `repo_get_oid()`'s only return
//! path, so no caller can decline it or report it in its own words.
//!
//! Every expectation below was measured against stock git 2.55.0
//! (`/opt/homebrew/bin/git`) on the same fixture before being written down.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The three commit timestamps the fixture uses, one minute apart. `+0200` is
/// recorded in the reflog line itself and `show_date(…, DATE_MODE(RFC2822))`
/// renders in *that* zone, so the warning text is the same on any machine.
const T1: &str = "1112904673 +0200";
const T2: &str = "1112904733 +0200";
const T3: &str = "1112904793 +0200";

/// The oldest entry of [`Fx::three_commits`], as the reach warning dates it.
const OLDEST: &str = "Thu, 7 Apr 2005 22:11:13 +0200";

/// A date older than every entry.
const OLD: &str = "2005-01-01";

const REACH: &str = "warning: log for 'HEAD' only goes back to";

const AMBIGUOUS_TAIL: &str = "unknown revision or path not in the working tree.\n\
                              Use '--' to separate paths from revisions, like this:\n\
                              'git <command> [<revision>...] -- [<file>...]'";

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
    command_in(repo, home, date, args).stdin(Stdio::null()).output().unwrap()
}

fn command_in(repo: &Path, home: &Path, date: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

impl Fx {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-getoid-{tag}-{}", std::process::id()));
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

    fn run(&self, args: &[&str]) -> Output {
        run_in(&self.repo, &self.root, T3, args)
    }

    /// One `--batch-check` line, so the `@{u}` die can be observed where it was
    /// first noticed: `batch_objects()` reaches `get_oid_with_context()` per line.
    fn batch(&self, stdin: &str) -> Output {
        let mut child = command_in(&self.repo, &self.root, T3, &["cat-file", "--batch-check"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
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

/// How many times `needle` appears in `out`'s stderr. Several of the rules below
/// are about the *count*, not the presence.
fn times(out: &Output, needle: &str) -> usize {
    err(out).matches(needle).count()
}

/// `repo_peel_to_type()`'s message for `name`, which always ends at a tree: a tag
/// peels to its target, a commit to its tree, and a tree has nowhere left to go.
fn peel_error(name: &str, want: &str) -> String {
    format!("error: {name}: expected {want} type, but the object dereferences to tree type\n")
}

// ---------------------------------------------------------------------------
// peel_onion()'s error(), and get_oid_1()'s fallback behind it
// ---------------------------------------------------------------------------

/// The whole of residual (1) in one operand: the `error:` line comes out *during*
/// the resolution, and the resolution then succeeds out of `get_oid_1()`'s
/// fallback — which reads the reflog a second time and so warns a second time.
#[test]
fn a_failed_peel_over_a_reflog_base_reports_and_then_resolves_anyway() {
    let f = Fx::three_commits("peelfallback");
    let c1 = f.rev("HEAD~2");
    let reach = format!("{REACH} {OLDEST}\n");

    for (spec, want) in [
        (format!("HEAD@{{{OLD}}}^{{blob}}"), "blob"),
        (format!("HEAD@{{{OLD}}}^{{tag}}"), "tag"),
    ] {
        let out = f.run(&["rev-parse", &spec]);
        assert_eq!(code(&out), 0, "{spec}: {}", err(&out));
        assert_eq!(out_text(&out), format!("{c1}\n"), "{spec}");
        assert_eq!(
            err(&out),
            format!("{reach}{}{reach}", peel_error(&spec, want)),
            "{spec}"
        );
    }
}

/// The fallback only warns when it has something to warn about: `@{0}` is in
/// range, and the selector the second `get_oid_basic()` call is handed —
/// `0}^{blob` — is a date `approxidate_careful()` reads as "now", so the reader
/// answers with the tip in silence and only the `error:` line survives.
#[test]
fn the_fallback_says_nothing_when_the_second_read_is_in_range() {
    let f = Fx::three_commits("peelinrange");
    let tip = f.rev("HEAD");

    let out = f.run(&["rev-parse", "HEAD@{0}^{blob}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{tip}\n"));
    assert_eq!(err(&out), peel_error("HEAD@{0}^{blob}", "blob"));
}

/// A peel that *works*, and one `peel_onion()` declines to attempt at all, are
/// both single-warning operands — the first because `get_oid_1()` returns at
/// `object-name.c:1129-1130`, the second because `peel_onion()` bails at
/// `object-name.c:950-951` before it resolves anything, leaving the whole name to
/// `get_oid_basic()` and its one reflog read.
#[test]
fn a_peel_that_does_not_report_leaves_exactly_one_reflog_read() {
    let f = Fx::three_commits("peelquiet");
    let c1 = f.rev("HEAD~2");
    let tree_of_c1 = f.rev("HEAD~2^{tree}");
    let reach = format!("{REACH} {OLDEST}\n");

    for (suffix, want) in [
        ("^{commit}", &c1),
        ("^{object}", &c1),
        ("^{}", &c1),
        ("^{tree}", &tree_of_c1),
        // `starts_with(sp, …)` matches none of the five type names, so
        // `peel_onion()` returns -1 without calling `get_oid_1()` at all.
        ("^{nonsense}", &c1),
    ] {
        let spec = format!("HEAD@{{{OLD}}}{suffix}");
        let out = f.run(&["rev-parse", &spec]);
        assert_eq!(code(&out), 0, "{spec}: {}", err(&out));
        assert_eq!(out_text(&out), format!("{want}\n"), "{spec}");
        assert_eq!(err(&out), reach, "{spec}");
    }
}

/// `read_ref_at()`'s `die()` is raised by the *first* `get_oid_basic()` call, so
/// it ends the command before `peel_onion()` can reach `repo_peel_to_type()` and
/// there is no `error:` line at all.
#[test]
fn the_die_precedes_the_peel() {
    let f = Fx::three_commits("peeldie");
    let out = f.run(&["rev-parse", "HEAD@{99}^{blob}"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(out_text(&out), "");
    assert_eq!(err(&out), "fatal: log for 'HEAD' only has 3 entries\n");
}

/// The `error:` is raised from inside `repo_get_oid()`, so it reaches every
/// command that resolves an argv operand — not only the two that had been
/// printing it from their own failure paths.
///
/// The count is the point: `rev-parse` and the revision walk resolve a failing
/// operand twice (`die_verify_filename()` → `maybe_die_on_misspelt_object_name()`,
/// `object-name.c:1880-1889`) and print it twice; every other verb resolves once.
#[test]
fn the_peel_error_reaches_every_verb_that_resolves_an_operand() {
    let f = Fx::three_commits("peelroute");
    let expected = peel_error("HEAD^{blob}", "blob");

    for args in [
        vec!["cat-file", "-t", "HEAD^{blob}"],
        vec!["merge-base", "HEAD^{blob}", "HEAD"],
        vec!["ls-tree", "--name-only", "HEAD^{blob}"],
    ] {
        let out = f.run(&args);
        assert_eq!(code(&out), 128, "{args:?}: {}", err(&out));
        assert_eq!(
            err(&out),
            format!("{expected}fatal: Not a valid object name HEAD^{{blob}}\n"),
            "{args:?}"
        );
    }

    // `name-rev` reports a name it could not resolve and carries on, so the
    // `error:` line is the only sign the peel was attempted.
    let out = f.run(&["name-rev", "HEAD^{blob}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(
        err(&out),
        format!("{expected}Could not get sha1 for HEAD^{{blob}}. Skipping.\n")
    );

    // Twice for the two verbs that resolve a failing operand a second time.
    for args in [
        vec!["rev-parse", "HEAD^{blob}"],
        vec!["log", "--format=%H", "-1", "HEAD^{blob}"],
        vec!["rev-list", "--count", "HEAD^{blob}"],
    ] {
        let out = f.run(&args);
        assert_eq!(code(&out), 128, "{args:?}: {}", err(&out));
        assert_eq!(
            err(&out),
            format!(
                "{expected}{expected}fatal: ambiguous argument 'HEAD^{{blob}}': {AMBIGUOUS_TAIL}\n"
            ),
            "{args:?}"
        );
    }
}

/// The message names the frame `peel_onion()` was entered with, which the
/// reduction has already cut the outer suffixes off — so all three suffix
/// families report `HEAD^{blob}` and not the operand.
#[test]
fn the_message_names_the_reduced_frame() {
    let f = Fx::three_commits("peelreduce");
    let expected = peel_error("HEAD^{blob}", "blob");

    for spec in ["HEAD^{blob}^", "HEAD^{blob}~1", "HEAD^{blob}:f"] {
        let out = f.run(&["cat-file", "-t", spec]);
        assert_eq!(code(&out), 128, "{spec}: {}", err(&out));
        assert!(err(&out).starts_with(&expected), "{spec}: {}", err(&out));
        assert_eq!(times(&out, "error: "), 1, "{spec}: {}", err(&out));
    }

    // And the walk stops at the *innermost* frame that reports: an outer
    // `peel_onion()` whose `get_oid_1()` failed bails at `object-name.c:959-960`
    // before `repo_peel_to_type()`, so it has nothing to say.
    let out = f.run(&["cat-file", "-t", "HEAD^{blob}^{tree}"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(
        err(&out),
        format!("{expected}fatal: Not a valid object name HEAD^{{blob}}^{{tree}}\n")
    );

    // Reversed, the inner peel succeeds and the outer one is the frame that
    // reports — naming the whole operand.
    let out = f.run(&["cat-file", "-t", "HEAD^{tree}^{blob}"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(
        err(&out),
        format!(
            "{}fatal: Not a valid object name HEAD^{{tree}}^{{blob}}\n",
            peel_error("HEAD^{tree}^{blob}", "blob")
        )
    );
}

// ---------------------------------------------------------------------------
// cmd_rev_parse()'s `name`
// ---------------------------------------------------------------------------

/// ```c
/// name = arg;
/// type = NORMAL;
/// if (*arg == '^') { name++; type = REVERSED; }
/// if (!repo_get_oid_with_flags(the_repository, name, &oid, flags)) {
/// ```
///
/// (`builtin/rev-parse.c:1163-1177`.) Every rule below the strip is applied to
/// `name`, the reflog branch included — and `repo_dwim_log("^HEAD")` cannot match
/// anything, because `check_refname_format()` bans `^` in a refname
/// (`refname_disposition[0x5e] == 4`, `refs.c:80-89`). Resolving the operand as
/// written therefore turned stock's `^<oid>` into `ambiguous argument`.
#[test]
fn the_exclusion_mark_is_stripped_before_the_operand_is_resolved() {
    let f = Fx::three_commits("caret");
    let (c2, tip) = (f.rev("HEAD~1"), f.rev("HEAD"));

    for (spec, want) in
        [("^HEAD", &tip), ("^HEAD@{0}", &tip), ("^HEAD@{1}", &c2), ("^HEAD~1", &c2)]
    {
        let out = f.run(&["rev-parse", spec]);
        assert_eq!(code(&out), 0, "{spec}: {}", err(&out));
        assert_eq!(out_text(&out), format!("^{want}\n"), "{spec}");
        assert_eq!(err(&out), "", "{spec}");
    }

    // `show_rev(type, &oid, name)` echoes the stripped name, so the `^` is the
    // only thing the mark contributes to the symbolic forms.
    for flag in ["--symbolic", "--abbrev-ref"] {
        let out = f.run(&["rev-parse", flag, "^main"]);
        assert_eq!(code(&out), 0, "{flag}: {}", err(&out));
        assert_eq!(out_text(&out), "^main\n", "{flag}");
    }

    // A range is claimed by `try_difference()` two lines above the strip, so its
    // caret is not the mark and its endpoints keep their own spelling.
    let out = f.run(&["rev-parse", "HEAD@{1}..HEAD"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{tip}\n^{c2}\n"));
}

// ---------------------------------------------------------------------------
// interpret_branch_mark()'s die()
// ---------------------------------------------------------------------------

/// `repo_dwim_ref()` runs `substitute_branch_name()` first (`refs.c:795-803`), so
/// an `@{u}`/`@{push}` mark that cannot be resolved to an upstream dies inside
/// `get_oid_basic()` (`object-name.c:748`) — below `repo_get_oid()`'s only return
/// path, which is why no verb gets to report it in its own words.
///
/// `cat-file --batch-check` is where the gap was noticed: it answered `missing`,
/// which is what a line that fails to resolve prints, for an operand stock never
/// gets far enough to call missing.
#[test]
fn an_unresolvable_upstream_mark_dies_inside_the_resolution() {
    let f = Fx::three_commits("upstream");
    const FATAL: &str = "fatal: no upstream configured for branch 'main'\n";

    // `upstream_mark()`/`push_mark()` are `strncasecmp`, and `branch_get(NULL)`
    // and `branch_get("HEAD")` are the same lookup, so all six spellings name the
    // checked-out branch.
    for spec in ["main@{u}", "@{u}", "@{upstream}", "main@{upstream}", "@{push}", "main@{push}"] {
        let out = f.batch(&format!("{spec}\n"));
        assert_eq!(code(&out), 128, "{spec}: {}", err(&out));
        assert_eq!(out_text(&out), "", "{spec} must not reach the missing line");
        assert_eq!(err(&out), FATAL, "{spec}");

        let out = f.run(&["cat-file", "-t", spec]);
        assert_eq!(code(&out), 128, "{spec}: {}", err(&out));
        assert_eq!(err(&out), FATAL, "{spec}");
    }

    // The same `die()`, through the same resolver, from verbs that never had it.
    for args in [
        vec!["merge-base", "main@{u}", "HEAD"],
        vec!["ls-tree", "--name-only", "main@{u}"],
        vec!["diff-tree", "--name-only", "main@{u}", "HEAD"],
        vec!["merge-tree", "main@{u}", "HEAD"],
        vec!["update-ref", "refs/heads/tmp", "main@{u}"],
    ] {
        let out = f.run(&args);
        assert_eq!(code(&out), 128, "{args:?}: {}", err(&out));
        assert_eq!(err(&out), FATAL, "{args:?}");
    }

    // `branch_get()` on a name whose ref does not exist is the other arm.
    let out = f.run(&["cat-file", "-t", "nosuch@{u}"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), "fatal: no such branch: 'nosuch'\n");
}

/// The `die()` is about `branch->merge[0]->dst`, not about the remote: a branch
/// with `branch.<n>.remote` and no `branch.<n>.merge` is still "no upstream
/// configured", and a `merge` no fetch refspec maps back is the third message.
#[test]
fn the_upstream_die_distinguishes_its_three_causes() {
    let f = Fx::three_commits("upstreamcauses");
    f.ok(&["config", "remote.origin.url", "https://example.invalid/r.git"]);
    f.ok(&["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);

    // A remote, but nothing to merge with.
    f.ok(&["config", "branch.main.remote", "origin"]);
    let out = f.run(&["cat-file", "-t", "main@{u}"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), "fatal: no upstream configured for branch 'main'\n");

    // A merge ref the fetch refspec does not map to a remote-tracking name.
    f.ok(&["config", "branch.main.merge", "refs/nope/main"]);
    let out = f.run(&["cat-file", "-t", "main@{u}"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(
        err(&out),
        "fatal: upstream branch 'refs/nope/main' not stored as a remote-tracking branch\n"
    );

    // And a configured upstream is not a `die()` at all: the operand resolves to
    // the remote-tracking ref's own value.
    f.ok(&["config", "branch.main.merge", "refs/heads/main"]);
    f.ok(&["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
    let c2 = f.rev("HEAD~1");
    let out = f.run(&["rev-parse", "main@{u}"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), format!("{c2}\n"));
    assert_eq!(err(&out), "");
}

// ---------------------------------------------------------------------------
// The five verbs that were resolving their operands themselves
// ---------------------------------------------------------------------------

/// `diff-tree`, `update-ref` and `merge-tree` reach `repo_get_oid()` once per
/// operand in stock, so they hear everything it raises — the warning that leaves
/// the operand resolvable and the `die()` that does not.
#[test]
fn the_three_self_resolving_verbs_hear_the_reflog_diagnostics() {
    let f = Fx::three_commits("selfresolve");
    let reach = format!("{REACH} {OLDEST}\n");
    let old = format!("HEAD@{{{OLD}}}");
    let old = old.as_str();

    let out = f.run(&["diff-tree", "--name-only", old, "HEAD"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_text(&out), "f\n");
    assert_eq!(err(&out), reach);

    let out = f.run(&["update-ref", "refs/heads/tmp", old]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), reach);
    assert_eq!(f.rev("refs/heads/tmp"), f.rev("HEAD~2"));

    let out = f.run(&["merge-tree", old, "HEAD"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), reach);

    // The `die()` reaches them too, in place of each verb's own wording.
    const FATAL: &str = "fatal: log for 'HEAD' only has 3 entries\n";
    for args in [
        vec!["diff-tree", "--name-only", "HEAD@{99}", "HEAD"],
        vec!["update-ref", "refs/heads/tmp2", "HEAD@{99}"],
        vec!["merge-tree", "HEAD@{99}", "HEAD"],
    ] {
        let out = f.run(&args);
        assert_eq!(code(&out), 128, "{args:?}: {}", err(&out));
        assert_eq!(err(&out), FATAL, "{args:?}");
    }
}

/// `checkout` and `switch` resolve the operand **twice**:
/// `parse_branchname_arg()` through `repo_get_oid_mb()`
/// (`builtin/checkout.c:1476`), and then `setup_new_branch_info_and_source_tree()`
/// through `setup_branch_path()` (`builtin/checkout.c:804-806,1311`) — but only
/// when `repo_dwim_ref()` fails, which a `<ref>@{<n>}` always does because it is
/// not a ref name.
#[test]
fn checkout_and_switch_resolve_the_operand_twice() {
    let f = Fx::three_commits("checkouttwice");
    let old = format!("HEAD@{{{OLD}}}");
    let c1 = f.rev("HEAD~2");

    let out = f.run(&["checkout", &old]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(times(&out, REACH), 2, "{}", err(&out));
    assert_eq!(f.rev("HEAD"), c1);

    let g = Fx::three_commits("switchtwice");
    let out = g.run(&["switch", "--detach", &old]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(times(&out, REACH), 2, "{}", err(&out));
    assert_eq!(g.rev("HEAD"), c1);

    // A plain branch name resolves at `repo_dwim_ref()`, so the second resolution
    // never happens and nothing is said twice.
    let h = Fx::three_commits("switchonce");
    h.ok(&["branch", "side"]);
    let out = h.run(&["switch", "--detach", "side"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(times(&out, REACH), 0);
}

/// The second resolution is reached only when the first one answered
/// (`parse_branchname_arg()` returns at `builtin/checkout.c:1518` otherwise), so
/// an operand that fails is diagnosed once and then falls through to the pathspec
/// interpretation.
#[test]
fn checkout_diagnoses_a_failed_operand_once_and_then_treats_it_as_a_path() {
    let f = Fx::three_commits("checkoutonce");

    let out = f.run(&["checkout", "HEAD^{blob}"]);
    assert_eq!(code(&out), 1, "{}", err(&out));
    assert_eq!(
        err(&out),
        format!(
            "{}error: pathspec 'HEAD^{{blob}}' did not match any file(s) known to git\n",
            peel_error("HEAD^{blob}", "blob")
        )
    );

    // The `die()` ends it before the pathspec interpretation is reached at all.
    let g = Fx::three_commits("checkoutdie");
    let out = g.run(&["checkout", "HEAD@{99}"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), "fatal: log for 'HEAD' only has 3 entries\n");

    let out = g.run(&["checkout", "main@{u}"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), "fatal: no upstream configured for branch 'main'\n");
}
