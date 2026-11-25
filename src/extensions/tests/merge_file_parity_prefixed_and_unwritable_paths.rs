//! `git merge-file` against stock git on the paths its errors name.
//!
//! * read_mmfile() is handed prefix_filename(prefix, argv[i])
//!   (builtin/merge-file.c:123, :133), so from a subdirectory `Could not stat`
//!   names the operand from the top of the work tree; an absolute operand and a
//!   run outside any repository name it as typed.
//! * The result is written with fopen(fpath, "wb") and fwrite()
//!   (builtin/merge-file.c:166-178): an unwritable `<current>` is `Could not open
//!   <argv[0]> for writing`, and the -1 replaces the conflict count, so the exit
//!   status is 255.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, ceiling: &Path, dir: &Path, args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", ceiling)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (out.status.code().unwrap_or(-1), out.stdout, out.stderr)
}

#[test]
fn operand_errors_name_the_prefixed_path_and_unwritable_output_exits_255() {
    use std::os::unix::fs::PermissionsExt;
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-merge-file-paths-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let sub = repo.join("a/b");
    let bare = root.join("plain");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(&bare).unwrap();
    let repo = std::fs::canonicalize(&repo).unwrap();
    let sub = std::fs::canonicalize(&sub).unwrap();
    let bare = std::fs::canonicalize(&bare).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    assert_eq!(git(stock, &root, &repo, &["init", "-q", "-b", "main", "."]).0, 0);
    for dir in [&sub, &bare] {
        std::fs::write(dir.join("base"), "a\nb\nc\n").unwrap();
        std::fs::write(dir.join("ours"), "a\nB1\nc\n").unwrap();
        std::fs::write(dir.join("theirs"), "a\nB2\nc\n").unwrap();
        std::fs::write(dir.join("ro"), "a\nB1\nc\n").unwrap();
        std::fs::set_permissions(dir.join("ro"), std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let absent = sub.join("absent");
    let absent = absent.to_str().unwrap();
    for dir in [&sub, &bare] {
        for args in [
            &["merge-file", "-p", "nosuch", "base", "theirs"][..],
            &["merge-file", "ours", "nosuch", "theirs"],
            &["merge-file", "-p", "ours", "base", "../nosuch"],
            &["merge-file", "-p", "-", "base", "theirs"],
            &["merge-file", "-p", "ours", "base", absent],
            &["merge-file", "ro", "base", "theirs"],
            &["merge-file", "-q", "ro", "base", "theirs"],
        ] {
            let s = git(stock, &root, dir, args);
            let z = git(ZVCS, &root, dir, args);
            assert_eq!(z, s, "{args:?} in {}", dir.display());
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
