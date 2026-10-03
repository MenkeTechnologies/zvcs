//! The destination `git clone` cannot create.
//!
//! builtin/clone.c:1108-1123 runs `safe_create_leading_directories_const()` on
//! the work tree (`die_errno`), `mkdir()`s it (`die_errno`, `could not create
//! work tree dir`), then runs the leading-directory walk on the git dir (`die`,
//! no errno) — all before the `Cloning into` banner. `SCLD_EXISTS` is a failure
//! there. zvcs made the destination with `create_dir_all`, which accepts the
//! empty path without creating anything, so `git clone src ""` went on to open
//! a repository at `''` and failed inside gitoxide at exit 1; a file in the way
//! surfaced as a bare `File exists (os error 17)`. A bare clone into `""` gets
//! past all of that and dies in `init_db()`'s `real_pathdup()`, below the banner.
//! Expectations captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-clone-work-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git(dir: &Path, args: &[&str]) -> (String, i32) {
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
    (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
}

#[test]
fn an_uncreatable_destination_dies_before_the_banner() {
    let root = scratch();
    git(&root, &["init", "-q", "-b", "main", "src"]);
    git(&root.join("src"), &["commit", "-q", "--allow-empty", "-m", "c1"]);
    std::fs::write(root.join("f"), "").unwrap();

    let cases: [(&[&str], &str); 5] = [
        (&["clone", "src", ""], "fatal: could not create work tree dir '': No such file or directory\n"),
        (
            &["clone", "--bare", "src", ""],
            "Cloning into bare repository ''...\nfatal: The empty string is not a valid path\n",
        ),
        (&["clone", "src", "f/x"], "fatal: could not create leading directories of 'f/x': Not a directory\n"),
        (&["clone", "src", "f/x/y"], "fatal: could not create leading directories of 'f/x/y': Not a directory\n"),
        (&["clone", "--bare", "src", "f/x"], "fatal: could not create leading directories of 'f/x'\n"),
    ];
    for (args, want) in cases {
        assert_eq!(git(&root, args), (want.to_string(), 128), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
