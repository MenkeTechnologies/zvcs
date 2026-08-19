//! `git show` on a merge, and the diff options the history verbs used to refuse.
//!
//! Every expectation is a byte string measured from stock git 2.55.0; nothing here
//! shells out to a second git, so the suite runs on a headless Linux CI box with
//! only this binary present.
//!
//! Each case is pinned twice: against the bytes stock produces *and* against the
//! command's own default. The second assertion is the one that matters — a flag
//! that is merely accepted and then dropped exits 0 and prints the default, so
//! only `assert_ne!` against the default can tell "plumbed" from "swallowed". That
//! is the whole trap for `--diff-merges` in particular: the ordinary fixture has no
//! merge commit, so every mode of it looks correct there whatever the code does.
//!
//! The shapes under test:
//!
//!   * `show_setup_revisions_tweak()` (builtin/log.c:651-659) defaults a merge to
//!     `dense-combined`, and to `first-parent` under `--first-parent` — but
//!     `diff_merges_default_to_first_parent()` also *upgrades* an explicit
//!     `separate` to `first-parent`, which is why the two orders differ;
//!   * `separate`/`-m` repeat the whole record once per parent with `show_log()`'s
//!     ` (from <oid>)` insert, `off` prints the header alone, and `combined` and
//!     `dense-combined` differ only in the section header and the hunk pass;
//!   * `diff_tree_combined()` (combine-diff.c:1600-1610) runs a different block
//!     order from `diff_flush()`: the count formats come first, against the *first
//!     parent*, and the raw block is the combined one;
//!   * `--check` is `DIFF_FORMAT_CHECKDIFF`, which clears every other format and
//!     reports through the `02` bit of `diff_result_code()`;
//!   * `DIFF_SYMBOL_SEPARATOR` is `o->line_termination` (diff.c:1436-1440), so
//!     `-z` separates blocks with a NUL rather than a blank line;
//!   * `cmd_show`'s `case OBJ_TAG:` (builtin/log.c:711-731) renders a tag object
//!     `--all` pended, which only happens because `handle_one_ref()` pends the ref's
//!     target *unpeeled*;
//!   * `handle_revision_arg_1()`'s `^-<n>` mark (revision.c:2192-2207), which the
//!     revision parser has no case for at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn cmd(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        // This machine's own `~/.gitconfig` sets `core.commentChar`; pin all four so
        // the run reads nothing but the repository's config.
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("COLUMNS", "80")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap()
}

