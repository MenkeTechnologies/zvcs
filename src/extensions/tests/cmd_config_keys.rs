//! The per-command config callbacks in `crate::cmd_config` — `grep_cmd_config`,
//! `grep_config`, `git_blame_config`, `git_fetch_config`, `repack_config` and
//! `gc_config` — plus the two `builtin/pull.c` readers and
//! `parse_push_recurse_submodules_arg`.
//!
//! Every expectation is a literal captured from a differential run against git
//! 2.55.0 in the fixture below. Two things are being pinned:
//!
//!   * the bytes and the exit code of each refusal, and
//!   * *when* it happens relative to the keys around it — which for `gc` is a
//!     fixed source order rather than the config order, and for the commit-listing
//!     commands puts `grep.*` last no matter where it is written.

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

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-cmdcfg-{tag}-{}", std::process::id()));
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

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

/// `grep_config()` (grep.c:59-111) as `git grep` reaches it, through
/// `grep_cmd_config` (builtin/grep.c:297-327).
#[test]
fn the_grep_keys_are_refused_by_grep() {
    let (repo, home) = fixture("grep");

    let cases: &[(&str, &str)] = &[
        (
            "grep.extendedRegexp=bogus",
            "fatal: bad boolean config value 'bogus' for 'grep.extendedregexp'\n",
        ),
        (
            "grep.lineNumber=bogus",
            "fatal: bad boolean config value 'bogus' for 'grep.linenumber'\n",
        ),
        ("grep.column=bogus", "fatal: bad boolean config value 'bogus' for 'grep.column'\n"),
        ("grep.fullName=bogus", "fatal: bad boolean config value 'bogus' for 'grep.fullname'\n"),
        ("grep.patternType=bogus", "fatal: bad grep.patterntype argument: bogus\n"),
        ("color.grep=bogus", "fatal: bad boolean config value 'bogus' for 'color.grep'\n"),
        (
            "color.grep.lineNumber=nosuchcolor",
            "error: invalid color value: nosuchcolor\n\
             fatal: unable to parse 'color.grep.linenumber' from command-line config\n",
        ),
        (
            "color.grep.match=nosuchcolor",
            "error: invalid color value: nosuchcolor\n\
             fatal: unable to parse 'color.grep.match' from command-line config\n",
        ),
    ];

    for (assignment, want) in cases {
        let out = run(&repo, &home, &["-c", assignment, "grep", "one"]);
        assert_eq!(stderr(&out), *want, "for {assignment}");
        assert_eq!(code(&out), FATAL, "for {assignment}");
    }

    // `color.grep.<slot>` is the one colour-slot arm in git that refuses an
    // *unknown* slot instead of ignoring it — `if (i < 0) return -1`
    // (grep.c:103-104), with no `error()` of its own, so the origin line is the
    // whole diagnostic.
    let out = run(&repo, &home, &["-c", "color.grep.nosuchslot=red", "grep", "one"]);
    assert_eq!(
        stderr(&out),
        "fatal: unable to parse 'color.grep.nosuchslot' from command-line config\n"
    );
    assert_eq!(code(&out), FATAL);

    // The five pattern types and a readable colour are accepted.
    for good in ["default", "basic", "extended", "fixed", "perl"] {
        let out = run(&repo, &home, &["-c", &format!("grep.patternType={good}"), "grep", "one"]);
        assert_eq!(stderr(&out), "", "for {good}");
    }
    let out = run(&repo, &home, &["-c", "color.grep.match=red", "grep", "one"]);
    assert_eq!(stderr(&out), "");
}

