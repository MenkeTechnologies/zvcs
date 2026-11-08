//! What a hook sees of the process that runs it, where that process is not a
//! plain `setup_git_directory()` command.
//!
//! * `receive-pack` finds its repository with `enter_repo()`, which `chdir()`s
//!   into the git directory and sets it to `.` (setup.c:1879-1897), so
//!   `find_hook()` names `hooks/<name>` (or the `core.hooksPath` spelling) and
//!   that is `$0` for `pre-receive`, `update`, `post-receive`, `proc-receive`,
//!   `push-to-checkout` and the `reference-transaction` hook alike, which also
//!   get no `GIT_PREFIX`. zvcs exec'd the absolute path, and ran
//!   `reference-transaction` in the work tree with an empty `GIT_PREFIX`.
//! * `push-to-checkout` runs with no `cp->dir` in that git directory, with
//!   `GIT_DIR=absolute_path(".")` and its stdout on stderr (receive-pack.c:1433-
//!   1467, hook.h:183-186). zvcs ran it in the work tree with stdout on the
//!   protocol stream, which broke the push.
//! * `worktree add` runs `git branch`, `git symbolic-ref` and `git reset --hard`
//!   children with `GIT_DIR=<path>/.git` and `GIT_WORK_TREE=<path>`
//!   (builtin/worktree.c:584-585); zvcs does their work in-process, and its
//!   `reference-transaction` hooks now see each child's cwd, `GIT_DIR`,
//!   `GIT_WORK_TREE` (`.` once `setup_work_tree()` ran, setup.c:494-514) and
//!   `GIT_PREFIX`.
//!
//! Each case runs stock git and zvcs on twin fixtures and compares the output
//! with the fixture's own path replaced.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PREFIX")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// A hook reporting `$0`, its cwd and the setup variables on stderr.
fn hook(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let script = format!(
        "#!/bin/sh\necho \"{name} 0=$0 PWD=$PWD GIT_DIR=${{GIT_DIR-unset}} GWT=${{GIT_WORK_TREE-unset}} PFX=${{GIT_PREFIX-unset}}\" >&2\ncat >/dev/null\n"
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Run `steps` against a fresh fixture for stock git and for zvcs, each step being a
/// subdirectory and stock-git arguments in which `@BIN@` stands for the binary under test;
/// `prepare` builds the fixture with stock git.
fn compare(tag: &str, prepare: impl Fn(&str, &Path), steps: &[(&str, Vec<String>)]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-hook-entered-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let root = base.join(side);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        prepare(stock, &root);
        let mut out = Vec::new();
        for (dir, args) in steps {
            let args: Vec<String> = args.iter().map(|a| a.replace("@BIN@", bin)).collect();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let (o, e, c) = run(stock, &root.join(dir), &args[..]);
            let cut = |s: String| s.replace(root.to_str().unwrap(), "ROOT");
            out.push((cut(o), cut(e), c));
        }
        outputs.push(out);
    }
    assert_eq!(outputs[1], outputs[0]);
}

fn base_repo(stock: &str, root: &Path) {
    let git =
        |dir: &str, args: &[&str]| assert_eq!(run(stock, &root.join(dir), args).2, 0, "{args:?}");
    std::fs::create_dir_all(root.join("r")).unwrap();
    git("r", &["init", "-q", "-b", "main", "."]);
    std::fs::write(root.join("r/a"), "a\n").unwrap();
    git("r", &["add", "a"]);
    git(
        "r",
        &["-c", "maintenance.auto=false", "commit", "-q", "-m", "a"],
    );
    git("r", &["branch", "x"]);
}

fn push(target: &str, refspec: &str) -> (&'static str, Vec<String>) {
    let args = [
        "push",
        "-q",
        target,
        refspec,
        "--receive-pack=@BIN@ receive-pack",
    ];
    ("r", args.iter().map(|s| s.to_string()).collect())
}

#[test]
fn receive_side_hooks_run_in_the_entered_git_dir() {
    compare(
        "receive",
        |stock, root| {
            base_repo(stock, root);
            let git = |dir: &str, args: &[&str]| {
                assert_eq!(run(stock, &root.join(dir), args).2, 0, "{args:?}")
            };
            git(".", &["init", "-q", "--bare", "b.git"]);
            git(".", &["init", "-q", "t"]);
            git(".", &["init", "-q", "--bare", "h.git"]);
            git("h.git", &["config", "core.hooksPath", "./myhooks"]);
            for (dir, names) in [
                (
                    "b.git/hooks",
                    &[
                        "pre-receive",
                        "update",
                        "post-receive",
                        "reference-transaction",
                    ][..],
                ),
                (
                    "t/.git/hooks",
                    &[
                        "pre-receive",
                        "update",
                        "post-receive",
                        "reference-transaction",
                    ][..],
                ),
                (
                    "h.git/myhooks",
                    &[
                        "pre-receive",
                        "update",
                        "post-receive",
                        "reference-transaction",
                    ][..],
                ),
            ] {
                for name in names {
                    hook(&root.join(dir), name);
                }
            }
        },
        &[
            push("../b.git", "x"),
            push("../t", "x"),
            push("../h.git", "x:y"),
        ],
    );
}

#[test]
fn push_to_checkout_runs_in_the_git_dir_with_its_stdout_on_stderr() {
    compare(
        "ptc",
        |stock, root| {
            base_repo(stock, root);
            let git = |dir: &str, args: &[&str]| {
                assert_eq!(run(stock, &root.join(dir), args).2, 0, "{args:?}")
            };
            git(".", &["clone", "-q", "r", "t"]);
            git(
                "t",
                &["config", "receive.denyCurrentBranch", "updateInstead"],
            );
            std::fs::write(root.join("r/a"), "b\n").unwrap();
            git(
                "r",
                &["-c", "maintenance.auto=false", "commit", "-q", "-am", "b"],
            );
            let path = root.join("t/.git/hooks/push-to-checkout");
            let script =
                "#!/bin/sh\necho \"ptc 0=$0 PWD=$PWD GIT_DIR=$GIT_DIR GWT=$GIT_WORK_TREE\"\n";
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        },
        &[push("../t", "main:main")],
    );
}

#[test]
fn worktree_add_hooks_see_each_childs_setup() {
    let steps: Vec<(&str, Vec<String>)> = [
        ("r", vec!["@BIN@", "worktree", "add", "-q", "../wt1"]),
        ("r/sub", vec!["@BIN@", "worktree", "add", "-q", "../../wt2"]),
        (
            "r",
            vec![
                "@BIN@", "worktree", "add", "-q", "--orphan", "-b", "o", "../wt3",
            ],
        ),
        (
            "r",
            vec!["@BIN@", "worktree", "add", "-q", "--detach", "../wt4"],
        ),
    ]
    .into_iter()
    .map(|(dir, args)| (dir, args.into_iter().map(String::from).collect()))
    .collect();
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-hook-entered-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let root = base.join(side);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        base_repo(stock, &root);
        std::fs::create_dir_all(root.join("r/sub")).unwrap();
        hook(&root.join("r/.git/hooks"), "reference-transaction");
        let mut out = Vec::new();
        for (dir, args) in &steps {
            let (prog, rest) = (args[0].replace("@BIN@", bin), &args[1..]);
            let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
            let (o, e, c) = run(&prog, &root.join(dir), &rest);
            let cut = |s: String| s.replace(root.to_str().unwrap(), "ROOT");
            out.push((cut(o), cut(e), c));
        }
        outputs.push(out);
    }
    assert_eq!(outputs[1], outputs[0]);
}
