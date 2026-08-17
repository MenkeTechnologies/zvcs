//! `setup_revisions()` as `cmd_blame()` reaches it — the revision machinery
//! behind `git blame` and `git annotate`.
//!
//! `cmd_blame()` parses its own options, cuts the path out of `argv`, and hands
//! everything left to `setup_revisions()`; `setup_scoreboard()` then picks the one
//! commit to dig from out of `revs->pending` with `find_single_final()` (or
//! `find_single_initial()` under `--reverse`). Almost everything this file pins
//! follows from those two sentences:
//!
//! * A **range** is not special-cased anywhere in blame. `A..B` queues `^A` and
//!   `B`, `find_single_final()` skips the `UNINTERESTING` one, and `assign_blame()`
//!   refuses to pass blame *from* a commit carrying that flag — so the range's
//!   bottom keeps the lines the range did not touch and prints with the boundary
//!   marker. `^A B`, `--not A B` and `A...B` are the same mechanism spelled
//!   differently.
//! * The **parent marks** `^@`, `^!` and `^-<n>` belong to
//!   `handle_revision_arg_1()`, not to the revision parser: `get_oid_1()` has no
//!   case for them, so an operand that still carries one after
//!   `add_parents_only()` declined it can only be `bad revision`.
//! * The **ref selectors** `--branches=<p>` and friends root their pattern under
//!   their own prefix and match the *untrimmed* refname, and a pattern with no
//!   `?`, `*` or `[` gains an implied `/` and `*` — so `--branches=main` selects
//!   nothing at all.
//! * A **value** git refuses is refused wherever the option stands, including in
//!   the trailing slot after `-- <path>` where `setup_revisions()` is the parser.
//!
//! Every expectation here was captured from stock git 2.55.0 in the same fixture
//! before being written down.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A well-formed SHA-1 object name no repository will ever contain — the control
/// for the ambiguity warning, which must stay silent for a hex that names no ref.
const MISSING: &str = "0123456789012345678901234567890123456789";

fn git(repo: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .env("HOME", home)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0200")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0200")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[track_caller]
fn must(repo: &Path, home: &Path, args: &[&str]) {
    let (_, stderr, code) = git(repo, home, args);
    assert_eq!(code, 0, "git {args:?} failed: {stderr}");
}

fn rev(repo: &Path, home: &Path, spec: &str) -> String {
    let (out, stderr, code) = git(repo, home, &["rev-parse", spec]);
    assert_eq!(code, 0, "rev-parse {spec}: {stderr}");
    out.trim().to_string()
}

/// The first field of each blame line — the id, with the boundary `^` still on it.
///
/// Comparing prefixes rather than whole ids keeps the assertions independent of
/// the abbreviation length, which `core.abbrev` and the object count both move.
fn blame_ids(repo: &Path, home: &Path, args: &[&str]) -> Vec<String> {
    let (out, stderr, code) = git(repo, home, args);
    assert_eq!(code, 0, "git {args:?} failed: {stderr}");
    out.lines()
        .map(|l| l.split_whitespace().next().unwrap_or_default().to_string())
        .collect()
}

/// Assert one blame's attribution against full commit ids, `^` meaning the
/// boundary marker `emit_other()` prints for an `UNINTERESTING` commit.
#[track_caller]
fn expect_blame(repo: &Path, home: &Path, args: &[&str], expected: &[(&str, bool)]) {
    let got = blame_ids(repo, home, args);
    assert_eq!(got.len(), expected.len(), "git {args:?} line count: {got:?}");
    for (n, (got, (full, boundary))) in got.iter().zip(expected).enumerate() {
        let (want_mark, id) = (*boundary, *full);
        let stripped = got.strip_prefix('^');
        assert_eq!(
            stripped.is_some(),
            want_mark,
            "git {args:?} line {} boundary marker: {got}",
            n + 1
        );
        let shown = stripped.unwrap_or(got);
        assert!(
            !shown.is_empty() && id.starts_with(shown),
            "git {args:?} line {}: {shown} is not a prefix of {id}",
            n + 1
        );
    }
}