/// `repo_init_revisions()` runs a *second* pass over `grep_config` alone for the
/// commit-listing commands, and it runs after the command's own callback — so the
/// `grep.*` key loses to any other bad key regardless of config order.
#[test]
fn the_grep_keys_reach_the_commit_listing_commands_last() {
    let (repo, home) = fixture("grep-revs");

    for verb in [
        vec!["log", "-1"],
        vec!["show"],
        vec!["whatchanged", "-1"],
        vec!["format-patch", "-1", "--stdout"],
        vec!["range-diff", "HEAD~1...HEAD"],
    ] {
        let mut argv = vec!["-c", "grep.patternType=bogus"];
        argv.extend(verb.iter().copied());
        let out = run(&repo, &home, &argv);
        assert_eq!(
            stderr(&out),
            "fatal: bad grep.patterntype argument: bogus\n",
            "for {verb:?}"
        );
    }

    // Verbs that never build a rev walk do not read them at all.
    for verb in [vec!["rev-list", "HEAD"], vec!["diff-tree", "HEAD"], vec!["status"]] {
        let mut argv = vec!["-c", "grep.patternType=bogus"];
        argv.extend(verb.iter().copied());
        let out = run(&repo, &home, &argv);
        assert_eq!(stderr(&out), "", "for {verb:?}");
    }

    // The order is fixed rather than config-ordered: the primary callback's key
    // reports first either way.
    for pair in [
        ["grep.patternType=bogus", "log.showRoot=bogus"],
        ["log.showRoot=bogus", "grep.patternType=bogus"],
    ] {
        let out = run(&repo, &home, &["-c", pair[0], "-c", pair[1], "log", "-1"]);
        assert_eq!(
            stderr(&out),
            "fatal: bad boolean config value 'bogus' for 'log.showroot'\n",
            "for {pair:?}"
        );
    }

    // For `git grep` the same two keys *are* one chain, so the config order wins.
    let out = run(
        &repo,
        &home,
        &["-c", "grep.patternType=bogus", "-c", "core.ignorecase=bogus", "grep", "one"],
    );
    assert_eq!(stderr(&out), "fatal: bad grep.patterntype argument: bogus\n");
    let out = run(
        &repo,
        &home,
        &["-c", "core.ignorecase=bogus", "-c", "grep.patternType=bogus", "grep", "one"],
    );
    assert_eq!(stderr(&out), "fatal: bad boolean config value 'bogus' for 'core.ignorecase'\n");
}

// ---------------------------------------------------------------------------
// blame
// ---------------------------------------------------------------------------

/// `git_blame_config()` (builtin/blame.c:714-805), for `blame` and `annotate`.
#[test]
fn the_blame_keys_are_refused_by_blame_and_annotate() {
    let (repo, home) = fixture("blame");

    let cases: &[(&str, &str)] = &[
        ("blame.showRoot=bogus", "fatal: bad boolean config value 'bogus' for 'blame.showroot'\n"),
        (
            "blame.blankBoundary=bogus",
            "fatal: bad boolean config value 'bogus' for 'blame.blankboundary'\n",
        ),
        (
            "blame.showEmail=bogus",
            "fatal: bad boolean config value 'bogus' for 'blame.showemail'\n",
        ),
        (
            "blame.markUnblamableLines=bogus",
            "fatal: bad boolean config value 'bogus' for 'blame.markunblamablelines'\n",
        ),
        (
            "blame.markIgnoredLines=bogus",
            "fatal: bad boolean config value 'bogus' for 'blame.markignoredlines'\n",
        ),
        (
            "blame.ignoreRevsFile=~zvcs-no-such-user/r",
            "fatal: failed to expand user dir in: '~zvcs-no-such-user/r'\n",
        ),
        (
            "diff.algorithm=bogus",
            "error: unknown value for config 'diff.algorithm': bogus\n\
             fatal: unable to parse 'diff.algorithm' from command-line config\n",
        ),
        (
            "diff.indentHeuristic=bogus",
            "fatal: bad boolean config value 'bogus' for 'diff.indentheuristic'\n",
        ),
    ];

    for (assignment, want) in cases {
        for verb in ["blame", "annotate"] {
            let out = run(&repo, &home, &["-c", assignment, verb, "f"]);
            assert_eq!(stderr(&out), *want, "for -c {assignment} {verb}");
            assert_eq!(code(&out), FATAL, "for -c {assignment} {verb}");
        }
        // `log` installs a different callback and does not see the `blame.*` half.
        if assignment.starts_with("blame.") {
            let out = run(&repo, &home, &["-c", assignment, "log", "-1"]);
            assert_eq!(stderr(&out), "", "log must ignore {assignment}");
        }
    }

    // The chain ends in `git_default_config`, so the config order decides between
    // a `blame.*` key and a `core.*` one.
    let out = run(
        &repo,
        &home,
        &["-c", "blame.showRoot=bogus", "-c", "core.ignorecase=bogus", "blame", "f"],
    );
    assert_eq!(stderr(&out), "fatal: bad boolean config value 'bogus' for 'blame.showroot'\n");
    let out = run(
        &repo,
        &home,
        &["-c", "core.ignorecase=bogus", "-c", "blame.showRoot=bogus", "blame", "f"],
    );
    assert_eq!(stderr(&out), "fatal: bad boolean config value 'bogus' for 'core.ignorecase'\n");
}

// ---------------------------------------------------------------------------
// fetch and repack
// ---------------------------------------------------------------------------

