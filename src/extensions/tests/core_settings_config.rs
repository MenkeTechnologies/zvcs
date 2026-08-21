//! The repository-settings and default-config keys, pinned to what git 2.55.0 was
//! observed to do with each value.
//!
//! Every expectation is a literal taken from a differential run against stock git
//! (`/opt/homebrew/bin/git -c <key>=<value> <cmd>` in a one-commit fixture), so
//! these run headless with nothing on `PATH` but the binary under test.
//!
//! Covered:
//!   * `core.packedGitLimit` / `core.packedGitWindowSize` — validated by
//!     `prepare_repo_settings()` (`crate::repo_settings`), last-value-wins.
//!   * `feature.manyFiles` / `feature.experimental` — the same block's two
//!     cascading macros, plus `manyFiles`' one honored effect: it is the default
//!     for `index.skipHash`, so the index trailer comes out zeroed.
//!   * `core.createObject` / `sparse.expectFilesOutsideOfPatterns` — validated
//!     while the config is *parsed* (`crate::default_config`), so they refuse a
//!     wider set of commands and refuse every occurrence rather than the last.
//!   * `checkout.workers` / `checkout.thresholdForParallelism` — read by
//!     `check_updates()`, so only the command lines that update the worktree.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// git's `die()` exit status.
const FATAL: i32 = 128;

/// Run the binary under test in `repo` with an isolated environment, so no
/// ambient global or system config can reach the run.
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

/// Same, asserting success — used to build fixtures, never as behaviour under test.
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

/// A one-commit repository on `main` with a second branch, plus an isolated empty
/// `HOME`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-coresettings-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::write(repo.join("f"), "a\n").unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    ok(&repo, &home, &["config", "user.email", "alice@example.com"]);
    ok(&repo, &home, &["config", "user.name", "Alice"]);
    ok(&repo, &home, &["add", "f"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c0"]);
    ok(&repo, &home, &["branch", "b2"]);
    (repo, home)
}

/// The 20 trailing bytes of the index, as hex — git's checksum, or twenty zeroes
/// when `index.skipHash` (or something defaulting it) is on.
fn index_trailer(repo: &Path) -> String {
    let bytes = std::fs::read(repo.join(".git/index")).unwrap();
    bytes[bytes.len() - 20..]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// prepare_repo_settings(): core.packedGitLimit, core.packedGitWindowSize
// ---------------------------------------------------------------------------

#[test]
fn packed_git_keys_reject_a_bad_value_the_way_git_does() {
    let (repo, home) = fixture("packed");

    // `git -c core.packedGitLimit=bogus status --porcelain`
    let out = run(&repo, &home, &["-c", "core.packedGitLimit=bogus", "status", "--porcelain"]);
    assert_eq!(
        stderr(&out),
        "fatal: bad numeric config value 'bogus' for 'core.packedgitlimit': invalid unit\n"
    );
    assert_eq!(code(&out), FATAL);

    // Same for the window size, and on a different verb in the same gate.
    let out = run(&repo, &home, &["-c", "core.packedGitWindowSize=bogus", "log", "--oneline"]);
    assert_eq!(
        stderr(&out),
        "fatal: bad numeric config value 'bogus' for 'core.packedgitwindowsize': invalid unit\n"
    );
    assert_eq!(code(&out), FATAL);

    // A value git can read is accepted, suffix and all — `1m` is 1 MiB, and a
    // window size that is not a multiple of `pagesize * 2` is rounded rather than
    // rejected (repo-settings.c:147-152).
    let out = run(
        &repo,
        &home,
        &[
            "-c",
            "core.packedGitLimit=1m",
            "-c",
            "core.packedGitWindowSize=3000",
            "status",
            "--porcelain",
        ],
    );
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());
}

#[test]
fn packed_git_keys_are_read_from_the_config_file_with_gits_origin_clause() {
    let (repo, home) = fixture("packed-file");
    ok(&repo, &home, &["config", "core.packedGitLimit", "bogus"]);

    // A file-backed value carries ` in file <path>`; a `-c` one does not.
    let out = run(&repo, &home, &["status", "--porcelain"]);
    assert_eq!(
        stderr(&out),
        "fatal: bad numeric config value 'bogus' for 'core.packedgitlimit' in file .git/config: invalid unit\n"
    );
    assert_eq!(code(&out), FATAL);

    // `git config` still round-trips the unreadable value: it reads the file, it
    // does not build the settings block.
    let out = ok(&repo, &home, &["config", "--get", "core.packedGitLimit"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "bogus\n");
}

#[test]
fn settings_keys_are_last_value_wins_unlike_the_parse_time_ones() {
    let (repo, home) = fixture("last-wins");

    // `repo_config_get_ulong` is a lookup, not a callback: only the winning value
    // is ever parsed, so an earlier unreadable one is never seen.
    let out = run(
        &repo,
        &home,
        &[
            "-c",
            "core.packedGitLimit=bogus",
            "-c",
            "core.packedGitLimit=1m",
            "status",
            "--porcelain",
        ],
    );
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());
}

#[test]
fn settings_gate_spares_the_verbs_git_spares() {
    let (repo, home) = fixture("gate");

    // `git branch` never asks for the settings block — measured against stock,
    // which lists the branches instead of refusing.
    let out = run(&repo, &home, &["-c", "core.packedGitLimit=bogus", "branch"]);
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("main"));

    // Neither does `git config`, which is how a bad value stays fixable.
    let out = run(&repo, &home, &["-c", "core.packedGitLimit=bogus", "config", "--get", "user.name"]);
    assert!(out.status.success());
}

