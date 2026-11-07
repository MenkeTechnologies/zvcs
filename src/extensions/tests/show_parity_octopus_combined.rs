//! Combined diffs of merges with more than two parents, and `-m` under `--graph`,
//! against stock git.
//!
//! `show_patch_diff()` runs its parent loop over every `num_parent`
//! (combine-diff.c:1184-1204): one `combine_diff()` per parent, or
//! `reuse_combine_diff()` (combine-diff.c:878-903) when an earlier parent held the
//! same blob, `make_hunks()` with `num_parent` mask bits, and `dump_sline()`'s
//! `num_parent + 1` `@` and one sign column per parent. zvcs died with "combined
//! diff of more than two parents is not supported" on an octopus merge.
//!
//! `log_tree_diff()`'s `-m` loop (log-tree.c:1146-1172) calls `show_log()` once per
//! parent with no `graph_update()` in between, so the records after the first get
//! `graph_show_commit()`'s padding row (graph.c:1864-1872). zvcs refused `-m` with
//! `--graph`.
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

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn lines(range: std::ops::RangeInclusive<u32>) -> String {
    range.map(|i| format!("{i}\n")).collect()
}

/// Four branches off `base`, each editing `n` (one in a line every other branch
/// also edits), appending to `c.c`, adding a file of its own and an `added` file
/// that differs per branch; `p2` deletes `del` and `p3` makes `n` executable. The
/// merges are written with `commit-tree` over a hand-resolved tree that also holds
/// an evil line no parent had: `oct` (p1..p4), `oct3` (p1, p3, p2) and `oct5`
/// (p1..p4 and `main`, so two parents share most blobs). `git merge` itself makes
/// `clean`, an octopus of q1, q2 and q3, which touch `n` in lines far apart and
/// add a file each, so it merges without conflicts.
fn repo(stock: &str, name: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("zvcs-show-octopus-combined-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| stdout_of(stock, &dir, args);
    git(&["init", "-q", "-b", "main"]);
    write(&dir, "n", &lines(1..=40));
    write(&dir, "del", "gone\n");
    write(&dir, "c.c", "func_a\n  body\n");
    write(&dir, "s", "shared\n");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "base"]);
    for (k, p) in ["p1", "p2", "p3", "p4"].iter().enumerate() {
        git(&["checkout", "-q", "-b", p, "main"]);
        let mut body: Vec<String> = lines(1..=40).lines().map(str::to_owned).collect();
        body[19] = format!("20{p}");
        body[4 + 10 * k] = format!("edit{p}");
        body.insert(9, format!("ins{p}"));
        write(&dir, "n", &(body.join("\n") + "\n"));
        write(&dir, "c.c", &format!("func_a\n  body\n{p}\n"));
        write(&dir, &format!("only_{p}"), &format!("{p}\n"));
        write(&dir, "added", &format!("add {p}\n"));
        if *p == "p2" {
            std::fs::remove_file(dir.join("del")).unwrap();
        }
        if *p != "p1" {
            write(&dir, "s", &format!("shared {p}\n"));
        }
        if *p == "p3" {
            make_executable(&dir.join("n"));
        }
        git(&["add", "-A", "."]);
        git(&["commit", "-q", "-m", p]);
    }
    for (k, q) in ["q1", "q2", "q3"].iter().enumerate() {
        git(&["checkout", "-q", "-b", q, "main"]);
        let mut body: Vec<String> = lines(1..=40).lines().map(str::to_owned).collect();
        body[2 + 15 * k] = format!("edit{q}");
        write(&dir, "n", &(body.join("\n") + "\n"));
        write(&dir, &format!("only_{q}"), &format!("{q}\n"));
        git(&["add", "-A", "."]);
        git(&["commit", "-q", "-m", q]);
    }
    git(&["checkout", "-q", "main"]);
    git(&["merge", "-q", "--no-edit", "q1", "q2", "q3"]);
    git(&["tag", "clean"]);
    git(&["reset", "-q", "--hard", "main~1"]);
    let mut body: Vec<String> = lines(1..=40).lines().map(str::to_owned).collect();
    body[19] = "twenty".to_owned();
    body.insert(30, "evil".to_owned());
    write(&dir, "n", &(body.join("\n") + "\n"));
    write(&dir, "c.c", "func_a\n  body\nresolved\n");
    write(&dir, "added", "merged\n");
    write(&dir, "s", "shared p2\n");
    std::fs::remove_file(dir.join("del")).unwrap();
    make_executable(&dir.join("n"));
    git(&["add", "-A", "."]);
    let tree = git(&["write-tree"]);
    for (tag, parents) in [
        ("oct", &["p1", "p2", "p3", "p4"][..]),
        ("oct3", &["p1", "p3", "p2"]),
        ("oct5", &["p1", "p2", "p3", "p4", "main"]),
    ] {
        let mut args = vec!["commit-tree", tree.as_str()];
        for p in parents {
            args.extend(["-p", p]);
        }
        args.extend(["-m", tag]);
        let id = git(&args);
        git(&["tag", tag, &id]);
    }
    git(&["reset", "-q", "--hard"]);
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
fn octopus_combined_patch_matches_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "patch");
    same(
        stock,
        &r,
        &[
            &["show", "oct"],
            &["show", "-c", "oct"],
            &["show", "oct3"],
            &["show", "-c", "oct5"],
            &["show", "--cc", "oct5"],
            &["show", "clean"],
            &["show", "-c", "clean"],
            &["show", "--cc", "-U0", "oct"],
            &["show", "--cc", "-U8", "oct3"],
            &["show", "--cc", "--combined-all-paths", "oct"],
            &["show", "--cc", "--full-index", "--no-prefix", "oct"],
            &["show", "--cc", "--color=always", "oct5"],
            &["show", "--cc", "oct", "--", "n", "s"],
            &["log", "--cc", "-p", "--format=%s", "oct5", "--", "c.c", "s"],
            &["log", "-c", "-p", "-1", "oct"],
            &["diff-tree", "-c", "-p", "oct"],
            &["diff-tree", "--cc", "oct5"],
            &["diff", "oct", "p1", "p2", "p3", "p4"],
            &["diff", "-c", "oct", "p4", "p3"],
            &["show", "p2...oct"],
        ],
    );
}

