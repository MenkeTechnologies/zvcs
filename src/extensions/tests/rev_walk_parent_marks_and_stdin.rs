//! The revision *walk*'s own grammar: the `^!`/`^@`/`^-<n>` parent marks that
//! `handle_revision_arg_1()` reads, `try_parent_shorthands()`'s separate reading
//! of the same three in `rev-parse`, and the position at which `--stdin` is read.
//!
//! These three share one property that makes them worth a suite of their own:
//! each has a failure mode that exits 0 with the *wrong commit set*, which no
//! exit-status check can see.
//!
//! * `<rev>^-<n>` is `<rev> ^<rev>^<n>` (`revision.c:2192-2206`). Before this was
//!   wired through, `git rev-list <merge>^-` was `fatal: ambiguous argument` — a
//!   loud failure — but `git rev-parse <rev>^!` resolved through gitoxide's
//!   `Spec::ExcludeParents` to the single commit and printed **one** line where
//!   stock prints the commit and one `^<parent>` per parent, at exit 0.
//!
//! * `try_parent_shorthands()` (`builtin/rev-parse.c:328-390`) is not the same
//!   code as the walk's block and does not agree with it everywhere: it parses
//!   `<n>` with `strtoul` rather than `strtol_i`, it ignores `--verify`, and it
//!   names the parents `<base>^<n>` only when `--symbolic` is on. It also runs
//!   *after* `try_difference()`, so `main..side^!` is refused rather than read as
//!   a range — gitoxide reads it as one and answered two object ids at exit 0.
//!
//! * `read_revisions_from_stdin()` is called from inside `setup_revisions()`'s
//!   argument loop (`revision.c:3058`), so `--stdin` is positional. It also keeps
//!   its **own** `int flags = 0`, so an argv `--not` in front of `--stdin` does
//!   not reach the lines and a `--not` among them does not escape. Reading stdin
//!   after the loop instead made `printf dup | git rev-list --stdin --not tri`
//!   print *nothing* at exit 0 where stock prints three commits.
//!
//! Every expectation below was measured from stock git 2.55.0 against the same
//! fixture as the test builds.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run_stdin(repo: &Path, home: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "zvcs test")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "zvcs test")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(text) = stdin {
        child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    run_stdin(repo, home, args, None)
}

fn git(repo: &Path, home: &Path, args: &[&str]) {
    let out = run(repo, home, args);
    assert!(out.status.success(), "git {args:?} failed: {}", err_of(&out));
}

fn err_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn out_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn oid(repo: &Path, home: &Path, spec: &str) -> String {
    let out = run(repo, home, &["rev-parse", spec]);
    assert!(out.status.success(), "rev-parse {spec}: {}", err_of(&out));
    out_of(&out).trim_end().to_string()
}

const AMBIGUOUS_TAIL: &str = "unknown revision or path not in the working tree.\n\
     Use '--' to separate paths from revisions, like this:\n\
     'git <command> [<revision>...] -- [<file>...]'\n";

fn ambiguous(spec: &str) -> String {
    format!("fatal: ambiguous argument '{spec}': {AMBIGUOUS_TAIL}")
}

/// A two-parent merge, a commit after it, an annotated tag on the merge and an
/// ambiguous `dup` (branch *and* tag). The merge is what gives `^-2` a second
/// parent to select and `^@` two lines to print; `dup` is what makes the base's
/// own `repo_get_oid_committish()` warn.
///
/// ```text
/// base ── m1 ──┐
///   └── s1 ────┴── merge ── m2   (main, HEAD)
/// ```
fn merge_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-revwalk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    for (name, body) in [("base", "base\n"), ("m1", "m1\n")] {
        std::fs::write(repo.join(format!("{name}.txt")), body).unwrap();
        git(&repo, &home, &["add", "."]);
        git(&repo, &home, &["commit", "-q", "-m", name]);
    }
    git(&repo, &home, &["checkout", "-q", "-b", "side", "HEAD~1"]);
    std::fs::write(repo.join("s1.txt"), "s1\n").unwrap();
    git(&repo, &home, &["add", "."]);
    git(&repo, &home, &["commit", "-q", "-m", "s1"]);
    git(&repo, &home, &["checkout", "-q", "main"]);
    git(&repo, &home, &["merge", "-q", "--no-ff", "-m", "merge", "side"]);
    std::fs::write(repo.join("m2.txt"), "m2\n").unwrap();
    git(&repo, &home, &["add", "."]);
    git(&repo, &home, &["commit", "-q", "-m", "m2"]);
    git(&repo, &home, &["tag", "-a", "-m", "annot", "atag", "HEAD~1"]);
    git(&repo, &home, &["branch", "dup"]);
    git(&repo, &home, &["tag", "dup", "HEAD~1"]);
    (repo, home)
}

