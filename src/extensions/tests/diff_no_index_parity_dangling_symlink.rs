//! `diff --no-index <dir> <dir>` dropped a symlink whose target is missing.
//!
//! `get_mode()` (diff-no-index.c:81-98) reads each side with `lstat()`, so a
//! dangling symlink is a side like any other and its blob is the target text.
//! zvcs also required the path to `exists()`, which follows the link, so both
//! halves of the pair became `/dev/null`: a `diff --git a/dev/null b/dev/null`
//! header with no body and a `/dev/null | 0` stat row.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `d1/l -> tgt` and `d2/l -> tgt2`, neither target present.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-no-index-dangling-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("d1")).unwrap();
        std::fs::create_dir_all(root.join("d2")).unwrap();
        std::os::unix::fs::symlink("tgt", root.join("d1/l")).unwrap();
        std::os::unix::fs::symlink("tgt2", root.join("d2/l")).unwrap();
        Fixture { root }
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
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
fn a_dangling_symlink_is_diffed_by_its_target_text() {
    let f = Fixture::new("patch");
    assert_eq!(
        f.run(&["diff", "--no-index", "d1", "d2"]),
        (
            "diff --git a/d1/l b/d2/l\nindex c7e58fc..91e4cef 120000\n--- a/d1/l\n+++ b/d2/l\n\
@@ -1 +1 @@\n-tgt\n\\ No newline at end of file\n+tgt2\n\\ No newline at end of file\n"
                .into(),
            String::new(),
            1
        )
    );
    assert_eq!(
        f.run(&["diff", "--no-index", "--stat", "d1", "d2"]).0,
        " {d1 => d2}/l | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n"
    );
}
