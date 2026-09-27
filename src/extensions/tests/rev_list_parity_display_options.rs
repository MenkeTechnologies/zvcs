//! Revision options `rev-list` accepts but whose output it does not shape.
//!
//! `handle_revision_opt()` takes `--always`, `--root`, `--no-commit-id`,
//! `--log-size`, `--show-linear-break[=<barrier>]`, `--[no-]show-signature`,
//! `--[no-]expand-tabs` and `--expand-tabs=<n>` for every walking command, but
//! they are read by `show_log()` and the diff machinery, which `cmd_rev_list()`
//! never reaches: its `show_commit()` renders through `pretty_print_commit()`
//! with a hand-built context whose `expand_tabs_in_log` stays 0
//! (builtin/rev-list.c:310-320). zvcs refused all of them as usage errors — and
//! expanded tabs in `--pretty=medium` bodies, which stock leaves alone.
//! `--relative-date` is `--date=relative` (revision.c:2661-2663), and
//! `--no-kept-objects` drops commits held in a `.keep` pack (revision.c:
//! 2541-2550, 4183-4187).
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
    /// A (whose message holds tabs) then B.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-display-options-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "A\ttab\n\nbody\twith tab"]);
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "b"]);
        f.run(&["commit", "-q", "-m", "B"]);
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
fn display_options_leave_the_listing_alone() {
    let f = Fixture::new("noop");
    let plain = f.run(&["rev-list", "--pretty=medium", "main"]);
    assert_eq!(plain.2, 0);
    assert!(plain.0.contains("    A\ttab\n"), "{}", plain.0);
    assert!(plain.0.contains("    body\twith tab\n"), "{}", plain.0);
    for opt in [
        "--always",
        "--root",
        "--no-commit-id",
        "--log-size",
        "--show-linear-break",
        "--show-linear-break=X",
        "--show-signature",
        "--no-show-signature",
        "--expand-tabs",
        "--expand-tabs=4",
        "--no-expand-tabs",
    ] {
        assert_eq!(f.run(&["rev-list", "--pretty=medium", opt, "main"]), plain, "{opt}");
    }
    let out = f.run(&["rev-list", "--expand-tabs=-1", "main"]);
    assert_eq!(out, (String::new(), "fatal: '-1': not a non-negative integer\n".to_string(), 128));
}

#[test]
fn relative_date_is_date_relative() {
    let f = Fixture::new("relative");
    assert_eq!(
        f.run(&["rev-list", "--format=%ad", "--relative-date", "main"]),
        f.run(&["rev-list", "--format=%ad", "--date=relative", "main"])
    );
}

#[test]
fn no_kept_objects_drops_commits_in_a_kept_pack() {
    let f = Fixture::new("kept");
    let (_, err, code) = f.run(&["repack", "-adq"]);
    assert_eq!(code, 0, "{err}");
    let pack_dir = f.work.join(".git/objects/pack");
    for entry in std::fs::read_dir(&pack_dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "pack") {
            std::fs::write(path.with_extension("keep"), "").unwrap();
        }
    }
    std::fs::write(f.work.join("c"), "c\n").unwrap();
    f.run(&["add", "c"]);
    f.run(&["commit", "-q", "-m", "C"]);
    let head = f.run(&["rev-parse", "main"]).0;
    assert_eq!(f.run(&["rev-list", "--no-kept-objects", "main"]), (head.clone(), String::new(), 0));
    assert_eq!(f.run(&["rev-list", "--no-kept-objects=on-disk", "main"]).0, head);
    // `in-core` names packs only `pack-objects` marks.
    assert_eq!(f.run(&["rev-list", "--no-kept-objects=in-core", "--count", "main"]).0, "3\n");
}
