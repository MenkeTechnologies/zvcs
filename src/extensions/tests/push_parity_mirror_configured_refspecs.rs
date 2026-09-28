//! `push --mirror` under configured `remote.<name>.push` refspecs.
//!
//! `do_push()` takes `remote->push` whenever the command line gave no refspec
//! and `--all` is off (builtin/push.c:431-436); `--mirror` only skips the
//! `push.default` fallback. `match_push_refs()` then expands those refspecs with
//! `MATCH_REFS_MIRROR` (transport.c:1459-1460), and `set_ref_status_for_push()`
//! leaves every advertised ref no refspec matched with a null `new_oid`
//! (remote.c:1693-1698), which makes it a deletion. So a mirror push through
//! `refs/heads/main` rewinds `main` (forced) and deletes every other remote ref.
//! zvcs ignored the refspecs and mirrored every local ref to its own name.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
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

impl Fixture {
    /// Two commits on `main` plus a branch and a tag, cloned bare to `dst.git`,
    /// then `main` rewound one commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-mirror-refspecs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for n in ["one", "two"] {
            std::fs::write(f.work.join("file"), n).unwrap();
            f.run(&["add", "file"]);
            f.run(&["commit", "-q", "-m", n]);
        }
        f.run(&["branch", "topic", "HEAD~1"]);
        f.run(&["tag", "v1"]);
        f.run(&["clone", "-q", "--bare", ".", "../dst.git"]);
        f.run(&["reset", "-q", "--hard", "HEAD~1"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
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
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn remote_main(&self) -> String {
        self.run(&["--git-dir=../dst.git", "rev-parse", "main"]).0
    }
}

#[test]
fn mirror_pushes_the_configured_refspecs_and_deletes_the_rest() {
    let f = Fixture::new("configured");
    f.run(&["remote", "add", "d", "../dst.git"]);
    f.run(&["config", "remote.d.push", "refs/heads/main"]);
    let old = f.remote_main();
    let new = f.run(&["rev-parse", "main"]).0;
    let (out, err, code) = f.run(&["push", "--mirror", "--porcelain", "d"]);
    assert_eq!(
        (out, err.as_str(), code),
        (
            format!(
                "To ../dst.git\n\
                 +\trefs/heads/main:refs/heads/main\t{}...{} (forced update)\n\
                 -\t:refs/heads/topic\t[deleted]\n\
                 -\t:refs/tags/v1\t[deleted]\n\
                 Done\n",
                &old[..7],
                &new[..7]
            ),
            "",
            0
        )
    );
    assert_eq!(
        f.run(&["--git-dir=../dst.git", "for-each-ref", "--format=%(refname)"]).0,
        "refs/heads/main\n"
    );
    assert_eq!(f.remote_main(), new);
}

#[test]
fn a_mirror_remote_pushes_through_its_refspec() {
    let f = Fixture::new("remote-mirror");
    f.run(&["init", "-q", "--bare", "../empty.git"]);
    f.run(&["remote", "add", "--mirror=push", "e", "../empty.git"]);
    f.run(&["config", "remote.e.push", "refs/heads/main:refs/heads/other"]);
    let (out, err, code) = f.run(&["push", "e"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "To ../empty.git\n * [new branch]      main -> other\n", 0)
    );
    assert_eq!(
        f.run(&["--git-dir=../empty.git", "for-each-ref", "--format=%(refname)"]).0,
        "refs/heads/other\n"
    );
}