#[track_caller]
fn expect_fatal(repo: &Path, home: &Path, args: &[&str], stderr_expected: &str, code_expected: i32) {
    let (stdout, stderr, code) = git(repo, home, args);
    assert_eq!(stderr, stderr_expected, "git {args:?} stderr");
    assert_eq!(code, code_expected, "git {args:?} exit (stderr was {stderr:?})");
    assert_eq!(stdout, "", "git {args:?} stdout");
}

fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-blamerev-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    (repo.canonicalize().unwrap(), home.canonicalize().unwrap())
}

/// Four commits, one line added or changed by each, so every line of the final
/// image belongs to a different commit and a range's bottom is visible:
///
/// | commit | `f.txt` |
/// |---|---|
/// | c1 | `l1` |
/// | c2 | `l1 l2` |
/// | c3 | `l1 l2 l3` |
/// | c4 | `l1 l2x l3` |
///
/// Plus `deleted.txt`, tracked in `HEAD` and absent from the working tree, and a
/// branch whose *name* is a full-length hex — the only shape `get_oid_basic()`'s
/// ambiguity warning fires for.
fn linear(tag: &str) -> (PathBuf, PathBuf, [String; 4], String) {
    let (repo, home) = scratch(tag);
    must(&repo, &home, &["init", "-q", "-b", "main"]);
    for body in ["l1\n", "l1\nl2\n", "l1\nl2\nl3\n", "l1\nl2x\nl3\n"] {
        std::fs::write(repo.join("f.txt"), body).unwrap();
        must(&repo, &home, &["add", "f.txt"]);
        must(&repo, &home, &["commit", "-qm", "c"]);
    }
    std::fs::write(repo.join("deleted.txt"), "gone\n").unwrap();
    must(&repo, &home, &["add", "deleted.txt"]);
    must(&repo, &home, &["commit", "-qm", "d"]);
    std::fs::remove_file(repo.join("deleted.txt")).unwrap();

    let ids = [
        rev(&repo, &home, "HEAD~4"),
        rev(&repo, &home, "HEAD~3"),
        rev(&repo, &home, "HEAD~2"),
        rev(&repo, &home, "HEAD~1"),
    ];
    // A ref named by 40 hex digits, pointing somewhere *else* so that resolving
    // the name and resolving the ref cannot be confused for one another.
    let hexname = rev(&repo, &home, "HEAD");
    must(&repo, &home, &["update-ref", &format!("refs/heads/{hexname}"), "HEAD~1"]);
    (repo, home, ids, hexname)
}

// ---------------------------------------------------------------------------
// ranges on the forward path
// ---------------------------------------------------------------------------

/// `git blame <a>..<b>` is a range like anywhere else: `b` is the commit dug
/// from, `a` and its ancestors are `UNINTERESTING`, and `assign_blame()` stops
/// there — so line 1, which no commit in the range touched, stays on `a` and
/// prints with the boundary marker instead of travelling back to the root.
#[test]
fn forward_range_stops_at_its_bottom() {
    let (repo, home, c, _) = linear("fwd-range");
    let [c1, c2, c3, c4] = [&c[0], &c[1], &c[2], &c[3]];

    // The control: no range at all, so line 1 reaches the root.
    let plain = [(c1.as_str(), true), (c4.as_str(), false), (c3.as_str(), false)];
    expect_blame(&repo, &home, &["blame", "--", "f.txt"], &plain);

    // Every spelling of "everything c2 does not reach", which `setup_revisions()`
    // reduces to the same two pending entries.
    let ranged = [(c2.as_str(), true), (c4.as_str(), false), (c3.as_str(), false)];
    for args in [
        vec!["blame", "HEAD~3..HEAD~1", "--", "f.txt"],
        vec!["blame", "^HEAD~3", "HEAD~1", "--", "f.txt"],
        vec!["blame", "HEAD~1", "^HEAD~3", "--", "f.txt"],
        // `A...B` with `A` an ancestor of `B`: the merge base *is* `A`, so the
        // one object carries `UNINTERESTING` for both pending entries and the
        // symmetric range behaves exactly like `A..B`.
        vec!["blame", "HEAD~3...HEAD~1", "--", "f.txt"],
        // Same range spelled with the path DWIM'd rather than after `--`.
        vec!["blame", "HEAD~3..HEAD~1", "f.txt"],
    ] {
        expect_blame(&repo, &home, &args, &ranged);
    }

    // `--not` flips the flag for *everything after it*, so both operands are
    // queued `UNINTERESTING`: no commit is dug from, the working-tree image
    // stands, and every line is a bottom.
    let head = rev(&repo, &home, "HEAD");
    let all_head = [(head.as_str(), true), (head.as_str(), true), (head.as_str(), true)];
    expect_blame(&repo, &home, &["blame", "--not", "HEAD~3", "HEAD~1", "--", "f.txt"], &[
        (c4.as_str(), true),
        (c4.as_str(), true),
        (c4.as_str(), true),
    ]);

    // An empty endpoint is `HEAD`, so `..HEAD` is `HEAD..HEAD` — the bottom is
    // the commit dug from and blame passes nowhere at all.
    expect_blame(&repo, &home, &["blame", "..HEAD", "--", "f.txt"], &all_head);
}

