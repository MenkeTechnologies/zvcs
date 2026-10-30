//! A gitlink whose `<path>/.git` is no git directory.
//!
//! `is_submodule_modified()` (submodule.c:1880-1902) reads `<path>/.git` through
//! `read_gitfile()`, falling back to the path itself, and when that is no git
//! directory it calls the submodule unpopulated — unless it is a directory, which
//! is `die("'%s' not recognized as a git repository")`. `read_gitfile()` is the
//! dying spelling of `read_gitfile_gently()`, so a gitfile it cannot use is
//! `invalid gitfile format: <path>/.git` or `gitfile does not point to a valid
//! repository: <path>/.git` (setup.c:927-952). `match_stat_with_submodule()`
//! (diff-lib.c:88-107) asks only when neither `ignore=all` nor `ignore=dirty`
//! applies, and `.gitmodules` plays no part beyond choosing that word, so a
//! named and an unnamed gitlink behave alike. zvcs called such a gitlink clean,
//! or reported a gitoxide error for a named one under `ignore=dirty`.
//!
//! The same walk compares an unborn submodule `HEAD` as git does:
//! `ce_compare_gitlink()` (read-cache.c:268-284) calls a `HEAD` that does not
//! resolve unchanged, where zvcs called the gitlink moved.
//!
//! Each case runs stock git and zvcs on identical fixtures and compares stdout,
//! stderr and the exit status.

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn stock_available() -> bool {
    Path::new(STOCK).exists()
}

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
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
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// What stands at `sub/.git` when the command runs.
#[derive(Clone, Copy, Debug)]
enum DotGit {
    /// A directory holding one stray file.
    JunkDir,
    /// An empty directory.
    EmptyDir,
    /// A symbolic link to a directory that is no repository.
    LinkToDir,
    /// A gitfile naming a path that does not exist.
    GitfileMissing,
    /// A gitfile naming a directory that is no repository.
    GitfileNotARepo,
    /// A file without the `gitdir: ` prefix.
    GitfileJunk,
    /// Nothing: `sub/` is empty.
    Absent,
    /// A git directory whose `HEAD` is unborn, with an untracked file beside it.
    UnbornWithUntracked,
}

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Two identical repositories, `stock/` and `zvcs/`, each with `a` and a
    /// gitlink `sub` committed — named in `.gitmodules` when `registered`, with
    /// `submodule.sub.ignore` set to `ignore` when given — and `sub/.git` shaped
    /// by `dot_git`. Stock git builds both.
    fn new(tag: &str, registered: bool, ignore: Option<&str>, dot_git: DotGit) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-submodule-not-a-repo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        let src = f.root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        f.git(&src, &["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("s"), "s\n").unwrap();
        f.git(&src, &["add", "s"]);
        f.git(&src, &["commit", "-q", "-m", "sub"]);
        let sub_head = f.git(&src, &["rev-parse", "HEAD"]).trim().to_string();
        for side in ["stock", "zvcs"] {
            let work = f.root.join(side);
            std::fs::create_dir_all(&work).unwrap();
            f.git(&work, &["init", "-q", "-b", "main", "."]);
            std::fs::write(work.join("a"), "a\n").unwrap();
            f.git(&work, &["add", "a"]);
            f.git(&work, &["update-index", "--add", "--cacheinfo", &format!("160000,{sub_head},sub")]);
            if registered {
                let mut modules = "[submodule \"sub\"]\n\tpath = sub\n\turl = ../src\n".to_string();
                if let Some(ignore) = ignore {
                    modules.push_str(&format!("\tignore = {ignore}\n"));
                }
                std::fs::write(work.join(".gitmodules"), modules).unwrap();
                f.git(&work, &["add", ".gitmodules"]);
            }
            f.git(&work, &["commit", "-q", "-m", "top"]);
            f.shape(&work, side, dot_git);
            // Refresh the stat data the writes above left behind, so only the
            // gitlink is in question.
            f.git(&work, &["update-index", "-q", "--refresh"]);
        }
        f
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let (out, err, code) = run(STOCK, dir, &self.root, args);
        assert_eq!(code, 0, "fixture step {args:?} failed: {err}");
        out
    }

    fn shape(&self, work: &Path, side: &str, dot_git: DotGit) {
        let sub = work.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let dot_git_path = sub.join(".git");
        let elsewhere = self.root.join(format!("elsewhere-{side}"));
        match dot_git {
            DotGit::JunkDir => {
                std::fs::create_dir_all(&dot_git_path).unwrap();
                std::fs::write(dot_git_path.join("junk"), "junk\n").unwrap();
            }
            DotGit::EmptyDir => std::fs::create_dir_all(&dot_git_path).unwrap(),
            DotGit::LinkToDir => {
                std::fs::create_dir_all(&elsewhere).unwrap();
                std::os::unix::fs::symlink(&elsewhere, &dot_git_path).unwrap();
            }
            DotGit::GitfileMissing => {
                std::fs::write(&dot_git_path, format!("gitdir: {}\n", elsewhere.display())).unwrap();
            }
            DotGit::GitfileNotARepo => {
                std::fs::create_dir_all(&elsewhere).unwrap();
                std::fs::write(&dot_git_path, format!("gitdir: {}\n", elsewhere.display())).unwrap();
            }
            DotGit::GitfileJunk => std::fs::write(&dot_git_path, "junk\n").unwrap(),
            DotGit::Absent => {}
            DotGit::UnbornWithUntracked => {
                self.git(&sub, &["init", "-q", "-b", "main", "."]);
                std::fs::write(sub.join("u"), "u\n").unwrap();
            }
        }
    }
}

