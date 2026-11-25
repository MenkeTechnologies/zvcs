//! `git merge-tree` against stock git on four things the parity corpus does not reach.
//!
//! * An add/add whose sides differ in the executable bit goes through
//!   handle_content_merge() unswapped: `a->mode` wins and the merge is unclean
//!   (merge-ort.c:2212-2218), even when both sides added the same blob, which is
//!   the trivial oid merge (merge-ort.c:2243-2246). The first output line is the
//!   written tree's id, so a dropped path or a wrong mode shows up there.
//! * The deprecated three-tree mode warns `Cannot merge binary files: <path> (.our
//!   vs. .their)` (merge-blobs.c:54-56), and show_outf() prints each xdiff buffer
//!   with `%.*s` (builtin/merge-tree.c:112-118), so a binary line stops at its NUL.
//! * init_basic_merge_options() runs before parse_options()
//!   (builtin/merge-tree.c:592-593), so a bad merge setting dies ahead of `-h`, a
//!   bad operand and the trivial merge.
//! * `--stdin` with `--merge-base` dies before stdin is read
//!   (builtin/merge-tree.c:620-622).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str], stdin: &[u8]) -> (i32, Vec<u8>, Vec<u8>) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.status.code().unwrap_or(-1), out.stdout, out.stderr)
}

fn write(dir: &Path, name: &str, content: &[u8], executable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    let mode = if executable { 0o755 } else { 0o644 };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// `base` with `o` and `bin.dat`; `x1` adds an executable `m`, `x2` the same blob
/// without the bit, `x3` other content without it; `b1`/`b2` rewrite `bin.dat`.
fn fixture(stock: &str, name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-merge-tree-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let run = |args: &[&str]| assert_eq!(git(stock, &dir, args, b"").0, 0, "{args:?}");
    let commit = |msg: &str| run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", msg]);
    run(&["init", "-q", "-b", "main", "."]);
    write(&dir, "o", b"o\n", false);
    write(&dir, "bin.dat", b"bin\0base\n", false);
    run(&["add", "."]);
    commit("base");
    for (branch, content, exec) in [("x1", &b"same\n"[..], true), ("x2", b"same\n", false), ("x3", b"diff\n", false)] {
        run(&["checkout", "-q", "-b", branch, "main"]);
        write(&dir, "m", content, exec);
        run(&["add", "m"]);
        commit(branch);
    }
    for (branch, content) in [("b1", &b"bin\0one\n"[..]), ("b2", b"bin\0two\n")] {
        run(&["checkout", "-q", "-b", branch, "main"]);
        write(&dir, "bin.dat", content, false);
        run(&["add", "bin.dat"]);
        commit(branch);
    }
    run(&["checkout", "-q", "main"]);
    dir
}

#[test]
fn add_add_mode_conflicts_keep_the_first_side_mode_and_stay_unclean() {
    let Some(stock) = stock_git::stock_git() else { return };
    let dir = fixture(stock, "modes");
    for pair in [["x1", "x2"], ["x2", "x1"], ["x1", "x3"], ["x3", "x1"]] {
        for extra in [&[][..], &["--name-only"], &["-X", "theirs"]] {
            let mut args = vec!["merge-tree"];
            args.extend_from_slice(extra);
            args.extend_from_slice(&pair);
            let s = git(stock, &dir, &args, b"");
            let z = git(ZVCS, &dir, &args, b"");
            assert_eq!(z, s, "{args:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn trivial_merge_warns_on_binary_and_prints_records_up_to_their_nul() {
    let Some(stock) = stock_git::stock_git() else { return };
    let dir = fixture(stock, "trivial");
    for args in [
        &["merge-tree", "main", "b1", "b2"][..],
        &["merge-tree", "--trivial-merge", "main", "b2", "b1"],
        &["merge-tree", "main", "x1", "b1"],
    ] {
        let s = git(stock, &dir, args, b"");
        let z = git(ZVCS, &dir, args, b"");
        assert_eq!(z, s, "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_settings_are_validated_before_options_and_operands() {
    let Some(stock) = stock_git::stock_git() else { return };
    let dir = fixture(stock, "config");
    for config in ["merge.verbosity=bad", "merge.renames=bogus", "merge.conflictStyle=bogus"] {
        for rest in [
            &["-h"][..],
            &["--bogus"],
            &["nosuch", "x1"],
            &["a", "b", "c", "d"],
            &["main", "b1", "b2"],
            &["--merge-base=nosuch", "x1", "x2"],
            &["--stdin"],
        ] {
            let mut args = vec!["-c", config, "merge-tree"];
            args.extend_from_slice(rest);
            let s = git(stock, &dir, &args, b"x1 x2\n");
            let z = git(ZVCS, &dir, &args, b"x1 x2\n");
            assert_eq!(z, s, "{args:?}");
        }
    }
    for args in [&["merge-tree", "--stdin", "--merge-base=main"][..], &["merge-tree", "--merge-base", "main", "--stdin"]] {
        let s = git(stock, &dir, args, b"x1 x2\n");
        let z = git(ZVCS, &dir, args, b"x1 x2\n");
        assert_eq!(z, s, "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
