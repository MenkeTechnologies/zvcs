//! `includeIf` and `include.path` refusals against stock git, for `git config` and the
//! commands that read the configuration (`log`, `status`).
//!
//! * **`hasconfig:remote.*.url:`.** `include_by_remote_url()` gathers the URLs once
//!   with `populate_remote_urls()` (config.c:338-394): the whole sequence, the command
//!   line included, read again with every such include followed, so a URL brought in
//!   by `include.path` or given with `-c` matches, the glob under `WM_PATHNAME`. A
//!   named remote's `url` in a file such an include brings in, directly or through a
//!   nested include, dies in `forbid_remote_url()` (config.c:352-366) whether the
//!   condition matches or not, and a `--list` has printed the entry that asked.
//! * **An `includeIf` whose condition holds** is `handle_path_include()` like
//!   `include.path` (config.c:142-191, :435-447): no value is `missing value for
//!   'include.path'`, a directory is `unable to access`, a `~user` that does not
//!   expand is `could not expand include path`, and the including file's parse ends
//!   on that line. `onbranch:` needs the repository, which the pager check's
//!   `read_early_config()` does not pass (config.c:1687-1715), so a listing prints the
//!   entries ahead of the refusal there and not for `gitdir:`.
//! * **A circular include** dies at `MAX_INCLUDE_DEPTH` only where `git config` reads
//!   the repository's includes: `--no-includes` and `--file` without `--includes` read
//!   past it, `--local` still reaches it (builtin/config.c:1001-1008).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, cwd: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CEILING_DIRECTORIES", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Two identical repositories on `main`, `stock/repo` and `zvcs/repo`, each with an
/// empty commit, made by stock git.
fn fixture(stock: &str, tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-cfg-includeif-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for side in ["stock", "zvcs"] {
        let dir = root.join(side).join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "a"][..],
        ] {
            let (_, err, code) = git(stock, &dir, &root, args);
            assert_eq!(code, 0, "{args:?}: {err}");
        }
    }
    Fixture { root }
}

impl Fixture {
    fn repo(&self, side: &str) -> PathBuf {
        self.root.join(side).join("repo")
    }

