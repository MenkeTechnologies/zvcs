//! `git rev-parse --bisect` (builtin/rev-parse.c:936-956) reads the terms with
//! `read_bisect_terms()` (bisect.c:1005-1031) and walks the refs twice:
//! `refs/bisect/<bad>` through `show_reference()` and `refs/bisect/<good>`
//! through `anti_reference()`. Both are string-prefix matches on the full
//! refname, so `bad` also takes `badly`; `show_reference()` honours pending
//! `--exclude` patterns and `anti_reference()` does not (:198-232). The terms
//! file is read line by line with `strbuf_getline_lf()`, so a file without a
//! second line makes the good term empty — every ref under `refs/bisect/` —
//! and only a missing file falls back to `bad`/`good`. zvcs refused the option
//! as "not ported yet".
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
        let root = std::env::temp_dir().join(format!("zvcs-rev-parse-bisect-{tag}-{}", std::process::id()));
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
    /// Two commits, with bisect refs laid out the way `git bisect` writes them
    /// plus a `badly` that only the prefix rule picks up.
    fn bisecting(tag: &str) -> (Self, String, String) {
        let f = Fixture::new(tag);
        f.write("a", "a2\n");
        f.run(&["commit", "-q", "-am", "two"]);
        let head = f.run(&["rev-parse", "HEAD"]).0.trim().to_string();
        let one = f.run(&["rev-parse", "HEAD~1"]).0.trim().to_string();
        f.run(&["update-ref", "refs/bisect/bad", "HEAD"]);
        f.run(&["update-ref", "refs/bisect/badly", "HEAD~1"]);
        f.run(&["update-ref", &format!("refs/bisect/good-{one}"), "HEAD~1"]);
        f.run(&["update-ref", "refs/bisect/skip-x", "HEAD"]);
        (f, head, one)
    }
}

#[test]
fn bad_refs_then_excluded_good_refs() {
    let (f, head, one) = Fixture::bisecting("plain");
    assert_eq!(
        f.run(&["rev-parse", "--bisect"]),
        (format!("{head}\n{one}\n^{one}\n"), String::new(), 0)
    );
    assert_eq!(
        f.run(&["rev-parse", "--symbolic", "--bisect"]),
        (format!("refs/bisect/bad\nrefs/bisect/badly\n^refs/bisect/good-{one}\n"), String::new(), 0)
    );
    // `--exclude` filters the bad walk only.
    assert_eq!(
        f.run(&["rev-parse", "--exclude=refs/bisect/*", "--bisect"]),
        (format!("^{one}\n"), String::new(), 0)
    );
}

#[test]
fn terms_come_from_bisect_terms_line_by_line() {
    let (f, _, one) = Fixture::bisecting("terms");
    f.run(&["update-ref", "refs/bisect/new", "HEAD"]);
    f.write(".git/BISECT_TERMS", "new\nskip\n");
    assert_eq!(
        f.run(&["rev-parse", "--symbolic", "--bisect"]),
        ("refs/bisect/new\n^refs/bisect/skip-x\n".into(), String::new(), 0)
    );
    // No second line: the good term is empty and matches everything.
    f.write(".git/BISECT_TERMS", "new");
    assert_eq!(
        f.run(&["rev-parse", "--symbolic", "--bisect"]),
        (
            format!(
                "refs/bisect/new\n^refs/bisect/bad\n^refs/bisect/badly\n\
                 ^refs/bisect/good-{one}\n^refs/bisect/new\n^refs/bisect/skip-x\n"
            ),
            String::new(),
            0
        )
    );
    std::fs::remove_file(f.work.join(".git/BISECT_TERMS")).unwrap();
    std::fs::create_dir(f.work.join(".git/BISECT_TERMS")).unwrap();
    assert_eq!(
        f.run(&["rev-parse", "--bisect"]),
        (String::new(), "fatal: could not read file '.git/BISECT_TERMS': Is a directory\n".into(), 128)
    );
}
