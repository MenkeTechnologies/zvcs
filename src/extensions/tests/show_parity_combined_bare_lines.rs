//! The two lines of a combined diff git writes with a bare `printf()`, and `-R`
//! on a merge's combined diff, against stock git.
//!
//! `show_combined_header()` prints the `mode <a>,<b>..<c>` continuation with
//! `printf("mode ")` (combine-diff.c:971) and `show_patch_diff()` prints
//! `printf("Binary files differ\n")` (combine-diff.c:1150), so neither takes
//! `diff_line_prefix()`: no `--line-prefix` and no `--graph` padding row. zvcs put
//! both in front of them under `show`, `log` and `diff-tree`.
//!
//! `-R` reaches the combined path set only through `find_paths_generic()`
//! (combine-diff.c:1378-1420), whose per-parent `diff_tree_oid()` queues pairs
//! `diff_change()` has swapped, so every parent holds the result's blob and the
//! result holds the first parent's (`index R,R..P1`). `log`/`show` take that scan
//! whenever rename detection is on (combine-diff.c:1542-1548), and
//! `show_combined_header()` reads `a/`/`b/` unswapped (combine-diff.c:931-932).
//! zvcs ignored `-R` there and swapped the prefixes.
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

fn write(dir: &Path, name: &str, body: &[u8]) {
    std::fs::write(dir.join(name), body).unwrap();
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Three branches off `base`, each editing the text file `t`, the binary `bin`
/// and `both`, plus files named `mode x` and `Binary files differ` so a name
/// list reads like the two bare lines. The merges are written with `commit-tree`
/// over a hand-resolved tree in which `both` and `mode x` turn executable, `gone`
/// is deleted and `new` holds content no parent had: `oct` (s1, s2, s3) and `two`
/// (s1, s2).
fn repo(stock: &str, name: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("zvcs-show-combined-bare-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| stdout_of(stock, &dir, args);
    git(&["init", "-q", "-b", "main"]);
    write(&dir, "t", b"a\nb\nc\n");
    write(&dir, "bin", b"x\0y\n");
    write(&dir, "both", b"q\n");
    write(&dir, "mode x", b"m\n");
    write(&dir, "Binary files differ", b"name\n");
    write(&dir, "gone", b"k\n");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "base"]);
    for (k, s) in ["s1", "s2", "s3"].iter().enumerate() {
        git(&["checkout", "-q", "-b", s, "main"]);
        let mut t = vec!["a", "b", "c"];
        let edit = format!("{}{s}", t[k]);
        t[k] = &edit;
        write(&dir, "t", (t.join("\n") + "\n").as_bytes());
        write(&dir, "bin", format!("x\0{s}\n").as_bytes());
        write(&dir, "both", format!("q {s}\n").as_bytes());
        write(&dir, "mode x", format!("m {s}\n").as_bytes());
        write(&dir, "Binary files differ", format!("name {s}\n").as_bytes());
        write(&dir, "gone", format!("k {s}\n").as_bytes());
        write(&dir, "new", format!("n {s}\n").as_bytes());
        git(&["add", "-A", "."]);
        git(&["commit", "-q", "-m", s]);
    }
    write(&dir, "t", b"as1\nbs2\ncs3\n");
    write(&dir, "bin", b"x\0merged\n");
    write(&dir, "both", b"merged\n");
    write(&dir, "mode x", b"merged\n");
    write(&dir, "Binary files differ", b"merged\n");
    write(&dir, "new", b"merged\n");
    std::fs::remove_file(dir.join("gone")).unwrap();
    make_executable(&dir.join("both"));
    make_executable(&dir.join("mode x"));
    git(&["add", "-A", "."]);
    let tree = git(&["write-tree"]);
    for (tag, parents) in [("oct", &["s1", "s2", "s3"][..]), ("two", &["s1", "s2"])] {
        let mut args = vec!["commit-tree", tree.as_str()];
        for p in parents {
            args.extend(["-p", p]);
        }
        args.extend(["-m", tag]);
        let id = git(&args);
        git(&["tag", tag, &id]);
    }
    git(&["reset", "-q", "--hard", "oct"]);
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
fn bare_mode_and_binary_lines_skip_the_line_prefix() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "prefix");
    same(
        stock,
        &r,
        &[
            &["show", "--cc", "--line-prefix=X", "oct"],
            &["show", "-c", "--line-prefix=> ", "two"],
            &["show", "--cc", "--color=always", "--line-prefix=X", "oct"],
            &["show", "--cc", "--stat", "--raw", "-p", "--line-prefix=X", "two"],
            &["show", "--cc", "--name-only", "--line-prefix=X", "oct"],
            &["show", "--cc", "--name-status", "--line-prefix=X", "two"],
            &["log", "--cc", "-p", "--line-prefix=X", "--all"],
            &["log", "-c", "-p", "--format=%s", "--line-prefix=X", "--all"],
            &["log", "--cc", "-p", "--no-commit-id", "--line-prefix=X", "-1", "oct"],
            &["whatchanged", "--cc", "--line-prefix=X", "--all"],
            &["diff-tree", "--cc", "--line-prefix=X", "two"],
            &["diff-tree", "-c", "-p", "--line-prefix=X", "oct"],
        ],
    );
}

#[test]
fn bare_mode_and_binary_lines_get_no_graph_row() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "graph");
    same(
        stock,
        &r,
        &[
            &["log", "--graph", "--cc", "-p", "--all"],
            &["log", "--graph", "--cc", "-p", "--line-prefix=X", "--all"],
            &["log", "--graph", "-c", "--stat", "-p", "--line-prefix=Z", "--all"],
            &["log", "--graph", "--cc", "--color=always", "--all"],
            &["log", "--graph", "-c", "-p", "--oneline", "--all"],
            &["show", "--graph", "--cc", "--line-prefix=X", "oct", "two"],
        ],
    );
}

#[test]
fn reverse_combined_diff_matches_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "reverse");
    same(
        stock,
        &r,
        &[
            &["show", "--cc", "-R", "two"],
            &["show", "-c", "-R", "oct"],
            &["show", "--cc", "-R", "--raw", "two"],
            &["show", "--cc", "-R", "--name-status", "oct"],
            &["show", "--cc", "-R", "--no-renames", "two"],
            &["show", "--cc", "-R", "--src-prefix=S/", "--dst-prefix=D/", "two"],
            &["show", "--cc", "-R", "-B", "--no-renames", "two"],
            &["log", "--cc", "-R", "-1", "two"],
            &["log", "-c", "-R", "--raw", "-1", "two"],
            &["log", "--graph", "--cc", "-p", "-R", "--all"],
            &["-c", "diff.renames=false", "show", "--cc", "-R", "oct"],
            &["-c", "diff.renames=false", "show", "--cc", "-R", "-M", "oct"],
            &["-c", "diff.renames=false", "log", "--cc", "-R", "-Gq s", "-1", "two"],
            &["-c", "diff.renames=false", "log", "--cc", "-R", "--name-status", "-1", "two"],
        ],
    );
}
