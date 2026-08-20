//! `pull.twohead` and `pull.octopus` — the two configurable default merge
//! strategies.
//!
//! Both keys are spelled `pull.*` and neither is read by `git pull`. They live in
//! `git_merge_config()` (builtin/merge.c:708-713), so the command that honors them
//! is `git merge` — a pull only inherits them by forwarding its heads to the merge
//! machinery. Every test here therefore drives `git merge` directly; a test that
//! drove `git pull` would still pass with the keys unread, because the fetch half
//! contributes the same heads either way.
//!
//! What the keys do, and what each test pins:
//!
//! * The default strategy is chosen by *head count*, and only when no `-s` was
//!   given: `if (!use_strategies)` → `add_strategies(pull_twohead, DEFAULT_TWOHEAD)`
//!   for one head, `add_strategies(pull_octopus, DEFAULT_OCTOPUS)` for more
//!   (builtin/merge.c:1600-1608). So `pull.twohead` cannot touch an octopus and
//!   `pull.octopus` cannot touch an ordinary two-head merge — asserted in both
//!   directions, since a single key wired to both counts would still pass the
//!   happy-path tests.
//! * The value replaces the built-in default rather than adding to it, and it is a
//!   *space-separated list* (`add_strategies()`, builtin/merge.c:872-889).
//! * An unknown name is `get_strategy()`'s failure, which is the same two lines a
//!   bogus `-s` produces, on stderr, with exit 1 and nothing on stdout.
//! * An explicit `-s` wins: `use_strategies` is non-empty, so the config is never
//!   consulted — even a bogus config value cannot break the merge.
//!
//! Every expectation was measured against git 2.55.0 (`/opt/homebrew/bin/git`)
//! first and copied byte for byte; where that binary is present the same fixture is
//! rebuilt with it and the outputs are compared directly, so the pins cannot drift
//! into "whatever this port happens to print". Identity and dates are pinned, which
//! makes the fixtures byte-identical between the two binaries — the tests assert
//! that before comparing anything else.
//!
//! Measured divergence, deliberately *not* asserted here so this file cannot cement
//! it: with an empty value (`git -c pull.twohead= merge <b>`, or `twohead =` in the
//! config file) stock splits the empty string into one empty strategy name and dies
//! with `Could not find merge strategy ''.`, while this port treats empty and
//! whitespace-only as unset and merges with the built-in default.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Stock git for the comparison arms. Absent on Linux CI, where every test below
/// still asserts the full behavior on its own — the stock arm only adds the proof
/// that the pinned bytes are git's rather than this port's.
fn stock() -> Option<&'static str> {
    let p = "/opt/homebrew/bin/git";
    Path::new(p).exists().then_some(p)
}

/// Identity and date vars git honors above config. CI exports some of these for
/// the whole job, which would change the commits these fixtures build — and the
/// fixtures have to be bit-identical between the two binaries for the comparison
/// arms to mean anything.
const PINNED: [&str; 6] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_DATE",
];

/// A scratch `$HOME` shared by every command in this file, outside any fixture.
/// This port writes a cache directory and a sqlite database under `$HOME`; with
/// `HOME` pointing into a repository those land in its worktree and show up in the
/// `git status` these tests read.
fn home() -> PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-mergecfg-home-{}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(bin);
    for v in PINNED {
        c.env_remove(v);
    }
    c.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        // This machine's ~/.gitconfig sets core.commentChar and friends; an
        // unpinned run measures a different config than the one under test.
        // A scratch HOME outside every fixture: this port keeps its own cache and
        // sqlite db under $HOME, and pointing that at a repository would leave
        // untracked files in the very worktree these tests inspect.
        .env("HOME", home())
        .env("ZVCS_HOME", home())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
        .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
        .output()
        .unwrap()
}

