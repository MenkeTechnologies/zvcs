//! `git fast-import`'s temporary packfile, and the option checks that decide
//! which exit route reaches it.
//!
//! `cmd_fast_import` opens `objects/pack/tmp_pack_XXXXXX` in `start_packfile()`
//! before it reads the stream and before argv is parsed at all
//! (`builtin/fast-import.c:3978` against `parse_argv()` at 4020), so the state a
//! rejected command line leaves behind depends entirely on how it is rejected:
//! `usage()` calls `exit(129)` without running `die_nicely`, so the temporary
//! survives, while `die()` and a clean run both reach `end_packfile()` and
//! unlink it. Every expectation below was checked against stock git 2.55.0 in
//! the same fixture before being written down.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A fresh repository under a unique temp dir, with its own `ZVCS_HOME`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fi-pack-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    assert!(
        Command::new(BIN)
            .args(["init", "-q", "-b", "main"])
            .current_dir(&repo)
            .env("ZVCS_HOME", root.join("home"))
            .status()
            .unwrap()
            .success(),
        "git init failed"
    );
    (root, repo)
}

/// Run `git fast-import <args>` with `stream` on stdin, returning stdout, stderr
/// and the exit code.
fn fast_import(repo: &Path, home: &Path, args: &[&str], stream: &str) -> (String, String, i32) {
    let mut child = Command::new(BIN)
        .arg("fast-import")
        .args(args)
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// The `tmp_pack_*` entries currently in the object store.
fn temp_packs(repo: &Path) -> Vec<String> {
    let dir = repo.join(".git").join("objects").join("pack");
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("tmp_pack_"))
        .collect();
    names.sort();
    names
}

/// An argument that is not an option is rejected by `usage()`, which exits
/// without any of `die_nicely`'s cleanup — so the temporary packfile
/// `start_packfile()` already opened stays in the object store.
#[test]
fn usage_error_leaves_the_temporary_packfile() {
    let (root, repo) = fixture("usage-positional");
    let home = root.join("home");

    let (stdout, stderr, code) = fast_import(&repo, &home, &["does-not-exist"], "");

    assert_eq!(code, 129, "stderr: {stderr}");
    assert_eq!(stdout, "");
    assert!(stderr.starts_with("usage: git fast-import [--date-format=<f>]"), "{stderr}");
    assert_eq!(temp_packs(&repo).len(), 1, "expected exactly one temporary packfile");
}

/// The other `usage()` shape — a value outside the set an option names — takes
/// the same route out, so it leaves the same temporary behind.
#[test]
fn rejected_option_value_leaves_the_temporary_packfile() {
    let (root, repo) = fixture("usage-signed-tags");
    let home = root.join("home");

    let (_, stderr, code) = fast_import(&repo, &home, &["--signed-tags=%H%n"], "");

    assert_eq!(code, 129, "stderr: {stderr}");
    assert_eq!(stderr, "usage: unknown --signed-tags mode '%H%n'\n");
    assert_eq!(temp_packs(&repo).len(), 1, "expected exactly one temporary packfile");
}

/// A `die()`, by contrast, runs `die_nicely` — which calls `end_packfile()`
/// before `dump_marks()` — so nothing is left in the object store.
#[test]
fn fatal_error_removes_the_temporary_packfile() {
    let (root, repo) = fixture("fatal-unknown-option");
    let home = root.join("home");

    let (_, stderr, code) = fast_import(&repo, &home, &["--bogus-opt"], "");

    assert_eq!(code, 128, "stderr: {stderr}");
    assert_eq!(stderr, "fatal: unknown option --bogus-opt\n");
    assert_eq!(temp_packs(&repo), Vec::<String>::new());
}

