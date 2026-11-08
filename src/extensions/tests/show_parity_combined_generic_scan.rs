//! A merge's combined diff under `find_paths_generic()`, against stock git.
//!
//! `diff_tree_combined()` takes the per-parent scan whenever
//! `need_generic_pathscan` holds (combine-diff.c:1542-1548): rename detection
//! (`diff.renames` makes that the default for `show` and `log`), `-B`,
//! `--diff-filter` or a `-S`/`-G` pickaxe. `find_paths_generic()`
//! (combine-diff.c:1378-1420) then runs `diff_tree_oid(parent_i, result)` and
//! `diffcore_std()` against each parent and `intersect_paths()` keeps what every
//! filtered queue still names, so a rename shows the parent's blob and `R`, the
//! filter drops a path any parent's queue lost, and `-R` swaps every pair. zvcs
//! walked all trees at once whatever the options, so `--diff-filter` and renames
//! never reached a merge, and `diff-tree` took none of these options for its
//! combined path set. `diff-tree -R` swaps its queue before `diffcore_std()`
//! (diff.c:7605-7607, 7647-7652), which the two-tree `--stat` order shows too.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (Vec<u8>, Vec<u8>, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (out.stdout, out.stderr, out.status.code().unwrap_or(-1))
}

fn stdout_of(bin: &str, dir: &Path, args: &[&str]) -> String {
    let (out, err, code) = run(bin, dir, args);
    assert_eq!(code, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
    String::from_utf8(out).unwrap().trim_end().to_owned()
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).unwrap();
}

fn lines(range: std::ops::RangeInclusive<u32>, edit: &[(u32, &str)]) -> String {
    range
        .map(|n| edit.iter().find(|(at, _)| *at == n).map_or(n.to_string(), |(_, s)| s.to_string()) + "\n")
        .collect()
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// `base`, then `side` and `third` editing `keep`, `both` and `q3`, and `main`
/// renaming `ren` to `moved` (with an edit) and making `mode` executable. Two
/// merges are written with `commit-tree` over hand-resolved trees: `oct` (main,
/// side, third), which deletes `del`, adds `added` and edits `moved` again, and
/// `two` (main, side), which renames `ren` to `r2` instead, deletes `del` and
/// adds `added`.
fn repo(stock: &str, name: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("zvcs-combined-generic-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| stdout_of(stock, &dir, args);
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "maintenance.auto", "false"]);
    write(&dir, "keep", &lines(1..=20, &[]));
    write(&dir, "ren", &lines(1..=30, &[]));
    write(&dir, "del", "d\n");
    write(&dir, "mode", "m\n");
    write(&dir, "both", "x\n");
    write(&dir, "q3", "q\n");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["checkout", "-q", "-b", "side"]);
    write(&dir, "keep", &lines(1..=20, &[(5, "five")]));
    write(&dir, "both", "side\n");
    write(&dir, "q3", "sq\n");
    git(&["commit", "-q", "-a", "-m", "side"]);
    git(&["checkout", "-q", "-b", "third", "main"]);
    write(&dir, "keep", &lines(1..=20, &[(15, "fifteen")]));
    write(&dir, "both", "third\n");
    write(&dir, "q3", "t3\n");
    git(&["commit", "-q", "-a", "-m", "third"]);
    git(&["checkout", "-q", "main"]);
    git(&["mv", "ren", "moved"]);
    write(&dir, "moved", &lines(1..=30, &[(1, "one")]));
    write(&dir, "both", "mainb\n");
    write(&dir, "q3", "mq\n");
    make_executable(&dir.join("mode"));
    git(&["add", "-A", "."]);
    git(&["commit", "-q", "-m", "main"]);

    write(&dir, "keep", &lines(1..=20, &[(5, "five"), (10, "ten"), (15, "fifteen")]));
    write(&dir, "both", "merged\n");
    write(&dir, "q3", "m3\n");
    write(&dir, "moved", &lines(1..=30, &[(1, "one"), (30, "thirty")]));
    write(&dir, "added", "new\n");
    std::fs::remove_file(dir.join("del")).unwrap();
    git(&["add", "-A", "."]);
    let tree = git(&["write-tree"]);
    let oct = git(&["commit-tree", &tree, "-p", "main", "-p", "side", "-p", "third", "-m", "oct"]);
    git(&["tag", "oct", &oct]);

    git(&["checkout", "-q", "-f", "-B", "work", "main~1"]);
    write(&dir, "keep", &lines(1..=20, &[(5, "five"), (10, "ten")]));
    write(&dir, "both", "merged2\n");
    write(&dir, "q3", "s3\n");
    write(&dir, "added", "new\n");
    std::fs::remove_file(dir.join("del")).unwrap();
    git(&["mv", "ren", "r2"]);
    write(&dir, "r2", &(lines(1..=30, &[]) + "z\n"));
    make_executable(&dir.join("mode"));
    git(&["add", "-A", "."]);
    let tree = git(&["write-tree"]);
    let two = git(&["commit-tree", &tree, "-p", "main", "-p", "side", "-m", "two"]);
    git(&["tag", "two", &two]);
    git(&["checkout", "-q", "-f", "main"]);
    Repo(dir)
}