/// The memo in `~/.zvcs/cache` is keyed by `(commit, path, algorithm)` and says
/// nothing about a range's bottom, so a bottom-limited blame must not be allowed
/// into it: it would answer for the *unlimited* blame of the same commit, in this
/// repository and — the cache being keyed by commit id alone — in every other one
/// holding that commit.
///
/// Ordering is the whole test: the range runs first, so a shared entry would be
/// written before the plain blame ever ran.
#[test]
fn range_blame_does_not_poison_the_plain_blame_memo() {
    let (repo, home, c, _) = linear("range-memo");
    let [c1, c2, c3, c4] = [&c[0], &c[1], &c[2], &c[3]];

    expect_blame(
        &repo,
        &home,
        &["blame", "HEAD~3..HEAD~1", "--", "f.txt"],
        &[(c2.as_str(), true), (c4.as_str(), false), (c3.as_str(), false)],
    );
    expect_blame(
        &repo,
        &home,
        &["blame", "HEAD~1", "--", "f.txt"],
        &[(c1.as_str(), true), (c4.as_str(), false), (c3.as_str(), false)],
    );
    // And the other way round, so neither direction is merely lucky.
    let (repo, home, c, _) = linear("range-memo-2");
    let [c1, c2, c3, c4] = [&c[0], &c[1], &c[2], &c[3]];
    expect_blame(
        &repo,
        &home,
        &["blame", "HEAD~1", "--", "f.txt"],
        &[(c1.as_str(), true), (c4.as_str(), false), (c3.as_str(), false)],
    );
    expect_blame(
        &repo,
        &home,
        &["blame", "HEAD~3..HEAD~1", "--", "f.txt"],
        &[(c2.as_str(), true), (c4.as_str(), false), (c3.as_str(), false)],
    );
}

/// `--reverse` reads the same `revs->pending` from the other end
/// (`find_single_initial()`), which the forward-range work must not have moved.
#[test]
fn reverse_ranges_still_dig_up_from_the_negative_end() {
    let (repo, home, c, _) = linear("rev-range");
    let [_, _, c3, c4] = [&c[0], &c[1], &c[2], &c[3]];

    // `A..B` reversed: `A` is the initial commit, so the final image is *its*
    // two-line file, and each line is reported against the last commit in the
    // range that still had it.
    let expected = [(c4.as_str(), false), (c3.as_str(), false)];
    for args in [
        vec!["blame", "--reverse", "HEAD~3..HEAD~1", "--", "f.txt"],
        vec!["blame", "--reverse", "^HEAD~3", "HEAD~1", "--", "f.txt"],
    ] {
        expect_blame(&repo, &home, &args, &expected);
    }

    // `dwim_reverse_initial()`: one positive operand and nothing else means
    // `<it>..HEAD`, which must still fire now that the pending list is built by
    // the shared queueing code.
    let (_, _, code) = git(&repo, &home, &["blame", "--reverse", "HEAD~3", "--", "f.txt"]);
    assert_eq!(code, 0);
}

// ---------------------------------------------------------------------------
// non-commit operands
// ---------------------------------------------------------------------------

