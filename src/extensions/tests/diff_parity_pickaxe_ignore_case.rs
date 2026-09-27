//! `git diff -i` (and the `--grep` dialect flags) were refused as unsupported.
//!
//! `git diff` hands its argv to `setup_revisions()`, whose
//! `handle_revision_opt()` takes `-i` / `--regexp-ignore-case` — setting
//! `DIFF_PICKAXE_IGNORE_CASE` as well as `--grep`'s `ignore_case` — and
//! `-E`/`-F`/`-P`/`--basic-regexp` with their long spellings, which only pick
//! `grep_filter.pattern_type_option` (revision.c:2686-2696). A diff never
//! greps commits, so the dialect flags are accepted and inert, while `-i`
//! makes `diffcore_pickaxe()` fold case: a kwset over `tolower_trans_tbl` for
//! a plain `-S`, `REG_ICASE` for `-G` and `--pickaxe-regex`
//! (diffcore-pickaxe.c:242-272).
//!
//! Expectations measured from stock git 2.55.0 under the same environment
//! (`LC_ALL=C`: the fold is ASCII-only).

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
    /// `one` gains `Beta.Gamma`, `two` gains `zeta`; both edits sit in the
    /// work tree against the commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-diff-pickaxe-icase-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("one"), "alpha\n").unwrap();
        std::fs::write(f.work.join("two"), "alpha\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("one"), "alpha\nBeta.Gamma\n").unwrap();
        std::fs::write(f.work.join("two"), "alpha\nzeta\n").unwrap();
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

    fn names(&self, extra: &[&str]) -> String {
        let mut args = vec!["diff", "--name-only"];
        args.extend_from_slice(extra);
        let (out, err, code) = self.run(&args);
        assert_eq!((err.as_str(), code), ("", 0), "{extra:?}");
        out
    }
}

#[test]
fn ignore_case_folds_every_pickaxe_kind() {
    let f = Fixture::new("fold");
    assert_eq!(f.names(&["-SBETA.GAMMA"]), "");
    assert_eq!(f.names(&["-i", "-SBETA.GAMMA"]), "one\n");
    // Still a literal under `-i`: the `.` matches only a dot.
    assert_eq!(f.names(&["-i", "-SBETAXGAMMA"]), "");
    assert_eq!(f.names(&["--regexp-ignore-case", "-GBETA|ZETA"]), "one\ntwo\n");
    assert_eq!(f.names(&["-i", "--pickaxe-regex", "-SZ[E]TA"]), "two\n");
    assert_eq!(f.names(&["HEAD", "-i", "-GZETA"]), "two\n");
}

#[test]
fn the_grep_dialect_flags_are_accepted_and_inert() {
    let f = Fixture::new("dialect");
    let all = f.names(&[]);
    assert_eq!(all, "one\ntwo\n");
    for flag in [
        "-E",
        "-F",
        "-P",
        "--basic-regexp",
        "--extended-regexp",
        "--fixed-strings",
        "--perl-regexp",
    ] {
        assert_eq!(f.names(&[flag]), all, "{flag}");
    }
    // `-G` stays extended whatever the dialect: `|` is alternation under `-F`.
    assert_eq!(f.names(&["-F", "-Gzeta|nothing"]), "two\n");
}
