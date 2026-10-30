//! `diff-files -c|--cc` reads each conflicted path's result the way `show_patch_diff()`'s
//! worktree branch does (combine-diff.c:1041-1117): a symlink is its target through
//! `readlink()`, never the file it points at, and a regular file goes through
//! `convert_to_git()` — `core.autocrlf`, `eol`/`text` attributes and a clean filter all
//! apply, its round-trip warning is printed, and `core.safecrlf=true` dies after the
//! sections ahead of the path. zvcs read the file with a plain `read()`, so CRLF results
//! diffed as every line changed, a clean filter never ran, and a symlink whose target
//! did not exist read as a deleted result.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git(stock: &str, dir: &Path, args: &[&str]) {
    let (rc, _, err) = run(stock, dir, args);
    assert_eq!(rc, 0, "{args:?}: {err}");
}

fn fresh(name: &str, side: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-difffiles-cwr-{name}-{side}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    git(stock_git().unwrap(), &root, &["init", "-q", "-b", "main"]);
    root
}

/// `a` and `f` each conflicted between `main` and `side`, the merge left in place.
fn text_conflict(stock: &str, name: &str, side: &str) -> PathBuf {
    let root = fresh(name, side);
    let write = |content: &str| {
        for p in ["a", "f"] {
            std::fs::write(root.join(p), content).unwrap();
        }
    };
    write("a\nb\nc\n");
    git(stock, &root, &["add", "a", "f"]);
    git(stock, &root, &["commit", "-q", "-m", "base"]);
    git(stock, &root, &["checkout", "-q", "-b", "side"]);
    write("a\nB\nc\n");
    git(stock, &root, &["commit", "-q", "-a", "-m", "side"]);
    git(stock, &root, &["checkout", "-q", "main"]);
    write("a\nM\nc\n");
    git(stock, &root, &["commit", "-q", "-a", "-m", "main"]);
    assert_eq!(run(stock, &root, &["merge", "-q", "side"]).0, 1);
    root
}

#[cfg(unix)]
fn link_conflict(stock: &str, name: &str, side: &str) -> PathBuf {
    use std::os::unix::fs::symlink;
    let root = fresh(name, side);
    let relink = |target: &str| {
        let _ = std::fs::remove_file(root.join("l"));
        symlink(target, root.join("l")).unwrap();
    };
    std::fs::write(root.join("tgt"), "x\n").unwrap();
    relink("base");
    git(stock, &root, &["add", "l", "tgt"]);
    git(stock, &root, &["commit", "-q", "-m", "base"]);
    git(stock, &root, &["checkout", "-q", "-b", "side"]);
    relink("side");
    git(stock, &root, &["commit", "-q", "-a", "-m", "side"]);
    git(stock, &root, &["checkout", "-q", "main"]);
    relink("main");
    git(stock, &root, &["commit", "-q", "-a", "-m", "main"]);
    assert_eq!(run(stock, &root, &["merge", "-q", "side"]).0, 1);
    root
}

const FORMATS: &[&[&str]] = &[&["-p"], &["-c"], &["--cc"], &["-c", "-U1"], &["--cc", "--line-prefix=> "]];

fn compare(stock: &str, name: &str, setup: &dyn Fn(&str, &str) -> PathBuf, cfg: &[&str]) {
    compare_runs(stock, name, setup, cfg, &["diff-files"], FORMATS);
}

/// Every `formats` entry after `cfg` and `verb`, on twin fixtures `setup` builds.
fn compare_runs(
    stock: &str,
    name: &str,
    setup: &dyn Fn(&str, &str) -> PathBuf,
    cfg: &[&str],
    verb: &[&str],
    formats: &[&[&str]],
) {
    let stock_dir = setup(stock, "stock");
    let zvcs_dir = setup(stock, "zvcs");
    for format in formats {
        let mut args: Vec<&str> = cfg.to_vec();
        args.extend_from_slice(verb);
        args.extend_from_slice(format);
        let want = run(stock, &stock_dir, &args);
        let got = run(ZVCS, &zvcs_dir, &args);
        assert_eq!(got, want, "{name}: git {args:?}: left is zvcs, right is stock");
    }
    let _ = std::fs::remove_dir_all(&stock_dir);
    let _ = std::fs::remove_dir_all(&zvcs_dir);
}

fn crlf_results(root: &Path) {
    for p in ["a", "f"] {
        std::fs::write(root.join(p), "a\r\n<<<\r\nM\r\n===\r\nB\r\n>>>\r\nc\r\n").unwrap();
    }
}

