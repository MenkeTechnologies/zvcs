//! `git update-index --stdin` reads with
//! `getline_fn = nul_term_line ? strbuf_getline_nul : strbuf_getline_lf`
//! (builtin/update-index.c:1181-1204). `_lf` strips the `\n` and nothing else,
//! so a CRLF line names a path ending in `\r`; and since only EOF ends the
//! loop, an empty line in the middle is a record — the empty path, which
//! `update_one()` reports as `Ignoring path ` on stderr. The `error()` and
//! `die()` that name a path go through `vfreportf()`, which prints control
//! bytes as `?` (usage.c:12-38). zvcs stripped the `\r`, skipped empty
//! records, and printed the raw bytes.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-update-index-stdin-lines-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("a", "a\n");
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f
    }

    fn write(&self, path: &str, body: &str) {
        let path = self.work.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args, None)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str], stdin: Option<&[u8]>) -> (String, String, i32) {
        use std::io::Write;
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(stdin.unwrap_or_default()).unwrap();
        drop(pipe);
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn an_empty_record_is_the_empty_path() {
    let f = Fixture::new("empty");
    for (args, input) in [
        (&["update-index", "--verbose", "--stdin"][..], &b"a\n\na\n"[..]),
        (&["update-index", "-z", "--verbose", "--stdin"][..], &b"a\0\0a\0"[..]),
    ] {
        assert_eq!(
            f.run_in(&f.work, args, Some(input)),
            ("add 'a'\nadd 'a'\n".into(), "Ignoring path \n".into(), 0),
            "{args:?}"
        );
    }
}

#[test]
fn a_crlf_line_names_a_path_ending_in_cr() {
    let f = Fixture::new("crlf");
    let before = f.run(&["ls-files", "-s"]);
    assert_eq!(
        f.run_in(&f.work, &["update-index", "--verbose", "--stdin"], Some(b"a\r\n")),
        (
            String::new(),
            "error: a?: does not exist and --remove not passed\nfatal: Unable to process path a?\n".into(),
            128
        )
    );
    assert_eq!(f.run(&["ls-files", "-s"]), before);
    f.write("b\r", "x");
    assert_eq!(
        f.run_in(&f.work, &["update-index", "--verbose", "--add", "--stdin"], Some(b"b\r\n")),
        ("add 'b\r'\n".into(), String::new(), 0)
    );
    assert_eq!(f.run(&["ls-files"]).0, "a\n\"b\\r\"\n");
}
