//! `git branch --unset-upstream` unsets `branch.<name>.remote` and
//! `branch.<name>.merge` with `repo_config_set_multivar(..., NULL, NULL,
//! CONFIG_FLAGS_MULTI_REPLACE)` (builtin/branch.c:1283-1288), the store
//! `git config --unset-all` uses, so a `[branch "<name>"]` section the two keys
//! leave empty is removed with them (`maybe_remove_section()`, config.c:2811).
//! zvcs removed the keys through gitoxide's section editor and left an empty
//! header behind.
//!
//! The upstream check is `branch_has_merge_config()`, true only when `set_merge()`
//! saw both a remote and a merge entry (remote.c:1786-1794): a merge entry alone
//! is "has no upstream information". And `HEAD` names the current branch
//! (`branch_get("HEAD")`, remote.c:1828-1829).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
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

/// A repository whose config ends with `[branch "<b>"]` entries `setup` writes,
/// then `git branch --unset-upstream <operand>` under `bin`: its output and the
/// config file it leaves.
fn outcome(bin: &str, stock: &str, root: &Path, tag: &str, setup: &[&[&str]], operand: &[&str]) -> ((String, String, i32), String) {
    let dir = root.join(tag);
    let home = root.join("home");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let g = |args: &[&str]| {
        let (_, err, code) = run(stock, &dir, &home, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    g(&["init", "-q", "-b", "main", "."]);
    g(&["commit", "-q", "--allow-empty", "-m", "one"]);
    g(&["branch", "side"]);
    for args in setup {
        g(args);
    }
    let mut args = vec!["branch", "--unset-upstream"];
    args.extend_from_slice(operand);
    let out = run(bin, &dir, &home, &args);
    let config = std::fs::read_to_string(dir.join(".git/config")).unwrap();
    (out, config)
}

#[test]
fn unset_upstream_matches_stock() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-branch-unset-upstream-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let both: &[&[&str]] = &[
        &["config", "branch.side.remote", "."],
        &["config", "branch.side.merge", "refs/heads/main"],
    ];
    let cases: [(&[&[&str]], &[&str]); 6] = [
        // The section holds only the two keys: it goes.
        (both, &["side"]),
        // Another key keeps it.
        (
            &[
                &["config", "branch.side.remote", "."],
                &["config", "branch.side.description", "d"],
                &["config", "branch.side.merge", "refs/heads/main"],
            ],
            &["side"],
        ),
        // Several merge entries go together.
        (
            &[
                &["config", "branch.side.remote", "."],
                &["config", "branch.side.merge", "refs/heads/main"],
                &["config", "--add", "branch.side.merge", "refs/heads/x"],
            ],
            &["side"],
        ),
        // A merge entry without a remote is no upstream.
        (&[&["config", "branch.side.merge", "refs/heads/main"]], &["side"]),
        // A remote without a merge entry is none either.
        (&[&["config", "branch.side.remote", "origin"]], &["side"]),
        // `HEAD` is the current branch.
        (
            &[
                &["config", "branch.main.remote", "."],
                &["config", "branch.main.merge", "refs/heads/side"],
            ],
            &["HEAD"],
        ),
    ];
    for (i, (setup, operand)) in cases.iter().enumerate() {
        assert_eq!(
            outcome(BIN, stock, &root, &format!("z{i}"), setup, operand),
            outcome(stock, stock, &root, &format!("s{i}"), setup, operand),
            "case {i}: {setup:?} {operand:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