// ---------------------------------------------------------------------------
// feature.manyFiles / feature.experimental
// ---------------------------------------------------------------------------

#[test]
fn feature_macros_reject_a_bad_boolean_without_an_origin_clause() {
    let (repo, home) = fixture("feature-bad");

    for key in ["feature.manyFiles", "feature.experimental"] {
        let out = run(&repo, &home, &["-c", &format!("{key}=bogus"), "status", "--porcelain"]);
        assert_eq!(
            stderr(&out),
            format!(
                "fatal: bad boolean config value 'bogus' for '{}'\n",
                key.to_lowercase()
            ),
            "for {key}"
        );
        assert_eq!(code(&out), FATAL, "for {key}");
    }

    // `git_config_bool` has no `key_value_info`, so even a file-backed value gets
    // the bare message — no ` in file .git/config` clause.
    ok(&repo, &home, &["config", "feature.manyFiles", "bogus"]);
    let out = run(&repo, &home, &["status", "--porcelain"]);
    assert_eq!(
        stderr(&out),
        "fatal: bad boolean config value 'bogus' for 'feature.manyfiles'\n"
    );
}

/// Stage a *new* path, so `update-index` always has an entry change to persist.
///
/// Rewriting an existing file is not enough: `f` is two bytes both before and
/// after, so whether the stat comparison calls it modified depends on filesystem
/// mtime granularity, and an unmodified `--add` writes no index at all. Adding a
/// path the index has never seen is unconditionally dirty.
fn stage_a_new_file(repo: &Path, home: &Path, name: &str, args: &[&str]) {
    std::fs::write(repo.join(name), format!("{name} contents\n")).unwrap();
    let mut argv = args.to_vec();
    argv.extend_from_slice(&["update-index", "--add", name]);
    ok(repo, home, &argv);
}

#[test]
fn many_files_defaults_index_skip_hash_and_an_explicit_key_still_wins() {
    let zeroes = "0".repeat(40);

    // Unconfigured: git computes the trailing checksum.
    let (repo, home) = fixture("mf-plain");
    stage_a_new_file(&repo, &home, "g", &[]);
    assert_ne!(index_trailer(&repo), zeroes);

    // `feature.manyFiles=true` turns `index.skipHash` on by default
    // (repo-settings.c:59-63 then :79), so the trailer is twenty zero bytes.
    let (repo, home) = fixture("mf-on");
    stage_a_new_file(&repo, &home, "g", &["-c", "feature.manyFiles=true"]);
    assert_eq!(index_trailer(&repo), zeroes);

    // The cascade is a *default*: an explicit `index.skipHash=false` beats it.
    let (repo, home) = fixture("mf-off");
    stage_a_new_file(
        &repo,
        &home,
        "g",
        &["-c", "feature.manyFiles=true", "-c", "index.skipHash=false"],
    );
    assert_ne!(index_trailer(&repo), zeroes);
}

#[test]
fn a_valueless_boolean_key_is_true_and_an_empty_one_is_false() {
    // `[feature]\n\tmanyFiles\n` — no `=`. `git_parse_maybe_bool_text` answers 1
    // for a NULL value before it looks at any text (parse.c:168-169), so the index
    // comes out with a zeroed trailer. Confirmed against git 2.55.0 on the same
    // config file.
    let (repo, home) = fixture("valueless-true");
    let cfg = repo.join(".git/config");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("[feature]\n\tmanyFiles\n");
    std::fs::write(&cfg, text).unwrap();

    stage_a_new_file(&repo, &home, "g", &[]);
    assert_eq!(index_trailer(&repo), "0".repeat(40));

    // An empty *value* is the other answer (parse.c:170-171), which is the whole
    // reason the two spellings have to be told apart.
    let (repo, home) = fixture("empty-false");
    let cfg = repo.join(".git/config");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("[feature]\n\tmanyFiles =\n");
    std::fs::write(&cfg, text).unwrap();

    stage_a_new_file(&repo, &home, "g", &[]);
    assert_ne!(index_trailer(&repo), "0".repeat(40));
}