/// `<rev>^-<n>` seeds the walk with the commit and *one* excluded parent, so it
/// is the whole side branch and not the whole history — the wrong-set failure
/// mode, checked as a commit list rather than an exit status.
#[test]
fn caret_dash_selects_one_parent_to_exclude() {
    let (repo, home) = merge_fixture("dash");
    let merge = oid(&repo, &home, "main~1");
    let m1 = oid(&repo, &home, "main~1^1");
    let s1 = oid(&repo, &home, "main~1^2");
    let head = oid(&repo, &home, "HEAD");

    // `<merge>^-` excludes the *first* parent, so the second parent's side is
    // what is left; `^-2` excludes the second and leaves the first.
    for (spec, want) in [
        ("main~1^-", vec![merge.clone(), s1.clone()]),
        ("main~1^-1", vec![merge.clone(), s1.clone()]),
        ("main~1^-2", vec![merge.clone(), m1.clone()]),
        // Empty tail, `+`, and a leading blank are all `strtol_i`'s parent 1.
        ("main^-", vec![head.clone()]),
        ("main^-+1", vec![head.clone()]),
        ("main^- 1", vec![head.clone()]),
    ] {
        let out = run(&repo, &home, &["rev-list", spec]);
        assert_eq!(out.status.code(), Some(0), "`rev-list {spec}`: {}", err_of(&out));
        assert_eq!(
            out_of(&out).lines().collect::<Vec<_>>(),
            want,
            "`rev-list {spec}`: wrong commit set"
        );
    }

    // An annotated tag peels before the parents are read, so `atag^-` walks the
    // merge's history just as `main~1^-` does.
    assert_eq!(
        out_of(&run(&repo, &home, &["rev-list", "atag^-"])),
        format!("{merge}\n{s1}\n")
    );
}

