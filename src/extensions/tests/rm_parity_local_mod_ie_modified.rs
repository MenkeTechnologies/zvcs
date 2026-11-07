//! `git rm`'s up-to-date check is `ie_match_stat()` on the index `refresh_index()`
//! left behind (builtin/rm.c:142-195, :363-364), not a raw hash of the file.
//!
//! * A path gone from the worktree, or a file that became a directory, is skipped
//!   before either kind of change is asked about, staged content and all.
//! * A type change, or an owner-x-bit change under `core.fileMode`, is a local
//!   change whatever the bytes say (`ce_match_stat_basic()`, read-cache.c:311-352).
//! * The content comparison is `ce_compare_data()`, which hashes the file through
//!   `convert_to_git()` (read-cache.c:231-245), so a clean filter or EOL
//!   normalisation is not a modification; a symlink compares its target.
//! * The staged comparison includes the mode: `ce->ce_mode != create_ce_mode(mode)`
//!   (builtin/rm.c:191-195).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
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
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn sh(dir: &Path, script: &str) {
    let ok = Command::new("/bin/sh").arg("-c").arg(script).current_dir(dir).status().unwrap();
    assert!(ok.success(), "{script}");
}

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-rmlocal-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    run(stock, &dir, &["config", "filter.rot.clean", "tr a-z n-za-m"]);
    run(stock, &dir, &["config", "filter.rot.smudge", "tr a-z n-za-m"]);
    std::fs::write(dir.join(".gitattributes"), "*.f filter=rot\n*.crlf text eol=crlf\n").unwrap();
    std::fs::write(dir.join("plain"), "x\n").unwrap();
    std::fs::write(dir.join("r.f"), "abc\n").unwrap();
    std::fs::write(dir.join("w.crlf"), "a\nb\n").unwrap();
    sh(&dir, "ln -s plain lnk");
    run(stock, &dir, &["add", "."]);
    assert_eq!(run(stock, &dir, &["commit", "-qm", "one"]).2, Some(0));
    // Push the index's timestamp past every file's mtime so no entry is racy.
    sh(&dir, "sleep 1");
    run(stock, &dir, &["update-index", "--refresh", "-q"]);
    dir
}

#[test]
fn local_and_staged_changes_follow_ie_match_stat() {
    let Some(stock) = stock_git() else { return };
    let cases: &[(&str, &[&str])] = &[
        ("chmod +x plain", &["rm", "plain"]),
        ("chmod +x plain", &["rm", "--cached", "plain"]),
        ("chmod +x plain", &["-c", "core.fileMode=false", "rm", "plain"]),
        ("", &["update-index", "--chmod=+x", "plain"]),
        ("", &["rm", "r.f"]),
        ("", &["rm", "w.crlf"]),
        ("printf 'abd\\n' > r.f", &["rm", "r.f"]),
        ("rm lnk && echo plain > lnk", &["rm", "lnk"]),
        ("rm lnk && ln -s other lnk", &["rm", "lnk"]),
        ("rm lnk && echo plain > lnk", &["-c", "core.symlinks=false", "rm", "lnk"]),
        ("echo y > plain && git add plain && rm plain", &["rm", "plain"]),
        ("echo y > plain && git add plain && rm plain && mkdir plain", &["rm", "plain"]),
        ("echo yy > plain", &["rm", "plain"]),
        ("touch -t 202001010000 plain", &["rm", "plain"]),
    ];
    for (setup, args) in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            let setup = setup.replace("git ", &format!("{stock} "));
            if !setup.is_empty() {
                sh(&dir, &setup);
            }
            // The `update-index --chmod` row stages a mode change, then removes it.
            let result = if args[0] == "update-index" {
                run(stock, &dir, args);
                run(bin, &dir, &["rm", "plain"])
            } else {
                run(bin, &dir, args)
            };
            let status = run(stock, &dir, &["status", "--porcelain=v2"]);
            let files = run(stock, &dir, &["ls-files", "-s"]);
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((result, status, files));
        }
        assert_eq!(seen[1], seen[0], "setup {setup:?} args {args:?}");
    }
}