// ---------------------------------------------------------------------------
// git_default_config(): core.createObject, sparse.expectFilesOutsideOfPatterns
// ---------------------------------------------------------------------------

#[test]
fn create_object_takes_rename_or_link_and_nothing_else() {
    let (repo, home) = fixture("create-object");

    for mode in ["rename", "link"] {
        let out = run(&repo, &home, &["-c", &format!("core.createObject={mode}"), "branch"]);
        assert_eq!(stderr(&out), "", "for {mode}");
        assert!(out.status.success(), "for {mode}");
    }

    // The C comparison is `strcmp`, so the capitalised spelling is not the mode.
    for bad in ["bogus", "Link", "RENAME", ""] {
        let out = run(&repo, &home, &["-c", &format!("core.createObject={bad}"), "branch"]);
        assert_eq!(
            stderr(&out),
            format!("fatal: invalid mode for object creation: {bad}\n"),
            "for {bad:?}"
        );
        assert_eq!(code(&out), FATAL, "for {bad:?}");
    }
}

#[test]
fn create_object_without_a_value_is_config_error_nonbool() {
    // git 2.55.0, on the same file:
    //
    //     error: missing value for 'core.createobject'
    //     fatal: bad config variable 'core.createobject' in file '.git/config' at line 9
    //
    // The ` at line 9` clause is reproduced too: gitoxide's config metadata
    // carries the source path but not the line, so `crate::config::walk_config`
    // re-parses the file to find it.
    let (repo, home) = fixture("create-object-nonbool");
    let cfg = repo.join(".git/config");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("[core]\n\tcreateObject\n");
    std::fs::write(&cfg, text).unwrap();

    let out = run(&repo, &home, &["branch"]);
    assert_eq!(code(&out), FATAL);
    let line = std::fs::read_to_string(&cfg)
        .unwrap()
        .lines()
        .position(|l| l.trim() == "createObject")
        .expect("the appended line is there")
        + 1;
    assert_eq!(
        stderr(&out),
        format!(
            "error: missing value for 'core.createobject'\n\
             fatal: bad config variable 'core.createobject' in file '.git/config' at line {line}\n"
        )
    );

    // An empty value is a different thing: it reaches the mode comparison and
    // fails there, exactly as git reports it.
    let (repo, home) = fixture("create-object-empty");
    let cfg = repo.join(".git/config");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("[core]\n\tcreateObject =\n");
    std::fs::write(&cfg, text).unwrap();

    let out = run(&repo, &home, &["branch"]);
    assert_eq!(code(&out), FATAL);
    assert_eq!(stderr(&out), "fatal: invalid mode for object creation: \n");
}

#[test]
fn parse_time_keys_reject_every_occurrence_not_just_the_winner() {
    let (repo, home) = fixture("every-occurrence");

    // `git_default_config` is a callback, so an overridden bad value is still seen.
    // Stock git 2.55.0 refuses both orders.
    for pair in [["core.createObject=bogus", "core.createObject=rename"],
                 ["core.createObject=rename", "core.createObject=bogus"]] {
        let out = run(&repo, &home, &["-c", pair[0], "-c", pair[1], "status", "--porcelain"]);
        assert_eq!(
            stderr(&out),
            "fatal: invalid mode for object creation: bogus\n",
            "for {pair:?}"
        );
        assert_eq!(code(&out), FATAL, "for {pair:?}");
    }
}

#[test]
fn sparse_expect_files_outside_of_patterns_is_a_boolean() {
    let (repo, home) = fixture("sparse-expect");

    let out = run(
        &repo,
        &home,
        &["-c", "sparse.expectFilesOutsideOfPatterns=bogus", "branch"],
    );
    assert_eq!(
        stderr(&out),
        "fatal: bad boolean config value 'bogus' for 'sparse.expectfilesoutsideofpatterns'\n"
    );
    assert_eq!(code(&out), FATAL);

    for good in ["true", "false", "yes", "no", "on", "off", "1", "0", ""] {
        let out = run(
            &repo,
            &home,
            &[
                "-c",
                &format!("sparse.expectFilesOutsideOfPatterns={good}"),
                "status",
                "--porcelain",
            ],
        );
        assert_eq!(stderr(&out), "", "for {good:?}");
        assert!(out.status.success(), "for {good:?}");
    }
}