/// `find_single_final()` refuses a queued object that `deref_tag()` cannot turn
/// into a commit — and it does so by *name*, which is the operand as typed.
/// `setup_revisions()` queued it happily first, which is why this is not
/// `bad revision`.
#[test]
fn non_commit_operand_is_refused_by_name() {
    let (repo, home, _, _) = linear("noncommit");
    let tree = rev(&repo, &home, "HEAD^{tree}");
    let blob = rev(&repo, &home, "HEAD:f.txt");

    for name in ["HEAD:f.txt", "HEAD^{tree}", tree.as_str(), blob.as_str()] {
        expect_fatal(
            &repo,
            &home,
            &["blame", name, "--", "f.txt"],
            &format!("fatal: Non commit {name}?\n"),
            128,
        );
    }
    // `--reverse` reads the other end of the same list and spells it with a comma
    // that the forward wording does not have.
    expect_fatal(
        &repo,
        &home,
        &["blame", "--reverse", "^HEAD^{tree}", "--", "f.txt"],
        "fatal: Non commit HEAD^{tree}?\n",
        128,
    );
}

// ---------------------------------------------------------------------------
// the parent marks
// ---------------------------------------------------------------------------

/// `^@` hands the operand to `add_parents_only()` and *returns* when that
/// succeeds, so only the parents are queued; `^!` queues them
/// `UNINTERESTING` and then queues the commit itself, which is the range
/// `<a>^..<a>`.
#[test]
fn parent_marks_queue_parents() {
    let (repo, home, c, _) = linear("marks");
    let [c1, c2, c3, c4] = [&c[0], &c[1], &c[2], &c[3]];

    // `HEAD~1^@` is `HEAD~2`, dug from as an ordinary single commit.
    expect_blame(
        &repo,
        &home,
        &["blame", "HEAD~1^@", "--", "f.txt"],
        &[(c1.as_str(), true), (c2.as_str(), false), (c3.as_str(), false)],
    );
    // `HEAD~1^!` is `HEAD~2..HEAD~1`: line 2 is the commit's own change, the rest
    // stay on the bottom.
    expect_blame(
        &repo,
        &home,
        &["blame", "HEAD~1^!", "--", "f.txt"],
        &[(c3.as_str(), true), (c4.as_str(), false), (c3.as_str(), true)],
    );
    // `^-<n>` with no digits is parent 1, so on a non-merge it is `^!`. The tail
    // is `strtol_i()`, not a digit run: it skips leading whitespace and takes a
    // sign, so `^-+1` and `^- 1` are parent 1 too.
    for spelling in ["HEAD~1^-", "HEAD~1^-1", "HEAD~1^-01", "HEAD~1^-+1", "HEAD~1^- 1"] {
        expect_blame(
            &repo,
            &home,
            &["blame", spelling, "--", "f.txt"],
            &[(c3.as_str(), true), (c4.as_str(), false), (c3.as_str(), true)],
        );
    }
}

/// The marks are `handle_revision_arg_1()`'s grammar and nothing else's:
/// `get_oid_1()` has no case for them, so the moment `add_parents_only()`
/// declines — a non-commit, a parent number past the end, a zero — the operand is
/// resolved with the mark still attached and can only fail.
///
/// `strstr()` finding the *first* `^!` and `!mark[2]` demanding it be last is why
/// a doubled mark is not a mark at all, and `handle_dotdot()` running first is why
/// a marked range endpoint never becomes a range.
#[test]
fn undigestible_marks_are_bad_revisions() {
    let (repo, home, _, _) = linear("bad-marks");
    for spelling in [
        // `deref_tag()` gives a tree, so `add_parents_only()` returns 0.
        "HEAD^{tree}^!",
        // Parent 2 of a single-parent commit.
        "HEAD^-2",
        // `exclude_parent < 1`, refused before `add_parents_only()` is called.
        "HEAD^-0",
        "HEAD^--1",
        // `strtol_i()` failing, which is refused in the same breath: a tail that
        // is not a number, and one that does not fit an `int`.
        "HEAD^-x",
        "HEAD^-2147483648",
        "HEAD^-99999999999999999999",
        // First `^!` is not the last two characters.
        "HEAD^!^!",
        // `handle_dotdot()` claims the token, and `get_oid("HEAD^!")` fails.
        "HEAD^!..HEAD",
        "HEAD..HEAD^!",
    ] {
        expect_fatal(
            &repo,
            &home,
            &["blame", spelling, "--", "f.txt"],
            &format!("fatal: bad revision '{spelling}'\n"),
            128,
        );
    }
}

