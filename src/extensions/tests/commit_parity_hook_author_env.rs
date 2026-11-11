//! The author identity `git commit` exports to its hooks, compared with stock
//! git.
//!
//! `determine_author_info()` ends with three `export_one()` calls
//! (builtin/commit.c:603-611, :686-688): `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`
//! and `GIT_AUTHOR_DATE` (as `@<seconds> <tz>`) are set to the ident the commit
//! records, before `pre-commit` runs, so `pre-commit`, `prepare-commit-msg`,
//! `commit-msg` and `post-commit` see `--author`, `--date`, an amended commit's
//! author and the configured identity. zvcs left the caller's environment as it
//! was.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const HOOKS: [&str; 4] = ["pre-commit", "prepare-commit-msg", "commit-msg", "post-commit"];

fn git(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (Vec<u8>, Vec<u8>, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        // No author name or address in the environment: the configured one is
        // what the plain commit exports.
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2023-11-14 22:13:20 +0100")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (out.stdout, out.stderr, out.status.code().expect("no signal"))
}

/// The same commits made by `bin`, each hook appending what it saw to
/// `hooks.log` beside the repository; returns that log.
fn run_side(bin: &str, stock: &str, root: &Path, side: &str) -> String {
    let work = root.join(side);
    std::fs::create_dir_all(&work).unwrap();
    let log = root.join(format!("{side}.log"));
    let setup = |args: &[&str]| {
        let (_, err, rc) = git(stock, &work, root, args);
        assert_eq!(rc, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
    };
    setup(&["init", "-q", "-b", "main", "."]);
    setup(&["config", "user.name", "Cfg User"]);
    setup(&["config", "user.email", "cfg@example.com"]);
    setup(&["config", "maintenance.auto", "false"]);
    for hook in HOOKS {
        let path = work.join(".git/hooks").join(hook);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"== {hook}\" >>'{log}'\nenv | grep '^GIT_AUTHOR_' | sort >>'{log}'\n",
                log = log.display()
            ),
        )
        .unwrap();
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        std::fs::set_permissions(&path, perm).unwrap();
    }
    let commits: [&[&str]; 4] = [
        &["commit", "-q", "--allow-empty", "-m", "plain"],
        &["commit", "-q", "--allow-empty", "--author=Forced Name <forced@example.com>", "--date=@1600000000 +0200", "-m", "forced"],
        &["commit", "-q", "--amend", "--allow-empty", "-m", "amended"],
        &["commit", "-q", "--amend", "--allow-empty", "--reset-author", "-m", "reset"],
    ];
    for args in commits {
        std::fs::write(&log, [std::fs::read(&log).unwrap_or_default(), format!("-- {args:?}\n").into_bytes()].concat())
            .unwrap();
        let (_, err, rc) = git(bin, &work, root, args);
        assert_eq!(rc, 0, "{side} {args:?}: {}", String::from_utf8_lossy(&err));
    }
    std::fs::read_to_string(&log).unwrap()
}

#[test]
fn commit_hooks_see_the_exported_author() {
    let Some(stock) = stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-commit-hook-author-env-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let fx = Fixture(root);
    let want = run_side(stock, stock, &fx.0, "stock");
    let got = run_side(BIN, stock, &fx.0, "zvcs");
    assert_eq!(got, want);
}
