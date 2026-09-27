//! The order of `count-objects -v`'s `garbage found` warnings.
//!
//! `cmd_count_objects()` walks the loose fan-out directories first
//! (builtin/count-objects.c: `for_each_loose_file_in_source(…, count_loose,
//! count_cruft, …)`), and `count_loose()` asks `has_object_pack()` of every
//! object it counts under `-v` (:71-72). That first question prepares the packs,
//! and `prepare_packed_git()` reports the pack directory's garbage right then —
//! so with a loose object ahead of it, pack garbage comes out before later
//! loose garbage; with no loose object at all it comes out last, from
//! `repo_for_each_pack()`. zvcs always reported loose garbage first.
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
    /// One commit packed, `objects/pack/garbage.txt`, and `objects/ff/zz`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-count-objects-garbage-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f.run(&["repack", "-adq"]);
        let objects = f.work.join(".git/objects");
        std::fs::write(objects.join("pack/garbage.txt"), "").unwrap();
        std::fs::create_dir_all(objects.join("ff")).unwrap();
        std::fs::write(objects.join("ff/zz"), "x\n").unwrap();
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
fn without_loose_objects_pack_garbage_is_reported_last() {
    let f = Fixture::new("none");
    let (_, err, code) = f.run(&["count-objects", "-v"]);
    assert_eq!(
        (err.as_str(), code),
        (
            "warning: garbage found: .git/objects/ff/zz\n\
             warning: garbage found: .git/objects/pack/garbage.txt\n",
            0
        )
    );
}

#[test]
fn a_loose_object_ahead_of_the_garbage_reports_the_pack_first() {
    let f = Fixture::new("loose");
    // e69de29… lands in `e6/`, ahead of `ff/`.
    let blob = f.run(&["hash-object", "-w", "--stdin"]).0;
    assert_eq!(blob, "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n");
    let (out, err, code) = f.run(&["count-objects", "-v"]);
    assert_eq!(
        (err.as_str(), code),
        (
            "warning: garbage found: .git/objects/pack/garbage.txt\n\
             warning: garbage found: .git/objects/ff/zz\n",
            0
        )
    );
    assert!(out.starts_with("count: 1\n"), "{out}");
    assert!(out.contains("garbage: 2\n"), "{out}");
}

#[test]
fn without_v_no_garbage_is_reported() {
    let f = Fixture::new("terse");
    let (_, err, code) = f.run(&["count-objects"]);
    assert_eq!((err.as_str(), code), ("", 0));
}
