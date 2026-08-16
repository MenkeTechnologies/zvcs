//! What `git range-diff` does with the `git diff` options `add_diff_options()`
//! wires into it (builtin/range-diff.c:83).
//!
//! Those options configure the *outer* diff — the diff-of-diffs — and nothing
//! else; only `--notes`, `--diff-merges` and `--remerge-diff` are
//! `OPT_PASSTHRU_ARGV` entries that reach the inner `git log`
//! (builtin/range-diff.c:56-66). This file pins the three groups the port has to
//! tell apart:
//!
//! * options that provably cannot change a byte here and are therefore accepted
//!   in silence (`--textconv`, the prefix options, `--exit-code`, …);
//! * options that stop the run *before* any revision is resolved (`--follow`
//!   and the three pickaxe combinations `diff_setup_done()` refuses);
//! * options the port actually renders (`--notes=<ref>`, `--max-memory`,
//!   `--output`, the diff algorithms, `-U<n>`, the output indicators).
//!
//! Every expectation below was read off stock git 2.55.0 in a repository built
//! by the same steps, then hardcoded. Commit ids are the only part that varies
//! between runs, so they are masked out before comparing. Self-contained: no
//! network, no system git, no terminal.

use std::path::{Path, PathBuf};
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

/// The page with every abbreviated commit id replaced by `<id>`, which is the
/// only thing that differs between two runs of the same fixture.
fn masked(page: &str) -> String {
    let mut out = String::with_capacity(page.len());
    for line in page.split_inclusive('\n') {
        let mut rest = line;
        while let Some(at) = rest.find(|c: char| c.is_ascii_hexdigit()) {
            let end = at + rest[at..]
                .find(|c: char| !c.is_ascii_hexdigit())
                .unwrap_or(rest.len() - at);
            let word_start = at == 0 || !rest.as_bytes()[at - 1].is_ascii_alphanumeric();
            let word_end = end == rest.len() || !rest.as_bytes()[end].is_ascii_alphanumeric();
            if end - at == 7 && word_start && word_end {
                out.push_str(&rest[..at]);
                out.push_str("<id>");
            } else {
                out.push_str(&rest[..end]);
            }
            rest = &rest[end..];
        }
        out.push_str(rest);
    }
    out
}

fn repo_at(root: &Path, name: &str) -> (PathBuf, PathBuf) {
    let home = root.join("home");
    let repo = root.join(name);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    (repo, home)
}

fn commit(repo: &Path, home: &Path, body: &str, message: &str) {
    std::fs::write(repo.join("f"), body).unwrap();
    run(repo, home, &["add", "f"]);
    run(repo, home, &["commit", "-q", "-m", message]);
}

/// Two two-patch series over the same base, rewritten on the right-hand side, so
/// both ranges are non-empty and both pairs are `!` — the shape in which a
/// deferred option *would* reach the output. Each side's first commit also
/// carries a note in the default tree and one in `refs/notes/alt`.
///
/// ```text
/// c0 --- patch one --- patch two    main     (c0 = v1)
///   \
///    -- patch one' -- patch two'    feature
/// ```
fn fixture_pairs(root: &Path) -> (PathBuf, PathBuf) {
    let (repo, home) = repo_at(root, "pairs");
    commit(&repo, &home, "a\nb\nc\nd\ne\nf\ng\nh\n", "c0");
    run(&repo, &home, &["tag", "v1"]);
    commit(&repo, &home, "a\nb\nCHANGED\nd\ne\nf\ng\nh\n", "patch one");
    commit(&repo, &home, "a\nb\nCHANGED\nd\ne\nf\ng\nZZZ\n", "patch two");
    run(&repo, &home, &["checkout", "-q", "-b", "feature", "v1"]);
    commit(&repo, &home, "a\nb\nCHANGED2\nd\ne\nf\ng\nh\n", "patch one");
    commit(&repo, &home, "a\nb\nCHANGED2\nd\ne\nf\ng\nYYY\n", "patch two");
    for (rev, text) in [("main~1", "default note L"), ("feature~1", "default note R")] {
        run(&repo, &home, &["notes", "add", "-m", text, rev]);
    }
    for (rev, text) in [("main~1", "alt note L"), ("feature~1", "alt note R")] {
        run(&repo, &home, &["notes", "--ref=alt", "add", "-m", text, rev]);
    }
    (repo, home)
}

/// One commit per side over a file of repeating tokens, chosen so that Myers,
/// histogram and patience each lay the diff-of-diffs out differently — without
/// that the four algorithm flags would all be indistinguishable no-ops and the
/// assertions below would prove nothing.
fn fixture_algorithms(root: &Path) -> (PathBuf, PathBuf) {
    let (repo, home) = repo_at(root, "algorithms");
    let lines = |s: &str| s.split(' ').collect::<Vec<_>>().join("\n") + "\n";
    commit(&repo, &home, &lines("c e d e d a a c d a a c e d c d c a d"), "base");
    run(&repo, &home, &["tag", "v1"]);
    commit(&repo, &home, &lines("c d e d a a c d a b a c e d c d c a d a"), "p1");
    run(&repo, &home, &["checkout", "-q", "-b", "feature", "v1"]);
    commit(&repo, &home, &lines("c e e d a a c d a a c d c d c a d"), "p1");
    (repo, home)
}

