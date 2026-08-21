//! The config callbacks stacked on top of `git_default_config()` —
//! `git_diff_ui_config` / `git_diff_basic_config` / `git_color_config`
//! (`crate::diff_config`), `git_status_config` / `git_commit_config`
//! (`crate::status_config`) and `git_log_config` / `git_format_config`
//! (`crate::log_config`).
//!
//! Every expectation is a literal captured from a differential run against stock
//! git 2.55.0 in the fixture these tests build, so they run headless with nothing
//! on `PATH` but the binary under test.
//!
//! The point of most of them is not that a bad value is refused — it is *which
//! commands* refuse it. The three callbacks form a chain, and a command installs
//! exactly one link of it, so the same value is fatal for `git diff` and fine for
//! `git diff-tree`. A port that validated the union would break the plumbing; a
//! port that validated only the intersection would let the porcelain through.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// git's `die()` exit status.
const FATAL: i32 = 128;

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn ok(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(repo, home, args);
    assert!(
        out.status.success(),
        "setup `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// A two-commit repository with a modified file, so every diff-producing verb
/// has something to render.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-diffcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::write(repo.join("f"), "one\ntwo\n").unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    ok(&repo, &home, &["config", "user.email", "alice@example.com"]);
    ok(&repo, &home, &["config", "user.name", "Alice"]);
    ok(&repo, &home, &["add", "f"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("f"), "one\nthree\n").unwrap();
    ok(&repo, &home, &["commit", "-qam", "c1"]);
    (repo, home)
}

/// Append a block to the repository config and return the line its last variable
/// landed on.
fn append_config(repo: &Path, block: &str) -> usize {
    let cfg = repo.join(".git/config");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(block);
    std::fs::write(&cfg, &text).unwrap();
    text.lines().count()
}

/// The command lines each layer of the chain is reached through, as measured.
const UI_VERBS: &[&[&str]] = &[
    &["diff", "HEAD~1"],
    &["log", "-1"],
    &["show"],
    &["whatchanged", "-1"],
    &["range-diff", "HEAD~1...HEAD"],
    &["status"],
    &["commit", "-m", "x"],
];
/// Verbs on the *basic* layer only: they read the plumbing keys and nothing above.
const BASIC_ONLY_VERBS: &[&[&str]] = &[
    &["diff-files"],
    &["diff-index", "HEAD"],
    &["diff-tree", "HEAD"],
    &["stash", "list"],
    &["merge-tree", "HEAD", "HEAD"],
];
/// Verbs that colour but never diff.
const COLOR_ONLY_VERBS: &[&[&str]] =
    &[&["branch"], &["tag"], &["grep", "one"], &["clean", "-n"], &["show-branch"]];

// ---------------------------------------------------------------------------
// git_diff_ui_config — the porcelain layer
// ---------------------------------------------------------------------------

/// `diff.context` and `diff.interHunkContext` (diff.c:382-394) have the strangest
/// refusal in the chain: a *negative* value is `return -1` with no `error()` of
/// its own, so the whole diagnostic is the origin line that
/// `git_die_config_linenr()` adds. An unreadable value dies one line earlier,
/// inside `git_config_int`, and therefore looks completely different.
#[test]
fn diff_context_reports_only_the_origin_when_it_is_negative() {
    let (repo, home) = fixture("context");

    for key in ["diff.context", "diff.interHunkContext"] {
        let lower = key.to_lowercase();
        let out = run(&repo, &home, &["-c", &format!("{key}=-1"), "diff", "HEAD~1"]);
        assert_eq!(
            stderr(&out),
            format!("fatal: unable to parse '{lower}' from command-line config\n"),
            "for {key}=-1"
        );
        assert_eq!(code(&out), FATAL, "for {key}=-1");

        let out = run(&repo, &home, &["-c", &format!("{key}=bogus"), "diff", "HEAD~1"]);
        assert_eq!(
            stderr(&out),
            format!("fatal: bad numeric config value 'bogus' for '{lower}': invalid unit\n"),
            "for {key}=bogus"
        );
        assert_eq!(code(&out), FATAL, "for {key}=bogus");

        // Zero and a positive value are both fine.
        for good in ["0", "5"] {
            let out = run(&repo, &home, &["-c", &format!("{key}={good}"), "diff", "HEAD~1"]);
            assert_eq!(stderr(&out), "", "for {key}={good}");
            assert!(out.status.success(), "for {key}={good}");
        }
    }
}

/// The UI-layer keys, and the plumbing's indifference to them.
///
/// Each value below is fatal for every verb on the UI layer and accepted by every
/// verb on the basic layer — which is the whole reason `git_diff_basic_config`
/// exists (diff.c:276-279: "These are to give UI layer defaults. The core-level
/// commands such as git-diff-files should never be affected").
#[test]
fn the_ui_keys_are_refused_by_porcelain_and_ignored_by_plumbing() {
    let (repo, home) = fixture("ui-layer");

    let cases: &[(&str, &str)] = &[
        (
            "diff.relative=bogus",
            "fatal: bad boolean config value 'bogus' for 'diff.relative'\n",
        ),
        (
            "diff.mnemonicPrefix=bogus",
            "fatal: bad boolean config value 'bogus' for 'diff.mnemonicprefix'\n",
        ),
        (
            "diff.noPrefix=bogus",
            "fatal: bad boolean config value 'bogus' for 'diff.noprefix'\n",
        ),
        (
            "diff.autoRefreshIndex=bogus",
            "fatal: bad boolean config value 'bogus' for 'diff.autorefreshindex'\n",
        ),
        (
            "diff.trustExitCode=bogus",
            "fatal: bad boolean config value 'bogus' for 'diff.trustexitcode'\n",
        ),
        (
            "diff.statGraphWidth=bogus",
            "fatal: bad numeric config value 'bogus' for 'diff.statgraphwidth': invalid unit\n",
        ),
        (
            "diff.statNameWidth=bogus",
            "fatal: bad numeric config value 'bogus' for 'diff.statnamewidth': invalid unit\n",
        ),
        (
            "diff.ignoreSubmodules=bogus",
            "fatal: bad --ignore-submodules argument: bogus\n",
        ),
        (
            "diff.algorithm=bogus",
            "error: unknown value for config 'diff.algorithm': bogus\n\
             fatal: unable to parse 'diff.algorithm' from command-line config\n",
        ),
        (
            "diff.colorMoved=bogus",
            "error: color moved setting must be one of 'no', 'default', 'blocks', 'zebra', \
             'dimmed-zebra', 'plain'\n\
             fatal: unable to parse 'diff.colormoved' from command-line config\n",
        ),
        (
            "diff.colorMovedWs=bogus",
            "error: unknown color-moved-ws mode 'bogus', possible values are \
             'ignore-space-change', 'ignore-space-at-eol', 'ignore-all-space', \
             'allow-indentation-change'\n\
             fatal: unable to parse 'diff.colormovedws' from command-line config\n",
        ),
    ];

    for (assignment, want) in cases {
        for verb in UI_VERBS {
            let mut argv = vec!["-c", assignment];
            argv.extend_from_slice(verb);
            let out = run(&repo, &home, &argv);
            assert_eq!(stderr(&out), *want, "for -c {assignment} {verb:?}");
            assert_eq!(code(&out), FATAL, "for -c {assignment} {verb:?}");
        }
        for verb in BASIC_ONLY_VERBS {
            let mut argv = vec!["-c", assignment];
            argv.extend_from_slice(verb);
            let out = run(&repo, &home, &argv);
            assert_eq!(stderr(&out), "", "plumbing must ignore {assignment} ({verb:?})");
        }
    }
}

/// `parse_color_moved_ws` (diff.c:338-374) is the one arm that reports more than
/// one `error()` before its fatal: every unknown token gets a line, and a legal
/// but contradictory combination adds one more.
#[test]
fn color_moved_ws_reports_every_bad_token_and_the_bad_combination() {
    let (repo, home) = fixture("cmws");

    let out = run(
        &repo,
        &home,
        &["-c", "diff.colorMovedWs=bogus,alsobad", "diff", "HEAD~1"],
    );
    assert_eq!(
        stderr(&out),
        "error: unknown color-moved-ws mode 'bogus', possible values are \
         'ignore-space-change', 'ignore-space-at-eol', 'ignore-all-space', \
         'allow-indentation-change'\n\
         error: unknown color-moved-ws mode 'alsobad', possible values are \
         'ignore-space-change', 'ignore-space-at-eol', 'ignore-all-space', \
         'allow-indentation-change'\n\
         fatal: unable to parse 'diff.colormovedws' from command-line config\n"
    );
    assert_eq!(code(&out), FATAL);

    // Every token here is valid on its own; the combination is not.
    let out = run(
        &repo,
        &home,
        &[
            "-c",
            "diff.colorMovedWs=allow-indentation-change,ignore-all-space",
            "diff",
            "HEAD~1",
        ],
    );
    assert_eq!(
        stderr(&out),
        "error: color-moved-ws: allow-indentation-change cannot be combined with other \
         whitespace modes\n\
         fatal: unable to parse 'diff.colormovedws' from command-line config\n"
    );
    assert_eq!(code(&out), FATAL);

    // A `no` token resets the running set rather than adding to it, so this pair
    // is legal even though the same two words in the other order are not.
    for good in [
        "ignore-all-space,no,allow-indentation-change",
        "ignore-space-change,ignore-space-at-eol",
        "allow-indentation-change",
    ] {
        let out = run(&repo, &home, &["-c", &format!("diff.colorMovedWs={good}"), "diff", "HEAD~1"]);
        assert_eq!(stderr(&out), "", "for {good}");
        assert!(out.status.success(), "for {good}");
    }
}

// ---------------------------------------------------------------------------
// git_diff_basic_config — the plumbing layer, reached from both
// ---------------------------------------------------------------------------

/// The basic-layer keys are refused by the plumbing *and* the porcelain, because
/// the porcelain callback ends by calling the plumbing one (diff.c:475).
#[test]
fn the_basic_keys_are_refused_everywhere_the_chain_reaches() {
    let (repo, home) = fixture("basic-layer");

    let cases: &[(&str, &str)] = &[
        (
            "diff.renameLimit=bogus",
            "fatal: bad numeric config value 'bogus' for 'diff.renamelimit': invalid unit\n",
        ),
        (
            "diff.suppressBlankEmpty=bogus",
            "fatal: bad boolean config value 'bogus' for 'diff.suppressblankempty'\n",
        ),
        (
            "diff.indentHeuristic=bogus",
            "fatal: bad boolean config value 'bogus' for 'diff.indentheuristic'\n",
        ),
        (
            "diff.wsErrorHighlight=bogus",
            "error: unknown value for config 'diff.wserrorhighlight': bogus\n\
             fatal: unable to parse 'diff.wserrorhighlight' from command-line config\n",
        ),
        (
            "color.diff.meta=nosuchcolor",
            "error: invalid color value: nosuchcolor\n\
             fatal: unable to parse 'color.diff.meta' from command-line config\n",
        ),
        (
            "diff.color.meta=nosuchcolor",
            "error: invalid color value: nosuchcolor\n\
             fatal: unable to parse 'diff.color.meta' from command-line config\n",
        ),
    ];

    for (assignment, want) in cases {
        for verb in UI_VERBS.iter().chain(BASIC_ONLY_VERBS.iter()) {
            let mut argv = vec!["-c", assignment];
            argv.extend_from_slice(verb);
            let out = run(&repo, &home, &argv);
            assert_eq!(stderr(&out), *want, "for -c {assignment} {verb:?}");
            assert_eq!(code(&out), FATAL, "for -c {assignment} {verb:?}");
        }
    }

    // A colour slot the table does not know is not validated at all
    // (diff.c:492-493 returns before the value is read).
    let out = run(
        &repo,
        &home,
        &["-c", "color.diff.nosuchslot=nosuchcolor", "diff", "HEAD~1"],
    );
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());

    // `parse_ws_error_highlight`'s tokens must end at a comma or the string end,
    // so a good name with junk glued on is an unknown token rather than a match.
    for good in ["none", "default", "all", "old,new,context", ""] {
        let out = run(
            &repo,
            &home,
            &["-c", &format!("diff.wsErrorHighlight={good}"), "diff", "HEAD~1"],
        );
        assert_eq!(stderr(&out), "", "for {good:?}");
        assert!(out.status.success(), "for {good:?}");
    }
    let out = run(&repo, &home, &["-c", "diff.wsErrorHighlight=newx", "diff", "HEAD~1"]);
    assert_eq!(code(&out), FATAL, "a token must end at a comma or the string end");
}

/// `diff.dirstat` (diff.c:522-533) collects every bad parameter into one
/// `warning()` and lets the command run, which is the only multi-line warning in
/// the chain. The two-space indent and the per-line newline come from
/// `parse_dirstat_params`' own `strbuf_addf`s.
#[test]
fn diff_dirstat_warns_once_with_every_bad_parameter() {
    let (repo, home) = fixture("dirstat");

    let out = run(&repo, &home, &["-c", "diff.dirstat=bogus", "diff", "HEAD~1"]);
    assert_eq!(
        stderr(&out),
        "warning: Found errors in 'diff.dirstat' config variable:\n\
         \x20 Unknown dirstat parameter 'bogus'\n\n",
        "`warning()` adds a newline of its own after the accumulated reasons, so \
         the block ends in a blank line"
    );
    assert!(out.status.success());

    let out = run(
        &repo,
        &home,
        &["-c", "diff.dirstat=lines,bogus,10x,cumulative", "diff", "HEAD~1"],
    );
    assert_eq!(
        stderr(&out),
        "warning: Found errors in 'diff.dirstat' config variable:\n\
         \x20 Unknown dirstat parameter 'bogus'\n\
         \x20 Failed to parse dirstat cut-off percentage '10x'\n\n"
    );
    assert!(out.status.success());

    for good in ["lines", "files,cumulative", "15", "12.5", "changes,noncumulative"] {
        let out = run(&repo, &home, &["-c", &format!("diff.dirstat={good}"), "diff", "HEAD~1"]);
        assert_eq!(stderr(&out), "", "for {good}");
        assert!(out.status.success(), "for {good}");
    }
}

// ---------------------------------------------------------------------------
// git_color_config — color.ui, which the plumbing does not see
// ---------------------------------------------------------------------------

/// `color.ui` reaches the diff porcelain through `git_diff_ui_config`
/// (diff.c:472) and the non-diff porcelain through `git_color_default_config`,
/// but never reaches `git_diff_basic_config` — so `git diff-tree` runs with a
/// value `git diff` refuses.
#[test]
fn color_ui_is_a_boolean_for_the_porcelain_and_invisible_to_the_plumbing() {
    let (repo, home) = fixture("colorui");

    let want = "fatal: bad boolean config value 'bogus' for 'color.ui'\n";
    for verb in UI_VERBS.iter().chain(COLOR_ONLY_VERBS.iter()) {
        let mut argv = vec!["-c", "color.ui=bogus"];
        argv.extend_from_slice(verb);
        let out = run(&repo, &home, &argv);
        assert_eq!(stderr(&out), want, "for {verb:?}");
        assert_eq!(code(&out), FATAL, "for {verb:?}");
    }
    for verb in BASIC_ONLY_VERBS {
        let mut argv = vec!["-c", "color.ui=bogus"];
        argv.extend_from_slice(verb);
        let out = run(&repo, &home, &argv);
        assert_eq!(stderr(&out), "", "plumbing must ignore color.ui ({verb:?})");
    }

    // `git_config_colorbool` takes three words before it reaches the boolean, and
    // matches them case-insensitively.
    for good in ["never", "ALWAYS", "Auto", "true", "0"] {
        let out = run(&repo, &home, &["-c", &format!("color.ui={good}"), "branch"]);
        assert_eq!(stderr(&out), "", "for {good}");
        assert!(out.status.success(), "for {good}");
    }
}

// ---------------------------------------------------------------------------
// git_status_config / git_commit_config
// ---------------------------------------------------------------------------

/// The `status.*` keys, which only `status` and `commit` read.
#[test]
fn the_status_keys_are_refused_by_status_and_commit_only() {
    let (repo, home) = fixture("status-keys");

    let cases: &[(&str, &str)] = &[
        ("status.branch=bogus", "fatal: bad boolean config value 'bogus' for 'status.branch'\n"),
        ("status.short=bogus", "fatal: bad boolean config value 'bogus' for 'status.short'\n"),
        (
            "status.aheadBehind=bogus",
            "fatal: bad boolean config value 'bogus' for 'status.aheadbehind'\n",
        ),
        (
            "status.showStash=bogus",
            "fatal: bad boolean config value 'bogus' for 'status.showstash'\n",
        ),
        (
            "status.displayCommentPrefix=bogus",
            "fatal: bad boolean config value 'bogus' for 'status.displaycommentprefix'\n",
        ),
        (
            "status.relativePaths=bogus",
            "fatal: bad boolean config value 'bogus' for 'status.relativepaths'\n",
        ),
        (
            "status.submoduleSummary=bogus",
            "fatal: bad numeric config value 'bogus' for 'status.submodulesummary': invalid unit\n",
        ),
        (
            "status.renameLimit=bogus",
            "fatal: bad numeric config value 'bogus' for 'status.renamelimit': invalid unit\n",
        ),
        ("status.renames=bogus", "fatal: bad boolean config value 'bogus' for 'status.renames'\n"),
        ("status.color=bogus", "fatal: bad boolean config value 'bogus' for 'status.color'\n"),
        ("color.status=bogus", "fatal: bad boolean config value 'bogus' for 'color.status'\n"),
        (
            "status.showUntrackedFiles=bogus",
            "error: Invalid untracked files mode 'bogus'\n\
             fatal: unable to parse 'status.showuntrackedfiles' from command-line config\n",
        ),
        (
            "color.status.added=nosuchcolor",
            "error: invalid color value: nosuchcolor\n\
             fatal: unable to parse 'color.status.added' from command-line config\n",
        ),
        (
            "status.color.noBranch=nosuchcolor",
            "error: invalid color value: nosuchcolor\n\
             fatal: unable to parse 'status.color.nobranch' from command-line config\n",
        ),
    ];

    for (assignment, want) in cases {
        for verb in [vec!["status"], vec!["commit", "-m", "x"]] {
            let mut argv = vec!["-c", assignment];
            argv.extend(verb.iter().copied());
            let out = run(&repo, &home, &argv);
            assert_eq!(stderr(&out), *want, "for -c {assignment} {verb:?}");
            assert_eq!(code(&out), FATAL, "for -c {assignment} {verb:?}");
        }
        // `log` is on the same chain one link further out and does *not* see them.
        let out = run(&repo, &home, &["-c", assignment, "log", "-1"]);
        assert_eq!(stderr(&out), "", "log must ignore {assignment}");
    }

    // `status.showUntrackedFiles` runs the boolean grammar first, so `1` is
    // `normal` and `off` is `no`; a colour slot outside the table is ignored.
    for good in ["no", "normal", "all", "1", "off", "true"] {
        let out = run(
            &repo,
            &home,
            &["-c", &format!("status.showUntrackedFiles={good}"), "status", "--short"],
        );
        assert_eq!(stderr(&out), "", "for {good}");
        assert!(out.status.success(), "for {good}");
    }
    let out = run(
        &repo,
        &home,
        &["-c", "color.status.nosuchslot=nosuchcolor", "status", "--short"],
    );
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());
}

