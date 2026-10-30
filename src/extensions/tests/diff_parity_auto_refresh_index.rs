//! `git diff`'s `diffcore_skip_stat_unmatch()` pass and the index refresh it buys.
//!
//! `cmd_diff()` arms `skip_stat_unmatch` from `diff.autoRefreshIndex`
//! (builtin/diff.c:525, v2.56.0). `diffcore_std()` then drops every queued pair
//! whose worktree side is dirty only in its stat data, counting them
//! (diff.c:7376-7399), and when it dropped any, `cmd_diff()` ends with
//! `refresh_index_quietly()` (builtin/diff.c:237-250, :648-650): the stat data is
//! written back under a lock taken without `LOCK_DIE_ON_ERROR`, so a held
//! `index.lock` costs the refresh and nothing else. With the key off the pairs
//! stay queued, and `--raw` / `--name-only` list them.
//!
//! The same pass reads each worktree side through `convert_to_git()` with
//! `core.safecrlf=true` demoted to a warning (diff.c:4483-4489), printed once per
//! file however many formats read it, where the port used to die.
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
        String::from_utf8_lossy(&out.stderr).replace("zvcs:", "git:"),
        out.status.code().expect("no signal"),
    )
}

/// 2000-01-01: what a stat-only change sets a file's mtime to.
const TOUCHED: u64 = 946_684_800;
/// 2001-01-01: what a real change sets a file's mtime to, so a refresh that
/// wrongly took its stat would show.
const CHANGED: u64 = 978_307_200;

fn set_mtime(path: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)).unwrap();
}

/// `a` and `d/c` are back-dated with their content intact; `b` is edited; `s` is
/// staged as `s2` and then put back to its committed content, so it differs from
/// the index but not from `HEAD`. `extra` runs (with stock git) before the files
/// are back-dated.
fn fixture(stock: &str, tag: &str, extra: &[&[&str]]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-diff-refresh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("d")).unwrap();
    let git = |args: &[&str]| run(stock, &dir, args);
    git(&["init", "-q", "-b", "main", "."]);
    for (name, body) in [("a", "a\n"), ("b", "b\n"), ("d/c", "c\n"), ("s", "s\n")] {
        std::fs::write(dir.join(name), body).unwrap();
    }
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "one"]);
    std::fs::write(dir.join("s"), "s2\n").unwrap();
    git(&["add", "s"]);
    std::fs::write(dir.join("s"), "s\n").unwrap();
    std::fs::write(dir.join("b"), "b2\n").unwrap();
    std::fs::write(dir.join("untracked"), "u\n").unwrap();
    for args in extra {
        git(args);
    }
    for name in ["a", "d/c"] {
        set_mtime(&dir.join(name), TOUCHED);
    }
    for name in ["b", "s"] {
        set_mtime(&dir.join(name), CHANGED);
    }
    dir
}

/// Per index entry, the mtime second and size `ls-files --debug` records, which is
/// what a refresh rewrites; the inode and ctime differ between two fixtures.
fn index_stat(stock: &str, dir: &Path) -> String {
    let (out, _, _) = run(stock, dir, &["ls-files", "--debug"]);
    let mut summary = String::new();
    for line in out.lines() {
        if !line.starts_with(' ') {
            summary.push_str(line);
        } else if let Some(m) = line.strip_prefix("  mtime: ") {
            // Only the two back-dated times are fixed; anything else is when the
            // fixture was made.
            let secs = m.split(':').next().unwrap();
            let secs = match secs.parse::<u64>() {
                Ok(TOUCHED) => "touched",
                Ok(CHANGED) => "changed",
                Ok(0) => "zero",
                _ => "recent",
            };
            summary.push_str(&format!(" mtime={secs}"));
        } else if let Some(s) = line.strip_prefix("  size: ") {
            summary.push_str(&format!(" size={}\n", s.split('\t').next().unwrap()));
        }
    }
    summary
}

