//! `git log -i -S<needle>` counted the needle case-sensitively.
//!
//! `-i` / `--regexp-ignore-case` sets `DIFF_PICKAXE_IGNORE_CASE` as well as
//! `--grep`'s `ignore_case` (revision.c:2690-2692). `diffcore_pickaxe()` then
//! counts a plain `-S` needle with a kwset over `tolower_trans_tbl` (a
//! non-ASCII needle goes through a quoted `REG_ICASE` regex instead), and
//! compiles a `--pickaxe-regex` needle with `REG_ICASE` (diffcore-pickaxe.c:
//! 242-272). zvcs applied `-i` to `-G` only, so `-i -Sbeta` never found a
//! commit that added `Beta`.
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
    /// `upper` adds `Beta.Gamma`, `lower` rewrites it as `beta.gamma` — the same
    /// line under a case fold, so only a case-sensitive count sees `lower` change.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-pickaxe-icase-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for (content, subject) in [
            ("alpha\n", "base"),
            ("alpha\nBeta.Gamma\n", "upper"),
            ("alpha\nbeta.gamma\n", "lower"),
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
fn ignore_case_folds_the_literal_needle() {
    let f = Fixture::new("literal");
    assert_eq!(f.subjects(&["-Sbeta.gamma"]), "lower\n");
    assert_eq!(f.subjects(&["-i", "-Sbeta.gamma"]), "upper\n");
    assert_eq!(f.subjects(&["--regexp-ignore-case", "-SBETA.GAMMA"]), "upper\n");
    // Still a literal: the `.` does not match any byte.
    assert_eq!(f.subjects(&["-i", "-SbetaXgamma"]), "");
}

#[test]
fn ignore_case_reaches_a_pickaxe_regex() {
    let f = Fixture::new("regex");
    assert_eq!(f.subjects(&["--pickaxe-regex", "-SBETA.G"]), "");
    assert_eq!(f.subjects(&["-i", "--pickaxe-regex", "-SBETA.G"]), "upper\n");
}
