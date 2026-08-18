//! `git merge -s <name>` and the `-X ignore-*` whitespace family.
//!
//! Two divergences this pins down, both measured against stock git 2.55.0:
//!
//! 1. `-s recursive` and `-s subtree` were refused outright. git 2.55 has no
//!    separate `recursive` back-end left — `try_merge_strategy()` sends
//!    `recursive`, `subtree` and `ort` into the same `merge_ort_recursive()`
//!    call (builtin/merge.c:800-834) — so the refusal was a divergence with no
//!    engine gap behind it. `subtree` differs only by seeding
//!    `o.subtree_shift = ""` before the `-X` loop (builtin/merge.c:815-816) and
//!    by carrying `NO_FAST_FORWARD` (builtin/merge.c:107), which is what makes
//!    it record a merge commit where `ort` fast-forwards.
//!
//! 2. `-Xignore-all-space` / `-Xignore-space-change` / `-Xignore-space-at-eol` /
//!    `-Xignore-cr-at-eol` were refused with `fatal: strategy option … is
//!    unsupported`. They are now `xdl_recmatch()`'s rules
//!    (xdiff/xutils.c:173-250) expressed as canonical line images; see
//!    `zvcs::merge_ws`.
//!
//! The whitespace cases below are a *separation* matrix, not four repetitions of
//! one case: each fixture changes exactly one kind of whitespace, so the four
//! rules disagree about it in a way that only the real `xdl_recmatch()`
//! precedence reproduces. An implementation that merely trimmed both sides, or
//! that treated the four flags as one, fails at least one row.
//!
//! | fixture | side's change | `-w` | `-b` | `--ignore-space-at-eol` | `--ignore-cr-at-eol` |
//! |---|---|---|---|---|---|
//! | `LEADING` | `two` → `  two` | clean | conflict | conflict | conflict |
//! | `TRAILING` | `two   ` → `two` | clean | clean | clean | conflict |
//! | `RUN_LENGTH` | `a  b` → `a\tb` | clean | clean | conflict | conflict |
//! | `CRLF` | `two` → `two\r` | clean | clean | clean | clean |
//!
//! Every expectation — exit code, the merged file byte for byte, the conflict
//! block, the strategy name in `Merge made by the '…' strategy.` — was read off
//! `/opt/homebrew/bin/git` 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("the child exited normally")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn read(dir: &Path, path: &str) -> String {
    std::fs::read_to_string(dir.join(path)).unwrap()
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "zvcs-mergestrat-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// A repo whose `side` branch rewrites `f.txt` one way and whose `main` branch
/// rewrites it another, over a shared base — so the two changes overlap and the
/// whitespace rule decides whether the merge is clean.
///
/// `core.autocrlf`/`core.eol` are pinned because a `\r` in a fixture is the
/// point of the CRLF case, and an inherited `autocrlf` would rewrite it away.
fn diverged(tag: &str, base: &str, side: &str, main: &str) -> PathBuf {
    let repo = temp_root(tag);
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    git(&repo, &["config", "core.eol", "lf"]);
    git(&repo, &["config", "rerere.enabled", "false"]);

    std::fs::write(repo.join("f.txt"), base).unwrap();
    git(&repo, &["add", "f.txt"]);
    git(&repo, &["commit", "-q", "-m", "base"]);

    git(&repo, &["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.join("f.txt"), side).unwrap();
    git(&repo, &["commit", "-q", "-am", "side"]);

    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f.txt"), main).unwrap();
    git(&repo, &["commit", "-q", "-am", "main"]);
    repo
}

/// `side` indents line 2; `main` rewrites it. Only `-w` can see past a leading
/// whitespace run that was not there before — `xdl_recmatch()`'s `-b` branch
/// skips whitespace only when *both* sides are sitting on it
/// (xdiff/xutils.c:206-212), so a run against no run is still a difference.
const LEADING: (&str, &str, &str) = (
    "one\ntwo\nthree\nfour\nfive\n",
    "one\n  two\nthree\nfour\nfive-side\n",
    "one\ntwo-changed\nthree\nfour\nfive\n",
);

/// `side` strips trailing blanks; `main` rewrites the line. Everything but
/// `--ignore-cr-at-eol` absorbs this, because that flag `return`s before
/// `xdl_recmatch()`'s trailing-whitespace tail (xdiff/xutils.c:228).
const TRAILING: (&str, &str, &str) = (
    "one\ntwo   \nthree\n",
    "one\ntwo\nthree\n",
    "one\ntwo-changed\nthree\n",
);

/// `side` retypes two spaces as a tab; `main` rewrites the line. `-b` collapses
/// runs so it absorbs this, `--ignore-space-at-eol` only trims the end and so
/// does not.
const RUN_LENGTH: (&str, &str, &str) = (
    "one\na  b\nthree\n",
    "one\na\tb\nthree\n",
    "one\na  b-changed\nthree\n",
);

/// `side` gives line 2 CRLF endings; `main` rewrites it. `\r` is whitespace to
/// `XDL_ISSPACE()`, so all four rules absorb it.
const CRLF: (&str, &str, &str) = (
    "one\ntwo\nthree\n",
    "one\ntwo\r\nthree\n",
    "one\ntwo-changed\nthree\n",
);

/// One row of the separation matrix: merge `side` with `flag` and check the exit
/// code and the resulting `f.txt` against what stock produced.
fn check(tag: &str, fixture: (&str, &str, &str), flag: &str, want_code: i32, want_file: &str) {
    let (base, side, main) = fixture;
    let repo = diverged(tag, base, side, main);
    let out = run(&repo, &["merge", flag, "side"]);
    assert_eq!(
        code(&out),
        want_code,
        "git merge {flag} side [{tag}]: {}{}",
        stdout(&out),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(read(&repo, "f.txt"), want_file, "merged f.txt after {flag} [{tag}]");
}

#[test]
fn ignore_all_space_is_the_only_rule_that_absorbs_a_new_leading_indent() {
    check(
        "lead-w",
        LEADING,
        "-Xignore-all-space",
        0,
        "one\ntwo-changed\nthree\nfour\nfive-side\n",
    );
    let conflicted = "one\n<<<<<<< HEAD\ntwo-changed\n=======\n  two\n>>>>>>> side\nthree\nfour\nfive-side\n";
    for flag in ["-Xignore-space-change", "-Xignore-space-at-eol", "-Xignore-cr-at-eol"] {
        check("lead-x", LEADING, flag, 1, conflicted);
    }
}

#[test]
fn trailing_blanks_are_absorbed_by_everything_except_ignore_cr_at_eol() {
    for flag in ["-Xignore-all-space", "-Xignore-space-change", "-Xignore-space-at-eol"] {
        check("trail-ok", TRAILING, flag, 0, "one\ntwo-changed\nthree\n");
    }
    check(
        "trail-cr",
        TRAILING,
        "-Xignore-cr-at-eol",
        1,
        "one\n<<<<<<< HEAD\ntwo-changed\n=======\ntwo\n>>>>>>> side\nthree\n",
    );
}

#[test]
fn a_retyped_whitespace_run_needs_ignore_space_change_or_stronger() {
    for flag in ["-Xignore-all-space", "-Xignore-space-change"] {
        check("run-ok", RUN_LENGTH, flag, 0, "one\na  b-changed\nthree\n");
    }
    let conflicted = "one\n<<<<<<< HEAD\na  b-changed\n=======\na\tb\n>>>>>>> side\nthree\n";
    for flag in ["-Xignore-space-at-eol", "-Xignore-cr-at-eol"] {
        check("run-x", RUN_LENGTH, flag, 1, conflicted);
    }
}

#[test]
fn every_rule_absorbs_a_bare_cr_before_the_newline() {
    for flag in [
        "-Xignore-all-space",
        "-Xignore-space-change",
        "-Xignore-space-at-eol",
        "-Xignore-cr-at-eol",
    ] {
        check("crlf", CRLF, flag, 0, "one\ntwo-changed\nthree\n");
    }
}

/// `ends_with_optional_cr()` will not ignore a CR at the end of an *incomplete*
/// line (xdiff/xutils.c:167-169, and the comment saying exactly that), so a file
/// whose last line has no newline still differs from the same line with a CR
/// appended. `git merge-tree` shares the rule, and used to report this merge
/// clean because its canonical form popped a bare trailing CR as well.
#[test]
fn a_cr_on_an_unterminated_last_line_is_not_ignorable() {
    let repo = diverged("cr-incomplete", "one\nabc", "one\nabc\r", "one\nabc-changed");

    let merged = run(&repo, &["merge-tree", "main", "side"]);
    let plain = code(&merged);
    let ignored = run(&repo, &["merge-tree", "-Xignore-cr-at-eol", "main", "side"]);
    assert_eq!(
        code(&ignored),
        plain,
        "-Xignore-cr-at-eol must not resolve an unterminated CR: {}{}",
        stdout(&ignored),
        String::from_utf8_lossy(&ignored.stderr)
    );
    assert_eq!(plain, 1, "the fixture has to conflict without the flag");

    // `git merge` reaches the same rule through the same function.
    let out = run(&repo, &["merge", "-Xignore-cr-at-eol", "side"]);
    assert_eq!(code(&out), 1, "{}{}", stdout(&out), String::from_utf8_lossy(&out.stderr));
}

/// Combining flags must not change which rule wins: `xdl_recmatch()` tests them
/// in a fixed order (xdiff/xutils.c:193-222), so `-w` still decides when a
/// weaker flag is also set. Without that precedence, `LEADING` would conflict.
#[test]
fn the_strongest_whitespace_rule_wins_when_several_are_given() {
    let (base, side, main) = LEADING;
    let repo = diverged("precedence", base, side, main);
    let out = run(
        &repo,
        &["merge", "-Xignore-cr-at-eol", "-Xignore-all-space", "side"],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(read(&repo, "f.txt"), "one\ntwo-changed\nthree\nfour\nfive-side\n");
}

/// A `-X` git itself rejects still has to reproduce git's own wording — the new
/// whitespace branches must not have widened what is accepted.
#[test]
fn an_unknown_strategy_option_is_still_gits_own_refusal() {
    let (base, side, main) = LEADING;
    let repo = diverged("unknown-x", base, side, main);
    let out = run(&repo, &["merge", "-Xignore-space", "side"]);
    assert_eq!(code(&out), 128);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: unknown strategy option: -Xignore-space\n"
    );
}

/// `-s recursive` runs, and reports itself as `recursive`: git echoes
/// `wt_strategy`, which is the name as spelled (builtin/merge.c:1794), not the
/// engine that ran.
#[test]
fn recursive_runs_the_ort_engine_and_keeps_its_own_name() {
    let (base, side, main) = LEADING;

    let ort = diverged("name-ort", base, side, main);
    let a = run(&ort, &["merge", "-s", "ort", "-Xignore-all-space", "side"]);
    assert_eq!(code(&a), 0, "{}", String::from_utf8_lossy(&a.stderr));
    assert!(
        stdout(&a).contains("Merge made by the 'ort' strategy."),
        "{}",
        stdout(&a)
    );

    let rec = diverged("name-rec", base, side, main);
    let b = run(&rec, &["merge", "-s", "recursive", "-Xignore-all-space", "side"]);
    assert_eq!(code(&b), 0, "{}", String::from_utf8_lossy(&b.stderr));
    assert!(
        stdout(&b).contains("Merge made by the 'recursive' strategy."),
        "{}",
        stdout(&b)
    );

    // Same engine, so the trees must be identical bit for bit.
    assert_eq!(
        git(&ort, &["rev-parse", "HEAD^{tree}"]),
        git(&rec, &["rev-parse", "HEAD^{tree}"])
    );
}

/// `-s subtree` carries `NO_FAST_FORWARD` (builtin/merge.c:107), so it records a
/// merge commit over a history `ort` would fast-forward. That attribute is
/// applied *after* the `--squash` conflict checks (builtin/merge.c:1503 vs
/// :1608), which is why `--squash -s subtree` is accepted where
/// `--squash --no-ff` dies — and why it takes the strategy path rather than the
/// fast-forward one.
#[test]
fn subtree_never_fast_forwards() {
    let ff = |tag: &str| {
        let repo = temp_root(tag);
        git(&repo, &["init", "-q", "-b", "main", "."]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "T"]);
        std::fs::write(repo.join("f.txt"), "one\ntwo\n").unwrap();
        git(&repo, &["add", "f.txt"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        git(&repo, &["checkout", "-q", "-b", "side"]);
        std::fs::write(repo.join("f.txt"), "one\ntwo\nthree\n").unwrap();
        git(&repo, &["commit", "-q", "-am", "side"]);
        git(&repo, &["checkout", "-q", "main"]);
        repo
    };

    let plain = ff("ff-ort");
    let a = run(&plain, &["merge", "-s", "ort", "side"]);
    assert_eq!(code(&a), 0, "{}", String::from_utf8_lossy(&a.stderr));
    assert!(stdout(&a).contains("Fast-forward"), "{}", stdout(&a));
    // A fast-forward moves HEAD onto `side`: one parent.
    assert_eq!(git(&plain, &["rev-list", "--parents", "-1", "HEAD"]).split_whitespace().count(), 2);

    let sub = ff("ff-subtree");
    let b = run(&sub, &["merge", "-s", "subtree", "side"]);
    assert_eq!(code(&b), 0, "{}", String::from_utf8_lossy(&b.stderr));
    assert!(
        stdout(&b).contains("Merge made by the 'subtree' strategy."),
        "{}",
        stdout(&b)
    );
    // A merge commit: the commit plus two parents.
    assert_eq!(git(&sub, &["rev-list", "--parents", "-1", "HEAD"]).split_whitespace().count(), 3);
    // The result is still `side`'s content — `NO_FAST_FORWARD` changes the shape
    // of the history, not the tree.
    assert_eq!(read(&sub, "f.txt"), "one\ntwo\nthree\n");

    // `--squash` reaches the strategy path too, and does not die the way
    // `--squash --no-ff` does.
    let squashed = ff("ff-subtree-squash");
    let c = run(&squashed, &["merge", "--squash", "-s", "subtree", "side"]);
    assert_eq!(code(&c), 0, "{}", String::from_utf8_lossy(&c.stderr));
    assert_eq!(stdout(&c), "Squash commit -- not updating HEAD\n");
    assert_eq!(
        String::from_utf8_lossy(&c.stderr),
        "Automatic merge went well; stopped before committing as requested\n"
    );
    // Not the fast-forward squash, which announces `Updating <a>..<b>` and
    // `Fast-forward` on stdout instead.
    assert!(!stdout(&c).contains("Fast-forward"), "{}", stdout(&c));
}

/// `-s subtree` seeds the automatic shift, so an imported project that moved
/// under a prefix on one side still lines up. The shift is a seed, not an
/// override: an explicit `-Xsubtree=<prefix>` afterwards replaces it
/// (builtin/merge.c:815-822).
#[test]
fn subtree_shifts_an_imported_project_under_its_prefix() {
    let build = |tag: &str| {
        let repo = temp_root(tag);
        git(&repo, &["init", "-q", "-b", "main", "."]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "T"]);
        std::fs::create_dir_all(repo.join("x")).unwrap();
        std::fs::write(repo.join("x/lib.txt"), "lib one\n").unwrap();
        std::fs::write(repo.join("readme.txt"), "readme one\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "import"]);

        git(&repo, &["checkout", "-q", "-b", "side"]);
        std::fs::write(repo.join("x/lib.txt"), "lib two\n").unwrap();
        git(&repo, &["commit", "-q", "-am", "side advances"]);

        git(&repo, &["checkout", "-q", "main"]);
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        git(&repo, &["mv", "x", "sub/x"]);
        git(&repo, &["mv", "readme.txt", "sub/readme.txt"]);
        std::fs::write(repo.join("top.txt"), "top\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "reroot"]);
        repo
    };

    for (tag, args) in [
        ("sub-auto", vec!["merge", "-s", "subtree", "side"]),
        ("sub-explicit", vec!["merge", "-s", "subtree", "-Xsubtree=sub", "side"]),
        ("sub-viaX", vec!["merge", "-Xsubtree=sub", "side"]),
    ] {
        let repo = build(tag);
        let out = run(&repo, &args);
        assert_eq!(code(&out), 0, "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        // `side`'s advance landed under the prefix, and `main`'s own file survived.
        assert_eq!(read(&repo, "sub/x/lib.txt"), "lib two\n", "{args:?}");
        assert_eq!(read(&repo, "top.txt"), "top\n", "{args:?}");
    }
}

/// An outright unknown name still gets git's `Could not find merge strategy`
/// block. Widening `-s` must not have widened it to everything.
///
/// `resolve` used to be refused here alongside it; it now runs
/// `git-merge-resolve`'s chain, so the second half asserts what stock 2.55.0
/// does with this fixture instead — see `merge_resolve_and_trivial_in_index`
/// for the full walk.
#[test]
fn unknown_strategies_are_still_rejected() {
    let (base, side, main) = LEADING;

    let repo = diverged("bogus", base, side, main);
    let out = run(&repo, &["merge", "-s", "nosuchstrategy", "side"]);
    assert_eq!(code(&out), 128);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Could not find merge strategy 'nosuchstrategy'.\n\
         Available strategies are: octopus ours recursive resolve subtree.\n"
    );

    let repo = diverged("resolve", base, side, main);
    let out = run(&repo, &["merge", "--no-edit", "-s", "resolve", "side"]);
    // The trivial pre-pass declines (both sides touched `f.txt`), then the
    // back-end conflicts: measured against stock 2.55.0.
    assert_eq!(code(&out), 1, "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        stdout(&out),
        "Trying really trivial in-index merge...\n\
         Nope.\n\
         Trying simple merge.\n\
         Simple merge failed, trying Automatic merge.\n\
         Auto-merging f.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    assert!(repo.join(".git/MERGE_HEAD").exists());
    assert!(read(&repo, "f.txt").contains("<<<<<<< .merge_file_"));
}
