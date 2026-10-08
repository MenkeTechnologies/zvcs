//! `git jump` against stock git: without `--stdout` the quickfix list is written to a
//! `mktemp -t git-jump.XXXXXX` file and handed to `git var GIT_EDITOR`.
//!
//! * A vi-compatible editor is run as `<editor> -q <file>`.
//! * An editor whose command contains `emacs` gets `--eval` with a `grep "cat <file>"` form.
//! * The script exits with the editor's status and removes the file afterwards.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str], envs: &[(&str, &Path)]) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("VISUAL")
        .env_remove("EDITOR")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// A repository with one unstaged change, and an editor script that records what it was given.
fn fixture(root: &Path, editor_name: &str) -> std::path::PathBuf {
    run(BIN, root, &["init", "-q", "-b", "main"], &[]);
    std::fs::write(root.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    run(BIN, root, &["add", "f.txt"], &[]);
    run(BIN, root, &["commit", "-qm", "init"], &[]);
    std::fs::write(root.join("f.txt"), "one\nTWO\nthree\n").unwrap();
    let editor = root.join(editor_name);
    std::fs::write(
        &editor,
        "#!/bin/sh\n{ for a in \"$@\"; do printf '[%s]\\n' \"$a\"; done; } > \"$(dirname \"$0\")/args.txt\"\n\
         for last; do :; done\ncat \"$last\" > \"$(dirname \"$0\")/list.txt\" 2>/dev/null\nexit 3\n",
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    editor
}

/// `args` with the `mktemp` path (its directory and random suffix differ per run) replaced by
/// `<tmp>`, and how many such files still exist.
fn mask_temp_file(args: &str) -> (String, usize) {
    let Some(at) = args.find("git-jump.") else { return (args.to_owned(), 0) };
    let start = args[..at].rfind(|c| c == '[' || c == ' ').map_or(0, |i| i + 1);
    let name = at + "git-jump.".len();
    let end = name + args[name..].find(|c: char| !(c.is_alphanumeric() || c == '.')).unwrap_or(args.len() - name);
    let left = usize::from(Path::new(&args[start..end]).exists());
    (format!("{}<tmp>{}", &args[..start], &args[end..]), left)
}

/// What the editor saw (its argv with the temp file's random suffix masked), what it read, and
/// how many `git-jump.*` files were left behind.
fn observe(bin: &str, root: &Path, editor_name: &str) -> (String, String, Option<i32>, String, String, usize) {
    let editor = fixture(root, editor_name);
    let (out, err, code) = run(bin, root, &["jump", "diff"], &[("GIT_EDITOR", &editor)]);
    let args = std::fs::read_to_string(root.join("args.txt")).unwrap_or_default();
    let list = std::fs::read_to_string(root.join("list.txt")).unwrap_or_default();
    let (masked, left) = mask_temp_file(&args);
    (out, err, code, masked, list, left)
}

fn compare(editor_name: &str, expected_list: &str) {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-jump-editor-{editor_name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let want = observe(stock, &base.join("s").canonicalize_or(&base.join("s")), editor_name);
    let got = observe(BIN, &base.join("z").canonicalize_or(&base.join("z")), editor_name);
    assert_eq!(got, want);
    assert_eq!(got.2, Some(3));
    assert_eq!(got.4, expected_list);
    assert_eq!(got.5, 0, "temp file left behind");
    let _ = std::fs::remove_dir_all(&base);
}

trait CanonicalizeOr {
    fn canonicalize_or(&self, fallback: &Path) -> std::path::PathBuf;
}

impl CanonicalizeOr for std::path::PathBuf {
    /// The directory may not exist yet; create it so `canonicalize` resolves `/var` -> `/private/var`.
    fn canonicalize_or(&self, fallback: &Path) -> std::path::PathBuf {
        std::fs::create_dir_all(self).unwrap();
        self.canonicalize().unwrap_or_else(|_| fallback.to_owned())
    }
}

#[test]
fn a_vi_compatible_editor_gets_dash_q_and_the_file() {
    compare("ed.sh", "f.txt:2:1: two\n");
}

#[test]
fn an_emacs_editor_gets_the_grep_eval_form() {
    compare("fake-emacs.sh", "");
}

/// `mode_merge` forwards its arguments to `git ls-files -u`, so that command's own option
/// errors and flags decide what happens before any file is searched for markers.
#[test]
fn merge_mode_arguments_are_ls_files_arguments() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-jump-merge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let conflicted = |root: &Path, bin: &str| {
        std::fs::create_dir_all(root).unwrap();
        let git = |args: &[&str]| run(bin, root, args, &[]);
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(root.join("c.txt"), "base\n").unwrap();
        git(&["add", "c.txt"]);
        git(&["commit", "-qm", "base"]);
        git(&["checkout", "-qb", "side"]);
        std::fs::write(root.join("c.txt"), "side\n").unwrap();
        git(&["commit", "-qam", "side"]);
        git(&["checkout", "-q", "main"]);
        std::fs::write(root.join("c.txt"), "main\n").unwrap();
        git(&["commit", "-qam", "main"]);
        git(&["merge", "side"]);
    };
    let (s, z) = (base.join("s"), base.join("z"));
    conflicted(&s, stock);
    conflicted(&z, BIN);
    for args in [
        &["jump", "--stdout", "merge", "--bogus"][..],
        &["jump", "--stdout", "merge", "--stdout"],
        &["jump", "--stdout", "merge", "--error-unmatch", "nope"],
        &["jump", "--stdout", "merge", "--", "c.txt"],
        &["jump", "--stdout", "merge", "-s"],
        &["jump", "--stdout", "auto", "--bogus"],
        &["jump", "--stdout"],
    ] {
        assert_eq!(run(BIN, &z, args, &[]), run(stock, &s, args, &[]), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
