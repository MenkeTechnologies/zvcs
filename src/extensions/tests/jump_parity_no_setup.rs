//! `git jump` is `contrib/git-jump`, a script git runs through
//! `execv_dashed_external()` without any repository setup. A `$GIT_DIR` that
//! names no repository therefore reaches the script, whose `mode_auto` sees
//! `git rev-parse --is-inside-work-tree` fail and prints its usage at exit 1 —
//! not setup's `fatal: not a git repository`. Measured on stock git 2.56.0:
//! 858 bytes of usage on stderr, nothing on stdout, exit 1.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-jump-no-setup-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let status = Command::new(BIN)
        .args(["init", "-q", "-b", "main"])
        .current_dir(&root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    root
}

#[test]
fn a_bogus_git_dir_reaches_the_scripts_usage() {
    let repo = fixture("bogus-git-dir");
    for common_dir in [None, Some(".git")] {
        let mut cmd = Command::new(BIN);
        cmd.args(["--git-dir=no-such", "jump", "--stdout", "--stdout"])
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        if let Some(dir) = common_dir {
            cmd.env("GIT_COMMON_DIR", dir);
        }
        let out = cmd.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{common_dir:?}: {stderr}");
        assert_eq!(out.stdout, b"");
        assert!(stderr.starts_with("usage: git jump [--stdout] <mode> [<args>]\n"), "{stderr}");
        assert_eq!(stderr.len(), 858);
    }
}
