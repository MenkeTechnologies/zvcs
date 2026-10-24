//! `--pathspec-from-file=` with an empty value. `OPT_PATHSPEC_FROM_FILE` is an
//! `OPT_FILENAME`, and `fix_filename()` maps an empty value to NULL
//! (parse-options.c:64-70) which then replaces any earlier value
//! (parse-options.c:227-232). So `cmd_add()` sees no file at all: `-p` does not
//! die over the `--pathspec-from-file`/`--interactive/--patch` clash
//! (builtin/add.c:494-495), and `--pathspec-file-nul` still demands a file.
//!
//! Expectations measured against stock git 2.56.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("zvcs-addpff-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fx = Fixture { dir };
        assert_eq!(fx.run(&["init", "-q", "-b", "main", "."]).status.code(), Some(0));
        std::fs::write(fx.dir.join("f"), "a\n").unwrap();
        assert_eq!(fx.run(&["add", "f"]).status.code(), Some(0));
        fx
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.dir)
            .env("HOME", &self.dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn add_parity_empty_pathspec_from_file_does_not_clash_with_patch() {
    let fx = Fixture::new("patch");
    let out = fx.run(&["add", "-v", "-p", "--pathspec-from-file="]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(text(&out.stdout), "No changes.\n");
    assert_eq!(text(&out.stderr), "");
}

#[test]
fn add_parity_empty_pathspec_from_file_clears_an_earlier_one() {
    let fx = Fixture::new("clear");
    let out = fx.run(&["add", "--pathspec-from-file=nope", "--pathspec-from-file=", "-p"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(text(&out.stdout), "No changes.\n");
}

#[test]
fn add_parity_empty_pathspec_from_file_leaves_nul_without_a_file() {
    let fx = Fixture::new("nul");
    let out = fx.run(&["add", "--pathspec-file-nul", "--pathspec-from-file="]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        text(&out.stderr),
        "fatal: the option '--pathspec-file-nul' requires '--pathspec-from-file'\n"
    );
}
