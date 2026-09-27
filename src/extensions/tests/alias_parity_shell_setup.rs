//! A `!` shell alias runs where repository setup left git.
//!
//! `handle_alias()` (git.c) runs `setup_git_directory_gently()` before it starts
//! a shell alias — "Aliases expect GIT_PREFIX, GIT_DIR etc to be set" — so the
//! child starts at the top of the work tree, `GIT_PREFIX` names the way back to
//! where the user typed (setup.c:2069-2076, empty and still exported when there
//! is none), and `GIT_DIR`/`GIT_WORK_TREE` are what setup exported. zvcs ran the
//! alias in the user's own directory with whatever `GIT_PREFIX` it inherited.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
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

const REPORT: &str = "!pwd; echo \"P=$GIT_PREFIX D=$GIT_DIR W=$GIT_WORK_TREE\"; echo args \"$@\"";

impl Fixture {
    /// A repository `r/` with `sub/deep/` and the alias `sh` reporting what it sees.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-alias-shell-setup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("r/sub/deep")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("r");
        let f = Fixture { root, work };
        f.run(&f.work, &["init", "-q", "-b", "main", "."], &[]);
        f.run(&f.work, &["config", "alias.sh", REPORT], &[]);
        f
    }

    fn run(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_PREFIX")
            .envs(env.iter().copied())
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
fn a_subdirectory_alias_runs_from_the_top_with_its_prefix() {
    let f = Fixture::new("sub");
    let top = f.work.display().to_string();
    let deep = f.work.join("sub/deep");
    assert_eq!(
        f.run(&deep, &["sh", "a", "b"], &[]),
        (format!("{top}\nP=sub/deep/ D= W=\nargs a b a b\n"), String::new(), 0)
    );
    // `prepare_shell_cmd()` appends the arguments to the body as well as binding
    // them to `"$@"`, hence the second `a b`. An inherited `GIT_PREFIX` is
    // replaced, not passed through.
    assert_eq!(
        f.run(&f.work.join("sub"), &["sh"], &[("GIT_PREFIX", "zz")]).0,
        format!("{top}\nP=sub/ D= W=\nargs\n")
    );
    // `-C` moves the starting point before setup measures from it.
    assert_eq!(f.run(&deep, &["-C", "..", "sh"], &[]).0, format!("{top}\nP=sub/ D= W=\nargs\n"));
}

#[test]
fn an_explicit_git_dir_is_exported_as_setup_left_it() {
    let f = Fixture::new("explicit");
    let top = f.work.display().to_string();
    let sub = f.work.join("sub");
    // No work tree named: git stays in `sub/`, which is not inside any work tree.
    assert_eq!(f.run(&sub, &["sh"], &[("GIT_DIR", "../.git")]).0, format!("{}\nP= D=../.git W=\nargs\n", sub.display()));
    // With one, setup climbs to it and makes the git directory absolute.
    assert_eq!(
        f.run(&sub, &["sh"], &[("GIT_DIR", "../.git"), ("GIT_WORK_TREE", "..")]).0,
        format!("{top}\nP=sub/ D={top}/.git W=..\nargs\n")
    );
    // Inside the git directory itself it is `.`.
    assert_eq!(f.run(&f.work.join(".git"), &["sh"], &[]).0, format!("{top}/.git\nP= D=. W=\nargs\n"));
}
