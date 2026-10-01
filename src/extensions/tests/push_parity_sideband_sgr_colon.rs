//! Remote output relayed over the sideband keeps SGR sequences whose
//! parameters use colon-separated subfields.
//!
//! `handle_ansi_sequence()` (sideband.c:157-223, v2.56.0) accepts `:` beside
//! digits and `;` inside `ESC [ ... m` (:217), so the 256-color
//! (`38:5:<n>`) and true-color (`38:2::<r>:<g>:<b>`) forms pass through, while
//! a sequence carrying any other byte is still escaped as `^[`. 2.55 and zvcs
//! escaped the colon forms too.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::{Path, PathBuf};
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
    fn new(tag: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("zvcs-sideband-sgr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root: std::fs::canonicalize(&root).unwrap() };
        let r = f.root.clone();
        f.git(&r, &["init", "-q", "--bare", "srv.git"]);
        let hook = r.join("srv.git/hooks/pre-receive");
        std::fs::write(
            &hook,
            "#!/bin/sh\nprintf '\\033[38:5:196mred\\033[m \\033[1;38:2::10:20:30mtc\\033[m \\033[3:Xm\\n' >&2\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        f.git(&r, &["init", "-q", "-b", "main", "w"]);
        f.git(&r.join("w"), &["commit", "-q", "--allow-empty", "-m", "m"]);
        f
    }

    fn git(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }
}

#[test]
fn colon_subfields_pass_through_and_other_bytes_are_still_escaped() {
    let f = Fixture::new("push");
    let (out, err, code) = f.git(&f.root.join("w"), &["push", "-q", "../srv.git", "main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "remote: \x1b[38:5:196mred\x1b[m \x1b[1;38:2::10:20:30mtc\x1b[m ^[[3:Xm        \n",
            0
        )
    );
}
