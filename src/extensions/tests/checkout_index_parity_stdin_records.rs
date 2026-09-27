//! `checkout-index --stdin`'s record reader.
//!
//! ```c
//! getline_fn = nul_term_line ? strbuf_getline_nul : strbuf_getline_lf;
//! while (getline_fn(&buf, stdin) != EOF) {
//! ```
//!
//! (builtin/checkout-index.c:319-331.) `strbuf_getline_lf()` strips the `\n`
//! and nothing else, so a CRLF line names a path that ends in `\r`; only EOF
//! ends the loop, so an empty record in the middle — in either mode — is the
//! empty path, which `checkout_file()` reports as `git checkout-index:  is not
//! in the cache` (with a raw `fprintf`, so the `\r` is printed as is). zvcs
//! stripped the `\r` and skipped every empty record.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    /// `a` and `b\r` committed, then both removed from the work tree.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-checkout-index-stdin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("r");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], b"");
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        std::fs::write(f.work.join("b\r"), "b\n").unwrap();
        f.run(&["add", "a", "b\r"], b"");
        f.run(&["commit", "-q", "-m", "a"], b"");
        std::fs::remove_file(f.work.join("a")).unwrap();
        std::fs::remove_file(f.work.join("b\r")).unwrap();
        f
    }

    fn run(&self, args: &[&str], stdin: &[u8]) -> (Vec<u8>, Vec<u8>, i32) {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.stdout, out.stderr, out.status.code().expect("no signal"))
    }
}

const EMPTY: &[u8] = b"git checkout-index:  is not in the cache\n";

#[test]
fn a_crlf_line_names_a_path_ending_in_cr() {
    let f = Fixture::new("crlf");
    assert_eq!(
        f.run(&["checkout-index", "--stdin"], b"a\r\n"),
        (Vec::new(), b"git checkout-index: a\r is not in the cache\n".to_vec(), 1)
    );
    assert!(!f.work.join("a").exists());
    assert_eq!(f.run(&["checkout-index", "--stdin"], b"b\r\n"), (Vec::new(), Vec::new(), 0));
    assert_eq!(std::fs::read(f.work.join("b\r")).unwrap(), b"b\n");
}

#[test]
fn an_empty_record_is_the_empty_path() {
    let f = Fixture::new("empty");
    assert_eq!(f.run(&["checkout-index", "--stdin"], b"\n"), (Vec::new(), EMPTY.to_vec(), 1));
    assert_eq!(f.run(&["checkout-index", "-f", "--stdin"], b"a\n\n"), (Vec::new(), EMPTY.to_vec(), 1));
    assert!(f.work.join("a").exists());
    assert_eq!(f.run(&["checkout-index", "-f", "-z", "--stdin"], b"a\0\0"), (Vec::new(), EMPTY.to_vec(), 1));
    assert_eq!(f.run(&["checkout-index", "-f", "-z", "--stdin"], b"\0"), (Vec::new(), EMPTY.to_vec(), 1));
    // The piece after the last separator is no record.
    assert_eq!(f.run(&["checkout-index", "-f", "--stdin"], b"a\n"), (Vec::new(), Vec::new(), 0));
    assert_eq!(f.run(&["checkout-index", "-f", "-z", "--stdin"], b"a\0"), (Vec::new(), Vec::new(), 0));
}
