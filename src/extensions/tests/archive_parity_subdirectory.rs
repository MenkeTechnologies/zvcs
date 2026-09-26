//! `git archive` run from a subdirectory.
//!
//! git 2.55.0 does not narrow the tree to the cwd. `parse_pathspec_arg()`
//! parses the arguments with `PATHSPEC_PREFER_CWD` against `args->prefix`
//! (archive.c:465-483), so with no argument the cwd itself is the pathspec,
//! and the whole tree is walked under it with the whole tree's attributes.
//! `write_archive_entry()` then looks `export-ignore`/`export-subst` up on the
//! full path and writes the entry under `relative_path(path, prefix)`,
//! skipping `./` and `../…` (archive.c:173-201). `path_exists()` refuses a
//! spec that reaches a file above the cwd before testing that it matches
//! (archive.c:414-452): `pathspec '%s' matches files outside the current
//! directory`.
//!
//! zvcs re-rooted the walk at the cwd's sub-tree instead: a top-level
//! `.gitattributes` rule for `d/s.txt` no longer matched `s.txt`, so an
//! `export-ignore`d file was archived; a tree-ish that is itself a sub-tree
//! (`HEAD:d`) died with the pre-2.x `current working directory is untracked`
//! (twice); and `../a.txt` / `:/a.txt` died as leaving the repository or
//! matching nothing.
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
    /// `a.txt`, `d/s.txt` (export-ignored by the top-level `.gitattributes`),
    /// `d/n.txt` and `d/e/f`, all in one commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-archive-subdirectory-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("d/e")).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a.txt"), "a\n").unwrap();
        std::fs::write(f.work.join(".gitattributes"), "d/s.txt export-ignore\n").unwrap();
        std::fs::write(f.work.join("d/s.txt"), "s\n").unwrap();
        std::fs::write(f.work.join("d/n.txt"), "n\n").unwrap();
        std::fs::write(f.work.join("d/e/f"), "f\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    /// Run in `d/`, writing the archive to `<root>/out`.
    fn archive_in_d(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.root.join("out");
        let _ = std::fs::remove_file(&out);
        let mut all = vec!["archive", "-v", "-o", out.to_str().unwrap()];
        all.extend_from_slice(args);
        self.run_in(&self.work.join("d"), &all)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
fn top_level_attributes_still_apply_to_the_cwd_entries() {
    let f = Fixture::new("attrs");
    // `-v` names every written entry; `s.txt` is export-ignored as `d/s.txt`.
    assert_eq!(f.archive_in_d(&["HEAD"]), (String::new(), "e/\ne/f\nn.txt\n".into(), 0));
    assert_eq!(
        f.archive_in_d(&["--format=zip", "HEAD"]),
        (String::new(), "e/\ne/f\nn.txt\n".into(), 0)
    );
    assert_eq!(
        f.archive_in_d(&["--prefix=p/", "HEAD"]),
        (String::new(), "p/\np/e/\np/e/f\np/n.txt\n".into(), 0)
    );
    // `:(top)` re-roots the spec; the written path is still cwd-relative.
    assert_eq!(f.archive_in_d(&["HEAD", ":(top)d/e"]), (String::new(), "e/\ne/f\n".into(), 0));
}

#[test]
fn a_sub_tree_tree_ish_archives_nothing() {
    let f = Fixture::new("subtree");
    // The implied pathspec `d/` matches nothing inside `HEAD:d`: an empty tar,
    // which is just the two terminating zero blocks padded to 10240 bytes.
    assert_eq!(f.archive_in_d(&["HEAD:d"]), (String::new(), String::new(), 0));
    let tar = std::fs::read(f.root.join("out")).unwrap();
    assert_eq!(tar.len(), 10240);
    assert!(tar.iter().all(|&b| b == 0));
}

#[test]
fn a_spec_reaching_above_the_cwd_is_refused() {
    let f = Fixture::new("outside");
    for spec in ["../a.txt", ":/a.txt", ".."] {
        let want = format!("fatal: pathspec '{spec}' matches files outside the current directory\n");
        assert_eq!(f.archive_in_d(&["HEAD", spec]), (String::new(), want, 128), "{spec}");
    }
    // A spec inside the cwd still gets the existence test.
    assert_eq!(
        f.archive_in_d(&["HEAD", "nothere"]),
        (String::new(), "fatal: pathspec 'nothere' did not match any files\n".into(), 128)
    );
}
