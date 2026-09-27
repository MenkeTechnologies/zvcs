//! Aliases resolve outside every repository.
//!
//! `alias_lookup()` (alias.c) reads the configuration through
//! `read_early_config()`, which discovers a repository gently and reads the
//! system, global and command-line scopes whether or not it found one. So an
//! alias from `~/.gitconfig` or `-c alias.<name>=…` works anywhere, and a
//! non-shell one then fails or succeeds as its target command would. zvcs looked
//! aliases up only in a discovered repository's configuration, so outside one
//! every alias was `not a git command`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    outside: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-alias-outside-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("out")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        std::fs::write(
            root.join("gitconfig"),
            "[alias]\n\tsh = \"!pwd; echo P=$GIT_PREFIX\"\n\tst = status\n\tvv = version\n",
        )
        .unwrap();
        let outside = root.join("out");
        Fixture { root, outside }
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.outside)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_PREFIX", "inherited")
            .env_remove("GIT_DIR")
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
fn global_and_command_line_aliases_resolve_without_a_repository() {
    let f = Fixture::new("resolve");
    let version = f.run(&["version"]).0;
    assert_eq!(f.run(&["vv"]), (version.clone(), String::new(), 0));
    assert_eq!(f.run(&["-c", "alias.yy=version", "yy"]), (version, String::new(), 0));
    assert_eq!(f.run(&["-c", "alias.zz=!echo hi", "zz"]), ("hi\n".into(), String::new(), 0));
    // A shell alias gets setup's empty `GIT_PREFIX`, not the inherited one.
    assert_eq!(f.run(&["sh"]), (format!("{}\nP=\n", f.outside.display()), String::new(), 0));
    // A command alias fails the way its command does.
    assert_eq!(
        f.run(&["st"]),
        (String::new(), "fatal: not a git repository (or any of the parent directories): .git\n".into(), 128)
    );
}
