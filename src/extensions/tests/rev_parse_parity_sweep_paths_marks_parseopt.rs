//! Gaps a differential sweep of `rev-parse` against stock found outside the
//! harness corpus:
//!
//! * below the git directory, GIT_DIR_BARE discovery chdir()s back to the cwd
//!   (`setup.c:1303-1308`), so `--path-format=relative` measures from where the
//!   command was typed: `../` from `.git/refs`, not `./`.
//! * `at_mark()` (`object-name.c:638-650`) only needs the mark to *start* at the
//!   `@`, so `@{u}^{tree}` is no reflog operand and peels the upstream.
//! * `reinterpret()` drops `nonfatal_dangling_mark` (`object-name.c:1376-1400`):
//!   `--verify -q @{-1}@{u}` still dies when the prior branch has no upstream,
//!   while an `@{-<n>}` the reflog does not reach (`object-name.c:1483-1486`)
//!   leaves `@{-9}@{u}` an unknown revision.
//! * 2.56's `repo_for_each_abbrev()` has no minimum length; a one-character
//!   prefix reads only the loose fan-out directory its zero padding names, and
//!   `match_hash()` lists no packed object below two characters.
//! * `--parseopt`: `--end-of-options` is dropped and ends option parsing,
//!   `do_get_value()` refuses a value on a long option that takes none,
//!   `register_abbrev()` treats the same option registered twice as ambiguous,
//!   and `optname()` names an option the way it was typed.
//! * `-n <count>` is echoed as a flag pair and `-n` alone dies
//!   (`builtin/rev-parse.c:799-811`).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str], stdin: &str) -> (String, String, i32) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run git");
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// `main` (three commits) tracking `refs/remotes/origin/main`, and `side`,
/// checked out once so `@{-1}` names it, with no upstream. Built by stock git
/// for both sides so the object ids agree.
fn fixture(stock: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-rpsweep35-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| assert_eq!(run(stock, &dir, args, "").2, 0, "{args:?}");
    git(&["init", "-q", "-b", "main", "."]);
    for n in ["1", "2", "3"] {
        std::fs::write(dir.join("f"), n).unwrap();
        git(&["add", "f"]);
        git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", n]);
    }
    git(&["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
    git(&["config", "branch.main.remote", "origin"]);
    git(&["config", "branch.main.merge", "refs/heads/main"]);
    git(&["checkout", "-q", "-b", "side"]);
    git(&["checkout", "-q", "main"]);
    dir
}

fn same(stock: &str, s: &Path, z: &Path, args: &[&str], stdin: &str) {
    assert_eq!(run(BIN, z, args, stdin), run(stock, s, args, stdin), "{args:?} <<< {stdin:?}");
}

#[test]
fn paths_marks_abbrevs_and_dash_n() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock, "s"), fixture(stock, "z"));
    for args in [
        &["rev-parse", "--path-format=relative", "--git-dir", "--git-common-dir"][..],
        &["rev-parse", "--path-format=relative", "--git-path", "objects"],
    ] {
        same(stock, &s.join(".git/refs/heads"), &z.join(".git/refs/heads"), args, "");
    }
    for operand in ["@{u}^{tree}", "main@{U}^{commit}", "@{push}^{tree}", "@{-1}@{u}", "@{-9}@{u}", "@@{u}^0"] {
        for modes in [&[][..], &["--verify", "-q"], &["--abbrev-ref"]] {
            let mut args = vec!["rev-parse"];
            args.extend_from_slice(modes);
            args.push(operand);
            same(stock, &s, &z, &args, "");
        }
    }
    for args in [&["rev-parse", "-n", "3", "HEAD"][..], &["rev-parse", "-n"], &["rev-parse", "--flags", "-n5", "-n", "2"]] {
        same(stock, &s, &z, args, "");
    }
    // The empty prefix, and the first hex digit of every loose fan-out
    // directory: each reads its `<c>0` directory, whether or not it exists.
    let mut prefixes = vec![String::new()];
    for e in std::fs::read_dir(s.join(".git/objects")).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()) && !prefixes.contains(&name[..1].to_string()) {
            prefixes.push(name[..1].to_string());
        }
    }
    for packed in [false, true] {
        if packed {
            for d in [&s, &z] {
                assert_eq!(run(stock, d, &["-c", "maintenance.auto=false", "repack", "-adq"], "").2, 0);
            }
        }
        for p in &prefixes {
            same(stock, &s, &z, &["rev-parse", &format!("--disambiguate={p}")], "");
        }
    }
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}

#[test]
fn parseopt_end_of_options_noarg_values_and_abbreviations() {
    let Some(stock) = stock_git() else { return };
    let dir = std::env::temp_dir().join(format!("zvcs-rpsweep35-po-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let spec = "cmd [options]\n--\nh,help show help\ncswitch a switch\ne,fuz=x required\nnoneg! cannot negate\nno-pos negated in table\nq,quiet be quiet\n";
    for argv in [
        &["--end-of-options", "-q"][..],
        &["-q", "--end-of-options", "--", "x"],
        &["--c=x"],
        &["--cswitch=x"],
        &["--qu=1"],
        &["--no-quiet=1"],
        &["--no-"],
        &["--no-pos=1"],
        &["-e"],
        &["--fuz"],
        &["-qe"],
    ] {
        let modes: &[&[&str]] = match argv[0] {
            "--end-of-options" | "-q" => &[&[], &["--keep-dashdash"]],
            _ => &[&[]],
        };
        for mode in modes {
            let mut args = vec!["rev-parse", "--parseopt"];
            args.extend_from_slice(mode);
            args.push("--");
            args.extend_from_slice(argv);
            assert_eq!(run(BIN, &dir, &args, spec), run(stock, &dir, &args, spec), "{args:?}");
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}