/// The `-1` guard in `git_status_config`'s `diff.renameLimit` arm
/// (builtin/commit.c:1516-1520) decides whether a *later* value is parsed at all,
/// so an assignment that would be fatal on its own becomes invisible once
/// something has written the field.
///
/// Under `git diff` the same key is read unconditionally
/// (`git_diff_ui_config`, diff.c:482-485), so the identical pair is fatal there.
/// That asymmetry is the test.
#[test]
fn the_status_rename_guards_skip_a_value_git_diff_still_parses() {
    let (repo, home) = fixture("guards");
    let bad = "fatal: bad numeric config value 'bogus' for 'diff.renamelimit': invalid unit\n";

    // Something wrote the field first, so the bad value is never parsed.
    let out = run(
        &repo,
        &home,
        &["-c", "status.renameLimit=5", "-c", "diff.renameLimit=bogus", "status", "--short"],
    );
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());

    let out = run(
        &repo,
        &home,
        &["-c", "diff.renameLimit=5", "-c", "diff.renameLimit=bogus", "status", "--short"],
    );
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());

    // The other order leaves the field unset when the bad value is met.
    let out = run(
        &repo,
        &home,
        &["-c", "diff.renameLimit=bogus", "-c", "status.renameLimit=5", "status", "--short"],
    );
    assert_eq!(stderr(&out), bad);
    assert_eq!(code(&out), FATAL);

    // …and so does a first value of -1, because that is the sentinel itself.
    let out = run(
        &repo,
        &home,
        &["-c", "diff.renameLimit=-1", "-c", "diff.renameLimit=bogus", "status", "--short"],
    );
    assert_eq!(stderr(&out), bad);
    assert_eq!(code(&out), FATAL);

    // `git diff` has no guard, so the pair that `status` accepted is fatal.
    let out = run(
        &repo,
        &home,
        &["-c", "diff.renameLimit=5", "-c", "diff.renameLimit=bogus", "diff", "HEAD~1"],
    );
    assert_eq!(stderr(&out), bad);
    assert_eq!(code(&out), FATAL);

    // The same guard on `diff.renames`, whose `false` is 0 and therefore *does*
    // count as written.
    let out = run(
        &repo,
        &home,
        &["-c", "diff.renames=false", "-c", "diff.renames=bogus", "status", "--short"],
    );
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());
}

