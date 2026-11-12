//! Signing that fails before gpg ever runs, against stock git.
//!
//! An unknown `gpg.format` is git_gpg_config()'s `error(invalid value …)`
//! followed by the config reader's own die, git_die_config_linenr()
//! (gpg-interface.c:775-779, config.c:1654-1673), which names the command line
//! or the file and line the value came from; zvcs printed only the first line.
//! A `gpg.program` that cannot be started is `cannot run <prog>` when the PATH
//! lookup of a bare name finds nothing, but `fatal: cannot exec '<prog>'` from
//! the child when the name has a slash (run-command.c:403-404, :432-441,
//! :757-763); zvcs printed the first form for both.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

#[test]
fn bad_format_and_unstartable_program_match_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-tag-signing-setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("stock"), base.join("zvcs"));
    let cases: &[(Option<&str>, &[&str])] = &[
        (None, &["-c", "gpg.format=bogus", "tag", "-s", "-m", "m", "t"]),
        (Some("bogus"), &["tag", "-s", "-m", "m", "t"]),
        (Some("bogus"), &["commit", "-S", "--allow-empty", "-m", "m"]),
        (None, &["-c", "gpg.program=/nonexistent/gpg", "tag", "-s", "-m", "m", "t"]),
        (None, &["-c", "gpg.program=./nosuchgpg", "tag", "-s", "-m", "m", "t"]),
        (None, &["-c", "gpg.program=nosuchgpg", "tag", "-s", "-m", "m", "t"]),
    ];
    for (format, args) in cases {
        for root in [&s, &z] {
            let _ = std::fs::remove_dir_all(root);
            std::fs::create_dir_all(root).unwrap();
            run(stock, root, &["init", "-q", "-b", "main"]);
            run(stock, root, &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "one"]);
            if let Some(format) = format {
                run(stock, root, &["config", "gpg.format", format]);
            }
        }
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
        let refs: &[&str] = &["for-each-ref", "--format=%(refname) %(objectname)"];
        assert_eq!(run(stock, &z, refs), run(stock, &s, refs), "refs after {args:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
