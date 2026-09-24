//! `git last-modified --max-depth=<n>` is `OPT_INTEGER_F` into a C `int`
//! (builtin/last-modified.c:535), so a value `git_parse_signed()` refuses is a
//! `parse_options` error: one `error:` line naming the option, no usage block,
//! exit 129. The port used to `str::parse` the value and bail through anyhow,
//! which printed its own wording and exited 1.
//!
//! Every expectation was measured against stock git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .env("ZVCS_HOME", repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-lmdepth-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::create_dir_all(repo.join("d")).unwrap();
    std::fs::write(repo.join("d/f"), "f\n").unwrap();
    git(&repo, &["add", "d/f"]);
    git(&repo, &["commit", "-qm", "one"]);
    repo
}

fn assert_refused(repo: &Path, args: &[&str], stderr: &str) {
    let out = run(repo, args);
    assert_eq!(out.status.code(), Some(129), "{args:?}");
    assert!(out.stdout.is_empty(), "{args:?}: nothing on stdout");
    assert_eq!(String::from_utf8_lossy(&out.stderr), stderr, "{args:?}");
}

#[test]
fn a_bad_max_depth_is_an_option_error_at_129() {
    let repo = fixture("bad");
    let not_a_number =
        "error: option `max-depth' expects an integer value with an optional k/m/g suffix\n";
    assert_refused(&repo, &["last-modified", "--max-depth=false"], not_a_number);
    // The separate-value form goes through the same `OPTION_INTEGER` arm.
    assert_refused(&repo, &["last-modified", "--max-depth", "false"], not_a_number);
    // Last one wins only for values that parse: the refusal stops the line.
    assert_refused(
        &repo,
        &["last-modified", "--max-depth=0", "-r", "--max-depth=2", "--max-depth=false"],
        not_a_number,
    );
    assert_refused(
        &repo,
        &["last-modified", "--max-depth="],
        "error: option `max-depth' expects a numerical value\n",
    );
    assert_refused(
        &repo,
        &["last-modified", "--max-depth=99999999999"],
        "error: value 99999999999 for option `max-depth' not in range [-2147483648,2147483647]\n",
    );
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn a_unit_suffix_is_a_depth() {
    // `git_parse_signed()` takes `k`/`m`/`g`: `1k` is depth 1024, which reaches
    // `d/f` exactly as `-r` does.
    let repo = fixture("unit");
    let with_unit = run(&repo, &["last-modified", "--max-depth=1k"]);
    let recursive = run(&repo, &["last-modified", "-r"]);
    assert_eq!(with_unit.status.code(), Some(0));
    assert_eq!(with_unit.stdout, recursive.stdout);
    assert!(String::from_utf8_lossy(&with_unit.stdout).contains("\td/f\n"));
    let _ = std::fs::remove_dir_all(&repo);
}
