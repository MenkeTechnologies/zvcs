//! `git blame` prints identities and subjects in the log output encoding, and
//! pads the author column by display width.
//!
//! `get_commit_info()` reads the author, the committer and the summary out of
//! `repo_logmsg_reencode(commit, NULL, get_log_output_encoding())`
//! (builtin/blame.c:199-222), so a commit stored in ISO-8859-1 is printed in
//! UTF-8 by default, and `--encoding=<enc>`, `i18n.logOutputEncoding` and
//! `i18n.commitEncoding` choose another (revision.c:2739-2745,
//! environment.c). zvcs printed the stored bytes and refused every
//! `--encoding` but UTF-8 and `none`.
//!
//! The author column is padded to `longest_author` by `utf8_strwidth()`
//! (builtin/blame.c:567, :688-690), so a name with a two-byte or a wide
//! character is padded by its columns, not by its bytes.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, dir: &Path, home: &Path, name: &[u8], args: &[&str]) -> (Vec<u8>, Vec<u8>, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_AUTHOR_NAME", OsString::from_vec(name.to_vec()))
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0100")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0100")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (out.stdout, out.stderr, out.status.code().expect("no signal"))
}

impl Fixture {
    /// Two copies of one history built by stock git: line 1 by a Latin-1 author
    /// in a commit stored as ISO-8859-1 with a Latin-1 subject, line 2 by a
    /// UTF-8 author whose name holds a two-byte and a wide character, line 3
    /// by an ASCII author.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-blame-log-encoding-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for side in ["stock", "zvcs"] {
            let work = root.join(side);
            std::fs::create_dir_all(&work).unwrap();
            let run = |name: &[u8], args: &[&str]| {
                let (_, err, rc) = git(stock, &work, &root, name, args);
                assert_eq!(rc, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
            };
            run(b"A", &["init", "-q", "-b", "main", "."]);
            std::fs::write(work.join("a"), "one\n").unwrap();
            run(b"A", &["add", "a"]);
            // The subject goes through a file so its Latin-1 byte reaches the
            // commit untouched.
            let msg = root.join(format!("msg-{side}"));
            std::fs::write(&msg, b"caf\xe9\n").unwrap();
            let msg = msg.to_str().unwrap();
            run(
                b"J\xf6rg",
                &[
                    "-c",
                    "i18n.commitEncoding=ISO-8859-1",
                    "-c",
                    "maintenance.auto=false",
                    "commit",
                    "-q",
                    "-F",
                    msg,
                ],
            );
            std::fs::write(work.join("a"), "one\ntwo\n").unwrap();
            run(
                "J\u{f6}rg \u{4e2d}".as_bytes(),
                &["-c", "maintenance.auto=false", "commit", "-q", "-am", "two"],
            );
            std::fs::write(work.join("a"), "one\ntwo\nthree\n").unwrap();
            run(b"Al", &["-c", "maintenance.auto=false", "commit", "-q", "-am", "three"]);
        }
        Fixture { root }
    }

    fn assert_same(&self, stock: &str, args: &[&str]) {
        let s = git(stock, &self.root.join("stock"), &self.root, b"A", args);
        let z = git(BIN, &self.root.join("zvcs"), &self.root, b"A", args);
        assert_eq!(z, s, "git {args:?}:\nzvcs:  {:?}\nstock: {:?}", String::from_utf8_lossy(&z.0), String::from_utf8_lossy(&s.0));
    }
}

#[test]
fn identities_and_subjects_are_reencoded_for_output() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "reencode");
    let configs: [&[&str]; 3] = [
        &[],
        &["-c", "i18n.logOutputEncoding=ISO-8859-1"],
        &["-c", "i18n.commitEncoding=ISO-8859-1"],
    ];
    let encodings: [&[&str]; 7] = [
        &[],
        &["--encoding=ISO-8859-1"],
        &["--encoding=UTF-8"],
        &["--encoding=none"],
        &["--encoding="],
        &["--encoding=bogus"],
        &["--encoding", "latin1"],
    ];
    for config in configs {
        for encoding in encodings {
            for format in [&[][..], &["--porcelain"], &["--line-porcelain"], &["--incremental"], &["-c"]] {
                let mut argv: Vec<&str> = config.to_vec();
                argv.push("blame");
                argv.extend_from_slice(encoding);
                argv.extend_from_slice(format);
                argv.push("a");
                f.assert_same(stock, &argv);
            }
        }
    }
}

#[test]
fn the_author_column_is_padded_by_display_width() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "width");
    f.assert_same(stock, &["blame", "a"]);
    f.assert_same(stock, &["blame", "-f", "-n", "a"]);
    f.assert_same(stock, &["blame", "HEAD~1", "--", "a"]);
    f.assert_same(stock, &["-c", "color.ui=always", "blame", "--color-lines", "a"]);
}
