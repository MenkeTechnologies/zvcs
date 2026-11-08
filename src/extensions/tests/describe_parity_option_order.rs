//! `git describe`'s checks run in `cmd_describe()`'s order, and
//! `--end-of-options` ends its options.
//!
//! `option '--dirty' and commit-ishes cannot be used together` and its
//! `--broken` twin are the last branches of `cmd_describe()`
//! (builtin/describe.c:786-790), reached only after the refs were read and
//! `No names found, cannot describe anything.` had its chance
//! (builtin/describe.c:718-723). zvcs checked the commit-ishes first.
//!
//! `parse_options_step()` matches `--end-of-options` by `strcmp()` and, with
//! no `PARSE_OPT_KEEP_UNKNOWN_OPT`, drops it and stops parsing
//! (parse-options.c:1119-1124); zvcs called it an unknown option.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

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

fn git(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0100")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0100")
        .env("GIT_EDITOR", "true")
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
    /// Two copies of a two-commit history built by stock git, tagged with
    /// `tags` (`<name>` lightweight, `a:<name>` annotated) at `HEAD~1`.
    fn new(stock: &str, tag: &str, tags: &[&str]) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-describe-option-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for side in ["stock", "zvcs"] {
            let work = root.join(side);
            std::fs::create_dir_all(&work).unwrap();
            let run = |args: &[&str]| {
                let (_, err, rc) = git(stock, &work, &root, args);
                assert_eq!(rc, 0, "{args:?}: {err}");
            };
            run(&["init", "-q", "-b", "main", "."]);
            std::fs::write(work.join("x"), "1\n").unwrap();
            run(&["add", "x"]);
            run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
            for t in tags {
                match t.strip_prefix("a:") {
                    Some(name) => run(&["tag", "-a", "-m", name, name]),
                    None => run(&["tag", t]),
                }
            }
            std::fs::write(work.join("x"), "2\n").unwrap();
            run(&["-c", "maintenance.auto=false", "commit", "-q", "-am", "two"]);
        }
        Fixture { root }
    }

    fn assert_same(&self, stock: &str, args: &[&str]) {
        let s = git(stock, &self.root.join("stock"), &self.root, args);
        let z = git(BIN, &self.root.join("zvcs"), &self.root, args);
        assert_eq!(z, s, "git {args:?}");
    }
}

#[test]
fn no_names_found_comes_before_the_commit_ish_conflict() {
    let Some(stock) = stock_git() else { return };
    for (tag, tags) in [("none", &[][..]), ("light", &["l1"]), ("annotated", &["a:v1"])] {
        let f = Fixture::new(stock, tag, tags);
        for args in [
            &["describe", "--dirty", "HEAD"][..],
            &["describe", "--broken", "HEAD"],
            &["describe", "--dirty=-x", "HEAD~1"],
            &["describe", "--dirty", "--always", "HEAD"],
            &["describe", "--tags", "--dirty", "HEAD"],
            &["describe", "--all", "--broken", "HEAD"],
            &["describe", "--match", "zz*", "--dirty", "HEAD"],
        ] {
            f.assert_same(stock, args);
        }
    }
}

#[test]
fn end_of_options_ends_the_options() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "eoo", &["a:v1"]);
    for args in [
        &["describe", "--end-of-options", "HEAD"][..],
        &["describe", "--tags", "--end-of-options", "HEAD~1", "HEAD"],
        &["describe", "--end-of-options", "--tags"],
        &["describe", "--end-of-options"],
        &["describe", "--end-of-option", "HEAD"],
    ] {
        f.assert_same(stock, args);
    }
}
