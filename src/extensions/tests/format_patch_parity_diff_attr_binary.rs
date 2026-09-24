//! `format-patch` decides binary-ness through the path's diff driver, not only the bytes.
//!
//! `diff_filespec_is_binary()` (diff.c:3712-3733) takes the driver's `binary` first,
//! and `userdiff_find_by_path()` answers `driver_false` (`binary = 1`) for a path
//! marked `-diff`. So a plain-text file under `-diff` is binary everywhere the
//! question is asked: the diffstat row (`builtin_diffstat()`, diff.c:4213-4215), the
//! body and its full-length `index` line (`builtin_diff()`, `fill_metainfo()`), and
//! the patch id `--base` prints for each prerequisite (`diff_get_patch_id()`), which
//! hashes a binary pair's object names instead of its lines.
//!
//! `-a` answers only two of those: it keeps `flags.binary` off (builtin/log.c:2243-2244)
//! and passes `builtin_diff()`'s `!o->flags.text` test, but `builtin_diffstat()` has no
//! such guard, so the stat row still says `Bin`.
//!
//! Expectations measured against stock git 2.55.0 on this exact fixture.
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
    /// `seed` adds `opaque.txt` under `-diff`, `grow` appends a line to it, and `other`
    /// touches an unrelated file so `grow` can be a `--base` prerequisite.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fpattrbin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join(".gitattributes"), "opaque.txt -diff\n").unwrap();
        std::fs::write(f.work.join("opaque.txt"), "one\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "seed"]);
        std::fs::write(f.work.join("opaque.txt"), "one\ntwo\n").unwrap();
        f.git(&["commit", "-q", "-am", "grow"]);
        std::fs::write(f.work.join("other.txt"), "x\n").unwrap();
        f.git(&["add", "other.txt"]);
        f.git(&["commit", "-q", "-m", "other"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        c
    }

    fn git(&self, args: &[&str]) {
        assert!(self.cmd(args).status().unwrap().success(), "git {args:?} failed");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }
}

#[test]
fn a_minus_diff_text_file_is_patched_as_binary() {
    let f = Fixture::new("binary");
    let out = f.stdout(&["format-patch", "--stdout", "-1", "HEAD~1"]);
    assert!(out.contains(" opaque.txt | Bin 4 -> 8 bytes\n"), "{out}");
    assert!(out.contains(" 1 file changed, 0 insertions(+), 0 deletions(-)\n"), "{out}");
    assert!(
        out.contains(
            "index 5626abf0f72e58d7a153368ba57db4c673c0e171..814f4a422927b82f5f8a43f8fab6d3839e3983f2 100644\n\
             GIT binary patch\n"
        ),
        "{out}"
    );
    assert!(!out.contains("+two\n"), "no textual hunk for a -diff path:\n{out}");
}

#[test]
fn text_flag_changes_the_body_but_not_the_stat_row() {
    let f = Fixture::new("text");
    let out = f.stdout(&["format-patch", "--stdout", "-1", "-a", "HEAD~1"]);
    assert!(out.contains(" opaque.txt | Bin 4 -> 8 bytes\n"), "{out}");
    assert!(
        out.contains(
            "index 5626abf..814f4a4 100644\n--- a/opaque.txt\n+++ b/opaque.txt\n@@ -1 +1,2 @@\n one\n+two\n"
        ),
        "{out}"
    );
}

#[test]
fn prerequisite_patch_id_hashes_the_pair_as_binary() {
    let f = Fixture::new("base");
    let out = f.stdout(&["format-patch", "--stdout", "-1", "--base=HEAD~2"]);
    assert!(
        out.contains("prerequisite-patch-id: 009e4dfe110b5953ae5e589c80015a8c80ed7a2f\n"),
        "{out}"
    );
}