// ---------------------------------------------------------------------------
// the ref selectors
// ---------------------------------------------------------------------------

/// `refs_for_each_ref_ext()` composes the pattern under the selector's prefix and
/// matches it against the **untrimmed** refname, and appends `/` `*` when the
/// pattern holds none of `?`, `*`, `[`. So `--branches=main` is
/// `refs/heads/main/*` and selects nothing, leaving `revs->pending` empty — which
/// forward blame answers with the working-tree image and `--reverse` refuses.
#[test]
fn ref_selector_patterns_are_rooted_and_gain_an_implied_star() {
    let (repo, home, c, _) = linear("selectors");
    let [c1, c3, c4] = [&c[0], &c[2], &c[3]];
    let plain = [(c1.as_str(), true), (c4.as_str(), false), (c3.as_str(), false)];

    // No glob characters: a directory prefix, so nothing matches.
    for selector in ["--branches=main", "--glob=refs/heads/main", "--glob=main"] {
        expect_blame(&repo, &home, &["blame", selector, "--", "f.txt"], &plain);
        expect_fatal(
            &repo,
            &home,
            &["blame", "--reverse", selector, "--", "f.txt"],
            "fatal: No commit to dig up from?\n",
            128,
        );
    }

    // With a glob character the pattern is used as written, so `--branches=*`
    // selects both branches — which `find_single_final()` refuses, proving the
    // selector matched rather than quietly selecting nothing.
    let hexname = rev(&repo, &home, "refs/heads/main");
    expect_fatal(
        &repo,
        &home,
        &["blame", "--branches=*", "--", "f.txt"],
        &format!("fatal: More than one commit to dig from main and {hexname}?\n"),
        128,
    );
}

/// `--all` queues every ref, so the second one reaches `find_single_final()` with
/// a commit already held — and the message names the one just reached *first*.
#[test]
fn walk_tip_options_are_honoured_in_the_trailing_slot() {
    let (repo, home, _, hexname) = linear("trailing-tips");

    // `refs_for_each_ref()` is refname-ordered, so the 40-hex branch is seen
    // before `main`, and `die()` names the one just reached first. `--all` has no
    // prefix to trim, so it quotes whole refnames where `--branches` quotes the
    // trimmed ones — the same entries under two different names.
    for (selector, first, second) in [
        ("--all", "refs/heads/main".to_string(), format!("refs/heads/{hexname}")),
        ("--branches", "main".to_string(), hexname.clone()),
    ] {
        let expected = format!("fatal: More than one commit to dig from {first} and {second}?\n");
        expect_fatal(&repo, &home, &["blame", selector, "--", "f.txt"], &expected, 128);
        expect_fatal(&repo, &home, &["blame", "--", "f.txt", selector], &expected, 128);
    }

    // `--merge` and `--follow` are refusals raised at the *end* of
    // `setup_revisions()`, and reachable from either slot.
    for args in [
        vec!["blame", "--", "f.txt", "--merge"],
        vec!["blame", "--merge", "--", "f.txt"],
    ] {
        expect_fatal(
            &repo,
            &home,
            &args,
            "fatal: --merge requires one of the pseudorefs MERGE_HEAD, \
             CHERRY_PICK_HEAD, REVERT_HEAD or REBASE_HEAD\n",
            128,
        );
    }
    // `--follow` is the one that is *not* symmetric: `parse_revision_opt()`
    // consumes an occurrence in front of the `--` and the line right after
    // `cmd_blame()`'s parse loop clears `follow_renames` again
    // (`builtin/blame.c:1035`), so only the trailing slot survives to
    // `diff_setup_done()`.
    expect_fatal(
        &repo,
        &home,
        &["blame", "--", "f.txt", "--follow"],
        "fatal: --follow requires exactly one pathspec\n",
        128,
    );
    let (_, stderr, code) = git(&repo, &home, &["blame", "--follow", "--", "f.txt"]);
    assert_eq!(code, 0, "git blame --follow -- f.txt: {stderr}");

    // `--ancestry-path` with no bottom commit dies inside
    // `prepare_revision_walk()`, later than anything `setup_revisions()` says.
    expect_fatal(
        &repo,
        &home,
        &["blame", "--", "f.txt", "--ancestry-path"],
        "fatal: --ancestry-path given but there are no bottom commits\n",
        128,
    );
}

