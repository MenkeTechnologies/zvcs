//! `git for-each-repo --config=<key>` and the two config entries that are not
//! ordinary paths.
//!
//! `repo_config_get_string_multi()` runs `check_multi_string()` over every
//! collected value and fails on the first one whose string is NULL — a name
//! written with no `=` (config.c:1873-1890). `cmd_for_each_repo` turns that
//! `err < 0` into `error: missing value for '<key>'` followed by
//! `fatal: got bad config --config=<key>`, a blank line, the usage block and exit
//! 129 (builtin/for-each-repo.c:58-61), running nothing at all. This is git's own
//! `error on NULL value for config keys` case in `t/t0068-for-each-repo.sh`.
//!
//! An *empty* value is a different thing entirely: it is a value, so the check
//! passes, `interpolate_path("")` copies it through unchanged (path.c:698-733),
//! and git runs the child with `-C ''` — a documented no-op that leaves it in the
//! current directory.
//!
//! Measured against git 2.55.0; the expectations below are stock's bytes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// git's usage block for this command, as `usage_msg_optf()` renders it.
const USAGE: &str = "usage: git for-each-repo --config=<config> [--] <arguments>\n\
                     \n    \
                     --[no-]config <config>\n\
                     \x20                         config key storing a list of repository paths\n    \
                     --[no-]keep-going     keep going even if command fails in a repository\n\n";

/// A scratch directory holding one real repository (`inner`) plus a config file
/// used as `GIT_CONFIG_GLOBAL`. The tree root is deliberately *not* a repository,
/// so `-C ''` lands somewhere a repository lookup fails and says so.
fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-fer-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let inner = root.join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    assert!(
        Command::new(BIN)
            .args(["init", "-q", "-b", "main"])
            .current_dir(&inner)
            .status()
            .unwrap()
            .success(),
        "init failed"
    );
    root
}

/// Run `for-each-repo` under a global config file holding exactly `body`.
fn run(root: &Path, body: &str, args: &[&str]) -> Output {
    let cfg = root.join("global-config");
    std::fs::write(&cfg, body).unwrap();
    let mut cmd = Command::new(BIN);
    cmd.arg("for-each-repo")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &cfg)
        .env("HOME", root)
        .env("ZVCS_HOME", root)
        .output()
        .unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A valueless entry is fatal, and it is fatal *before* anything runs — the
/// `-C` child is never spawned, so a sibling entry that names a perfectly good
/// repository produces no output either.
#[test]
fn a_valueless_entry_is_rejected_and_nothing_runs() {
    let root = scratch("novalue");
    let want = format!(
        "error: missing value for 'my.repos'\nfatal: got bad config --config=my.repos\n\n{USAGE}"
    );

    // The only entry is valueless.
    let alone = run(&root, "[my]\n\trepos\n", &["--config=my.repos", "rev-parse", "--git-dir"]);
    assert_eq!(alone.status.code(), Some(129), "{alone:?}");
    assert_eq!(stderr(&alone), want);
    assert!(alone.stdout.is_empty(), "{alone:?}");

    // A usable entry ahead of it does not rescue the lookup: `check_multi_string`
    // walks the whole list, and one NULL condemns all of it.
    let mixed = run(
        &root,
        "[my]\n\trepos = inner\n\trepos\n",
        &["--config=my.repos", "rev-parse", "--git-dir"],
    );
    assert_eq!(mixed.status.code(), Some(129), "{mixed:?}");
    assert_eq!(stderr(&mixed), want);
    assert!(
        mixed.stdout.is_empty(),
        "the good repository ran despite the bad entry: {mixed:?}"
    );
}

/// `--keep-going` is about a *child* that fails, not about a malformed config:
/// the lookup happens first, so the refusal is identical with the flag set.
#[test]
fn keep_going_does_not_soften_a_valueless_entry() {
    let root = scratch("kgnovalue");
    let out = run(
        &root,
        "[my]\n\trepos = inner\n\trepos\n",
        &["--keep-going", "--config=my.repos", "rev-parse", "--git-dir"],
    );
    assert_eq!(out.status.code(), Some(129), "{out:?}");
    assert_eq!(
        stderr(&out),
        format!(
            "error: missing value for 'my.repos'\nfatal: got bad config --config=my.repos\n\n{USAGE}"
        )
    );
}

/// An entry written `repos =` has a value — the empty string — so the lookup
/// succeeds and the child runs with `git -C ''`, which git documents as a no-op.
/// The tree root is not a repository, so the child says so and its 128 propagates.
/// The distinction from the valueless case above is the whole point: `key` and
/// `key =` are different config, and only the first is an error here.
#[test]
fn an_empty_value_runs_the_child_where_it_stands() {
    let root = scratch("emptyvalue");
    let out = run(&root, "[my]\n\trepos =\n", &["--config=my.repos", "rev-parse", "--git-dir"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert!(
        stderr(&out).starts_with("fatal: not a git repository"),
        "expected the child's own repository-lookup failure, got {:?}",
        stderr(&out)
    );
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// A key that is simply absent is not an error at all: `err > 0` returns 0 with
/// nothing run (builtin/for-each-repo.c:62-63). Pinned alongside the two cases
/// above so the "missing value" detection cannot be widened into "missing key".
#[test]
fn an_absent_key_runs_nothing_and_succeeds() {
    let root = scratch("absent");
    let out = run(
        &root,
        "[my]\n\trepos = inner\n",
        // The child would fail loudly if it were ever spawned.
        &["--config=other.key", "--", "help", "--no-such-option"],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
}
