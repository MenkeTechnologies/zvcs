//! `git range-diff`'s argument-shape dispatch (`cmd_range_diff`,
//! `builtin/range-diff.c`) and what it does with the operands that trail the
//! shape it chose.
//!
//! Upstream accepts three shapes — `<base> <old-tip> <new-tip>`,
//! `<range1> <range2>` and `<old-tip>...<new-tip>` — and everything past the
//! operand count of the shape it picked is pushed onto `log_arg`
//! (builtin/range-diff.c:128/148/179), which `read_patches()` splices in after the
//! range on the command line of the `git log` it runs for *each* range
//! (range-diff.c:71-73). So a trailing operand is not an error: it is another
//! `git log` operand, and it widens both walks or limits them to a pathspec.
//!
//! The regression this pins: the port used to refuse any trailing operand that
//! resolved, with `fatal: range-diff: a stray revision operand is not supported`
//! and exit 128, where stock 2.55.0 prints a range-diff and exits 0.
//!
//! Every expectation below was read off stock git 2.55.0 in a repository built by
//! the same steps. Self-contained: no network, no system git.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// stdout, stderr and the exit status of one run of the shadow binary.
fn run(cwd: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// A range-diff page reduced to what an argument-shape test can assert about it.
///
/// Every output line is `<left>:  <id> <marker> <right>:  <id> <subject>`, and the
/// two ids are the only part that varies between runs of the same fixture. Drop
/// them and what is left — which position each commit took on each side, whether
/// it was matched, and its subject — is exactly the evidence for which commits the
/// two walks contained.
fn shape(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(|line| {
            let mut f = line.split_whitespace();
            let left = f.next().unwrap_or_default();
            f.next(); // left id
            let marker = f.next().unwrap_or_default();
            let right = f.next().unwrap_or_default();
            f.next(); // right id
            let subject: Vec<&str> = f.collect();
            format!("{left} {marker} {right} {}", subject.join(" "))
        })
        .collect()
}

/// ```text
/// c0 --- c1 --- c2      main   (c1 = v1, c2 = v2, an annotated tag)
///          \
///           -- s1       feature
/// ```
///
/// `c2` touches only `h`, `s1` only `g`, so a pathspec tells the two branches
/// apart and neither commit is a plausible rewrite of the other.
fn fixture(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    for m in ["c0", "c1"] {
        std::fs::write(repo.join("f"), format!("{m}\n")).unwrap();
        run(&repo, &home, &["add", "f"]);
        run(&repo, &home, &["commit", "-q", "-m", m]);
    }
    run(&repo, &home, &["tag", "v1"]);
    run(&repo, &home, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("g"), "s1\n").unwrap();
    run(&repo, &home, &["add", "g"]);
    run(&repo, &home, &["commit", "-q", "-m", "s1"]);
    run(&repo, &home, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("h"), "c2\n").unwrap();
    run(&repo, &home, &["add", "h"]);
    run(&repo, &home, &["commit", "-q", "-m", "c2"]);
    run(&repo, &home, &["tag", "-a", "v2", "-m", "two"]);
    (repo, home)
}

/// All three shapes describe the same pair of ranges here, so all three have to
/// print the same page: `c2` only on the left, `s1` only on the right.
#[test]
fn every_accepted_argument_shape_names_the_same_two_ranges() {
    let root = std::env::temp_dir().join(format!("zvcs-rdshape-a{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (repo, home) = fixture(&root);

    let expected = ["1: < -: c2", "-: > 1: s1"];
    for args in [
        // `<base> <old-tip> <new-tip>`, which becomes `v1..main` and `v1..feature`.
        &["range-diff", "v1", "main", "feature"][..],
        // `<range1> <range2>`, spelled out.
        &["range-diff", "v1..main", "v1..feature"][..],
        // `<old-tip>...<new-tip>`, which becomes `feature..main` and `main..feature`.
        &["range-diff", "main...feature"][..],
    ] {
        let (out, err, code) = run(&repo, &home, args);
        assert_eq!(code, 0, "{args:?} stderr: {err}");
        assert_eq!(shape(&out), expected, "{args:?}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// A trailing revision operand is appended to the `git log` of *both* ranges, so
/// naming `main` pulls `c2` into the right-hand walk as well and the two sides
/// pair up. This is the case that used to die with `a stray revision operand is
/// not supported`.
#[test]
fn a_trailing_revision_operand_widens_both_walks() {
    let root = std::env::temp_dir().join(format!("zvcs-rdshape-b{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (repo, home) = fixture(&root);

    let (out, err, code) = run(
        &repo,
        &home,
        &["range-diff", "v1..main", "v1..feature", "main"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(shape(&out), ["1: = 1: c2", "-: > 2: s1"]);

    // The same operand under the symmetric shape adds nothing: `main` is already
    // a tip of one side and hidden by the other.
    let (out, _, code) = run(&repo, &home, &["range-diff", "main...feature", "main"]);
    assert_eq!(code, 0);
    assert_eq!(shape(&out), ["1: < -: c2", "-: > 1: s1"]);

    // `^<rev>` is a negative operand: hiding `main` empties the left side. It also
    // proves `UNINTERESTING` beats a positive mention — `v1..main` names `main` as
    // its tip, and the operand still wins.
    let (out, _, code) = run(
        &repo,
        &home,
        &["range-diff", "v1..main", "v1..feature", "^main"],
    );
    assert_eq!(code, 0);
    assert_eq!(shape(&out), ["-: > 1: s1"]);

    let _ = std::fs::remove_dir_all(&root);
}

/// An operand that does not resolve but names a worktree path is the pathspec,
/// with or without the `--` that would have said so explicitly.
#[test]
fn a_trailing_path_operand_becomes_the_pathspec() {
    let root = std::env::temp_dir().join(format!("zvcs-rdshape-c{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (repo, home) = fixture(&root);

    // `h` is only touched by `c2`, so `s1` drops out of the right-hand range.
    for args in [
        &["range-diff", "v1..main", "v1..feature", "h"][..],
        &["range-diff", "v1..main", "v1..feature", "--", "h"][..],
    ] {
        let (out, err, code) = run(&repo, &home, args);
        assert_eq!(code, 0, "{args:?} stderr: {err}");
        assert_eq!(shape(&out), ["1: < -: c2"], "{args:?}");
    }

    // Revisions first, then the path: `main` widens the walks and `h` limits them.
    let (out, _, code) = run(
        &repo,
        &home,
        &["range-diff", "v1..main", "v1..feature", "main", "h"],
    );
    assert_eq!(code, 0);
    assert_eq!(shape(&out), ["1: = 1: c2"]);

    let _ = std::fs::remove_dir_all(&root);
}

/// The shapes git genuinely rejects, with the message and status it rejects them
/// with. A `--` at a given position *forces* the shape of that arity, so the
/// operands before it are then validated against it rather than auto-detected.
#[test]
fn rejected_shapes_keep_gits_message_and_status() {
    let root = std::env::temp_dir().join(format!("zvcs-rdshape-d{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (repo, home) = fixture(&root);

    // No operands at all: `usage_msg_opt()`, which is a usage error at 129.
    let (_, err, code) = run(&repo, &home, &["range-diff"]);
    assert_eq!(code, 129);
    assert_eq!(err.lines().next(), Some("fatal: need two commit ranges"));

    // `--` in slot 2 forces the two-range shape, and `v1` is not a range.
    let (_, err, code) = run(&repo, &home, &["range-diff", "v1", "main", "--", "feature"]);
    assert_eq!(code, 129);
    assert_eq!(err.lines().next(), Some("fatal: not a commit range: 'v1'"));

    // `--` in slot 1 forces the symmetric shape, and `v1..main` has no `...`.
    let (_, err, code) = run(&repo, &home, &["range-diff", "v1..main", "--", "f"]);
    assert_eq!(code, 129);
    assert_eq!(
        err.lines().next(),
        Some("fatal: not a symmetric range: 'v1..main'")
    );

    // An operand that is neither a revision nor a path reaches the inner `git
    // log`, which dies; range-diff adds its own `error()` and returns -1 — git's
    // exit status 255.
    let (_, err, code) = run(
        &repo,
        &home,
        &["range-diff", "v1..main", "v1..feature", "nope"],
    );
    assert_eq!(code, 255);
    assert_eq!(
        err,
        "fatal: ambiguous argument 'nope': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n\
         error: could not parse log for 'v1..main'\n"
    );

    // A token that is already in path position is not a misspelt revision, so it
    // gets the shorter message.
    let (_, err, code) = run(
        &repo,
        &home,
        &["range-diff", "v1..main", "v1..feature", "h", "nope"],
    );
    assert_eq!(code, 255);
    assert_eq!(
        err,
        "fatal: nope: no such path in the working tree.\n\
         Use 'git <command> -- <path>...' to specify paths that do not exist locally.\n\
         error: could not parse log for 'v1..main'\n"
    );

    // An explicitly negative operand has no pathspec reading to fall back on.
    let (_, err, code) = run(
        &repo,
        &home,
        &["range-diff", "v1..main", "v1..feature", "^nope"],
    );
    assert_eq!(code, 255);
    assert_eq!(
        err,
        "fatal: bad revision '^nope'\nerror: could not parse log for 'v1..main'\n"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `get_oid_basic()`'s first branch (`object-name.c`): a name of exactly the
/// hash's hex length **is** the object id, decoded before the ref store is
/// consulted at all.
///
/// ```c
/// if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid)) {
///         …
///         return 0;
/// }
/// ```
///
/// So in a repository that holds a ref literally named 40 hex digits, an operand
/// spelling those digits resolves to the *object* — even when the repository does
/// not have it, in which case `get_reference()` dies `bad object` — and never to
/// the ref. gitoxide's `rev_parse` looks the id up and falls back to a ref of the
/// same name, which silently walked that ref's history here.
///
/// The `warning: refname … is ambiguous.` is the same branch's other half, and
/// its *count* is the evidence for how many times the operand was resolved:
/// upstream runs one `git log` per range and hands both of them the trailing
/// operands, so a trailing 40-hex earns the warning once per range.
#[test]
fn a_full_length_hex_operand_is_the_object_id_not_a_ref_of_that_name() {
    let root = std::env::temp_dir().join(format!("zvcs-rdshape-e{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (repo, home) = fixture(&root);

    // A ref whose name is 40 hex digits naming an object that does not exist…
    let absent = "0123456789012345678901234567890123456789";
    run(&repo, &home, &["update-ref", &format!("refs/heads/{absent}"), "main"]);
    // …and one naming an object that does, so both sides of the rule are covered.
    let (present, _, _) = run(&repo, &home, &["rev-parse", "main~1"]);
    let present = present.trim().to_owned();
    run(&repo, &home, &["update-ref", &format!("refs/heads/{present}"), "main"]);

    let warnings = |err: &str| err.lines().filter(|l| l.starts_with("warning: refname")).count();

    // As a range operand: `is_range_diff_range()` resolves it, `get_reference()`
    // finds nothing, and `setup_revisions()` dies inside the inner log.
    let (out, err, code) = run(&repo, &home, &["range-diff", "v1..main", absent]);
    assert_eq!(code, 128, "stderr: {err}");
    assert!(out.is_empty(), "{out}");
    assert_eq!(warnings(&err), 1, "{err}");
    assert!(
        err.contains(&format!("fatal: bad object {absent}\n")),
        "{err}"
    );

    // As a trailing operand it reaches range1's log, which dies the same way;
    // range-diff adds its own `error()` and returns -1, git's exit status 255.
    let (_, err, code) = run(
        &repo,
        &home,
        &["range-diff", "v1..main", "v1..feature", absent],
    );
    assert_eq!(code, 255, "stderr: {err}");
    assert_eq!(warnings(&err), 1, "{err}");
    assert!(err.contains(&format!("fatal: bad object {absent}\n")), "{err}");
    assert!(err.ends_with("error: could not parse log for 'v1..main'\n"), "{err}");

    // The object is present this time, so both logs resolve it and both warn.
    // `main~1` is already reachable, so the page is the plain one.
    let (out, err, code) = run(
        &repo,
        &home,
        &["range-diff", "v1..main", "v1..feature", &present],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(warnings(&err), 2, "{err}");
    assert_eq!(shape(&out), ["1: < -: c2", "-: > 1: s1"]);

    // The same id as a range endpoint: `handle_dotdot_1()` resolves it, so the
    // warning comes once from the shape classifier and once from range2's log.
    let (out, err, code) = run(
        &repo,
        &home,
        &["range-diff", "v1..main", &format!("{present}..main")],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(warnings(&err), 2, "{err}");
    assert_eq!(shape(&out), ["1: = 1: c2"]);

    let _ = std::fs::remove_dir_all(&root);
}

/// `is_range_diff_range()` counts the *pending list*, and `UNINTERESTING` is a
/// bit on the `struct object` each entry points at — not on the entry:
///
/// ```c
/// if (setup_revisions(3, argv, &revs, NULL) == 1) {
///         for (i = 0; i < revs.pending.nr; i++)
///                 if (revs.pending.objects[i].item->flags & UNINTERESTING)
///                         negative++;
///                 else
///                         positive++;
/// }
/// …
/// return negative > 0 && positive > 0;
/// ```
///
/// So when both endpoints name the same commit the two entries *are* the same
/// object, and `handle_dotdot_1()`'s
///
/// ```c
/// a_obj->flags |= a_flags;
/// b_obj->flags |= flags;
/// ```
///
/// leaves it `UNINTERESTING` — `b_obj->flags |= 0` cannot clear what the line
/// above set. `<x>..<x>` is `negative == 2, positive == 0` and therefore **not**
/// a range, which is why stock 2.55.0 answers `fatal: need two commit ranges`
/// for a command line whose two operands both look like ranges.
///
/// A port that asks only "are both lists non-empty?" calls it a range, silently
/// takes the two-range shape, and prints a page where stock refuses the command.
#[test]
fn a_range_whose_endpoints_are_one_commit_is_not_a_range() {
    let root = std::env::temp_dir().join(format!("zvcs-rdshape-f{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (repo, home) = fixture(&root);

    // The control: two operands that really are ranges take the two-range shape.
    let (out, err, code) = run(&repo, &home, &["range-diff", "v1..main", "v1..feature"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(shape(&out), ["1: < -: c2", "-: > 1: s1"]);

    // One collapsed operand is enough to lose the shape, on either side and
    // whether the endpoints are spelled alike or not — `v1` and `main~1` are one
    // commit under two names.
    for args in [
        &["range-diff", "main..main", "v1..feature"][..],
        &["range-diff", "v1..main", "main..main"][..],
        &["range-diff", "v1..v1", "v1..feature"][..],
        &["range-diff", "v1..main", "v1..main~1"][..],
    ] {
        let (out, err, code) = run(&repo, &home, args);
        assert_eq!(code, 129, "{args:?}: stderr: {err}");
        assert!(out.is_empty(), "{args:?}: {out}");
        assert!(
            err.starts_with("fatal: need two commit ranges\n\n"),
            "{args:?}: {err}"
        );
    }

    // The symmetric spelling collapses through the merge base instead:
    // `repo_get_merge_bases(a, a)` is `a`, and `add_pending_commit_list()` marks
    // it `UNINTERESTING` *before* `a_flags`/`flags` are applied. So `<x>...<x>`
    // is not a range either, and `cmd_range_diff` falls past the two-range shape
    // onto the `strstr(argv[0], "...")` one — which makes the trailing operand
    // the `log_arg` both walks share, and both walks then hold exactly it.
    let (out, err, code) = run(&repo, &home, &["range-diff", "main...main", "v1..feature"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(shape(&out), ["1: = 1: s1"]);
}

/// `cmd_range_diff` tests its first three operands with
/// `repo_get_oid_committish()`, which is handed `argv[n]` **as typed**.
///
/// The `^` exclusion mark belongs to `revision.c`:
///
/// ```c
/// if (*arg == '^') {
///         local_flags = UNINTERESTING | BOTTOM;
///         arg++;
/// }
/// ```
///
/// and `object-name.c` never performs that strip on its own. So a leading `^`
/// makes `get_oid_basic()` measure one character too many for its full-hex
/// branch, and the `warning: refname … is ambiguous.` that branch carries is not
/// printed — while the *same* operand reaching `is_range_diff_range()`, whose
/// `setup_revisions()` does strip the mark, prints it exactly once.
///
/// One warning, not two. Resolving the shape probe through a helper that strips
/// the mark the way `handle_revision_arg_1()` does doubles it, and an
/// over-warning port is worse than a silent one: the message exists to point at
/// a ref created by mistake, so a spurious one sends the reader hunting for a
/// ref that is behaving perfectly.
#[test]
fn the_exclusion_mark_is_not_stripped_before_the_committish_probe() {
    let root = std::env::temp_dir().join(format!("zvcs-rdshape-g{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (repo, home) = fixture(&root);

    let (present, _, _) = run(&repo, &home, &["rev-parse", "main~1"]);
    let present = present.trim().to_owned();
    run(&repo, &home, &["update-ref", &format!("refs/heads/{present}"), "main"]);
    let warnings = |err: &str| err.lines().filter(|l| l.starts_with("warning: refname")).count();

    // `^<40-hex>` is not a committish and not a range, so the shape dispatch runs
    // out of forms — after exactly one resolution has warned.
    let (out, err, code) = run(&repo, &home, &["range-diff", &format!("^{present}"), "v1", "main"]);
    assert_eq!(code, 129, "stderr: {err}");
    assert!(out.is_empty(), "{out}");
    assert_eq!(warnings(&err), 1, "{err}");
    assert!(err.starts_with("warning: refname"), "{err}");
    assert!(err.contains("fatal: need two commit ranges\n"), "{err}");

    // The same shape without the 40-hex spelling: same refusal, nothing to warn
    // about, so the warning above is the operand's and not the shape's.
    let (_, err, code) = run(&repo, &home, &["range-diff", "^v1", "v1", "main"]);
    assert_eq!(code, 129, "stderr: {err}");
    assert_eq!(warnings(&err), 0, "{err}");
    assert!(err.starts_with("fatal: need two commit ranges\n"), "{err}");
}
