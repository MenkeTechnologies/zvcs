//! `git ls-remote` outside a repository.
//!
//! `ls-remote` is `RUN_SETUP_GENTLY`: without a repository `remote_get(dest)`
//! has only the system, global and command-line configuration, so the operand
//! is the URL (after `url.<base>.insteadOf`) and `transport_get()` lists it as
//! usual (builtin/ls-remote.c:126-156). With no operand there is no default
//! remote — `No remote configured to list refs from.` — and a date `--sort`
//! key dies in `parse_ref_filter_atom()` for want of object data. zvcs refused
//! every run with `ls-remote outside a repository is not supported`.
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
    /// `root/up` has `main` with an annotated tag `v1`; `root` itself is no
    /// repository (the ceiling stops discovery there).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-ls-remote-outside-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&up, &["tag", "-a", "-m", "t", "v1"]);
        f
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.root, args)
    }

    fn rev(&self, rev: &str) -> String {
        self.run_in(&self.root.join("up"), &["rev-parse", rev]).0.trim_end().to_owned()
    }
}

#[test]
fn a_path_or_file_url_is_listed() {
    let f = Fixture::new("list");
    let (main, tag) = (f.rev("main"), f.rev("v1"));
    let expect = format!(
        "{main}\tHEAD\n{main}\trefs/heads/main\n{tag}\trefs/tags/v1\n{main}\trefs/tags/v1^{{}}\n"
    );
    assert_eq!(f.run(&["ls-remote", "up"]), (expect.clone(), String::new(), 0));
    assert_eq!(
        f.run(&["-c", "protocol.version=0", "ls-remote", "up"]),
        (expect, String::new(), 0)
    );
    assert_eq!(
        f.run(&["ls-remote", "--refs", "--sort=-refname", "up"]),
        (format!("{tag}\trefs/tags/v1\n{main}\trefs/heads/main\n"), String::new(), 0)
    );
    assert_eq!(f.run(&["ls-remote", "--exit-code", "up", "nope"]), (String::new(), String::new(), 2));
}

#[test]
fn insteadof_from_the_command_line_rewrites_the_operand() {
    let f = Fixture::new("insteadof");
    assert_eq!(
        f.run(&["-c", "url.up.insteadOf=zz", "ls-remote", "--get-url", "zz"]),
        ("up\n".into(), String::new(), 0)
    );
    let (out, _, code) = f.run(&["-c", "url.up.insteadOf=zz", "ls-remote", "--heads", "zz"]);
    assert_eq!((out, code), (format!("{}\trefs/heads/main\n", f.rev("main")), 0));
}

#[test]
fn what_cannot_work_without_a_repository_is_refused() {
    let f = Fixture::new("refused");
    let fatal = |msg: &str| (String::new(), format!("fatal: {msg}\n"), 128);
    assert_eq!(f.run(&["ls-remote"]), fatal("No remote configured to list refs from."));
    assert_eq!(
        f.run(&["ls-remote", "--sort=committerdate", "up"]),
        fatal("not a git repository, but the field 'committerdate' requires access to object data")
    );
    assert_eq!(
        f.run(&["ls-remote", "nope"]),
        (
            String::new(),
            "fatal: 'nope' does not appear to be a git repository\n\
             fatal: Could not read from remote repository.\n\n\
             Please make sure you have the correct access rights\nand the repository exists.\n"
                .into(),
            128
        )
    );
}
