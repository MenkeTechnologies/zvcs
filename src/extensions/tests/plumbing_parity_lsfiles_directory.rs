//! Three rules `git ls-files -o` follows that a flat walk plus a post-filter
//! does not get for free.
//!
//!   * The walk's entries are ordered by the names `dir_add_name()` stored, and a
//!     directory's was stored *with* its trailing `/` — `treat_directory()`
//!     appends the slash before adding it. `cmp_dir_entry()` is a plain
//!     `name_compare()` over those bytes (dir.c), so `path2-junk` sorts before
//!     `path2/`: `-` is 0x2D and `/` is 0x2F. Ordering the slashless names puts
//!     `path2` first instead.
//!   * `--directory` collapses a wholly untracked directory only where the
//!     pathspec is satisfied *by that directory*. Where the pathspec names
//!     something strictly below it — `match_pathspec_item()` answering
//!     `MATCHED_RECURSIVELY_LEADING_PATHSPEC` (dir.c:457-464) —
//!     `treat_directory()` returns `path_recurse` instead (dir.c:2074-2081), so
//!     `ls-files -o --directory untracked/deep/` reports `untracked/deep/`.
//!   * `--error-unmatch` reads `ps_matched`, and only the lines a `show_*`
//!     actually printed set it. A `-o`-only listing never walks the index at all
//!     (`if (!(show_cached || show_stage || show_deleted || show_modified))
//!     return;`, builtin/ls-files.c:417-418), so a pathspec naming a *tracked*
//!     path is unmatched there — while `show_dir_entry()` marks the walked ones
//!     through `dir_path_match()`, which drops the trailing `/` and re-offers it
//!     as the `is_dir` flag.
//!
//! Every expectation was read off stock git 2.55.0 in the same throwaway
//! repository, under the same pinned environment.
#![cfg(unix)]

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
        let root = std::env::temp_dir().join(format!("zvcs-lsf-dir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
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
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }

    fn touch(&self, rel: &str) {
        let path = self.work.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x\n").unwrap();
    }
}

/// A sibling whose name begins with a byte below `/` must sort before the
/// directory whose collapsed name ends in one.
#[test]
fn ls_files_others_directory_sorts_the_trailing_slash() {
    let f = Fixture::new("sort");
    f.touch("path2/file");
    f.touch("path2-junk");
    f.touch("path2.junk");
    // `.` is 0x2E, `-` is 0x2D, `/` is 0x2F — so the collapsed directory sorts
    // last of the three even though the bare name `path2` sorts first.
    assert_eq!(
        f.stdout(&["ls-files", "--others", "--directory"]),
        "path2-junk\npath2.junk\npath2/\n"
    );
}

/// A pathspec that names something below a wholly untracked directory keeps the
/// walk descending; one satisfied by the directory itself collapses there.
#[test]
fn ls_files_others_directory_descends_for_a_leading_pathspec() {
    let f = Fixture::new("leading");
    f.touch("partially_tracked/content");
    f.touch("partially_tracked/untracked_dir/file");
    f.touch("untracked/deep/path");
    f.touch("untracked/deep/foo.c");
    f.git(&["add", "partially_tracked/content"]);

    // The pathspec is the directory itself: collapse there.
    assert_eq!(
        f.stdout(&["ls-files", "-o", "--directory", "untracked/deep/"]),
        "untracked/deep/\n"
    );
    // A file below it: descend all the way to the file.
    assert_eq!(
        f.stdout(&[
            "ls-files",
            "-o",
            "--directory",
            "partially_tracked/",
            "untracked/deep/path",
        ]),
        "partially_tracked/untracked_dir/\nuntracked/deep/path\n"
    );
    // A glob below it: the wildcard is reached only after descending, and a
    // directory that the glob itself matches stops the descent.
    assert_eq!(
        f.stdout(&["ls-files", "--others", "--directory", "untracked/*.c"]),
        "untracked/deep/foo.c\n"
    );
    assert_eq!(
        f.stdout(&["ls-files", "--others", "--directory", "untracked/?*"]),
        "untracked/deep/\n"
    );
    // Without a deeper pathspec the whole directory still collapses at the top.
    assert_eq!(
        f.stdout(&["ls-files", "-o", "--directory", "partially_tracked/", "untracked/"]),
        "partially_tracked/untracked_dir/\nuntracked/\n"
    );
}

/// `--error-unmatch` under `-o` judges the pathspec against the *walked* entries
/// only: a tracked path is unmatched, an untracked one is matched.
#[test]
fn ls_files_others_error_unmatch_ignores_the_index() {
    let f = Fixture::new("unmatch");
    f.touch("tracked");
    f.touch("untracked");
    f.git(&["add", "tracked"]);

    // `-o` alone: the index pass never runs, so `tracked` matched nothing.
    let (out, err, code) = f.run(&["ls-files", "-o", "--error-unmatch", "tracked", "untracked"]);
    assert_eq!(out, "untracked\n");
    assert_eq!(
        err,
        "error: pathspec 'tracked' did not match any file(s) known to git\n\
         Did you forget to 'git add'?\n"
    );
    assert_eq!(code, 1);

    // `-c -o` does reach the index pass, so both are matched.
    assert_eq!(
        f.run(&["ls-files", "-c", "-o", "--error-unmatch", "tracked", "untracked"]),
        ("untracked\ntracked\n".to_string(), String::new(), 0)
    );

    // A collapsed directory satisfies the spec that produced it: `dir_path_match()`
    // strips the trailing `/` and matches `untracked_dir` as a directory.
    f.touch("untracked_dir/file");
    assert_eq!(
        f.run(&["ls-files", "-o", "--directory", "--error-unmatch", "untracked_dir"]),
        ("untracked_dir/\n".to_string(), String::new(), 0)
    );
}