/// The same idea for the indent heuristic: a diff-of-diffs with a hunk that can
/// slide, so `--no-indent-heuristic` lands it somewhere else.
fn fixture_indent(root: &Path) -> (PathBuf, PathBuf) {
    let (repo, home) = repo_at(root, "indent");
    let lines = |s: &str| s.split(' ').collect::<Vec<_>>().join("\n") + "\n";
    commit(&repo, &home, &lines("a d d d b e c a c a c e"), "base");
    run(&repo, &home, &["tag", "v1"]);
    commit(&repo, &home, &lines("d a b d c d d b e c a c a c e"), "p1");
    run(&repo, &home, &["checkout", "-q", "-b", "feature", "v1"]);
    commit(&repo, &home, &lines("a c d d d b c a c a c e"), "p1");
    (repo, home)
}

/// Nine commits per side, so the `n x n` cost matrix is 1296 bytes — past the
/// KiB boundary of `strbuf_humanise_bytes()`, which the fatal's `%s` uses.
fn fixture_nine(root: &Path) -> (PathBuf, PathBuf) {
    let (repo, home) = repo_at(root, "nine");
    let mut body = String::from("base\n");
    commit(&repo, &home, &body, "base");
    run(&repo, &home, &["tag", "v1"]);
    let base = body.clone();
    for i in 1..=9 {
        body.push_str(&format!("m{i}\n"));
        commit(&repo, &home, &body, &format!("c{i}"));
    }
    run(&repo, &home, &["checkout", "-q", "-b", "feature", "v1"]);
    body = base;
    for i in 1..=9 {
        body.push_str(&format!("f{i}\n"));
        commit(&repo, &home, &body, &format!("c{i}"));
    }
    (repo, home)
}

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-rddefer-{tag}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

/// A context line of the diff-of-diffs is the four-space `output_prefix`, then
/// the context marker, then the record — so a blank record renders as five
/// spaces. Those lines are spelled with a trailing `\x20` throughout this file,
/// because a literal trailing space in the source would not survive an editor.
const PAIRS_BASELINE: &str = "\
1:  <id> ! 1:  <id> patch one
    @@ Commit message
    \x20
    \x20
      ## Notes ##
    -    default note L
    +    default note R
    \x20
      ## f ##
     @@
      a
      b
     -c
    -+CHANGED
    ++CHANGED2
      d
      e
      f
2:  <id> ! 2:  <id> patch two
    @@ f: d
      f
      g
     -h
    -+ZZZ
    ++YYY
";