    /// Write `name` (relative to each side's repository) on both sides.
    fn write(&self, name: &str, bytes: &str) {
        for side in ["stock", "zvcs"] {
            let path = self.repo(side).join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
    }

    /// Append to `name` on both sides.
    fn append(&self, name: &str, bytes: &str) {
        for side in ["stock", "zvcs"] {
            let path = self.repo(side).join(name);
            let mut text = std::fs::read_to_string(&path).unwrap();
            text.push_str(bytes);
            std::fs::write(path, text).unwrap();
        }
    }

    /// Run each of `runs` on both sides and compare stdout, stderr and the exit code.
    fn compare(&self, stock: &str, runs: &[&[&str]]) {
        for args in runs {
            let expected = git(stock, &self.repo("stock"), &self.root.join("stock"), args);
            let actual = git(ZVCS, &self.repo("zvcs"), &self.root.join("zvcs"), args);
            assert_eq!(actual, expected, "git {args:?}");
        }
    }
}

/// The URLs a `hasconfig:remote.*.url:` condition matches, and `forbid_remote_url()`.
#[test]
fn hasconfig_gathers_every_url_and_forbids_one_it_includes() {
    let Some(stock) = stock_git() else { return };
    let f = fixture(stock, "hasconfig");
    f.write(".git/remote-inc", "[remote \"r\"]\n\turl = https://inc.example/r\n");
    f.write(".git/user-a", "[user]\n\tname = via-include\n");
    f.write(".git/user-b", "[user]\n\tname = via-cmdline\n");
    // A URL from `include.path`, then one only the command line gives; `*` stops at `/`.
    f.append(
        ".git/config",
        "[include]\n\tpath = remote-inc\n\
         [includeIf \"hasconfig:remote.*.url:https://inc.example/*\"]\n\tpath = user-a\n\
         [includeIf \"hasconfig:remote.*.url:https://cmd.example/*\"]\n\tpath = user-b\n\
         [includeIf \"hasconfig:remote.*.url:https://*/r\"]\n\tpath = user-b\n",
    );
    f.compare(
        stock,
        &[
            &["config", "--get-all", "user.name"],
            &["-c", "remote.c.url=https://cmd.example/x", "config", "--get-all", "user.name"],
            &["-c", "remote.c.url=https://cmd.example/x/y", "config", "--get-all", "user.name"],
        ],
    );

    // A remote URL two includes below a condition that does not even match.
    f.write(".git/mid", "[include]\n\tpath = deep\n");
    f.write(".git/deep", "[remote \"q\"]\n\turl = https://deep.example/q\n");
    f.append(".git/config", "[includeIf \"hasconfig:remote.*.url:nomatch\"]\n\tpath = mid\n");
    f.compare(
        stock,
        &[
            &["config", "user.name"],
            &["config", "--list"],
            &["config", "--no-includes", "user.name"],
            &["log", "-1", "--format=%s"],
            &["status", "--short"],
        ],
    );

    // `--file --includes` gathers from that file alone, the entry that asked printed.
    f.write("ff", "[includeIf \"hasconfig:remote.*.url:x\"]\n\tpath = fi\n[remote \"z\"]\n\turl = https://f/x\n");
    f.write("fi", "[remote \"y\"]\n\turl = https://f/y\n");
    f.compare(stock, &[&["config", "--file", "ff", "--includes", "--list"], &["config", "--file", "ff", "--list"]]);
}

/// `handle_path_include()` for an `includeIf` whose condition holds.
#[test]
fn a_true_condition_refuses_its_path_as_include_path_does() {
    let Some(stock) = stock_git() else { return };
    let runs: &[&[&str]] = &[
        &["config", "--list"],
        &["config", "user.name"],
        &["log", "-1", "--format=%an"],
        &["status", "--short"],
    ];
    for (tag, text) in [
        ("gitdir-nonbool", "[includeIf \"gitdir:**/repo/\"]\n\tpath\n[user]\n\tname = after\n"),
        ("onbranch-nonbool", "[includeIf \"onbranch:main\"]\n\tpath\n[user]\n\tname = after\n"),
        ("gitdir-dir", "[includeIf \"gitdir:**/repo/\"]\n\tpath = adir\n[user]\n\tname = after\n"),
        ("onbranch-glob", "[includeIf \"onbranch:ma*\"]\n\tpath = adir\n[user]\n\tname = after\n"),
        ("unexpanded", "[includeIf \"gitdir/i:**/REPO/\"]\n\tpath = ~nosuchuser-zvcs/x\n[user]\n\tname = after\n"),
        ("false", "[includeIf \"onbranch:other\"]\n\tpath\n[includeIf \"bogus:x\"]\n\tpath\n[user]\n\tname = after\n"),
    ] {
        let f = fixture(stock, tag);
        f.write(".git/adir/keep", "");
        f.append(".git/config", text);
        f.compare(stock, runs);
    }
}

/// A circular include under the reads of `git config` that do and do not follow it.
#[test]
fn a_circular_include_dies_only_where_includes_are_read() {
    let Some(stock) = stock_git() else { return };
    let f = fixture(stock, "circular");
    f.write("loop", "[include]\n\tpath = loop\n");
    f.append(".git/config", "[include]\n\tpath = ../loop\n[user]\n\tname = c\n");
    f.compare(
        stock,
        &[
            &["config", "--no-includes", "user.name"],
            &["config", "--file", ".git/config", "user.name"],
            &["config", "--file", ".git/config", "--includes", "user.name"],
            &["config", "--local", "user.name"],
            &["config", "--list", "--includes"],
            &["status", "--short"],
        ],
    );
}
