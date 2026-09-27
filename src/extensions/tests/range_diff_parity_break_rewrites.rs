//! `git range-diff -B` can never break the outer filepair.
//!
//! `patch_diff()` queues one pair whose sides `get_filespec()` names `a` and
//! `b` (range-diff.c:477-495), and `diffcore_break()` only splits a pair whose
//! two paths are equal (diffcore-break.c:188-191), so `-B` / `--break-rewrites`
//! leaves every format byte-identical to the flagless run — even for two
//! patches that share nothing. Its `<n>/<m>` value is still validated by
//! `diff_opt_break_rewrites()` at parse time (129). zvcs stopped every run with
//! `fatal: unsupported flag "-B"` and let a malformed value through to it.
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
    /// `t1` adds `x` holding 1000..=1060, `t2` adds `y` holding 5000..=5060, both
    /// with the subject `change`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-range-diff-break-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        let seq = |from: u32| (from..=from + 60).map(|n| format!("{n}\n")).collect::<String>();
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("base"), "base\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "t1"]);
        std::fs::write(f.work.join("x"), seq(1000)).unwrap();
        f.run(&["add", "x"]);
        f.run(&["commit", "-q", "-m", "change"]);
        f.run(&["checkout", "-q", "-b", "t2", "main"]);
        std::fs::write(f.work.join("y"), seq(5000)).unwrap();
        f.run(&["add", "y"]);
        f.run(&["commit", "-q", "-m", "change"]);
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
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@x")
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
}

#[test]
fn a_complete_rewrite_of_the_patch_is_not_broken() {
    let f = Fixture::new("rewrite");
    let base = ["range-diff", "--creation-factor=1000", "main..t1", "main..t2"];
    let (plain, err, code) = f.run(&base);
    assert_eq!((err.as_str(), code), ("", 0));
    assert!(plain.starts_with("1:  7fce7b8 ! 1:  6a2ab04 change\n    @@ Metadata\n"), "{plain}");
    assert!(plain.contains("\n    - ## x (new) ##\n    + ## y (new) ##\n"), "{plain}");
    for extra in [&["-B"][..], &["--break-rewrites"], &["-B50/60"], &["--break-rewrites=5", "--stat"]] {
        let mut args = vec![base[0]];
        args.extend_from_slice(extra);
        args.extend_from_slice(&base[1..]);
        let mut flagless = base.to_vec();
        flagless.splice(1..1, extra[1..].iter().copied());
        assert_eq!(f.run(&args), f.run(&flagless), "{extra:?}");
    }
}

#[test]
fn a_malformed_score_is_a_parse_error() {
    let f = Fixture::new("score");
    for opt in ["-Bfoo", "--break-rewrites=x", "-B50/x"] {
        let (out, err, code) = f.run(&["range-diff", opt, "main..t1", "main..t2"]);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "error: break-rewrites expects <n>/<m> form\n", 129),
            "{opt}"
        );
    }
}