/// `cmd_shortlog()` runs the same `setup_revisions()`, so the marks are its
/// grammar too — and it keeps a *separate* pending list, which is exactly the
/// shape that goes missing when one verb is wired up and its neighbours are not.
#[test]
fn shortlog_reads_the_same_parent_marks() {
    let (repo, home) = merge_fixture("shortlog");

    // `<merge>^-` is the merge plus the second parent's side: `merge` and `s1`.
    let out = run(&repo, &home, &["shortlog", "main~1^-"]);
    assert_eq!(out.status.code(), Some(0), "{}", err_of(&out));
    assert_eq!(out_of(&out), "zvcs test (2):\n      s1\n      merge\n\n");

    // `^-2` swaps which parent is excluded, so the first-parent side is left.
    let out = run(&repo, &home, &["shortlog", "main~1^-2"]);
    assert_eq!(out.status.code(), Some(0), "{}", err_of(&out));
    assert_eq!(out_of(&out), "zvcs test (2):\n      m1\n      merge\n\n");

    // `^!` is the commit alone; `^@` is the parents alone.
    let out = run(&repo, &home, &["shortlog", "main~1^!"]);
    assert_eq!(out.status.code(), Some(0), "{}", err_of(&out));
    assert_eq!(out_of(&out), "zvcs test (1):\n      merge\n\n");

    let out = run(&repo, &home, &["shortlog", "main~1^-3"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(err_of(&out), ambiguous("main~1^-3"));
}

/// The `<n>` git refuses. Each of these leaves `add_parents_only()` unreached or
/// answering 0, and the operand then fails as an ordinary unresolvable name —
/// so the *shape* of the refusal is what distinguishes a correct port from one
/// that invented its own message.
#[test]
fn caret_dash_rejects_out_of_range_and_non_numeric() {
    let (repo, home) = merge_fixture("dashbad");
    // `main~1` has two parents, `HEAD` has one.
    for spec in ["main~1^-3", "HEAD^-2", "HEAD^-0", "HEAD^-abc", "main^--1", "HEAD^-1x"] {
        let out = run(&repo, &home, &["rev-list", spec]);
        assert_eq!(out.status.code(), Some(128), "`rev-list {spec}` must fail");
        assert_eq!(err_of(&out), ambiguous(spec), "`rev-list {spec}`: wrong diagnostic");
        assert_eq!(out_of(&out), "", "`rev-list {spec}` writes no commit");
    }
}

/// `rev-parse`'s reading of the same three marks is `try_parent_shorthands()`,
/// which prints a *list* — the arm most easily replaced by a single-object
/// resolution that exits 0 with one line.
#[test]
fn rev_parse_parent_shorthands_print_every_parent() {
    let (repo, home) = merge_fixture("shorthand");
    let merge = oid(&repo, &home, "main~1");
    let m1 = oid(&repo, &home, "main~1^1");
    let s1 = oid(&repo, &home, "main~1^2");
    let atag = oid(&repo, &home, "atag");

    for (args, want) in [
        // `^!` shows the rev then every parent, reversed.
        (vec!["rev-parse", "main~1^!"], format!("{merge}\n^{m1}\n^{s1}\n")),
        // `^@` shows only the parents, and *not* reversed.
        (vec!["rev-parse", "main~1^@"], format!("{m1}\n{s1}\n")),
        // `^-<n>` shows the rev and the one selected parent.
        (vec!["rev-parse", "main~1^-2"], format!("{merge}\n^{s1}\n")),
        // `show_rev(NORMAL, &oid, arg)` prints what
        // `repo_get_oid_committish()` produced, so an annotated tag leads with
        // the tag object while the parents come from the commit it peels to.
        (vec!["rev-parse", "atag^!"], format!("{atag}\n^{m1}\n^{s1}\n")),
        // `if (symbolic) name = xstrfmt("%s^%d", arg, parent_number);`
        (
            vec!["rev-parse", "--symbolic", "main~1^!"],
            "main~1\n^main~1^1\n^main~1^2\n".to_string(),
        ),
        (vec!["rev-parse", "--symbolic", "main~1^@"], "main~1^1\nmain~1^2\n".to_string()),
        // `--symbolic-full-name` resolves each name through `repo_dwim_ref()`,
        // and `main~1^<n>` is not a ref — so every line is dropped.
        (vec!["rev-parse", "--symbolic-full-name", "main~1^@"], String::new()),
        // `--abbrev-ref` alone leaves the parent's `name` NULL, and `show_rev()`
        // then falls through to the object id rather than printing nothing.
        (vec!["rev-parse", "--abbrev-ref", "main~1^!"], format!("^{m1}\n^{s1}\n")),
    ] {
        let out = run(&repo, &home, &args);
        assert_eq!(out.status.code(), Some(0), "`git {}`: {}", args.join(" "), err_of(&out));
        assert_eq!(out_of(&out), want, "`git {}`: wrong output", args.join(" "));
    }
}

/// `try_parent_shorthands()` never consults `verify`, so `--verify <rev>^!`
/// prints all three lines and *then* fails with `revs_count` still zero. A port
/// that routed the mark through the ordinary single-revision path would exit 0
/// with one line instead.
#[test]
fn rev_parse_verify_still_prints_the_whole_shorthand() {
    let (repo, home) = merge_fixture("verify");
    let merge = oid(&repo, &home, "main~1");
    let m1 = oid(&repo, &home, "main~1^1");
    let s1 = oid(&repo, &home, "main~1^2");

    let out = run(&repo, &home, &["rev-parse", "--verify", "main~1^!"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(out_of(&out), format!("{merge}\n^{m1}\n^{s1}\n"));
    assert_eq!(err_of(&out), "fatal: Needed a single revision\n");
}

/// `try_parent_shorthands()` runs *after* `try_difference()` and resolves the
/// text in front of the mark as one name, so a range that carries a mark is
/// neither a range nor a shorthand. gitoxide's grammar accepts both spellings and
/// answered a two-line range at exit 0.
#[test]
fn marked_ranges_and_marked_excludes_are_not_revisions() {
    let (repo, home) = merge_fixture("marked");
    for spec in ["main..side^!", "main^!..side", "main..side^@", "^main^!", "main^!^!"] {
        let out = run(&repo, &home, &["rev-parse", spec]);
        assert_eq!(out.status.code(), Some(128), "`rev-parse {spec}` must fail");
        assert_eq!(err_of(&out), ambiguous(spec), "`rev-parse {spec}`: wrong diagnostic");
        // `as_is` echoes the operand on stdout before the fatal.
        assert_eq!(out_of(&out), format!("{spec}\n"));
    }
}

/// `add_parents_only()`'s first act is `repo_get_oid_committish(arg)`, so the
/// *base* is what earns the ambiguity warning — once, and naming the base rather
/// than the operand.
#[test]
fn parent_shorthand_warns_about_the_base_name() {
    let (repo, home) = merge_fixture("warn");
    let merge = oid(&repo, &home, "main~1");

    let out = run(&repo, &home, &["rev-parse", "dup^!"]);
    assert_eq!(out.status.code(), Some(0), "{}", err_of(&out));
    assert_eq!(err_of(&out), "warning: refname 'dup' is ambiguous.\n");
    assert!(
        out_of(&out).starts_with(&format!("{merge}\n^")),
        "`rev-parse dup^!` resolved the branch, not the tag: {}",
        out_of(&out)
    );
}

/// `--stdin` is read at its argv position and with its own `flags`, which is
/// three separate observations: the lines are seeded before a later `--not`, an
/// earlier `--not` does not reach them, and a `--not` among them does not escape.
#[test]
fn stdin_is_positional_and_keeps_its_own_not_state() {
    let (repo, home) = merge_fixture("stdin-pos");
    let merge = oid(&repo, &home, "main~1");
    let s1 = oid(&repo, &home, "main~1^2");
    let base = oid(&repo, &home, "main~3");

    // `--stdin` first: `main~1` is seeded interesting, then `--not side` excludes
    // the side branch, leaving the merge and the first-parent side.
    let out = run_stdin(&repo, &home, &["rev-list", "--stdin", "--not", "side"], Some("main~1\n"));
    assert_eq!(out.status.code(), Some(0), "{}", err_of(&out));
    let seen: Vec<String> = out_of(&out).lines().map(str::to_string).collect();
    assert!(seen.contains(&merge), "the merge must be listed: {seen:?}");
    assert!(!seen.contains(&s1), "`--not side` must exclude s1: {seen:?}");

    // The reader's `int flags = 0` is its own, so this argv `--not` does *not*
    // make the stdin line uninteresting — the walk still starts at the merge.
    let out = run_stdin(&repo, &home, &["rev-list", "--not", "--stdin"], Some("main~1\n"));
    assert_eq!(out.status.code(), Some(0), "{}", err_of(&out));
    assert!(
        out_of(&out).lines().next() == Some(merge.as_str()),
        "an argv --not must not reach the stdin lines: {:?}",
        out_of(&out)
    );

    // …and a `--not` read from stdin does not escape back to argv: `main~3` is
    // excluded, `main~1` on argv is not.
    let out = run_stdin(&repo, &home, &["rev-list", "--stdin", "main~1"], Some("--not\nmain~3\n"));
    assert_eq!(out.status.code(), Some(0), "{}", err_of(&out));
    let seen: Vec<String> = out_of(&out).lines().map(str::to_string).collect();
    assert!(seen.contains(&merge), "argv operand must survive: {seen:?}");
    assert!(!seen.contains(&base), "the stdin --not must exclude base: {seen:?}");
}

/// The reader's four refusals and its one silent stop, none of which the
/// ordinary argv scan produces.
#[test]
fn stdin_reader_has_its_own_grammar() {
    let (repo, home) = merge_fixture("stdin-grammar");

    // `if (!sb.len) break;` — an empty line ends the read, so `main` never
    // arrives: two commits rather than the five both branches reach together.
    let with_gap =
        run_stdin(&repo, &home, &["rev-list", "--count", "--stdin"], Some("side\n\nmain\n"));
    let without =
        run_stdin(&repo, &home, &["rev-list", "--count", "--stdin"], Some("side\nmain\n"));
    assert_eq!(with_gap.status.code(), Some(0), "{}", err_of(&with_gap));
    assert_eq!(out_of(&with_gap), "2\n", "an empty stdin line must stop the read");
    assert_eq!(out_of(&without), "5\n", "…and without it both lines are read");

    for (args, stdin, want_err) in [
        (
            vec!["rev-list", "--stdin", "--stdin"],
            "main\n",
            "fatal: --stdin given twice?\n".to_string(),
        ),
        (
            vec!["rev-list", "--stdin"],
            "--oneline\n",
            "fatal: invalid option '--oneline' in --stdin mode\n".to_string(),
        ),
        // `REVARG_CANNOT_BE_FILENAME`: a stdin line can never become a pathspec,
        // so the short `bad revision` replaces the "ambiguous argument" block.
        (
            vec!["rev-list", "--stdin"],
            "nosuchref\n",
            "fatal: bad revision 'nosuchref'\n".to_string(),
        ),
        // A real file name is no different — it is still not a revision.
        (
            vec!["rev-list", "--stdin"],
            "base.txt\n",
            "fatal: bad revision 'base.txt'\n".to_string(),
        ),
    ] {
        let out = run_stdin(&repo, &home, &args, Some(stdin));
        assert_eq!(out.status.code(), Some(128), "`git {}` must fail", args.join(" "));
        assert_eq!(err_of(&out), want_err, "`git {}`: wrong diagnostic", args.join(" "));
    }

    // `--end-of-options` switches the option test off for the rest of the block,
    // so a line that looks like a flag is read as a revision — and fails as one.
    let out = run_stdin(&repo, &home, &["rev-list", "--stdin"], Some("--end-of-options\n--oneline\n"));
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(err_of(&out), "fatal: bad revision '--oneline'\n");

    // A pseudo-option *is* accepted, and lands at its position in the block.
    let all = run_stdin(&repo, &home, &["rev-list", "--count", "--stdin"], Some("--all\n"));
    let argv_all = run(&repo, &home, &["rev-list", "--count", "--all"]);
    assert_eq!(all.status.code(), Some(0), "{}", err_of(&all));
    assert_eq!(out_of(&all), out_of(&argv_all), "`--all` on stdin must seed the same set");
}

/// `revarg_opt |= REVARG_CANNOT_BE_FILENAME` when the *argument vector* holds a
/// `--`, which changes the diagnostic for every operand in front of it.
#[test]
fn a_separator_anywhere_shortens_the_bad_revision_message() {
    let (repo, home) = merge_fixture("dashdash");

    let out = run(&repo, &home, &["rev-list", "nosuchrev", "--", "base.txt"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(err_of(&out), "fatal: bad revision 'nosuchrev'\n");

    // Without the separator the same operand may still be a path, so the long
    // block is what git prints.
    let out = run(&repo, &home, &["rev-list", "nosuchrev"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(err_of(&out), ambiguous("nosuchrev"));
}

/// A branch with a remote-tracking ref and a sibling with none, without ever
/// running a network operation: `update-ref` plants the tracking ref and the
/// refspecs are configuration.
fn push_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-revwalk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    // Three commits so the tracking ref itself has a parent, which is what makes
    // `<mark>~1` a resolvable name rather than a second failure.
    for body in ["one\n", "two\n", "three\n"] {
        std::fs::write(repo.join("f.txt"), body).unwrap();
        git(&repo, &home, &["add", "f.txt"]);
        git(&repo, &home, &["commit", "-q", "-m", body.trim()]);
    }
    git(&repo, &home, &["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
    git(&repo, &home, &["config", "remote.origin.url", "./up.git"]);
    git(&repo, &home, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
    git(&repo, &home, &["config", "branch.main.remote", "origin"]);
    git(&repo, &home, &["config", "branch.main.merge", "refs/heads/main"]);
    git(&repo, &home, &["branch", "nou"]);
    (repo, home)
}

/// `at_mark()` compares with `strncasecmp`, so the three marks are
/// case-insensitive — and `push_mark()`'s `suffix_len <= len` lets anything
/// follow.
#[test]
fn at_marks_are_case_insensitive() {
    let (repo, home) = push_fixture("case");
    let tracking = oid(&repo, &home, "refs/remotes/origin/main");

    for spec in ["main@{push}", "main@{PUSH}", "main@{Push}", "main@{u}", "main@{U}", "main@{UpStReAm}"] {
        let out = run(&repo, &home, &["rev-parse", spec]);
        assert_eq!(out.status.code(), Some(0), "`rev-parse {spec}`: {}", err_of(&out));
        assert_eq!(out_of(&out), format!("{tracking}\n"), "`rev-parse {spec}`");
    }
    // `suffix_len <= len` rather than `==`, so a suffix may follow the mark and
    // applies to what the mark resolved to.
    let parent = oid(&repo, &home, "refs/remotes/origin/main~1");
    let out = run(&repo, &home, &["rev-parse", "main@{PUSH}~1"]);
    assert_eq!(out.status.code(), Some(0), "{}", err_of(&out));
    assert_eq!(out_of(&out), format!("{parent}\n"));
}

/// `branch_get_push_1()`'s `push.default` machinery dies with five distinct
/// messages, and `interpret_branch_mark()` raises them from *inside* `get_oid()`
/// — before the command has a failed operand to report, so they replace the
/// "ambiguous argument" block rather than preceding it.
#[test]
fn push_mark_reports_its_own_failures() {
    let (repo, home) = push_fixture("pushfail");

    for (config, spec, want) in [
        (vec![], "nou@{push}", "fatal: no upstream configured for branch 'nou'\n"),
        (vec![], "nosuch@{push}", "fatal: no such branch: 'nosuch'\n"),
        (
            vec![("push.default", "nothing")],
            "main@{push}",
            "fatal: push has no destination (push.default is 'nothing')\n",
        ),
        (
            vec![("branch.main.pushRemote", "nosuchremote")],
            "main@{push}",
            "fatal: push destination 'refs/heads/main' on remote 'nosuchremote' \
             has no local tracking branch\n",
        ),
        (
            vec![("remote.origin.push", "refs/heads/zzz:refs/heads/zzz")],
            "main@{push}",
            "fatal: push refspecs for 'origin' do not include 'main'\n",
        ),
        // `simple` demands that the push destination and the upstream agree.
        (
            vec![("branch.main.merge", "refs/heads/other")],
            "main@{push}",
            "fatal: cannot resolve 'simple' push to a single destination\n",
        ),
    ] {
        for (key, value) in &config {
            git(&repo, &home, &["config", key, value]);
        }
        let out = run(&repo, &home, &["rev-parse", spec]);
        assert_eq!(out.status.code(), Some(128), "`rev-parse {spec}` with {config:?} must fail");
        assert_eq!(err_of(&out), want, "`rev-parse {spec}` with {config:?}");
        assert_eq!(out_of(&out), "", "a die inside get_oid() writes nothing");
        for (key, _) in &config {
            git(&repo, &home, &["config", "--unset", key]);
        }
        if config.iter().any(|(k, _)| *k == "branch.main.merge") {
            git(&repo, &home, &["config", "branch.main.merge", "refs/heads/main"]);
        }
    }
}

/// `push.default=current` maps the branch's own refname through the *fetch*
/// refspecs, so a branch with no upstream still names a tracking ref — one git
/// does not die about, and which then fails as an ordinary unknown revision.
/// Answering "no push destination" for every failure would get this wrong.
#[test]
fn push_mark_has_outcomes_git_does_not_die_on() {
    let (repo, home) = push_fixture("pushsoft");
    git(&repo, &home, &["config", "push.default", "current"]);

    let out = run(&repo, &home, &["rev-parse", "nou@{push}"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(err_of(&out), ambiguous("nou@{push}"));

    // `main` does have `refs/remotes/origin/main`, so the same setting resolves.
    let out = run(&repo, &home, &["rev-parse", "main@{push}"]);
    assert_eq!(out.status.code(), Some(0), "{}", err_of(&out));
    assert_eq!(out_of(&out), format!("{}\n", oid(&repo, &home, "refs/remotes/origin/main")));
}