// ---------------------------------------------------------------------------
// value parsers
// ---------------------------------------------------------------------------

/// A value git *refuses* is refused wherever the option stands. The trailing slot
/// after `-- <path>` is `setup_revisions()`'s, and reaches exactly the same
/// parsers — with exactly the same exit statuses, which differ per parser:
/// `die()` is 128 and parse-options' `error()` is 129.
#[test]
fn trailing_slot_values_reach_their_parsers() {
    let (repo, home, _, _) = linear("values");
    for (opt, message, code) in [
        ("--max-count=", "fatal: '': not an integer\n", 128),
        ("--max-count=abc", "fatal: 'abc': not an integer\n", 128),
        ("--skip=x", "fatal: 'x': not an integer\n", 128),
        (
            "--diff-algorithm=",
            "error: option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"\n",
            129,
        ),
        (
            "--diff-algorithm=bogus",
            "error: option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"\n",
            129,
        ),
        ("--find-renames=zz", "error: invalid argument to find-renames\n", 129),
        ("--find-copies=zz", "error: invalid argument to find-copies\n", 129),
        ("--break-rewrites=x", "error: break-rewrites expects <n>/<m> form\n", 129),
        ("--stat-width=x", "error: stat-width expects a numerical value\n", 129),
        ("--unified=x", "error: --unified expects a numerical value\n", 129),
        // `diff_opt_unified()` runs two tests, and only a *number* reaches the
        // second one.
        ("--unified=-1", "error: --unified expects a non-negative integer\n", 129),
        (
            "--inter-hunk-context=",
            "error: option `inter-hunk-context' expects a numerical value\n",
            129,
        ),
        (
            "--inter-hunk-context=-1",
            "error: option `inter-hunk-context' expects a non-negative integer value \
             with an optional k/m/g suffix\n",
            129,
        ),
        (
            "--inter-hunk-context=4294967296",
            "error: value 4294967296 for option `inter-hunk-context' not in range [0,4294967295]\n",
            129,
        ),
    ] {
        expect_fatal(&repo, &home, &["blame", "--", "f.txt", opt], message, code);
        expect_fatal(&repo, &home, &["blame", opt, "--", "f.txt"], message, code);
    }

    // The controls: values these parsers accept, in the same slot. `strtoul` takes
    // a sign and overflows silently, and `git_parse_unsigned()` reads base 0 — so
    // `0x10` is sixteen where the base-10 `--max-count=0x10` is not a number.
    for ok in [
        "--max-count=-1",
        "--max-count=+3",
        "--diff-algorithm=histogram",
        "--find-renames=50%",
        "--break-rewrites=20/60",
        "--stat-width=-1",
        "--stat-width=99999999999999999999",
        "--unified=",
        "--unified=99999999999999999999",
        "--inter-hunk-context=0x10",
        "--inter-hunk-context=3k",
    ] {
        let (_, stderr, code) = git(&repo, &home, &["blame", "--", "f.txt", ok]);
        assert_eq!(code, 0, "git blame -- f.txt {ok}: {stderr}");
    }
}

/// `-B<score>` in either slot is `diff_opt_break_rewrites()`, whose complaint is
/// parse-options' `error()` — 129, and no usage block.
#[test]
fn short_option_scores_reach_their_parsers() {
    let (repo, home, _, _) = linear("short-scores");
    for bad in ["-BB", "-Bx/y", "-B1,2"] {
        for args in [
            vec!["blame", bad, "--", "f.txt"],
            vec!["blame", "--", "f.txt", bad],
            vec!["blame", bad, "f.txt"],
        ] {
            expect_fatal(
                &repo,
                &home,
                &args,
                "error: break-rewrites expects <n>/<m> form\n",
                129,
            );
        }
    }
    for ok in ["-B", "-B50", "-B50%", "-B/", "-B20/60", "-B1.5/2.5"] {
        let (_, stderr, code) = git(&repo, &home, &["blame", ok, "--", "f.txt"]);
        assert_eq!(code, 0, "git blame {ok} -- f.txt: {stderr}");
    }
    // `-U` shares `diff_opt_unified()`'s two messages.
    expect_fatal(
        &repo,
        &home,
        &["blame", "--", "f.txt", "-U-1"],
        "error: --unified expects a non-negative integer\n",
        129,
    );
    expect_fatal(
        &repo,
        &home,
        &["blame", "--", "f.txt", "-Ux"],
        "error: --unified expects a numerical value\n",
        129,
    );
}

