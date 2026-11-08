//! `git clean -i` run from inside an untracked directory that is itself a
//! candidate, compared against stock git 2.56.0 on identical fixtures.
//!
//! `cmd_clean()` stores each candidate as `relative_path(ent->name, prefix, &buf)`
//! (builtin/clean.c:1044-1045), and `relative_path("ud/", "ud/")` is `./`
//! (path.c:1012-1014). Every interactive listing — `pretty_print_dels()`, the
//! numbered `select by numbers` menu and `ask each` — shows that string, so the
//! directory the command runs in reads `./`, where zvcs printed an empty name.
//!
//! `select by numbers` lists each string as stored, unquoted
//! (`print_highlight_menu_stuff()`, builtin/clean.c:389-399): `q"t` stays `q"t` there
//! while `pretty_print_dels()` shows `"q\"t"`. And `--end-of-options` ends the
//! options like `--` (parse-options.c:1113-1124).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, top: &Path, cwd: &str, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(top.join(cwd))
        .env("HOME", top)
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
        .env("TERM", "dumb")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Tracked `t`; untracked `u`, `ud/deep/x` and `q"t`, built by stock git.
fn fixture(stock: &str, top: &Path) {
    let _ = std::fs::remove_dir_all(top);
    std::fs::create_dir_all(top.join("ud/deep")).unwrap();
    let g = |args: &[&str]| {
        let out = run(stock, top, "", args, "");
        assert_eq!(out.0, 0, "setup {args:?}: {out:?}");
    };
    g(&["init", "-q", "-b", "main", "."]);
    std::fs::write(top.join("t"), "t\n").unwrap();
    g(&["add", "t"]);
    g(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "t"]);
    std::fs::write(top.join("u"), "u\n").unwrap();
    std::fs::write(top.join("ud/deep/x"), "x\n").unwrap();
    std::fs::write(top.join("q\"t"), "q\n").unwrap();
}

fn compare(tag: &str, cwd: &str, args: &[&str], stdin: &str) {
    let Some(stock) = stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-clean-icwd-{tag}-{}", std::process::id()));
    let _guard = Dir(root.clone());
    let mut seen = Vec::new();
    for (side, bin) in [("stock", stock), ("zvcs", BIN)] {
        let top = root.join(side);
        fixture(stock, &top);
        let out = run(bin, &top, cwd, args, stdin);
        let status = run(stock, &top, "", &["status", "--porcelain", "-uall"], "");
        seen.push((out, status));
    }
    assert_eq!(seen[0], seen[1], "{tag}: git {args:?} from {cwd:?} (stock, zvcs)");
}

#[test]
fn the_cwd_candidate_is_listed_as_dot_slash() {
    compare("c", "ud", &["clean", "-di"], "c\n");
    compare("q", "ud", &["clean", "-di"], "q\n");
    compare("sel", "ud", &["clean", "-di"], "3\n\nq\n");
    compare("ask", "ud", &["clean", "-di"], "4\nn\n");
    compare("filter", "ud", &["clean", "-di"], "2\nzz\n\nq\n");
    // From a deeper directory the candidate is reached as `../`.
    compare("deep", "ud/deep", &["clean", "-di"], "q\n");
    compare("dry", "ud", &["clean", "-ndi"], "c\n");
}

#[test]
fn select_by_numbers_lists_names_unquoted() {
    compare("sel-top", "", &["clean", "-di"], "3\n2\n\nc\n");
    compare("sel-prefix", "", &["clean", "-di"], "3\nq\n\nc\n");
    compare("sel-help", "", &["clean", "-di"], "3\n?\n\nq\n");
}

#[test]
fn end_of_options_ends_the_options() {
    compare("eoo", "", &["clean", "-nd", "--end-of-options", "ud"], "");
    compare("eoo-dash", "", &["clean", "-nd", "--end-of-options", "-x"], "");
    compare("eoo-f", "", &["clean", "-f", "--end-of-options", "u"], "");
}
