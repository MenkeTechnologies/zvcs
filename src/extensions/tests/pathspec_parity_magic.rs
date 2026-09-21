//! `parse_pathspec()` parity across the verbs that take a pathspec.
//!
//! git reads a pathspec the same way for every command: `parse_pathspec()`
//! (pathspec.c:637-668) scans all of argv for an empty element, then, per
//! element, parses the magic (`parse_element_magic()`, pathspec.c:430-442),
//! resolves the path (`prefix_path_gently()`, setup.c:119-147) and finally
//! measures the magic against the command's `magic_mask` (pathspec.c:657-658).
//! Every failure is a `die()`, so the answer is `fatal: <body>` and exit 128 for
//! *every* verb — before the command has done any work.
//!
//! Each verb used to answer from wherever it happened to meet the bad element,
//! which meant a different wording, a different exit code, or no answer at all,
//! per verb. These cases pin the shared answer; each expectation below is the
//! measured stock git 2.55.0 output for the same argv in the same fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", repo)
        .env("ZVCS_HOME", repo)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_LITERAL_PATHSPECS")
        .env_remove("GIT_GLOB_PATHSPECS")
        .env_remove("GIT_ICASE_PATHSPECS")
        .output()
        .unwrap()
}

fn ok(repo: &Path, args: &[&str]) {
    let out = run(repo, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn err_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// One commit holding `a.txt`, `dir/b.txt` and `UPPER.txt`, plus an untracked
/// file so the verbs that report a worktree have something to say.
fn fixture(name: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-psmagic-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(repo.join("dir")).unwrap();
    let repo = repo.canonicalize().unwrap();
    ok(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    std::fs::write(repo.join("UPPER.txt"), "U\n").unwrap();
    std::fs::write(repo.join("dir/b.txt"), "b\n").unwrap();
    ok(&repo, &["add", "-A"]);
    ok(&repo, &["-c", "user.name=t", "-c", "user.email=t@e", "commit", "-qm", "one"]);
    std::fs::write(repo.join("un.txt"), "u\n").unwrap();
    repo
}

/// The verbs whose `parse_pathspec()` call takes a `magic_mask` of 0 — which is
/// all of them but `ls-tree` and `check-ignore` — with an argv shaped so the
/// pathspec is the only operand under test.
const VERBS: &[&[&str]] = &[
    &["add", "-n"],
    &["status", "--porcelain"],
    &["commit", "--dry-run"],
    &["reset", "-q"],
    &["clean", "-n"],
    &["grep", "-n", "a"],
    &["archive", "--format=tar", "HEAD"],
    &["update-index", "--again"],
];

/// Every `die()` in `parse_element_magic()` and the two that bracket it, for
/// every verb that parses a pathspec with no mask of its own.
///
/// The wording is the contract — each of these used to surface gitoxide's own
/// text ("Found \"bogus\" in signature, which is not a valid keyword"), at exit
/// 1, or in `status`' case as an empty stderr with exit 128.
#[test]
fn every_parse_die_is_gits_text_at_128_for_every_verb() {
    let repo = fixture("dies");
    let cases: &[(&str, &str)] = &[
        (":(bogus)x", "fatal: Invalid pathspec magic 'bogus' in ':(bogus)x'\n"),
        (":(glob", "fatal: Missing ')' at the end of pathspec magic in ':(glob'\n"),
        (":%x", "fatal: Unimplemented pathspec magic '%' in ':%x'\n"),
        (":(literal,glob)x", "fatal: :(literal,glob)x: 'literal' and 'glob' are incompatible\n"),
        (":(attr:)x", "fatal: attr spec must not be empty\n"),
        (":(attr:a=b*c)x", "fatal: cannot use '*' for value matching\n"),
        // `strcspn_escaped(pos, ",)")` (pathspec.c:341) splits the long form on
        // commas only, so a *space* keeps `attr:a attr:b` as one keyword whose
        // body then splits into the attribute names `a` and `attr:b` — and `:`
        // is not in `[-A-Za-z0-9_.]` (attr.c:199-216). The comma form is the one
        // that reaches the "only one" die.
        (":(attr:a attr:b)x", "fatal: invalid attribute name attr:b\n"),
        (":(attr:a,attr:b)x", "fatal: Only one 'attr:' specification is allowed.\n"),
        (
            "",
            "fatal: empty string is not a valid pathspec. \
             please use . instead if you meant to match all paths\n",
        ),
    ];
    for verb in VERBS {
        for (spec, want) in cases {
            let mut args = verb.to_vec();
            args.push("--");
            args.push(spec);
            let out = run(&repo, &args);
            assert_eq!(err_of(&out), *want, "git {args:?}");
            assert_eq!(out.status.code(), Some(128), "git {args:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&repo);
}

/// `init_pathspec_item()`'s second `die()` (pathspec.c:500-501) names **two**
/// operands that are not the same string: `elt`, the element as typed, and
/// `copyfrom`, the element with only its magic removed. `:(icase)../x` is the
/// shape that separates them; a bare `..` cannot tell the two apart, which is
/// why it was the only one anybody checked.
#[test]
fn the_outside_repository_die_names_the_element_and_then_the_path() {
    let repo = fixture("outside");
    let root = repo.display().to_string();
    let cases: &[(&str, String)] = &[
        ("..", format!("fatal: ..: '..' is outside repository at '{root}'\n")),
        (
            ":(icase)../x",
            format!("fatal: :(icase)../x: '../x' is outside repository at '{root}'\n"),
        ),
    ];
    for verb in VERBS {
        // `update-index --again` matches literally against the index and so
        // never resolves the element against the work tree; it is covered by the
        // parse cases above.
        if verb[0] == "update-index" {
            continue;
        }
        for (spec, want) in cases {
            let mut args = verb.to_vec();
            args.push("--");
            args.push(spec);
            let out = run(&repo, &args);
            assert_eq!(err_of(&out), *want, "git {args:?}");
            assert_eq!(out.status.code(), Some(128), "git {args:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&repo);
}

/// `:(top)` and `:(prefix:<n>)` take `copyfrom` verbatim and never reach
/// `prefix_path_gently()` (pathspec.c:482-487), so neither can raise "is outside
/// repository" however far the path climbs.
///
/// The gate has to know that: it runs the same resolution `parse_pathspec()`
/// does, and applying it to a rooted element would answer `:(top)../x` with a
/// three-operand fatal git never prints.
///
/// Measured gap, deliberately not asserted here: stock exits 0 for
/// `:(top)../x` while this port exits 128 with an empty stderr, because the
/// matcher still normalises a rooted element's path where git keeps it
/// verbatim. That is below this gate, in `gix-pathspec`'s `Pattern::normalize`,
/// and the same divergence makes `:(top)./a.txt` match `a.txt` here and nothing
/// in stock. What *is* pinned is that no wording is invented for it.
#[test]
fn a_rooted_element_is_never_outside_the_repository() {
    let repo = fixture("rooted");
    for spec in [":(top)../x", ":/../x"] {
        let out = run(&repo, &["status", "--porcelain", "--", spec]);
        assert_eq!(err_of(&out), "", "git status -- {spec}");
    }
    // A rooted element that stays inside resolves and reports normally.
    std::fs::write(repo.join("a.txt"), "a\nz\n").unwrap();
    for spec in [":(top)a.txt", ":/a.txt"] {
        let out = run(&repo, &["status", "--porcelain", "--", spec]);
        assert_eq!(String::from_utf8_lossy(&out.stdout), " M a.txt\n", "git status -- {spec}");
        assert_eq!(out.status.code(), Some(0), "git status -- {spec}");
    }
    let _ = std::fs::remove_dir_all(&repo);
}

/// `unsupported_magic()` (pathspec.c:581-593) renders the bits through
/// `pathspec_magic_names()`, which walks `pathspec_magic[]` in *table* order
/// (top, literal, glob, icase, exclude, attr) and appends the mnemonic when the
/// table gives one. So `:(icase,glob)` comes back as `'glob', 'icase'` — the
/// order the user wrote is not the order git prints — and `attr:label` is named
/// by its keyword `attr`, not by the body the user attached to it.
///
/// `check-ignore` passes `PATHSPEC_ALL_MAGIC & ~PATHSPEC_FROMTOP`
/// (builtin/check-ignore.c:93), which makes it the one verb here where these
/// bits are reachable.
#[test]
fn unsupported_magic_is_named_in_table_order_with_mnemonics() {
    let repo = fixture("mask");
    let cases: &[(&str, &str)] = &[
        (
            ":(icase,glob)DIR/*.txt",
            "fatal: :(icase,glob)DIR/*.txt: pathspec magic not supported by this command: \
             'glob', 'icase'\n",
        ),
        (
            ":(attr:label)",
            "fatal: :(attr:label): pathspec magic not supported by this command: 'attr'\n",
        ),
        (
            ":!x",
            "fatal: :!x: pathspec magic not supported by this command: \
             'exclude' (mnemonic: '!')\n",
        ),
    ];
    for (spec, want) in cases {
        let out = run(&repo, &["check-ignore", "--", spec]);
        assert_eq!(err_of(&out), *want, "git check-ignore -- {spec}");
        assert_eq!(out.status.code(), Some(128), "git check-ignore -- {spec}");
    }

    // And the mask is consulted *after* the parse (pathspec.c:657-658), so an
    // element that fails to parse reports the parse failure even though the
    // keyword it named would also have been unsupported.
    let out = run(&repo, &["check-ignore", "--", ":(bogus)x"]);
    assert_eq!(err_of(&out), "fatal: Invalid pathspec magic 'bogus' in ':(bogus)x'\n");
    assert_eq!(out.status.code(), Some(128));
    // `:(top)` is the one keyword outside the mask, and it still works.
    let out = run(&repo, &["check-ignore", "--", ":(top)a.txt"]);
    assert_eq!(err_of(&out), "");
    assert_eq!(out.status.code(), Some(1), "a.txt is tracked and not ignored");
    let _ = std::fs::remove_dir_all(&repo);
}

/// `blame` never calls `parse_pathspec()` — `add_prefix()` is `prefix_path()`
/// (builtin/blame.c:709-712) — so its die has two operands, not three, and its
/// path goes through `normalize_path_copy_len()` (path.c:1121-1204) first. Both
/// halves were missing: `sub/../a.txt` was "no such path" where git blames
/// `a.txt`, and `..` reported the object lookup rather than the escape.
#[test]
fn blame_resolves_its_path_through_prefix_path() {
    let repo = fixture("blame");
    let root = repo.display().to_string();

    let out = run(&repo, &["blame", "-s", "--", ".."]);
    assert_eq!(err_of(&out), format!("fatal: '..' is outside repository at '{root}'\n"));
    assert_eq!(out.status.code(), Some(128));

    // `..` inside the path is folded, not rejected — and no magic is read, so a
    // `:(…)` prefix is part of the filename.
    let folded = run(&repo, &["blame", "-s", "--", "dir/../a.txt"]);
    let direct = run(&repo, &["blame", "-s", "--", "a.txt"]);
    assert_eq!(err_of(&folded), "");
    assert_eq!(folded.stdout, direct.stdout);

    let out = run(&repo, &["blame", "-s", "--", ":(bogus)x"]);
    assert_eq!(err_of(&out), "fatal: no such path ':(bogus)x' in HEAD\n");
    assert_eq!(out.status.code(), Some(128));

    // From a subdirectory the path is joined to the prefix before it is folded,
    // which is what lets a `..` that stays inside the work tree resolve.
    let sub = repo.join("dir");
    let out = run(&sub, &["blame", "-s", "--", "../a.txt"]);
    assert_eq!(err_of(&out), "");
    assert_eq!(out.status.code(), Some(0));
    let out = run(&sub, &["blame", "-s", "--", "../../x"]);
    assert_eq!(err_of(&out), format!("fatal: '../../x' is outside repository at '{root}'\n"));
    assert_eq!(out.status.code(), Some(128));

    let _ = std::fs::remove_dir_all(&repo);
}

/// The empty-element scan runs over the *whole* argument vector before any
/// element is parsed (pathspec.c:637-643), so it outranks a parse failure that
/// comes earlier in argv. This is the one ordering rule a per-element loop
/// cannot reproduce by accident.
#[test]
fn an_empty_element_outranks_a_bad_one_that_precedes_it() {
    let repo = fixture("order");
    let out = run(&repo, &["status", "--porcelain", "--", ":(bogus)x", ""]);
    assert_eq!(
        err_of(&out),
        "fatal: empty string is not a valid pathspec. \
         please use . instead if you meant to match all paths\n"
    );
    assert_eq!(out.status.code(), Some(128));
    let _ = std::fs::remove_dir_all(&repo);
}

/// `GIT_LITERAL_PATHSPECS` short-circuits `parse_element_magic()` entirely
/// (pathspec.c:434), so an element that would otherwise be a fatal is just a
/// filename nothing matches.
#[test]
fn literal_pathspecs_turns_every_refusal_off() {
    let repo = fixture("literal");
    let out = Command::new(BIN)
        .args(["status", "--porcelain", "--", ":(bogus)x"])
        .current_dir(&repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &repo)
        .env("ZVCS_HOME", &repo)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .output()
        .unwrap();
    assert_eq!(err_of(&out), "");
    assert_eq!(out.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&repo);
}

/// `mv` runs every operand through `prefix_path()` too
/// (`internal_prefix_pathspec()`, builtin/mv.c), so its die is the same
/// two-operand one `blame` raises — and the hint is the *absolute*,
/// symlink-resolved work tree, which at the top of a work tree is not the `.`
/// gix reports. `prefix_path()` also normalises `''` and `.` to the empty
/// string, which `lstat("")` then fails on, so both come back through
/// `bad source` (builtin/mv.c:306-323) rather than through the directory
/// branch below it.
#[test]
fn mv_resolves_its_operands_through_prefix_path() {
    let repo = fixture("mv");
    let root = repo.display().to_string();

    let out = run(&repo, &["mv", "..", "dst"]);
    assert_eq!(err_of(&out), format!("fatal: '..' is outside repository at '{root}'\n"));
    assert_eq!(out.status.code(), Some(128));

    for empty in ["", "."] {
        let out = run(&repo, &["mv", empty, "dst"]);
        assert_eq!(
            err_of(&out),
            "fatal: bad source, source=, destination=dst\n",
            "git mv '{empty}' dst"
        );
        assert_eq!(out.status.code(), Some(128), "git mv '{empty}' dst");
    }

    // A `..` that stays inside is folded, not refused — and the folded name is
    // what the rename is reported under, since `item->match` is the normalised
    // path.
    let out = run(&repo, &["mv", "-n", "dir/../a.txt", "dst"]);
    assert_eq!(err_of(&out), "");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Checking rename of 'a.txt' to 'dst'\nRenaming a.txt to dst\n"
    );
    assert_eq!(out.status.code(), Some(0));

    let _ = std::fs::remove_dir_all(&repo);
}
