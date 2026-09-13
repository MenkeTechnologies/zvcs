//! `repo_config_get_string()` / `repo_config_get_string_tmp()` (config.c:2374-2394)
//! die through `git_die_config()` (config.c:2561-2577) when the last value of the
//! key has no `=`: `error: missing value for '<key>'`, then a `fatal:` naming the
//! command line or the file and line, exit 128. Each test here is a call site
//! whose zvcs equivalent used to read the valueless key as unset.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-getstrdie-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.ok(&["add", "f"]);
        f.ok(&["commit", "-qm", "m"]);
        f
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env_remove("GIT_CONFIG_GLOBAL")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_NOTES_REF")
            .env("GIT_EDITOR", "true")
            .env("GIT_PAGER", "cat")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) {
        let out = self.run(args);
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn append_config(&self, text: &str) -> usize {
        let path = self.work.join(".git/config");
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str(text);
        std::fs::write(&path, &body).unwrap();
        body.lines().count()
    }
}

fn from_command_line(key: &str) -> String {
    format!("error: missing value for '{key}'\nfatal: unable to parse '{key}' from command-line config\n")
}

fn assert_died(out: &Output, stderr: &str) {
    assert_eq!(String::from_utf8_lossy(&out.stderr), stderr, "{out:?}");
    assert_eq!(out.status.code(), Some(128));
}

/// graph.c:362, builtin/log.c:242-245 and notes.c:1009, each reached only when
/// the reader runs: `--graph`, a decoration being shown, and notes being
/// displayed (the default for `log` without a `--pretty`).
#[test]
fn log_reads_die_only_where_git_reads_them() {
    let f = Fixture::new("log");
    assert_died(&f.run(&["-c", "log.graphcolors", "log", "--graph", "--oneline"]), &from_command_line("log.graphcolors"));
    f.ok(&["-c", "log.graphcolors", "log", "--oneline"]);

    assert_died(
        &f.run(&["-c", "log.initialDecorationSet", "log", "-1", "--format=%d"]),
        &from_command_line("log.initialdecorationset"),
    );
    f.ok(&["-c", "log.initialdecorationset", "log", "-1", "--oneline"]);

    for args in [&["log", "-1"][..], &["show", "-s"], &["notes", "list"], &["log", "-1", "--notes"]] {
        let mut argv = vec!["-c", "core.notesRef"];
        argv.extend_from_slice(args);
        assert_died(&f.run(&argv), &from_command_line("core.notesref"));
    }
    f.ok(&["-c", "core.notesref", "log", "-1", "--oneline"]);
}

/// branch.c:353-364 `read_branch_desc()`, for `branch --edit-description` and
/// the format-patch cover letter, with the file origin and its line.
#[test]
fn valueless_branch_description_dies_in_both_readers() {
    let f = Fixture::new("desc");
    assert_died(
        &f.run(&["-c", "branch.main.description", "branch", "--edit-description"]),
        &from_command_line("branch.main.description"),
    );
    let line = f.append_config("[branch \"main\"]\n\tdescription\n");
    let out = f.run(&["format-patch", "--cover-letter", "-1", "--stdout"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "error: missing value for 'branch.main.description'\n\
             fatal: bad config variable 'branch.main.description' in file '.git/config' at line {line}\n"
        )
    );
    assert_eq!(out.status.code(), Some(128));
    f.ok(&["format-patch", "--cover-letter", "--cover-from-description=none", "-1", "--stdout"]);
}
