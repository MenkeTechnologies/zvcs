//! A `GIT_WORK_TREE` that `stat()` does not find.
//!
//! builtin/clone.c:1091-1129: past the `path_exists()` refusal, a non-bare clone with
//! `GIT_WORK_TREE` set makes the destination the git directory itself
//! (`git_dir = xstrdup(dir)`), creates the work tree's leading directories and then
//! the work tree with `mkdir()` (only when the destination did not exist), and arms
//! `junk_work_tree` on it beside `junk_git_dir` on the destination. `init_db()` then
//! records the work tree's real path as `core.worktree` unless the git directory is
//! its `.git` (setup.c:2434-2442, :2640-2641).
//!
//! So a dangling symlink dies `could not create work tree dir 'ln': File exists`, a
//! missing directory is created and checked out with the repository at the
//! destination, an existing empty destination leaves the work tree unmade and
//! `checkout()`'s `setup_work_tree()` dies on it, and a clone that fails partway
//! takes both directories away. zvcs ignored the variable and cloned into the
//! destination. Each case runs stock git and zvcs on identical fixtures and compares
//! stderr, the exit status and the tree left behind, configuration included.

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn stock_available() -> bool {
    Path::new(STOCK).exists()
}

fn run(bin: &str, dir: &Path, home: &Path, work_tree: Option<&str>, args: &[&str]) -> (String, String, i32) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
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
        .env("TZ", "UTC")
        .env_remove("GIT_DIR");
    match work_tree {
        Some(wt) => cmd.env("GIT_WORK_TREE", wt),
        None => cmd.env_remove("GIT_WORK_TREE"),
    };
    let out = cmd.output().expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// Every path under `dir` but the object store and the sample hooks, then the contents of
/// each `config` and of any regular file named `dst` (a `--separate-git-dir` link file),
/// with `dir` itself written as `<C>`.
fn snapshot(dir: &Path) -> String {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(base).expect("under base").to_path_buf();
            out.push(rel);
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            let name = entry.file_name();
            if is_dir && name != "objects" && name != "hooks" {
                walk(base, &path, out);
            }
        }
    }
    let mut paths = Vec::new();
    walk(dir, dir, &mut paths);
    paths.sort();
    let mut text = String::new();
    for rel in &paths {
        text.push_str(&format!("{}\n", rel.display()));
    }
    for rel in &paths {
        let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let full = dir.join(rel);
        if (name == "config" || name == "dst") && full.is_file() {
            let body = std::fs::read_to_string(&full).unwrap_or_default();
            text.push_str(&format!("== {}\n{body}", rel.display()));
        }
    }
    let real = std::fs::canonicalize(dir).expect("case dir");
    text.replace(&*real.to_string_lossy(), "<C>").replace(&*dir.to_string_lossy(), "<C>")
}

/// `main` with one tracked file and a tag `v1`, built by stock git.
fn source(root: &Path) -> PathBuf {
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("src");
    let git = |args: &[&str]| run(STOCK, &src, root, None, args);
    git(&["init", "-q", "-b", "main", "."]);
    std::fs::write(src.join("a"), "one\n").expect("write a");
    git(&["add", "a"]);
    git(&["commit", "-q", "-m", "init"]);
    git(&["tag", "v1"]);
    src
}

/// Run `prepare` then `git clone <args>` under `GIT_WORK_TREE=<work_tree>` in a fresh
/// directory for each binary, and compare everything both leave behind.
fn compare(root: &Path, name: &str, prepare: &dyn Fn(&Path), work_tree: &str, args: &[&str]) {
    let src = root.join("src");
    let mut seen = Vec::new();
    for (label, bin) in [("stock", STOCK), ("zvcs", ZVCS)] {
        let case = root.join(label).join(name);
        std::fs::create_dir_all(&case).expect("case dir");
        prepare(&case);
        let mut full = vec!["clone"];
        full.extend_from_slice(args);
        let src_arg = src.to_string_lossy().into_owned();
        full.push(&src_arg);
        let (_, err, code) = run(bin, &case, root, Some(work_tree), &full);
        let err = err.replace(&*case.to_string_lossy(), "<C>");
        seen.push((err, code, snapshot(&case)));
    }
    assert_eq!(seen[0], seen[1], "{name}: stock vs zvcs (stderr, exit, tree)");
}

