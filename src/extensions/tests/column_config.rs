//! `git column` honors the `column.ui` and `column.<command>` config keys as the
//! DEFAULT layout mode, overridden by the `--mode`/`--no-mode` command-line flags —
//! verified byte-for-byte against real git 2.55.0.
//!
//! `builtin/column.c` reads config through `git_column_config`: `column.ui` always,
//! plus `column.<name>` when `--command=<name>` is the first argument. The value is
//! the same token language `--mode=` uses (`always`/`never`/`auto`/`plain`/`column`/
//! `row`/`dense`/`nodense`), with the rule that naming a layout token but no enable
//! state implies `always`. zvcs reimplements this in `apply_config`/`parse_config`
//! (src/extensions/src/porcelain/column.rs); these tests pin that the config drives
//! the default and that `--mode` overrides it, matching git exactly.
//!
//! Layout is made deterministic headless with a fixed `--width`, so no terminal
//! probing is involved. Each case diffs zvcs stdout against real git run in the same
//! repo (both read the same `.git/config`), so any behavioral drift fails the test.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A fixed list of short, distinct cells. 12 items at `--width=40` produces a
/// multi-row, multi-column table under `always`, so column vs row fill order and
/// trailing-space suppression are both exercised.
const LIST: &str = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\niota\nkappa\nlambda\nmu";

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A minimal repo whose `.git/config` both binaries will read. No commits are
/// needed — `git column` only consults config and stdin.
fn repo(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-colcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    (repo, home)
}

