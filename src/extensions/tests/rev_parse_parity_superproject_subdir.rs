//! `rev-parse --show-superproject-working-tree` from inside a submodule.
//!
//! `get_superproject_working_tree()` (submodule.c:2613-2680) starts from
//! `xgetcwd()` — git's cwd after setup, which is the top of the submodule's
//! work tree wherever the command was typed — and looks for the gitlink one
//! directory up. zvcs measured from the process's own directory, so from a
//! subdirectory of the submodule it looked for `smod/x` in the wrong place and
//! printed nothing.
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
    /// `r/` with the repository `sm/` registered as the submodule `smod`, which
    /// has a directory `x/`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rev-parse-superproject-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("r")).unwrap();
        std::fs::create_dir_all(root.join("sm")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        let (r, sm) = (f.root.join("r"), f.root.join("sm"));
        for dir in [&r, &sm] {
            f.run(dir, &["init", "-q", "-b", "main", "."]);
            std::fs::write(dir.join("f"), "f\n").unwrap();
            f.run(dir, &["add", "f"]);
            f.run(dir, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "i"]);
        }
        f.run(&r, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", "../sm", "smod"]);
        std::fs::create_dir_all(r.join("smod/x/y")).unwrap();
        f
    }

    fn run(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
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
fn every_directory_of_the_submodule_names_the_superproject() {
    let f = Fixture::new("subdir");
    let want = (format!("{}\n", f.root.join("r").display()), String::new(), 0);
    for dir in ["r/smod", "r/smod/x", "r/smod/x/y"] {
        assert_eq!(f.run(&f.root.join(dir), &["rev-parse", "--show-superproject-working-tree"]), want, "{dir}");
    }
    // The superproject itself has none.
    assert_eq!(
        f.run(&f.root.join("r"), &["rev-parse", "--show-superproject-working-tree"]),
        (String::new(), String::new(), 0)
    );
}