/// `git_commit_config`'s own five keys (builtin/commit.c:1669-1693), which
/// `status` does not read.
#[test]
fn the_commit_keys_are_refused_by_commit_only() {
    let (repo, home) = fixture("commit-keys");

    let cases: &[(&str, &str)] = &[
        ("commit.status=bogus", "fatal: bad boolean config value 'bogus' for 'commit.status'\n"),
        ("commit.gpgSign=bogus", "fatal: bad boolean config value 'bogus' for 'commit.gpgsign'\n"),
        (
            "commit.verbose=bogus",
            "fatal: bad numeric config value 'bogus' for 'commit.verbose': invalid unit\n",
        ),
        (
            "commit.template=~zvcs-no-such-user/t",
            "fatal: failed to expand user dir in: '~zvcs-no-such-user/t'\n",
        ),
    ];

    for (assignment, want) in cases {
        let out = run(&repo, &home, &["-c", assignment, "commit", "-m", "x"]);
        assert_eq!(stderr(&out), *want, "for {assignment}");
        assert_eq!(code(&out), FATAL, "for {assignment}");

        let out = run(&repo, &home, &["-c", assignment, "status", "--short"]);
        assert_eq!(stderr(&out), "", "status must ignore {assignment}");
    }

    // `commit.verbose` is `git_config_bool_or_int`, so the boolean words parse.
    for good in ["true", "false", "2"] {
        let out = run(&repo, &home, &["-c", &format!("commit.verbose={good}"), "status", "--short"]);
        assert_eq!(stderr(&out), "", "for {good}");
    }
}

