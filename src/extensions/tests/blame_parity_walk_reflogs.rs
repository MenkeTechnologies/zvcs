//! `git blame -g` / `--walk-reflogs`, compared with stock git.
//!
//! `-g` allocates `revs->reflog_info` (revision.c:2459-2460), and from then on
//! `add_pending_object_with_path()` hands every commit operand to
//! `add_reflog_for_walk()` instead of `revs->pending` (revision.c:306-319). So
//! `blame -g HEAD -- f` has no final commit and blames the working tree, an
//! annotated tag (not a commit object) stays pending, an `UNINTERESTING`
//! commit dies `cannot walk reflogs for <name>` (reflog-walk.c:172-173), and a
//! history-limiting option dies at the end of `setup_revisions()`
//! (revision.c:3218-3219). zvcs refused `-g` and ignored `--walk-reflogs`.
//!
//! Every case uses `-s`, so the working-tree line carries no clock time.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(bin: &str, dir: &Path, home: &Path, date: &str, args: &[&str]) -> (Vec<u8>, Vec<u8>, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("GIT_EDITOR", "true")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (out.stdout, out.stderr, out.status.code().expect("no signal"))
}

/// Three commits on `main`, `v1` a lightweight tag on the second and `v2` an
/// annotated one on it, and a fourth line left uncommitted; plus a bare clone.
fn fixture(stock: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-blame-walk-reflogs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let run = |dir: &Path, date: &str, args: &[&str]| {
        let (_, err, rc) = git(stock, dir, &root, date, args);
        assert_eq!(rc, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
    };
    run(&work, "@1700000000 +0000", &["init", "-q", "-b", "main", "."]);
    let mut text = String::new();
    for (n, line) in ["a", "b", "c"].iter().enumerate() {
        text.push_str(line);
        text.push('\n');
        std::fs::write(work.join("f"), &text).unwrap();
        let date = format!("@{} +0000", 1_700_000_000 + 100 * n);
        run(&work, &date, &["add", "f"]);
        run(&work, &date, &["-c", "maintenance.auto=false", "commit", "-q", "-m", line]);
        if n == 1 {
            run(&work, &date, &["tag", "v1"]);
            run(&work, &date, &["tag", "-a", "-m", "two", "v2"]);
        }
    }
    text.push_str("d\n");
    std::fs::write(work.join("f"), &text).unwrap();
    run(&root, "@1700000000 +0000", &["clone", "-q", "--bare", "work", "bare.git"]);
    Fixture(root)
}

#[test]
fn blame_walk_reflogs_matches_stock() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture(stock);
    let work = fx.0.join("work");
    let bare = fx.0.join("bare.git");
    let cases: &[(&Path, &[&str])] = &[
        // The commit operand goes to the reflog walk: the working tree is blamed.
        (&work, &["blame", "-s", "-g", "HEAD", "--", "f"]),
        (&work, &["blame", "-s", "--walk-reflogs", "HEAD~1", "f"]),
        (&work, &["blame", "-s", "HEAD~1", "-g", "f"]),
        (&work, &["blame", "-sg", "main@{1}", "f"]),
        // An annotated tag is a tag object, so it stays pending.
        (&work, &["blame", "-s", "-g", "v2", "f"]),
        (&work, &["blame", "-s", "-g", "--all", "f"]),
        // `--since` still bounds the dig from the working tree.
        (&work, &["blame", "-s", "-g", "--since=1700000150", "f"]),
        (&work, &["blame", "-s", "-g", "^v2", "f"]),
        // `add_reflog_for_walk()` refuses an `UNINTERESTING` commit.
        (&work, &["blame", "-g", "^v1", "f"]),
        (&work, &["blame", "-g", "v1..HEAD", "f"]),
        (&work, &["blame", "-g", "--not", "--all", "f"]),
        // `revs->limited` with a reflog walk.
        (&work, &["blame", "-g", "--reverse", "HEAD", "f"]),
        (&work, &["blame", "-g", "--topo-order", "f"]),
        (&work, &["blame", "-g", "HEAD", "--cherry-pick", "--", "f"]),
        // The `HEAD` a bare repository queues goes to the walk too, leaving no
        // final commit and no work tree to fake one from.
        (&bare, &["blame", "-g", "f"]),
        (&bare, &["blame", "-s", "-g", "v2", "f"]),
    ];
    let mut diverged = Vec::new();
    for (dir, args) in cases {
        let want = git(stock, dir, &fx.0, "@1700000000 +0000", args);
        let got = git(BIN, dir, &fx.0, "@1700000000 +0000", args);
        if want != got {
            diverged.push(format!(
                "{args:?}\n  stock rc={} out={:?} err={:?}\n  zvcs  rc={} out={:?} err={:?}",
                want.2,
                String::from_utf8_lossy(&want.0),
                String::from_utf8_lossy(&want.1),
                got.2,
                String::from_utf8_lossy(&got.0),
                String::from_utf8_lossy(&got.1),
            ));
        }
    }
    assert!(diverged.is_empty(), "{}", diverged.join("\n"));
}