/// Run `<bin> column <extra>` in `dir`, feeding `LIST` on stdin. Both zvcs and
/// real git are driven identically; the environment isolates config to the repo.
fn run(bin: &str, dir: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["column"];
    args.extend_from_slice(extra);
    let mut child = Command::new(bin)
        .args(&args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .env_remove("COLUMNS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(LIST.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// zvcs stdout must equal real git stdout, byte-for-byte, for the same repo config
/// and CLI. Returns the shared output so callers can additionally assert its shape.
fn assert_match(repo: &Path, home: &Path, extra: &[&str]) -> Vec<u8> {
    let z = run(BIN, repo, home, extra);
    let g = run("git", repo, home, extra);
    assert!(z.status.success(), "zvcs column {extra:?} failed: {:?}", z);
    assert!(g.status.success(), "git column {extra:?} failed: {:?}", g);
    assert_eq!(
        z.stdout,
        g.stdout,
        "column {extra:?} diverged from git\nzvcs: {:?}\n git: {:?}",
        String::from_utf8_lossy(&z.stdout),
        String::from_utf8_lossy(&g.stdout)
    );
    z.stdout
}

/// `column.ui=always` lays the list out in a table; `column.ui=never` prints one
/// cell per line. `--mode` (bare) forces `always` regardless of a `never` config,
/// and `--no-mode` forces the plain fallback regardless of an `always` config.
#[test]
fn column_ui_sets_default_and_mode_flag_overrides() {
    let (repo, home) = repo("ui");

    // always -> a real table (more than one cell on the first line).
    git(&repo, &["config", "column.ui", "always"]);
    let always = assert_match(&repo, &home, &["--width=40"]);
    let first = String::from_utf8_lossy(&always);
    let first_line = first.lines().next().unwrap();
    assert!(
        first_line.split_whitespace().count() > 1,
        "column.ui=always must produce a table, got first line {first_line:?}"
    );

    // --no-mode overrides the always config back to one-per-line plain output.
    let plain = assert_match(&repo, &home, &["--no-mode", "--width=40"]);
    assert_eq!(
        String::from_utf8_lossy(&plain).lines().count(),
        12,
        "--no-mode overrides column.ui=always to one cell per line"
    );

    // never -> one cell per line.
    git(&repo, &["config", "column.ui", "never"]);
    let never = assert_match(&repo, &home, &["--width=40"]);
    assert_eq!(
        String::from_utf8_lossy(&never).lines().count(),
        12,
        "column.ui=never must print every cell on its own line"
    );

    // bare --mode overrides the never config to a table.
    let forced = assert_match(&repo, &home, &["--mode", "--width=40"]);
    assert!(
        String::from_utf8_lossy(&forced).lines().next().unwrap().split_whitespace().count() > 1,
        "--mode overrides column.ui=never back to a table"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// A layout token with no enable state (`column`, `dense column`) implies `always`,
/// and `--mode=<layout>` overrides the config's layout while staying enabled.
#[test]
fn column_ui_layout_token_implies_always_and_mode_value_overrides() {
    let (repo, home) = repo("layout");

    // `column` alone (a layout, not an enable state) implies always.
    git(&repo, &["config", "column.ui", "column"]);
    let col = assert_match(&repo, &home, &["--width=40"]);
    assert!(
        String::from_utf8_lossy(&col).lines().next().unwrap().split_whitespace().count() > 1,
        "a bare layout token in column.ui implies always"
    );

    // `dense column` also implies always; dense packs more columns per row.
    git(&repo, &["config", "column.ui", "dense column"]);
    assert_match(&repo, &home, &["--width=40"]);

    // --mode=row overrides the config's column layout (row-major fill order),
    // still byte-for-byte with git.
    assert_match(&repo, &home, &["--mode=row", "--width=40"]);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `--command=<name>` (first argument) makes `git column` read `column.<name>`
/// instead of `column.ui`; `column.ui` is consulted first but a table only renders
/// when the resolved value enables it. Here `column.ui=never` but `column.foo=always`.
#[test]
fn column_command_section_selects_alternate_config_key() {
    let (repo, home) = repo("command");
    git(&repo, &["config", "column.ui", "never"]);
    git(&repo, &["config", "column.foo", "always"]);

    // With --command=foo the always from column.foo wins -> a table.
    let out = assert_match(&repo, &home, &["--command=foo", "--width=40"]);
    assert!(
        String::from_utf8_lossy(&out).lines().next().unwrap().split_whitespace().count() > 1,
        "--command=foo must read column.foo=always and render a table"
    );

    // Without --command only column.ui=never applies -> one cell per line.
    let plain = assert_match(&repo, &home, &["--width=40"]);
    assert_eq!(
        String::from_utf8_lossy(&plain).lines().count(),
        12,
        "without --command, column.ui=never governs"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `column.ui=auto` piped to a non-terminal resolves to disabled (one cell per
/// line), exactly as git's `finalize_colopts` does with no tty and no pager.
#[test]
fn column_ui_auto_to_pipe_is_disabled() {
    let (repo, home) = repo("auto");
    git(&repo, &["config", "column.ui", "auto"]);

    let out = assert_match(&repo, &home, &["--width=40"]);
    assert_eq!(
        String::from_utf8_lossy(&out).lines().count(),
        12,
        "column.ui=auto to a pipe must fall back to one cell per line"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// An unparseable `column.ui` value is a fatal config error: exit 128, and the
/// first two diagnostic lines match git. zvcs omits git's trailing
/// ` in file '<path>' at line <n>` suffix (gitoxide does not surface it — a
/// documented divergence in column.rs), so only the shared prefix is asserted.
#[test]
fn column_ui_invalid_value_is_fatal_128() {
    let (repo, home) = repo("invalid");
    git(&repo, &["config", "column.ui", "bogus"]);

    let z = run(BIN, &repo, &home, &["--width=40"]);
    let g = run("git", &repo, &home, &["--width=40"]);

    assert_eq!(z.status.code(), Some(128), "bad column.ui must exit 128");
    assert_eq!(g.status.code(), Some(128), "sanity: git exits 128 too");
    assert!(z.stdout.is_empty(), "no table on a config error");

    let zerr = String::from_utf8_lossy(&z.stderr);
    assert!(
        zerr.contains("error: unsupported option 'bogus'\n")
            && zerr.contains("error: invalid column.ui mode bogus\n")
            && zerr.contains("fatal: bad config variable 'column.ui'"),
        "zvcs error text must match git's first two lines and the fatal line:\n{zerr}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