/// `git_column_config` (column.c:328-343) reached through `git_status_config`
/// (builtin/commit.c:1457-1458): `column.ui` and the one key named after the
/// command, with the token's own reason printed before the mode line.
#[test]
fn the_column_keys_report_the_token_and_then_the_mode() {
    let (repo, home) = fixture("column");

    for (key, name) in [("column.ui", "ui"), ("column.status", "status")] {
        let out = run(&repo, &home, &["-c", &format!("{key}=bogus"), "status"]);
        assert_eq!(
            stderr(&out),
            format!(
                "error: unsupported option 'bogus'\n\
                 error: invalid column.{name} mode bogus\n\
                 fatal: unable to parse '{key}' from command-line config\n"
            ),
            "for {key}"
        );
        assert_eq!(code(&out), FATAL, "for {key}");
    }

    // A `column.<other-command>` key is not this command's, and is ignored.
    let out = run(&repo, &home, &["-c", "column.branch=bogus", "status"]);
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());
}

// ---------------------------------------------------------------------------
// git_log_config / git_format_config
// ---------------------------------------------------------------------------

/// The `log.*` keys, which `log`, `show` and `whatchanged` read and `diff` does
/// not.
#[test]
fn the_log_keys_are_refused_by_the_commit_listing_verbs() {
    let (repo, home) = fixture("log-keys");

    let cases: &[(&str, &str)] = &[
        (
            "log.abbrevCommit=bogus",
            "fatal: bad boolean config value 'bogus' for 'log.abbrevcommit'\n",
        ),
        ("log.showRoot=bogus", "fatal: bad boolean config value 'bogus' for 'log.showroot'\n"),
        ("log.follow=bogus", "fatal: bad boolean config value 'bogus' for 'log.follow'\n"),
        ("log.mailmap=bogus", "fatal: bad boolean config value 'bogus' for 'log.mailmap'\n"),
        (
            "log.showSignature=bogus",
            "fatal: bad boolean config value 'bogus' for 'log.showsignature'\n",
        ),
        (
            "format.encodeEmailHeaders=bogus",
            "fatal: bad boolean config value 'bogus' for 'format.encodeemailheaders'\n",
        ),
        (
            "format.filenameMaxLength=bogus",
            "fatal: bad numeric config value 'bogus' for 'format.filenamemaxlength': invalid unit\n",
        ),
        (
            "log.diffMerges=bogus",
            "fatal: unable to parse 'log.diffmerges' from command-line config\n",
        ),
        (
            "color.decorate.branch=nosuchcolor",
            "error: invalid color value: nosuchcolor\n\
             fatal: unable to parse 'color.decorate.branch' from command-line config\n",
        ),
    ];

    for (assignment, want) in cases {
        for verb in [vec!["log", "-1"], vec!["show"], vec!["whatchanged", "-1"]] {
            let mut argv = vec!["-c", assignment];
            argv.extend(verb.iter().copied());
            let out = run(&repo, &home, &argv);
            assert_eq!(stderr(&out), *want, "for -c {assignment} {verb:?}");
            assert_eq!(code(&out), FATAL, "for -c {assignment} {verb:?}");
        }
        let out = run(&repo, &home, &["-c", assignment, "diff", "HEAD~1"]);
        assert_eq!(stderr(&out), "", "diff must ignore {assignment}");
    }

    // `log.decorate` cannot fail: `parse_decoration_style` returning -1 is turned
    // into 0 with a "maybe warn?" comment (builtin/log.c:492-497).
    let out = run(&repo, &home, &["-c", "log.decorate=bogus", "log", "-1"]);
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());

    // A decorate colour slot outside the table is ignored.
    let out = run(&repo, &home, &["-c", "color.decorate.nosuchslot=nosuchcolor", "log", "-1"]);
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());
}

