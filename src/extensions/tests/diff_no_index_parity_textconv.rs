//! `diff --no-index` ignored textconv, so the patch and `-S`/`-G` saw raw bytes.
//!
//! `cmd_diff()` sets `allow_textconv` before it branches to the no-index path
//! (builtin/diff.c:512), `builtin_diff()` takes the patch between the sides'
//! `fill_textconv()` output (diff.c:3890-3893, 3964-3966), and
//! `pickaxe_match()` searches that output too (diffcore-pickaxe.c:148-166); the
//! stat formats keep the bytes on disk (`builtin_diffstat()`). A textconv program
//! that fails is `fatal: unable to read files to diff` (diff.c:7824). zvcs had
//! no textconv pass in its no-index engine and refused `--[no-]textconv`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A repository whose `*.t` files upper-case through `diff.up.textconv`, and
    /// two such files that are not tracked.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-no-index-textconv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q"]);
        std::fs::write(f.root.join(".gitattributes"), "*.t diff=up\n").unwrap();
        f.run(&["config", "diff.up.textconv", "tr a-z A-Z <"]);
        std::fs::write(f.root.join("a.t"), "hello\n").unwrap();
        std::fs::write(f.root.join("b.t"), "world\n").unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
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

#[test]
fn the_patch_is_taken_between_textconv_outputs() {
    let f = Fixture::new("patch");
    let (out, err, code) = f.run(&["diff", "--no-index", "a.t", "b.t"]);
    assert_eq!(err, "");
    assert_eq!(code, 1);
    assert_eq!(
        out,
        "diff --git a/a.t b/b.t\nindex ce01362..cc628cc 100644\n--- a/a.t\n+++ b/b.t\n\
@@ -1 +1 @@\n-HELLO\n+WORLD\n"
    );
    let (out, _, _) = f.run(&["diff", "--no-index", "--no-textconv", "a.t", "b.t"]);
    assert!(out.ends_with("-hello\n+world\n"), "{out}");
    // The stat counts the files themselves.
    let (out, _, _) = f.run(&["diff", "--no-index", "--textconv", "--stat", "a.t", "b.t"]);
    assert_eq!(out, " a.t => b.t | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n");
}

#[test]
fn pickaxe_searches_textconv_output() {
    let f = Fixture::new("pickaxe");
    let (out, _, code) = f.run(&["diff", "--no-index", "-S", "HELLO", "--name-only", "a.t", "b.t"]);
    assert_eq!((out.as_str(), code), ("b.t\n", 1));
    let (out, _, code) = f.run(&["diff", "--no-index", "-S", "hello", "--name-only", "a.t", "b.t"]);
    assert_eq!((out.as_str(), code), ("", 0));
    let (out, _, code) =
        f.run(&["diff", "--no-index", "--no-textconv", "-S", "hello", "--name-only", "a.t", "b.t"]);
    assert_eq!((out.as_str(), code), ("b.t\n", 1));
}

#[test]
fn a_failing_program_is_fatal() {
    let f = Fixture::new("fail");
    let (out, err, code) = f.run(&["-c", "diff.up.textconv=false", "diff", "--no-index", "a.t", "b.t"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: unable to read files to diff\n", 128)
    );
}