#[test]
fn stat_only_pairs_are_dropped_and_the_index_refreshed() {
    let Some(stock) = stock_git() else { return };
    let none: &[&[&str]] = &[];
    let vectors: &[(&[&str], &[&[&str]])] = &[
        (&["diff"], none),
        (&["diff", "HEAD"], none),
        (&["diff", "-c"], none),
        (&["diff", "-c", "HEAD"], none),
        (&["diff", "--raw"], none),
        (&["diff", "HEAD", "--raw"], none),
        (&["diff", "--stat"], none),
        (&["diff", "--name-only"], none),
        (&["diff", "-R", "HEAD"], none),
        (&["diff", "--", "a"], none),
        (&["diff", "--", "b"], none),
        (&["diff", "HEAD", "--", "s"], none),
        (&["diff", "--cached"], none),
        (&["diff", "--quiet"], none),
        (&["diff", "--quiet", "--no-ext-diff", "--", "a", "b"], none),
        (&["diff", "--quiet", "--no-ext-diff", "--", "b", "d"], none),
        (&["diff", "HEAD", "--quiet", "--no-ext-diff", "--", "b", "d"], none),
        (&["diff", "--exit-code", "--no-ext-diff", "--", "b", "d"], none),
        (&["-C", "d", "diff", "--relative"], none),
        (&["-c", "diff.autoRefreshIndex=false", "diff"], none),
        (&["-c", "diff.autoRefreshIndex=false", "diff", "--raw"], none),
        (&["-c", "diff.autoRefreshIndex=false", "diff", "HEAD", "--name-status"], none),
        (&["-c", "diff.autoRefreshIndex=false", "diff", "--stat"], none),
        (&["-c", "diff.autoRefreshIndex=false", "diff", "--numstat"], none),
        (&["-c", "diff.autoRefreshIndex=false", "diff", "--exit-code"], none),
        (&["diff"], &[&["update-index", "--split-index"]]),
        (&["-c", "core.splitIndex=false", "diff"], &[&["update-index", "--split-index"]]),
        (&["-c", "core.untrackedCache=true", "diff"], none),
        (&["diff"], &[&["update-index", "--index-version", "4"]]),
    ];
    for (n, (args, extra)) in vectors.iter().enumerate() {
        let (s, z) = (fixture(stock, &format!("s{n}"), extra), fixture(stock, &format!("z{n}"), extra));
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
        assert_eq!(index_stat(stock, &z), index_stat(stock, &s), "{args:?} (index stat)");
        let shared = |dir: &Path| {
            std::fs::read_dir(dir.join(".git"))
                .unwrap()
                .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("sharedindex."))
                .count()
        };
        assert_eq!(shared(&z), shared(&s), "{args:?} (shared indexes)");
        for check in [&["status", "--porcelain"][..], &["fsck", "--no-progress"]] {
            assert_eq!(run(stock, &z, check), run(stock, &s, check), "{args:?} {check:?}");
        }
        let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
    }

    // Another process holds `index.lock`: the diff is unaffected, the index is
    // left alone, and the lock is not the diff's to remove.
    for args in [&["diff"][..], &["diff", "HEAD", "--raw"]] {
        let (s, z) = (fixture(stock, "sl", none), fixture(stock, "zl", none));
        for dir in [&s, &z] {
            std::fs::write(dir.join(".git/index.lock"), "").unwrap();
        }
        let before = std::fs::read(z.join(".git/index")).unwrap();
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?} (locked)");
        assert_eq!(std::fs::read(z.join(".git/index")).unwrap(), before, "{args:?} (locked index)");
        assert!(z.join(".git/index.lock").exists(), "{args:?} (lock kept)");
        assert_eq!(index_stat(stock, &z), index_stat(stock, &s), "{args:?} (locked index stat)");
        let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
    }
}

/// `core.autocrlf=true` over LF files: `f` is edited, `t` back-dated with its
/// content intact, `x` made executable, and `n` added with `add -N`.
fn crlf_fixture(stock: &str, tag: &str, safecrlf: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-diff-crlf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| run(stock, &dir, args);
    git(&["init", "-q", "-b", "main", "."]);
    for (name, body) in [("f", "a\nb\n"), ("t", "t\n"), ("x", "x\n")] {
        std::fs::write(dir.join(name), body).unwrap();
    }
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "one"]);
    git(&["config", "core.autocrlf", "true"]);
    git(&["config", "core.safecrlf", safecrlf]);
    std::fs::write(dir.join("f"), "a\nb\nc\n").unwrap();
    std::fs::write(dir.join("n"), "x\ny\n").unwrap();
    git(&["add", "-N", "n"]);
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("x"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    set_mtime(&dir.join("t"), TOUCHED);
    dir
}

#[test]
fn safecrlf_warns_once_per_worktree_read_and_never_dies() {
    let Some(stock) = stock_git() else { return };
    let vectors: &[&[&str]] = &[
        &["diff"],
        &["diff", "HEAD"],
        &["diff", "HEAD", "--", "n"],
        &["diff", "--raw"],
        &["diff", "HEAD", "--raw"],
        &["diff", "--stat"],
        &["diff", "--numstat"],
        &["diff", "--name-only"],
        &["diff", "HEAD", "--name-status"],
        &["diff", "--exit-code", "--raw"],
        &["diff", "--quiet", "--no-ext-diff"],
        &["diff", "-R", "HEAD"],
        &["diff", "-M"],
        &["diff", "--", "t"],
    ];
    for safecrlf in ["warn", "true"] {
        for (n, args) in vectors.iter().enumerate() {
            let s = crlf_fixture(stock, &format!("s{safecrlf}{n}"), safecrlf);
            let z = crlf_fixture(stock, &format!("z{safecrlf}{n}"), safecrlf);
            assert_eq!(run(BIN, &z, args), run(stock, &s, args), "safecrlf={safecrlf} {args:?}");
            assert_eq!(index_stat(stock, &z), index_stat(stock, &s), "safecrlf={safecrlf} {args:?} (index)");
            let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
        }
    }
}
