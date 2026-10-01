//! A path deleted from a split index and staged again survives the next write.
//!
//! `merge_base_index()` marks every base entry the delete bitmap names
//! `CE_REMOVE` (`mark_entry_for_delete()`, split-index.c:126-134) and drops it
//! from `istate->cache[]`. An entry staged at that path afterwards is a new
//! `cache_entry` with `ce->index == 0`, so `prepare_to_write_split_index()`
//! (split-index.c:235-393) appends it whole to the split half and keeps the old
//! base entry's delete bit.
//!
//! zvcs matched entries to the base by path alone, so the re-staged entry was
//! paired with the deleted base entry: its content equalled that entry's, so it
//! was neither written as a replacement nor appended, and the delete bit still
//! removed it. `mv`, `rm` and `commit` over a split index, then `checkout
//! HEAD~1 -- <path>`, left the restored file untracked.
//!
//! Expectations measured from stock git 2.56.0; stock also reads the index
//! zvcs wrote.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> String {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap_or_else(|e| panic!("{bin} {args:?}: {e}"));
    assert!(out.status.success(), "{bin} {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Two tracked files under a split index, the shared half holding both.
fn split_fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir()
        .join(format!("zvcs-mv-split-index-readded-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a"), "a\n").unwrap();
    std::fs::write(root.join("b"), "b\n").unwrap();
    run(BIN, &root, &["init", "-q", "-b", "main", "."]);
    run(BIN, &root, &["add", "a", "b"]);
    run(BIN, &root, &["commit", "-q", "-m", "seed"]);
    run(BIN, &root, &["update-index", "--split-index"]);
    Fixture { root }
}

const BOTH_STAGED: &str = "\
100644 78981922613b2afb6025042ff6bd878ac1994e85 0\ta
100644 61780798228d17af2d34fce4cfbdf35556832472 0\tb
";

#[test]
fn rm_cached_then_checkout_restages_the_path() {
    let f = split_fixture("rm-cached");
    run(BIN, &f.root, &["rm", "-q", "--cached", "b"]);
    run(BIN, &f.root, &["checkout", "HEAD", "--", "b"]);
    assert_eq!(run(BIN, &f.root, &["status", "--porcelain"]), "");
    assert_eq!(run(BIN, &f.root, &["ls-files", "-s"]), BOTH_STAGED);
    if let Some(stock) = stock_git() {
        assert_eq!(run(stock, &f.root, &["ls-files", "-s"]), BOTH_STAGED);
    }
}

#[test]
fn moved_and_removed_then_checked_out_from_the_parent() {
    let f = split_fixture("mv-rm");
    run(BIN, &f.root, &["mv", "a", "moved"]);
    run(BIN, &f.root, &["rm", "-q", "-f", "b"]);
    run(BIN, &f.root, &["commit", "-q", "-m", "moved and removed"]);
    run(BIN, &f.root, &["checkout", "HEAD~1", "--", "b"]);
    let want = "\
100644 61780798228d17af2d34fce4cfbdf35556832472 0\tb
100644 78981922613b2afb6025042ff6bd878ac1994e85 0\tmoved
";
    assert_eq!(run(BIN, &f.root, &["status", "--porcelain"]), "A  b\n");
    assert_eq!(run(BIN, &f.root, &["ls-files", "-s"]), want);
    if let Some(stock) = stock_git() {
        assert_eq!(run(stock, &f.root, &["ls-files", "-s"]), want);
    }
}
