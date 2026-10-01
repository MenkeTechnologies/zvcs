//! `git sparse-checkout set` narrowing the cone past an intent-to-add path, as
//! git 2.56 does it.
//!
//! `clean_tracked_sparse_directories()` (builtin/sparse-checkout.c:115-204)
//! builds an in-memory sparse index and warns about, or removes, every
//! sparse-directory entry still on disk. `convert_to_sparse()` rebuilds the
//! cache-tree first, and an intent-to-add entry invalidates every cache-tree
//! node above it (cache-tree.c:436-441, :472-481, :517). 2.55 recursed into
//! such a node with its `entry_count` of -1 as the span and crashed; 2.56
//! leaves the entries under it uncollapsed (sparse-index.c:116-121), so no
//! directory below an intent-to-add path is treated as out of the cone.
//!
//! zvcs collapsed the excluded `folder1/0/` next to `folder1/newita` and warned
//! that it held untracked files. Expectations measured from stock git 2.56.0.
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
    /// The t1092 shape, trimmed: a root file, `deep/` (the cone kept), and three
    /// directories the narrowed cone excludes, `folder1/` nesting two levels.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-sc-ita-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        for d in ["deep/a", "folder1/0/0", "folder2", "x"] {
            std::fs::create_dir_all(work.join(d)).unwrap();
        }
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        for p in ["a", "deep/a/b", "deep/c", "folder1/0/0/0", "folder1/a", "folder2/a", "x/a"] {
            f.write(p, &format!("{p}\n"));
        }
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "initial"]);
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
            .env("GIT_AUTHOR_DATE", "1112911993 +0000")
            .env("GIT_COMMITTER_DATE", "1112911993 +0000")
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

    /// (stdout, stderr, exit code).
    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.work.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn exists(&self, rel: &str) -> bool {
        self.work.join(rel).exists()
    }
}

/// Stage `ita` with `add -N`, leave untracked files in `folder1/0/0/` and
/// `folder2/`, then narrow the cone to `deep`. Only `folder2/` is a sparse
/// directory; everything under `folder1/` stays uncollapsed whatever depth the
/// intent-to-add path sits at.
fn narrow_past(ita: &str) {
    let f = Fixture::new(&ita.replace('/', "-"));
    f.git(&["sparse-checkout", "set", "deep", "folder1", "folder2"]);
    f.write(ita, "text\n");
    f.git(&["add", "-N", ita]);
    f.write("folder1/0/0/junk", "junk\n");
    f.write("folder2/junk", "junk\n");

    let (out, err, code) = f.run(&["sparse-checkout", "set", "deep"]);
    assert_eq!(code, 0, "set failed: {err}");
    assert_eq!(out, "");
    assert_eq!(
        err,
        format!(
            "warning: The following paths are not up to date and were left despite sparse patterns:\n\
             \t{ita}\n\
             \n\
             After fixing the above paths, you may want to run `git sparse-checkout reapply`.\n\
             warning: directory 'folder2/' contains untracked files, but is not in the sparse-checkout cone\n"
        ),
        "intent-to-add at {ita}"
    );
    assert!(f.exists(ita));
    assert!(f.exists("folder1/0/0/junk"));
    assert!(f.exists("folder2/junk"));
    assert!(!f.exists("folder1/0/0/0"), "the out-of-cone tracked file was kept");

    let (out, _, code) = f.run(&["status", "--porcelain=v2"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "1 .A N... 000000 000000 100644 0000000000000000000000000000000000000000 \
             0000000000000000000000000000000000000000 {ita}\n\
             ? folder1/0/0/junk\n\
             ? folder2/junk\n"
        )
    );
}

#[test]
fn sparse_checkout_set_leaves_the_parent_of_an_intent_to_add_path_uncollapsed() {
    narrow_past("folder1/newita");
}

#[test]
fn sparse_checkout_set_leaves_every_ancestor_of_a_deep_intent_to_add_path_uncollapsed() {
    narrow_past("folder1/0/newita");
    narrow_past("folder1/0/0/newita");
}