/// `git_format_config`'s short-circuit (builtin/log.c:1005-1008 and :1131-1133)
/// makes `format-patch` accept five keys that `log` — one link further out on the
/// same chain — refuses.
#[test]
fn format_patch_short_circuits_five_keys_the_rest_of_the_chain_refuses() {
    let (repo, home) = fixture("format-shortcircuit");

    for assignment in [
        "color.ui=bogus",
        "diff.color=bogus",
        "color.diff=bogus",
        "diff.noPrefix=bogus",
        "diff.submodule=bogus",
    ] {
        let out = run(&repo, &home, &["-c", assignment, "format-patch", "-1", "--stdout"]);
        assert_eq!(stderr(&out), "", "format-patch must swallow {assignment}");
        assert!(out.status.success(), "format-patch must swallow {assignment}");

        // …and `log`, which does not have the short-circuit, must not. Four of the
        // five are fatal there; `diff.submodule` is the one that only warns, so
        // the invariant asserted for all five is "stderr is not empty".
        let out = run(&repo, &home, &["-c", assignment, "log", "-1"]);
        assert_ne!(stderr(&out), "", "log must still report {assignment}");
    }

    // Spelled out for the odd one, so the difference is not just "not empty".
    let out = run(&repo, &home, &["-c", "diff.submodule=bogus", "log", "-1"]);
    assert_eq!(
        stderr(&out),
        "warning: Unknown value for 'diff.submodule' config variable: 'bogus'\n"
    );
    assert!(out.status.success());

    // A UI key that is *not* short-circuited is still fatal for format-patch.
    let out = run(&repo, &home, &["-c", "diff.relative=bogus", "format-patch", "-1", "--stdout"]);
    assert_eq!(
        stderr(&out),
        "fatal: bad boolean config value 'bogus' for 'diff.relative'\n"
    );
    assert_eq!(code(&out), FATAL);
}