#[test]
fn octopus_combined_summaries_match_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "summary");
    same(
        stock,
        &r,
        &[
            &["show", "--cc", "--stat", "oct"],
            &["show", "-c", "--numstat", "oct5"],
            &["show", "-c", "--name-status", "oct"],
            &["show", "--cc", "--raw", "oct5"],
            &["show", "-c", "--raw", "--combined-all-paths", "oct"],
            &["diff-tree", "-c", "--stat", "-p", "oct5"],
            &["diff-tree", "-c", "-r", "--raw", "-z", "oct"],
            &["diff", "--cc", "--stat", "oct", "p1", "p2", "p3", "p4"],
            &["diff", "--name-status", "oct", "p1", "p2", "p3", "p4"],
            &["diff", "--raw", "oct", "p1", "p2", "p3", "p4"],
        ],
    );
}

#[test]
fn separate_merges_under_graph_match_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "graph");
    same(
        stock,
        &r,
        &[
            &["log", "--graph", "-m", "--stat", "oct"],
            &["log", "--graph", "-m", "-p", "oct3"],
            &["log", "--graph", "-m", "--oneline", "--name-status", "oct5"],
            &["log", "--graph", "-m", "--format=%h%n%s", "--raw", "--all"],
            &["log", "--graph", "-m", "--format=tformat:%h", "--name-only", "clean"],
            &["log", "--graph", "-m", "-s", "--all"],
            &["log", "--graph", "-m", "-p", "--first-parent", "oct"],
            &["log", "--graph", "-m", "-S", "evil", "--stat", "--all"],
            &["log", "--graph", "-m", "-p", "--line-prefix=> ", "-2", "oct"],
            &["log", "--graph", "-m", "--stat", "--color=always", "oct3"],
            &["show", "--graph", "-m", "main..oct"],
            &["show", "--graph", "-m", "--stat", "main..clean"],
        ],
    );
}
