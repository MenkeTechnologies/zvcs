//! An external diff program runs at the top of the worktree.
//!
//! `setup_git_directory()` moves git to the top of the worktree before the
//! command runs, and `run_external_diff()` (diff.c:4777) starts the program
//! there with no directory of its own. The worktree paths it is handed — a
//! side `prepare_temp_file()` borrows from the worktree (diff.c:4714-4749),
//! such as `git diff`'s post-image or range-diff's `a` — are relative to that
//! top, and so is the `lstat()` that decides whether such a file exists. zvcs
//! ran the program in the directory the command was started from, so from a
//! subdirectory the program could not open `sub/x`, and range-diff read
//! `sub/a` instead of the top-level `a`.
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
    /// `t1`/`t2` each carry one commit rewriting line 3 of `g` differently, so
    /// range-diff shows one `!` pair; `main` tracks `sub/x`, which the worktree
    /// then modifies. The top level holds `a` = `left`, `sub/` holds its own `a`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-ext-diff-toplevel-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };
        let g = |three: &str| -> String {
            (1..=20).map(|n| if n == 3 { format!("{three}\n") } else { format!("{n}\n") }).collect()
        };
        f.run_in(&f.work, &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("g"), g("3")).unwrap();
        std::fs::write(f.work.join("sub/x"), "x\n").unwrap();
        f.run_in(&f.work, &["add", "g", "sub/x"]);
        f.run_in(&f.work, &["commit", "-q", "-m", "init"]);
        for (branch, three) in [("t1", "three"), ("t2", "THREE")] {
            f.run_in(&f.work, &["checkout", "-q", "-b", branch, "main"]);
            std::fs::write(f.work.join("g"), g(three)).unwrap();
            f.run_in(&f.work, &["commit", "-q", "-am", "change g"]);
        }
        f.run_in(&f.work, &["checkout", "-q", "main"]);
        std::fs::write(f.work.join("sub/x"), "x\ny\n").unwrap();
        std::fs::write(f.work.join("a"), "left\n").unwrap();
        std::fs::write(f.work.join("sub/a"), "in-sub\n").unwrap();
        let script = f.root.join("ext.sh");
        std::fs::write(&script, "#!/bin/sh\necho \"$(basename \"$PWD\") $1 $2|$5\"\ncat \"$2\" \"$5\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        f
    }

    fn script(&self) -> String {
        self.root.join("ext.sh").to_str().unwrap().to_owned()
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_EXTERNAL_DIFF")
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

#[test]
fn git_diff_from_a_subdirectory_runs_the_program_at_the_top() {
    let f = Fixture::new("diff");
    let pgm = format!("diff.external={}", f.script());
    // `$5` is the worktree's own `sub/x`, which only resolves from the top.
    let (out, err, code) = f.run_in(&f.work.join("sub"), &["-c", &pgm, "diff"]);
    let (first, rest) = out.split_once('\n').unwrap();
    let (head, new) = first.split_once('|').unwrap();
    assert_eq!((head.split(' ').take(2).collect::<Vec<_>>(), new), (vec!["work", "sub/x"], "sub/x"));
    assert_eq!((rest, err.as_str(), code), ("x\nx\ny\n", "", 0));
}

#[test]
fn range_diff_from_a_subdirectory_reads_the_top_level_a() {
    let f = Fixture::new("range");
    let pgm = format!("diff.external={}", f.script());
    let (out, err, code) =
        f.run_in(&f.work.join("sub"), &["-c", &pgm, "range-diff", "--ext-diff", "main..t1", "main..t2"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("1:  19cb6cf ! 1:  3929a89 change g\nwork a a|/dev/null\nleft\n", "", 0)
    );
}
