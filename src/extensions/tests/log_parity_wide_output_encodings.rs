//! `log`, `show`, `rev-list`, `format-patch` and `range-diff` under a UTF-16 or
//! UTF-32 output encoding, compared byte for byte with stock git.
//!
//! `repo_logmsg_reencode()` returns a bare `char *`, and pretty.c reads it as a C
//! string: `pp_header()`, `pp_title_line()` and `pp_remainder()` stop at the
//! first NUL (pretty.c:227-240, :2053-2107), so a built-in format prints no
//! `Author:`, no subject and no body, and a `Merge:` line only when the stub
//! before the NUL is a header line of its own. A user format is expanded in
//! UTF-8 and converted as a whole record (pretty.c:2026-2046), which `--log-size`
//! measures and `--graph` lays out with `strchr()` (graph.c:1948-1973). Notes are
//! converted on their own and cut at their first NUL (notes.c:1305-1312).
//! format-patch writes no `From:`/`Date:` for a commit whose re-coded buffer
//! loses its `author` line, its cover letter groups the `%aN`/`%s` records cut
//! the same way (builtin/shortlog.c:59-85, :241-266), and range-diff's pair header
//! loses the space in front of an empty subject to `strbuf_rtrim()`
//! (pretty.c:2363). zvcs refused all of these encodings.
//!
//! A commit whose `encoding` header says UTF-16 over UTF-8 bytes re-codes into
//! text with no header lines at all, so `%an` and `%s` are empty while `%T` is
//! still the parsed tree (pretty.c:1578-1585), and the cover letter groups it
//! under a literal `%aN` (pretty.c:856-865, :2021-2022).
//!
//! The byte order of a bare `UTF-16`/`UTF-32` is the platform iconv's, so most
//! cases name `LE`/`BE` forms, which are the same everywhere; the bare forms run
//! on macOS only, where libiconv writes `FE FF` with the first character and
//! nothing for an empty record.

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

/// Per side, two repositories built by stock git. `main`: a root commit, a
/// second commit and a side branch merged back, a note on the second commit,
/// and `re`, the second commit reworded on top of the root plus one more.
/// `c16`: two commits made under `i18n.commitEncoding=UTF-16` with UTF-8 bytes.
fn fixture(stock: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-log-wide-encodings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for side in ["stock", "zvcs"] {
        let work = root.join(side).join("main");
        std::fs::create_dir_all(&work).unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let mut full = vec!["-c", "maintenance.auto=false"];
            full.extend_from_slice(args);
            let (_, err, rc) = git(stock, dir, &root, &full);
            assert_eq!(rc, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
        };
        run(&work, &["init", "-q", "-b", "main", "."]);
        std::fs::write(work.join("f"), "a\n").unwrap();
        run(&work, &["add", "f"]);
        run(&work, &["commit", "-q", "-m", "first subject", "-m", "body line"]);
        std::fs::write(work.join("f"), "a\nb\n").unwrap();
        run(&work, &["commit", "-q", "-a", "-m", "second"]);
        run(&work, &["checkout", "-q", "-b", "side", "HEAD~1"]);
        std::fs::write(work.join("s"), "s\n").unwrap();
        run(&work, &["add", "s"]);
        run(&work, &["commit", "-q", "-m", "side café"]);
        run(&work, &["checkout", "-q", "-b", "re", "main~1"]);
        std::fs::write(work.join("f"), "a\nb\n").unwrap();
        run(&work, &["commit", "-q", "-a", "-m", "second reworded"]);
        std::fs::write(work.join("f"), "a\nb\nc\n").unwrap();
        run(&work, &["commit", "-q", "-a", "-m", "third"]);
        run(&work, &["checkout", "-q", "main"]);
        run(&work, &["merge", "-q", "--no-edit", "side"]);
        run(&work, &["notes", "add", "-m", "note é", "HEAD~1"]);

        let c16 = root.join(side).join("c16");
        std::fs::create_dir_all(&c16).unwrap();
        run(&c16, &["init", "-q", "-b", "main", "."]);
        run(&c16, &["config", "i18n.commitEncoding", "UTF-16"]);
        std::fs::write(c16.join("f"), "a\n").unwrap();
        run(&c16, &["add", "f"]);
        run(&c16, &["commit", "-q", "-m", "first subject", "-m", "body"]);
        std::fs::write(c16.join("f"), "a\nb\n").unwrap();
        run(&c16, &["commit", "-q", "-a", "-m", "secönd x"]);
    }
    Fixture(root)
}

