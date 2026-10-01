//! `--compose` reads the subject of the first patch through
//! `get_patch_subject()`, which dies when no line starts with `Subject: `.
//! 2.56 reworded that `die` and gave it a trailing newline
//! (git-send-email.perl:866), so Perl no longer appends ` at <path> line <n>.`:
//! the whole of stderr is `No 'Subject:' line in '<file>'`.
//!
//! Measured against stock git 2.56.0 with `sendemail.from` set, so the last
//! child before the `die` is `rev-parse --verify` on the file operand and the
//! exit status is 1. `--dry-run` with the `die` before any send: no socket.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[test]
fn compose_on_a_patch_without_a_subject_line_dies_with_the_2_56_text() {
    let root = std::env::temp_dir().join(format!("zvcs-se-nosubj-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();
    let git = |args: &[&str]| {
        Command::new(BIN)
            .args(args)
            .current_dir(&repo)
            .env("HOME", &repo)
            .env("ZVCS_HOME", &repo)
            .env("GIT_CEILING_DIRECTORIES", &repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    assert!(git(&["init", "-q", "-b", "main"]).status.success());
    std::fs::write(repo.join("p.txt"), "hello\n").unwrap();

    let out = git(&[
        "-c",
        "sendemail.from=x@y.z",
        "send-email",
        "--compose",
        "--to=a@b.c",
        "--dry-run",
        "p.txt",
    ]);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "No 'Subject:' line in 'p.txt'\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "p.txt\n");
    assert_eq!(out.status.code(), Some(1));

    let _ = std::fs::remove_dir_all(&repo);
}
