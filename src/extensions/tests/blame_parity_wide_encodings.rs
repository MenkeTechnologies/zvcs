//! Output in UTF-16 and UTF-32, which `reencode_string_len()` gets from iconv
//! (utf8.c:566-623) and `encoding_rs` cannot produce, compared with stock git.
//!
//! `repo_logmsg_reencode()` returns a bare `char *`, so blame's `get_ac_line()`
//! and `find_commit_subject()` read the re-coded commit as the C string that
//! ends at its first NUL: every identity is `(unknown)`, the time 0, the tz
//! string `(unknown)` and the summary `(<hex>)` (builtin/blame.c:131-217). The
//! paths that convert a finished record — `rev-list --pretty`, `shortlog` and
//! `commit`'s summary line — print it in the wide form. zvcs printed the stored
//! UTF-8 bytes for all of them.
//!
//! The byte order of a bare `UTF-16`/`UTF-32` is the platform iconv's, so the
//! cases name `LE`/`BE` forms, which are the same everywhere, and git's own
//! `UTF-16LE-BOM` (utf8.c:588-591).

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

fn git(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (Vec<u8>, Vec<u8>, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_AUTHOR_NAME", "Jürgen Ü")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0100")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0100")
        .env("GIT_EDITOR", "true")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (out.stdout, out.stderr, out.status.code().expect("no signal"))
}

/// One repository per side, each built by stock git: two commits, one with an
/// astral character in its subject so the UTF-16 forms need a surrogate pair.
fn fixture(stock: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-blame-wide-encodings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for side in ["stock", "zvcs"] {
        let work = root.join(side);
        std::fs::create_dir_all(&work).unwrap();
        let run = |args: &[&str]| {
            let (_, err, rc) = git(stock, &work, &root, args);
            assert_eq!(rc, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
        };
        run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(work.join("f"), "a\n").unwrap();
        run(&["add", "f"]);
        run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "één 😀"]);
        std::fs::write(work.join("f"), "a\nb\n").unwrap();
    }
    Fixture(root)
}

#[test]
fn wide_output_encodings_match_stock() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture(stock);
    let mut cases: Vec<Vec<&str>> = vec![
        vec!["blame", "--encoding=UTF-16LE", "HEAD", "f"],
        vec!["blame", "--encoding=utf16be", "--porcelain", "HEAD", "f"],
        vec!["blame", "--encoding=UTF-32BE", "-t", "-c", "HEAD", "f"],
        vec!["blame", "--encoding=UTF-16LE-BOM", "-t", "-e", "HEAD", "f"],
        vec!["-c", "i18n.logOutputEncoding=UTF-32LE", "blame", "--incremental", "HEAD", "f"],
        vec!["rev-list", "--encoding=UTF-16BE", "--pretty", "HEAD"],
        vec!["-c", "i18n.logOutputEncoding=UTF-16LE", "shortlog", "HEAD"],
        // `print_commit_summary()`'s user format, converted as a whole record.
        vec![
            "-c",
            "maintenance.auto=false",
            "-c",
            "i18n.logOutputEncoding=UTF-16BE",
            "commit",
            "-q",
            "--allow-empty",
            "--author=Ä A <a@example.com>",
            "-m",
            "x",
        ],
        vec!["-c", "maintenance.auto=false", "-c", "i18n.logOutputEncoding=UTF-32LE", "commit", "-a", "-m", "zwei é"],
    ];
    // macOS's libiconv writes a bare `UTF-16` big-endian behind `FE FF`.
    if cfg!(target_os = "macos") {
        cases.push(vec!["blame", "--encoding=UTF-16", "HEAD", "f"]);
        cases.push(vec!["rev-list", "--encoding=UTF-32", "--pretty=oneline", "HEAD"]);
    }
    let mut diverged = Vec::new();
    for args in &cases {
        let want = git(stock, &fx.0.join("stock"), &fx.0, args);
        let got = git(BIN, &fx.0.join("zvcs"), &fx.0, args);
        if want != got {
            diverged.push(format!(
                "{args:?}\n  stock rc={} out={:02x?} err={:?}\n  zvcs  rc={} out={:02x?} err={:?}",
                want.2,
                want.0,
                String::from_utf8_lossy(&want.1),
                got.2,
                got.0,
                String::from_utf8_lossy(&got.1),
            ));
        }
    }
    assert!(diverged.is_empty(), "{}", diverged.join("\n"));
}
