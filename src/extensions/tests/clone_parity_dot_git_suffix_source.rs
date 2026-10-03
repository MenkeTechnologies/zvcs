//! A local source that only its `.git` spelling names.
//!
//! `get_repo_path_1()` (builtin/clone.c) and `enter_repo()` (setup.c), which
//! `upload-pack` runs on the path it is handed, both try `<path>/.git`,
//! `<path>`, `<path>.git/.git` and `<path>.git` in that order. So `git clone
//! src` with only `src.git` on disk — bare, or a work tree of its own — is a
//! local clone of it: `done.`, and `remote.origin.url` is the operand made
//! absolute, without the suffix. zvcs's transport tried `<path>` and
//! `<path>/.git` only and failed with gitoxide's `Could not verify that "<path>"
//! url is a valid git directory`; its local-clone test required the operand
//! itself to be a directory. Expectations captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-clone-dot-git-suffix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .current_dir(dir)
        .output()
        .expect("run the binary under test");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

#[test]
fn the_dot_git_spelling_of_a_local_source_is_cloned_locally() {
    let root = scratch();
    git(&root, &["init", "-q", "-b", "main", "work.git"]);
    git(&root.join("work.git"), &["commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&root, &["clone", "-q", "--bare", "work.git", "bare.git"]);
    let tip = git(&root.join("work.git"), &["rev-parse", "HEAD"]).0;

    for source in ["work", "bare"] {
        let dest = format!("{source}-clone");
        let (_, err, code) = git(&root, &["clone", source, &dest]);
        assert_eq!((err.as_str(), code), (format!("Cloning into '{dest}'...\ndone.\n").as_str(), 0));
        let clone = root.join(&dest);
        assert_eq!(git(&clone, &["rev-parse", "HEAD"]).0, tip, "{source}");
        let url = git(&clone, &["config", "remote.origin.url"]).0;
        assert_eq!(url.trim_end(), root.canonicalize().unwrap().join(source).display().to_string(), "{source}");
    }

    let (out, _, code) = git(&root, &["ls-remote", "bare", "HEAD"]);
    assert_eq!((out, code), (format!("{}\tHEAD\n", tip.trim_end()), 0));
    let _ = std::fs::remove_dir_all(&root);
}
