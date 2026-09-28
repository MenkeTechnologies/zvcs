//! `git notes merge`'s two streams, captured together.
//!
//! `notes-merge.c` reports each note it handles with `printf()` — `Auto-merging
//! notes for <oid>`, `CONFLICT (content): …` (notes-merge.c:405-436) — while
//! `builtin/notes.c` prints the conflict summary with `fprintf(stderr, …)`
//! (builtin/notes.c:1004-1007). Off a terminal stdout is fully buffered until
//! `exit()`, so a caller reading `2>&1` sees the summary first. zvcs wrote the
//! stdout lines as it went and put the summary last.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-notes-merge-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "c1"]);
        f.run(&["notes", "add", "-m", "base", "HEAD"]);
        f.run(&["update-ref", "refs/notes/other", "refs/notes/commits"]);
        f.run(&["notes", "add", "-f", "-m", "ours", "HEAD"]);
        f.run(&["notes", "--ref", "other", "add", "-f", "-m", "theirs", "HEAD"]);
        f
    }

    /// stdout and stderr on one file, as `2>&1` gives them: the dup shares the file
    /// offset, so the bytes land in the order the child wrote them.
    fn combined(&self, args: &[&str]) -> (String, i32) {
        let sink = self.root.join("capture");
        let writer = std::fs::File::create(&sink).unwrap();
        let mut child = Command::new(BIN)
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
            .stdin(Stdio::null())
            .stdout(writer.try_clone().unwrap())
            .stderr(writer)
            .spawn()
            .unwrap();
        let code = child.wait().unwrap().code().expect("no signal");
        (std::fs::read_to_string(&sink).unwrap(), code)
    }

    fn run(&self, args: &[&str]) -> String {
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
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[test]
fn the_conflict_summary_comes_before_the_buffered_report() {
    let f = Fixture::new("conflict");
    let head = f.run(&["rev-parse", "HEAD"]);
    let head = head.trim();
    assert_eq!(
        f.combined(&["notes", "merge", "other"]),
        (
            format!(
                "Automatic notes merge failed. Fix conflicts in .git/NOTES_MERGE_WORKTREE and commit \
                 the result with 'git notes merge --commit', or abort the merge with 'git notes merge \
                 --abort'.\nAuto-merging notes for {head}\nCONFLICT (content): Merge conflict in notes \
                 for object {head}\n"
            ),
            1
        )
    );
}
