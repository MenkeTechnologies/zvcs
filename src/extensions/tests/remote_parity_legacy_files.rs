//! Remotes defined by `$GIT_DIR/remotes/<name>` and `$GIT_DIR/branches/<name>`.
//!
//! `remotes_remote_get_1()` (remote.c:797-822) reads the `remotes/` file, then
//! the `branches/` file, for a remote whose configuration names no URL.
//! `read_remotes_file()` (remote.c:349-379) takes `URL:`, `Pull:` and `Push:`
//! lines; `read_branches_file()` (remote.c:381-428) takes one
//! `<url>[#<branch>]` line, fetching `refs/heads/<branch>` into
//! `refs/heads/<name>` and pushing `HEAD` to it, with `<branch>` defaulting to
//! `repo_default_branch_name()`. Each read prints the deprecation warning of
//! `warn_about_deprecated_remote_type()` (remote.c:334-347), once per process,
//! and marks the remote configured, so `git remote get-url` finds it and
//! `git remote add` collides with it. zvcs read neither file: the name was
//! taken as a path.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn warning(kind: &str, name: &str) -> String {
    format!(
        "warning: reading remote from \"{kind}/{name}\", which is nominated for removal.\n\
         \n\
         If you still use the \"remotes/\" directory it is recommended to\n\
         migrate to config-based remotes:\n\
         \n\
         \tgit remote rename {name} {name}\n\
         \n\
         If you cannot, please let us know why you still need to use it by\n\
         sending an e-mail to <git@vger.kernel.org>.\n"
    )
}

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
    /// `up` has `main` (two commits) and `topic` (the first); `work` is an empty
    /// repository with `remotes/r` and `branches/b` pointing at `up`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-remote-legacy-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&up, &["branch", "topic"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f.run_in(&f.root, &["init", "-q", "-b", "main", "work"]);
        let git = f.work.join(".git");
        std::fs::create_dir_all(git.join("remotes")).unwrap();
        std::fs::create_dir_all(git.join("branches")).unwrap();
        std::fs::write(
            git.join("remotes/r"),
            "URL: ../up\nPull: refs/heads/main:refs/remotes/r/main\n\
             Pull: +refs/heads/topic:refs/remotes/r/topic\nPush: refs/heads/main:refs/heads/pushed\n",
        )
        .unwrap();
        std::fs::write(git.join("branches/b"), "../up#topic\n").unwrap();
        f
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
        self.run_in(&self.work, args)
    }

    fn rev(&self, dir: &str, rev: &str) -> String {
        self.run_in(&self.root.join(dir), &["rev-parse", rev]).0
    }
}

#[test]
fn the_url_comes_from_the_file_with_one_warning() {
    let f = Fixture::new("url");
    assert_eq!(
        f.run(&["ls-remote", "--get-url", "r"]),
        ("../up\n".into(), warning("remotes", "r"), 0)
    );
    assert_eq!(
        f.run(&["remote", "get-url", "b"]),
        ("../up\n".into(), warning("branches", "b"), 0)
    );
    let (_, err, code) = f.run(&["remote", "add", "b", "../elsewhere"]);
    assert_eq!((err, code), (format!("{}error: remote b already exists.\n", warning("branches", "b")), 3));
}

#[test]
fn a_remotes_file_fetches_and_pushes_through_its_refspecs() {
    let f = Fixture::new("remotes");
    let (_, err, code) = f.run(&["fetch", "-q", "r"]);
    assert_eq!((err, code), (warning("remotes", "r"), 0));
    assert_eq!(f.rev("work", "r/main"), f.rev("up", "main"));
    assert_eq!(f.rev("work", "r/topic"), f.rev("up", "topic"));
    f.run(&["checkout", "-q", "-b", "main", "r/main"]);
    let (_, err, code) = f.run(&["push", "-q", "r"]);
    assert_eq!((err, code), (warning("remotes", "r"), 0));
    assert_eq!(f.rev("up", "pushed"), f.rev("up", "main"));
    let (out, _, code) = f.run(&["remote", "show", "-n", "r"]);
    assert_eq!(
        (out.as_str(), code),
        (
            "* remote r\n  Fetch URL: ../up\n  Push  URL: ../up\n  HEAD branch: (not queried)\n  \
             Remote branches: (status not queried)\n    main\n    topic\n  \
             Local ref configured for 'git push' (status not queried):\n    \
             refs/heads/main pushes to refs/heads/pushed\n",
            0
        )
    );
}

#[test]
fn a_branches_file_fetches_its_branch_under_its_own_name() {
    let f = Fixture::new("branches");
    let (_, err, code) = f.run(&["fetch", "-q", "b"]);
    assert_eq!((err, code), (warning("branches", "b"), 0));
    assert_eq!(f.rev("work", "refs/heads/b"), f.rev("up", "topic"));
    let (out, _, _) = f.run(&["for-each-ref", "--format=%(refname)"]);
    assert_eq!(out, "refs/heads/b\n");
}
