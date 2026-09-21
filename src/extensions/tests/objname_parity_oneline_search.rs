//! `:/<text>` — `get_oid_with_context_1()`'s oneline search
//! (`object-name.c:1763-1775`) and the `get_oid_oneline()` walk behind it
//! (`object-name.c:1184-1238`).
//!
//! ```c
//! if (name[0] == ':') {
//!         …
//!         if (!only_to_die && namelen > 2 && name[1] == '/') {
//!                 struct handle_one_ref_cb cb;
//!                 struct commit_list *list = NULL;
//!                 cb.repo = repo;
//!                 cb.list = &list;
//!                 refs_for_each_ref(get_main_ref_store(repo), handle_one_ref, &cb);
//!                 refs_head_ref(get_main_ref_store(repo), handle_one_ref, &cb);
//!                 ret = get_oid_oneline(repo, name + 2, oid, list);
//!                 commit_list_free(list);
//!                 return ret;
//!         }
//! ```
//!
//! The form is not `<rev>^{/<text>}` with an implied `HEAD`. It seeds the walk
//! with **every ref** plus HEAD and pops from a `prio_queue` ordered by *commit
//! date*, so the answer need not be reachable from HEAD and is not what a
//! topological walk would reach first. A date tie goes to whichever tip the
//! priority queue saw first, and `commit_list_insert()`/`refs_head_ref()` between
//! them put HEAD at the front.
//!
//! Every expectation here was captured from stock git 2.55.0 against the same
//! fixture shape and is reproduced verbatim.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(repo: &Path, home: &Path, args: &[&str], date: &str) -> Output {
    Command::new(BIN)
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

/// The commit dates are the whole point of this fixture, so every step pins one.
const EPOCH: i64 = 1_700_000_000;

