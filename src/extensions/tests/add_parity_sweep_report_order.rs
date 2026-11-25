//! What `git add` prints to stderr, and in which order, when one invocation
//! meets several things worth reporting, and how `-v`/`-n` report an unmerged
//! path; against stock git.
//!
//! `cmd_add()` runs `add_files_to_cache()` over the tracked paths first and
//! `add_files()` over the untracked `dir->entries` second (builtin/add.c:671-684).
//! The first dies `updating files failed` at the first tracked path it cannot
//! index (read-cache.c:4012-4014). The second opens with the gitignore block
//! (builtin/add.c:355-362), then visits the sorted entries one at a time: an
//! unindexable one is its two `error:` lines and, without `--ignore-errors`, an
//! immediate `adding files failed`; an embedded repository is
//! `check_embedded_repo()`'s warning (:371-377). A `core.autocrlf` warning comes
//! out of `add_file_to_index()` for the path it converts, so it falls between the
//! others in path order.
//!
//! zvcs printed the embedded-repository warning while walking, ahead of the
//! gitignore block and of any error before it in path order; printed the gitignore
//! block ahead of the tracked pass's errors; printed every error before the first
//! fatal; and printed the `core.autocrlf` warnings after all of them.
//!
//! An unmerged path reaches `update_callback()` once, and the `alias`
//! `add_to_index()` finds for it is a stage > 0 entry, so `-v` and `-n` always
//! name it (read-cache.c:807-819); zvcs named none. `fix_unmerged_status()`
//! (read-cache.c:3905-3924) makes an unmerged path whose file is gone a removal
//! of every stage, or under `--ignore-removal` an `add_file_to_index()` that dies
//! on the `lstat()`; zvcs left the stages in place and exited 0. `-N` replaces
//! the stages with the intent-to-add entry (read-cache.c:1277-1297), which zvcs
//! did not, and `--renormalize` leaves unmerged paths alone (builtin/add.c:87-88),
//! where zvcs resolved them.
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

/// Copy the pristine fixture to `run`, the same path for both sides, delete the
/// unmerged `both` when `gone`, run `args` there with `bin`, and return the
/// output with the index it left.
fn outcome(bin: &str, stock: &str, pristine: &Path, run_dir: &Path, gone: bool, args: &[&str]) -> String {
    let _ = std::fs::remove_dir_all(run_dir);
    let ok = Command::new("cp").arg("-Rp").arg(pristine).arg(run_dir).status().unwrap();
    assert!(ok.success());
    let repo = run_dir.join("repo");
    for p in ["t.txt", "u.txt"] {
        let _ = Command::new("chmod").arg("000").arg(repo.join(p)).status();
    }
    if gone {
        std::fs::remove_file(repo.join("both")).unwrap();
    }
    let got = run(bin, &repo, args);
    for p in ["t.txt", "u.txt"] {
        let _ = Command::new("chmod").arg("644").arg(repo.join(p)).status();
    }
    let index = run(stock, &repo, &["ls-files", "-s"]).0;
    format!("{got:?}\n{index}")
}

/// A tracked `t.txt` modified, an untracked `u.txt`, both made unreadable per
/// run; an ignored `z.log`; an LF file `crlf.txt` that `core.autocrlf=true`
/// warns about; an embedded repository `emb` and one without a commit, `bare`;
/// and the unmerged `both` from a conflicted merge.
fn fixture(stock: &str, root: &Path) {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| assert_eq!(run(stock, &repo, args).2, Some(0), "{args:?}");
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(repo.join("both"), "base\n").unwrap();
    std::fs::write(repo.join("t.txt"), "t\n").unwrap();
    git(&["add", "."]);
    git(&["-c", "maintenance.auto=false", "commit", "-qm", "base"]);
    git(&["checkout", "-qb", "side"]);
    std::fs::write(repo.join("both"), "side\n").unwrap();
    git(&["-c", "maintenance.auto=false", "commit", "-qam", "side"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(repo.join("both"), "main\n").unwrap();
    git(&["-c", "maintenance.auto=false", "commit", "-qam", "main"]);
    assert_eq!(run(stock, &repo, &["merge", "-q", "side"]).2, Some(1));
    std::fs::write(repo.join("t.txt"), "t2\n").unwrap();
    std::fs::write(repo.join("u.txt"), "u\n").unwrap();
    std::fs::write(repo.join("z.log"), "z\n").unwrap();
    std::fs::write(repo.join("crlf.txt"), "x\n").unwrap();
    git(&["init", "-q", "emb"]);
    git(&["-C", "emb", "commit", "-q", "--allow-empty", "-m", "e"]);
    git(&["init", "-q", "bare"]);
}

#[test]
fn stderr_follows_the_tracked_then_untracked_passes_and_unmerged_paths_are_reported() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-add-report-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let pristine = base.join("pristine");
    fixture(stock, &pristine);
    let run_dir = base.join("run");

    let cases: &[(bool, &[&str])] = &[
        // The gitignore block, then the embedded repository's warning.
        (false, &["add", "emb", "z.log"]),
        // `bare/` sorts first and is fatal before `emb/` is reached.
        (false, &["add", "bare", "emb", "crlf.txt"]),
        // The tracked pass dies before the gitignore block.
        (false, &["add", "t.txt", "z.log"]),
        // A tracked error, the gitignore block, then the untracked entries.
        (false, &["add", "--ignore-errors", "t.txt", "z.log", "u.txt", "emb"]),
        // core.autocrlf warnings interleave with the errors in path order.
        (false, &["-c", "core.autocrlf=true", "add", "crlf.txt", "u.txt"]),
        // The unmerged path in the -v and -n reports.
        (false, &["add", "-v", "both"]),
        (false, &["add", "-n", "-u"]),
        // An unmerged path whose file is gone is removed, every stage of it, or
        // under --ignore-removal is the lstat die; with the file there, -N puts the
        // intent-to-add entry in place of its stages and --renormalize leaves it.
        (true, &["add", "-v", "both"]),
        (true, &["add", "-n", "--ignore-removal", "crlf.txt", "both"]),
        (false, &["add", "-N", "-v", "both"]),
        (false, &["add", "-v", "--renormalize", "."]),
    ];
    for (gone, args) in cases {
        assert_eq!(
            outcome(BIN, stock, &pristine, &run_dir, *gone, args),
            outcome(stock, stock, &pristine, &run_dir, *gone, args),
            "{gone} {args:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}
