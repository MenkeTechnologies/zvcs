//! `parse_dirstat_opt()` (diff.c:5433-5438) dies on a bad `--dirstat` parameter inside the
//! same left-to-right `diff_opt_parse()` pass where `diff_opt_color_moved()` (diff.c:5585-5603)
//! rejects a bad `--color-moved=` value with 129, so whichever comes first in argv decides the
//! error and the exit code. `diff-index` died on the dirstat parameter wherever it stood.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn fixture(bin: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("zvcs-dixdirstat-{}-{}", std::process::id(), if bin == BIN { "zvcs" } else { "stock" }));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.join("src/b.rs"), "b\n").unwrap();
    run(bin, &dir, &["add", "."]);
    run(bin, &dir, &["commit", "-q", "-m", "one"]);
    std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
    dir
}

#[test]
fn a_bad_dirstat_parameter_dies_at_its_argv_position() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock), fixture(BIN));
    let vectors: &[&[&str]] = &[
        &["diff-index", "--color-moved=\t", "--dirstat==", "HEAD"],
        &["diff-index", "--dirstat==", "--color-moved=\t", "HEAD"],
        &["diff-index", "--dirstat==", "nosuchrev"],
        &["diff-index", "nosuchrev", "--dirstat=="],
        &["diff-index", "-Xbogus", "--color-moved=nope", "HEAD"],
        &["diff-index", "--color-moved=nope", "-Xbogus", "HEAD"],
        &["diff-index", "--dirstat=files,10", "HEAD"],
    ];
    for args in vectors {
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
    }
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