fn run(repo: &Path, home: &Path, args: &[&str]) {
    let o = cmd(repo, home, args);
    assert!(o.status.success(), "git {args:?} failed: {}", err(&o));
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// A history whose tip is an ordinary commit and whose `HEAD~1` is a merge that
/// resolved a conflict in `f` and took both sides in `g` — so the merge differs
/// from *both* parents in both paths and the combined diff has two sections. The
/// tip renames `g` to `h`, adds a trailing-whitespace line to it and rewrites a
/// line of `f`, which is what `--check`, `-S` and the rename formats need.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-showmerge-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "a\nb\nc\n").unwrap();
    std::fs::write(repo.join("g"), "s\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "base"]);
    run(&repo, &home, &["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.join("f"), "a\nSIDE\nc\n").unwrap();
    std::fs::write(repo.join("g"), "s\nside\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "side"]);
    run(&repo, &home, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f"), "a\nMAIN\nc\n").unwrap();
    std::fs::write(repo.join("g"), "s\nmain\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "main2"]);
    // The merge conflicts in both paths; the resolution is what makes the combined
    // diff differ from every parent.
    let _ = cmd(&repo, &home, &["merge", "side"]);
    std::fs::write(repo.join("f"), "a\nMERGED\nc\n").unwrap();
    std::fs::write(repo.join("g"), "s\nside\nmain\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "merge side"]);
    // The tip: a rename that keeps 63% of the file, a trailing-whitespace line and
    // an unrelated edit, so `-S` has something to filter and `--check` something to
    // report.
    run(&repo, &home, &["mv", "g", "h"]);
    std::fs::write(repo.join("h"), "s\nside\nmain\ntrail \n").unwrap();
    std::fs::write(repo.join("f"), "a\nTIP\nc\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "tip"]);
    run(&repo, &home, &["tag", "-a", "-m", "tag body", "v1", "HEAD"]);
    (repo, home)
}

/// `show` with `--format=%s` on the merge, so nothing here depends on a commit id.
fn merge(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["show", "--no-decorate", "--format=%s"];
    args.extend_from_slice(extra);
    args.push("HEAD~1");
    cmd(repo, home, &args)
}

const COMBINED_BODY: &str = concat!(
    "index af70335,f794161..121cba9\n",
    "--- a/f\n",
    "+++ b/f\n",
    "@@@ -1,3 -1,3 +1,3 @@@\n",
    "  a\n",
    "- MAIN\n",
    " -SIDE\n",
    "++MERGED\n",
    "  c\n",
);

const DENSE_COMBINED: &str = concat!(
    "merge side\n",
    "\n",
    "diff --cc f\n",
    "index af70335,f794161..121cba9\n",
    "--- a/f\n",
    "+++ b/f\n",
    "@@@ -1,3 -1,3 +1,3 @@@\n",
    "  a\n",
    "- MAIN\n",
    " -SIDE\n",
    "++MERGED\n",
    "  c\n",
    "diff --cc g\n",
    "index 6b4735d,3562fac..8078138\n",
    "--- a/g\n",
    "+++ b/g\n",
    "@@@ -1,2 -1,2 +1,3 @@@\n",
    "  s\n",
    "+ side\n",
    " +main\n",
);

/// The two-way patch against the first parent, which is what `first-parent`/`--dd`
/// and the first record of `separate`/`-m` show.
const FIRST_PARENT_PATCH: &str = concat!(
    "merge side\n",
    "\n",
    "diff --git a/f b/f\n",
    "index af70335..121cba9 100644\n",
    "--- a/f\n",
    "+++ b/f\n",
    "@@ -1,3 +1,3 @@\n",
    " a\n",
    "-MAIN\n",
    "+MERGED\n",
    " c\n",
    "diff --git a/g b/g\n",
    "index 6b4735d..8078138 100644\n",
    "--- a/g\n",
    "+++ b/g\n",
    "@@ -1,2 +1,3 @@\n",
    " s\n",
    "+side\n",
    " main\n",
);

const SECOND_PARENT_PATCH: &str = concat!(
    "merge side\n",
    "\n",
    "diff --git a/f b/f\n",
    "index f794161..121cba9 100644\n",
    "--- a/f\n",
    "+++ b/f\n",
    "@@ -1,3 +1,3 @@\n",
    " a\n",
    "-SIDE\n",
    "+MERGED\n",
    " c\n",
    "diff --git a/g b/g\n",
    "index 3562fac..8078138 100644\n",
    "--- a/g\n",
    "+++ b/g\n",
    "@@ -1,2 +1,3 @@\n",
    " s\n",
    " side\n",
    "+main\n",
);

#[test]
fn diff_merges_modes_render_the_merge_git_renders() {
    let (repo, home) = fixture("modes");

    // The default is `dense-combined`, and `--cc` and `--diff-merges=dense-combined`
    // are the same thing spelled twice.
    let default = merge(&repo, &home, &[]);
    assert!(default.status.success(), "{}", err(&default));
    assert_eq!(out(&default), DENSE_COMBINED);
    assert_eq!(out(&merge(&repo, &home, &["--cc"])), DENSE_COMBINED);
    assert_eq!(
        out(&merge(&repo, &home, &["--diff-merges=dense-combined"])),
        DENSE_COMBINED
    );
    assert_eq!(out(&merge(&repo, &home, &["--diff-merges=cc"])), DENSE_COMBINED);

    // `-c` is the same body under a `diff --combined` header
    // (`show_combined_header()`, combine-diff.c:944).
    let combined = out(&merge(&repo, &home, &["-c"]));
    assert_eq!(combined, DENSE_COMBINED.replace("diff --cc ", "diff --combined "));
    assert!(combined.contains(COMBINED_BODY), "{combined}");
    assert_ne!(combined, DENSE_COMBINED, "`-c` must not print the dense header");
    assert_eq!(out(&merge(&repo, &home, &["--diff-merges=combined"])), combined);

    // `off`/`none`/`--no-diff-merges` leave `log_tree_diff()` before it queues
    // anything, so `log_tree_commit()`'s `always_show_header` prints the header
    // alone — no patch, and no `--stat` either.
    assert_eq!(out(&merge(&repo, &home, &["--diff-merges=off"])), "merge side\n");
    assert_eq!(out(&merge(&repo, &home, &["--diff-merges=none"])), "merge side\n");
    assert_eq!(out(&merge(&repo, &home, &["--no-diff-merges"])), "merge side\n");
    assert_eq!(
        out(&merge(&repo, &home, &["--diff-merges=off", "--stat"])),
        "merge side\n",
        "`off` suppresses every format, not just the patch"
    );
    assert_ne!(out(&merge(&repo, &home, &["--diff-merges=off"])), DENSE_COMBINED);

    // `first-parent`, `1` and `--dd` are the ordinary two-way patch against
    // `parents[0]`, with no ` (from <oid>)` insert.
    assert_eq!(
        out(&merge(&repo, &home, &["--diff-merges=first-parent"])),
        FIRST_PARENT_PATCH
    );
    assert_eq!(out(&merge(&repo, &home, &["--diff-merges=1"])), FIRST_PARENT_PATCH);
    assert_eq!(out(&merge(&repo, &home, &["--dd"])), FIRST_PARENT_PATCH);
    assert_eq!(out(&merge(&repo, &home, &["--first-parent"])), FIRST_PARENT_PATCH);
    assert_ne!(out(&merge(&repo, &home, &["--dd"])), DENSE_COMBINED);

    // `separate`/`-m`/`on`/`m` repeat the record once per parent.
    let separate = format!("{FIRST_PARENT_PATCH}{SECOND_PARENT_PATCH}");
    assert_eq!(out(&merge(&repo, &home, &["--diff-merges=separate"])), separate);
    assert_eq!(out(&merge(&repo, &home, &["-m"])), separate);
    assert_eq!(out(&merge(&repo, &home, &["--diff-merges=on"])), separate);
    assert_eq!(out(&merge(&repo, &home, &["--diff-merges=m"])), separate);
    assert_ne!(out(&merge(&repo, &home, &["-m"])), FIRST_PARENT_PATCH);

    // `set_diff_merges()`'s die (diff-merges.c:94).
    let bogus = merge(&repo, &home, &["--diff-merges=bogus"]);
    assert_eq!(code(&bogus), 128);
    assert_eq!(err(&bogus), "fatal: invalid value for '--diff-merges': 'bogus'\n");
}

/// `show_log()`'s ` (from %s)` insert (log-tree.c:824-826), printed at the header's
/// own abbreviation width — full length under `medium`, which is what `git show`
/// renders by default.
#[test]
fn separate_records_carry_the_from_insert() {
    let (repo, home) = fixture("from");
    let p1 = out(&cmd(&repo, &home, &["rev-parse", "HEAD~1^1"])).trim_end().to_string();
    let p2 = out(&cmd(&repo, &home, &["rev-parse", "HEAD~1^2"])).trim_end().to_string();
    let merge_id = out(&cmd(&repo, &home, &["rev-parse", "HEAD~1"])).trim_end().to_string();

    let o = cmd(&repo, &home, &["show", "--no-decorate", "-s", "-m", "HEAD~1"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains(&format!("commit {merge_id} (from {p1})\n")), "{text}");
    assert!(text.contains(&format!("commit {merge_id} (from {p2})\n")), "{text}");

    // A non-merge never gets one, whatever the mode says.
    let tip = out(&cmd(&repo, &home, &["show", "--no-decorate", "-s", "-m", "HEAD"]));
    assert!(!tip.contains("(from "), "{tip}");
    // Nor does the combined form, which prints one record for the whole merge.
    let cc = out(&cmd(&repo, &home, &["show", "--no-decorate", "-s", "--cc", "HEAD~1"]));
    assert!(!cc.contains("(from "), "{cc}");
}

/// `show_setup_revisions_tweak()` (builtin/log.c:651-659) plus
/// `diff_merges_default_to_first_parent()` (diff-merges.c:158-164): the second
/// upgrades `separate` to `first-parent`, so the two orders below are not
/// symmetric.
#[test]
fn first_parent_upgrades_separate_but_yields_to_combined() {
    let (repo, home) = fixture("tweak");

    // An explicit `separate` beside `--first-parent` becomes `first-parent`,
    // whichever order they are written in.
    assert_eq!(
        out(&merge(&repo, &home, &["--diff-merges=separate", "--first-parent"])),
        FIRST_PARENT_PATCH
    );
    assert_eq!(
        out(&merge(&repo, &home, &["--first-parent", "--diff-merges=separate"])),
        FIRST_PARENT_PATCH
    );

    // An explicit `combined` is left alone: `diff_merges_default_to_first_parent()`
    // only touches `separate_merges`.
    let combined = DENSE_COMBINED.replace("diff --cc ", "diff --combined ");
    assert_eq!(
        out(&merge(&repo, &home, &["--first-parent", "--diff-merges=combined"])),
        combined
    );
    assert_eq!(
        out(&merge(&repo, &home, &["--diff-merges=combined", "--first-parent"])),
        combined
    );

    // And `off` stays off — `set_none()` clears `separate_merges` too.
    assert_eq!(
        out(&merge(&repo, &home, &["--first-parent", "--diff-merges=off"])),
        "merge side\n"
    );
}

/// `diff_tree_combined()` (combine-diff.c:1600-1610): the `STAT_FORMAT_MASK`
/// formats are written by `find_paths_generic()`'s first pass, against the *first
/// parent*, and therefore precede the combined raw block rather than following it
/// as `diff_flush()`'s order would.
#[test]
fn combined_merges_reorder_the_format_blocks() {
    let (repo, home) = fixture("blocks");

    // `--raw` under a combined mode is `show_raw_diff()`'s record: one colon and
    // one mode per parent, then the result's, then one status letter per parent.
    assert_eq!(
        out(&merge(&repo, &home, &["--cc", "--raw"])),
        concat!(
            "merge side\n",
            "\n",
            "::100644 100644 100644 af70335 f794161 121cba9 MM\tf\n",
            "::100644 100644 100644 6b4735d 3562fac 8078138 MM\tg\n",
        ),
    );
    // The two-way raw listing is what `first-parent` prints, and it is different —
    // one colon, two modes, one letter.
    let two_way = out(&merge(&repo, &home, &["--dd", "--raw"]));
    assert!(two_way.contains(":100644 100644 af70335 121cba9 M\tf\n"), "{two_way}");
    assert_ne!(out(&merge(&repo, &home, &["--cc", "--raw"])), two_way);

    // The stat block comes first and is measured against the first parent only.
    assert_eq!(
        out(&merge(&repo, &home, &["--cc", "--stat", "-p"])),
        format!(
            concat!(
                "merge side\n",
                "\n",
                " f | 2 +-\n",
                " g | 1 +\n",
                " 2 files changed, 2 insertions(+), 1 deletion(-)\n",
                "\n",
                "{}",
            ),
            DENSE_COMBINED.trim_start_matches("merge side\n\n"),
        ),
    );
}

/// `--check` is `DIFF_FORMAT_CHECKDIFF`: it clears every other output format
/// (`diff_setup_done()`) and reports through `diff_result_code()`'s `02` bit.
#[test]
fn check_replaces_every_format_and_sets_the_status() {
    let (repo, home) = fixture("check");
    const REPORT: &str = "tip\n\nh:4: trailing whitespace.\n+trail \n";

    for verb in [
        vec!["show", "--no-decorate", "--format=%s", "--check", "HEAD"],
        vec!["show", "--no-decorate", "--format=%s", "--check", "--stat", "HEAD"],
        vec!["show", "--no-decorate", "--format=%s", "--check", "-p", "HEAD"],
    ] {
        let o = cmd(&repo, &home, &verb);
        assert_eq!(out(&o), REPORT, "{verb:?}");
        assert_eq!(code(&o), 2, "{verb:?}");
    }
    // Not the default output — the guard against a swallowed flag.
    let plain = cmd(&repo, &home, &["show", "--no-decorate", "--format=%s", "HEAD"]);
    assert_ne!(out(&plain), REPORT);
    assert_eq!(code(&plain), 0);

    // `git log --check` reports every commit in the walk, and the status is the
    // whole run's.
    let log = cmd(&repo, &home, &["log", "--no-decorate", "--format=%s", "--check"]);
    assert_eq!(code(&log), 2);
    assert!(out(&log).starts_with(REPORT), "{}", out(&log));

    // A commit with no whitespace error prints its header and exits 0 — the
    // separator is still there, because the pair queue was not empty.
    let clean = cmd(&repo, &home, &["show", "--no-decorate", "--format=%s", "--check", "HEAD~2"]);
    assert_eq!(code(&clean), 0);
    assert_eq!(out(&clean), "main2\n\n");

    // `diff_tree_combined()` never looks at `DIFF_FORMAT_CHECKDIFF`, so a merge
    // under `-c`/`--cc` reports nothing at all.
    let merge_check = merge(&repo, &home, &["--check"]);
    assert_eq!(out(&merge_check), "merge side\n\n");
    assert_eq!(code(&merge_check), 0);

    // `-s` *assigns* `DIFF_FORMAT_NO_OUTPUT`, clearing the `CHECKDIFF` bit set
    // before it; written the other way round both bits stand and
    // `diff_setup_done()` dies.
    let after = cmd(&repo, &home, &["show", "--no-decorate", "--format=%s", "--check", "-s", "HEAD"]);
    assert_eq!(code(&after), 0);
    assert_eq!(out(&after), "tip\n");
    let before = cmd(&repo, &home, &["log", "--no-decorate", "-s", "--check"]);
    assert_eq!(code(&before), 128);
    assert_eq!(
        err(&before),
        "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together\n"
    );
    // `--check` is `PARSE_OPT_NONEG`, so there is no `--no-check`.
    assert_ne!(code(&cmd(&repo, &home, &["show", "--no-check", "HEAD"])), 0);
}

/// `--exit-code` (`o->flags.exit_with_status`): `diff_result_code()`'s `01` bit,
/// and `log_tree_diff()`'s `all_need_diff` on its own — so the queue is built even
/// with no output format asking for one.
#[test]
fn exit_code_reports_changes_without_printing_them() {
    let (repo, home) = fixture("exit");

    // `-s` prints nothing and still reports 1.
    let o = cmd(&repo, &home, &["show", "--no-decorate", "--format=%s", "--exit-code", "-s", "HEAD"]);
    assert_eq!(out(&o), "tip\n");
    assert_eq!(code(&o), 1);
    assert_eq!(code(&cmd(&repo, &home, &["show", "--no-decorate", "-s", "HEAD"])), 0);

    // A pathspec that matches nothing leaves the queue empty, so the status is 0.
    let empty = cmd(
        &repo,
        &home,
        &["show", "--no-decorate", "--format=%s", "--exit-code", "-s", "HEAD", "--", "nosuch"],
    );
    assert_eq!(code(&empty), 0);

    // `diff_tree_combined()` has no `has_changes` assignment at all, so a merge
    // under the default `--cc` reports 0 however much it changed — while the same
    // merge under `-m` reports 1.
    assert_eq!(code(&merge(&repo, &home, &["--exit-code", "-s"])), 0);
    assert_eq!(code(&merge(&repo, &home, &["-m", "--exit-code", "-s"])), 1);

    // `OPT_BOOL`, so the last spelling on the line wins.
    assert_eq!(
        code(&cmd(
            &repo,
            &home,
            &["show", "--no-decorate", "--exit-code", "--no-exit-code", "-s", "HEAD"]
        )),
        0
    );

    // `--check` reports through `check_failed` instead, which is why the two
    // together are 2 rather than 3.
    assert_eq!(
        code(&cmd(&repo, &home, &["show", "--no-decorate", "--check", "--exit-code", "HEAD"])),
        2
    );
}

/// `DIFF_SYMBOL_SEPARATOR` writes `o->line_termination` (diff.c:1436-1440), and
/// `show_numstat()` splits a rename into three NUL-terminated fields under `-z`
/// (diff.c:3261-3276).
#[test]
fn z_makes_the_block_separator_and_the_numstat_names_nul() {
    let (repo, home) = fixture("nul");

    assert_eq!(
        out(&cmd(&repo, &home, &["show", "--no-decorate", "--format=%s", "-z", "--numstat", "HEAD"])),
        "tip\0\n1\t1\tf\01\t0\t\0g\0h\0",
    );
    // Without `-z` the rename is one `<from> => <to>` field and the records end in
    // newlines — the assertion that says the flag was not swallowed.
    assert_eq!(
        out(&cmd(&repo, &home, &["show", "--no-decorate", "--format=%s", "--numstat", "HEAD"])),
        "tip\n\n1\t1\tf\n1\t0\tg => h\n",
    );

    // The raw block and the patch are separated by a NUL, not a blank line.
    let both = out(&cmd(
        &repo,
        &home,
        &["show", "--no-decorate", "--format=%s", "-z", "--raw", "-p", "HEAD"],
    ));
    assert!(
        both.contains("R063\0g\0h\0\0diff --git a/f b/f\n"),
        "the separator is `o->line_termination`: {both:?}"
    );
    assert!(!both.contains("h\0\ndiff --git"), "{both:?}");
}

/// `--line-prefix` is `diff_line_prefix()`, which `emit_line_0()` writes before
/// every emitted line — the header `show_log()` wrote included.
#[test]
fn line_prefix_reaches_the_header_too() {
    let (repo, home) = fixture("prefix");

    assert_eq!(
        out(&cmd(
            &repo,
            &home,
            &["show", "--no-decorate", "--format=%s", "--line-prefix=| ", "--stat", "HEAD"]
        )),
        concat!(
            "| tip\n",
            "| \n",
            "|  f      | 2 +-\n",
            "|  g => h | 1 +\n",
            "|  2 files changed, 2 insertions(+), 1 deletion(-)\n",
        ),
    );
    // `git log` prefixes every record of the walk the same way.
    let log = out(&cmd(&repo, &home, &["log", "--no-decorate", "--format=%s", "--line-prefix=| "]));
    assert!(log.lines().all(|l| l.starts_with("| ")), "{log}");
    assert_ne!(
        log,
        out(&cmd(&repo, &home, &["log", "--no-decorate", "--format=%s"]))
    );
}

/// `diffcore_pickaxe()`: `--pickaxe-all` keeps the whole queue when any pair
/// matched, and `--pickaxe-regex` promotes `-S`'s literal to a regular expression.
#[test]
fn pickaxe_all_and_regex_reach_show() {
    let (repo, home) = fixture("pickaxe");
    let names = |extra: &[&str]| {
        let mut args = vec!["show", "--no-decorate", "--format=%s", "--name-only"];
        args.extend_from_slice(extra);
        args.push("HEAD");
        out(&cmd(&repo, &home, &args))
    };

    // Only the file whose change text holds the needle.
    assert_eq!(names(&["-Strail"]), "tip\n\nh\n");
    // `--pickaxe-all` keeps the commit's whole queue once anything matched.
    assert_eq!(names(&["-Strail", "--pickaxe-all"]), "tip\n\nf\nh\n");
    assert_ne!(names(&["-Strail", "--pickaxe-all"]), names(&["-Strail"]));

    // A literal `-S` does not match `tr.il`; `--pickaxe-regex` makes it one.
    assert_eq!(names(&["-Str.il"]), "");
    assert_eq!(names(&["-Str.il", "--pickaxe-regex"]), "tip\n\nh\n");
    assert_ne!(names(&["-Str.il", "--pickaxe-regex"]), names(&["-Str.il"]));
}

/// `find_paths_generic()` (combine-diff.c:1378-1420) runs `diffcore_std()` — and so
/// `diffcore_pickaxe()` — against each parent in turn and intersects the surviving
/// path sets, so the pickaxe reaches a merge's combined sections too. The header is
/// printed before any path is scanned (combine-diff.c:1506-1516), so an emptied
/// merge still prints one.
#[test]
fn the_pickaxe_narrows_a_merges_combined_sections() {
    let (repo, home) = fixture("cpickaxe");

    // `MERGED` is added against both parents, so `f` survives the intersection —
    // and `g`, whose two sides each match one parent, does not.
    assert_eq!(out(&merge(&repo, &home, &["-SMERGED", "--name-only"])), "merge side\n\nf\n");
    assert_eq!(
        out(&merge(&repo, &home, &["-SMERGED", "--raw"])),
        concat!(
            "merge side\n",
            "\n",
            "::100644 100644 100644 af70335 f794161 121cba9 MM\tf\n",
        ),
    );
    assert_eq!(
        out(&merge(&repo, &home, &["-SMERGED"])),
        format!("merge side\n\ndiff --cc f\n{COMBINED_BODY}"),
    );
    // Not the unfiltered listing — the assertion that says the pickaxe reached the
    // combined path set rather than being dropped there.
    assert_ne!(out(&merge(&repo, &home, &["-SMERGED"])), DENSE_COMBINED);

    // `side` is on the second parent's side of `g`, so it hits against one parent
    // and not the other: the intersection is empty and only the header prints.
    assert_eq!(out(&merge(&repo, &home, &["-Sside", "--name-only"])), "merge side\n\n");
    assert_eq!(out(&merge(&repo, &home, &["-Snosuch", "--name-only"])), "merge side\n\n");

    // `--pickaxe-all` widens each parent's queue to all of it once anything matched.
    assert_eq!(
        out(&merge(&repo, &home, &["-SMERGED", "--pickaxe-all", "--name-only"])),
        "merge side\n\nf\ng\n",
    );
}

/// `handle_one_ref()` (revision.c:1625-1637) pends the object a ref *names*, with
/// no peeling, and `cmd_show`'s pending loop then reaches `case OBJ_TAG:`
/// (builtin/log.c:711-731) — which is the only reason `git show --all` renders an
/// annotated tag at all.
#[test]
fn show_all_renders_the_annotated_tag_object() {
    let (repo, home) = fixture("all");

    let o = cmd(&repo, &home, &["show", "--no-decorate", "--format=%s", "--all", "-s"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        out(&o),
        concat!("tip\n", "side\n", "\n", "tag v1\n", "Tagger: t <t@e.x>\n", "\n", "tag body\n"),
    );
    // Without the tag ref there is no tag block — the guard that says the block
    // above came from the ref and not from somewhere else.
    let branches = cmd(&repo, &home, &["show", "--no-decorate", "--format=%s", "--branches", "-s"]);
    assert_eq!(out(&branches), "tip\nside\n");
    assert_ne!(out(&branches), out(&o));
}

/// `handle_revision_arg_1()`'s `^-<n>` mark (revision.c:2192-2207): the named
/// parent is pended `UNINTERESTING` and the commit itself positive, so
/// `<merge>^-` is "the merge and everything the *second* parent adds". The
/// revision parser has no case for the mark, which is why decoding it has to come
/// first — without that, `git show <merge>^-` answered with one commit at exit 0.
#[test]
fn caret_dash_selects_one_parent_to_exclude() {
    let (repo, home) = fixture("caretdash");

    assert_eq!(
        out(&cmd(&repo, &home, &["log", "--no-decorate", "--format=%s", "HEAD~1^-"])),
        "merge side\nside\n",
    );
    assert_eq!(
        out(&cmd(&repo, &home, &["log", "--no-decorate", "--format=%s", "HEAD~1^-1"])),
        "merge side\nside\n",
    );
    assert_eq!(
        out(&cmd(&repo, &home, &["log", "--no-decorate", "--format=%s", "HEAD~1^-2"])),
        "merge side\nmain2\n",
    );
    assert_eq!(
        out(&cmd(&repo, &home, &["show", "--no-decorate", "-s", "--format=%s", "HEAD~1^-"])),
        "merge side\nside\n",
    );
    // The whole point: not the same answer as the bare revision.
    assert_ne!(
        out(&cmd(&repo, &home, &["show", "--no-decorate", "-s", "--format=%s", "HEAD~1^-"])),
        out(&cmd(&repo, &home, &["show", "--no-decorate", "-s", "--format=%s", "HEAD~1"])),
    );

    // A parent the commit does not have is `add_parents_only()`'s `return 0`, which
    // leaves the operand carrying a mark no resolver has a case for.
    for verb in ["log", "show"] {
        let o = cmd(&repo, &home, &[verb, "--no-decorate", "--format=%s", "HEAD~1^-3"]);
        assert_eq!(code(&o), 128, "{verb}");
        assert!(err(&o).starts_with("fatal: ambiguous argument 'HEAD~1^-3'"), "{}", err(&o));
    }
    // `strtol_i()` refuses the `<n>` before `add_parents_only()` is reached.
    let bad = cmd(&repo, &home, &["log", "--no-decorate", "--format=%s", "HEAD~1^-x"]);
    assert_eq!(code(&bad), 128);
    assert!(err(&bad).starts_with("fatal: ambiguous argument 'HEAD~1^-x'"), "{}", err(&bad));

    // `strstr`'s first-match rule: `^!^!` carries no usable mark at all.
    let twice = cmd(&repo, &home, &["log", "--no-decorate", "--format=%s", "HEAD^!^!"]);
    assert_eq!(code(&twice), 128);
    assert!(err(&twice).starts_with("fatal: ambiguous argument 'HEAD^!^!'"), "{}", err(&twice));
}

/// `setup_revisions()`'s `if (seen_dashdash || *arg == '^') die(_("bad revision
/// '%s'"), arg);` (revision.c:3035-3036): once a `--` has been seen the operand can
/// no longer be a pathspec, so the three-line advice is not printed.
#[test]
fn a_gated_operand_is_a_bad_revision_not_an_ambiguous_argument() {
    let (repo, home) = fixture("gated");

    for args in [
        vec!["log", "--format=%s", "nosuchrev", "--", "f"],
        vec!["log", "--format=%s", "nosuchrev", "--"],
        vec!["show", "-s", "--format=%s", "nosuchrev", "--", "f"],
    ] {
        let o = cmd(&repo, &home, &args);
        assert_eq!(code(&o), 128, "{args:?}");
        assert_eq!(err(&o), "fatal: bad revision 'nosuchrev'\n", "{args:?}");
    }

    // Without the separator the same operand is still a pathspec candidate.
    let o = cmd(&repo, &home, &["log", "--format=%s", "nosuchrev"]);
    assert_eq!(code(&o), 128);
    assert_eq!(
        err(&o),
        concat!(
            "fatal: ambiguous argument 'nosuchrev': unknown revision or path not in the working tree.\n",
            "Use '--' to separate paths from revisions, like this:\n",
            "'git <command> [<revision>...] -- [<file>...]'\n",
        ),
    );
}
