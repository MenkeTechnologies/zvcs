//! `fast-export` of a tag whose object is another tag.
//!
//! `get_commit()` files every tag of the chain under the one ref name,
//! outermost first (builtin/fast-export.c:1040-1045), and
//! `handle_tags_and_duplicates()` walks that list backwards (:1127), so the
//! innermost tag is written first under the outer name. `handle_tag()` then
//! takes the mark of the object the tag names directly (:977-978) — for the
//! outer tag that is the inner tag, which only has one under `--mark-tags` —
//! and writes `reset <name>` / `from <null>` ahead of a tag of a tag
//! (:1008-1011). Without `--mark-tags` the default
//! `--tag-of-filtered-object=abort` dies on the outer tag, `drop` skips it,
//! and `rewrite` dies `cannot export nested tags unless --mark-tags is
//! specified.` (:990-991). zvcs refused every nested tag with its own error.
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
    /// One commit; `v1` tags it, `v2` tags `v1`, `v3` tags `v2`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fast-export-nested-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f.run(&["tag", "-a", "-m", "ann", "v1"]);
        f.run(&["tag", "-a", "-m", "nest", "v2", "v1"]);
        f.run(&["tag", "-a", "-m", "nest3", "v3", "v2"]);
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
}

const TAGGER: &str = "tagger C O Mitter <committer@example.com> 1700000000 +0000";
const NULL: &str = "0000000000000000000000000000000000000000";

/// The blob and commit every export below starts with, labelled `name`.
fn head(name: &str) -> String {
    format!(
        "blob\nmark :1\ndata 2\na\n\n\
         reset {name}\n\
         commit {name}\nmark :2\n\
         author A U Thor <author@example.com> 1700000000 +0000\n\
         committer C O Mitter <committer@example.com> 1700000000 +0000\n\
         data 4\none\nM 100644 :1 a\n\n"
    )
}

#[test]
fn mark_tags_writes_the_chain_innermost_first_under_the_outer_name() {
    let f = Fixture::new("mark2");
    let (out, err, code) = f.run(&["fast-export", "--mark-tags", "v2"]);
    let want = format!(
        "{}tag v2\nmark :3\nfrom :2\n{TAGGER}\ndata 4\nann\n\n\
         reset refs/tags/v2\nfrom {NULL}\n\n\
         tag v2\nmark :4\nfrom :3\n{TAGGER}\ndata 5\nnest\n\n",
        head("refs/tags/v2")
    );
    assert_eq!((out.as_str(), err.as_str(), code), (want.as_str(), "", 0));
}

#[test]
fn a_three_tag_chain_resets_before_each_outer_tag() {
    let f = Fixture::new("mark3");
    for extra in [None, Some("--tag-of-filtered-object=rewrite")] {
        let mut args = vec!["fast-export", "--mark-tags"];
        args.extend(extra);
        args.push("v3");
        let (out, err, code) = f.run(&args);
        let want = format!(
            "{}tag v3\nmark :3\nfrom :2\n{TAGGER}\ndata 4\nann\n\n\
             reset refs/tags/v3\nfrom {NULL}\n\n\
             tag v3\nmark :4\nfrom :3\n{TAGGER}\ndata 5\nnest\n\n\
             reset refs/tags/v3\nfrom {NULL}\n\n\
             tag v3\nmark :5\nfrom :4\n{TAGGER}\ndata 6\nnest3\n\n",
            head("refs/tags/v3")
        );
        assert_eq!((out.as_str(), err.as_str(), code), (want.as_str(), "", 0), "{extra:?}");
    }
}

#[test]
fn without_mark_tags_the_outer_tag_is_an_unexported_object() {
    let f = Fixture::new("abort");
    let v2 = f.run(&["rev-parse", "v2"]).0;
    let v2 = v2.trim_end();
    let inner = format!("{}tag v2\nfrom :2\n{TAGGER}\ndata 4\nann\n\n", head("refs/tags/v2"));

    let (out, err, code) = f.run(&["fast-export", "v2"]);
    let fatal = format!(
        "fatal: tag {v2} tags unexported object; use --tag-of-filtered-object=<mode> to handle it\n"
    );
    assert_eq!((out.as_str(), err.as_str(), code), (inner.as_str(), fatal.as_str(), 128));

    let (out, err, code) = f.run(&["fast-export", "--tag-of-filtered-object=drop", "v2"]);
    assert_eq!((out.as_str(), err.as_str(), code), (inner.as_str(), "", 0));

    let (out, err, code) = f.run(&["fast-export", "--tag-of-filtered-object=rewrite", "v2"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (inner.as_str(), "fatal: cannot export nested tags unless --mark-tags is specified.\n", 128)
    );
}
