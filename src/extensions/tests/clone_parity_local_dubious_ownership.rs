//! A local clone refuses to copy another user's object store.
//!
//! `copy_or_link_directory()` opens with
//! `die_upon_dubious_ownership(NULL, NULL, src_repo)` (builtin/clone.c:253-265):
//! hardlinking or copying a store someone else can rewrite is refused unless
//! `safe.directory` names the source repository directory itself. `--shared`,
//! `--no-local` and a `file://` URL never reach it. zvcs copied the store
//! regardless.
//!
//! `GIT_TEST_ASSUME_DIFFERENT_OWNER=1` makes the check treat the source as
//! foreign-owned, as it does in stock.
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
    /// A committed repository `r` beside the clones.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-clone-dubious-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("r")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        f.git(false, &["-C", "r", "init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("r/f"), "a\n").unwrap();
        f.git(false, &["-C", "r", "add", "f"]);
        f.git(false, &["-C", "r", "-c", "maintenance.auto=false", "commit", "-q", "-m", "i"]);
        f
    }

    fn git(&self, foreign: bool, args: &[&str]) -> (String, String, i32) {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .env("LC_ALL", "C");
        if foreign {
            cmd.env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1");
        }
        let out = cmd.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn refusal(&self) -> String {
        let src = self.root.join("r/.git").display().to_string();
        format!(
            "fatal: detected dubious ownership in repository at '{src}'\n\
             To add an exception for this directory, call:\n\
             \n\
             \tgit config --global --add safe.directory {src}\n"
        )
    }
}

#[test]
fn copying_or_linking_a_foreign_store_is_refused() {
    let f = Fixture::new("refused");
    for (args, dst) in [
        (&["clone", "-q", "r", "c"][..], "c"),
        (&["clone", "-q", "--no-hardlinks", "r", "c1"], "c1"),
        // Naming the work tree is not naming the repository directory.
        (&["-c", "safe.directory=WORK", "clone", "-q", "r", "c2"], "c2"),
    ] {
        let work = format!("safe.directory={}", f.root.join("r").display());
        let args: Vec<&str> = args.iter().map(|a| if *a == "safe.directory=WORK" { work.as_str() } else { a }).collect();
        assert_eq!(f.git(true, &args), (String::new(), f.refusal(), 128), "{args:?}");
        assert!(!f.root.join(dst).exists(), "{dst} was left behind");
    }
}

#[test]
fn the_paths_that_copy_nothing_are_not() {
    let f = Fixture::new("allowed");
    let file_url = format!("file://{}", f.root.join("r").display());
    let safe = format!("safe.directory={}", f.root.join("r/.git").display());
    for args in [
        &["clone", "-q", "-s", "r", "c4"][..],
        &["clone", "-q", "--no-local", "r", "c5"],
        &["clone", "-q", &file_url, "c8"],
        &["-c", &safe, "clone", "-q", "r", "c6"],
    ] {
        assert_eq!(f.git(true, args), (String::new(), String::new(), 0), "{args:?}");
    }
}
