//! `blame` of commits made under `i18n.commitEncoding=UTF-16` or `UTF-16LE`,
//! whose `encoding` header names a form their UTF-8 message bytes are not
//! written in, compared byte for byte with stock git.
//!
//! `get_commit_info()` reads `repo_logmsg_reencode()`'s C string
//! (builtin/blame.c:200-217). iconv reads the stored buffer as UTF-16 pairs: an
//! odd length or an unpaired surrogate fails and leaves the stored bytes, while
//! an even one comes out as byte-swapped or widened text in which
//! `find_commit_subject()` (commit.c:675-691) still finds a `\n\n` and the line
//! after it, where zvcs parsed the buffer as a commit object and fell back to
//! `(<hex>)`. The synthetic working-tree commit's buffer (blame.c:218-235) goes
//! through the same re-coding, so a wide output encoding prints `(unknown)`
//! identities and a `(000…)` summary for it, where zvcs printed `Not Committed
//! Yet`. The log output encoding defaults to the commit encoding, so the plain
//! `blame` is a wide one here.
//!
//! How iconv orders a bare `UTF-16` is the platform's, so that repository's cases
//! run on macOS only; `UTF-16LE` is the same everywhere.

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
        .env("GIT_AUTHOR_NAME", "A U Thor")
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

/// Two repositories built by stock git, `u16` and `u16le`, each committing one
/// line of `f` per message under its commit encoding, and a line left
/// uncommitted in the working tree.
fn fixture(stock: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-blame-invalid-wide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (name, encoding) in [("u16", "UTF-16"), ("u16le", "UTF-16LE")] {
        let work = root.join(name);
        std::fs::create_dir_all(&work).unwrap();
        let run = |args: &[&str]| {
            let mut full = vec!["-c", "maintenance.auto=false"];
            full.extend_from_slice(args);
            let (_, err, rc) = git(stock, &work, &root, &full);
            assert_eq!(rc, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
        };
        run(&["init", "-q", "-b", "main", "."]);
        run(&["config", "i18n.commitEncoding", encoding]);
        let mut content = String::new();
        for (n, msg) in ["x", "xy", "عربي", "subj é", "two\nline subject", "a\u{639}b"].iter().enumerate() {
            content.push_str(&format!("{n}\n"));
            std::fs::write(work.join("f"), &content).unwrap();
            run(&["add", "f"]);
            run(&["commit", "-q", "-m", msg]);
        }
        content.push_str("wt\n");
        std::fs::write(work.join("f"), &content).unwrap();
    }
    Fixture(root)
}

#[test]
fn blame_reads_the_reencoded_buffer_as_stock_does() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture(stock);
    let mut repos = vec!["u16le"];
    if cfg!(target_os = "macos") {
        repos.push("u16");
    }
    // The working tree's lines carry the current time unless the output is wide,
    // so the narrow encodings blame `HEAD`.
    let cases: [&[&str]; 8] = [
        &["blame", "f"],
        &["blame", "--line-porcelain", "f"],
        &["blame", "--encoding=UTF-32BE", "-t", "-e", "f"],
        &["blame", "--encoding=UTF-16LE", "--porcelain", "HEAD", "--", "f"],
        &["blame", "--encoding=UTF-8", "-p", "HEAD", "--", "f"],
        &["blame", "--encoding=UTF-8", "HEAD", "--", "f"],
        &["blame", "--encoding=none", "--line-porcelain", "HEAD", "--", "f"],
        &["blame", "--encoding=ISO-8859-1", "--incremental", "HEAD", "--", "f"],
    ];
    let mut diverged = Vec::new();
    for repo in repos {
        let dir = fx.0.join(repo);
        for args in cases {
            let want = git(stock, &dir, &fx.0, args);
            let got = git(BIN, &dir, &fx.0, args);
            if want != got {
                diverged.push(format!(
                    "{repo}: {args:?}\n  stock rc={} out={:02x?} err={:?}\n  zvcs  rc={} out={:02x?} err={:?}",
                    want.2,
                    want.0,
                    String::from_utf8_lossy(&want.1),
                    got.2,
                    got.0,
                    String::from_utf8_lossy(&got.1),
                ));
            }
        }
    }
    assert!(diverged.is_empty(), "{}", diverged.join("\n"));
}
