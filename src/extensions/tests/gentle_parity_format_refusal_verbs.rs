//! The gently set-up verbs in a repository whose format `verify_repository_format()`
//! refuses — a v1-only extension at `core.repositoryFormatVersion = 0`, an unknown
//! extension at version 1, and the same behind a `.git` file.
//!
//! `setup_git_directory_gently()` with a non-`NULL` `nongit_ok` turns the refusal into
//! a `warning:` and `*nongit_ok = -1` (setup.c:771-777), after the walk's chdir to the
//! top of the work tree (setup.c:1957-1959), so the verb runs outside any repository
//! from there. Every `read_early_config()` re-walks and warns again as `ignoring git
//! dir '<dir>': …` (setup.c:1765-1767): `check_pager_config()` in `run_builtin()` for
//! the `RUN_SETUP_GENTLY` entries (git.c:493-495), `setup_auto_pager()` for `config`,
//! `list_aliases()` for `help`. A later `setup_git_directory()` (grep's index read,
//! `hash-object -w`) and `enter_repo()` (upload-pack, receive-pack,
//! upload-archive--writer) have no `nongit_ok` and die with the refusal.
//!
//! The port skipped all of it outside `diff`: `git var GIT_DIR` printed its usage
//! with no warning, `git grep` died `not a git repository`, `git hash-object -w`
//! wrote into the refused repository and `git upload-pack .` advertised its refs.
//!
//! Each case runs stock git and this binary on identical fixtures built by stock git,
//! from the top and from a subdirectory, and compares stdout, stderr and the exit
//! status with the fixture path masked. Skipped when no stock git is available.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[derive(Clone, Copy)]
enum Kind {
    /// `extensions.noop-v1` at version 0: `repo version is 0, but v1-only extension found`.
    V1Only,
    /// `extensions.bogusext` at version 1: `unknown repository extension found`.
    Unknown,
    /// [`Kind::V1Only`] with the git directory behind a `.git` file.
    Gitfile,
}

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, home: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .output()
        .unwrap()
}

