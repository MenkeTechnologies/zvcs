//! `cmd_fmt_merge_msg` runs `repo_config(repo, fmt_merge_msg_config, …)`
//! (builtin/fmt-merge-msg.c:56) before `parse_options()`, so a value that
//! callback refuses — `merge.log` through `git_config_bool_or_int()`, which dies
//! in `die_bad_number()` — ends the command at 128 ahead of `-h` and ahead of the
//! `argc > 0` usage error. The port read `merge.log` leniently after parsing, so
//! `git fmt-merge-msg HEAD` answered the usage block at 129 instead.
//!
//! Every expectation was measured against stock git 2.55.0.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn run(repo: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .env("ZVCS_HOME", repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-fmtgate-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "f\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-qm", "one"]);
    repo
}

fn assert_dies(out: &Output, stderr: &str) {
    assert_eq!(out.status.code(), Some(128));
    assert!(out.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&out.stderr), stderr);
}

#[test]
fn a_bad_merge_log_in_a_file_beats_the_usage_error_and_help() {
    let repo = fixture("file");
    git(&repo, &["config", "merge.log", "bogus"]);
    let refusal =
        "fatal: bad numeric config value 'bogus' for 'merge.log' in file .git/config: invalid unit\n";
    assert_dies(&run(&repo, &["fmt-merge-msg", "HEAD"], "x\n"), refusal);
    assert_dies(&run(&repo, &["fmt-merge-msg", "-h"], ""), refusal);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn a_negative_length_and_the_default_chain_are_refused_too() {
    let repo = fixture("cmdline");
    // `return error(...)` from the callback: `configset_iter()` adds the
    // `unable to parse` line.
    assert_dies(
        &run(&repo, &["-c", "merge.log=-1", "fmt-merge-msg"], ""),
        "error: merge.log: negative length -1\n\
         fatal: unable to parse 'merge.log' from command-line config\n",
    );
    // Everything else falls through to `git_default_config`.
    assert_dies(
        &run(&repo, &["-c", "core.createObject=bogus", "fmt-merge-msg", "-h"], ""),
        "fatal: invalid mode for object creation: bogus\n",
    );
    let _ = std::fs::remove_dir_all(&repo);
}