/// Every diff option upstream accepts here that provably cannot change a byte of
/// the page, either because the bytes it touches are already discarded
/// (`--textconv` against a preset driver, the prefixes and `--line-prefix`
/// against `suppress_diff_headers` / `output_prefix_data`) or because the
/// machinery it addresses does not exist in a diff of two in-memory buffers
/// (`--relative`, `--ignore-submodules`, `--max-depth`, the `--ita-*` pair).
///
/// The regression this pins is the one that made them worth accepting: they used
/// to be *deferred*, which on this both-sides-non-empty fixture meant
/// `fatal: unsupported flag` and exit 128 where stock prints the page and exits
/// 0. Asserting against the flagless page rather than a copy of it also catches
/// the opposite mistake — an option quietly starting to change the output.
#[test]
fn provable_no_op_diff_options_leave_the_page_untouched() {
    let root = scratch("noop");
    let (repo, home) = fixture_pairs(&root);

    let (base, err, code) = run(&repo, &home, &["range-diff", "--no-color", "v1..main", "v1..feature"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(masked(&base), PAIRS_BASELINE);

    for flag in [
        "--textconv",
        "--no-textconv",
        "--src-prefix=X/",
        "--dst-prefix=Y/",
        "--no-prefix",
        "--default-prefix",
        "--line-prefix=>>",
        "--exit-code",
        "--no-exit-code",
        "--relative",
        "--no-relative",
        "--ignore-submodules",
        "--ignore-submodules=all",
        "--submodule=log",
        "--ita-invisible-in-index",
        "--ita-visible-in-index",
        "--max-depth=1",
        "--full-index",
        "--binary",
        // Modifiers that `diffcore_std()` only consults once a `-S`/`-G`/
        // `--find-object` kind bit is set (diff.c:7517), and every option that
        // sets one is refused separately.
        "--pickaxe-all",
        "--pickaxe-regex",
    ] {
        let (out, err, code) = run(
            &repo,
            &home,
            &["range-diff", "--no-color", flag, "v1..main", "v1..feature"],
        );
        assert_eq!(code, 0, "{flag} stderr: {err}");
        assert_eq!(out, base, "{flag} changed the page");
    }

    // The same options spelled with a detached value: the value has to be
    // consumed, not classified as a third revision operand.
    for pair in [
        ["--src-prefix", "X/"],
        ["--dst-prefix", "Y/"],
        ["--line-prefix", ">>"],
        ["--max-depth", "1"],
    ] {
        let (out, err, code) = run(
            &repo,
            &home,
            &["range-diff", "--no-color", pair[0], pair[1], "v1..main", "v1..feature"],
        );
        assert_eq!(code, 0, "{pair:?} stderr: {err}");
        assert_eq!(out, base, "{pair:?} changed the page");
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// `--quiet` is `flags.quick`, which `diff_setup_done()` turns into
/// `DIFF_FORMAT_NO_OUTPUT` and `exit_with_status` (diff.c:5348-5352) — and
/// range-diff never calls `diff_result_code()`, so the status stays 0 and the
/// only visible effect is `-s`'s: pair headers, no bodies. Because that
/// assignment happens *after* the `check_mask` test at diff.c:5259, `--quiet`
/// also never joins the `cannot be used together` fatal that `-s` does.
#[test]
fn quiet_is_no_patch_without_joining_the_format_conflict() {
    let root = scratch("quiet");
    let (repo, home) = fixture_pairs(&root);

    let headers_only = "1:  <id> ! 1:  <id> patch one\n2:  <id> ! 2:  <id> patch two\n";
    for args in [
        &["range-diff", "--no-color", "-s", "v1..main", "v1..feature"][..],
        &["range-diff", "--no-color", "--quiet", "v1..main", "v1..feature"][..],
        // `--name-only` is deferred, but `--quiet` means no body is rendered at
        // all, so it provably cannot show — the page is emitted, not refused.
        &["range-diff", "--no-color", "--quiet", "--name-only", "v1..main", "v1..feature"][..],
    ] {
        let (out, err, code) = run(&repo, &home, args);
        assert_eq!(code, 0, "{args:?} stderr: {err}");
        assert_eq!(masked(&out), headers_only, "{args:?}");
    }

    // `-s --name-only` is two output-format bits, which is the fatal.
    let (_, err, code) = run(
        &repo,
        &home,
        &["range-diff", "-s", "--name-only", "v1..main", "v1..feature"],
    );
    assert_eq!(code, 128);
    assert_eq!(
        err,
        "fatal: options '--name-only', '--name-status', '--check', and '-s' \
         cannot be used together\n"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `--notes[=<ref>]` reaches the inner `git log` verbatim
/// (builtin/range-diff.c:56-58), so the notes it selects become part of the text
/// being compared. `read_patches()` then rewrites each `Notes[ (<ref>)]:` header
/// into ` ## Notes[ (<ref>)] ##` (range-diff.c:181-186).
///
/// The regression this pins: `--notes=<ref>` used to be treated as a bare
/// `--notes`, silently comparing the *default* tree's notes under the default
/// heading no matter which ref was asked for.
#[test]
fn notes_ref_selects_the_tree_and_names_the_block() {
    let root = scratch("notes");
    let (repo, home) = fixture_pairs(&root);

    let rd = |args: &[&str]| {
        let mut argv = vec!["range-diff", "--no-color"];
        argv.extend_from_slice(args);
        argv.extend_from_slice(&["v1..main", "v1..feature"]);
        let (out, err, code) = run(&repo, &home, &argv);
        assert_eq!(code, 0, "{args:?} stderr: {err}");
        masked(&out)
    };

    // The head of the page down to the end of the notes block is the only part
    // any of these change; the file section below it is `PAIRS_BASELINE`'s.
    let head = |page: &str| -> String {
        page.lines()
            .take_while(|l| !l.contains("## f ##"))
            .map(|l| format!("{l}\n"))
            .collect()
    };

    assert_eq!(rd(&[]), PAIRS_BASELINE);

    // An explicit ref replaces the default tree, and names its own block.
    assert_eq!(
        head(&rd(&["--notes=alt"])),
        "\
1:  <id> ! 1:  <id> patch one
    @@ Commit message
    \x20
    \x20
      ## Notes (alt) ##
    -    alt note L
    +    alt note R
    \x20
"
    );
    // `refs/notes/` is prepended if absent, and the block is named the same way.
    assert_eq!(rd(&["--notes=refs/notes/alt"]), rd(&["--notes=alt"]));

    // A bare `--notes` re-enables the default tree, so both blocks print — the
    // default first, then the explicitly named one, whichever order they were
    // given in.
    let both = "\
1:  <id> ! 1:  <id> patch one
    @@ Commit message
    \x20
    \x20
      ## Notes ##
    -    default note L
    +    default note R
    \x20
    \x20
      ## Notes (alt) ##
    -    alt note L
    +    alt note R
    \x20
";
    assert_eq!(head(&rd(&["--notes=alt", "--notes"])), both);
    assert_eq!(head(&rd(&["--notes", "--notes=alt"])), both);
    // `--notes=commits` names the default tree, which is where the default block
    // comes from — so it is the baseline, not a second block.
    assert_eq!(rd(&["--notes=commits"]), PAIRS_BASELINE);

    // `--no-notes` forgets every ref asked for; a later `--notes=<ref>` starts
    // again from nothing but the ref.
    assert!(!rd(&["--no-notes"]).contains("## Notes"));
    assert_eq!(rd(&["--notes=alt", "--no-notes"]), rd(&["--no-notes"]));
    assert_eq!(rd(&["--no-notes", "--notes=alt"]), rd(&["--notes=alt"]));

    let _ = std::fs::remove_dir_all(&root);
}

/// `--output=<file>` replaces `diffopt.file`, and `output_pair_header()` writes
/// through that same handle (range-diff.c:467) — so the *whole* page lands in the
/// file and stdout stays empty, headers included. The file is opened by
/// `xfopen()` while parse-options runs (diff.c:5829-5830), which is what makes
/// an uncreatable path fatal ahead of every other check.
#[test]
fn output_file_takes_the_whole_page_including_the_headers() {
    let root = scratch("output");
    let (repo, home) = fixture_pairs(&root);
    let out_path = root.join("page.txt");
    let out_arg = format!("--output={}", out_path.display());

    let (stdout, err, code) = run(
        &repo,
        &home,
        &["range-diff", "--no-color", &out_arg, "v1..main", "v1..feature"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(stdout, "", "stdout must stay empty");
    assert_eq!(masked(&std::fs::read_to_string(&out_path).unwrap()), PAIRS_BASELINE);

    // Detached value spelling, and the header-only page of a range whose two
    // sides cannot match: this is the case the honesty guard used to let reach
    // *stdout* while stock wrote it to the file.
    let (stdout, err, code) = run(
        &repo,
        &home,
        &["range-diff", "--no-color", "--output", out_path.to_str().unwrap(), "v1...main"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(stdout, "");
    assert_eq!(
        masked(&std::fs::read_to_string(&out_path).unwrap()),
        "-:  ------- > 1:  <id> patch one\n-:  ------- > 2:  <id> patch two\n"
    );

    // A range that does not resolve still truncates the file first, because the
    // open happened at parse time.
    std::fs::write(&out_path, "stale\n").unwrap();
    let (_, _, code) = run(
        &repo,
        &home,
        &["range-diff", "--no-color", &out_arg, "nope...alsonope"],
    );
    assert_eq!(code, 255);
    assert_eq!(std::fs::read_to_string(&out_path).unwrap(), "");

    // An uncreatable path is `xfopen()`'s die, and it beats even the
    // `diff_setup_done()` refusals that otherwise run first.
    let missing = root.join("nodir").join("x");
    let bad = format!("--output={}", missing.display());
    let (_, err, code) = run(
        &repo,
        &home,
        &["range-diff", &bad, "--name-only", "--check", "v1..main", "v1..feature"],
    );
    assert_eq!(code, 128);
    assert_eq!(
        err,
        format!(
            "fatal: could not open '{}' for writing: No such file or directory\n",
            missing.display()
        )
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `--max-memory=<size>` is checked in `get_correspondences()` (range-diff.c:335)
/// against `sizeof(int) * (a->nr + b->nr)^2`, *before* any pairing is computed —
/// so it fires even on a range where one side is empty and nothing could match.
/// The message renders each figure twice, once through
/// `strbuf_humanise_bytes()` and once as a plain byte count.
#[test]
fn max_memory_refuses_an_oversized_cost_matrix() {
    let root = scratch("maxmem");
    let (repo, home) = fixture_pairs(&root);

    let fatal = |args: &[&str]| -> (String, i32) {
        let mut argv = vec!["range-diff", "--no-color"];
        argv.extend_from_slice(args);
        let (out, err, code) = run(&repo, &home, &argv);
        assert_eq!(out, "", "{args:?} wrote to stdout");
        (err, code)
    };

    // 2 + 2 patches: 4*4 ints = 64 bytes, and the test is `>=`, so 64 is already
    // too small and 65 is enough. `1` renders singular.
    let needed = "range-diff: unable to compute the range-diff, since it exceeds the maximum \
                  memory for the cost matrix: 64 bytes (64 bytes) needed, limited to";
    for (limit, tail) in [
        ("1", "1 byte (1 bytes)"),
        ("0", "0 bytes (0 bytes)"),
        ("64", "64 bytes (64 bytes)"),
    ] {
        let arg = format!("--max-memory={limit}");
        let (err, code) = fatal(&[&arg, "v1..main", "v1..feature"]);
        assert_eq!(code, 128, "limit {limit}");
        assert_eq!(err, format!("fatal: {needed} {tail}\n"), "limit {limit}");
    }
    let (_, err, code) = run(
        &repo,
        &home,
        &["range-diff", "--no-color", "--max-memory=65", "v1..main", "v1..feature"],
    );
    assert_eq!(code, 0, "stderr: {err}");

    // One side empty, so no commit can pair — but `n` is still 2 and the check
    // runs before `output()`, so it dies anyway.
    let (err, code) = fatal(&["--max-memory=1", "v1...main"]);
    assert_eq!(code, 128);
    assert!(
        err.contains("16 bytes (16 bytes) needed, limited to 1 byte (1 bytes)"),
        "{err}"
    );

    // `parse_max_memory()` returns 0 without touching the value when unset, so
    // `--no-max-memory` does *not* restore the 4G default.
    let (err, code) = fatal(&["--max-memory=1", "--no-max-memory", "v1..main", "v1..feature"]);
    assert_eq!(code, 128);
    assert!(err.contains("limited to 1 byte (1 bytes)"), "{err}");

    // A detached value is consumed, and a k/m/g magnitude is accepted.
    let (_, err, code) = run(
        &repo,
        &home,
        &["range-diff", "--no-color", "--max-memory", "1k", "v1..main", "v1..feature"],
    );
    assert_eq!(code, 0, "stderr: {err}");

    // `git_parse_unsigned()`'s single failure message, reported at parse time.
    for bad in ["abc", "", "-1", "12x"] {
        let arg = format!("--max-memory={bad}");
        let (err, code) = fatal(&[&arg, "v1..main", "v1..feature"]);
        assert_eq!(code, 129, "value {bad:?}");
        assert_eq!(err, format!("error: invalid max-memory value: {bad}\n"));
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// The KiB arm of `strbuf_humanise_bytes()`: nine patches a side make the matrix
/// 18*18*4 = 1296 bytes, which renders as `1.27 KiB` with git's truncating
/// fraction arithmetic. A byte count alone would not catch a wrong divisor.
#[test]
fn max_memory_humanises_a_kib_sized_matrix() {
    let root = scratch("maxmemkib");
    let (repo, home) = fixture_nine(&root);

    let (out, err, code) = run(
        &repo,
        &home,
        &["range-diff", "--no-color", "--max-memory=1100", "v1..main", "v1..feature"],
    );
    assert_eq!(code, 128);
    assert_eq!(out, "");
    assert_eq!(
        err,
        "fatal: range-diff: unable to compute the range-diff, since it exceeds the maximum \
         memory for the cost matrix: 1.27 KiB (1296 bytes) needed, limited to 1.07 KiB \
         (1100 bytes)\n"
    );

    // Just above the requirement the run succeeds, which is what proves 1296 is
    // the number being compared rather than a coincidence.
    let (out, err, code) = run(
        &repo,
        &home,
        &["range-diff", "--no-color", "-s", "--max-memory=1297", "v1..main", "v1..feature"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.lines().count(), 18);

    let _ = std::fs::remove_dir_all(&root);
}

/// `--follow` sets `flags.follow_renames`, and `diff_setup_done()` hands
/// `diffopt.pathspec` to `diff_check_follow_pathspec()` (diff.c:5364-5365).
/// Range-diff routes a trailing `-- <path>` to `log_arg` instead of that
/// pathspec (builtin/range-diff.c:128/148/179), so `ps->nr` is always 0 and the
/// `ps->nr != 1` die (diff.c:5223-5226) always fires — before any revision is
/// resolved, with or without a pathspec, whatever the range says.
#[test]
fn follow_always_dies_before_the_ranges_are_looked_at() {
    let root = scratch("follow");
    let (repo, home) = fixture_pairs(&root);

    for args in [
        &["range-diff", "--follow", "v1..main", "v1..feature"][..],
        &["range-diff", "--follow", "v1..main", "v1..feature", "--", "f"][..],
        &["range-diff", "--follow", "v1...main"][..],
        // A range that names nothing, and an argument shape that is a usage
        // error: `diff_setup_done()` still runs first.
        &["range-diff", "--follow", "nope..nope2", "x..y"][..],
        &["range-diff", "--follow"][..],
        // `--left-only --right-only` is the 255 refusal, and it also loses.
        &["range-diff", "--follow", "--left-only", "--right-only", "v1..main", "v1..feature"][..],
    ] {
        let (out, err, code) = run(&repo, &home, args);
        assert_eq!(code, 128, "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(err, "fatal: --follow requires exactly one pathspec\n", "{args:?}");
    }

    // `--no-follow` clears the flag, so a later run is ordinary.
    let (_, err, code) = run(
        &repo,
        &home,
        &["range-diff", "--no-color", "--follow", "--no-follow", "v1..main", "v1..feature"],
    );
    assert_eq!(code, 0, "stderr: {err}");

    let _ = std::fs::remove_dir_all(&root);
}

/// The three `pickaxe_opts` refusals `diff_setup_done()` raises in order
/// (diff.c:5263-5273), each ahead of the argument-shape and range checks and
/// each behind the output-format one. The bits are OR'd and never cleared, so
/// repeating the *same* option is not a conflict.
#[test]
fn pickaxe_combinations_keep_gits_message_and_ordering() {
    let root = scratch("pickaxe");
    let (repo, home) = fixture_pairs(&root);
    let (head, _, _) = run(&repo, &home, &["rev-parse", "HEAD"]);
    let find_object = format!("--find-object={}", head.trim());

    const KINDS: &str = "fatal: options '-G', '-S', and '--find-object' cannot be used together\n";
    const G_REGEX: &str = "fatal: options '-G' and '--pickaxe-regex' cannot be used together, \
                           use '--pickaxe-regex' with '-S'\n";
    const ALL_OBJFIND: &str = "fatal: options '--pickaxe-all' and '--find-object' cannot be used \
                               together, use '--pickaxe-all' with '-G' and '-S'\n";

    for (args, expected) in [
        (vec!["-S", "x", "-G", "y"], KINDS),
        // The same options with their value attached, which is the spelling that
        // used to slip past the bit tracking and reach the honesty guard instead.
        (vec!["-Sx", "-Gy"], KINDS),
        (vec!["-S", "x", "-Gy"], KINDS),
        (vec!["-Sx", &find_object], KINDS),
        (vec!["-Gy", "--pickaxe-regex"], G_REGEX),
        (vec!["-S", "x", &find_object], KINDS),
        (vec!["-G", "y", &find_object], KINDS),
        (vec!["-G", "x", "--pickaxe-regex"], G_REGEX),
        (vec!["--pickaxe-all", &find_object], ALL_OBJFIND),
        // All three conflicts at once: the kinds one is tested first.
        (
            vec!["-G", "x", "--pickaxe-regex", "--pickaxe-all", &find_object],
            KINDS,
        ),
        // The output-format refusal still outranks all of them.
        (
            vec!["--name-only", "--check", "-S", "x", "-G", "y"],
            "fatal: options '--name-only', '--name-status', '--check', and '-s' \
             cannot be used together\n",
        ),
    ] {
        let mut argv = vec!["range-diff"];
        argv.extend(args.iter().copied());
        argv.extend_from_slice(&["v1..main", "v1..feature"]);
        let (out, err, code) = run(&repo, &home, &argv);
        assert_eq!(code, 128, "{argv:?}");
        assert_eq!(out, "", "{argv:?}");
        assert_eq!(err, expected, "{argv:?}");
    }

    // The same kind twice is one bit, so it is no conflict — it reaches the
    // honesty guard instead, because `-S` itself is not implemented.
    for args in [
        vec!["-S", "x", "-S", "y"],
        vec!["-G", "x", "-G", "y"],
        vec!["-S", "x", "--pickaxe-regex"],
    ] {
        let mut argv = vec!["range-diff"];
        argv.extend(args.iter().copied());
        argv.extend_from_slice(&["v1..main", "v1..feature"]);
        let (_, err, code) = run(&repo, &home, &argv);
        assert_eq!(code, 128, "{argv:?}");
        assert!(err.starts_with("fatal: unsupported flag"), "{argv:?}: {err}");
    }

    // `diff_opt_find_object()` resolves its value before it sets the bit
    // (diff.c:5531-5537), so an unresolvable one is a parse-time 129 and no
    // conflict is reported even though `-S` is also present.
    for argv in [
        vec!["range-diff", "--find-object=zzz", "v1..main", "v1..feature"],
        vec!["range-diff", "--find-object=zzz", "-S", "x", "v1..main", "v1..feature"],
    ] {
        let (_, err, code) = run(&repo, &home, &argv);
        assert_eq!(code, 129, "{argv:?}");
        assert_eq!(err, "error: unable to resolve 'zzz'\n", "{argv:?}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// The four xdiff algorithms of the diff-of-diffs. The fixture is chosen so all
/// three distinct layouts differ, which is the only way an assertion here can
/// tell a wired-up flag from an ignored one: `--minimal` matches Myers on this
/// input, `--histogram` and `--patience` each rewrite the hunk.
///
/// `set_diff_algorithm()` clears the previous choice before setting the new one
/// (diff.c:3833-3835), so the last spelling on the command line wins.
#[test]
fn diff_algorithm_selects_the_layout_of_the_diff_of_diffs() {
    let root = scratch("algo");
    let (repo, home) = fixture_algorithms(&root);

    let rd = |args: &[&str]| {
        let mut argv = vec!["range-diff", "--no-color", "--creation-factor=999"];
        argv.extend_from_slice(args);
        argv.extend_from_slice(&["v1..main", "v1..feature"]);
        let (out, err, code) = run(&repo, &home, &argv);
        assert_eq!(code, 0, "{args:?} stderr: {err}");
        masked(&out)
    };

    const MYERS: &str = "\
1:  <id> ! 1:  <id> p1
    @@ Commit message
      ## f ##
     @@
      c
    --e
    - d
    + e
    +-d
      e
      d
    -@@ f: a
    - c
    - d
      a
    -+b
    +@@ f: d
    + a
      a
      c
    - e
    -@@ f: d
    +-e
    + d
      c
    - a
      d
    -+a
";
    const HISTOGRAM: &str = "\
1:  <id> ! 1:  <id> p1
    @@ Commit message
      ## f ##
     @@
      c
    + e
    +-d
    + e
    + d
    + a
    +@@ f: d
    + a
    + a
    + c
     -e
      d
    - e
    - d
    -@@ f: a
      c
      d
    - a
    -+b
    - a
    - c
    - e
    -@@ f: d
    - c
    - a
    - d
    -+a
";
    const PATIENCE: &str = "\
1:  <id> ! 1:  <id> p1
    @@ Commit message
      ## f ##
     @@
      c
    --e
    - d
      e
    - d
    -@@ f: a
    - c
    +-d
    + e
      d
      a
    -+b
    - a
    - c
    - e
     @@ f: d
    - c
      a
    + a
    + c
    +-e
    + d
    + c
      d
    -+a
";

    assert_eq!(rd(&[]), MYERS, "default");
    assert_eq!(rd(&["--diff-algorithm=myers"]), MYERS);
    assert_eq!(rd(&["--diff-algorithm=default"]), MYERS);
    assert_eq!(rd(&["--minimal"]), MYERS);
    assert_eq!(rd(&["--diff-algorithm=minimal"]), MYERS);
    assert_eq!(rd(&["--histogram"]), HISTOGRAM);
    assert_eq!(rd(&["--diff-algorithm=histogram"]), HISTOGRAM);
    // `parse_algorithm_value()` is a `strcasecmp` (diff.c:220-236).
    assert_eq!(rd(&["--diff-algorithm=HISTOGRAM"]), HISTOGRAM);
    // A detached value is consumed rather than read as a revision.
    assert_eq!(rd(&["--diff-algorithm", "histogram"]), HISTOGRAM);
    assert_eq!(rd(&["--patience"]), PATIENCE);
    assert_eq!(rd(&["--diff-algorithm=patience"]), PATIENCE);
    // Last one wins, in both directions.
    assert_eq!(rd(&["--histogram", "--patience"]), PATIENCE);
    assert_eq!(rd(&["--patience", "--diff-algorithm=myers"]), MYERS);

    let _ = std::fs::remove_dir_all(&root);
}

/// `XDF_INDENT_HEURISTIC` is on by default here, so only `--no-indent-heuristic`
/// moves anything — and it moves a sliding hunk, which is exactly what the
/// heuristic is for.
#[test]
fn indent_heuristic_places_a_sliding_hunk() {
    let root = scratch("indent");
    let (repo, home) = fixture_indent(&root);

    let rd = |args: &[&str]| {
        let mut argv = vec!["range-diff", "--no-color", "--creation-factor=999"];
        argv.extend_from_slice(args);
        argv.extend_from_slice(&["v1..main", "v1..feature"]);
        let (out, err, code) = run(&repo, &home, &argv);
        assert_eq!(code, 0, "{args:?} stderr: {err}");
        masked(&out)
    };

    const ON: &str = "\
1:  <id> ! 1:  <id> p1
    @@ Commit message
    \x20
      ## f ##
     @@
    -+d
      a
    -+b
    - d
     +c
    + d
      d
      d
      b
    +-e
    + c
    + a
    + c
";
    const OFF: &str = "\
1:  <id> ! 1:  <id> p1
    @@ Commit message
    \x20
      ## f ##
     @@
    -+d
      a
    -+b
    - d
     +c
      d
      d
    + d
      b
    +-e
    + c
    + a
    + c
";

    assert_eq!(rd(&[]), ON, "default");
    assert_eq!(rd(&["--indent-heuristic"]), ON);
    assert_eq!(rd(&["--no-indent-heuristic"]), OFF);
    // Last one wins.
    assert_eq!(rd(&["--no-indent-heuristic", "--indent-heuristic"]), ON);

    let _ = std::fs::remove_dir_all(&root);
}

/// `-U<n>` / `--unified=<n>` is `diffopt.context` for the diff-of-diffs, and
/// `diff_opt_unified()` only assigns when a value came with the option
/// (`if (arg)`, diff.c:5953) — the option is `PARSE_OPT_OPTARG`, so a bare `-U`
/// keeps the default 3 and never eats the next argv element.
///
/// Shrinking the context also moves the `@@ <section>` names, because
/// `get_func_line()` searches backwards from each hunk's new start.
#[test]
fn unified_sets_the_context_of_the_diff_of_diffs() {
    let root = scratch("unified");
    let (repo, home) = fixture_pairs(&root);

    let rd = |args: &[&str]| {
        let mut argv = vec!["range-diff", "--no-color"];
        argv.extend_from_slice(args);
        argv.extend_from_slice(&["v1..main", "v1..feature"]);
        let (out, err, code) = run(&repo, &home, &argv);
        assert_eq!(code, 0, "{args:?} stderr: {err}");
        masked(&out)
    };

    const U1: &str = "\
1:  <id> ! 1:  <id> patch one
    @@ Commit message
      ## Notes ##
    -    default note L
    +    default note R
    \x20
    @@ f
     -c
    -+CHANGED
    ++CHANGED2
      d
2:  <id> ! 2:  <id> patch two
    @@ f: d
     -h
    -+ZZZ
    ++YYY
";
    const U0: &str = "\
1:  <id> ! 1:  <id> patch one
    @@ Notes
    -    default note L
    +    default note R
    @@ f
    -+CHANGED
    ++CHANGED2
2:  <id> ! 2:  <id> patch two
    @@ f: d
    -+ZZZ
    ++YYY
";

    assert_eq!(rd(&["-U1"]), U1);
    assert_eq!(rd(&["--unified=1"]), U1);
    assert_eq!(rd(&["-U0"]), U0);
    assert_eq!(rd(&["--unified=0"]), U0);
    // A bare `-U` / `--unified` leaves the default alone.
    assert_eq!(rd(&["-U"]), PAIRS_BASELINE);
    assert_eq!(rd(&["--unified"]), PAIRS_BASELINE);
    assert_eq!(rd(&["-U3"]), PAIRS_BASELINE);

    let _ = std::fs::remove_dir_all(&root);
}

/// The three `--output-indicator-*` markers rewrite the `+`, `-` and ` ` column
/// of the diff-of-diffs — never the inner patch text those lines carry, and
/// never the `@@` hunk header. `diff_opt_char()` stores `arg[0]`, so an empty
/// value stores NUL and `emit_line_0()`'s `if (first)` (diff.c:786-787) drops
/// the column entirely.
#[test]
fn output_indicators_rewrite_the_marker_column() {
    let root = scratch("indicators");
    let (repo, home) = fixture_pairs(&root);

    let (out, err, code) = run(
        &repo,
        &home,
        &[
            "range-diff",
            "--no-color",
            "--output-indicator-new=%",
            "--output-indicator-old=~",
            "--output-indicator-context=.",
            "v1..main",
            "v1..feature",
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        masked(&out),
        "\
1:  <id> ! 1:  <id> patch one
    @@ Commit message
    .
    .
    . ## Notes ##
    ~    default note L
    %    default note R
    .
    . ## f ##
    .@@
    . a
    . b
    .-c
    ~+CHANGED
    %+CHANGED2
    . d
    . e
    . f
2:  <id> ! 2:  <id> patch two
    @@ f: d
    . f
    . g
    .-h
    ~+ZZZ
    %+YYY
"
    );

    // An empty value removes the column; the four-space indent stays.
    let (out, err, code) = run(
        &repo,
        &home,
        &[
            "range-diff",
            "--no-color",
            "--output-indicator-context=",
            "v1..main",
            "v1..feature",
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("\n    \n    \n     ## Notes ##\n"), "{out}");
    assert!(out.contains("\n     a\n     b\n    -c\n"), "{out}");

    // A detached value is consumed, and more than one byte is refused at parse
    // time by `diff_opt_char()`.
    let (out, err, code) = run(
        &repo,
        &home,
        &["range-diff", "--no-color", "--output-indicator-new", "%", "v1..main", "v1..feature"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("    %+CHANGED2\n"), "{out}");

    let (_, err, code) = run(
        &repo,
        &home,
        &["range-diff", "--output-indicator-new=ab", "v1..main", "v1..feature"],
    );
    assert_eq!(code, 129);
    assert_eq!(err, "error: output-indicator-new expects a character, got 'ab'\n");

    let _ = std::fs::remove_dir_all(&root);
}

/// The honesty guard for the options this port does *not* render: a deferred
/// option can only be seen through `patch_diff()`, which `output()` calls solely
/// for a matched pair (range-diff.c:567-573). So a run with no matched pair at
/// all emits its header-only page exactly as upstream does, and only a run that
/// would render a body stops.
#[test]
fn a_deferred_option_only_stops_a_run_that_would_render_a_body() {
    let root = scratch("guard");
    let (repo, home) = fixture_pairs(&root);

    // Both ranges non-empty and both pairs matched: `--stat` would replace every
    // body, and this port does not render one, so it refuses rather than print a
    // page that ignored the flag.
    let (out, err, code) = run(
        &repo,
        &home,
        &["range-diff", "--no-color", "--stat", "v1..main", "v1..feature"],
    );
    assert_eq!(code, 128);
    assert_eq!(out, "");
    assert_eq!(err, "fatal: unsupported flag \"--stat\"\n");

    // One range empty: nothing can match, every commit is a bare `>` header, and
    // the flag provably cannot reach the page.
    let (out, err, code) = run(&repo, &home, &["range-diff", "--no-color", "--stat", "v1...main"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        masked(&out),
        "-:  ------- > 1:  <id> patch one\n-:  ------- > 2:  <id> patch two\n"
    );

    // Both ranges non-empty but disjoint — `--creation-factor=0` makes every
    // creation free, so nothing pairs up and there is still no body to render.
    let (out, err, code) = run(
        &repo,
        &home,
        &[
            "range-diff",
            "--no-color",
            "--creation-factor=0",
            "--stat",
            "v1..main",
            "v1..feature",
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(!out.contains(" ! "), "nothing should have paired up: {out}");
    assert_eq!(out.lines().count(), 4);

    let _ = std::fs::remove_dir_all(&root);
}
