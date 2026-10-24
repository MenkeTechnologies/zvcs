//! `update-index` reaches an attribute lookup in two places, and both die on a
//! `GIT_ATTR_SOURCE` naming no tree-ish (attr.c `compute_default_attr_source`):
//!
//! - `add_one_path()` hashes a regular file through `index_fd()`, whose first step is
//!   `would_convert_to_git_filter_fd()` (object-file.c:952);
//! - every index write runs `ce_smudge_racily_clean_entry()` over the racily clean
//!   entries (read-cache.c:2916-2917), and `ce_compare_data()` hashes those through
//!   `index_fd()` too, so a command that changes nothing but the index version or a
//!   flag bit dies at the write.
//!
//! zvcs hashed and wrote without consulting the source. The fixture back-dates the
//! index so every entry is racily clean, independent of wall-clock timing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// A committed repository with an unstaged edit to `b.txt`, whose index is back-dated
/// so every entry is racily clean.
fn fixture(bin: &str, case: usize) -> PathBuf {
    let side = if bin == BIN { "zvcs" } else { "stock" };
    let dir = std::env::temp_dir().join(format!("zvcs-uiracyattr-{}-{side}-{case}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    run(bin, &dir, &[], &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("src/a.rs"), "a\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    run(bin, &dir, &[], &["add", "."]);
    run(bin, &dir, &[], &["commit", "-q", "-m", "one"]);
    std::fs::write(dir.join("b.txt"), "b\nedited\n").unwrap();
    let index = std::fs::File::options().write(true).open(dir.join(".git/index")).unwrap();
    index.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800)).unwrap();
    dir
}

#[test]
fn hashing_and_the_racy_write_die_on_a_bad_attr_source() {
    let Some(stock) = stock_git() else { return };
    let bad = [("GIT_ATTR_SOURCE", "does-not-exist")];
    let vectors: &[&[&str]] = &[
        &["update-index", "b.txt"],
        &["update-index", "--index-version=3"],
        &["update-index", "--assume-unchanged", "src/a.rs"],
        &["update-index", "--force-remove", "no-such-path"],
        // `--again` takes the rest of the command line as its pathspec, so the
        // `--index-version=1` after it is never range-checked.
        &["update-index", "--index-version=2", "--again", "--index-version=1"],
        &["update-index", "--again"],
    ];
    for (case, args) in vectors.iter().enumerate() {
        for env in [&bad[..], &[]] {
            let (s, z) = (fixture(stock, case), fixture(BIN, case));
            let index = |d: &Path| std::fs::read(d.join(".git/index")).unwrap();
            let before = index(&z);
            let want = run(stock, &s, env, args);
            assert_eq!(run(BIN, &z, env, args), want, "{args:?} env={env:?}");
            if want.2 == 128 {
                // The die leaves the index exactly as it was read, and no lock behind.
                assert_eq!(index(&z), before, "{args:?} rewrote the index");
                assert!(!z.join(".git/index.lock").exists(), "{args:?} left a lock");
            }
            let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
        }
    }
}