#[test]
fn the_parse_time_gate_reaches_verbs_the_settings_gate_does_not() {
    let (repo, home) = fixture("wider-gate");

    // Measured against stock: each of these refuses `core.createObject=bogus`
    // while running happily with `core.packedGitLimit=bogus`.
    for args in [
        vec!["branch"],
        vec!["tag"],
        vec!["symbolic-ref", "HEAD"],
        vec!["count-objects"],
        vec!["var", "GIT_AUTHOR_IDENT"],
    ] {
        let mut refused = vec!["-c", "core.createObject=bogus"];
        refused.extend_from_slice(&args);
        let out = run(&repo, &home, &refused);
        assert_eq!(
            stderr(&out),
            "fatal: invalid mode for object creation: bogus\n",
            "for {args:?}"
        );

        let mut allowed = vec!["-c", "core.packedGitLimit=bogus"];
        allowed.extend_from_slice(&args);
        let out = run(&repo, &home, &allowed);
        assert_eq!(stderr(&out), "", "for {args:?}");
    }
}

// ---------------------------------------------------------------------------
// check_updates(): checkout.workers, checkout.thresholdForParallelism
// ---------------------------------------------------------------------------

#[test]
fn parallel_checkout_keys_report_with_the_camel_case_spelling() {
    let (repo, home) = fixture("threshold-name");

    // `parallel-checkout.c:65` passes the key as a camelCase literal, and
    // `die_bad_number` prints what it was handed — unlike `core.packedgitlimit`,
    // which is a lowercase literal in `repo-settings.c`.
    let out = run(
        &repo,
        &home,
        &["-c", "checkout.thresholdForParallelism=bogus", "checkout", "main"],
    );
    assert_eq!(
        stderr(&out),
        "fatal: bad numeric config value 'bogus' for 'checkout.thresholdForParallelism': invalid unit\n"
    );
    assert_eq!(code(&out), FATAL);

    // The worker count is read first, so it is the one that reports when both are
    // unreadable (parallel-checkout.c:60 before :65).
    let out = run(
        &repo,
        &home,
        &[
            "-c",
            "checkout.workers=bogus",
            "-c",
            "checkout.thresholdForParallelism=bogus",
            "checkout",
            "main",
        ],
    );
    assert_eq!(
        stderr(&out),
        "fatal: bad numeric config value 'bogus' for 'checkout.workers': invalid unit\n"
    );
}

#[test]
fn parallel_checkout_keys_are_read_only_when_the_worktree_is_updated() {
    let (repo, home) = fixture("threshold-modes");
    let key = "checkout.thresholdForParallelism=bogus";

    // Refused — every one of these reaches `check_updates()` under stock git,
    // including a `checkout` that has nothing to update.
    for args in [
        vec!["checkout", "main"],
        vec!["checkout", "b2"],
        vec!["switch", "main"],
        vec!["restore", "f"],
        vec!["restore", "--worktree", "f"],
        vec!["restore", "--staged", "--worktree", "f"],
        vec!["reset", "--hard", "HEAD"],
        vec!["reset", "--merge", "HEAD"],
        vec!["reset", "--keep", "HEAD"],
        vec!["checkout-index", "-a"],
        vec!["read-tree", "-m", "-u", "HEAD"],
    ] {
        let mut argv = vec!["-c", key];
        argv.extend_from_slice(&args);
        let out = run(&repo, &home, &argv);
        assert_eq!(
            stderr(&out),
            "fatal: bad numeric config value 'bogus' for 'checkout.thresholdForParallelism': invalid unit\n",
            "expected {args:?} to be refused"
        );
        assert_eq!(code(&out), FATAL, "for {args:?}");
    }

    // Not refused — these never update the worktree, so stock git never reads the
    // key and neither does this port.
    for args in [
        vec!["reset", "--soft", "HEAD"],
        vec!["reset", "--mixed", "HEAD"],
        vec!["restore", "--staged", "f"],
        vec!["read-tree", "-m", "HEAD"],
        vec!["status", "--porcelain"],
    ] {
        let mut argv = vec!["-c", key];
        argv.extend_from_slice(&args);
        let out = run(&repo, &home, &argv);
        assert!(
            !stderr(&out).contains("thresholdForParallelism"),
            "expected {args:?} not to read the key, got: {}",
            stderr(&out)
        );
    }
}

#[test]
fn parallel_checkout_keys_accept_the_values_git_accepts() {
    let (repo, home) = fixture("threshold-good");

    // `checkout.workers=0` means "one per core" rather than "none", and a
    // threshold of 0 is a perfectly good number.
    let out = run(
        &repo,
        &home,
        &[
            "-c",
            "checkout.workers=0",
            "-c",
            "checkout.thresholdForParallelism=0",
            "checkout",
            "main",
        ],
    );
    assert!(
        !stderr(&out).contains("bad numeric config value"),
        "unexpected stderr: {}",
        stderr(&out)
    );
}