/// `git_fetch_config()` (builtin/fetch.c:115-177).
#[test]
fn the_fetch_keys_are_refused_by_fetch() {
    let (repo, home) = fixture("fetch");

    let cases: &[(&str, &str)] = &[
        ("fetch.all=bogus", "fatal: bad boolean config value 'bogus' for 'fetch.all'\n"),
        ("fetch.prune=bogus", "fatal: bad boolean config value 'bogus' for 'fetch.prune'\n"),
        (
            "fetch.pruneTags=bogus",
            "fatal: bad boolean config value 'bogus' for 'fetch.prunetags'\n",
        ),
        (
            "fetch.showForcedUpdates=bogus",
            "fatal: bad boolean config value 'bogus' for 'fetch.showforcedupdates'\n",
        ),
        (
            "fetch.parallel=bogus",
            "fatal: bad numeric config value 'bogus' for 'fetch.parallel': invalid unit\n",
        ),
        ("fetch.parallel=-1", "fatal: fetch.parallel cannot be negative\n"),
        (
            "submodule.fetchJobs=-1",
            "fatal: negative values not allowed for submodule.fetchJobs\n",
        ),
        (
            "fetch.recurseSubmodules=bogus",
            "fatal: bad fetch.recursesubmodules argument: bogus\n",
        ),
        ("fetch.output=bogus", "fatal: invalid value for 'fetch.output': 'bogus'\n"),
    ];

    for (assignment, want) in cases {
        let out = run(&repo, &home, &["-c", assignment, "fetch"]);
        assert_eq!(stderr(&out), *want, "for {assignment}");
        assert_eq!(code(&out), FATAL, "for {assignment}");
    }

    // `fetch.recurseSubmodules` takes the full boolean grammar plus `on-demand`.
    for good in ["true", "false", "on-demand", "1", "0"] {
        let out = run(&repo, &home, &["-c", &format!("fetch.recurseSubmodules={good}"), "fetch"]);
        assert!(
            !stderr(&out).contains("bad fetch.recursesubmodules"),
            "for {good}: {}",
            stderr(&out)
        );
    }
}

/// `repack_config()` (builtin/repack.c:55-113).
#[test]
fn the_repack_keys_are_refused_by_repack() {
    let (repo, home) = fixture("repack");

    let cases: &[(&str, &str)] = &[
        (
            "repack.useDeltaBaseOffset=bogus",
            "fatal: bad boolean config value 'bogus' for 'repack.usedeltabaseoffset'\n",
        ),
        (
            "repack.packKeptObjects=bogus",
            "fatal: bad boolean config value 'bogus' for 'repack.packkeptobjects'\n",
        ),
        (
            "repack.writeBitmaps=bogus",
            "fatal: bad boolean config value 'bogus' for 'repack.writebitmaps'\n",
        ),
        (
            "pack.writeBitmaps=bogus",
            "fatal: bad boolean config value 'bogus' for 'pack.writebitmaps'\n",
        ),
        (
            "repack.useDeltaIslands=bogus",
            "fatal: bad boolean config value 'bogus' for 'repack.usedeltaislands'\n",
        ),
        (
            "repack.updateServerInfo=bogus",
            "fatal: bad boolean config value 'bogus' for 'repack.updateserverinfo'\n",
        ),
        (
            "repack.midxSplitFactor=bogus",
            "fatal: bad numeric config value 'bogus' for 'repack.midxsplitfactor': invalid unit\n",
        ),
    ];

    for (assignment, want) in cases {
        let out = run(&repo, &home, &["-c", assignment, "repack"]);
        assert_eq!(stderr(&out), *want, "for {assignment}");
        assert_eq!(code(&out), FATAL, "for {assignment}");
    }
}

// ---------------------------------------------------------------------------
// gc
// ---------------------------------------------------------------------------

