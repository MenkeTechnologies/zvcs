//! An `includeIf` in the system, XDG or user configuration whose condition needs
//! the repository, against stock git.
//!
//! `read_very_early_config()` reads those files with no `git_dir` and no
//! `inc->repo` (config.c:1717-1728), so `gitdir:`, `worktree:` and `onbranch:`
//! never hold there. Once the repository is set up, `repo_read_config()` and
//! `read_early_config()` hand `config_with_options()` the whole sequence again
//! (config.c:1687-1711, :2299-2315), and `git_config_include()` (config.c:419-447)
//! follows or refuses the include where it sits in the global file: a valueless
//! `path` is `missing value for 'include.path'` and `bad config line <n> in file
//! <global>`. Outside a repository, and for `git version`, nothing holds.
//!
//! zvcs evaluated those conditions in the very early read only, so inside a
//! repository it read past them.
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

/// Run `args` in `cwd` with the user file `global` and, when given, the system
/// file `system`.
fn git(bin: &str, cwd: &Path, home: &Path, global: &Path, system: Option<&Path>, args: &[&str]) -> (String, String, i32) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CEILING_DIRECTORIES", home)
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TERM", "dumb");
    match system {
        Some(path) => cmd.env("GIT_CONFIG_SYSTEM", path),
        None => cmd.env("GIT_CONFIG_NOSYSTEM", "1"),
    };
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// `stock/repo`, `zvcs/repo` on `main` with one empty commit, a directory outside
/// any repository per side, and the configuration files both sides share, so the
/// diagnostics name the same path.
fn fixture(stock: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-cfg-global-includeif-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let empty = root.join("empty");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&empty, "").unwrap();
    for side in ["stock", "zvcs"] {
        let dir = root.join(side).join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(root.join(side).join("out")).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "a"][..],
        ] {
            let (_, err, code) = git(stock, &dir, &root, &empty, None, args);
            assert_eq!(code, 0, "{args:?}: {err}");
        }
    }
    Fixture { root }
}

#[test]
fn a_repository_condition_in_a_global_file_is_taken_once_the_repository_is_set_up() {
    let Some(stock) = stock_git() else { return };
    let f = fixture(stock);
    let file = f.root.join("conf");
    let empty = f.root.join("empty");
    let runs: &[&[&str]] = &[
        &["config", "--list"],
        &["config", "user.name"],
        &["log", "-1", "--format=%s"],
        &["status", "--short"],
        &["version"],
    ];
    for text in [
        "[user]\n\tname = U\n[includeIf \"onbranch:main\"]\n\tpath\n",
        "[includeIf \"gitdir:**/repo/\"]\n\tpath\n[user]\n\tname = V\n",
        "[includeIf \"onbranch:m*\"]\n\tpath = .\n",
        "[includeIf \"onbranch:other\"]\n\tpath\n[user]\n\tname = W\n",
    ] {
        std::fs::write(&file, text).unwrap();
        // As the user file, then as the system file.
        for (global, system) in [(&file, None), (&empty, Some(file.as_path()))] {
            for place in ["repo", "out"] {
                for args in runs {
                    let run = |bin: &str, side: &str| {
                        let home = f.root.join(side);
                        git(bin, &home.join(place), &home, global, system, args)
                    };
                    let (mut actual, mut expected) = (run(ZVCS, "zvcs"), run(stock, "stock"));
                    // `git version` names the build; only whether it got that far counts.
                    if args[0] == "version" {
                        actual.0.clear();
                        expected.0.clear();
                    }
                    assert_eq!(actual, expected, "{text:?} {system:?} {place} {args:?}");
                }
            }
        }
    }
}
