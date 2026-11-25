//! `git ls-remote --sort=<key>` with any `for-each-ref` atom, run by stock git
//! and by zvcs on twin copies of one fixture at the same path.
//!
//! * `cmd_ls_remote()` pushes every advertised ref into a `ref_array` and sorts
//!   it with ref-filter's engine (builtin/ls-remote.c:150-164), so a key is any
//!   atom `parse_ref_filter_atom()` accepts: an unknown one is `unknown field
//!   name`, and one that reads the object dies `missing object <oid> for <ref>`
//!   on the first ref `git_qsort_s()` compares whose object is not local.
//! * The items carry no `kind`, `flag` or ahead-behind counts, so `%(flag)`,
//!   `%(worktreepath)` and `%(ahead-behind:…)` are empty for every ref, while
//!   `%(HEAD)` and `%(upstream)` read the local repository.
//! * Without `--sort` nothing is sorted: the advertisement order stands, each
//!   `<tag>^{}` right behind its tag.
//! * Outside a repository an atom that needs object data is refused before its
//!   parser runs (ref-filter.c:1074-1077).
//!
//! No network: the remote is a directory beside the clone.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .output()
        .expect("spawn git")
}

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str]) {
    let out = run(bin, dir, home, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `up` (main and feat; annotated `a1` and lightweight `a1.1` and `v1`, so the
/// advertisement has `a1^{}` ahead of `a1.1`) cloned into `cl`, then advanced
/// on main so the clone lacks that commit.
fn base(stock: &str, root: &Path, home: &Path) -> PathBuf {
    let base = root.join("base");
    let up = base.join("up");
    std::fs::create_dir_all(&up).unwrap();
    let g = |dir: &Path, args: &[&str]| ok(stock, dir, home, args);
    g(&up, &["init", "-q", "-b", "main"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "zeta"]);
    g(&up, &["branch", "feat"]);
    g(&up, &["tag", "-a", "-m", "ann", "a1"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "alpha"]);
    g(&up, &["tag", "a1.1"]);
    g(&up, &["tag", "v1", "HEAD^{tree}"]);
    g(&base, &["clone", "-q", "up", "cl"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "beta"]);
    base
}

/// Runs `cases` (`sh -c` scripts with `$G` naming the binary) from `cl` under
/// both binaries. `ls-remote` writes nothing, so one fixture serves every run.
fn compare(tag: &str, cases: &[&str]) {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-lsremote-atoms-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _scratch = Scratch(root.clone());
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".gitconfig"), "[maintenance]\n\tauto = false\n[gc]\n\tauto = 0\n").unwrap();
    let cl = base(stock, &root, &home).join("cl");
    for case in cases {
        let mut seen = Vec::new();
        for bin in [stock, BIN] {
            let out = Command::new("sh")
                .arg("-c")
                .arg(case)
                .current_dir(&cl)
                .env("G", bin)
                .env("HOME", &home)
                .env("GIT_CEILING_DIRECTORIES", &root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
                .env_remove("XDG_CONFIG_HOME")
                .env("GIT_EDITOR", "true")
                .env("LC_ALL", "C")
                .env("TERM", "dumb")
                .output()
                .unwrap();
            seen.push(format!(
                "rc={:?}\n--stdout\n{}--stderr\n{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            ));
        }
        assert_eq!(seen[1], seen[0], "case {case:?}: zvcs (left) vs stock (right)");
    }
}

#[test]
fn any_atom_sorts_and_object_atoms_need_the_object() {
    compare(
        "atoms",
        &[
            // Every object is local in `up` itself.
            "cd ../up && $G ls-remote --sort=-objectsize --sort=subject --sort='*objectname' .",
            // The clone lacks the new main, which `HEAD` and `refs/heads/main` name.
            "$G ls-remote --sort=objectsize origin",
            // Atoms answered from the item and the local repository alone.
            "$G ls-remote --sort=-flag --sort=ahead-behind:HEAD --sort=HEAD --sort=upstream origin",
            "$G ls-remote --sort=refname:short --sort=-v:creatordate origin feat 'a1*'",
            // The first rejected key on the command line is reported.
            "$G ls-remote --sort=bogus --sort=objectsize origin",
        ],
    );
}

#[test]
fn advertisement_order_and_outside_a_repository() {
    compare(
        "order",
        &[
            "$G ls-remote origin 'a1*'",
            "cd .. && $G ls-remote --sort=objectname up",
            "cd .. && $G ls-remote --sort=-symref --sort=refname:lstrip=2 up",
        ],
    );
}