/// `a.txt` and `sub/b.txt` committed, a pack of them copied to `top.idx`/`top.pack`
/// and `sub/p.idx`/`sub/p.pack`, `a.txt` then modified, and the format broken.
fn build(stock: &str, home: &Path, work: &Path, kind: Kind) {
    std::fs::create_dir_all(work.join("sub")).unwrap();
    let ok = |args: &[&str]| {
        let out = run(stock, home, work, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    let separate = format!("--separate-git-dir={}.gd", work.display());
    match kind {
        Kind::Gitfile => ok(&["init", "-q", "-b", "main", &separate, "."]),
        _ => ok(&["init", "-q", "-b", "main", "."]),
    }
    std::fs::write(work.join("a.txt"), "hello\n").unwrap();
    std::fs::write(work.join("sub/b.txt"), "world\n").unwrap();
    ok(&["add", "."]);
    ok(&["commit", "-qm", "init"]);
    ok(&["repack", "-adq"]);
    let git_dir = String::from_utf8(run(stock, home, work, &["rev-parse", "--git-dir"]).stdout).unwrap();
    let pack_dir = work.join(git_dir.trim()).join("objects/pack");
    for entry in std::fs::read_dir(&pack_dir).unwrap() {
        let path = entry.unwrap().path();
        let ext = path.extension().unwrap().to_str().unwrap().to_owned();
        if ext == "idx" || ext == "pack" {
            std::fs::copy(&path, work.join(format!("top.{ext}"))).unwrap();
            std::fs::copy(&path, work.join(format!("sub/p.{ext}"))).unwrap();
        }
    }
    std::fs::write(work.join("a.txt"), "hello\nchanged\n").unwrap();
    match kind {
        Kind::V1Only | Kind::Gitfile => {
            ok(&["config", "core.repositoryformatversion", "0"]);
            ok(&["config", "extensions.noop-v1", "true"]);
        }
        Kind::Unknown => {
            ok(&["config", "core.repositoryformatversion", "1"]);
            ok(&["config", "extensions.bogusext", "true"]);
        }
    }
}

fn masked(bytes: &[u8], root: &Path) -> String {
    String::from_utf8_lossy(bytes).replace(&*root.to_string_lossy(), "<ROOT>")
}

/// Every case from the top and from `sub/`, on a fresh fixture per binary.
fn compare(tag: &str, kind: Kind, cases: &[&[&str]]) {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-gentle-fmt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _root = Root(base.clone());
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("gitconfig"), "[user]\n\tname = U\n\temail = u@example.com\n[alias]\n\tst = status\n")
        .unwrap();
    let mut failures = Vec::new();
    for (n, case) in cases.iter().enumerate() {
        for dir in [".", "sub"] {
            let mut seen = Vec::new();
            for (which, bin) in [("stock", stock), ("zvcs", BIN)] {
                // One path for both binaries, rebuilt in between, so every message that
                // names the work tree names the same one.
                let root = base.join(format!("case{n}"));
                let _ = std::fs::remove_dir_all(&root);
                let _ = std::fs::remove_dir_all(base.join(format!("case{n}.gd")));
                build(stock, &home, &root, kind);
                let out = run(bin, &home, &root.join(dir), case);
                seen.push((which, masked(&out.stdout, &base), masked(&out.stderr, &base), out.status.code()));
            }
            if seen[0].1 != seen[1].1 || seen[0].2 != seen[1].2 || seen[0].3 != seen[1].3 {
                failures.push(format!("git {case:?} in {dir}:\n  stock: {:?}\n  zvcs:  {:?}", seen[0], seen[1]));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

const SETUP_GENTLY: &[&[&str]] = &[
    &["var", "GIT_DIR"],
    &["var", "-h"],
    &["-P", "var", "GIT_DIR"],
    &["var", "GIT_EDITOR"],
    &["grep", "hello"],
    &["grep", "--no-index", "hello"],
    &["-P", "grep", "--no-index", "hello"],
    &["apply", "--stat", "/dev/null"],
    &["merge-file", "-p", "a.txt", "a.txt", "a.txt"],
    &["column"],
    &["interpret-trailers", "--parse"],
    &["for-each-repo", "--config=x.y"],
    &["hook", "list", "pre-commit"],
    &["show-index"],
    &["patch-id"],
    &["shortlog"],
    &["bundle", "verify", "x.bundle"],
    &["archive", "HEAD"],
    &["ls-remote"],
    &["ls-remote", "."],
];

const CONFIG_AND_HELP: &[&[&str]] = &[
    &["config", "--list"],
    &["config", "list"],
    &["config", "user.name"],
    &["config", "get", "user.name"],
    &["config", "--get-all", "user.name"],
    &["config", "--local", "--list"],
    &["help", "--aliases-for-completion"],
    &["--list-cmds=alias"],
    &["help", "st"],
];

const SELF_SETUP: &[&[&str]] = &[
    &["check-ref-format", "--branch", "@{-1}"],
    &["check-ref-format", "--branch", "main"],
    &["check-ref-format", "refs/heads/x"],
    &["hash-object", "a.txt"],
    &["hash-object", "-w", "a.txt"],
    &["stripspace", "-s"],
    &["stripspace"],
    &["verify-pack", "p.idx"],
    &["verify-pack", "-v", "top.idx"],
    &["upload-pack", "."],
    &["receive-pack", "."],
    &["upload-archive", "."],
];

#[test]
fn gentle_parity_format_refusal_run_setup_gently_verbs() {
    compare("rsg-v1", Kind::V1Only, SETUP_GENTLY);
    compare("rsg-unknown", Kind::Unknown, SETUP_GENTLY);
}

#[test]
fn gentle_parity_format_refusal_config_and_help_early_config() {
    compare("cfg-v1", Kind::V1Only, CONFIG_AND_HELP);
    compare("cfg-gitfile", Kind::Gitfile, CONFIG_AND_HELP);
}

#[test]
fn gentle_parity_format_refusal_verbs_setting_up_themselves() {
    compare("self-v1", Kind::V1Only, SELF_SETUP);
    compare("self-unknown", Kind::Unknown, SELF_SETUP);
    compare("self-gitfile", Kind::Gitfile, SELF_SETUP);
}