fn git(repo: &Path, home: &Path, at: i64, args: &[&str]) {
    let date = format!("{} +0000", EPOCH + at);
    let out = run(repo, home, args, &date);
    assert!(
        out.status.success(),
        "fixture step `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

fn rev(repo: &Path, home: &Path, spec: &str) -> Output {
    run(repo, home, &["rev-parse", spec], "0 +0000")
}

fn id(repo: &Path, home: &Path, spec: &str) -> String {
    let out = rev(repo, home, spec);
    assert!(
        out.status.success(),
        "`rev-parse {spec}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout_of(&out)
}

/// Six commits across four refs, with the dates chosen so that graph order and
/// date order disagree and one date is a tie:
///
/// ```text
///   t+300  main   delta          <- HEAD
///   t+300  tied   tied delta     <- same commit date as HEAD's tip
///   t+200  side   gamma needle
///   t+100  main~1 beta needle
///   t+50   bang   xray !bang
///   t+0    root   alpha
/// ```
///
/// `gamma needle` is *not* reachable from HEAD, so a HEAD-only walk cannot find
/// it; `tied delta` shares HEAD's commit date, so a queue without git's
/// insertion-order tie-break answers it instead of `delta`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("zvcs-objname-oneline-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, 0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "a\n").unwrap();
    git(&repo, &home, 0, &["add", "f"]);
    git(&repo, &home, 0, &["commit", "-q", "-m", "alpha"]);
    std::fs::write(repo.join("f"), "b\n").unwrap();
    git(&repo, &home, 100, &["commit", "-q", "-am", "beta needle"]);

    git(&repo, &home, 200, &["checkout", "-q", "-b", "side", "HEAD~1"]);
    std::fs::write(repo.join("g"), "g\n").unwrap();
    git(&repo, &home, 200, &["add", "g"]);
    git(&repo, &home, 200, &["commit", "-q", "-m", "gamma needle"]);

    git(&repo, &home, 300, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f"), "c\n").unwrap();
    git(&repo, &home, 300, &["commit", "-q", "-am", "delta"]);

    git(&repo, &home, 300, &["checkout", "-q", "-b", "tied", "main~2"]);
    std::fs::write(repo.join("t"), "t\n").unwrap();
    git(&repo, &home, 300, &["add", "t"]);
    git(&repo, &home, 300, &["commit", "-q", "-m", "tied delta"]);

    git(&repo, &home, 50, &["checkout", "-q", "-b", "bang", "main~2"]);
    std::fs::write(repo.join("x"), "x\n").unwrap();
    git(&repo, &home, 50, &["add", "x"]);
    git(&repo, &home, 50, &["commit", "-q", "-m", "xray !bang"]);

    git(&repo, &home, 300, &["checkout", "-q", "main"]);
    git(&repo, &home, 300, &["tag", "-a", "-m", "ann", "atag", "side"]);

    (repo, home)
}

/// The queue is ordered by commit date and seeded from every ref, so a pattern
/// several commits match is answered by the newest of them — and a commit no
/// branch but `side` carries is still a candidate.
#[test]
fn the_newest_matching_commit_wins_across_all_refs() {
    let (repo, home) = fixture("order");
    let delta = id(&repo, &home, "main");
    let gamma = id(&repo, &home, "side");

    // `.` matches every message, so this is purely a question of order.
    assert_eq!(id(&repo, &home, ":/."), delta);
    assert_eq!(id(&repo, &home, ":/e"), delta);

    // `needle` is on `beta needle` (t+100) and `gamma needle` (t+200). The newer
    // one is on `side`, which HEAD cannot reach — a walk rooted at HEAD would
    // answer `beta needle` and a topological walk would answer neither reliably.
    assert_eq!(id(&repo, &home, ":/needle"), gamma);

    // Reachability is not the rule, but neither is "any ref": a pattern only the
    // detached history carries still resolves to that commit.
    assert_eq!(id(&repo, &home, ":/tied"), id(&repo, &home, "tied"));
    assert_eq!(id(&repo, &home, ":/xray"), id(&repo, &home, "bang"));
}

/// `prio_queue`'s date tie-break: `compare()` falls back to the insertion
/// counter, and the list handed to the queue is HEAD first
/// (`refs_head_ref()` runs last and `commit_list_insert()` prepends).
///
/// `main` and `tied` have the *same* commit date and both match, so this is the
/// only thing that decides the answer.
#[test]
fn a_commit_date_tie_is_broken_in_favour_of_head() {
    let (repo, home) = fixture("tie");
    let head = id(&repo, &home, "HEAD");
    let tied = id(&repo, &home, "tied");
    assert_ne!(head, tied, "fixture assumes two distinct tips");
    assert_eq!(
        id(&repo, &home, "main^{/delta}"),
        head,
        "fixture assumes both tips' messages match the pattern"
    );

    assert_eq!(id(&repo, &home, ":/delta"), head);
    assert_eq!(id(&repo, &home, ":/."), head);
}

/// The leading-`!` decode is three cases, not two:
///
/// ```c
/// if (prefix[0] == '!') {
///         prefix++;
///         if (prefix[0] == '-') { prefix++; negative = 1; }
///         else if (prefix[0] != '!') return -1;
/// }
/// ```
#[test]
fn the_bang_prefix_has_three_cases() {
    let (repo, home) = fixture("bang");
    let delta = id(&repo, &home, "main");
    let gamma = id(&repo, &home, "side");
    let bang = id(&repo, &home, "bang");

    // `!-` negates: the newest commit whose message does *not* match.
    assert_eq!(id(&repo, &home, ":/!-needle"), delta);
    assert_eq!(id(&repo, &home, ":/!-delta"), gamma);

    // `!!` keeps the second `!` as part of the pattern, so this searches for the
    // literal text `!bang`.
    assert_eq!(id(&repo, &home, ":/!!bang"), bang);

    // A lone `!` is `return -1` — not a search for `xray`, although that commit
    // is there and `:/xray` finds it.
    assert_eq!(id(&repo, &home, ":/xray"), bang);
    let out = rev(&repo, &home, ":/!xray");
    assert!(!out.status.success(), "a lone `!` must not be dropped");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: ambiguous argument ':/!xray': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    );
}

/// `regexec(&regex, p + 2, …)` runs over the raw object from the first blank
/// line on, with `REG_EXTENDED` and without `REG_NEWLINE`.
#[test]
fn the_pattern_is_an_ere_over_the_message_body() {
    let (repo, home) = fixture("regex");

    // ERE, not a literal: the alternation and the character class both apply.
    assert_eq!(id(&repo, &home, ":/(gamma|nothing)"), id(&repo, &home, "side"));
    assert_eq!(id(&repo, &home, ":/^alpha"), id(&repo, &home, "main~2"));

    // The body is `alpha\n`, and without `REG_NEWLINE` a `$` anchors at the end
    // of the whole string — so this matches nothing at all.
    let out = rev(&repo, &home, ":/^alpha$");
    assert!(!out.status.success(), "`$` must not match before the trailing newline");

    // A pattern nothing matches is a plain failure, not a different message.
    let out = rev(&repo, &home, ":/nomatch");
    assert!(!out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: ambiguous argument ':/nomatch': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    );
}

/// The verbs that resolve an argv operand through `repo_get_oid()` all reach the
/// same search, so the answer cannot be a `rev-parse` speciality.
#[test]
fn every_verb_that_resolves_an_operand_gets_the_same_commit() {
    let (repo, home) = fixture("verbs");
    let delta = id(&repo, &home, "main");
    let gamma = id(&repo, &home, "side");
    // `gamma needle` forked from `main~2`, so that is where the two meet.
    let base = id(&repo, &home, "main~2");

    for (args, want) in [
        (vec!["show", "--oneline", "-s", "--no-patch", ":/!-needle"], &delta),
        (vec!["describe", "--always", ":/!-needle"], &delta),
        (vec!["merge-base", "HEAD", ":/needle"], &base),
        (vec!["rev-parse", ":/needle"], &gamma),
    ] {
        let out = run(&repo, &home, &args, "0 +0000");
        assert!(
            out.status.success(),
            "`git {args:?}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout_of(&out).contains(&want[..7]),
            "`git {args:?}` answered {:?}, wanted {want}",
            stdout_of(&out)
        );
    }
}

/// `namelen > 2` keeps a bare `:/` out of the search, and `check_filename()`
/// then answers it — along with `:!` and `:^` — without a stat:
///
/// ```c
/// if (skip_prefix(arg, ":/", &arg)) {
///         if (!*arg) /* ":/" is root dir, always exists */
///                 return 1;
///         prefix = NULL;
/// } else if (skip_prefix(arg, ":!", &arg) ||
///            skip_prefix(arg, ":^", &arg)) {
///         if (!*arg) /* excluding everything is silly, but allowed */
///                 return 1;
/// }
/// ```
///
/// So stock `git rev-parse :/` echoes the operand and exits 0, where an operand
/// that is neither a revision nor a path is fatal.
#[test]
fn bare_short_pathspec_magic_is_a_path_not_a_search() {
    let (repo, home) = fixture("magic");

    for spec in [":/", ":!", ":^"] {
        let out = rev(&repo, &home, spec);
        assert!(
            out.status.success(),
            "`rev-parse {spec}` must be a path: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(stdout_of(&out), spec);
        assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    }

    // The exemption is only for the *bare* magic; with a path after it the stat
    // decides, and a missing one is fatal.
    for spec in [":/nosuchpath", ":!nosuchpath", ":^nosuchpath"] {
        assert!(!rev(&repo, &home, spec).status.success(), "`rev-parse {spec}`");
    }
    // …and an existing one passes.
    for spec in [":/f", ":!f", ":^f"] {
        let out = rev(&repo, &home, spec);
        assert!(out.status.success(), "`rev-parse {spec}`");
        assert_eq!(stdout_of(&out), spec);
    }
}