const COMMANDS: &[&[&str]] = &[
    &["diff-files"],
    &["diff-files", "-p"],
    &["diff-files", "--ignore-submodules=dirty"],
    &["diff-files", "--ignore-submodules=none"],
    &["diff"],
    &["diff", "--stat"],
    &["diff", "HEAD"],
    &["diff", "--ignore-submodules=untracked"],
    &["diff", "--ignore-submodules=dirty"],
    &["diff", "--ignore-submodules=all"],
    &["status"],
    &["status", "--porcelain"],
    &["status", "-uno"],
    &["status", "--ignore-submodules=dirty"],
    &["diff-index", "HEAD"],
    &["diff-index", "-p", "HEAD"],
    &["diff-index", "--ignore-submodules=dirty", "HEAD"],
    &["diff-index", "--cached", "HEAD"],
];

fn check(registered: bool, ignore: Option<&str>, dot_git: DotGit) {
    let tag = format!("{registered}-{}-{dot_git:?}", ignore.unwrap_or("unset"));
    let f = Fixture::new(&tag, registered, ignore, dot_git);
    for args in COMMANDS {
        let stock = run(STOCK, &f.root.join("stock"), &f.root, args);
        let zvcs = run(ZVCS, &f.root.join("zvcs"), &f.root, args);
        assert_eq!(zvcs, stock, "{tag}: git {args:?}: (stdout, stderr, status) differ");
    }
}

const SHAPES: &[DotGit] = &[
    DotGit::JunkDir,
    DotGit::EmptyDir,
    DotGit::LinkToDir,
    DotGit::GitfileMissing,
    DotGit::GitfileNotARepo,
    DotGit::GitfileJunk,
    DotGit::Absent,
];

#[test]
fn an_unnamed_gitlink_dies_where_is_submodule_modified_does() {
    if !stock_available() {
        return;
    }
    for &shape in SHAPES {
        check(false, None, shape);
    }
}

#[test]
fn a_named_gitlink_dies_the_same_way() {
    if !stock_available() {
        return;
    }
    for &shape in SHAPES {
        check(true, None, shape);
    }
}

#[test]
fn submodule_ignore_all_or_dirty_skips_the_probe() {
    if !stock_available() {
        return;
    }
    for ignore in ["all", "dirty", "untracked"] {
        check(true, Some(ignore), DotGit::JunkDir);
        check(true, Some(ignore), DotGit::GitfileJunk);
    }
}

#[test]
fn an_unborn_submodule_head_is_no_move() {
    if !stock_available() {
        return;
    }
    check(false, None, DotGit::UnbornWithUntracked);
    check(true, None, DotGit::UnbornWithUntracked);
}
