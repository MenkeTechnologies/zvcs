//! `git apply` refuses a hunk that has no file header.
//!
//! `find_header()` (apply.c:1616-1628) scans for the next `diff --git` or
//! `---`/`+++` pair, and a line that parses as a fragment header on the way is
//! "a sign that we didn't find a header, and that a patch has become
//! corrupted/broken up": `error: patch fragment without header at
//! <file>:<line>: <the line>`, exit 128. zvcs skipped the stray hunk and
//! reported `No valid patches in input`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-apply-fragment-header-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], None);
        std::fs::write(f.work.join("a"), "1\n2\n3\n").unwrap();
        f.run(&["add", "a"], None);
        f.run(&["commit", "-q", "-m", "base"], None);
        std::fs::write(f.root.join("frag.diff"), "@@ -1,3 +1,3 @@\n 1\n-2\n+two\n 3\n").unwrap();
        std::fs::write(f.root.join("late.diff"), "junk\n@@ -1 +1 @@\n").unwrap();
        // Not a fragment header: `parse_range()` wants a number, and a line with no
        // newline never parses at all.
        std::fs::write(f.root.join("bad.diff"), "@@ -x +1 @@\n@@ -1 +1 @@").unwrap();
        f
    }

    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> (String, String, i32) {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        {
            use std::io::Write;
            let mut pipe = child.stdin.take().unwrap();
            pipe.write_all(stdin.unwrap_or_default()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn a_stray_fragment_is_named_with_its_location() {
    let f = Fixture::new("stray");
    let frag = f.root.join("frag.diff");
    let late = f.root.join("late.diff");
    for args in [vec!["apply"], vec!["apply", "--numstat"], vec!["apply", "--check"]] {
        let mut args = args.clone();
        args.push(frag.to_str().unwrap());
        let want = format!(
            "error: patch fragment without header at {}:1: @@ -1,3 +1,3 @@\n",
            frag.display()
        );
        assert_eq!(f.run(&args, None), (String::new(), want, 128), "{args:?}");
    }
    let want = format!("error: patch fragment without header at {}:2: @@ -1 +1 @@\n", late.display());
    assert_eq!(f.run(&["apply", late.to_str().unwrap()], None), (String::new(), want, 128));
    // Standard input is `<stdin>`, and `-q` mutes the `error()`.
    let body = std::fs::read(&frag).unwrap();
    assert_eq!(
        f.run(&["apply"], Some(&body)),
        (
            String::new(),
            "error: patch fragment without header at <stdin>:1: @@ -1,3 +1,3 @@\n".to_owned(),
            128
        )
    );
    assert_eq!(f.run(&["apply", "-q", frag.to_str().unwrap()], None), (String::new(), String::new(), 128));
}

#[test]
fn a_line_that_does_not_parse_as_a_fragment_is_skipped() {
    let f = Fixture::new("skip");
    assert_eq!(
        f.run(&["apply", f.root.join("bad.diff").to_str().unwrap()], None),
        (
            String::new(),
            "error: No valid patches in input (allow with \"--allow-empty\")\n".to_owned(),
            128
        )
    );
}
