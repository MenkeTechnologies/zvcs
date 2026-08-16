//! `repack --window` / `--window-memory` / `--depth` / `--threads`: repack does
//! not parse them, it forwards them.
//!
//! All four are `OPT_STRING` in `builtin/repack.c:206-213`. `cmd_repack()` copies
//! them into `po_args` untouched and `prepare_pack_objects()` (`repack.c:17-24`)
//! pushes them at the `pack-objects` child, which is what parses them. Three
//! observable consequences, one test apiece:
//!
//! ```text
//!   * the diagnostic is reported in the *child's* argv order — window,
//!     window-memory, depth, threads — not in the order the options were typed;
//!   * it comes after everything repack itself diagnoses, since the child is
//!     started only once repack's own parse and pre-flight checks are through;
//!   * a value repack would have rejected as an integer but `pack-objects`
//!     accepts, such as a negative one, is simply forwarded.
//! ```
//!
//! Every expectation was read off git 2.55.0 on this fixture first.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the binary under test in `dir`, asserting success. Fixture only.
fn git(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run the binary under test with an isolated, deterministic environment.
fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

/// A one-commit repository — the diagnostics under test are decided before any
/// object is read, and the accepted values only have to leave a working pack.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-rpfwd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    for dir in [&home, &repo] {
        std::fs::create_dir_all(dir).unwrap();
    }
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "alice@example.com"]);
    git(&repo, &home, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "contents of f\n").unwrap();
    git(&repo, &home, &["add", "f"]);
    git(&repo, &home, &["commit", "-q", "-m", "f"]);
    (repo, home)
}

/// `--threads=x --window=y`: both are bad, and it is `window` that is named,
/// because that is the one `prepare_pack_objects()` pushes first.
#[test]
fn the_child_argv_order_decides_which_bad_value_is_reported() {
    let (repo, home) = fixture("order");

    let out = run(&repo, &home, &["repack", "-q", "--threads=x", "--window=y"]);
    assert_eq!(out.status.code(), Some(129), "exit status");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: option `window' expects an integer value with an optional k/m/g suffix\n",
    );

    // window-memory comes before depth for the same reason.
    let out = run(&repo, &home, &["repack", "-q", "--depth=x", "--window-memory=y"]);
    assert_eq!(out.status.code(), Some(129), "exit status");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: option `window-memory' expects a non-negative integer value with an optional k/m/g suffix\n",
    );
}

/// The child only runs once repack is through its own parse and its pre-flight
/// conflicts, so both of those win over a bad forwarded value however early it
/// appears in argv.
#[test]
fn repacks_own_diagnostics_come_first() {
    let (repo, home) = fixture("first");

    // `--filter`'s spec check is a parse-options callback, so it dies during the
    // parse the child has not yet been started for.
    let out = run(&repo, &home, &["repack", "-q", "--window=", "--filter=bogus:spec"]);
    assert_eq!(out.status.code(), Some(128), "exit status");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "fatal: invalid filter-spec 'bogus:spec'\n");

    // A pre-flight conflict sits between the parse and `start_command()`.
    let out = run(&repo, &home, &["repack", "-q", "--filter-to=zz", "--window=x"]);
    assert_eq!(out.status.code(), Some(128), "exit status");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: option '--filter-to' can only be used along with '--filter'\n",
    );
}

/// `pack-objects` reads `--window` with `OPT_INTEGER`, which takes a negative
/// value and a `k`/`m`/`g` suffix; only an overflow of a C `int` is refused.
#[test]
fn values_pack_objects_accepts_are_forwarded() {
    let (repo, home) = fixture("accept");

    for value in ["-5", "1k", "10"] {
        let out = run(&repo, &home, &["repack", "-q", &format!("--window={value}")]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "--window={value}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let out = run(&repo, &home, &["repack", "-q", "--window=99999999999999999999"]);
    assert_eq!(out.status.code(), Some(129), "exit status");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: value 99999999999999999999 for option `window' not in range [-2147483648,2147483647]\n",
    );
}
