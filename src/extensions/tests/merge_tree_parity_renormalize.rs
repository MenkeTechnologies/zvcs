//! `merge.renormalize` / `-Xrenormalize` in merge-ort, against stock git.
//!
//! Attributes come from the fake index `initialize_attr_index()` builds from the merge's
//! `.gitattributes` (merge-ort.c:2049-2108): a side that only *adds* the file is found when it is
//! the second side and not when it is the first, so one merge is clean in one order and
//! conflicted in the other, and a file found nowhere is read from the worktree instead.
//! `blob_unchanged()` (merge-ort.c:3748-3783) turns a modify/delete whose modification was only
//! a renormalization into a clean deletion (merge-ort.c:4387-4402).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        // Outside the repository: zvcs keeps state under $HOME.
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.code().unwrap_or(-1), text(&out.stdout), text(&out.stderr))
}

/// CRLF blobs at the base; `edit` changes a line of each, `del` deletes them, `norm` adds
/// `* text=auto` and renormalizes, `crlf` adds `* text eol=crlf`, renormalizes and changes `f`.
fn fixture(stock: &str, root: &Path) -> PathBuf {
    let repo = root.join("template");
    std::fs::create_dir_all(&repo).unwrap();
    let run = |args: &[&str]| {
        let (code, _, err) = git(stock, &repo, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    let write = |rel: &str, body: &str| std::fs::write(repo.join(rel), body).unwrap();
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "maintenance.auto", "false"]);
    run(&["config", "core.autocrlf", "false"]);
    write("f", "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n");
    write("d", "gone\r\nline\r\n");
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "base"]);
    for branch in ["edit", "del", "norm", "crlf"] {
        run(&["branch", branch]);
    }
    run(&["checkout", "-q", "edit"]);
    write("f", "ONE\r\ntwo\r\nthree\r\nfour\r\nfive\r\n");
    run(&["commit", "-q", "-am", "edit"]);
    run(&["checkout", "-q", "del"]);
    run(&["rm", "-q", "d", "f"]);
    run(&["commit", "-q", "-m", "del"]);
    for (branch, attrs) in [("norm", "* text=auto\n"), ("crlf", "* text eol=crlf\n")] {
        run(&["checkout", "-q", branch]);
        write(".gitattributes", attrs);
        run(&["add", ".gitattributes"]);
        run(&["add", "--renormalize", "."]);
        if branch == "crlf" {
            write("f", "one\r\ntwo\r\nthree\r\nfour\r\nFIVE\r\n");
            run(&["add", "f"]);
        }
        run(&["commit", "-q", "-m", branch]);
    }
    run(&["checkout", "-q", "main"]);
    repo
}

fn copy(from: &Path, to: &Path) {
    let ok = Command::new("cp").arg("-R").arg(from).arg(to).status().unwrap();
    assert!(ok.success());
}

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn root(label: &str) -> Root {
    let root = std::env::temp_dir().join(format!("zvcs-merge-renormalize-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    Root(std::fs::canonicalize(&root).unwrap())
}

#[test]
fn merge_tree_reads_attributes_from_the_merged_gitattributes() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = root("merge-tree");
    let repo = fixture(stock, &root.0);
    for args in [
        // `norm` adds the attributes as the second side: found, clean.
        &["-c", "merge.renormalize=true", "merge-tree", "edit", "norm"][..],
        // ... and as the first side: not found, conflicted.
        &["-c", "merge.renormalize=true", "merge-tree", "norm", "edit"],
        &["merge-tree", "-Xrenormalize", "norm", "edit"],
        &["merge-tree", "--messages", "-z", "-Xrenormalize", "edit", "norm"],
        &["-c", "merge.renormalize=true", "merge-tree", "-Xno-renormalize", "edit", "norm"],
        // A modify/delete whose modification is only the renormalization is a deletion.
        &["merge-tree", "-Xrenormalize", "del", "norm"],
        &["merge-tree", "-Xrenormalize", "norm", "del"],
        // Added by both: the first entry of the fake index is stage 1 and is never found.
        &["merge-tree", "-Xrenormalize", "norm", "crlf"],
        &["merge-tree", "-Xrenormalize", "edit", "crlf"],
        &["merge-tree", "--merge-base=main", "-Xrenormalize", "edit", "norm"],
    ] {
        assert_eq!(git(ZVCS, &repo, args), git(stock, &repo, args), "{args:?}");
    }
    let stdin = |bin: &str| {
        let mut child = Command::new(bin)
            .args(["merge-tree", "--stdin", "-Xrenormalize"])
            .current_dir(&repo)
            .env("HOME", &root.0)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"edit norm\nnorm edit\n").unwrap();
        child.wait_with_output().unwrap()
    };
    let (z, s) = (stdin(ZVCS), stdin(stock));
    assert_eq!((z.status.code(), z.stdout), (s.status.code(), s.stdout), "--stdin");

    // Attributes the fake index lacks are read from the worktree: with `norm` checked out,
    // `norm` as the first side is clean after all.
    let (code, _, err) = git(stock, &repo, &["checkout", "-q", "norm"]);
    assert_eq!(code, 0, "{err}");
    let args = &["merge-tree", "-Xrenormalize", "norm", "edit"];
    assert_eq!(git(ZVCS, &repo, args), git(stock, &repo, args), "{args:?}");
}

#[test]
fn merge_and_cherry_pick_renormalize() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = root("porcelain");
    let template = fixture(stock, &root.0);
    let cases = [
        ("edit", &["merge", "-q", "-Xrenormalize", "norm"][..]),
        ("edit", &["-c", "merge.renormalize=true", "merge", "-q", "norm"]),
        ("norm", &["cherry-pick", "-Xrenormalize", "edit"]),
        ("norm", &["-c", "merge.renormalize=true", "cherry-pick", "del"]),
    ];
    for (case, (checkout, args)) in cases.into_iter().enumerate() {
        let mut states = Vec::new();
        for (name, bin) in [("stock", stock), ("zvcs", ZVCS)] {
            let repo = root.0.join(format!("{name}-{case}"));
            let _ = std::fs::remove_dir_all(&repo);
            copy(&template, &repo);
            let (code, _, err) = git(stock, &repo, &["checkout", "-q", checkout]);
            assert_eq!(code, 0, "{err}");
            let run = git(bin, &repo, args);
            let state = ["rev-parse HEAD HEAD^{tree}", "status --porcelain", "ls-files -s"]
                .map(|cmd| git(stock, &repo, &cmd.split(' ').collect::<Vec<_>>()).1);
            states.push((run, state));
        }
        assert_eq!(states[1], states[0], "{checkout}: {args:?}");
    }
}
