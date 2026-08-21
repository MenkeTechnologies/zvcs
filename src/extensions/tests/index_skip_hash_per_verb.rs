//! `index.skipHash` (and the `feature.manyFiles` macro that defaults it) reaches
//! *every* verb that writes an index, not just the plumbing ones.
//!
//! git has no per-command switch for this: `do_write_index()` sets the hashfile's
//! `skip_hash` from the repository's settings block before it serialises anything
//! (`read-cache.c:2830-2831`), and every index write in the C — the real index,
//! the partial-commit `next-index-<pid>` (`builtin/commit.c:541-550`), a scratch
//! index handed to a hook through `GIT_INDEX_FILE` — goes through that one
//! function. So the trailer an index carries is a property of the repository, and
//! an index written by `add` cannot differ from one written by `update-index`.
//!
//! [`core_settings_config`](../core_settings_config.rs) pins the cascade itself
//! (`feature.manyFiles` defaulting the key, the valueless-boolean spellings) on
//! `update-index`. This file pins the *reach* of the resolved value: one case per
//! porcelain verb that writes an index, asserting the twenty trailing bytes.
//!
//! Every expectation was taken from a differential run against stock git 2.55.0
//! (`/opt/homebrew/bin/git -c index.skipHash=true <verb> …`, then
//! `tail -c 20 .git/index | od -An -tx1`) in the same fixture, but the tests
//! themselves shell out to nothing but the binary under test.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A zeroed twenty-byte trailer, as [`index_trailer`] renders it.
fn zeroes() -> String {
    "0".repeat(40)
}

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

