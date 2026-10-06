//! `git log -G<regex>` was compiled in `--grep`'s dialect — basic by default —
//! instead of as the extended expression `diffcore_pickaxe()` compiles.
//!
//! `-G` (and `-S` under `--pickaxe-regex`) is not a `grep_pat`:
//! `diffcore_pickaxe()` calls `regcomp()` itself with
//! `REG_EXTENDED | REG_NEWLINE`, adding `REG_ICASE` only for
//! `DIFF_PICKAXE_IGNORE_CASE`, which `-i` / `--regexp-ignore-case` sets
//! (diffcore-pickaxe.c:242-246, revision.c:2690-2692). `-E`/`-F`/`-P` pick the
//! dialect of `--grep`, `--author` and `--committer` only. zvcs built the `-G`
//! needle through the `--grep` compiler, so `+`, `|` and `(...)` were literals
//! under the default and `-F` turned `.` into a literal too.
//!
//! A needle that does not compile is `regcomp_or_die()`'s `die()`
//! (diffcore-pickaxe.c:219-228), raised from `diffcore_std()` for the first
//! commit the walk diffs, so an empty walk exits 0; zvcs died on
//! `--pickaxe-regex -S<bad>` while parsing, and `-G<bad>` under the default
//! dialect compiled as something else entirely.
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
    /// Three commits: `base`, `digits` (adds `beta 42`), `paren` (adds `(x)`).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-pickaxe-ere-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for (content, subject) in [
            ("alpha\n", "base"),
            ("alpha\nbeta 42\n", "digits"),
            ("alpha\nbeta 42\n(x)\n", "paren"),
        ] {
            std::fs::write(f.work.join("f"), content).unwrap();
            f.run(&["add", "f"]);
            f.run(&["commit", "-q", "-m", subject]);
        }
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
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn subjects(&self, extra: &[&str]) -> String {
        let mut args = vec!["log", "--format=%s"];
        args.extend_from_slice(extra);
        let (out, err, code) = self.run(&args);
        assert_eq!((err.as_str(), code), ("", 0), "{extra:?}");
        out
    }
}

#[test]
fn the_needle_is_an_extended_expression_whatever_grep_dialect_is_chosen() {
    let f = Fixture::new("ere");
    // `+` and `|` are operators.
    assert_eq!(f.subjects(&["-Gbeta [0-9]+"]), "digits\n");
    assert_eq!(f.subjects(&["-Gbeta 4|zzz"]), "digits\n");
    // `\(` is a literal parenthesis in an ERE; in a BRE it would open a group.
    assert_eq!(f.subjects(&["-G\\(x"]), "paren\n");
    // `-F` only changes `--grep`: `.` still matches the space.
    assert_eq!(f.subjects(&["-F", "-Gbeta.42"]), "digits\n");
    assert_eq!(f.subjects(&["--basic-regexp", "-Gbeta [0-9]+"]), "digits\n");
    // `-i` is `DIFF_PICKAXE_IGNORE_CASE`, i.e. `REG_ICASE` on the same ERE.
    assert_eq!(f.subjects(&["-i", "-GBETA [0-9]+"]), "digits\n");
}

/// `regerror()`'s text is the C library's, since git compiles with the platform
/// `regcomp()`: Darwin's wording on macOS, glibc's on Linux (measured with glibc
/// 2.36's `regcomp(3)`/`regerror(3)`, which git 2.39 on the same system prints).
fn regerror(darwin: &'static str, glibc: &'static str) -> &'static str {
    if cfg!(all(target_os = "linux", target_env = "gnu")) { glibc } else { darwin }
}

#[test]
fn a_bad_needle_dies_only_once_a_commit_is_diffed() {
    let f = Fixture::new("bad");
    for needle in [&["-G("][..], &["--pickaxe-regex", "-S["][..]] {
        let mut empty = vec!["log", "HEAD..HEAD"];
        empty.extend_from_slice(needle);
        assert_eq!(f.run(&empty), (String::new(), String::new(), 0), "{needle:?}");
    }
    assert_eq!(
        f.run(&["log", "--format=%s", "-G("]),
        (String::new(), format!("fatal: invalid regex: {}\n", regerror("parentheses not balanced", "Unmatched ( or \\(")), 128)
    );
    assert_eq!(
        f.run(&["log", "--format=%s", "--pickaxe-regex", "-S["]),
        (String::new(), format!("fatal: invalid regex: {}\n", regerror("brackets ([ ]) not balanced", "Invalid regular expression")), 128)
    );
}