#[test]
fn autocrlf_converts_the_worktree_result() {
    let Some(stock) = stock_git() else { return };
    let setup = |s: &str, side: &str| {
        let root = text_conflict(s, "autocrlf", side);
        crlf_results(&root);
        root
    };
    compare(stock, "autocrlf", &setup, &["-c", "core.autocrlf=true"]);
    // `autocrlf=input` round-trip warning, printed as each path is read.
    compare(stock, "autocrlf-input", &setup, &["-c", "core.autocrlf=input"]);
    // `core.safecrlf=true` dies on `a`; nothing after it is printed.
    compare(stock, "safecrlf", &setup, &["-c", "core.autocrlf=input", "-c", "core.safecrlf=true"]);
}

#[test]
fn eol_and_text_attributes_convert_the_worktree_result() {
    let Some(stock) = stock_git() else { return };
    for (name, attr, body) in [
        ("eol", "* eol=crlf\n", "a\r\nM\r\nB\r\nc\r\n"),
        ("text", "* text\n", "a\r\nM\r\nB\r\nc\n"),
        ("binary-attr", "a -text\n", "a\r\nM\r\nB\r\nc\r\n"),
    ] {
        let setup = |s: &str, side: &str| {
            let root = text_conflict(s, name, side);
            std::fs::write(root.join(".gitattributes"), attr).unwrap();
            for p in ["a", "f"] {
                std::fs::write(root.join(p), body).unwrap();
            }
            root
        };
        compare(stock, name, &setup, &[]);
    }
}

#[test]
fn a_clean_filter_runs_on_the_worktree_result() {
    let Some(stock) = stock_git() else { return };
    let setup = |s: &str, side: &str| {
        let root = text_conflict(s, "filter", side);
        std::fs::write(root.join(".gitattributes"), "f filter=up\n").unwrap();
        for p in ["a", "f"] {
            std::fs::write(root.join(p), "a\nm\nb\nc\n").unwrap();
        }
        root
    };
    compare(stock, "filter", &setup, &["-c", "filter.up.clean=tr a-z A-Z"]);
}

#[test]
fn a_deleted_result_is_deleted_file() {
    let Some(stock) = stock_git() else { return };
    let setup = |s: &str, side: &str| {
        let root = text_conflict(s, "deleted", side);
        std::fs::remove_file(root.join("f")).unwrap();
        root
    };
    compare(stock, "deleted", &setup, &[]);
}

#[cfg(unix)]
#[test]
fn a_symlink_result_is_its_target() {
    let Some(stock) = stock_git() else { return };
    // The merge leaves `l` pointing at `main`, which does not exist.
    compare(stock, "dangling", &|s: &str, side: &str| link_conflict(s, "dangling", side), &[]);
    // A link to an existing file still reads as the link text, not the file.
    let setup = |s: &str, side: &str| {
        let root = link_conflict(s, "resolving", side);
        std::fs::remove_file(root.join("l")).unwrap();
        std::os::unix::fs::symlink("tgt", root.join("l")).unwrap();
        root
    };
    compare(stock, "resolving", &setup, &[]);
    // A regular file where the link was: its own mode, read as a file.
    let setup = |s: &str, side: &str| {
        let root = link_conflict(s, "file", side);
        std::fs::remove_file(root.join("l")).unwrap();
        std::fs::write(root.join("l"), "main\n").unwrap();
        root
    };
    compare(stock, "file", &setup, &[]);
    // `core.symlinks=false` and every parent a link: the file is a fake symlink,
    // read at `S_IFLNK` without `convert_to_git()`.
    compare(stock, "fake", &setup, &["-c", "core.symlinks=false", "-c", "core.autocrlf=true"]);
}

