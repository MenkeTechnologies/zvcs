//! `gc` runs `reflog expire --all` as a child (`gc_foreground_tasks()`, builtin/gc.c). The
//! child reads `gc.reflogExpire` / `gc.reflogExpireUnreachable` through `git_config_expiry_date()`
//! and dies on a value that is not a timestamp (`'always' for 'gc.reflogexpire…' is not a valid
//! timestamp`); `gc` reports `error: failed to run reflog` and goes on with the other tasks
//! instead of ending, so the repack still happens and the exit status stays 0.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, global: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn fixture(bin: &str, tag: &str, global: &Path) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-gc-reflogcfg-{tag}-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run(bin, &dir, global, &["init", "-q", "-b", "main", "."]);
    for (name, msg) in [("a", "one"), ("b", "two")] {
        std::fs::write(dir.join(name), format!("{name}\n")).unwrap();
        run(bin, &dir, global, &["add", name]);
        run(bin, &dir, global, &["commit", "-q", "-m", msg]);
    }
    dir
}

fn packs(bin: &str, dir: &Path, global: &Path) -> String {
    let out = run(bin, dir, global, &["count-objects", "-v"]).0;
    out.lines()
        .filter(|l| l.starts_with("count:") || l.starts_with("in-pack:") || l.starts_with("packs:"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn a_reflog_child_that_dies_does_not_end_gc() {
    let Some(stock) = stock_git() else { return };
    let cfg_dir = std::env::temp_dir().join(format!("zvcs-gc-reflogcfg-cfg-{}", std::process::id()));
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let cases = [
        ("always", "[gc]\n\treflogExpireUnreachable = \"-0\"\n[gc]\n\treflogExpireUnreachable = \"always\"\n"),
        ("bare", "[gc]\n\treflogExpire = bogus\n"),
        ("pattern", "[gc \"refs/heads/*\"]\n\treflogExpire = nonsense\n"),
        ("valid", "[gc]\n\treflogExpire = 2.days.ago\n\treflogExpireUnreachable = never\n"),
    ];
    for (tag, text) in cases {
        let global = cfg_dir.join(format!("{tag}.cfg"));
        std::fs::write(&global, text).unwrap();
        let (s, z) = (fixture(stock, tag, &global), fixture(BIN, tag, &global));
        let want = run(stock, &s, &global, &["gc", "--prune=now"]);
        let got = run(BIN, &z, &global, &["gc", "--prune=now"]);
        // The config file path is the same for both runs.
        assert_eq!(got, want, "{tag}");
        assert_eq!(packs(BIN, &z, &global), packs(stock, &s, &global), "{tag}: store after gc");
        let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
    }
    let _ = std::fs::remove_dir_all(cfg_dir);
}
