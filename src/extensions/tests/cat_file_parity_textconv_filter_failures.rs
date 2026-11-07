//! `git cat-file --textconv` / `--filters` when the configured program fails.
//!
//! A textconv program that cannot be started is `start_command()`'s
//! `error_errno("cannot run %s", cmd->args.v[0])` (run-command.c:759-761) ahead of
//! `fill_textconv()`'s `unable to read files to diff`; a required smudge filter
//! that fails is `convert_to_working_tree()`'s `die("%s: smudge filter %s failed")`
//! (convert.c:1518), exit 128, in the single-object and the batch forms alike.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str], stdin: &str) -> (String, String, Option<i32>) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn fixture(stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-cfconvfail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join(".gitattributes"), "*.up diff=upper\n*.f filter=rot\n").unwrap();
    std::fs::write(dir.join("t.up"), "lower\n").unwrap();
    std::fs::write(dir.join("r.f"), "abc\n").unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"], "");
    run(stock, &dir, &["config", "filter.rot.clean", "tr a-z n-za-m"], "");
    run(stock, &dir, &["add", "."], "");
    assert_eq!(run(stock, &dir, &["commit", "-qm", "one"], "").2, Some(0));
    dir
}

#[test]
fn failing_conversion_programs_report_as_stock_does() {
    let Some(stock) = stock_git() else { return };
    let dir = fixture(stock);
    let missing = "diff.upper.textconv=zvcs-no-such-textconv-program";
    let cases: &[(&[&str], &str)] = &[
        (&["-c", missing, "cat-file", "--textconv", "HEAD:t.up"], ""),
        (&["-c", missing, "cat-file", "--textconv", "--path=t.up", "HEAD:r.f"], ""),
        (&["-c", "filter.rot.smudge=false", "-c", "filter.rot.required=true", "cat-file", "--filters", "HEAD:r.f"], ""),
        (&["-c", "filter.rot.smudge=false", "cat-file", "--filters", "HEAD:r.f"], ""),
        (
            &["-c", "filter.rot.smudge=false", "-c", "filter.rot.required=true", "cat-file", "--batch", "--filters"],
            "HEAD:r.f r.f\n",
        ),
        (&["-c", "filter.rot.smudge=false", "-c", "filter.rot.required=true", "cat-file", "--batch=%(objectsize)", "--filters"], "HEAD:r.f r.f\n"),
    ];
    for (args, input) in cases {
        assert_eq!(run(BIN, &dir, args, input), run(stock, &dir, args, input), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
