//! `%d`/`%D` in a format rendered without a `rev_info` of its own.
//!
//! `format_decorations()` looks the commit up through `get_name_decoration()`
//! (log-tree.c:94-98), which runs `load_ref_decorations(NULL,
//! DECORATE_SHORT_REFS)` on first use. `rev-list --format` never sets up
//! decorations, so that lazy load is the one that happens — with a NULL filter,
//! which `add_ref_decoration()` does not consult (log-tree.c:153-154). Every
//! ref decorates, `refs/bisect/*` included, and `log.excludeDecoration` (read
//! only by `git log`'s `set_default_decoration_filter()`) has no say.
//!
//! zvcs rendered both placeholders empty under `rev-list`.
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
    /// `one` carries the annotated tag `v1` and the branch `side`; `two` is
    /// `main` and also `refs/bisect/bad`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-format-deco-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.stdout(&["init", "-q", "-b", "main", "."]);
        f.stdout(&["commit", "-q", "--allow-empty", "-m", "one"]);
        f.stdout(&["tag", "-a", "-m", "ann", "v1"]);
        f.stdout(&["branch", "side"]);
        f.stdout(&["commit", "-q", "--allow-empty", "-m", "two"]);
        f.stdout(&["update-ref", "refs/bisect/bad", "HEAD"]);
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
            .env("GIT_PAGER", "cat")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }

    fn oid(&self, rev: &str) -> String {
        self.stdout(&["rev-parse", rev]).trim_end().to_string()
    }
}

#[test]
fn rev_list_decorates_from_every_ref() {
    let f = Fixture::new("all");
    let (two, one) = (f.oid("main"), f.oid("main~1"));
    assert_eq!(
        f.stdout(&["rev-list", "--format=%s:%d|%D", "main"]),
        format!(
            "commit {two}\ntwo: (HEAD -> main, refs/bisect/bad)|HEAD -> main, refs/bisect/bad\n\
             commit {one}\none: (tag: v1, side)|tag: v1, side\n"
        )
    );
    // `git log` builds its own filtered set, which leaves `refs/bisect` out.
    assert_eq!(
        f.stdout(&["log", "--format=%s:%d", "main"]),
        "two: (HEAD -> main)\none: (tag: v1, side)\n"
    );
}

#[test]
fn rev_list_ignores_log_exclude_decoration() {
    let f = Fixture::new("exclude");
    let (two, one) = (f.oid("main"), f.oid("main~1"));
    assert_eq!(
        f.stdout(&["-c", "log.excludeDecoration=refs/tags", "rev-list", "--format=%s:%d", "main"]),
        format!("commit {two}\ntwo: (HEAD -> main, refs/bisect/bad)\ncommit {one}\none: (tag: v1, side)\n")
    );
}
