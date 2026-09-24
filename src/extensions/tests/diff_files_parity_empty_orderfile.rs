//! `-O ''` names no order file. `OPT_FILENAME('O', …)` (diff.c:6291) stores what
//! `fix_filename()` returns, and that is NULL for an empty name
//! (parse-options.c:64-70), so an empty `-O` clears an earlier `-O<file>` — and,
//! for `git diff`, a `diff.orderFile` seeded by `diff_setup()` — instead of being
//! an error. `diff-files` refused it with `error: -O requires an argument` (128);
//! `diff` kept the empty name and died opening it.
//!
//! Every expectation was measured against stock git 2.55.0.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `a.txt` and `z.txt` modified in the worktree, and an order file `ord` that
/// puts `z.txt` first.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("zvcs-dforder-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fx = Fixture { dir: dir.canonicalize().unwrap() };
        fx.ok(&["init", "-q", "-b", "main"]);
        fx.write("a.txt", "a\n");
        fx.write("z.txt", "z\n");
        fx.ok(&["add", "a.txt", "z.txt"]);
        fx.write("a.txt", "a\nA\n");
        fx.write("z.txt", "z\nZ\n");
        fx.write("ord", "z.txt\n");
        fx
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.dir.join(name), body).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stderr.is_empty(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn diff_files_empty_orderfile_is_accepted_and_clears_an_earlier_one() {
    let fx = Fixture::new("files");
    assert_eq!(fx.ok(&["diff-files", "-O", "", "--name-only"]), "a.txt\nz.txt\n");
    assert_eq!(fx.ok(&["diff-files", "-O", "ord", "--name-only"]), "z.txt\na.txt\n");
    assert_eq!(fx.ok(&["diff-files", "-O", "ord", "-O", "", "--name-only"]), "a.txt\nz.txt\n");
}

#[test]
fn diff_empty_orderfile_overrides_the_configured_one() {
    let fx = Fixture::new("diff");
    // An unreadable `diff.orderFile` would die at 128; `-O ''` must drop it first.
    assert_eq!(
        fx.ok(&["-c", "diff.orderFile=missing", "diff", "-O", "", "--name-only"]),
        "a.txt\nz.txt\n"
    );
    assert_eq!(fx.ok(&["diff", "-O", "ord", "-O", "", "--name-only"]), "a.txt\nz.txt\n");
}
