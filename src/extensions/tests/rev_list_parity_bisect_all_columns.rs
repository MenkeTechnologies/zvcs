//! `rev-list --bisect-all` with `--parents` or `--children`.
//!
//! `show_commit()` writes the object name, then the parents, then the
//! children, and only then `show_decorations()` (builtin/rev-list.c:289-305),
//! so the `(refs, dist=<n>)` list comes last on the line. The children are
//! the ones `set_children()` recorded at the end of `prepare_revision_walk()`
//! (revision.c:4026-4027), over the date-ordered list, before
//! `find_bisection()` reorders it. zvcs wrote the decorations straight after
//! the object name and collected the children from the reordered list.
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
    /// one; `side` forks from it; two on main; main merges side. Each commit a
    /// minute after the last.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rl-bisect-all-columns-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(0, &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "1\n").unwrap();
        f.run(1, &["add", "a"]);
        f.run(1, &["commit", "-q", "-m", "one"]);
        f.run(2, &["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("s"), "s\n").unwrap();
        f.run(2, &["add", "s"]);
        f.run(2, &["commit", "-q", "-m", "side"]);
        f.run(3, &["checkout", "-q", "main"]);
        std::fs::write(f.work.join("a"), "2\n").unwrap();
        f.run(3, &["commit", "-q", "-am", "two"]);
        f.run(4, &["merge", "-q", "side"]);
        f
    }

    fn rev(&self, spec: &str) -> String {
        self.run(9, &["rev-parse", spec]).0.trim_end().to_string()
    }

    fn run(&self, minute: u64, args: &[&str]) -> (String, String, i32) {
        let date = format!("{} +0000", 1_700_000_000 + 60 * minute);
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
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_MERGE_AUTOEDIT", "no")
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
fn parents_come_before_the_decorations() {
    let f = Fixture::new("parents");
    let (merge, two, side, one) = (f.rev("main"), f.rev("main^1"), f.rev("side"), f.rev("main~2"));
    let want = format!(
        "{side} {one} (side, dist=2)\n\
         {two} {one} (dist=2)\n\
         {one} (dist=1)\n\
         {merge} {two} {side} (HEAD -> main, dist=0)\n"
    );
    assert_eq!(
        f.run(9, &["rev-list", "--bisect-all", "--parents", "main"]),
        (want, String::new(), 0)
    );
}

#[test]
fn children_are_collected_before_the_search_reorders() {
    let f = Fixture::new("children");
    let (merge, two, side, one) = (f.rev("main"), f.rev("main^1"), f.rev("side"), f.rev("main~2"));
    let want = format!(
        "{side} {merge} (side, dist=2)\n\
         {two} {merge} (dist=2)\n\
         {one} {side} {two} (dist=1)\n\
         {merge} (HEAD -> main, dist=0)\n"
    );
    assert_eq!(
        f.run(9, &["rev-list", "--bisect-all", "--children", "main"]),
        (want, String::new(), 0)
    );
}
