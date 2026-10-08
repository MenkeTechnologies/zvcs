//! `--keep-unreachable` is only pushed to `pack-objects` inside `if (pack_everything &
//! ALL_INTO_ONE)` (builtin/repack.c). Without `-a`/`-A` the repack is incremental and
//! leaves unreachable objects where they are; this port packed every object in the store
//! whenever `-k` was given, so `repack -k` and `repack -d -k` wrote a pack where stock says
//! `Nothing new to pack.`.

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

/// Two loose commits plus one loose blob nothing references.
fn fixture(bin: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-repack-keepunreach-{tag}-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    for (name, msg) in [("a", "one"), ("b", "two")] {
        std::fs::write(dir.join(name), format!("{name}\n")).unwrap();
        run(bin, &dir, &["add", name]);
        run(bin, &dir, &["commit", "-q", "-m", msg]);
    }
    std::fs::write(dir.join("unref"), "unreferenced\n").unwrap();
    run(bin, &dir, &["hash-object", "-w", "unref"]);
    std::fs::remove_file(dir.join("unref")).unwrap();
    dir
}

/// The object counts `count-objects -v` reports, which do not depend on pack names.
fn counts(bin: &str, dir: &Path) -> Vec<String> {
    run(bin, dir, &["count-objects", "-v"])
        .0
        .lines()
        .filter(|l| l.starts_with("count:") || l.starts_with("in-pack:") || l.starts_with("packs:"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn keep_unreachable_without_all_is_incremental() {
    let Some(stock) = stock_git() else { return };
    let vectors: &[&[&str]] = &[
        &["repack", "-k"],
        &["repack", "-d", "-k"],
        &["repack", "--keep-unreachable", "-d"],
        &["repack", "-a", "-d", "-k"],
        &["repack", "-a", "-k"],
    ];
    for (n, args) in vectors.iter().enumerate() {
        let tag = format!("v{n}");
        let (s, z) = (fixture(stock, &tag), fixture(BIN, &tag));
        // Pack the commits first so the second repack has "nothing new" to say.
        run(stock, &s, &["repack", "-d"]);
        run(BIN, &z, &["repack", "-d"]);
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
        assert_eq!(counts(BIN, &z), counts(stock, &s), "{args:?}: object counts");
        let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
    }
}
