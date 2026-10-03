//! `git remote rename` with no identity configured moves the tracking refs and
//! rewrites the fetch refspec, as it does with one.
//!
//! The rename's reflog line (`remote: renamed <old> to <new>`) is signed with
//! `git_committer_info(0)`, which falls back to the system identity rather than
//! failing; a refusal there would leave the config section renamed and the
//! refs and refspec still under the old name. Expectations captured from stock
//! git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-remote-rename-no-ident-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .env("EMAIL", "t@e.x")
        .current_dir(dir)
        .output()
        .expect("run the binary under test")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn refs_and_refspec_follow_the_new_name() {
    let w = scratch("rename");
    git(&w, &["init", "-q", "-b", "main", "."]);
    git(&w, &["-c", "user.name=t", "-c", "user.email=t@e.x", "commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&w, &["remote", "add", "o", "."]);
    assert_eq!(git(&w, &["fetch", "-q", "o"]).status.code(), Some(0));

    let out = git(&w, &["remote", "rename", "o", "p"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));

    assert_eq!(
        stdout(&git(&w, &["for-each-ref", "--format=%(refname)", "refs/remotes"])),
        "refs/remotes/p/HEAD\nrefs/remotes/p/main\n"
    );
    assert_eq!(
        stdout(&git(&w, &["config", "--get-all", "remote.p.fetch"])),
        "+refs/heads/*:refs/remotes/p/*\n"
    );
    let log = std::fs::read_to_string(w.join(".git/logs/refs/remotes/p/main")).expect("reflog");
    assert!(
        log.lines().last().unwrap().ends_with("\tremote: renamed refs/remotes/o/main to refs/remotes/p/main"),
        "{log}"
    );
}