/// `git_format_config`'s own keys (builtin/log.c:978-1130), including the two
/// with an extra word and the one whose refusal carries `hint:` lines.
#[test]
fn the_format_keys_take_their_words_before_their_booleans() {
    let (repo, home) = fixture("format-keys");

    let cases: &[(&str, &str)] = &[
        ("format.numbered=bogus", "fatal: bad boolean config value 'bogus' for 'format.numbered'\n"),
        (
            "format.coverLetter=bogus",
            "fatal: bad boolean config value 'bogus' for 'format.coverletter'\n",
        ),
        ("format.thread=bogus", "fatal: bad boolean config value 'bogus' for 'format.thread'\n"),
        ("format.signoff=bogus", "fatal: bad boolean config value 'bogus' for 'format.signoff'\n"),
        (
            "format.useAutoBase=bogus",
            "fatal: bad boolean config value 'bogus' for 'format.useautobase'\n",
        ),
        (
            "format.forceInBodyFrom=bogus",
            "fatal: bad boolean config value 'bogus' for 'format.forceinbodyfrom'\n",
        ),
        ("format.mboxrd=bogus", "fatal: bad boolean config value 'bogus' for 'format.mboxrd'\n"),
        (
            "format.coverFromDescription=bogus",
            "fatal: bogus: invalid cover from description mode\n",
        ),
        (
            "format.signatureFile=~zvcs-no-such-user/s",
            "fatal: failed to expand user dir in: '~zvcs-no-such-user/s'\n",
        ),
    ];

    for (assignment, want) in cases {
        let out = run(&repo, &home, &["-c", assignment, "format-patch", "-1", "--stdout"]);
        assert_eq!(stderr(&out), *want, "for {assignment}");
        assert_eq!(code(&out), FATAL, "for {assignment}");
    }

    // `format.noprefix` is the one boolean whose refusal is `die_message()` plus
    // `advise()`, so the fatal line is followed by two `hint:` lines.
    let out = run(&repo, &home, &["-c", "format.noprefix=bogus", "format-patch", "-1", "--stdout"]);
    assert_eq!(
        stderr(&out),
        "fatal: bad boolean config value 'bogus' for 'format.noprefix'\n\
         hint: 'format.noprefix' used to accept any value and treat that as 'true'.\n\
         hint: Now it only accepts boolean values, like what 'diff.noprefix' does.\n"
    );
    assert_eq!(code(&out), FATAL);

    // The words each arm takes before its boolean, and the values that cannot
    // fail at all.
    for good in [
        "format.numbered=auto",
        "format.coverLetter=auto",
        "format.thread=deep",
        "format.thread=shallow",
        "format.useAutoBase=whenAble",
        "format.attach=anything",
        "format.notes=anything",
        "format.from=anything",
        "format.coverFromDescription=subject",
    ] {
        let out = run(&repo, &home, &["-c", good, "format-patch", "-1", "-o", "/dev/null"]);
        // The run may still fail for reasons of its own (an output directory it
        // cannot write); what must not appear is a config refusal.
        assert!(
            !stderr(&out).contains("fatal: bad")
                && !stderr(&out).contains("invalid cover from description"),
            "for {good}: {}",
            stderr(&out)
        );
    }

    // `format.headers` is the one arm that `die()`s on a missing value instead of
    // reporting it, so there is no origin clause even from a file.
    let (repo, home) = fixture("format-headers");
    append_config(&repo, "[format]\n\theaders\n");
    let out = run(&repo, &home, &["format-patch", "-1", "--stdout"]);
    assert_eq!(stderr(&out), "fatal: format.headers without value\n");
    assert_eq!(code(&out), FATAL);
}

/// The file and environment scopes for one key from each new callback, so the
/// origin clause is pinned outside `-c` too.
#[test]
fn the_new_callbacks_name_their_origin_in_every_scope() {
    let (repo, home) = fixture("origins");
    let line = append_config(&repo, "[diff]\n\trenames = true\n\talgorithm = bogus\n");
    let out = run(&repo, &home, &["diff", "HEAD~1"]);
    assert_eq!(
        stderr(&out),
        format!(
            "error: unknown value for config 'diff.algorithm': bogus\n\
             fatal: bad config variable 'diff.algorithm' in file '.git/config' at line {line}\n"
        )
    );
    assert_eq!(code(&out), FATAL);

    let (repo, home) = fixture("origins-env");
    let out = Command::new(BIN)
        .args(["status"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", &home)
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "status.showUntrackedFiles")
        .env("GIT_CONFIG_VALUE_0", "bogus")
        .output()
        .unwrap();
    assert_eq!(
        stderr(&out),
        "error: Invalid untracked files mode 'bogus'\n\
         fatal: unable to parse 'status.showuntrackedfiles' from command-line config\n"
    );
    assert_eq!(code(&out), FATAL);
}
