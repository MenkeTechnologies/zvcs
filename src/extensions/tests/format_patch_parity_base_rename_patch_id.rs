//! `format-patch --base` hashes a renamed prerequisite as a rename.
//!
//! `prepare_bases()` computes each `prerequisite-patch-id:` with
//! `commit_patch_id()` (builtin/log.c:1855-1888, patch-ids.c:14-27), whose
//! `diffcore_std()` runs rename detection under the `diff.renames` default.
//! `diff_get_patch_id()` (diff.c:6895-6990) then hashes the pair under both of
//! its names — `diff--gita/<old>b/<new>`, `---a/<old>+++b/<new>` — and
//! `diff_unmodified_pair()` keeps a pure rename because its paths differ
//! (diff.c:6520-6523). zvcs detected the rename but hashed the pair as a
//! creation with mode 0, so every prerequisite that renamed a file had a
//! patch id stock never prints.
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
    /// `base`, then a pure rename `b -> c`, a rename of `a` with an edit, and a
    /// tip commit the patch is made of.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-format-patch-rename-patch-id-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        let seq: String = (1..=30).map(|n| format!("{n}\n")).collect();
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), &seq).unwrap();
        std::fs::write(f.work.join("b"), "x\n").unwrap();
        f.ok(&["add", "a", "b"]);
        f.ok(&["commit", "-q", "-m", "base"]);
        f.ok(&["mv", "b", "c"]);
        f.ok(&["commit", "-q", "-m", "rename b to c"]);
        f.ok(&["mv", "a", "a2"]);
        std::fs::write(f.work.join("a2"), format!("{seq}31\n")).unwrap();
        f.ok(&["add", "a2"]);
        f.ok(&["commit", "-q", "-m", "rename a with an edit"]);
        std::fs::write(f.work.join("c"), "x\ny\n").unwrap();
        f.ok(&["commit", "-q", "-am", "tip"]);
        f
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }
}

#[test]
fn renamed_prerequisites_hash_under_both_names() {
    let f = Fixture::new("renames");
    let out = f.ok(&["format-patch", "--base=HEAD~3", "--stdout", "-1"]);
    assert!(
        out.ends_with(
            "\nbase-commit: 637c5baeeea0ff24d65872d69621eac222e7eb45\n\
             prerequisite-patch-id: 721eef913c7f2068b12dbddfee2143c3bc294fa2\n\
             prerequisite-patch-id: cb2edc06553f8551c5417473680ce8bc0d0b6a51\n-- \n2.55.0\n\n"
        ),
        "{out}"
    );
}

#[test]
fn diff_renames_false_hashes_a_deletion_and_a_creation() {
    let f = Fixture::new("norenames");
    let out = f.ok(&["-c", "diff.renames=false", "format-patch", "--base=HEAD~3", "--stdout", "-1"]);
    assert!(
        out.ends_with(
            "prerequisite-patch-id: f84ea6a25733f21f812f4b00a322526ba9e91c9a\n\
             prerequisite-patch-id: c81d488eb7d2eb78290c19da919f3be67d91c61c\n-- \n2.55.0\n\n"
        ),
        "{out}"
    );
}
