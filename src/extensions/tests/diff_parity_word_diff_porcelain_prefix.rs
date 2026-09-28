//! `--word-diff=porcelain` under `--line-prefix`, and range-diff's indent.
//!
//! `emit_line_0()` opens every line it writes with `diff_line_prefix()`
//! (diff.c:763), but the word diff's records are `DIFF_SYMBOL_WORD_DIFF`, printed
//! verbatim (diff.c:1622-1623): the prefix goes only where
//! `fn_out_diff_words_write_helper()` puts it after a newline it wrote
//! (diff.c:2019-2020), ahead of a removal-only flush and ahead of a hunk or the
//! trailing context that starts a line (diff.c:2127-2129, 2237-2275). Porcelain's
//! words end in `\n` of their own and `~\n` follows each line bare
//! (diff.c:1547-1552), so most of its lines carry no prefix at all. zvcs
//! prefixed every line afterwards; range-diff, whose `output_prefix_cb()` is the
//! same hook (range-diff.c:501-529), refused the option instead.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn empty(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-word-diff-porcelain-prefix-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        std::fs::write(self.work.join(file), body).unwrap();
        self.run(&["add", file]);
        self.run(&["commit", "-q", "-m", msg]);
    }

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0.trim_end().to_string()
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// `a` holds "two" at HEAD and "hello world\nsecond line\n" in the work tree.
fn changed(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.commit("a", "two\n", "base");
    std::fs::write(f.work.join("a"), "hello world\nsecond line\n").unwrap();
    f
}

const BODY: &str = "P@@ -1 +1,2 @@\nP-two\n+hello world\n~\nP+second line\n~\n";

fn body_of(out: &str) -> &str {
    &out[out.find("P@@").unwrap_or(0)..]
}

#[test]
fn the_word_records_keep_their_own_prefixes() {
    let f = changed("cmds");
    for args in [
        &["diff", "--word-diff=porcelain", "--line-prefix=P"][..],
        &["diff-files", "-p", "--word-diff=porcelain", "--line-prefix=P"][..],
        &["diff-index", "-p", "--word-diff=porcelain", "--line-prefix=P", "HEAD"][..],
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        assert!(out.starts_with("Pdiff --git a/a b/a\nPindex "), "{args:?}: {out}");
        assert_eq!(body_of(&out), BODY, "{args:?}");
    }
    std::fs::write(f.work.join("b"), "two\n").unwrap();
    let (out, _, code) = f.run(&["diff", "--no-index", "--word-diff=porcelain", "--line-prefix=P", "b", "a"]);
    assert_eq!(code, 1);
    assert_eq!(body_of(&out), BODY);
}

#[test]
fn diff_pairs() {
    let f = changed("pairs");
    f.run(&["commit", "-q", "-am", "change"]);
    let (raw, _, _) = f.run(&["diff-tree", "-r", "-z", "--raw", "HEAD~1", "HEAD"]);
    let mut child = std::process::Command::new(BIN)
        .args(["diff-pairs", "-z", "-p", "--word-diff=porcelain", "--line-prefix=P"])
        .current_dir(&f.work)
        .env("HOME", &f.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(raw.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(body_of(&String::from_utf8_lossy(&out.stdout)), BODY);
}

#[test]
fn range_diff_indents_like_git() {
    let f = Fixture::empty("range");
    f.commit("base", "base\n", "base");
    let lines = |seven: &str| {
        (1..=9).map(|n| if n == 7 { format!("7 {seven} here\n") } else { format!("{n} word here\n") }).collect::<String>()
    };
    f.run(&["checkout", "-q", "-b", "v1", "main"]);
    f.commit("f", &lines("word"), "feat");
    f.run(&["checkout", "-q", "-b", "v2", "main"]);
    f.commit("f", &lines("WORD"), "feat");
    let (out, err, code) = f.run(&["range-diff", "--word-diff=porcelain", "main", "v1", "v2"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert!(out.contains("\n     +6 word here\n~\n     +7 \n-word\n+WORD\n  here\n~\n     +8 word here\n~\n"), "{out}");
}
