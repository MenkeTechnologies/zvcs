//! `%(upstream)` and `%(push)` atoms that never read the branch tip.
//!
//! Only `:track` and `:trackshort` measure ahead/behind, which needs the tip's commit; `:short`
//! needs the abbreviation machinery. The plain atom, `:lstrip`/`:rstrip`, `:remotename` and
//! `:remoteref` are answered from configuration and ref names alone, so a repository setting git
//! refuses on first object access (`feature.manyFiles=input`) does not stop them. zvcs peeled
//! the tip before looking at which option was asked for.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, bad: bool, args: &[&str]) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("HOME", dir)
        .env("LC_ALL", "C");
    if bad {
        cmd.env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "feature.manyFiles")
            .env("GIT_CONFIG_VALUE_0", "input");
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-feruptr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, false, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    run(stock, &dir, false, &["add", "."]);
    run(stock, &dir, false, &["commit", "-qm", "one"]);
    run(stock, &dir, false, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    run(stock, &dir, false, &["config", "remote.origin.url", "/nonexistent"]);
    run(stock, &dir, false, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
    run(stock, &dir, false, &["config", "branch.main.remote", "origin"]);
    run(stock, &dir, false, &["config", "branch.main.merge", "refs/heads/main"]);
    dir
}

#[test]
fn only_the_options_that_need_objects_die_on_the_repository_setting() {
    let Some(stock) = stock_git() else { return };
    let atoms = [
        "%(upstream)",
        "%(push)",
        "%(upstream:lstrip=2)",
        "%(upstream:rstrip=1)",
        "%(upstream:remotename)",
        "%(upstream:remoteref)",
        "%(push:remotename)",
        "%(upstream:short)",
        "%(upstream:track)",
        "%(upstream:trackshort)",
        "%(push:track)",
    ];
    for bad in [true, false] {
        for atom in atoms {
            let format = format!("--format={atom}");
            let mut seen = Vec::new();
            for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
                let dir = fixture(who, stock);
                let result = run(bin, &dir, bad, &["for-each-ref", &format, "refs/heads"]);
                let _ = std::fs::remove_dir_all(&dir);
                seen.push(result);
            }
            assert_eq!(seen[1], seen[0], "bad setting {bad}: {atom}");
        }
    }
}
