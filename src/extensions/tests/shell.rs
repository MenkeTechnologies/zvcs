//! The shell-convenience verbs make `zrepl` navigable like a shell: because the
//! console is one process, `zcd` and `zenv` mutate state that later lines see.
//! Driving them through a piped `git zrepl` session is the real usage path and
//! keeps the state-persistence guarantee under test.

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Feed `script` to `git zrepl` on stdin (non-tty → the raw line reader) and
/// return its stdout. A private ZVCS_HOME keeps it off the real daemon/ledger.
fn repl(script: &str) -> String {
    let home = std::env::temp_dir().join(format!("zvcs-shell-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&home);
    let mut child = Command::new(BIN)
        .arg("zrepl")
        .env("ZVCS_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn zrepl");
    child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&home);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn zcd_persists_across_console_lines() {
    let target = std::env::temp_dir().join(format!("zvcs-cd-{}", std::process::id()));
    std::fs::create_dir_all(&target).unwrap();
    let canon = target.canonicalize().unwrap();

    let out = repl(&format!("zcd {}\nzpwd\n", target.display()));
    let landed = out.lines().any(|l| l == canon.to_string_lossy());
    assert!(landed, "zpwd after `zcd` should report {}:\n{out}", canon.display());

    let _ = std::fs::remove_dir_all(&target);
}

#[test]
fn zenv_set_query_and_unset_round_trip() {
    // Set a var, read it back, unset it, read again (now empty). All in one
    // session so the process-global env carries between lines.
    let out = repl("zenv ZVCS_SHELL_TEST=marker\nzenv ZVCS_SHELL_TEST\nzunset ZVCS_SHELL_TEST\nzenv ZVCS_SHELL_TEST\n");
    let markers = out.lines().filter(|l| *l == "marker").count();
    assert_eq!(markers, 1, "the value should print once (set→query), then be gone after zunset:\n{out}");
}

#[test]
fn zecho_joins_args_and_honors_dash_n() {
    // `-n` suppresses the newline, so the next echo's output concatenates.
    let out = repl("zecho -n abc\nzecho def\n");
    assert!(out.contains("abcdef"), "zecho -n should suppress the newline:\n{out}");
}