/// `sub` conflicted between two commits of its own repository, which the merge
/// left checked out at the stage #2 commit. None of the three gitlink commits is
/// an object of the superproject.
fn gitlink_conflict(stock: &str, name: &str, side: &str) -> PathBuf {
    let root = fresh(name, side);
    let sub = root.join("sub");
    std::fs::create_dir(&sub).unwrap();
    git(stock, &sub, &["init", "-q", "-b", "main"]);
    let mut commits = Vec::new();
    for (n, from) in [("1", None), ("2", None), ("3", Some("HEAD~1"))] {
        if let Some(rev) = from {
            git(stock, &sub, &["checkout", "-q", rev]);
        }
        std::fs::write(sub.join("f"), n).unwrap();
        git(stock, &sub, &["add", "f"]);
        git(stock, &sub, &["commit", "-q", "-m", n]);
        let (_, head, _) = run(stock, &sub, &["rev-parse", "HEAD"]);
        commits.push(head.trim().to_owned());
    }
    let gitlink = |c: &str| format!("160000,{c},sub");
    git(stock, &root, &["update-index", "--add", "--cacheinfo", &gitlink(&commits[0])]);
    git(stock, &root, &["commit", "-q", "-m", "base"]);
    git(stock, &root, &["checkout", "-q", "-b", "side"]);
    git(stock, &root, &["update-index", "--cacheinfo", &gitlink(&commits[1])]);
    git(stock, &root, &["commit", "-q", "-m", "side"]);
    git(stock, &root, &["checkout", "-q", "main"]);
    git(stock, &root, &["update-index", "--cacheinfo", &gitlink(&commits[2])]);
    git(stock, &root, &["commit", "-q", "-m", "main"]);
    assert_eq!(run(stock, &root, &["merge", "-q", "side"]).0, 1);
    root
}

const GITLINK_FORMATS: &[&[&str]] =
    &[&["-c"], &["--cc"], &["--cc", "--abbrev=4"], &["--cc", "--full-index"], &["-c", "--raw"]];

#[test]
fn a_gitlink_result_is_its_head_and_abbreviates_like_any_id() {
    let Some(stock) = stock_git() else { return };
    // `repo_find_unique_abbrev()` keeps a commit the superproject does not hold at
    // the requested width, and `grab_blob()` reads each stage as its
    // `Subproject commit` line, never as an object (`git diff` died there).
    let clean = |s: &str, side: &str| gitlink_conflict(s, "gitlink", side);
    compare_runs(stock, "gitlink", &clean, &[], &["diff-files"], GITLINK_FORMATS);
    compare_runs(stock, "gitlink-diff", &clean, &[], &["diff"], &[&[], &["-c"], &["--cc", "--stat"]]);
    // Dirty inside, and moved to the merge base: the result is still the
    // submodule's `HEAD`.
    let dirty = |s: &str, side: &str| {
        let root = gitlink_conflict(s, "gitlink-dirty", side);
        std::fs::write(root.join("sub/f"), "dirty\n").unwrap();
        root
    };
    compare_runs(stock, "gitlink-dirty", &dirty, &[], &["diff-files"], GITLINK_FORMATS);
    let moved = |s: &str, side: &str| {
        let root = gitlink_conflict(s, "gitlink-moved", side);
        git(s, &root.join("sub"), &["checkout", "-q", "HEAD~1"]);
        root
    };
    compare_runs(stock, "gitlink-moved", &moved, &[], &["diff-files"], GITLINK_FORMATS);
    compare_runs(stock, "gitlink-moved-diff", &moved, &[], &["diff"], &[&[], &["-c"]]);
}

#[test]
fn textconv_runs_on_the_worktree_result_and_every_stage() {
    let Some(stock) = stock_git() else { return };
    let setup = |s: &str, side: &str| {
        let root = text_conflict(s, "textconv", side);
        std::fs::write(root.join(".gitattributes"), "f diff=up\n").unwrap();
        root
    };
    let cfg = &["-c", "diff.up.textconv=tr a-z A-Z <"];
    // `diff-files` leaves `allow_textconv` off unless `--textconv` asks for it.
    let formats: &[&[&str]] = &[
        &["-c"],
        &["--cc"],
        &["--cc", "--textconv"],
        &["-c", "--textconv", "-U1"],
        &["--cc", "--textconv", "--no-textconv"],
    ];
    compare_runs(stock, "textconv", &setup, cfg, &["diff-files"], formats);
    compare_runs(stock, "textconv-diff", &setup, cfg, &["diff"], &[&[], &["--no-textconv"], &["-c"]]);
    // A binary result read through textconv is never `Binary files differ`.
    let binary = |s: &str, side: &str| {
        let root = text_conflict(s, "textconv-binary", side);
        std::fs::write(root.join(".gitattributes"), "f diff=up\n").unwrap();
        std::fs::write(root.join("f"), "a\0\n<<<\nM\n===\nB\n>>>\nc\n").unwrap();
        root
    };
    let cfg = &["-c", "diff.up.textconv=tr \\\\000 @ <"];
    compare_runs(stock, "textconv-binary", &binary, cfg, &["diff-files"], formats);
}
