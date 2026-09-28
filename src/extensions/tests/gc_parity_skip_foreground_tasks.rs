//! `git gc --skip-foreground-tasks`.
//!
//! `OPT_HIDDEN_BOOL(0, "skip-foreground-tasks", …)` (builtin/gc.c:889) is what the
//! `gc` task of `maintenance run` hands its background child
//! (builtin/gc.c:1253-1272), having run `pack-refs` and `reflog expire` itself;
//! `if (opts.detach <= 0 && !skip_foreground_tasks) gc_foreground_tasks(…)`
//! (builtin/gc.c:1012) then leaves both out. zvcs refused the option as unknown,
//! exit 129.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-gc-skip-foreground-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
        f.run(&["branch", "side"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
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
fn skip_foreground_tasks_leaves_the_refs_loose() {
    let f = Fixture::new("skip");
    let git = f.work.join(".git");
    assert_eq!(f.run(&["gc", "--skip-foreground-tasks", "--quiet"]), (String::new(), String::new(), 0));
    assert!(!git.join("packed-refs").exists());
    assert!(git.join("refs/heads/side").exists());
    // The repack still ran.
    assert!(std::fs::read_dir(git.join("objects/pack")).unwrap().flatten().any(|e| {
        e.file_name().to_string_lossy().ends_with(".pack")
    }));

    // `--no-skip-foreground-tasks` puts them back.
    assert_eq!(f.run(&["gc", "--skip-foreground-tasks", "--no-skip-foreground-tasks", "--quiet"]), (String::new(), String::new(), 0));
    assert!(git.join("packed-refs").exists());
    assert!(!git.join("refs/heads/side").exists());
}