/// `gc_config()` (builtin/gc.c:176-233) is a sequence of targeted lookups, not a
/// callback, so within it the *source* order decides which key reports — and the
/// whole block runs before the `git_default_config` walk at :232.
#[test]
fn the_gc_keys_report_in_source_order_and_before_the_default_walk() {
    let (repo, home) = fixture("gc");

    let cases: &[(&str, &str)] = &[
        ("gc.packRefs=bogus", "fatal: bad boolean config value 'bogus' for 'gc.packrefs'\n"),
        (
            "gc.reflogExpire=bogus",
            "fatal: failed to parse 'gc.reflogexpire' value 'bogus'\n",
        ),
        (
            "gc.aggressiveWindow=bogus",
            "fatal: bad numeric config value 'bogus' for 'gc.aggressivewindow': invalid unit\n",
        ),
        (
            "gc.aggressiveDepth=bogus",
            "fatal: bad numeric config value 'bogus' for 'gc.aggressivedepth': invalid unit\n",
        ),
        (
            "gc.auto=bogus",
            "fatal: bad numeric config value 'bogus' for 'gc.auto': invalid unit\n",
        ),
        (
            "gc.autoPackLimit=bogus",
            "fatal: bad numeric config value 'bogus' for 'gc.autopacklimit': invalid unit\n",
        ),
        ("gc.autoDetach=bogus", "fatal: bad boolean config value 'bogus' for 'gc.autodetach'\n"),
        ("gc.cruftPacks=bogus", "fatal: bad boolean config value 'bogus' for 'gc.cruftpacks'\n"),
        (
            "gc.pruneExpire=bogus",
            "error: Invalid gc.pruneexpire: 'bogus'\n\
             fatal: unable to parse 'gc.pruneexpire' from command-line config\n",
        ),
        (
            "gc.worktreePruneExpire=bogus",
            "error: Invalid gc.worktreepruneexpire: 'bogus'\n\
             fatal: unable to parse 'gc.worktreepruneexpire' from command-line config\n",
        ),
        (
            "gc.bigPackThreshold=bogus",
            "fatal: bad numeric config value 'bogus' for 'gc.bigpackthreshold': invalid unit\n",
        ),
        (
            "pack.deltaCacheSize=bogus",
            "fatal: bad numeric config value 'bogus' for 'pack.deltacachesize': invalid unit\n",
        ),
        (
            "core.deltaBaseCacheLimit=bogus",
            "fatal: bad numeric config value 'bogus' for 'core.deltabasecachelimit': invalid unit\n",
        ),
    ];

    for (assignment, want) in cases {
        let out = run(&repo, &home, &["-c", assignment, "gc", "--auto"]);
        assert_eq!(stderr(&out), *want, "for {assignment}");
        assert_eq!(code(&out), FATAL, "for {assignment}");
    }

    // The whole block precedes the default walk, so a `gc.*` key wins over a
    // `core.*` one written before it.
    for pair in [
        ["core.ignorecase=bogus", "gc.auto=bogus"],
        ["gc.auto=bogus", "core.ignorecase=bogus"],
    ] {
        let out = run(&repo, &home, &["-c", pair[0], "-c", pair[1], "gc", "--auto"]);
        assert_eq!(
            stderr(&out),
            "fatal: bad numeric config value 'bogus' for 'gc.auto': invalid unit\n",
            "for {pair:?}"
        );
    }

    // And within the block the source order decides: `gc.packRefs` is read at
    // builtin/gc.c:182 and `gc.auto` at :195, so the former reports even when it
    // is written second.
    let out = run(
        &repo,
        &home,
        &["-c", "gc.auto=bogus", "-c", "gc.packRefs=bogus", "gc", "--auto"],
    );
    assert_eq!(stderr(&out), "fatal: bad boolean config value 'bogus' for 'gc.packrefs'\n");
    // `gc.auto` (:195) beats `gc.autoPackLimit` (:196) and `gc.pruneExpire` (:201).
    for other in ["gc.autoPackLimit=bogus", "gc.pruneExpire=bogus"] {
        let out = run(&repo, &home, &["-c", other, "-c", "gc.auto=bogus", "gc", "--auto"]);
        assert_eq!(
            stderr(&out),
            "fatal: bad numeric config value 'bogus' for 'gc.auto': invalid unit\n",
            "for {other}"
        );
    }

    // `gc.packRefs` takes one extra word, and the expiry keys take `now`, `never`
    // and any moment in the past.
    let out = run(&repo, &home, &["-c", "gc.packRefs=notbare", "gc", "--auto"]);
    assert_eq!(stderr(&out), "");
    for good in ["now", "never", "2.weeks.ago"] {
        let out = run(&repo, &home, &["-c", &format!("gc.pruneExpire={good}"), "gc", "--auto"]);
        assert_eq!(stderr(&out), "", "for {good}");
    }

    // `gc_config_is_timestamp_never()`'s `&&` short-circuits, so the second
    // reflog key is only read when the first resolved to "never" — which is why
    // a bogus value there alone runs clean.
    let out = run(&repo, &home, &["-c", "gc.reflogExpireUnreachable=bogus", "gc", "--auto"]);
    assert_eq!(stderr(&out), "");
    let out = run(
        &repo,
        &home,
        &[
            "-c",
            "gc.reflogExpire=never",
            "-c",
            "gc.reflogExpireUnreachable=bogus",
            "gc",
            "--auto",
        ],
    );
    assert_eq!(
        stderr(&out),
        "fatal: failed to parse 'gc.reflogexpireunreachable' value 'bogus'\n"
    );
    assert_eq!(code(&out), FATAL);
}

// ---------------------------------------------------------------------------
// pull and push
// ---------------------------------------------------------------------------

