//! A `.git` file the discovery walk cannot follow ends every command that runs
//! setup, naming the gitfile.
//!
//! `repo_discovery_find_dir()` dies on any `read_gitfile_gently()` error other
//! than "missing" and "is a directory" (setup.c:1634-1661, v2.56.0), and
//! `setup_git_directory_gently()` asks it to whether or not the caller passed
//! `nongit_ok` (setup.c:1952). 2.56 reworded the error for a target that is not
//! a repository to name the gitfile, `gitfile does not point to a valid
//! repository: <path>` (setup.c:946-948); 2.55 printed `not a git repository:`
//! followed by a `NULL` directory. zvcs walked past the file for every verb but
//! bare `rev-parse` and ended on `not a git repository (or any of the parent
//! directories)`, or for a `RUN_SETUP_GENTLY` verb ran as if there were no
//! repository at all.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    /// `a/.git` holds `gitdir: ../nope`, which names nothing; `a/sub` is empty
    /// and `a/f` holds `x`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-gitfile-walk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/sub")).unwrap();
        let f = Fixture { root: std::fs::canonicalize(&root).unwrap() };
        std::fs::write(f.a().join(".git"), "gitdir: ../nope\n").unwrap();
        std::fs::write(f.a().join("f"), "x\n").unwrap();
        f
    }

    fn a(&self) -> PathBuf {
        self.root.join("a")
    }

    /// The fatal line stock prints for `a/.git`.
    fn refusal(&self) -> String {
        format!("fatal: gitfile does not point to a valid repository: {}/.git\n", self.a().display())
    }

    fn run(&self, dir: &Path, env: &[(&str, &str)], args: &[&str], stdin: &str) -> (String, String, i32) {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }
}

/// `RUN_SETUP` verbs, from the directory and from below it, and with `-h`,
/// which only downgrades the setup to `RUN_SETUP_GENTLY` (git.c run_builtin()).
#[test]
fn run_setup_verbs_name_the_gitfile() {
    let f = Fixture::new("strict");
    let refusal = (String::new(), f.refusal(), 128);
    assert_eq!(f.run(&f.a(), &[], &["status"], ""), refusal);
    assert_eq!(f.run(&f.a().join("sub"), &[], &["status"], ""), refusal);
    assert_eq!(f.run(&f.a(), &[], &["status", "-h"], ""), refusal);
    assert_eq!(f.run(&f.a(), &[], &["rev-parse", "--git-dir"], ""), refusal);
    // The walk dies before the first configuration read parses `-c`.
    assert_eq!(f.run(&f.a(), &[], &["-c", "foo", "status"], ""), refusal);
}

/// `RUN_SETUP_GENTLY` verbs and the ones that call setup themselves die too;
/// verbs that never walk do not.
#[test]
fn gentle_setup_dies_and_setup_free_verbs_run() {
    let f = Fixture::new("gentle");
    let refusal = (String::new(), f.refusal(), 128);
    assert_eq!(f.run(&f.a(), &[], &["config", "--global", "-l"], ""), refusal);
    assert_eq!(f.run(&f.a(), &[], &["hash-object", "f"], ""), refusal);
    assert_eq!(f.run(&f.a(), &[], &["diff", "--no-index", "f", "f"], ""), refusal);
    assert_eq!(f.run(&f.a(), &[], &["var", "GIT_EDITOR"], ""), refusal);
    assert_eq!(f.run(&f.a(), &[], &["stripspace", "-s"], "hi\n"), refusal);

    assert_eq!(f.run(&f.a(), &[], &["stripspace"], "hi\n"), ("hi\n".into(), String::new(), 0));
    let (out, err, code) = f.run(&f.a(), &[], &["version"], "");
    assert!(out.starts_with("git version "), "{out:?}");
    assert_eq!((err.as_str(), code), ("", 0));
}

/// `upload-pack` opens its operand through `enter_repo()`, whose
/// `read_gitfile()` names the path it was built from.
#[test]
fn enter_repo_names_the_relative_gitfile() {
    let f = Fixture::new("enter");
    assert_eq!(
        f.run(&f.a(), &[], &["upload-pack", "."], ""),
        (String::new(), "fatal: gitfile does not point to a valid repository: ./.git\n".into(), 128)
    );
}

/// A ceiling below the broken file stops the walk before it is read, and a
/// repository nearer the cwd ends the walk first.
#[test]
fn the_walk_stops_before_reaching_the_gitfile() {
    let f = Fixture::new("ceiling");
    let sub = f.a().join("sub");
    let ceiling = f.a().display().to_string();
    let env = [("GIT_CEILING_DIRECTORIES", ceiling.as_str())];
    assert_eq!(
        f.run(&sub, &env, &["status"], ""),
        (String::new(), "fatal: not a git repository (or any of the parent directories): .git\n".into(), 128)
    );
    assert_eq!(
        f.run(&sub, &env, &["hash-object", "../f"], ""),
        ("587be6b4c3f93f93c489c0111bba5596147a26cb\n".into(), String::new(), 0)
    );

    let (_, _, code) = f.run(&sub, &[], &["init", "-q"], "");
    assert_eq!(code, 0);
    assert_eq!(f.run(&sub, &[], &["rev-parse", "--git-dir"], ""), (".git\n".into(), String::new(), 0));
}
