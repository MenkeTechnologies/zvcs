//! `format-patch --ignore-if-in-upstream` drops the patches upstream already has.
//!
//! `get_patch_ids()` (builtin/log.c:1170-1210) walks the range the other way
//! round with `max_parents = 1` and records every commit's patch id under
//! `init_patch_ids()`, which clears `detect_rename` (patch-ids.c:66-75); the
//! main walk then skips each commit `has_commit_patch_id()` finds among them
//! (builtin/log.c:2348-2349). The walk's boundary is still counted over every
//! commit it returned, so the cover letter's diffstat keeps its base. zvcs
//! refused the flag on any real range (`unsupported flag`).
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
    /// `upstream` and `main` both fork `base`. Both change line 7 of `a` and move
    /// `b` to `e`; only `main` also changes line 9.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-format-patch-ignore-upstream-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        let a = |edits: &[(u32, &str)]| -> String {
            (1..=20u32)
                .map(|n| match edits.iter().find(|(at, _)| *at == n) {
                    Some((_, w)) => format!("{w}\n"),
                    None => format!("{n}\n"),
                })
                .collect()
        };
        let write = |s: String| std::fs::write(f.work.join("a"), s).unwrap();
        f.ok(&["init", "-q", "-b", "main", "."]);
        write(a(&[]));
        std::fs::write(f.work.join("b"), "x\n").unwrap();
        f.ok(&["add", "a", "b"]);
        f.ok(&["commit", "-q", "-m", "base"]);
        f.ok(&["checkout", "-q", "-b", "upstream"]);
        write(a(&[(7, "seven")]));
        f.ok(&["commit", "-q", "-am", "upstream: seven"]);
        f.ok(&["mv", "b", "e"]);
        f.ok(&["commit", "-q", "-m", "upstream: move b"]);
        f.ok(&["checkout", "-q", "main"]);
        write(a(&[(7, "seven")]));
        f.ok(&["commit", "-q", "-am", "ours: seven"]);
        write(a(&[(7, "seven"), (9, "nine")]));
        f.ok(&["commit", "-q", "-am", "ours: nine"]);
        f.ok(&["mv", "b", "e"]);
        f.ok(&["commit", "-q", "-m", "ours: move b"]);
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
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    }
}

#[test]
fn patches_upstream_already_has_are_dropped() {
    let f = Fixture::new("drop");
    // The pure move matches upstream's too: with renames off both are a deletion
    // of `b` plus a creation of `e`.
    assert_eq!(f.ok(&["format-patch", "--ignore-if-in-upstream", "upstream"]), "0001-ours-nine.patch\n");
    let off = f.ok(&["format-patch", "--ignore-if-in-upstream", "--no-ignore-if-in-upstream", "upstream"]);
    assert_eq!(off, "0001-ours-seven.patch\n0002-ours-nine.patch\n0003-ours-move-b.patch\n");
}

#[test]
fn the_cover_letter_diffstat_keeps_the_walk_boundary() {
    let f = Fixture::new("cover");
    let out = f.ok(&["format-patch", "--ignore-if-in-upstream", "--cover-letter", "--stdout", "upstream"]);
    assert!(
        out.contains(
            "A U Thor (1):\n  ours: nine\n\n a | 4 ++--\n 1 file changed, 2 insertions(+), 2 deletions(-)\n\n"
        ),
        "{out}"
    );
}

#[test]
fn a_range_is_still_required() {
    let f = Fixture::new("range");
    assert_eq!(
        f.run(&["format-patch", "--ignore-if-in-upstream", "--stdout", "main", "upstream"]),
        (String::new(), "fatal: not a range\n".to_owned(), 128)
    );
    assert_eq!(
        f.run(&["format-patch", "--ignore-if-in-upstream", "--stdout", "upstream..upstream"]),
        (String::new(), String::new(), 0)
    );
}
