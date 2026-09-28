//! `git fetch <remote> tag` with no tag name.
//!
//! `fetch_one()` expands `tag <name>` into `refs/tags/<name>:refs/tags/<name>`
//! and, with the name missing, `die(_("you need to specify a tag name"))`
//! (builtin/fetch.c:2433-2438): a `fatal:` at 128. zvcs reported it as a
//! `zvcs: fetch:` error at exit 1.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[test]
fn a_missing_tag_name_is_fatal() {
    let root = std::env::temp_dir().join(format!("zvcs-fetch-tag-no-name-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let git = |args: &[&str]| {
        Command::new(BIN)
            .args(args)
            .current_dir(&root)
            .env("HOME", &root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .output()
            .unwrap()
    };
    git(&["init", "-q", "-b", "main", "."]);
    let out = git(&["fetch", ".", "tag"]);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code()
        ),
        (String::new(), "fatal: you need to specify a tag name\n".into(), Some(128))
    );
}
