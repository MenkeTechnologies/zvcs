//! `apply --3way` reports a binary patch it cannot rebuild before falling back.
//!
//! `try_threeway()` (apply.c:3715-3760) runs `apply_fragments()` over the blob
//! the patch names, and for a binary patch that is `apply_binary()`, which
//! reports its refusal with `error()` — `cannot apply binary patch to '<name>'
//! without full index line` for a `Binary files … differ` patch — before the
//! function returns -1. `apply_data()` then prints `Falling back to direct
//! application...` and the direct attempt fails the same way again
//! (apply.c:3790-3803). zvcs dropped the first report.
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
    /// `bin` committed, and `../p.diff` the `Binary files … differ` patch of an
    /// edit to it, with the worktree put back.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-apply-threeway-binary-error-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("bin"), b"bin\0\x01").unwrap();
        f.run(&["add", "bin"]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("bin"), b"bin\0\x02").unwrap();
        let (patch, _, _) = f.run(&["diff"]);
        assert!(patch.ends_with("Binary files a/bin and b/bin differ\n"), "{patch}");
        std::fs::write(f.root.join("p.diff"), patch).unwrap();
        f.run(&["checkout", "-q", "bin"]);
        f
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
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const REPORT: &str = "error: cannot apply binary patch to 'bin' without full index line\n\
                      Falling back to direct application...\n\
                      error: cannot apply binary patch to 'bin' without full index line\n\
                      error: bin: patch does not apply\n";

#[test]
fn threeway_reports_before_falling_back() {
    let f = Fixture::new("apply");
    let patch = f.root.join("p.diff");
    let patch = patch.to_str().unwrap();
    for args in [vec!["apply", "--3way", patch], vec!["apply", "--check", "--3way", patch]] {
        assert_eq!(f.run(&args), (String::new(), REPORT.to_owned(), 1), "{args:?}");
    }
    // `-q` mutes every `error()` along with the fallback line.
    assert_eq!(f.run(&["apply", "-q", "--3way", patch]), (String::new(), String::new(), 1));
}
