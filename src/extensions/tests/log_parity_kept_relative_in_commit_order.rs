//! Three revision options `log` refused as unsupported.
//!
//! - `--no-kept-objects[=on-disk]` ignores commits held in a `.keep` pack
//!   (revision.c:2541-2550, 4183-4187); `=in-core` names packs only
//!   `pack-objects` marks.
//! - `--relative-date` is `--date=relative` with `date_mode_explicit`
//!   (revision.c:2661-2663).
//! - `--in-commit-order` orders an object listing `log` never prints.
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
    /// A and B packed into a kept pack, then C loose.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-kept-relative-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for (file, msg) in [("a", "A"), ("b", "B")] {
            std::fs::write(f.work.join(file), format!("{file}\n")).unwrap();
            f.run(&["add", file]);
            f.run(&["commit", "-q", "-m", msg]);
        }
        let (_, err, code) = f.run(&["repack", "-adq"]);
        assert_eq!(code, 0, "{err}");
        for entry in std::fs::read_dir(f.work.join(".git/objects/pack")).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "pack") {
                std::fs::write(path.with_extension("keep"), "").unwrap();
            }
        }
        std::fs::write(f.work.join("c"), "c\n").unwrap();
        f.run(&["add", "c"]);
        f.run(&["commit", "-q", "-m", "C"]);
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
            .env("GIT_PAGER", "cat")
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
fn kept_commits_are_dropped_and_the_rest_are_accepted() {
    let f = Fixture::new("kept");
    let ok = |s: &str| (s.to_string(), String::new(), 0);
    assert_eq!(f.run(&["log", "--format=%s", "--no-kept-objects"]), ok("C\n"));
    assert_eq!(f.run(&["log", "--format=%s", "--no-kept-objects=on-disk"]), ok("C\n"));
    assert_eq!(f.run(&["log", "--format=%s", "--no-kept-objects=in-core"]), ok("C\nB\nA\n"));
    assert_eq!(f.run(&["log", "--format=%s", "--in-commit-order"]), ok("C\nB\nA\n"));
    assert_eq!(
        f.run(&["log", "--format=%ad", "--relative-date", "-1"]),
        f.run(&["log", "--format=%ad", "--date=relative", "-1"])
    );
}
