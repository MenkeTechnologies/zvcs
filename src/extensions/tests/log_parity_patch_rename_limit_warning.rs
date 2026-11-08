//! `log -p` never printed the rename-limit warning.
//!
//! `cmd_log_walk()` ends in `diff_result_code()`, whose `diff_warn_rename_limit()`
//! reports the `needed_rename_limit` each commit's `diffcore_rename()` left in
//! `opt->diffopt` (diff.c:7524-7528, diffcore-rename.c:1092, :1111). zvcs only
//! collected those warnings on the paths that queue the changes for the name and
//! count formats; a patch alone was rendered by the batched patch window, which
//! dropped them. `--follow`'s own pass is `try_to_follow_renames()`'s fresh
//! `diff_options` (tree-diff.c:631-640), which takes `diff.renameLimit`, never the
//! command's `-l<n>` (diff.c:5128, :5317-5318).
//!
//! Each case runs stock git and zvcs on twin repositories and compares stdout,
//! stderr and the exit status.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
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
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

impl Fixture {
    /// Three 50-line files renamed with one line appended to each, then an empty commit.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zvcs-log-patch-rename-limit-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| assert_eq!(run(stock, &root, args).2, 0, "{args:?}");
        git(&["init", "-q", "-b", "main", "."]);
        for i in 1..=3 {
            let body: String = (1..=50).map(|n| format!("f{i} {n}\n")).collect();
            std::fs::write(root.join(format!("a{i}")), body).unwrap();
        }
        git(&["add", "."]);
        git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
        for i in 1..=3 {
            git(&["mv", &format!("a{i}"), &format!("b{i}")]);
            let path = root.join(format!("b{i}"));
            let body = std::fs::read_to_string(&path).unwrap() + "x\n";
            std::fs::write(path, body).unwrap();
        }
        git(&["add", "-A"]);
        git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "two"]);
        git(&[
            "-c",
            "maintenance.auto=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "three",
        ]);
        Fixture { root }
    }
}

fn compare(tag: &str, cases: &[&[&str]]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let f = Fixture::new(stock, tag);
    for args in cases {
        assert_eq!(
            run(BIN, &f.root, args),
            run(stock, &f.root, args),
            "{args:?}"
        );
    }
}

#[test]
fn a_patch_reports_the_limit_its_rename_pass_hit() {
    compare(
        "patch",
        &[
            &["-c", "diff.renameLimit=1", "log", "-p", "-1", "HEAD~1"],
            &["log", "-l1", "-p", "-1", "HEAD~1"],
            &["log", "-l1", "-p"],
            &["log", "-l1", "-p", "--format=%s"],
            &["log", "-l1", "-p", "--reverse"],
            &["log", "-l1", "-p", "--graph"],
            &["log", "-C", "-C", "-l1", "-p", "-1", "HEAD~1"],
            &["log", "-l1", "-p", "--no-renames"],
            &["log", "-l2", "-p"],
        ],
    );
}

#[test]
fn follow_takes_the_configured_limit_not_the_switch() {
    compare(
        "follow",
        &[
            &["log", "-l1", "--follow", "-p", "--", "b1"],
            &["log", "-l1", "--follow", "--name-status", "--", "b1"],
            &[
                "-c",
                "diff.renameLimit=1",
                "log",
                "--follow",
                "-p",
                "--",
                "b1",
            ],
        ],
    );
}
