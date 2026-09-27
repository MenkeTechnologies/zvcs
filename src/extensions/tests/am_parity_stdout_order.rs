//! Where `git am`'s stdout lines land against its stderr, captured.
//!
//! `am` writes `Applying: <subject>` with `say()` (builtin/am.c:256-266) and
//! `Patch failed at <n> <subject>` with `printf_ln()` (builtin/am.c:1910),
//! both into stdio's stdout buffer, while `apply`'s errors and the resolve
//! hints go to unbuffered stderr. Off a terminal that buffer reaches the fd
//! only at `exit()` or at a `start_command()`'s `fflush(NULL)`
//! (run-command.c:743) — `build_fake_ancestor()`'s `git apply` child in the
//! three-way fallback (builtin/am.c:1562-1576) — plus merge-ort's
//! `diff_warn_rename_limit()` flush (diff.c:7040). So a plain failure prints
//! both lines after every error and hint, and a three-way one prints
//! `Applying:` early but `Patch failed` last. `rebase --apply` runs this as its
//! `am` child. zvcs wrote every line as it went.
//!
//! Both streams go to ONE file, the only way the interleaving is observable.
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::fs::File;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    /// `main` and `side` both rewrite `file` from a common base; `side` is
    /// exported as one patch in `<root>/p`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-am-stdout-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("file"), "side\n").unwrap();
        f.run(&["commit", "-q", "-am", "side"]);
        f.run(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("file"), "main\n").unwrap();
        f.run(&["commit", "-q", "-am", "main"]);
        f.run(&["format-patch", "-q", "-o", "../p", "main..side"]);
        f
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
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
            .env("GIT_EDITOR", "true")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        c
    }

    fn run(&self, args: &[&str]) {
        let out = self.command(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Both streams into one file, plus the exit code.
    fn merged(&self, args: &[&str]) -> (String, i32) {
        let sink = self.root.join("sink");
        let file = File::create(&sink).unwrap();
        let status = self
            .command(args)
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file))
            .status()
            .unwrap();
        (std::fs::read_to_string(&sink).unwrap(), status.code().expect("no signal"))
    }
}

const HINTS: &str = "\
hint: Use 'git am --show-current-patch=diff' to see the failed patch
hint: When you have resolved this problem, run \"git am --continue\".
hint: If you prefer to skip this patch, run \"git am --skip\" instead.
hint: To restore the original branch and stop patching, run \"git am --abort\".
hint: Disable this message with \"git config set advice.mergeConflict false\"
";

#[test]
fn a_plain_failure_prints_its_stdout_last() {
    let f = Fixture::new("plain");
    let want = format!(
        "error: patch failed: file:1\nerror: file: patch does not apply\n{HINTS}\
         Applying: side\nPatch failed at 0001 side\n"
    );
    assert_eq!(f.merged(&["am", "../p/0001-side.patch"]), (want, 128));
}

#[test]
fn a_three_way_failure_flushes_at_the_fake_ancestor_child() {
    let f = Fixture::new("three");
    let want = format!(
        "Applying: side\n\
         Using index info to reconstruct a base tree...\n\
         M\tfile\n\
         Falling back to patching base and 3-way merge...\n\
         Auto-merging file\n\
         CONFLICT (content): Merge conflict in file\n\
         error: Failed to merge in the changes.\n{HINTS}\
         Patch failed at 0001 side\n"
    );
    assert_eq!(f.merged(&["am", "-3", "../p/0001-side.patch"]), (want, 128));
    assert!(f.work.join(".git/rebase-apply").is_dir());
}
