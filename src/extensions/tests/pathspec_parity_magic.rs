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
/// The matcher has to know it too: it used to normalise a rooted element's path
/// where git keeps it verbatim, so the `..` that git simply fails to match
/// became `OutsideOfWorktree` and surfaced as exit 128 with an empty stderr.
/// See [`a_rooted_elements_path_is_taken_verbatim`] for the matching half.
#[test]
fn a_rooted_element_is_never_outside_the_repository() {
    let repo = fixture("rooted");
    for spec in [":(top)../x", ":/../x"] {
        let out = run(&repo, &["status", "--porcelain", "--", spec]);
        assert_eq!(err_of(&out), "", "git status -- {spec}");
        assert_eq!(out.status.code(), Some(0), "git status -- {spec}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "",
            "a path that climbs out names no entry: git status -- {spec}"
        );
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

/// `ls-tree` measures its mask *after* `init_pathspec_item()` has parsed the
/// element and resolved its path, not before: `parse_pathspec()` runs the three
/// checks per element in that order (pathspec.c:650-668), and the mask is the
/// last of them (pathspec.c:657-658).
///
/// This verb tested its mask first, from a magic splitter of its own, so
/// `:(attr:)x` named the mask where git names the attr body — and the two
/// failures that splitter had no opinion on, the empty element and a reserved
/// short mnemonic, were accepted outright at exit 0.
///
/// Its mask is `PATHSPEC_ALL_MAGIC & ~(PATHSPEC_FROMTOP | PATHSPEC_LITERAL)`
/// (builtin/ls-tree.c:420-423), so `top` and `literal` are the two that survive.
#[test]
fn ls_tree_measures_its_mask_after_the_parse_and_after_the_path() {
    let repo = fixture("lstree-mask");
    let root = repo.display().to_string();

    // Parse failures outrank the mask, even for an element whose magic the mask
    // would also have refused.
    let parse_first: &[(&str, &str)] = &[
        (":(attr:)x", "fatal: attr spec must not be empty\n"),
        (":(attr:a,attr:b)x", "fatal: Only one 'attr:' specification is allowed.\n"),
        (":(bogus)x", "fatal: Invalid pathspec magic 'bogus' in ':(bogus)x'\n"),
        (":(icase", "fatal: Missing ')' at the end of pathspec magic in ':(icase'\n"),
        // Neither of these has anything to do with the mask, and both were exit 0.
        (":%x", "fatal: Unimplemented pathspec magic '%' in ':%x'\n"),
        (
            "",
            "fatal: empty string is not a valid pathspec. \
             please use . instead if you meant to match all paths\n",
        ),
    ];
    for (spec, want) in parse_first {
        let out = run(&repo, &["ls-tree", "HEAD", "--", spec]);
        assert_eq!(err_of(&out), *want, "git ls-tree HEAD -- {spec}");
        assert_eq!(out.status.code(), Some(128), "git ls-tree HEAD -- {spec}");
        assert!(out.stdout.is_empty(), "nothing is listed: git ls-tree HEAD -- {spec}");
    }

    // The mask itself, rendered through `pathspec_magic_names()` in table order.
    let masked: &[(&str, &str)] = &[
        (":(icase)x", "fatal: :(icase)x: pathspec magic not supported by this command: 'icase'\n"),
        (
            ":(glob)*.txt",
            "fatal: :(glob)*.txt: pathspec magic not supported by this command: 'glob'\n",
        ),
        (
            ":(exclude)x",
            "fatal: :(exclude)x: pathspec magic not supported by this command: \
             'exclude' (mnemonic: '!')\n",
        ),
    ];
    for (spec, want) in masked {
        let out = run(&repo, &["ls-tree", "HEAD", "--", spec]);
        assert_eq!(err_of(&out), *want, "git ls-tree HEAD -- {spec}");
        assert_eq!(out.status.code(), Some(128), "git ls-tree HEAD -- {spec}");
    }

    // The path is resolved *before* the mask, so an element that is both outside
    // the repository and carries unsupported magic reports the path.
    let sub = repo.join("dir");
    let out = run(&sub, &["ls-tree", "HEAD", "--", ":(icase)../../x"]);
    assert_eq!(
        err_of(&out),
        format!("fatal: :(icase)../../x: '../../x' is outside repository at '{root}'\n")
    );
    assert_eq!(out.status.code(), Some(128));
    // …and one that stays inside gets as far as the mask.
    let out = run(&sub, &["ls-tree", "HEAD", "--", ":(icase)../a.txt"]);
    assert_eq!(
        err_of(&out),
        "fatal: :(icase)../a.txt: pathspec magic not supported by this command: 'icase'\n"
    );
    assert_eq!(out.status.code(), Some(128));

    // A rooted element's path is verbatim here too (pathspec.c:485-487), so its
    // `.` is a path component naming an entry no tree has — while `:(top)` with
    // no path at all is the whole tree.
    for spec in [":(top).", ":(top)./a.txt", ":(top)dir//b.txt", ":(top)a.txt/.."] {
        let out = run(&repo, &["ls-tree", "-r", "HEAD", "--", spec]);
        assert_eq!(err_of(&out), "", "git ls-tree -r HEAD -- {spec}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "",
            "git ls-tree -r HEAD -- {spec}"
        );
    }
    let out = run(&repo, &["ls-tree", "-r", "--name-only", "HEAD", "--", ":(top)dir/"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "dir/b.txt\n");

    // The two keywords outside the mask still work.
    for spec in [":(literal)a.txt", ":(top)a.txt"] {
        let out = run(&repo, &["ls-tree", "HEAD", "--", spec]);
        assert_eq!(err_of(&out), "", "git ls-tree HEAD -- {spec}");
        assert!(
            String::from_utf8_lossy(&out.stdout).ends_with("\ta.txt\n"),
            "git ls-tree HEAD -- {spec}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    let _ = std::fs::remove_dir_all(&repo);
}

/// `cmd_checkout()` parses its pathspec with a mask of `0`
/// (builtin/checkout.c:2016-2018), so every magic keyword is accepted — the
/// malformed ones as `parse_pathspec()`'s `die()` at 128, the well-formed ones
/// as a selection.
///
/// This verb read none of it: every element, `:(literal)a.txt` and `:(bogus)x`
/// alike, was a literal path and so `error: pathspec … did not match any
/// file(s) known to git` at exit 1.
#[test]
fn checkout_and_restore_read_pathspec_magic() {
    let repo = fixture("checkout-magic");
    let dies: &[(&str, &str)] = &[
        (":(bogus)x", "fatal: Invalid pathspec magic 'bogus' in ':(bogus)x'\n"),
        (":(icase", "fatal: Missing ')' at the end of pathspec magic in ':(icase'\n"),
        (":(literal,glob)x", "fatal: :(literal,glob)x: 'literal' and 'glob' are incompatible\n"),
        (":(attr:)x", "fatal: attr spec must not be empty\n"),
        (":%x", "fatal: Unimplemented pathspec magic '%' in ':%x'\n"),
        (
            "",
            "fatal: empty string is not a valid pathspec. \
             please use . instead if you meant to match all paths\n",
        ),
    ];
    for (spec, want) in dies {
        for verb in [&["checkout", "--"][..], &["restore", "--"][..]] {
            let mut args = verb.to_vec();
            args.push(spec);
            let out = run(&repo, &args);
            assert_eq!(err_of(&out), *want, "git {args:?}");
            assert_eq!(out.status.code(), Some(128), "git {args:?}");
        }
    }

    // A well-formed element selects rather than failing to be a filename.
    for spec in [":(literal)a.txt", ":(glob)*.txt", ":(top)a.txt", ":!a.txt", ":", ":/"] {
        let out = run(&repo, &["checkout", "--", spec]);
        assert_eq!(err_of(&out), "", "git checkout -- {spec}");
        assert_eq!(out.status.code(), Some(0), "git checkout -- {spec}");
    }
    let _ = std::fs::remove_dir_all(&repo);
}

/// The same magic, as *matching* rather than as diagnostics: a checkout from
/// another branch writes exactly the files the element selects.
///
/// Each of these was previously a no-match at exit 1, so nothing was written at
/// all. `:(glob)` uses `WM_PATHNAME`, so `*.txt` stops at a `/` and leaves
/// `dir/b.txt` alone; `:!a.txt` subtracts from what `.` selects, which is the
/// one thing a per-element matcher cannot express.
#[test]
fn checkout_magic_selects_the_files_git_selects() {
    let repo = fixture("checkout-select");
    ok(&repo, &["checkout", "-q", "-b", "other"]);
    for (rel, body) in [("a.txt", "a2\n"), ("UPPER.txt", "U2\n"), ("dir/b.txt", "b2\n")] {
        std::fs::write(repo.join(rel), body).unwrap();
    }
    // Named rather than `-A`: the fixture points `HOME` at the work tree, so the
    // binary's own state files live there and `-A` would commit them — and then
    // the branch switch below would refuse to overwrite them.
    ok(&repo, &["add", "--", "a.txt", "UPPER.txt", "dir/b.txt"]);
    ok(&repo, &["-c", "user.name=t", "-c", "user.email=t@e", "commit", "-qm", "two"]);
    ok(&repo, &["checkout", "-q", "main"]);

    let read = |rel: &str| std::fs::read_to_string(repo.join(rel)).unwrap();
    let reset = || ok(&repo, &["checkout", "-q", "main", "--", "."]);

    let out = run(&repo, &["checkout", "other", "--", ":(glob)*.txt"]);
    assert_eq!(err_of(&out), "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!((read("a.txt"), read("UPPER.txt")), ("a2\n".into(), "U2\n".into()));
    assert_eq!(read("dir/b.txt"), "b\n", ":(glob)*.txt must not cross a '/'");
    reset();

    let out = run(&repo, &["checkout", "other", "--", ":(icase)upper.txt"]);
    assert_eq!(err_of(&out), "");
    assert_eq!(read("UPPER.txt"), "U2\n", ":(icase) must match the tracked spelling");
    assert_eq!(read("a.txt"), "a\n");
    reset();

    let out = run(&repo, &["checkout", "other", "--", ".", ":!a.txt"]);
    assert_eq!(err_of(&out), "");
    assert_eq!(read("a.txt"), "a\n", "the exclusion must subtract from what `.` selects");
    assert_eq!((read("UPPER.txt"), read("dir/b.txt")), ("U2\n".into(), "b2\n".into()));
    reset();

    // From a subdirectory, `:(top)` is the whole point: the element is rooted at
    // the work tree, not at the prefix.
    let out = run(&repo.join("dir"), &["checkout", "other", "--", ":(top)a.txt"]);
    assert_eq!(err_of(&out), "");
    assert_eq!(read("a.txt"), "a2\n");
    reset();

    // An element that matches nothing is still `report_path_error()`'s exit 1,
    // and it is the element that matched nothing that gets named — not an
    // exclusion, which `do_match_pathspec()` marks seen as it subtracts.
    let out = run(&repo, &["checkout", "other", "--", ":(glob)*.nosuch"]);
    assert_eq!(
        err_of(&out),
        "error: pathspec ':(glob)*.nosuch' did not match any file(s) known to git\n"
    );
    assert_eq!(out.status.code(), Some(1));
    let out = run(&repo, &["checkout", "other", "--", ".", ":!nosuch.txt"]);
    assert_eq!(err_of(&out), "", "an exclusion that matches nothing is not reported");
    assert_eq!(out.status.code(), Some(0));
    reset();

    let _ = std::fs::remove_dir_all(&repo);
}

/// `:(prefix:<n>)` is read by `parse_long_magic()` before the keyword table
/// (pathspec.c:352-358), raises no magic bit, and is validated by `strtol`'s own
/// `endptr` — so what is legal is what `strtol` consumes in full, and
/// `:(prefix:)` is legal because it leaves nothing behind.
///
/// The vendored parser's keyword table had no `prefix:` at all, so every one of
/// these was `Invalid pathspec magic 'prefix:0'`.
#[test]
fn prefix_magic_parses_the_way_strtol_does() {
    let repo = fixture("prefix");
    for spec in [":(prefix:0)a.txt", ":(prefix:2)a.txt", ":(prefix: 1)a.txt", ":(prefix:+0)a.txt"] {
        let out = run(&repo, &["log", "--oneline", "--name-only", "--", spec]);
        assert_eq!(err_of(&out), "", "git log -- {spec}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("a.txt"),
            "git log -- {spec}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    // Accepted, and rooted, so it selects nothing rather than dying.
    let out = run(&repo, &["log", "--oneline", "--name-only", "--", ":(prefix:)x"]);
    assert_eq!(err_of(&out), "");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert_eq!(out.status.code(), Some(0));
    // A rooted element's path is verbatim, so `./` is not folded away.
    let out = run(&repo, &["log", "--oneline", "--name-only", "--", ":(prefix:0)./a.txt"]);
    assert_eq!(err_of(&out), "");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");

    // What `strtol` does not consume in full is the one die this magic has.
    for spec in [":(prefix:0x)a.txt", ":(prefix:x)a.txt"] {
        let out = run(&repo, &["log", "--oneline", "--", spec]);
        assert_eq!(err_of(&out), "fatal: invalid parameter for pathspec magic 'prefix'\n");
        assert_eq!(out.status.code(), Some(128), "git log -- {spec}");
    }

    // `pathspec_prefix >= 0` is the rooted test (pathspec.c:474, :482), so a
    // negative parameter parses and leaves the element ordinary — which is the
    // only way to tell the two apart from a subdirectory.
    let sub = repo.join("dir");
    let root = repo.display().to_string();
    let out = run(&sub, &["log", "--oneline", "--", ":(prefix:-1)../../x"]);
    assert_eq!(
        err_of(&out),
        format!("fatal: :(prefix:-1)../../x: '../../x' is outside repository at '{root}'\n")
    );
    assert_eq!(out.status.code(), Some(128));
    let _ = std::fs::remove_dir_all(&repo);
}

/// A rooted element's path is `xstrdup(copyfrom)` (pathspec.c:482-487): no
/// prefix is joined to it and, because `prefix_path_gently()` is what calls
/// `normalize_path_copy_len()`, nothing folds its `.`, `..` or repeated `/`
/// either.
///
/// The matcher normalised it anyway, which made `:(top)./a.txt` match `a.txt`,
/// `:(top).` and `:(top)a.txt/..` select the whole tree, and `:(top)../x` — a
/// plain no-match in git — an error that surfaced as exit 128 with empty stderr.
#[test]
fn a_rooted_elements_path_is_taken_verbatim() {
    let repo = fixture("verbatim");
    let sub = repo.join("dir");

    // Each of these is a literal path that names no entry, so: no output, exit 0.
    for spec in [
        ":(top)../x",
        ":(top)./a.txt",
        ":(top).",
        ":(top)a.txt/..",
        ":(top)dir//b.txt",
        ":/./a.txt",
    ] {
        for dir in [&repo, &sub] {
            let out = run(dir, &["log", "--oneline", "--name-only", "--", spec]);
            assert_eq!(err_of(&out), "", "git log -- {spec} in {}", dir.display());
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "",
                "git log -- {spec} in {}",
                dir.display()
            );
            assert_eq!(out.status.code(), Some(0), "git log -- {spec} in {}", dir.display());
        }
    }

    // And what a rooted element *does* name, it names from the work tree root
    // whichever directory the command ran in.
    for (spec, want) in [(":(top)a.txt", "a.txt\n"), (":(top)dir/", "dir/b.txt\n")] {
        for dir in [&repo, &sub] {
            let out = run(dir, &["log", "--format=", "--name-only", "--", spec]);
            assert_eq!(err_of(&out), "", "git log -- {spec} in {}", dir.display());
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim_start_matches('\n'),
                want,
                "git log -- {spec} in {}",
                dir.display()
            );
        }
    }
    let _ = std::fs::remove_dir_all(&repo);
}

/// `normalize_path_copy_len()` folds a trailing `.` away but keeps the separator
/// it sat behind (path.c:1121-1204), so `a/.` is `a/` — a *directory* spec — and
/// not `a`.
///
/// The matcher raised its directory flag only for a slash the user wrote last
/// and then folded the `/.` away without a trace, so `a.txt/.` matched the file
/// `a.txt` that git leaves alone. Only `checkout` got this right, from a matcher
/// of its own that no other verb used.
#[test]
fn a_trailing_dot_component_is_a_directory_spec_everywhere() {
    let repo = fixture("trailing-dot");
    // A file cannot be named by a spec that ends at a directory boundary…
    for spec in ["a.txt/.", "a.txt/", "a.txt/./", "dir/b.txt/."] {
        let out = run(&repo, &["log", "--format=", "--name-only", "--", spec]);
        assert_eq!(err_of(&out), "", "git log -- {spec}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "git log -- {spec}");
    }
    // …while a directory is named by every spelling of it.
    for spec in ["dir", "dir/", "dir/.", "dir/./", "./dir/."] {
        let out = run(&repo, &["log", "--format=", "--name-only", "--", spec]);
        assert_eq!(err_of(&out), "", "git log -- {spec}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "dir/b.txt\n", "git log -- {spec}");
    }
    // `..` pops without leaving a directory boundary behind, so a file's parent
    // spec still reaches the file.
    let out = run(&repo, &["log", "--format=", "--name-only", "--", "dir/b.txt/.."]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "dir/b.txt\n");
    let _ = std::fs::remove_dir_all(&repo);
}