/// `option_depth` holds `--depth` to `MAX_DEPTH`, `(1 << 13) - 1`, because the
/// delta depth is stored in a 13-bit bitfield. Over the ceiling is a `die()`,
/// not a `usage()`, which is what puts it on the cleanup path — and the value is
/// read with `strtoul(arg, NULL, 0)`, so the hexadecimal spelling of a legal
/// depth is accepted.
#[test]
fn depth_ceiling_is_a_fatal_and_the_radix_is_gits() {
    let (root, repo) = fixture("depth");
    let home = root.join("home");

    let (_, stderr, code) = fast_import(&repo, &home, &["--depth=8192"], "");
    assert_eq!(code, 128, "stderr: {stderr}");
    assert_eq!(stderr, "fatal: --depth cannot exceed 8191\n");
    assert_eq!(temp_packs(&repo), Vec::<String>::new());

    // `strtoul` saturates rather than failing, so an absurd value still reports
    // the ceiling instead of a parse error.
    let (_, stderr, code) = fast_import(&repo, &home, &["--depth=99999999999999999999999"], "");
    assert_eq!(code, 128, "stderr: {stderr}");
    assert_eq!(stderr, "fatal: --depth cannot exceed 8191\n");

    // Base 0: `0x1fff` is 8191, the largest depth git accepts.
    let (_, stderr, code) = fast_import(&repo, &home, &["--depth=0x1fff"], "");
    assert_eq!(code, 0, "stderr: {stderr}");

    // A `-` anywhere in the argument is rejected outright by `ulong_arg`.
    let (_, stderr, code) = fast_import(&repo, &home, &["--depth=-1"], "");
    assert_eq!(code, 128, "stderr: {stderr}");
    assert_eq!(stderr, "fatal: --depth: argument must be a non-negative integer\n");
}

/// A run that succeeds must leave the object store as clean as it found it: an
/// empty stream stores nothing, and `end_packfile()` discards the temporary.
#[test]
fn empty_stream_succeeds_and_leaves_nothing_behind() {
    let (root, repo) = fixture("empty-stream");
    let home = root.join("home");

    let (stdout, stderr, code) = fast_import(&repo, &home, &[], "");

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "");
    assert_eq!(temp_packs(&repo), Vec::<String>::new());
}

/// The same for a stream that actually imports: the ref lands, and the
/// temporary is still gone. The commit id is stock git 2.55.0's for this exact
/// stream — the identity line is copied verbatim, so it is a constant.
#[test]
fn real_import_lands_the_ref_and_leaves_nothing_behind() {
    let (root, repo) = fixture("real-import");
    let home = root.join("home");
    let stream = concat!(
        "blob\n",
        "mark :1\n",
        "data 4\n",
        "xyz\n",
        "\n",
        "commit refs/heads/imp\n",
        "mark :2\n",
        "committer p <p@e.invalid> 1700000000 +0000\n",
        "data 3\n",
        "hi\n",
        "M 100644 :1 f\n",
        "\n",
        "done\n",
    );

    let (_, stderr, code) = fast_import(&repo, &home, &["--done"], stream);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(temp_packs(&repo), Vec::<String>::new());

    let out = Command::new(BIN)
        .args(["rev-parse", "refs/heads/imp"])
        .current_dir(&repo)
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "62f002f5e350d49cd3e399bf596e74ef9c2d164a"
    );
}

/// `show_usage_if_asked` answers a lone help flag on *stdout* with exit 129, and
/// it runs before `start_packfile()`, so no temporary is created at all.
#[test]
fn lone_help_flag_prints_on_stdout_before_any_packfile() {
    let (root, repo) = fixture("help-flag");
    let home = root.join("home");

    for flag in ["-h", "--help-all"] {
        let (stdout, stderr, code) = fast_import(&repo, &home, &[flag], "");
        assert_eq!(code, 129, "{flag}: stderr: {stderr}");
        assert_eq!(stderr, "", "{flag} must not write to stderr");
        assert!(stdout.starts_with("usage: git fast-import [--date-format=<f>]"), "{stdout}");
        assert_eq!(temp_packs(&repo), Vec::<String>::new(), "{flag}");
    }

    // Only when it is the sole argument: alongside anything else it is just an
    // unrecognised option.
    let (_, stderr, code) = fast_import(&repo, &home, &["-h", "--quiet"], "");
    assert_eq!(code, 128, "stderr: {stderr}");
    assert_eq!(stderr, "fatal: unknown option -h\n");
}