#[test]
fn a_work_tree_stat_does_not_find_is_made_and_the_destination_holds_the_git_dir() {
    if !stock_available() {
        return;
    }
    let root = std::env::temp_dir().join(format!("zvcs-clone-missing-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("root");
    let root = std::fs::canonicalize(&root).expect("real root");
    source(&root);
    let nothing = |_: &Path| {};
    let empty_dst = |case: &Path| std::fs::create_dir(case.join("dst")).expect("mkdir dst");

    // The ordinary case: `wt` is made and checked out, `dst` is the git dir with
    // `core.worktree` pointing at the work tree's real path.
    compare(&root, "missing", &nothing, "wt", &["dst"]);
    compare(&root, "quiet", &nothing, "wt", &["-q", "dst"]);
    compare(&root, "leading", &nothing, "a/b/wt", &["dst"]);
    compare(&root, "nested-destination", &nothing, "wt", &["x/y/dst"]);
    compare(&root, "no-checkout", &nothing, "wt", &["-n", "dst"]);
    compare(&root, "detached", &nothing, "wt", &["-b", "v1", "dst"]);
    // The git dir is the work tree's own `.git`: no `core.worktree`.
    compare(&root, "dot-git", &nothing, "wt", &["wt/.git"]);
    // `--separate-git-dir`: the link file takes the destination's place.
    compare(&root, "separate-git-dir", &nothing, "wt", &["--separate-git-dir=real", "dst"]);

    // `mkdir()` refuses a dangling symlink, and a leading directory through a file.
    compare(
        &root,
        "dangling",
        &|case: &Path| std::os::unix::fs::symlink("nowhere", case.join("ln")).expect("symlink"),
        "ln",
        &["dst"],
    );
    compare(
        &root,
        "through-file",
        &|case: &Path| {
            std::fs::create_dir(case.join("p")).expect("mkdir p");
            std::fs::write(case.join("p/f"), "").expect("write f");
        },
        "p/f/wt",
        &["dst"],
    );

    // An existing empty destination: the work tree is never made, so the checkout
    // dies with the repository kept; `--separate-git-dir` is refused by `init_db()`.
    compare(&root, "existing-destination", &empty_dst, "wt", &["dst"]);
    compare(&root, "existing-destination-sgd", &empty_dst, "wt", &["--separate-git-dir=real", "dst"]);

    // A clone that dies after the transfer removes the git dir and the work tree; an
    // existing destination is emptied but kept, and leading directories stay.
    compare(&root, "bad-branch", &nothing, "wt", &["-b", "nosuch", "dst"]);
    compare(&root, "bad-branch-existing", &empty_dst, "wt", &["-b", "nosuch", "dst"]);
    compare(&root, "bad-branch-leading", &nothing, "a/b/wt", &["-b", "nosuch", "dst"]);

    // The clone is usable: stock git reads it the same way from both.
    let mut reads = Vec::new();
    for label in ["stock", "zvcs"] {
        let case = root.join(label).join("missing");
        let mut out = String::new();
        for args in [
            &["--git-dir=dst", "status", "--porcelain"][..],
            &["--git-dir=dst", "for-each-ref"],
            &["--git-dir=dst", "reflog", "list"],
            &["--git-dir=dst", "fsck"],
            &["--git-dir=dst", "rev-parse", "--show-toplevel"],
        ] {
            let (stdout, stderr, code) = run(STOCK, &case, &root, None, args);
            out.push_str(&format!("{args:?} {code}\n{stdout}{stderr}"));
        }
        reads.push(out.replace(&*case.to_string_lossy(), "<C>"));
    }
    assert_eq!(reads[0], reads[1], "stock git reading each clone");
    let _ = std::fs::remove_dir_all(&root);
}
