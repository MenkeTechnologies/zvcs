//! `git restore --pathspec-from-file=<f>` reads its list the way
//! `parse_pathspec_file()` does.
//!
//! ```c
//! if (!strcmp(file, "-"))
//!         in = stdin;
//! else
//!         in = xfopen(file, "r");
//!
//! while (getline_fn(&buf, in) != EOF) {
//!         if (!nul_term_line && buf.buf[0] == '"') {
//!                 strbuf_reset(&unquoted);
//!                 if (unquote_c_style(&unquoted, buf.buf, NULL))
//!                         die(_("line is badly quoted: %s"), buf.buf);
//!                 strbuf_swap(&buf, &unquoted);
//!         }
//!         strvec_push(&parsed_file, buf.buf);
//! ```
//!
//! (pathspec.c:687-721.) `restore` carried a private re-implementation that read
//! the file with a plain read-and-split, so it diverged from that C — and from
//! `checkout`, which already used the shared port — four ways:
//!
//! * a missing file gave the Rust io error at exit 1 instead of `xfopen()`'s
//!   `fatal: could not open '<f>' for reading: …` at 128;
//! * a `"`-quoted line was taken literally instead of going through
//!   `unquote_c_style()`, so `"a b.txt"` matched nothing;
//! * a badly quoted line never reached `fatal: line is badly quoted: …`;
//! * an embedded NUL in the newline form was kept rather than ending the record,
//!   as `strvec_push()`'s C string does.
//!
//! Every expectation below was measured against stock git 2.55.0 first.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fixture {
    /// `a b.txt` (a space in the name, so it needs quoting) and `plain.txt`, both
    /// changed on `other`.
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("zvcs-rpsf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let f = Fixture { base, root };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("a b.txt", "one\n");
        f.write("plain.txt", "two\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["checkout", "-q", "-b", "other"]);
        f.write("a b.txt", "ONE\n");
        f.write("plain.txt", "TWO\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "other"]);
        f.git(&["checkout", "-q", "main"]);
        f
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    fn write_bytes(&self, rel: &str, body: &[u8]) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn git(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("HOME", self.base.join("home"))
            .env("ZVCS_HOME", self.base.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
            .output()
            .unwrap();
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        (s, out.status.code().unwrap_or(-1))
    }
}

#[test]
fn a_quoted_line_is_unquoted_before_it_becomes_a_pathspec() {
    let f = Fixture::new("quote");
    f.write("list", "\"a b.txt\"\n");
    let (out, rc) = f.git(&["restore", "--source=other", "--pathspec-from-file=list"]);
    assert_eq!(rc, 0, "a C-quoted line must resolve, got: {out}");
    assert_eq!(out, "", "a successful restore says nothing: {out:?}");
    assert_eq!(f.read("a b.txt"), "ONE\n", "the quoted path must be restored");
    assert_eq!(f.read("plain.txt"), "two\n", "and nothing else");
}

#[test]
fn a_badly_quoted_line_is_fatal_at_128() {
    let f = Fixture::new("badquote");
    f.write("list", "\"bad\n");
    let (out, rc) = f.git(&["restore", "--source=other", "--pathspec-from-file=list"]);
    assert_eq!(rc, 128, "unquote_c_style() failing is a die(), got: {out}");
    assert_eq!(
        out, "fatal: line is badly quoted: \"bad\n",
        "the line is echoed verbatim after the message: {out:?}"
    );
    assert_eq!(f.read("a b.txt"), "one\n", "nothing may be restored: {out}");
}

#[test]
fn a_missing_list_file_is_xfopens_fatal_at_128() {
    let f = Fixture::new("missing");
    let (out, rc) = f.git(&["restore", "--source=other", "--pathspec-from-file=nope"]);
    assert_eq!(rc, 128, "xfopen() dies, it does not return an error, got: {out}");
    assert_eq!(
        out, "fatal: could not open 'nope' for reading: No such file or directory\n",
        "strerror() has no Rust `(os error N)` tail: {out:?}"
    );
}

#[test]
fn an_embedded_nul_ends_the_record_in_the_newline_form() {
    let f = Fixture::new("nul");
    // `strvec_push()` takes the record as a C string, so everything from the NUL on
    // is dropped: this names `plain.txt` alone, not `plain.txt\0junk`.
    f.write_bytes("list", b"plain.txt\0junk\n");
    let (out, rc) = f.git(&["restore", "--source=other", "--pathspec-from-file=list"]);
    assert_eq!(rc, 0, "the record must truncate at the NUL, got: {out}");
    assert_eq!(f.read("plain.txt"), "TWO\n", "plain.txt must be restored");
    assert_eq!(f.read("a b.txt"), "one\n", "and nothing else");
}

#[test]
fn the_nul_form_reads_whole_records_and_does_not_unquote() {
    let f = Fixture::new("nulform");
    // With `--pathspec-file-nul` the `"` is part of the name, so a quoted spelling
    // matches nothing; the bare name does.
    f.write_bytes("list", b"a b.txt\0plain.txt\0");
    let (out, rc) = f.git(&[
        "restore",
        "--source=other",
        "--pathspec-from-file=list",
        "--pathspec-file-nul",
    ]);
    assert_eq!(rc, 0, "NUL-separated records should resolve, got: {out}");
    assert_eq!(f.read("a b.txt"), "ONE\n", "a name with a space needs no quoting here");
    assert_eq!(f.read("plain.txt"), "TWO\n", "both records are used");

    f.write_bytes("list", b"\"a b.txt\"\0");
    let (out, rc) = f.git(&[
        "restore",
        "--source=other",
        "--pathspec-from-file=list",
        "--pathspec-file-nul",
    ]);
    assert_eq!(rc, 1, "the quotes are part of the name in NUL mode, got: {out}");
    assert_eq!(
        out, "error: pathspec '\"a b.txt\"' did not match any file(s) known to git\n",
        "and the spec is reported with its quotes: {out:?}"
    );
}
