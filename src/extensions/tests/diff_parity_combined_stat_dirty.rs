//! `git diff -c|--cc <rev>` over files whose stat data moved while their content
//! did not.
//!
//! `get_stat_data()` asks `match_stat_with_submodule()` with no `CE_MATCH_*`
//! option, so an entry whose stat no longer matches its file is changed and its
//! `oid` is the null id (diff-lib.c:315-347, v2.56.0). `show_modified()`'s
//! combined arm then fires for it even when the tree and the index agree
//! (diff-lib.c:408-424): `--raw` lists it as `MM` against the null result, the
//! patch prints its header with no hunk, and a racily clean entry whose stat still
//! matches is settled by `ce_modified_check_fs()` and stays out. The result is
//! read through `convert_to_git()` (combine-diff.c:1097-1107), whose round-trip
//! check warns, or under `core.safecrlf=true` dies after the sections already
//! printed. None of this refreshes the index, so a second run lists the same.
//!
//! The port dropped these paths: gix proves their content equal and keeps them
//! for an index write instead of yielding them.
#![cfg(unix)]

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
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// `a`, `d/c`, `crlf` (CRLF content), `x` (executable), `q` (assume-unchanged)
/// and `r` (skip-worktree) are committed and then back-dated on disk, so their
/// stat no longer matches the index; `b` is staged as `b2` and edited to `b3`;
/// `s` is staged as `s2` and back-dated. `fresh` keeps its stat.
fn fixture(stock: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-cc-stat-dirty-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("d")).unwrap();
    let git = |args: &[&str]| run(stock, &dir, args);
    git(&["init", "-q", "-b", "main", "."]);
    git(&["config", "core.autocrlf", "false"]);
    for (name, body) in [
        ("a", "a\n"),
        ("b", "b\n"),
        ("d/c", "c\n"),
        ("crlf", "x\r\ny\r\n"),
        ("x", "x\n"),
        ("q", "q\n"),
        ("r", "r\n"),
        ("s", "s\n"),
        ("fresh", "fresh\n"),
    ] {
        std::fs::write(dir.join(name), body).unwrap();
    }
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("x"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "one"]);
    std::fs::write(dir.join("b"), "b2\n").unwrap();
    std::fs::write(dir.join("s"), "s2\n").unwrap();
    git(&["add", "b", "s"]);
    std::fs::write(dir.join("b"), "b3\n").unwrap();
    git(&["update-index", "--assume-unchanged", "q"]);
    git(&["update-index", "--skip-worktree", "r"]);
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800);
    for name in ["a", "d/c", "crlf", "x", "q", "r", "s"] {
        let f = std::fs::File::options().write(true).open(dir.join(name)).unwrap();
        f.set_modified(old).unwrap();
    }
    // `b3` is written in the second `b2` was staged in more often than not, and
    // whether a stock write of the index then lands in that second decides what
    // stock reports: racily clean, `ce_modified_check_fs()` opens it and `diff HEAD`
    // hashes the file; otherwise the stat matches and the index id stands
    // (read-cache.c:353-375, :434-438). An mtime a second away from the entry's
    // takes the timing out: the stat no longer matches, in every fixture.
    let f = std::fs::File::options().write(true).open(dir.join("b")).unwrap();
    f.set_modified(old + std::time::Duration::from_secs(1)).unwrap();
    dir
}

#[test]
fn stat_dirty_entries_combine_against_one_revision() {
    let Some(stock) = stock_git() else { return };
    let vectors: &[&[&str]] = &[
        &["diff", "HEAD", "-c"],
        &["diff", "HEAD", "--cc"],
        &["diff", "HEAD", "-c", "--raw"],
        &["diff", "HEAD", "--cc", "--raw", "-z"],
        &["diff", "HEAD", "-c", "--name-status"],
        &["diff", "HEAD", "-c", "--name-only"],
        &["diff", "HEAD", "-c", "--stat"],
        &["diff", "HEAD", "-c", "--exit-code"],
        &["diff", "HEAD", "--cc", "--quiet"],
        &["diff", "HEAD", "-c", "--", "a", "d"],
        &["diff", "HEAD", "-c", "--relative=d"],
        &["diff", "HEAD", "-c", "-R"],
        &["diff", "HEAD", "--cc", "--combined-all-paths"],
        &["diff", "HEAD", "--cc", "--color=always"],
        &["-c", "core.autocrlf=true", "diff", "HEAD", "-c"],
        &["-c", "core.autocrlf=true", "diff", "HEAD", "-c", "--raw"],
        &["-c", "core.safecrlf=true", "diff", "HEAD", "--cc"],
        &["-c", "core.autocrlf=true", "-c", "core.safecrlf=true", "diff", "HEAD", "--cc"],
        &["-c", "core.trustctime=false", "diff", "HEAD", "-c", "--raw"],
        &["-c", "core.checkstat=minimal", "diff", "HEAD", "-c", "--raw"],
        &["diff", "HEAD"],
        &["diff", "-c"],
        &["diff-files", "-c"],
        &["diff-files", "--cc"],
    ];
    for (n, args) in vectors.iter().enumerate() {
        let (s, z) = (fixture(stock, &format!("s{n}")), fixture(stock, &format!("z{n}")));
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
        // The run leaves the index as stock does: a second one lists the same.
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?} (second run)");
        let porcelain = ["status", "--porcelain"];
        assert_eq!(run(stock, &z, &porcelain), run(stock, &s, &porcelain), "{args:?} (status)");
        let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
    }

    // `--output=<file>` does not reach `show_combined_diff()`, which prints with
    // `printf()`: the combined sections stay on stdout and only the queue's patch
    // lands in the file.
    let (s, z) = (fixture(stock, "so"), fixture(stock, "zo"));
    let args = ["diff", "HEAD", "--cc", "--output=out.patch"];
    assert_eq!(run(BIN, &z, &args), run(stock, &s, &args), "{args:?}");
    assert_eq!(
        std::fs::read(z.join("out.patch")).unwrap(),
        std::fs::read(s.join("out.patch")).unwrap(),
        "{args:?} (file)"
    );
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