/// [`run`] with `input` on stdin, for the one verb that only writes an index from
/// its interactive loop.
fn run_with_stdin(repo: &Path, home: &Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// A two-file, one-commit repository on `main`, plus an isolated empty `HOME`.
///
/// `tag` has to be unique per fixture: the tests run concurrently in one process
/// and share the pid.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-skiphash-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo/d")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::write(repo.join("f"), "1\n2\n3\n").unwrap();
    std::fs::write(repo.join("d/e"), "sub\n").unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    ok(&repo, &home, &["config", "user.email", "alice@example.com"]);
    ok(&repo, &home, &["config", "user.name", "Alice"]);
    ok(&repo, &home, &["add", "f", "d/e"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c0"]);
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

/// The three configurations every verb is put through: unconfigured, the key
/// itself, and the macro that defaults it (`repo-settings.c:59-63`, then `:81`).
const CONFIGS: [(&str, &[&str]); 3] = [
    ("unset", &[]),
    ("skipHash", &["-c", "index.skipHash=true"]),
    ("manyFiles", &["-c", "feature.manyFiles=true"]),
];

/// Put one verb through [`CONFIGS`] and assert the trailer each leaves behind:
/// a real checksum unconfigured, twenty zero bytes under either switch.
///
/// `prepare` runs against a fresh fixture before the verb does, and returns the
/// argument list — which lets a verb that needs a dirty worktree, a staged
/// change or a second branch build one without a second fixture helper.
fn assert_verb<F>(tag: &str, prepare: F)
where
    F: Fn(&Path, &Path) -> Vec<String>,
{
    for (name, cfg) in CONFIGS {
        let (repo, home) = fixture(&format!("{tag}-{name}"));
        let argv = prepare(&repo, &home);
        let mut args: Vec<&str> = cfg.to_vec();
        args.extend(argv.iter().map(String::as_str));
        // The verb's own exit status is not what is under test here (some of
        // these legitimately fail, e.g. a conflicted merge), only the bytes it
        // left in the index.
        run(&repo, &home, &args);
        let trailer = index_trailer(&repo);
        match name {
            "unset" => assert_ne!(trailer, zeroes(), "{tag}: unconfigured index must be checksummed"),
            _ => assert_eq!(trailer, zeroes(), "{tag}: `{name}` must zero the index trailer"),
        }
    }
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn add_writes_the_configured_trailer() {
    assert_verb("add", |repo, _home| {
        std::fs::write(repo.join("f"), "1\n2\n3\n4\n").unwrap();
        argv(&["add", "f"])
    });
}

#[test]
fn add_intent_to_add_writes_the_configured_trailer() {
    // `-N` takes its own write path out of `cmd_add()`, so it needs its own case.
    assert_verb("add-ita", |repo, _home| {
        std::fs::write(repo.join("new"), "n\n").unwrap();
        argv(&["add", "-N", "new"])
    });
}

#[test]
fn commit_all_writes_the_configured_trailer() {
    // `commit -a`: `add_files_to_cache()` then `write_locked_index()`
    // (builtin/commit.c:454-465).
    assert_verb("commit-all", |repo, _home| {
        std::fs::write(repo.join("f"), "1\n2\n3\n4\n").unwrap();
        argv(&["commit", "-q", "-a", "-m", "c1"])
    });
}

#[test]
fn commit_include_writes_the_configured_trailer() {
    // `commit -i <path>` is the same branch of `prepare_index()` as `-a`.
    assert_verb("commit-include", |repo, _home| {
        std::fs::write(repo.join("f"), "1\n2\n3\n4\n").unwrap();
        argv(&["commit", "-q", "-i", "-m", "c1", "f"])
    });
}

#[test]
fn partial_commit_writes_the_configured_trailer() {
    // `commit <path>` writes the real index at step (2)/(3) of
    // `prepare_index()`'s partial-commit block (builtin/commit.c:534-538).
    assert_verb("commit-partial", |repo, _home| {
        std::fs::write(repo.join("f"), "1\n2\n3\n4\n").unwrap();
        argv(&["commit", "-q", "-m", "c1", "f"])
    });
}

#[test]
fn rm_writes_the_configured_trailer() {
    assert_verb("rm", |_repo, _home| argv(&["rm", "-q", "d/e"]));
}

#[test]
fn mv_writes_the_configured_trailer() {
    assert_verb("mv", |_repo, _home| argv(&["mv", "f", "g"]));
}

#[test]
fn restore_staged_writes_the_configured_trailer() {
    assert_verb("restore-staged", |repo, home| {
        std::fs::write(repo.join("f"), "1\n2\n3\n4\n").unwrap();
        ok(repo, home, &["add", "f"]);
        argv(&["restore", "--staged", "f"])
    });
}

#[test]
fn restore_worktree_writes_the_configured_trailer() {
    // A worktree restore from the index still rewrites the index, to refresh the
    // stat cache for the files it just replaced.
    assert_verb("restore-worktree", |repo, _home| {
        std::fs::write(repo.join("f"), "1\n2\n3\n4\n").unwrap();
        argv(&["restore", "f"])
    });
}

#[test]
fn checkout_index_refresh_writes_the_configured_trailer() {
    // `-u` is the flag that makes `checkout-index` write at all
    // (builtin/checkout-index.c:341-347).
    assert_verb("checkout-index", |repo, _home| {
        std::fs::remove_file(repo.join("f")).unwrap();
        argv(&["checkout-index", "-u", "-a"])
    });
}

#[test]
fn add_interactive_revert_writes_the_configured_trailer() {
    // The interactive `revert` command resets the chosen path to `HEAD` in the
    // index, which is an ordinary index write inside the loop. `1` picks the one
    // staged path, the empty line ends the selection, `q` leaves.
    for (name, cfg) in CONFIGS {
        let (repo, home) = fixture(&format!("add-i-{name}"));
        std::fs::write(repo.join("f"), "1\n2\n3\n4\n").unwrap();
        ok(&repo, &home, &["add", "f"]);
        let mut args: Vec<&str> = cfg.to_vec();
        args.extend_from_slice(&["add", "-i"]);
        run_with_stdin(&repo, &home, &args, "revert\n1\n\nq\n");

        // The revert really happened: the staged change is unstaged again.
        let status = run(&repo, &home, &["status", "--porcelain"]);
        assert_eq!(String::from_utf8_lossy(&status.stdout), " M f\n");

        let trailer = index_trailer(&repo);
        match name {
            "unset" => assert_ne!(trailer, zeroes()),
            _ => assert_eq!(trailer, zeroes(), "add -i: `{name}` must zero the index trailer"),
        }
    }
}

#[test]
fn rerere_autoupdate_stage_writes_the_configured_trailer() {
    // `rerere.autoUpdate` stages a replayed resolution from inside the failed
    // merge — the last index write of that command, and one no porcelain verb
    // owns. Recording the resolution and replaying it both run under the binary
    // under test.
    for (name, cfg) in CONFIGS {
        let (repo, home) = fixture(&format!("rerere-{name}"));
        ok(&repo, &home, &["config", "rerere.enabled", "true"]);
        ok(&repo, &home, &["config", "rerere.autoUpdate", "true"]);

        ok(&repo, &home, &["checkout", "-q", "-b", "side"]);
        std::fs::write(repo.join("f"), "S\n2\n3\n").unwrap();
        ok(&repo, &home, &["commit", "-q", "-a", "-m", "side"]);
        ok(&repo, &home, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("f"), "M\n2\n3\n").unwrap();
        ok(&repo, &home, &["commit", "-q", "-a", "-m", "main"]);

        // Conflict once, resolve by hand, commit: the resolution is now on file.
        let out = run(&repo, &home, &["merge", "side"]);
        assert!(!out.status.success(), "the fixture merge must conflict");
        std::fs::write(repo.join("f"), "R\n2\n3\n").unwrap();
        ok(&repo, &home, &["add", "f"]);
        ok(&repo, &home, &["commit", "-q", "-m", "merged"]);
        ok(&repo, &home, &["reset", "-q", "--hard", "HEAD~1"]);

        // Conflict again: rerere replays the resolution and stages it.
        let mut args: Vec<&str> = cfg.to_vec();
        args.extend_from_slice(&["merge", "side"]);
        let out = run(&repo, &home, &args);
        // The line lands on stderr, where git prints it.
        let replayed = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            replayed.contains("Staged 'f' using previous resolution."),
            "rerere did not replay: {replayed}{}",
            String::from_utf8_lossy(&out.stdout)
        );

        let trailer = index_trailer(&repo);
        match name {
            "unset" => assert_ne!(trailer, zeroes()),
            _ => assert_eq!(trailer, zeroes(), "rerere: `{name}` must zero the index trailer"),
        }
    }
}

#[test]
fn an_explicit_false_beats_the_many_files_default_for_every_verb() {
    // The cascade is a *default* (`repo-settings.c:81` passes the cascaded value
    // in as the fallback), so `index.skipHash=false` wins over `feature.manyFiles`
    // — and has to win in each verb, not just the one the key was tested on.
    let cfg = ["-c", "feature.manyFiles=true", "-c", "index.skipHash=false"];

    let (repo, home) = fixture("explicit-false-add");
    std::fs::write(repo.join("f"), "1\n2\n3\n4\n").unwrap();
    let mut args = cfg.to_vec();
    args.extend_from_slice(&["add", "f"]);
    ok(&repo, &home, &args);
    assert_ne!(index_trailer(&repo), zeroes());

    let (repo, home) = fixture("explicit-false-commit");
    std::fs::write(repo.join("f"), "1\n2\n3\n4\n").unwrap();
    let mut args = cfg.to_vec();
    args.extend_from_slice(&["commit", "-q", "-a", "-m", "c1"]);
    ok(&repo, &home, &args);
    assert_ne!(index_trailer(&repo), zeroes());

    let (repo, home) = fixture("explicit-false-mv");
    let mut args = cfg.to_vec();
    args.extend_from_slice(&["mv", "f", "g"]);
    ok(&repo, &home, &args);
    assert_ne!(index_trailer(&repo), zeroes());
}

#[test]
fn one_repository_gets_one_trailer_whichever_verb_wrote_it() {
    // The point of the whole exercise: a repository configured for
    // `feature.manyFiles` must not end up with a checksummed index just because
    // the last command to touch it happened to be a porcelain one. Six verbs in
    // a row, same repository, trailer checked after each.
    let (repo, home) = fixture("mixed-sequence");
    let cfg = ["-c", "feature.manyFiles=true"];

    let steps: [&[&str]; 6] = [
        &["add", "f"],
        &["update-index", "--refresh"],
        &["commit", "-q", "-a", "-m", "c1"],
        &["mv", "f", "g"],
        &["rm", "-q", "d/e"],
        &["read-tree", "HEAD"],
    ];
    for step in steps {
        std::fs::write(repo.join("f"), format!("{}\n", step.join("-"))).unwrap();
        let mut args = cfg.to_vec();
        args.extend_from_slice(step);
        run(&repo, &home, &args);
        assert_eq!(
            index_trailer(&repo),
            zeroes(),
            "`git {}` left a checksummed index in a manyFiles repository",
            step.join(" ")
        );
    }
}
