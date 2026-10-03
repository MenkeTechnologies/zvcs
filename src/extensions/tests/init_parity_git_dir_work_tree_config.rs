//! `GIT_DIR=<new> GIT_WORK_TREE=<tree> git init` writes `core` in git's order.
//!
//! `create_default_files()` (setup.c) sets `core.bare = false`, then
//! `core.logallrefupdates`, then `core.worktree`, and only after those runs its
//! filesystem probes (`core.ignorecase`, `core.precomposeunicode` on a
//! case-insensitive or decomposing filesystem). zvcs let gitoxide lay the
//! skeleton down with the probe keys and appended the three after them.
//! Expectations captured from stock git 2.56.0.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[test]
fn work_tree_keys_precede_the_filesystem_probes() {
    let root = std::env::temp_dir().join(format!("zvcs-init-gitdir-worktree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let out = Command::new(BIN)
        .args(["init", "-q"])
        .current_dir(&root)
        .env("HOME", &root)
        .env("ZVCS_HOME", &root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_DIR", "gd")
        .env("GIT_WORK_TREE", "wt")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let config = std::fs::read_to_string(root.join("gd/config")).unwrap();
    let wt = root.canonicalize().unwrap().join("wt");
    let head = format!(
        "[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = false\n\
         \tlogallrefupdates = true\n\tworktree = {}\n",
        wt.display()
    );
    assert!(config.starts_with(&head), "{config}");
    let rest = &config[head.len()..];
    assert!(
        rest.lines().all(|l| l == "\tignorecase = true" || l == "\tprecomposeunicode = true"),
        "{config}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
