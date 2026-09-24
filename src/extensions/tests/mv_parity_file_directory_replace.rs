//! `rename_index_entry_at()` re-adds each moved entry with
//! `ADD_CACHE_OK_TO_REPLACE` (read-cache.c:182-185), so
//! `check_file_directory_conflict()` drops an entry that names a leading
//! directory of the new path. `git mv README.md lnk/` through a tracked symlink
//! `lnk` to a directory therefore removes the `lnk` entry. The port kept it, and
//! the index then held both `lnk` and `lnk/README.md`.
//!
//! Measured against git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("zvcs-mvdf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dir")).unwrap();
        let fx = Fixture { dir: dir.canonicalize().unwrap() };
        fx.ok(&["init", "-q", "-b", "main"]);
        std::fs::write(fx.dir.join("README.md"), "hi\n").unwrap();
        std::fs::write(fx.dir.join("dir/f"), "x\n").unwrap();
        std::os::unix::fs::symlink("dir", fx.dir.join("lnk")).unwrap();
        fx.ok(&["add", "README.md", "dir/f", "lnk"]);
        fx.ok(&["-c", "user.name=t", "-c", "user.email=t@e.x", "commit", "-q", "-m", "i"]);
        fx
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn moving_into_a_tracked_symlink_replaces_its_entry() {
    let fx = Fixture::new("lnk");
    fx.ok(&["mv", "README.md", "lnk/"]);
    assert_eq!(fx.ok(&["ls-files"]), "dir/f\nlnk/README.md\n");
    // The index is one stock can write a tree from; with both `lnk` and
    // `lnk/README.md` present it is not.
    assert_eq!(fx.ok(&["write-tree"]), "a950e442c6059756bc5a5d263ca03d4b0ed04762\n");
}
