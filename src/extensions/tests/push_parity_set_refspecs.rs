//! `set_refspecs()` (builtin/push.c:104-138) turns the command-line refs into
//! the push refspec list one argument at a time, and has two refusals and one
//! shorthand of its own:
//!
//! ```c
//! if (!strcmp("tag", ref)) {
//!         if (nr <= ++i)
//!                 die(_("tag shorthand without <tag>"));
//!         ref = refs[i];
//!         if (deleterefs)
//!                 refspec_appendf(&rs, ":refs/tags/%s", ref);
//!         else
//!                 refspec_appendf(&rs, "refs/tags/%s", ref);
//! } else if (deleterefs) {
//!         if (strchr(ref, ':') || !*ref)
//!                 die(_("--delete only accepts plain target ref names"));
//!         refspec_appendf(&rs, ":%s", ref);
//! }
//! ```
//!
//! zvcs had none of it: `push o tag v1` looked for a local ref named `tag`,
//! `push -d o tag v1` tried to delete a remote `tag`, and `push -d o a:b` asked
//! the remote to delete `a:b`.
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
    /// A bare `up.git` holding `main` and tag `v1`, and a work repository `w`
    /// (tags `v1` and `tag`) whose remote `o` points at it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-set-refspecs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.root, &["init", "-q", "--bare", "up.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["tag", "v1"]);
        // A tag literally named `tag`, so the shorthand is not just a lookup miss.
        f.run(&["tag", "tag"]);
        f.run(&["remote", "add", "o", "../up.git"]);
        f.run(&["push", "-q", "o", "main", "v1"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
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
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn remote_refs(&self) -> String {
        self.run(&["--git-dir=../up.git", "for-each-ref", "--format=%(refname)"]).0
    }
}

#[test]
fn tag_shorthand_names_a_tag() {
    let f = Fixture::new("push");
    let (out, err, code) = f.run(&["push", "o", "tag", "tag"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "To ../up.git\n * [new tag]         tag -> tag\n", 0)
    );
    // The word after `tag` is the whole refspec, destination included.
    let (_, err, code) = f.run(&["push", "o", "tag", "v1:refs/tags/v2"]);
    assert_eq!((err.as_str(), code), ("To ../up.git\n * [new tag]         v1 -> v2\n", 0));
    assert_eq!(
        f.remote_refs(),
        "refs/heads/main\nrefs/tags/tag\nrefs/tags/v1\nrefs/tags/v2\n"
    );
}

#[test]
fn tag_shorthand_deletes_a_tag() {
    let f = Fixture::new("delete");
    let (out, err, code) = f.run(&["push", "-d", "o", "tag", "v1"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "To ../up.git\n - [deleted]         v1\n", 0)
    );
    assert_eq!(f.remote_refs(), "refs/heads/main\n");
}

#[test]
fn a_trailing_tag_has_nothing_to_name() {
    let f = Fixture::new("trailing");
    for argv in [&["push", "o", "main", "tag"][..], &["push", "-d", "o", "x", "tag"][..]] {
        let (out, err, code) = f.run(argv);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "fatal: tag shorthand without <tag>\n", 128),
            "{argv:?}"
        );
    }
    assert_eq!(f.remote_refs(), "refs/heads/main\nrefs/tags/v1\n");
}

#[test]
fn delete_refuses_anything_but_a_plain_name() {
    let f = Fixture::new("plain");
    for argv in [&["push", "-d", "o", "main:main"][..], &["push", "-d", "o", ""][..]] {
        let (out, err, code) = f.run(argv);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "fatal: --delete only accepts plain target ref names\n", 128),
            "{argv:?}"
        );
    }
    assert_eq!(f.remote_refs(), "refs/heads/main\nrefs/tags/v1\n");
}
