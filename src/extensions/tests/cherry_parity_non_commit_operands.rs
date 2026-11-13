//! `git cherry` operands that name something other than a commit, and a
//! malformed `--abbrev`.
//!
//! `add_pending_commit()` (builtin/log.c:2574-2587) peels each operand with the
//! loud `lookup_commit_reference()`, so a tree or a blob is reported by
//! `object_as_type()`'s `error: object <id> is a tree, not a commit` before
//! `cmd_cherry()` dies with `fatal: unknown commit <name>` (builtin/log.c:2782-2797).
//!
//! `OPT__ABBREV`'s callback answers a non-number with `error()`, which
//! `parse_options()` ends on at exit 129 without printing the usage block.
//!
//! Every comparison runs stock git and zvcs on twin fixtures built by stock git
//! with pinned dates.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        .stdin(Stdio::null())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

/// `base` on `main`, one commit on `up`, one on `work`, and a tag `tt` that
/// points at `up`'s tree.
fn prepare(stock: &str, r: &Path) {
    let ok = |args: &[&str]| {
        let (_, err, code) = run(stock, r, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    ok(&["init", "-q", "-b", "main", "."]);
    for (branch, file) in [("main", "base"), ("up", "u"), ("work", "w")] {
        if branch != "main" {
            ok(&["checkout", "-q", "-b", branch, "main"]);
        }
        std::fs::write(r.join(file), file).unwrap();
        ok(&["add", file]);
        ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", file]);
    }
    ok(&["tag", "-a", "-m", "tree", "tt", "up^{tree}"]);
}

fn compare(tag: &str, args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-cherry-nc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let r = base.join(side);
        std::fs::create_dir_all(&r).unwrap();
        prepare(stock, &r);
        outputs.push(run(bin, &r, args));
    }
    assert_eq!(outputs[1], outputs[0], "{tag}: {args:?}");
}

#[test]
fn a_non_commit_operand_is_reported_before_unknown_commit() {
    compare("head-tree", &["cherry", "-v", "up", "work^{tree}"]);
    compare("upstream-tree", &["cherry", "-v", "up^{tree}", "work"]);
    compare("limit-blob", &["cherry", "-v", "up", "work", "main:base"]);
    compare("tag-of-tree", &["cherry", "tt", "work"]);
}

#[test]
fn a_non_numeric_abbrev_has_no_usage_block() {
    compare("abbrev-x", &["cherry", "--abbrev=x", "up"]);
    compare("abbrev-3x", &["cherry", "--abbrev=3x", "up"]);
}
