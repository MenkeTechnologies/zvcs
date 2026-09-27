//! Both `cat-file` batch loops read their input with
//! `strbuf_getdelim_strip_crlf(&input, stdin, opt->input_delim)`
//! (builtin/cat-file.c:764, :1009). With the default `\n` terminator that
//! drops the newline and then one `\r` in front of it (strbuf.c:735-745), so
//! input written with CRLF line endings names the same objects. Only one `\r`
//! goes, and a NUL-terminated `-z` line keeps its `\r`. zvcs dropped the
//! terminator only, so every CRLF line reported `<name>\r missing`.
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
        let root = std::env::temp_dir().join(format!("zvcs-cat-file-batch-crlf-{tag}-{}", std::process::id()));
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

impl Fixture {
    fn batch(&self, args: &[&str], input: &[u8]) -> (String, String, i32) {
        self.run_in(&self.work, args, Some(input))
    }
}

#[test]
fn one_carriage_return_before_the_newline_is_dropped() {
    let f = Fixture::new("crlf");
    let head = f.run(&["rev-parse", "HEAD"]).0.trim().to_string();
    let size = f.run(&["cat-file", "-s", "HEAD"]).0.trim().to_string();
    assert_eq!(
        f.batch(&["cat-file", "--batch-check"], b"HEAD\r\nHEAD\r\r\n"),
        (format!("{head} commit {size}\nHEAD\r missing\n"), String::new(), 0)
    );
    // The `\r` is gone before `%(rest)` is split off.
    assert_eq!(
        f.batch(&["cat-file", "--batch-check=%(rest)|"], b"HEAD \r\n"),
        ("|\n".into(), String::new(), 0)
    );
    assert_eq!(
        f.batch(&["cat-file", "--batch-command"], b"info HEAD\r\n"),
        (format!("{head} commit {size}\n"), String::new(), 0)
    );
}

#[test]
fn a_nul_terminated_line_keeps_its_carriage_return() {
    let f = Fixture::new("nul");
    assert_eq!(
        f.batch(&["cat-file", "-z", "--batch-check"], b"HEAD\r\0"),
        ("HEAD\r missing\n".into(), String::new(), 0)
    );
}
