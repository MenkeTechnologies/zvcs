//! Applying a patch that creates a symlink under `core.symlinks=false`.
//!
//! The file system is declared unable to hold links, so the created entry is a regular file
//! whose content is the link target — with no trailing newline — as a checkout would make it.
//! zvcs called `symlink(2)` regardless, leaving a link where stock leaves a plain file.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("HOME", dir)
        .env("LC_ALL", "C");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

const PATCH: &str = "diff --git a/later-link b/later-link\nnew file mode 120000\n\
index 0000000..a1b2c3d\n--- /dev/null\n+++ b/later-link\n@@ -0,0 +1 @@\n+empty.txt\n\\ No newline at end of file\n";

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-applysym-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("empty.txt"), "").unwrap();
    std::fs::write(dir.join("link.patch"), PATCH).unwrap();
    run(stock, &dir, &[], &["init", "-q", "-b", "main"]);
    run(stock, &dir, &[], &["add", "empty.txt"]);
    assert_eq!(run(stock, &dir, &[], &["commit", "-qm", "one"]).2, Some(0));
    dir
}

/// File type and content of `later-link`, as the work tree holds it.
fn look(dir: &Path) -> String {
    let path = dir.join("later-link");
    match std::fs::symlink_metadata(&path) {
        Err(e) => format!("absent: {:?}", e.kind()),
        Ok(md) if md.file_type().is_symlink() => format!("symlink -> {:?}", std::fs::read_link(&path).unwrap()),
        Ok(_) => format!("file: {:?}", std::fs::read(&path).unwrap()),
    }
}

#[test]
fn the_created_link_is_a_plain_file_when_symlinks_are_off() {
    let Some(stock) = stock_git() else { return };
    let cases: [&[&str]; 4] = [
        &["-c", "core.symlinks=false", "apply", "link.patch"],
        &["apply", "link.patch"],
        &["-c", "core.symlinks=false", "apply", "--index", "link.patch"],
        &["-c", "core.symlinks=true", "apply", "link.patch"],
    ];
    for args in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            let result = run(bin, &dir, &[], args);
            let after = (look(&dir), run(stock, &dir, &[], &["ls-files", "--stage", "later-link"]).0);
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((result, after));
        }
        assert_eq!(seen[1], seen[0], "args {args:?}");
    }
}
