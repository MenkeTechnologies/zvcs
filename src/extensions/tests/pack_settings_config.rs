//! The six object-storage `pack.*` keys `pack-objects` resolves before it parses
//! its command line, and the one diagnostic pair one of them can reach.
//!
//! git splits them across two reads, both ahead of `parse_options` and therefore
//! ahead of `-h`:
//!
//! * `prepare_repo_settings()` (`builtin/pack-objects.c:5164`) — `pack.useSparse`,
//!   `pack.usePathWalk`, `pack.readReverseIndex`,
//!   `pack.useBitmapBoundaryTraversal`, each a `git_config_bool`.
//! * `git_pack_config()` (`:5173`) — `pack.useBitmaps` (also a `git_config_bool`)
//!   and `pack.allowPackReuse`, whose grammar is the *word*-only boolean plus
//!   `single`/`multi` and whose rejection is its own `die()`.
//!
//! Five of the six tune machinery this port does not have: sparse reachability,
//! reading a `.rev`, bitmap-boundary traversal, bitmap-accelerated counting and
//! verbatim pack reuse. What is asserted for those is what is portable — that a
//! value git cannot read is fatal here with the same bytes and the same exit
//! code, at the same point in the command's life.
//!
//! `pack.usePathWalk` is different: it is the default for `--path-walk`, and
//! `builtin/pack-objects.c:5216-5228` warns and disables the walk when it is on
//! together with `--delta-islands` or a `--filter` the path-walk API cannot take.
//! Those two lines are reachable from the config alone, so they are asserted as
//! behavior, not just as validation.
//!
//! Every literal below was captured from git 2.55.0 (`/opt/homebrew/bin/git`) run
//! in a one-commit repository; the commands are named in each test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A one-commit repository plus an isolated, empty `HOME`, so no ambient global
/// `pack.*` config leaks into the run.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-packset-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "alice@example.com"]);
    run(&repo, &home, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    run(&repo, &home, &["add", "f"]);
    let commit = run(&repo, &home, &["commit", "-q", "-m", "c0"]);
    assert!(commit.status.success(), "fixture commit must succeed");
    (repo, home)
}

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        // git's own path-walk test hook: it is the last fallback in the
        // `--path-walk` default, so an inherited value would decide the answer
        // for the unset-config cases below.
        .env_remove("GIT_TEST_PACK_PATH_WALK")
        .output()
        .expect("run binary")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// The four `prepare_repo_settings()` booleans and `pack.useBitmaps`, each with
/// the lowercased name git's `git_config_bool` prints.
const BOOL_KEYS: &[(&str, &str)] = &[
    ("pack.useSparse", "pack.usesparse"),
    ("pack.usePathWalk", "pack.usepathwalk"),
    ("pack.readReverseIndex", "pack.readreverseindex"),
    ("pack.useBitmapBoundaryTraversal", "pack.usebitmapboundarytraversal"),
    ("pack.useBitmaps", "pack.usebitmaps"),
];

#[test]
fn bad_boolean_pack_settings_are_fatal_before_the_usage_block() {
    // git 2.55.0, in a repository:
    //     $ git -c pack.useSparse=bogus pack-objects -h
    //     fatal: bad boolean config value 'bogus' for 'pack.usesparse'
    //     (exit 128, nothing on stdout)
    // `-h` is the sharpest probe available: it is the earliest thing
    // `parse_options` can do, so a diagnostic that beats it can only have come
    // from a config read that ran first. Reverting any of these reads turns the
    // exit into 129 and puts the 4170-byte usage block on stdout.
    for (key, lowered) in BOOL_KEYS {
        let (repo, home) = fixture(&format!("badbool-{}", lowered.replace('.', "-")));
        let out = run(&repo, &home, &["-c", &format!("{key}=bogus"), "pack-objects", "-h"]);
        assert_eq!(
            stderr(&out),
            format!("fatal: bad boolean config value 'bogus' for '{lowered}'\n"),
            "{key} must be rejected with git's wording"
        );
        assert_eq!(code(&out), 128, "{key}: git dies rather than printing usage");
        assert!(out.stdout.is_empty(), "{key}: nothing reaches stdout");
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }
}