/// `parse_config_rebase()` (builtin/pull.c:41-54) names the key it was reading,
/// and its `fatal` argument decides whether the run dies (128) or the option
/// parser ends it (129).
#[test]
fn the_pull_rebase_value_names_the_key_it_came_from() {
    let (repo, home) = fixture("pull-rebase");

    let out = run(&repo, &home, &["-c", "pull.rebase=bogus", "pull"]);
    assert_eq!(stderr(&out), "fatal: invalid value for 'pull.rebase': 'bogus'\n");
    assert_eq!(code(&out), FATAL);

    let out = run(&repo, &home, &["-c", "branch.main.rebase=bogus", "pull"]);
    assert_eq!(stderr(&out), "fatal: invalid value for 'branch.main.rebase': 'bogus'\n");
    assert_eq!(code(&out), FATAL);

    let out = run(&repo, &home, &["pull", "--rebase=bogus"]);
    assert_eq!(stderr(&out), "error: invalid value for '--rebase': 'bogus'\n");
    assert_eq!(code(&out), 129);

    // `preserve` prints its own `error()` first and then falls through to invalid.
    let out = run(&repo, &home, &["-c", "pull.rebase=preserve", "pull"]);
    assert_eq!(
        stderr(&out),
        "error: preserve: 'preserve' superseded by 'merges'\n\
         fatal: invalid value for 'pull.rebase': 'preserve'\n"
    );
    assert_eq!(code(&out), FATAL);

    // `rebase_parse_value` takes the full boolean grammar and the one-letter mode
    // names, so none of these is an invalid value.
    for good in ["true", "false", "1", "0", "merges", "m", "interactive", "i"] {
        let out = run(&repo, &home, &["-c", &format!("pull.rebase={good}"), "pull"]);
        assert!(
            !stderr(&out).contains("invalid value for"),
            "for {good}: {}",
            stderr(&out)
        );
    }
}

/// `config_get_ff()` (builtin/pull.c:171-190) runs before the integration policy
/// and before the "no tracking information" refusal.
#[test]
fn pull_ff_is_validated_before_anything_else_pull_would_say() {
    let (repo, home) = fixture("pull-ff");

    let out = run(&repo, &home, &["-c", "pull.ff=bogus", "pull"]);
    assert_eq!(stderr(&out), "fatal: invalid value for 'pull.ff': 'bogus'\n");
    assert_eq!(code(&out), FATAL);

    // It beats `pull.rebase` in either config order…
    for pair in [
        ["pull.rebase=bogus", "pull.ff=bogus"],
        ["pull.ff=bogus", "pull.rebase=bogus"],
    ] {
        let out = run(&repo, &home, &["-c", pair[0], "-c", pair[1], "pull"]);
        assert_eq!(
            stderr(&out),
            "fatal: invalid value for 'pull.ff': 'bogus'\n",
            "for {pair:?}"
        );
    }
    // …and loses to the config-parse-time keys, which are read before `cmd_pull`
    // runs at all.
    let out = run(&repo, &home, &["-c", "pull.ff=bogus", "-c", "core.ignorecase=bogus", "pull"]);
    assert_eq!(stderr(&out), "fatal: bad boolean config value 'bogus' for 'core.ignorecase'\n");

    for good in ["true", "false", "only", "1", "0"] {
        let out = run(&repo, &home, &["-c", &format!("pull.ff={good}"), "pull"]);
        assert!(
            !stderr(&out).contains("invalid value for 'pull.ff'"),
            "for {good}: {}",
            stderr(&out)
        );
    }
}

/// `parse_push_recurse()` (submodule-config.c:498-526) names its `opt` argument,
/// and the two callers pass different names.
#[test]
fn push_recurse_submodules_names_the_config_key_or_the_option() {
    let (repo, home) = fixture("push-recurse");

    let out = run(&repo, &home, &["-c", "push.recurseSubmodules=bogus", "push"]);
    assert_eq!(stderr(&out), "fatal: bad push.recursesubmodules argument: bogus\n");
    assert_eq!(code(&out), FATAL);

    let out = run(&repo, &home, &["push", "--recurse-submodules=bogus"]);
    assert_eq!(stderr(&out), "fatal: bad recurse-submodules argument: bogus\n");
    assert_eq!(code(&out), FATAL);

    // There is no plain "on" for pushing, so a boolean-true value is refused too.
    let out = run(&repo, &home, &["-c", "push.recurseSubmodules=true", "push"]);
    assert_eq!(stderr(&out), "fatal: bad push.recursesubmodules argument: true\n");
    assert_eq!(code(&out), FATAL);
}
