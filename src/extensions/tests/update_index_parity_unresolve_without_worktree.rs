//! `update-index --unresolve` swallows the rest of the command line as paths and runs
//! `prefix_path()` over each without ever calling `setup_work_tree()`, so from inside the
//! git directory (no work tree) a relative path is merely normalised, and one that climbs
//! out of the root or is absolute is `'<path>' is outside repository at '<git dir>'`.
//! zvcs refused every path with `this operation must be run in a work tree`.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn fixture(bin: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("zvcs-uiunres-{}-{}", std::process::id(), if bin == BIN { "zvcs" } else { "stock" }));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    run(bin, &dir, &["add", "a"]);
    run(bin, &dir, &["commit", "-q", "-m", "one"]);
    dir
}

/// The git directory's own absolute path differs per fixture; name it generically.
fn scrub(r: (String, String, i32), dir: &Path) -> (String, String, i32) {
    (r.0.replace(dir.to_str().unwrap(), "<R>"), r.1.replace(dir.to_str().unwrap(), "<R>"), r.2)
}

#[test]
fn unresolve_paths_are_normalised_without_a_work_tree() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock), fixture(BIN));
    let vectors: &[&[&str]] = &[
        &["update-index", "--unresolve", "foo"],
        &["update-index", "--unresolve", "--info-only"],
        &["update-index", "--unresolve", "a/../b", "c//d", "./e"],
        &["update-index", "--unresolve", "../x"],
        &["update-index", "--unresolve", "a/../../x"],
        &["update-index", "--unresolve", "/abs/x"],
        &["update-index", "--unresolve", "ok", "../x"],
    ];
    for args in vectors {
        let want = scrub(run(stock, &s.join(".git/refs/heads"), args), &s);
        let got = scrub(run(BIN, &z.join(".git/refs/heads"), args), &z);
        assert_eq!(got, want, "{args:?}");
    }
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