#[test]
fn valid_boolean_pack_settings_leave_the_command_alone() {
    // The other half of the same claim: a readable value must not become a
    // diagnostic. `-h` still prints its usage block and exits 129.
    let (repo, home) = fixture("goodbool");
    for (key, _) in BOOL_KEYS {
        for value in ["true", "false", "on", "off", "1", "0"] {
            let out = run(&repo, &home, &["-c", &format!("{key}={value}"), "pack-objects", "-h"]);
            assert_eq!(code(&out), 129, "{key}={value} must be accepted");
            assert!(
                stderr(&out).is_empty(),
                "{key}={value} must print nothing on stderr, got: {}",
                stderr(&out)
            );
            assert!(
                out.stdout.starts_with(b"usage: git pack-objects"),
                "{key}={value} must still reach the usage block"
            );
        }
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn allow_pack_reuse_takes_words_and_modes_but_not_digits() {
    // `git_pack_config()` reads this one with `git_parse_maybe_bool_text`, the
    // word-only boolean grammar, and only falls through to `single`/`multi` when
    // that fails — so the digits are *not* booleans here. git 2.55.0:
    //     $ git -c pack.allowPackReuse=1 pack-objects -h
    //     fatal: invalid pack.allowPackReuse value: '1'
    // Note the key is spelled in camelCase: the message is a `die()` with the
    // literal name in its format string, not the lowercased `var` the config
    // reader passes the callback.
    let (repo, home) = fixture("packreuse");

    for value in ["true", "false", "yes", "no", "on", "off", "", "single", "multi", "SINGLE", "Multi"] {
        let out = run(
            &repo,
            &home,
            &["-c", &format!("pack.allowPackReuse={value}"), "pack-objects", "-h"],
        );
        assert_eq!(code(&out), 129, "pack.allowPackReuse={value:?} must be accepted");
        assert!(stderr(&out).is_empty(), "pack.allowPackReuse={value:?} must be silent");
    }

    for value in ["1", "0", "2", "bogus", "singl", "multiple"] {
        let out = run(
            &repo,
            &home,
            &["-c", &format!("pack.allowPackReuse={value}"), "pack-objects", "-h"],
        );
        assert_eq!(
            stderr(&out),
            format!("fatal: invalid pack.allowPackReuse value: '{value}'\n"),
            "pack.allowPackReuse={value} must be refused with git's wording"
        );
        assert_eq!(code(&out), 128);
        assert!(out.stdout.is_empty());
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn settings_block_is_read_before_the_pack_config_block() {
    // git prepares the repo settings (`:5164`) and only then runs
    // `git_pack_config` (`:5173`), so when both a settings key and a pack-config
    // key are unreadable the settings key is the one named — in either `-c`
    // order. Verified against git 2.55.0.
    let (repo, home) = fixture("readorder");
    for order in [
        vec!["-c", "pack.useSparse=bogus", "-c", "pack.packSizeLimit=bogus"],
        vec!["-c", "pack.packSizeLimit=bogus", "-c", "pack.useSparse=bogus"],
    ] {
        let mut args = order.clone();
        args.extend_from_slice(&["pack-objects", "-h"]);
        let out = run(&repo, &home, &args);
        assert_eq!(
            stderr(&out),
            "fatal: bad boolean config value 'bogus' for 'pack.usesparse'\n",
            "the settings block reports first, whatever the -c order"
        );
        assert_eq!(code(&out), 128);
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `pack-objects --revs --stdout` with the object list on stdin left empty:
/// enough to reach the post-parse `--path-walk` block without needing anything
/// in particular in the pack.
///
/// `cfg` holds the `-c <key>=<value>` pairs, which have to precede the verb, and
/// `extra` the options that follow it.
fn pack_objects(repo: &Path, home: &Path, cfg: &[&str], extra: &[&str]) -> Output {
    let mut args: Vec<&str> = cfg.to_vec();
    args.extend_from_slice(&["pack-objects", "--revs", "--stdout"]);
    args.extend_from_slice(extra);
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .env_remove("GIT_TEST_PACK_PATH_WALK")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .output()
        .expect("run binary")
}

#[test]
fn use_path_walk_config_reaches_the_delta_islands_warning() {
    // The key's one observable effect here. git 2.55.0:
    //     $ git -c pack.usePathWalk=true pack-objects --revs --stdout --delta-islands
    //     warning: cannot use --delta-islands with --path-walk
    //     $ git pack-objects --revs --stdout --delta-islands
    //     (silent)
    // If the read is reverted the warning disappears, because nothing else on
    // the line turns the path walk on.
    let (repo, home) = fixture("pathwalk-islands");

    let on = pack_objects(&repo, &home, &["-c", "pack.usePathWalk=true"], &["--delta-islands"]);
    assert_eq!(
        stderr(&on),
        "warning: cannot use --delta-islands with --path-walk\n",
        "pack.usePathWalk=true must turn the walk on, and the pairing must warn"
    );
    assert!(on.status.success(), "the warning is not fatal");

    let off = pack_objects(&repo, &home, &[], &["--delta-islands"]);
    assert!(stderr(&off).is_empty(), "unset config leaves the walk off, so nothing warns");

    let explicit_off = pack_objects(
        &repo,
        &home,
        &["-c", "pack.usePathWalk=true"],
        &["--no-path-walk", "--delta-islands"],
    );
    assert!(
        stderr(&explicit_off).is_empty(),
        "--no-path-walk overrides the config, so the pairing never arises"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn use_path_walk_config_reaches_the_incompatible_filter_pair() {
    // `path_walk_filter_compatible()` is `prepare_filters(NULL, …)`, which fails
    // only on a `tree:<n>` with a non-zero depth — including one nested inside a
    // `combine:`. It reports the depth itself before the caller's warning, so the
    // pair is two lines. git 2.55.0:
    //     $ git -c pack.usePathWalk=true pack-objects --revs --stdout --filter=tree:1
    //     error: tree:1 filter not supported by the path-walk API
    //     warning: cannot use --filter with --path-walk
    let (repo, home) = fixture("pathwalk-filter");

    for (spec, depth) in [("tree:1", "1"), ("combine:blob:none+tree:2", "2")] {
        let out = pack_objects(
            &repo,
            &home,
            &["-c", "pack.usePathWalk=true"],
            &[&format!("--filter={spec}")],
        );
        assert_eq!(
            stderr(&out),
            format!(
                "error: tree:{depth} filter not supported by the path-walk API\n\
                 warning: cannot use --filter with --path-walk\n"
            ),
            "--filter={spec} is incompatible with the path walk"
        );
        assert!(out.status.success(), "--filter={spec}: the pair is not fatal");
    }

    // `tree:0` *is* compatible, so the filter is not the incompatible option and
    // the islands branch reports instead — which also pins that git checks the
    // filter first.
    let compatible = pack_objects(
        &repo,
        &home,
        &["-c", "pack.usePathWalk=true"],
        &["--filter=tree:0", "--delta-islands"],
    );
    assert_eq!(
        stderr(&compatible),
        "warning: cannot use --delta-islands with --path-walk\n",
        "tree:0 is path-walk compatible, so --delta-islands is the option named"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn path_walk_default_is_not_reached_when_bitmaps_or_an_external_rev_list_are_asked_for() {
    // `builtin/pack-objects.c:5188-5191`: an explicit `--use-bitmap-index`, or no
    // internal rev list at all, settles `--path-walk` at 0 before the config is
    // consulted. `pack.useBitmaps` is *not* what the first arm reads — that key
    // is not folded into `use_bitmap_index` until `:5334` — so setting it must
    // not suppress the warning the way the flag does.
    let (repo, home) = fixture("pathwalk-gates");

    let with_flag = pack_objects(
        &repo,
        &home,
        &["-c", "pack.usePathWalk=true"],
        &["--use-bitmap-index", "--delta-islands"],
    );
    assert!(
        stderr(&with_flag).is_empty(),
        "--use-bitmap-index settles --path-walk at 0 before the config is read"
    );

    let with_config = pack_objects(
        &repo,
        &home,
        &["-c", "pack.usePathWalk=true", "-c", "pack.useBitmaps=true"],
        &["--delta-islands"],
    );
    assert_eq!(
        stderr(&with_config),
        "warning: cannot use --delta-islands with --path-walk\n",
        "pack.useBitmaps is resolved far later and must not stand in for the flag"
    );

    // Without `--revs` there is no internal rev list, which is the second half of
    // the same condition.
    let no_rev_list = Command::new(BIN)
        .args(["pack-objects", "--stdout", "--delta-islands"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .env_remove("GIT_TEST_PACK_PATH_WALK")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .output()
        .expect("run binary");
    assert!(
        stderr(&no_rev_list).is_empty(),
        "no internal rev list means no path walk, whatever the config says"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn feature_macros_do_not_turn_the_path_walk_on() {
    // `repo-settings.c:78` passes a literal `0` as `repo_cfg_bool`'s default, so
    // the `pack_use_path_walk = 1` that `feature.experimental` (`:57`) and
    // `feature.manyFiles` (`:63`) set is overwritten whenever `pack.usePathWalk`
    // itself is unset. Confirmed against git 2.55.0, which is silent for both.
    // This is the guard against "helpfully" cascading them here.
    let (repo, home) = fixture("feature-macros");
    for macro_key in ["feature.experimental", "feature.manyFiles"] {
        let out = pack_objects(&repo, &home, &["-c", &format!("{macro_key}=true")], &["--delta-islands"]);
        assert!(
            stderr(&out).is_empty(),
            "{macro_key}=true must not enable the path walk, got: {}",
            stderr(&out)
        );
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn repack_and_gc_reject_the_pack_config_keys_only_once_they_do_real_work() {
    // git's `repack` and `gc` reach these keys through the `pack-objects` child
    // they start, so `repack -h` prints usage while a real `repack -a -d` dies.
    // This port packs inline, so the same split has to be arranged deliberately:
    // the read sits in `execute()`, past the usage block. Verified against git
    // 2.55.0 for both verbs.
    let (repo, home) = fixture("repack-gc");

    for verb in ["repack", "gc"] {
        let help = run(&repo, &home, &["-c", "pack.allowPackReuse=bogus", verb, "-h"]);
        assert_eq!(code(&help), 129, "{verb} -h must still print usage");
        assert!(stderr(&help).is_empty(), "{verb} -h must not report the config");
    }

    let repack = run(&repo, &home, &["-c", "pack.allowPackReuse=bogus", "repack", "-a", "-d", "-q"]);
    assert_eq!(
        stderr(&repack),
        "fatal: invalid pack.allowPackReuse value: 'bogus'\n",
        "a real repack reaches git_pack_config"
    );
    assert_eq!(code(&repack), 128);

    let gc = run(&repo, &home, &["-c", "pack.useBitmaps=bogus", "gc", "-q"]);
    assert_eq!(
        stderr(&gc),
        "fatal: bad boolean config value 'bogus' for 'pack.usebitmaps'\n",
        "a real gc reaches it through its repack"
    );
    assert_eq!(code(&gc), 128);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
