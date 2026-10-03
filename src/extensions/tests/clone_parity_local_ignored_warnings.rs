//! The shallow selectors and `--filter` a local clone ignores are warned about
//! under `-q` too.
//!
//! `cmd_clone()` reports each with `warning()` (builtin/clone.c:1325-1333),
//! which `-q` does not reach: only the banner and progress follow
//! `option_verbosity`. zvcs printed the warnings only without `-q`.
//! Expectations captured from stock git 2.56.0.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env_remove("GIT_DIR")
        .output()
        .expect("run the binary under test");
    (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
}

#[test]
fn quiet_local_clone_still_warns() {
    let root = std::env::temp_dir().join(format!("zvcs-clone-local-ignored-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main", "src"]);
    git(&root.join("src"), &["commit", "-q", "--allow-empty", "-m", "c1"]);
    let (err, code) = git(&root, &["clone", "-q", "--depth=1", "--filter=blob:none", "src", "d"]);
    assert_eq!(code, 0);
    assert_eq!(
        err,
        "warning: --depth is ignored in local clones; use file:// instead.\n\
         warning: --filter is ignored in local clones; use file:// instead.\n"
    );
    let _ = std::fs::remove_dir_all(&root);
}
