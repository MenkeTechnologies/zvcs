//! `git submodule add -q` / `--quiet`.
//!
//! `cmd_add()` takes the switch among its own options (`-q|--quiet) quiet=$1`,
//! git-submodule.sh:84-86) and hands it to `submodule--helper add`, whose clone
//! then says nothing — the same as `git submodule --quiet add`. zvcs accepted the
//! switch after `add` and dropped it, so the clone still printed `Cloning into
//! '…'...` and `done.`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-submodule-add-quiet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("top")).unwrap();
        let f = Fixture { root };
        let sub = f.root.join("sub");
        f.run(&sub, &["init", "-q", "."]);
        std::fs::write(sub.join("s"), "s\n").unwrap();
        f.run(&sub, &["add", "s"]);
        f.run(&sub, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "s"]);
        f.run(&f.root.join("top"), &["init", "-q", "."]);
        f
    }

    fn run(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
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
fn the_switch_after_add_silences_the_clone() {
    let f = Fixture::new();
    let top = f.root.join("top");
    let silent = (String::new(), String::new(), 0);
    for (path, switch) in [("sm1", "-q"), ("sm2", "--quiet")] {
        assert_eq!(
            f.run(&top, &["-c", "protocol.file.allow=always", "submodule", "add", switch, "../sub", path]),
            silent,
            "{switch}"
        );
        assert!(top.join(path).join("s").is_file(), "{path}");
    }
}
