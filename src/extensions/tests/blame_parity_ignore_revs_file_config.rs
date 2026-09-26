//! `blame.ignoreRevsFile` as `git_blame_config()` collects it.
//!
//! Each value goes through `git_config_pathname()` (config.c:1308-1329): a
//! valueless one is `config_error_nonbool()`, `~` is expanded, and an
//! `:(optional)` path that does not exist is dropped. The result is
//! `string_list_insert()`ed (builtin/blame.c:738-749), which keeps the list
//! sorted by `strcmp()` with duplicates dropped, so an empty value sorts first
//! instead of clearing the entries before it. `build_ignorelist()` then walks
//! the list, config entries followed by `--ignore-revs-file` arguments, and an
//! empty name clears the set built so far (builtin/blame.c:908-922). Every
//! file is opened relative to the work-tree top, where setup has moved git.
//!
//! zvcs kept the config values in file order and cleared on an empty one,
//! took `:(optional)` as part of the file name, sent `--ignore-revs-file=` to
//! `open("")`, and read relative names from the cwd. (A valueless key already
//! died; the walk that replaced the snapshot read keeps that.)
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `file` = `one\ntwo\n`, then `two` rewritten to `TWO` in a second
    /// commit, whose id is written to `revs`; `sub/` is an empty directory.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-blame-ignore-revs-file-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\ntwo\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("file"), "one\nTWO\n").unwrap();
        f.run(&["commit", "-q", "-am", "two"]);
        let head = f.run(&["rev-parse", "HEAD"]).0;
        std::fs::write(f.work.join("revs"), head).unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// `blame -s -l -L2,2` of the rewritten line, run from `dir`: the commit it
    /// is blamed on is the second commit unless that commit is ignored.
    fn line_two(&self, dir: &Path, config: &[&str], extra: &[&str]) -> (String, String, i32) {
        let mut args: Vec<&str> = Vec::new();
        for c in config {
            args.extend(["-c", c]);
        }
        args.extend(["blame", "-s", "-l", "-L2,2"]);
        args.extend_from_slice(extra);
        let file = if dir == self.work { "file" } else { "../file" };
        args.push(file);
        self.run_in(dir, &args)
    }

    fn ignored(&self) -> String {
        // The boundary commit, with git's `^` and one hex digit less.
        let one = self.run(&["rev-parse", "HEAD~1"]).0;
        format!("^{} 2) TWO\n", &one.trim()[..39])
    }

    fn not_ignored(&self) -> String {
        format!("{} 2) TWO\n", self.run(&["rev-parse", "HEAD"]).0.trim())
    }
}

#[test]
fn an_empty_config_value_sorts_first_instead_of_clearing() {
    let f = Fixture::new("empty-config");
    let got = f.line_two(&f.work, &["blame.ignoreRevsFile=revs", "blame.ignoreRevsFile="], &[]);
    assert_eq!(got, (f.ignored(), String::new(), 0));
    // The config entries are sorted: `aa` is opened, and missed, first.
    let got = f.line_two(&f.work, &["blame.ignoreRevsFile=zz", "blame.ignoreRevsFile=aa"], &[]);
    assert_eq!(got, (String::new(), "fatal: could not open object name list: aa\n".into(), 128));
}

#[test]
fn an_empty_command_line_file_clears_what_came_before() {
    let f = Fixture::new("empty-cli");
    let got = f.line_two(&f.work, &["blame.ignoreRevsFile=revs"], &["--ignore-revs-file="]);
    assert_eq!(got, (f.not_ignored(), String::new(), 0));
    let got = f.line_two(&f.work, &[], &["--ignore-revs-file=", "--ignore-revs-file=revs"]);
    assert_eq!(got, (f.ignored(), String::new(), 0));
}

#[test]
fn optional_paths_and_the_work_tree_top() {
    let f = Fixture::new("optional");
    let got = f.line_two(
        &f.work,
        &["blame.ignoreRevsFile=:(optional)nope", "blame.ignoreRevsFile=:(optional)revs"],
        &[],
    );
    assert_eq!(got, (f.ignored(), String::new(), 0));
    // From `sub/`, both spellings name `revs` at the top of the work tree.
    let sub = f.work.join("sub");
    assert_eq!(f.line_two(&sub, &["blame.ignoreRevsFile=revs"], &[]), (f.ignored(), String::new(), 0));
    assert_eq!(f.line_two(&sub, &[], &["--ignore-revs-file=revs"]), (f.ignored(), String::new(), 0));
}

#[test]
fn a_valueless_key_dies() {
    let f = Fixture::new("nonbool");
    let got = f.line_two(&f.work, &["blame.ignoreRevsFile"], &[]);
    assert_eq!(
        got,
        (
            String::new(),
            "error: missing value for 'blame.ignorerevsfile'\n\
             fatal: unable to parse 'blame.ignorerevsfile' from command-line config\n"
                .into(),
            128
        )
    );
}