fn ok(bin: &str, dir: &Path, args: &[&str]) {
    let out = run(bin, dir, args);
    assert!(out.status.success(), "{bin} {args:?} failed: {}", err(&out));
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn err(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn out_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn rev(bin: &str, dir: &Path, spec: &str) -> String {
    let out = run(bin, dir, &["rev-parse", spec]);
    assert!(out.status.success(), "rev-parse {spec}: {}", err(&out));
    out_str(&out).trim().to_string()
}

/// Number of parents of `HEAD`: 1 for a linear commit, 2 for an ordinary merge,
/// 3 for a two-head octopus.
fn head_parents(bin: &str, dir: &Path) -> usize {
    let out = run(bin, dir, &["rev-list", "--parents", "-n", "1", "HEAD"]);
    out_str(&out).split_whitespace().count() - 1
}

/// `base` on `main`, side branches `a` and `b` off it, then one more commit on
/// `main` — so `main` has genuinely diverged from both sides and no merge here can
/// fast-forward. `a` and `b` touch disjoint paths, which is what lets the octopus
/// arms merge cleanly.
fn fixture(bin: &str, tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-mergecfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let r = root.join("r");
    std::fs::create_dir_all(&r).unwrap();

    ok(bin, &r, &["init", "-q", "-b", "main", "."]);
    std::fs::write(r.join("f"), "a\n").unwrap();
    ok(bin, &r, &["add", "f"]);
    ok(bin, &r, &["commit", "-q", "-m", "base"]);
    for side in ["a", "b"] {
        ok(bin, &r, &["checkout", "-q", "-b", side, "main"]);
        std::fs::write(r.join(side), format!("{side}\n")).unwrap();
        ok(bin, &r, &["add", side]);
        ok(bin, &r, &["commit", "-q", "-m", side]);
    }
    ok(bin, &r, &["checkout", "-q", "main"]);
    std::fs::write(r.join("m"), "m\n").unwrap();
    ok(bin, &r, &["add", "m"]);
    ok(bin, &r, &["commit", "-q", "-m", "mainc"]);
    r
}

/// Build the same fixture with stock git and run the same argv there, returning
/// `None` when stock is not installed. The fixture's `HEAD` is asserted equal to
/// the zvcs-built one first: if the two ever stop building bit-identical commits,
/// the byte comparison below would be comparing two different repositories.
fn stock_run(tag: &str, base: &str, args: &[&str]) -> Option<(PathBuf, Output)> {
    let bin = stock()?;
    let r = fixture(bin, &format!("stock-{tag}"));
    assert_eq!(
        rev(bin, &r, "HEAD"),
        base,
        "fixtures differ between stock and this port; the byte comparison would be meaningless"
    );
    let out = run(bin, &r, args);
    Some((r, out))
}

/// The two lines `get_strategy()` dies with. Copied from git 2.55.0's stderr; the
/// trailing period on each line and the alphabetical, space-separated list are
/// git's own formatting.
const UNKNOWN: &str = "Could not find merge strategy 'bogus'.\n\
                       Available strategies are: octopus ours recursive resolve subtree.\n";

/// `pull.twohead` replaces `ort` for the one-head case: `ours` keeps our tree
/// wholesale while still recording the side as a second parent, and announces
/// itself by name on stdout.
#[test]
fn twohead_config_picks_the_strategy_for_a_one_head_merge() {
    let r = fixture(BIN, "twohead-ours");
    let before_tree = rev(BIN, &r, "HEAD^{tree}");
    let base = rev(BIN, &r, "HEAD");
    let out = run(BIN, &r, &["-c", "pull.twohead=ours", "merge", "a"]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_str(&out), "Merge made by the 'ours' strategy.\n");
    assert_eq!(err(&out), "");
    // `ours` is the discriminator: the default `ort` would have merged `a`'s file
    // in and changed the tree.
    assert_eq!(rev(BIN, &r, "HEAD^{tree}"), before_tree, "the 'ours' strategy keeps our tree");
    assert_eq!(head_parents(BIN, &r), 2);

    if let Some((sr, sout)) = stock_run("twohead-ours", &base, &["-c", "pull.twohead=ours", "merge", "a"]) {
        let bin = stock().unwrap();
        assert_eq!(out_str(&sout), out_str(&out), "stdout differs from stock");
        assert_eq!(err(&sout), err(&out), "stderr differs from stock");
        assert_eq!(code(&sout), code(&out));
        assert_eq!(rev(bin, &sr, "HEAD"), rev(BIN, &r, "HEAD"), "stock built a different merge commit");
    }
}

/// The same fixture with no config: `ort` merges `a` in and says so. Control for
/// the test above — without it, `ours` proving anything depends on `ort` not
/// having produced the same tree by accident.
#[test]
fn the_same_merge_without_the_config_uses_ort_and_changes_the_tree() {
    let r = fixture(BIN, "twohead-control");
    let before_tree = rev(BIN, &r, "HEAD^{tree}");
    let out = run(BIN, &r, &["merge", "a"]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(
        out_str(&out),
        "Merge made by the 'ort' strategy.\n a | 1 +\n 1 file changed, 1 insertion(+)\n create mode 100644 a\n"
    );
    assert_ne!(rev(BIN, &r, "HEAD^{tree}"), before_tree);
}

/// An unknown name in `pull.twohead` is `get_strategy()`'s error: two lines on
/// stderr, exit 1, nothing on stdout — and nothing touched, because the strategy
/// list is resolved before the merge starts.
#[test]
fn an_unknown_twohead_strategy_fails_the_merge_before_it_starts() {
    let r = fixture(BIN, "twohead-bogus");
    let before = rev(BIN, &r, "HEAD");
    let out = run(BIN, &r, &["-c", "pull.twohead=bogus", "merge", "a"]);

    assert_eq!(code(&out), 1, "{}", err(&out));
    assert_eq!(err(&out), UNKNOWN);
    assert_eq!(out_str(&out), "");
    assert_eq!(rev(BIN, &r, "HEAD"), before, "the refusal is before any ref mutation");
    assert!(!r.join(".git/MERGE_HEAD").exists(), "no merge was left in progress");
    assert_eq!(out_str(&run(BIN, &r, &["status", "--porcelain"])), "");

    if let Some((_, sout)) = stock_run("twohead-bogus", &before, &["-c", "pull.twohead=bogus", "merge", "a"]) {
        assert_eq!(err(&sout), err(&out), "stderr differs from stock");
        assert_eq!(out_str(&sout), out_str(&out));
        assert_eq!(code(&sout), code(&out), "stock exits 1 here, not 128");
    }
}

/// `pull.octopus` replaces `octopus` for a merge with more than one head. `ours`
/// over two heads still records both as parents (three in total) while keeping our
/// tree, which is what distinguishes it from the octopus that would otherwise run.
#[test]
fn octopus_config_picks_the_strategy_for_a_multi_head_merge() {
    let r = fixture(BIN, "octopus-ours");
    let before_tree = rev(BIN, &r, "HEAD^{tree}");
    let base = rev(BIN, &r, "HEAD");
    let out = run(BIN, &r, &["-c", "pull.octopus=ours", "merge", "a", "b"]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(out_str(&out), "Merge made by the 'ours' strategy.\n");
    assert_eq!(err(&out), "");
    assert_eq!(rev(BIN, &r, "HEAD^{tree}"), before_tree);
    assert_eq!(head_parents(BIN, &r), 3, "both heads are still recorded as parents");

    if let Some((sr, sout)) = stock_run("octopus-ours", &base, &["-c", "pull.octopus=ours", "merge", "a", "b"]) {
        let bin = stock().unwrap();
        assert_eq!(out_str(&sout), out_str(&out), "stdout differs from stock");
        assert_eq!(err(&sout), err(&out), "stderr differs from stock");
        assert_eq!(rev(bin, &sr, "HEAD"), rev(BIN, &r, "HEAD"), "stock built a different merge commit");
    }
}

/// The same two-head merge with no config: the built-in octopus, which narrates
/// each head it folds in. Control for the test above.
#[test]
fn the_same_multi_head_merge_without_the_config_uses_the_octopus() {
    let r = fixture(BIN, "octopus-control");
    let out = run(BIN, &r, &["merge", "a", "b"]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(
        out_str(&out),
        "Trying simple merge with a\n\
         Trying simple merge with b\n\
         Merge made by the 'octopus' strategy.\n\
         \x20a | 1 +\n\
         \x20b | 1 +\n\
         \x202 files changed, 2 insertions(+)\n\
         \x20create mode 100644 a\n\
         \x20create mode 100644 b\n"
    );
    assert_eq!(head_parents(BIN, &r), 3);
}

/// An unknown name in `pull.octopus` fails the same way, on the head count that
/// key governs.
#[test]
fn an_unknown_octopus_strategy_fails_the_merge_before_it_starts() {
    let r = fixture(BIN, "octopus-bogus");
    let before = rev(BIN, &r, "HEAD");
    let out = run(BIN, &r, &["-c", "pull.octopus=bogus", "merge", "a", "b"]);

    assert_eq!(code(&out), 1, "{}", err(&out));
    assert_eq!(err(&out), UNKNOWN);
    assert_eq!(out_str(&out), "");
    assert_eq!(rev(BIN, &r, "HEAD"), before);
    assert!(!r.join(".git/MERGE_HEAD").exists());

    if let Some((_, sout)) = stock_run("octopus-bogus", &before, &["-c", "pull.octopus=bogus", "merge", "a", "b"]) {
        assert_eq!(err(&sout), err(&out), "stderr differs from stock");
        assert_eq!(code(&sout), code(&out));
    }
}

/// Each key governs exactly one head count. A value bogus enough to abort the
/// merge is the sharpest probe available: if either key were consulted for the
/// other count, these merges would die instead of succeeding.
#[test]
fn neither_key_reaches_the_other_head_count() {
    for (key, argv, expected) in [
        (
            "pull.twohead=bogus",
            vec!["merge", "a", "b"],
            "Trying simple merge with a\n\
             Trying simple merge with b\n\
             Merge made by the 'octopus' strategy.\n\
             \x20a | 1 +\n\
             \x20b | 1 +\n\
             \x202 files changed, 2 insertions(+)\n\
             \x20create mode 100644 a\n\
             \x20create mode 100644 b\n",
        ),
        (
            "pull.octopus=bogus",
            vec!["merge", "a"],
            "Merge made by the 'ort' strategy.\n\
             \x20a | 1 +\n\
             \x201 file changed, 1 insertion(+)\n\
             \x20create mode 100644 a\n",
        ),
    ] {
        let tag = format!("cross-{}", &key[5..key.find('=').unwrap()]);
        let r = fixture(BIN, &tag);
        let base = rev(BIN, &r, "HEAD");
        let mut args = vec!["-c", key];
        args.extend_from_slice(&argv);
        let out = run(BIN, &r, &args);

        assert_eq!(code(&out), 0, "{key} {argv:?}: {}", err(&out));
        assert_eq!(out_str(&out), expected, "{key} {argv:?}");
        assert_eq!(err(&out), "", "{key} {argv:?}");

        if let Some((_, sout)) = stock_run(&tag, &base,  &args) {
            assert_eq!(out_str(&sout), out_str(&out), "{key} {argv:?}: stdout differs from stock");
            assert_eq!(code(&sout), code(&out), "{key} {argv:?}");
        }
    }
}

/// `if (!use_strategies)`: an explicit `-s` is the whole test, so the config is
/// never read and a value that would otherwise abort the merge is inert.
#[test]
fn an_explicit_strategy_flag_stops_the_config_being_read() {
    let r = fixture(BIN, "explicit-s");
    let base = rev(BIN, &r, "HEAD");
    let args = ["-c", "pull.twohead=bogus", "merge", "-s", "ort", "a"];
    let out = run(BIN, &r, &args);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(
        out_str(&out),
        "Merge made by the 'ort' strategy.\n a | 1 +\n 1 file changed, 1 insertion(+)\n create mode 100644 a\n"
    );

    if let Some((_, sout)) = stock_run("explicit-s", &base, &args) {
        assert_eq!(out_str(&sout), out_str(&out), "stdout differs from stock");
        assert_eq!(code(&sout), code(&out));
    }
}

/// The value is a space-separated *list*, not one name. `octopus resolve` over a
/// single head selects neither `ort`'s message nor an error: both listed strategies
/// lack `NO_TRIVIAL`, so the trivial in-index merge ahead of the dispatch takes the
/// merge — a code path the unconfigured default never reaches, which is what makes
/// this observable at all.
#[test]
fn the_value_is_a_space_separated_strategy_list() {
    let r = fixture(BIN, "list");
    let base = rev(BIN, &r, "HEAD");
    let args = ["-c", "pull.twohead=octopus resolve", "merge", "a"];
    let out = run(BIN, &r, &args);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(
        out_str(&out),
        "Trying really trivial in-index merge...\n\
         Wonderful.\n\
         In-index merge\n\
         \x20a | 1 +\n\
         \x201 file changed, 1 insertion(+)\n\
         \x20create mode 100644 a\n"
    );
    assert_eq!(head_parents(BIN, &r), 2);

    if let Some((sr, sout)) = stock_run("list", &base, &args) {
        let bin = stock().unwrap();
        assert_eq!(out_str(&sout), out_str(&out), "stdout differs from stock");
        assert_eq!(err(&sout), err(&out), "stderr differs from stock");
        assert_eq!(rev(bin, &sr, "HEAD"), rev(BIN, &r, "HEAD"));
    }
}

/// A bogus name anywhere in the list aborts before any strategy runs, even when a
/// usable one follows it — the list is resolved as a whole, not tried one name at a
/// time until something parses.
#[test]
fn an_unknown_name_anywhere_in_the_list_aborts() {
    let r = fixture(BIN, "list-bogus");
    let before = rev(BIN, &r, "HEAD");
    let args = ["-c", "pull.twohead=bogus ours", "merge", "a"];
    let out = run(BIN, &r, &args);

    assert_eq!(code(&out), 1, "{}", err(&out));
    assert_eq!(err(&out), UNKNOWN);
    assert_eq!(rev(BIN, &r, "HEAD"), before);

    if let Some((_, sout)) = stock_run("list-bogus", &before, &args) {
        assert_eq!(err(&sout), err(&out), "stderr differs from stock");
        assert_eq!(code(&sout), code(&out));
    }
}

/// An **empty** value is configured, not absent. `git_config_string()` stores
/// `""`, `add_strategies()` splits it into one empty field, and
/// `get_strategy("")` names no strategy — so the merge fails before it starts,
/// with the same two lines an unknown name produces. Treating it as unset would
/// silently merge with `ort` instead, which is the failure this pins.
#[test]
fn an_empty_strategy_config_is_configured_not_unset() {
    const EMPTY: &str = "Could not find merge strategy ''.\n\
                         Available strategies are: octopus ours recursive resolve subtree.\n";
    for value in ["pull.twohead=", "pull.twohead= "] {
        let r = fixture(BIN, &format!("twohead-empty-{}", value.len()));
        let before = rev(BIN, &r, "HEAD");
        let out = run(BIN, &r, &["-c", value, "merge", "a"]);

        assert_eq!(code(&out), 1, "{}", err(&out));
        assert_eq!(err(&out), EMPTY, "value {value:?}");
        assert_eq!(out_str(&out), "");
        assert_eq!(rev(BIN, &r, "HEAD"), before, "the merge ran anyway");

        if let Some((_, sout)) = stock_run(&format!("twohead-empty-{}", value.len()), &before, &["-c", value, "merge", "a"]) {
            assert_eq!(err(&sout), err(&out), "stderr differs from stock for {value:?}");
            assert_eq!(code(&sout), code(&out));
        }
    }
}
