//! `git branch -c/-m/-d` edit the branch's config section through
//! `repo_config_copy_section()` / `repo_config_rename_section()`
//! (builtin/branch.c:229-236, 697-703), i.e. `repo_config_copy_or_rename_section_in_file()`
//! (config.c:3353): a copy is written right after the section it copies, where
//! zvcs appended it at the end of the file.
//!
//! Deleting a branch also removes the directories between `refs/heads/` and the
//! ref that the deletion leaves empty: `files_transaction_finish()` runs
//! `try_remove_empty_parents(..., REMOVE_EMPTY_PARENTS_REF)` for every deleted
//! ref, packed or loose (refs/files-backend.c:3430-3471). zvcs left
//! `refs/heads/nested/` behind.

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

/// Every directory and file under `.git/refs`, relative and sorted.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, at: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(at).unwrap().flatten() {
            let p = e.path();
            out.push(p.strip_prefix(base).unwrap().display().to_string());
            if p.is_dir() {
                walk(base, &p, out);
            }
        }
    }
    let base = dir.join(".git");
    let mut out = Vec::new();
    walk(&base, &base.join("refs"), &mut out);
    out.sort();
    out
}

/// Build the fixture with stock git, run `script` under `bin`, and return each
/// command's result plus the config file and ref tree it leaves.
fn outcome(bin: &str, stock: &str, dir: &Path, script: &[&[&str]]) -> (Vec<(String, String, i32)>, String, Vec<String>) {
    let home = dir.with_extension("home");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let g = |args: &[&str]| {
        let (_, err, code) = run(stock, dir, &home, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    g(&["init", "-q", "-b", "main", "."]);
    g(&["commit", "-q", "--allow-empty", "-m", "one"]);
    for b in ["tracker", "other", "nested/one", "nested/two/deep"] {
        g(&["branch", b]);
    }
    g(&["config", "branch.tracker.remote", "."]);
    g(&["config", "branch.tracker.merge", "refs/heads/main"]);
    g(&["config", "branch.other.description", "d"]);
    let results = script.iter().map(|args| run(bin, dir, &home, args)).collect();
    let config = std::fs::read_to_string(dir.join(".git/config")).unwrap();
    (results, config, tree(dir))
}

#[test]
fn branch_config_sections_and_ref_directories_match_stock() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-branch-section-dirs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let scripts: [&[&[&str]]; 7] = [
        &[&["branch", "-c", "tracker", "copy"]],
        &[&["branch", "-C", "tracker", "other"]],
        &[&["branch", "-m", "tracker", "renamed"]],
        &[&["branch", "-D", "tracker", "other"]],
        &[&["branch", "-d", "nested/one", "nested/two/deep"]],
        &[&["branch", "-D", "nested/two/deep"]],
        &[&["pack-refs", "--all"], &["branch", "-D", "nested/two/deep", "nested/one"]],
    ];
    for (i, script) in scripts.iter().enumerate() {
        assert_eq!(
            outcome(BIN, stock, &root.join(format!("z{i}")), script),
            outcome(stock, stock, &root.join(format!("s{i}")), script),
            "{script:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
