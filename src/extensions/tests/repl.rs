//! `git zrepl` runs piped commands line-by-line and exits on `quit`/EOF.

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[test]
fn zrepl_runs_piped_commands_then_quits() {
    let home = std::env::temp_dir().join(format!("zvcs-repl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    let mut child = Command::new(BIN)
        .args(["zrepl"])
        .env("ZVCS_HOME", &home)
        .current_dir(&home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"zjobs\nquit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success(), "zrepl exited non-zero");
    assert!(
        stdout.to_lowercase().contains("no jobs"),
        "zrepl did not run the piped `zjobs`; stdout:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&home);
}