fn same(stock: &str, r: &Repo, cases: &[&[&str]]) {
    for args in cases {
        let ours = run(BIN, &r.0, args);
        let theirs = run(stock, &r.0, args);
        assert_eq!(
            (String::from_utf8_lossy(&ours.0), String::from_utf8_lossy(&ours.1), ours.2),
            (String::from_utf8_lossy(&theirs.0), String::from_utf8_lossy(&theirs.1), theirs.2),
            "{args:?}"
        );
    }
}

#[test]
fn renames_reach_the_combined_path_set() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "renames");
    same(
        stock,
        &r,
        &[
            &["show", "--cc", "--raw", "two"],
            &["show", "-c", "--name-status", "oct"],
            &["show", "--cc", "two"],
            &["show", "--cc", "--combined-all-paths", "--raw", "-p", "two"],
            &["show", "-c", "--combined-all-paths", "--name-status", "oct"],
            &["show", "--cc", "--raw", "-z", "two"],
            &["show", "--cc", "-B", "--raw", "oct"],
            &["show", "--cc", "-C", "-C", "--raw", "two"],
            &["log", "--cc", "--raw", "--all"],
            &["log", "-c", "-p", "--format=%s", "--all"],
            &["log", "--graph", "--cc", "--stat", "--all"],
            &["diff-tree", "--cc", "-M", "--raw", "two"],
            &["diff-tree", "-c", "-M", "--combined-all-paths", "--raw", "oct"],
            &["diff-tree", "--cc", "-C", "--find-copies-harder", "-p", "two"],
            &["diff-tree", "-c", "-B", "--name-status", "oct"],
            &["diff-tree", "-c", "--raw", "oct"],
        ],
    );
}

#[test]
fn diff_filter_narrows_every_parent() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "filter");
    same(
        stock,
        &r,
        &[
            &["show", "--cc", "--diff-filter=M", "oct"],
            &["show", "--cc", "--diff-filter=A", "two"],
            &["show", "-c", "--diff-filter=R", "--raw", "oct"],
            &["show", "-c", "--diff-filter=d", "--name-status", "two"],
            &["show", "--cc", "--diff-filter=AM", "--stat", "-p", "oct"],
            &["show", "--cc", "--diff-filter=a", "--name-only", "two"],
            &["show", "--cc", "--diff-filter=M", "two", "--", "keep", "both"],
            &["log", "--cc", "--diff-filter=M", "--raw", "--all"],
            &["log", "-c", "--diff-filter=A", "-p", "--format=%s", "--all"],
            &["log", "--cc", "-M", "--diff-filter=R", "--name-status", "--all"],
            &["diff-tree", "--cc", "--diff-filter=M", "oct"],
            &["diff-tree", "-c", "--diff-filter=A", "--raw", "two"],
            &["diff-tree", "-c", "--diff-filter=d", "--name-status", "-M", "oct"],
            &["diff-tree", "--cc", "--diff-filter=D", "-p", "--stat", "two"],
        ],
    );
}

#[test]
fn reverse_and_pickaxe_reach_diff_tree() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "reverse");
    same(
        stock,
        &r,
        &[
            &["diff-tree", "-M", "--cc", "-R", "two"],
            &["diff-tree", "-M", "-c", "-R", "--raw", "oct"],
            &["diff-tree", "-B", "--cc", "-R", "--name-status", "two"],
            &["diff-tree", "--diff-filter=M", "--cc", "-R", "oct"],
            &["diff-tree", "-Gten", "--cc", "-R", "two"],
            &["diff-tree", "-Sfive", "-c", "--raw", "oct"],
            &["diff-tree", "-Sfive", "--pickaxe-all", "-c", "--raw", "oct"],
            &["diff-tree", "-M", "-R", "--stat", "two^", "two"],
            &["diff-tree", "-r", "-M", "-R", "two^", "two"],
            &["diff-tree", "--stat", "--diff-filter=d", "-R", "oct^", "oct"],
            &["diff-tree", "--cc", "-R", "--stat", "-p", "--diff-filter=d", "oct"],
            &["show", "--cc", "-R", "--diff-filter=A", "--raw", "two"],
            &["log", "--cc", "-R", "-M", "--name-status", "--all"],
        ],
    );
}