#[test]
fn wide_output_encodings_match_stock() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture(stock);
    let mut cases: Vec<(&str, Vec<&str>)> = vec![
        ("main", vec!["log", "--encoding=UTF-16LE"]),
        ("main", vec!["log", "--encoding=UTF-16BE-BOM", "--pretty=fuller", "--parents"]),
        ("main", vec!["log", "--encoding=UTF-32BE", "--pretty=raw", "--notes"]),
        ("main", vec!["log", "--encoding=UTF-16LE-BOM", "--oneline", "--graph", "--decorate"]),
        ("main", vec!["log", "--encoding=UTF-32LE", "--pretty=email", "-p", "-1", "HEAD~1"]),
        ("main", vec!["log", "--encoding=UTF-16LE", "--format=%H%n%s%n%N", "--notes"]),
        ("main", vec!["log", "--encoding=UTF-16BE", "--log-size", "--format=%B"]),
        ("main", vec!["log", "--encoding=UTF-16LE", "--graph", "--format=%h%n%b"]),
        ("main", vec!["log", "--encoding=UTF-32LE", "--format=%h %p"]),
        ("main", vec!["log", "--encoding=ISO-8859-1", "--notes", "--log-size", "--format=%s%n%N"]),
        ("main", vec!["show", "--encoding=UTF-16LE"]),
        ("main", vec!["show", "--encoding=UTF-16BE-BOM", "--pretty=raw", "HEAD^2"]),
        ("main", vec!["rev-list", "--encoding=UTF-16LE-BOM", "--pretty=medium", "HEAD"]),
        ("main", vec!["rev-list", "--encoding=UTF-32BE", "--pretty=raw", "HEAD"]),
        ("main", vec!["format-patch", "--no-signature", "--encoding=UTF-16LE", "--stdout", "--cover-letter", "-2"]),
        ("main", vec!["format-patch", "--no-signature", "--encoding=UTF-32BE", "--stdout", "--from=X <x@example.com>", "-1"]),
        ("main", vec!["format-patch", "--no-signature", "--encoding=UTF-16LE", "--stdout", "--range-diff=main~1", "re~1..re"]),
        ("main", vec!["-c", "i18n.logOutputEncoding=UTF-16LE", "range-diff", "main~1...re"]),
        ("c16", vec!["log", "--format=%T%x09%an%x09%s", "--encoding=UTF-8"]),
        ("c16", vec!["log", "--format=%H", "--encoding=UTF-16LE"]),
        ("c16", vec!["show", "--encoding=none", "--format=%s|%an", "-s"]),
        ("c16", vec!["format-patch", "--no-signature", "--encoding=UTF-8", "--stdout", "--cover-letter", "-2"]),
    ];
    if cfg!(target_os = "macos") {
        cases.push(("main", vec!["log", "--encoding=UTF-16", "-1"]));
        cases.push(("c16", vec!["log", "--format=%an"]));
        cases.push(("main", vec!["-c", "i18n.logOutputEncoding=UTF-32", "range-diff", "-s", "main~1...re"]));
    }
    let mut diverged = Vec::new();
    for (repo, args) in &cases {
        let want = git(stock, &fx.0.join("stock").join(repo), &fx.0, args);
        let got = git(BIN, &fx.0.join("zvcs").join(repo), &fx.0, args);
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
    assert!(diverged.is_empty(), "{}", diverged.join("\n"));
}
