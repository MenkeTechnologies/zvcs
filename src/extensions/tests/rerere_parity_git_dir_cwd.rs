//! `git rerere` run from inside `.git`, against stock git: there is no work tree to chdir to, so
//! the recorded paths are opened against the cwd, each reports `could not open`, and the command
//! still exits 0.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// `main` and `side` add `rr.txt` differently; the merge stops with rerere recording it.
fn fixture(stock: &str, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let git = |args: &[&str]| run(stock, root, args);
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "rerere.enabled", "true"]);
    std::fs::write(root.join("f"), "a\n").unwrap();
    git(&["add", "f"]);
    git(&["commit", "-qm", "one"]);
    git(&["checkout", "-qb", "side"]);
    std::fs::write(root.join("rr.txt"), "side\n").unwrap();
    git(&["add", "rr.txt"]);
    git(&["commit", "-qm", "side"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(root.join("rr.txt"), "main\n").unwrap();
    git(&["add", "rr.txt"]);
    git(&["commit", "-qm", "main"]);
    git(&["merge", "side"]);
}

fn compare(stock: &str, tag: &str, cwd_in_git_dir: bool, cases: &[&[&str]]) {
    let base = std::env::temp_dir().join(format!("zvcs-rerere-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for args in cases {
        let (s, z) = (base.join("stock"), base.join("zvcs"));
        for root in [&s, &z] {
            let _ = std::fs::remove_dir_all(root);
            fixture(stock, root);
        }
        let (sd, zd) = if cwd_in_git_dir { (s.join(".git"), z.join(".git")) } else { (s.clone(), z.clone()) };
        let want = run(stock, &sd, args);
        if cwd_in_git_dir && args == &["rerere"] {
            assert!(want.1.contains("could not open 'rr.txt'"), "{want:?}");
        }
        let got = run(BIN, &zd, args);
        assert_eq!(got, want, "{args:?}");
        let state = |bin: &str, root: &Path| run(bin, root, &["status", "--porcelain=v2", "--branch"]).0;
        let rr = |root: &Path| {
            let mut names: Vec<String> = walk(&root.join(".git/rr-cache"))
                .into_iter()
                .map(|p| p.strip_prefix(root).unwrap().display().to_string())
                .collect();
            names.sort();
            names
        };
        // Same files recorded, same index state, whichever side ran.
        assert_eq!(rr(&z), rr(&s), "{args:?}");
        assert_eq!(state(BIN, &z).lines().skip(1).collect::<Vec<_>>(), state(stock, &s).lines().skip(1).collect::<Vec<_>>(), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn from_inside_the_git_directory_the_recorded_paths_are_opened_against_the_cwd() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let cases: [&[&str]; 3] = [
        &["rerere"],
        &["rerere", "--no-rerere-autoupdate"],
        &["rerere", "status"],
    ];
    compare(stock, "gitdir", true, &cases);
}
