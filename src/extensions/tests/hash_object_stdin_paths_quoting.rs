//! `git hash-object --stdin-paths` decodes a C-quoted record before it opens it.
//!
//! ```c
//! while (strbuf_getline(&buf, stdin) != EOF) {
//!         if (buf.buf[0] == '"') {
//!                 strbuf_reset(&unquoted);
//!                 if (unquote_c_style(&unquoted, buf.buf, NULL))
//!                         die("line is badly quoted");
//!                 strbuf_swap(&buf, &unquoted);
//!         }
//!         hash_object(buf.buf, type, no_filters ? NULL : buf.buf, flags);
//! }
//! ```
//!
//! (`hash_stdin_paths()`, builtin/hash-object.c:47-56.) The swap means the
//! decoded name is both the path that gets opened and the virtual path the
//! attribute lookup uses, so a quoted record is filtered as the name it decodes
//! to. The port took every record literally, so `git ls-files -z`-style quoted
//! output — which is what a caller pipes in — reached `open()` with its quotes
//! and backslashes still attached and answered `could not open '"a b.txt"' for
//! reading`, while stock git hashed the file.
//!
//! `unquote_c_style()` (quote.c:386-441) is strict about its octal escape:
//! exactly three digits, each `0`-`7`, and a leading digit above `3` would
//! overflow a byte and is refused. A record that fails it is `fatal: line is
//! badly quoted` — note that git never even tries to open such a name.
//!
//! Every expectation here was captured from stock git 2.55.0.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-hoquote-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work };
        fx.ok(&["init", "-q", "-b", "main", "."]);
        fx
    }

    fn feed(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.root.join("gitconfig-system"))
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run binary");
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.feed(args, b"");
        assert!(out.status.success(), "setup git {args:?}: {out:?}");
        out
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_quoted_record_is_unquoted_before_it_is_opened() {
    let fx = Fixture::new("decode");
    // Three names that can only be fed in quoted: one with a space, one whose
    // plain spelling is reachable only through an octal escape, and one with an
    // escaped backslash in it.
    std::fs::write(fx.work.join("a b.txt"), "spaced\n").unwrap();
    std::fs::write(fx.work.join("plain.txt"), "plain\n").unwrap();
    std::fs::write(fx.work.join("back\\slash.txt"), "slashed\n").unwrap();

    let want = stdout(&fx.ok(&["hash-object", "a b.txt", "plain.txt", "back\\slash.txt"]));
    assert_eq!(want.lines().count(), 3, "three ids expected: {want:?}");

    // `\141` is `a`, so the second record names `plain.txt` the long way round.
    let quoted = "\"a b.txt\"\n\"pl\\141in.txt\"\n\"back\\\\slash.txt\"\n";
    let out = fx.feed(&["hash-object", "--stdin-paths"], quoted.as_bytes());
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stderr(&out), "", "{out:?}");
    assert_eq!(stdout(&out), want, "quoted records must hash the same files");
}

#[test]
fn the_decoded_name_is_also_the_filter_path() {
    let fx = Fixture::new("filter");
    // `strbuf_swap()` hands the *decoded* name to `hash_object()` as the virtual
    // path too, so the attribute that matches is the one for `a b.txt`, not for
    // the quoted spelling. The `text` attribute makes that observable: the
    // check-in conversion folds CRLF to LF, so the id differs from the
    // unfiltered one only if the attribute was found.
    std::fs::write(fx.work.join(".gitattributes"), "\"a b.txt\" text\n").unwrap();
    std::fs::write(fx.work.join("a b.txt"), "one\r\ntwo\r\n").unwrap();

    let filtered = stdout(&fx.ok(&["hash-object", "a b.txt"]));
    let raw = stdout(&fx.ok(&["hash-object", "--no-filters", "a b.txt"]));
    assert_ne!(filtered, raw, "the text attribute must change the id at all");

    let out = fx.feed(&["hash-object", "--stdin-paths"], b"\"a b.txt\"\n");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stdout(&out), filtered, "the decoded name must drive the filters");
}

#[test]
fn a_record_unquote_c_style_rejects_is_badly_quoted() {
    let fx = Fixture::new("bad");
    // `unquote_c_style()` returning -1 is a `die()`, and it happens before any
    // `open()`, so the message never names a file. Each of these breaks a
    // different arm of quote.c:394-435: no closing quote, an escape that is not
    // in the table, an octal run that is too short, and a leading octal digit
    // above 3 (which would overflow a byte).
    for record in [
        "\"unterminated\n",
        "\"bad\\q\"\n",
        "\"bad\\12\"\n",
        "\"bad\\400\"\n",
    ] {
        let out = fx.feed(&["hash-object", "--stdin-paths"], record.as_bytes());
        assert_eq!(out.status.code(), Some(128), "{record:?}: {out:?}");
        assert_eq!(stderr(&out), "fatal: line is badly quoted\n", "{record:?}");
        assert_eq!(stdout(&out), "", "{record:?} wrote an id");
    }
}

#[test]
fn an_unquoted_record_is_still_taken_literally() {
    let fx = Fixture::new("literal");
    // The decode is gated on the record's *first* byte being a double quote, so
    // a name that merely contains one is opened exactly as written — and a name
    // with no quote at all is untouched by this change.
    std::fs::write(fx.work.join("say\"hi.txt"), "quoted\n").unwrap();
    std::fs::write(fx.work.join("plain.txt"), "plain\n").unwrap();

    let want = stdout(&fx.ok(&["hash-object", "say\"hi.txt", "plain.txt"]));
    let out = fx.feed(&["hash-object", "--stdin-paths"], b"say\"hi.txt\nplain.txt\n");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stdout(&out), want, "an unquoted record must not be decoded");
}
