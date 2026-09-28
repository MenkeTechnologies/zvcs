//! `git diff --no-index` takes `-S`/`-G` and clumped short options.
//!
//! The no-index parser is `add_diff_options()`'s whole table
//! (diff-no-index.c:372), so `parse_short_opt()` reads `-bw` as `-b -w`, and a
//! required-value letter such as `-S` swallows the rest of the clump
//! (`-Sfoo`) or the next word. `-S`/`-G` record the needle and its kind bit
//! (diff.c:5879-5902, an empty needle being the callback's `error()`),
//! `diff_setup_done()` refuses two kinds, `-G` with `--pickaxe-regex` and
//! `--pickaxe-all` with `--find-object` (diff.c:5263-5273), and
//! `diffcore_pickaxe()` (diffcore-pickaxe.c:130-218) keeps the pairs whose
//! occurrence count changed (`-S`) or whose changed lines match (`-G`, which
//! skips a binary pair without `--text`) — or, under `--pickaxe-all`, the whole
//! queue when any pair hits. zvcs refused every one of these with
//! `unsupported option`.
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
    /// No repository: `p`/`q`, and two directories `L`/`R` whose `one` gains a
    /// `y` line, whose `two` gains a `foo` line and whose `b` is a binary pair.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-no-index-pickaxe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("L")).unwrap();
        std::fs::create_dir_all(root.join("R")).unwrap();
        let w = |p: &str, d: &[u8]| std::fs::write(root.join(p), d).unwrap();
        w("p", b"a\nfoo bar\nc\n");
        w("q", b"a\nFOO  bar\nc\nd\n");
        w("L/one", b"x\n");
        w("R/one", b"x\ny\n");
        w("L/two", b"k\n");
        w("R/two", b"k\nfoo\n");
        w("L/b", b"bin\0x");
        w("R/b", b"bin\0y");
        Fixture { root }
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(["diff", "--no-index"])
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", self.root.parent().unwrap())
            .env("LC_ALL", "C")
            .env_remove("COLUMNS")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 1), "{args:?}");
        out
    }

    fn first_err_line(&self, args: &[&str]) -> (String, i32) {
        let (out, err, code) = self.run(args);
        assert_eq!(out, "", "{args:?}");
        (err.lines().next().unwrap_or_default().to_owned(), code)
    }
}

#[test]
fn s_keeps_the_pairs_whose_count_changed() {
    let f = Fixture::new("s");
    assert_eq!(
        f.stdout(&["--stat", "-Sfoo", "L", "R"]),
        " {L => R}/two | 1 +\n 1 file changed, 1 insertion(+)\n"
    );
    // One hit keeps the whole queue.
    assert_eq!(
        f.stdout(&["--stat", "-Sfoo", "--pickaxe-all", "L", "R"]),
        " {L => R}/b   | Bin 5 -> 5 bytes\n {L => R}/one |   1 +\n {L => R}/two |   1 +\n \
         3 files changed, 2 insertions(+)\n"
    );
    // `foo` becomes `FOO`, so the count drops; `bar` appears once on each side.
    assert!(f.stdout(&["-S", "foo", "p", "q"]).starts_with("diff --git a/p b/q\n"));
    assert_eq!(f.run(&["-Sbar", "p", "q"]), (String::new(), String::new(), 0));
}

#[test]
fn g_matches_changed_lines_and_skips_binary_pairs_without_text() {
    let f = Fixture::new("g");
    assert_eq!(
        f.stdout(&["--stat", "-G", "y", "L", "R"]),
        " {L => R}/one | 1 +\n 1 file changed, 1 insertion(+)\n"
    );
    assert_eq!(
        f.stdout(&["--stat", "-Gy", "-a", "L", "R"]),
        " {L => R}/b   | Bin 5 -> 5 bytes\n {L => R}/one |   1 +\n 2 files changed, 1 insertion(+)\n"
    );
}

#[test]
fn short_options_clump() {
    let f = Fixture::new("clump");
    assert_eq!(
        f.stdout(&["-bw", "--stat", "p", "q"]),
        " p => q | 3 ++-\n 1 file changed, 2 insertions(+), 1 deletion(-)\n"
    );
    // `-i` is `setup_revisions()`'s, not the diff table's: the clump stops there.
    assert_eq!(f.first_err_line(&["-wi", "p", "q"]), ("error: unknown switch `i'".to_owned(), 129));
}

#[test]
fn pickaxe_refusals() {
    let f = Fixture::new("refuse");
    assert_eq!(
        f.first_err_line(&["-Sfoo", "-Gd", "p", "q"]),
        ("fatal: options '-G', '-S', and '--find-object' cannot be used together".to_owned(), 128)
    );
    assert_eq!(
        f.first_err_line(&["--pickaxe-regex", "-Gd", "p", "q"]),
        (
            "fatal: options '-G' and '--pickaxe-regex' cannot be used together, use '--pickaxe-regex' with '-S'"
                .to_owned(),
            128
        )
    );
    assert_eq!(
        f.first_err_line(&["-S", "", "p", "q"]),
        ("error: -S requires a non-empty argument".to_owned(), 129)
    );
    assert_eq!(f.first_err_line(&["-S"]), ("error: switch `S' requires a value".to_owned(), 129));
}
