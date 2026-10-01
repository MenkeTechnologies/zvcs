//! `git log --follow` across merges (git 2.56).
//!
//! Up to 2.55 `--follow` kept one global path and rewrote it whenever a commit
//! turned out to have renamed it, so after walking one side of a merge the other
//! side was searched under the first side's old name. 2.56 records a path per
//! commit (`follow_pathspec_slab`, log-tree.c:1093-1150): before a commit is
//! shown its recorded path is restored (log-tree.c:1279-1287), and afterwards a
//! merge works the path out for each parent with a follow-renames diff of that
//! parent against the merge, while a single parent inherits the current path
//! (log-tree.c:1302-1314).
//!
//! The three histories are the ones git's own t4219-log-follow-merge.sh builds;
//! expectations were measured from stock git 2.56.0 on the same commits.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    tick: std::cell::Cell<u64>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-follow-merge-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        Fixture { root, tick: std::cell::Cell::new(0) }
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn write(&self, path: &str, body: &str) {
        let p = self.root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let date = format!("{} -0700", 1_112_911_993 + self.tick.get() * 60);
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Commit in `dir`, a minute after the previous commit.
    fn commit(&self, dir: &Path, args: &[&str]) {
        self.tick.set(self.tick.get() + 1);
        let mut argv = vec!["commit", "-q"];
        argv.extend_from_slice(args);
        self.git(dir, &argv);
    }

    fn init(&self, name: &str) -> PathBuf {
        self.git(&self.root, &["init", "-q", "-b", "master", name]);
        self.dir(name)
    }
}

#[test]
fn follows_into_a_subtree_merged_history() {
    let f = Fixture::new("subtree");
    let inner = f.init("inner");
    f.write("inner/inner.txt", "inner\n");
    f.git(&inner, &["add", "inner.txt"]);
    f.commit(&inner, &["-m", "inner init"]);
    let outer = f.init("outer");
    f.write("outer/outer.txt", "outer\n");
    f.git(&outer, &["add", "outer.txt"]);
    f.commit(&outer, &["-m", "outer init"]);
    f.git(&outer, &["fetch", "-q", "../inner", "master"]);
    f.git(
        &outer,
        &["merge", "-q", "-s", "ours", "--no-commit", "--allow-unrelated-histories", "FETCH_HEAD"],
    );
    f.git(&outer, &["read-tree", "--prefix=inner/", "-u", "FETCH_HEAD"]);
    f.commit(&outer, &["-m", "Merge inner repo into inner/ subdirectory"]);

    assert_eq!(
        f.git(&outer, &["log", "--follow", "--pretty=tformat:%s", "inner/inner.txt"]),
        "inner init\n"
    );
}

#[test]
fn follows_both_sides_renaming_to_the_same_name() {
    let f = Fixture::new("both");
    let foo = f.init("foo");
    f.write("foo/foo/README", "foo readme\n");
    f.git(&foo, &["add", "foo/README"]);
    f.commit(&foo, &["-m", "add foo README"]);
    f.git(&foo, &["mv", "foo/README", "README"]);
    f.commit(&foo, &["-m", "promote foo README to toplevel"]);
    f.write("foo/foo.c", "foo c\n");
    f.git(&foo, &["add", "foo.c"]);
    f.commit(&foo, &["-m", "add foo C impl"]);
    let bar = f.init("bar");
    f.write("bar/bar/README", "bar readme\n");
    f.git(&bar, &["add", "bar/README"]);
    f.commit(&bar, &["-m", "add bar README"]);
    f.git(&bar, &["mv", "bar/README", "README"]);
    f.commit(&bar, &["-m", "promote bar README to toplevel"]);
    f.write("bar/bar.c", "bar c\n");
    f.git(&bar, &["add", "bar.c"]);
    f.commit(&bar, &["-m", "add bar C impl"]);
    f.git(&foo, &["fetch", "-q", "../bar", "master"]);
    f.git(
        &foo,
        &["merge", "-q", "-s", "ours", "--no-commit", "--allow-unrelated-histories", "FETCH_HEAD"],
    );
    f.git(&foo, &["checkout", "FETCH_HEAD", "--", "bar.c"]);
    f.commit(&foo, &["-m", "merge bar into foo"]);

    assert_eq!(
        f.git(&foo, &["log", "--follow", "--pretty=tformat:%s", "README"]),
        "promote bar README to toplevel\n\
         add bar README\n\
         promote foo README to toplevel\n\
         add foo README\n"
    );
}

#[test]
fn follows_each_side_of_a_fork_under_its_own_name() {
    let f = Fixture::new("diamond");
    let d = f.init("diamond");
    let lines = |three: &str| format!("line 1\nline 2\n{three}\nline 4\nline 5\n");
    f.write("diamond/path0", &lines("line 3"));
    f.git(&d, &["add", "path0"]);
    f.commit(&d, &["-m", "A: add path0"]);
    f.git(&d, &["checkout", "-q", "-b", "upper"]);
    f.write("diamond/path0", &lines("line 3 modified by B"));
    f.commit(&d, &["-am", "B: modify path0 on upper"]);
    f.git(&d, &["mv", "path0", "path1"]);
    f.commit(&d, &["-m", "X: rename path0 to path1"]);
    f.git(&d, &["checkout", "-q", "-b", "lower", "master"]);
    f.write("diamond/path0", &lines("line 3 modified by C"));
    f.commit(&d, &["-am", "C: modify path0 on lower"]);
    f.git(&d, &["mv", "path0", "path2"]);
    f.commit(&d, &["-m", "Y: rename path0 to path2"]);
    f.git(&d, &["checkout", "-q", "upper"]);
    f.git(&d, &["merge", "-q", "-s", "ours", "--no-commit", "lower"]);
    f.git(&d, &["rm", "-q", "path1"]);
    f.write("diamond/path", &lines("line 3 merged"));
    f.git(&d, &["add", "path"]);
    f.commit(&d, &["-m", "M: merge with rename to path"]);
    f.write("diamond/path", &lines("line 3 merged again"));
    f.commit(&d, &["-am", "Z: modify path"]);

    assert_eq!(
        f.git(&d, &["log", "--follow", "--pretty=tformat:%s", "path"]),
        "Z: modify path\n\
         Y: rename path0 to path2\n\
         C: modify path0 on lower\n\
         X: rename path0 to path1\n\
         B: modify path0 on upper\n\
         A: add path0\n"
    );
    assert_eq!(
        f.git(&d, &["log", "--follow", "--name-status", "--pretty=tformat:%s", "path"]),
        "Z: modify path\n\nM\tpath\n\
         Y: rename path0 to path2\n\nR100\tpath0\tpath2\n\
         C: modify path0 on lower\n\nM\tpath0\n\
         X: rename path0 to path1\n\nR100\tpath0\tpath1\n\
         B: modify path0 on upper\n\nM\tpath0\n\
         A: add path0\n\nA\tpath0\n"
    );
}
