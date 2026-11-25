//! Where `git add` dies on a `core.bigFileThreshold` it cannot parse, against
//! stock git.
//!
//! The key is read the first time `index_fd()` hashes an opened regular file
//! (object-file.c:955-957, repo-settings.c:167-172), so the refusal is a `die()`
//! at that path's place in `cmd_add()`'s two passes, `add_files_to_cache()` over
//! the tracked paths and then `add_files()` over the sorted untracked entries
//! (builtin/add.c:671-684). Whatever those passes printed before it is already
//! out: the gitignore block (builtin/add.c:355-362), an embedded repository's
//! warning that sorts ahead of the file (:371-377), an earlier path's errors, and
//! the `-v`/`-n` lines of the paths before it (read-cache.c:818-819). It comes
//! after the pathspec checks (builtin/add.c:568), and a symlink never asks.
//! `renormalize_tracked_files()` reaches it the same way.
//!
//! zvcs died while walking, ahead of the pathspec check and of everything the
//! passes print.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// Copy the pristine fixture to `run_dir`, the same path for both sides, run
/// `args` there with `bin`, and return the output with the index and the loose
/// objects it left.
fn outcome(bin: &str, stock: &str, pristine: &Path, run_dir: &Path, args: &[&str]) -> String {
    let _ = std::fs::remove_dir_all(run_dir);
    let ok = Command::new("cp").arg("-Rp").arg(pristine).arg(run_dir).status().unwrap();
    assert!(ok.success());
    let repo = run_dir.join("repo");
    let _ = Command::new("chmod").arg("000").arg(repo.join("b-unread")).status();
    let got = run(bin, &repo, args);
    let _ = Command::new("chmod").arg("644").arg(repo.join("b-unread")).status();
    let index = run(stock, &repo, &["ls-files", "-s"]).0;
    let objects = run(stock, &repo, &["count-objects"]).0;
    format!("{got:?}\n{index}{objects}")
}

/// A tracked `tracked` modified; an untracked embedded repository `a-emb`, a
/// symlink `a-link`, a file `b-unread` made unreadable per run, a file `z-file`
/// and an ignored `ign.o`.
fn fixture(stock: &str, root: &Path) {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| assert_eq!(run(stock, &repo, args).2, Some(0), "{args:?}");
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("tracked"), "t\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "*.o\n").unwrap();
    git(&["add", "tracked", ".gitignore"]);
    git(&["-c", "maintenance.auto=false", "commit", "-qm", "base"]);
    git(&["init", "-q", "a-emb"]);
    git(&["-C", "a-emb", "-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "e"]);
    std::os::unix::fs::symlink("tracked", repo.join("a-link")).unwrap();
    std::fs::write(repo.join("b-unread"), "u\n").unwrap();
    std::fs::write(repo.join("z-file"), "z\n").unwrap();
    std::fs::write(repo.join("ign.o"), "o\n").unwrap();
    std::fs::write(repo.join("tracked"), "t\nchanged\n").unwrap();
}

#[test]
fn a_bad_threshold_dies_at_the_first_regular_file_the_passes_hash() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-add-big-file-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let pristine = base.join("pristine");
    fixture(stock, &pristine);
    let run_dir = base.join("run");

    const BAD: [&str; 2] = ["-c", "core.bigFileThreshold=warn"];
    let cases: &[&[&str]] = &[
        // The embedded repository and the symlink sort ahead of `z-file`.
        &["add", "-v", "a-emb", "a-link", "z-file"],
        // The gitignore block, an unreadable path's errors, the warning, the die.
        &["add", "--ignore-errors", "a-emb", "b-unread", "z-file", "ign.o"],
        // A pathspec that matches nothing dies first.
        &["add", "z-file", "nope"],
        // `renormalize_tracked_files()`.
        &["add", "-v", "--renormalize", "."],
    ];
    for args in cases {
        let args: Vec<&str> = BAD.iter().chain(args.iter()).copied().collect();
        assert_eq!(
            outcome(BIN, stock, &pristine, &run_dir, &args),
            outcome(stock, stock, &pristine, &run_dir, &args),
            "{args:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}