// ---------------------------------------------------------------------------
// the working-tree image
// ---------------------------------------------------------------------------

/// `fake_working_tree_commit()`'s `lstat()`, which git runs before it reads
/// anything: a path tracked in `HEAD` but gone from the working tree is fatal on
/// the overlay path, not a silent blame of `HEAD`.
///
/// The trailing option is the shape that first showed this: it makes the operand
/// list non-empty without adding a revision, so the overlay is still chosen.
#[test]
fn deleted_worktree_file_cannot_be_lstatted() {
    let (repo, home, _, _) = linear("lstat");
    let message = "fatal: Cannot lstat 'deleted.txt': No such file or directory\n";
    for args in [
        vec!["blame", "deleted.txt"],
        vec!["blame", "--", "deleted.txt"],
        vec!["annotate", "deleted.txt", "--diff-algorithm=histogram"],
        vec!["annotate", "--", "deleted.txt", "--diff-algorithm=histogram"],
    ] {
        expect_fatal(&repo, &home, &args, message, 128);
    }

    // With a positive revision there is no overlay, so `lstat()` never runs and
    // the blame is of the commit's blob.
    let (_, stderr, code) = git(&repo, &home, &["blame", "HEAD", "--", "deleted.txt"]);
    assert_eq!(code, 0, "git blame HEAD -- deleted.txt: {stderr}");
}

// ---------------------------------------------------------------------------
// the ambiguity warning
// ---------------------------------------------------------------------------

/// `get_oid_basic()` warns once per resolution, and the marks decide how many
/// resolutions one operand gets: `add_parents_only()` opens with its own
/// `repo_get_oid_committish()`, and `^!` then puts the truncated name back and
/// falls through to `handle_revision_arg_1()`'s own call.
///
/// Over-warning is the failure this guards: the controls are a 40-hex that names
/// no ref, a shorter name, and the ref selectors, none of which may say anything.
#[test]
fn ambiguity_warning_follows_the_resolution_count() {
    let (repo, home, _, hexname) = linear("ambiguity");
    let warns = |args: &[&str]| -> usize {
        let (_, stderr, _) = git(&repo, &home, args);
        stderr.matches("is ambiguous").count()
    };

    assert_eq!(warns(&["blame", &hexname, "--", "f.txt"]), 1);
    assert_eq!(warns(&["blame", &format!("^{hexname}"), "HEAD", "--", "f.txt"]), 1);
    // `^!` resolves the base twice: once inside `add_parents_only()` and once
    // after it hands the truncated name back.
    assert_eq!(warns(&["blame", &format!("{hexname}^!"), "--", "f.txt"]), 2);
    // `^@` succeeds and returns, so `handle_revision_arg_1()` never resolves again.
    assert_eq!(warns(&["blame", &format!("{hexname}^@"), "--", "f.txt"]), 1);

    // Controls — nothing here may warn.
    for args in [
        vec!["blame", "--", "f.txt"],
        vec!["blame", "HEAD", "--", "f.txt"],
        vec!["blame", "main", "--", "f.txt"],
        // A full-length hex that names no ref: the warning is about the *ref*.
        vec!["blame", MISSING, "--", "f.txt"],
        vec!["blame", MISSING, "f.txt"],
        // `handle_one_ref()` is handed an id, so no selector resolves a name.
        vec!["blame", "--branches", "--", "f.txt"],
        vec!["blame", "--all", "--", "f.txt"],
        vec!["blame", "--", "f.txt", "--all"],
    ] {
        assert_eq!(warns(&args), 0, "git {args:?} must not warn");
    }
}
