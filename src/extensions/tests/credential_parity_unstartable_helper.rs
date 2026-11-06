//! `git credential fill` with a helper that cannot run, followed by one that
//! answers, compared with stock git.
//!
//! `credential_do()` appends the operation to the helper's text and
//! `run_credential_helper()` runs it with `use_shell` (credential.c:446-500),
//! so `prepare_shell_cmd()` always hands it to `sh -c '<cmd>' '<cmd>'`
//! (run-command.c:288-309): a missing or non-executable helper is the shell's
//! to report, with the command as `$0`, and its failure only ends that helper.
//! `credential_fill()` goes on to the next one whatever `credential_do()`
//! returned (credential.c:519-520).

use std::path::Path;
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[derive(Debug, PartialEq)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(bin: &str, dir: &Path, helper: &str) -> Run {
    let answer = dir.join("answer.sh");
    let mut child = Command::new(bin)
        .args(["-c", &format!("credential.helper={helper}")])
        .args(["-c", &format!("credential.helper={}", answer.display())])
        .args(["credential", "fill"])
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"protocol=https\nhost=example.com\n\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let dir = dir.to_string_lossy();
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).replace(dir.as_ref(), "DIR"),
        stderr: String::from_utf8_lossy(&out.stderr).replace(dir.as_ref(), "DIR"),
    }
}

#[test]
fn credential_parity_helper_that_cannot_run_is_skipped() {
    let Some(stock) = stock_git() else { return };
    let dir = std::env::temp_dir().join(format!("zvcs-cred-unstartable-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("a-directory")).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    std::fs::write(dir.join("answer.sh"), "#!/bin/sh\necho username=u\necho password=p\n").unwrap();
    std::fs::write(dir.join("not-executable.sh"), "#!/bin/sh\necho username=x\n").unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir.join("answer.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(dir.join("not-executable.sh"), std::fs::Permissions::from_mode(0o644)).unwrap();

    let helpers = [
        format!("{}/missing", dir.display()),
        format!("{}/not-executable.sh", dir.display()),
        format!("{}/a-directory", dir.display()),
        "!nonexistent-cmd-zvcs".to_string(),
        "!nonexistent-cmd-zvcs --with arg".to_string(),
    ];
    for helper in &helpers {
        let want = run(stock, &dir, helper);
        let got = run(BIN, &dir, helper);
        assert_eq!(got, want, "credential.helper={helper}: zvcs and stock git differ");
        assert_eq!(want.code, Some(0), "{helper}: the next helper answers");
        assert!(!want.stderr.is_empty(), "{helper}: the shell reports the helper");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
