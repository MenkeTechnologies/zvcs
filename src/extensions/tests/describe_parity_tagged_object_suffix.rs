//! The `-<depth>-g<hash>` tail `git describe` gives an annotated tag sitting on
//! the commit itself, and the one it gives a misnamed tag.
//!
//! ```c
//! /* exact match */
//! append_name(n, dst);
//! if (n->misnamed || longformat)
//!         append_suffix(0, n->tag ? get_tagged_oid(n->tag) : &cmit->object.oid, dst);
//! …
//! append_name(all_matches[0].name, dst);
//! if (all_matches[0].name->misnamed || abbrev)
//!         append_suffix(all_matches[0].depth, &cmit->object.oid, dst);
//! ```
//!
//! (builtin/describe.c:343-346, :493-495.) An exact match's tail names the object
//! the tag points at, which for a tag of a tag is the inner tag rather than the
//! commit, and a tag whose ref name differs from the name inside it keeps its tail
//! without `--long` and even under `--abbrev=0`, where it is the whole hex. Under
//! `--all` its warning names `tags/<ref>`. zvcs printed the commit's hash, dropped
//! the tail and named the bare ref.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// `A`-`B`-`C` on `main`. `B` carries the annotated `inner` and, dated later so
/// it wins the commit, `outer`, a tag of `inner`. `light` is a lightweight ref at
/// the annotated tag `named` on `A`, so `light` is externally known as `named`.
const FIXTURE: &str = r#"
set -e
t=1700000000
c() { t=$((t+3600)); GIT_AUTHOR_DATE="@$t +0000" GIT_COMMITTER_DATE="@$t +0000" "$GIT" commit -q --allow-empty -m "$1"; }
"$GIT" init -q -b main .
c A
GIT_COMMITTER_DATE="@1700000100 +0000" "$GIT" tag -a -m named named
"$GIT" tag light named
"$GIT" tag -d named >/dev/null
c B
GIT_COMMITTER_DATE="@1700000200 +0000" "$GIT" tag -a -m inner inner
GIT_COMMITTER_DATE="@1700000300 +0000" "$GIT" -c advice.nestedTag=false tag -a -m outer outer inner
c C
"#;

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Twin {
    fn new(stock: &str, label: &str) -> Twin {
        let root = std::env::temp_dir().join(format!("zvcs-describe-tagged-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for side in ["stock", "zvcs"] {
            let dir = root.join(side);
            std::fs::create_dir_all(&dir).unwrap();
            let out = env(&mut Command::new("sh"), &root)
                .args(["-c", FIXTURE])
                .env("GIT", stock)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "fixture: {}", String::from_utf8_lossy(&out.stderr));
        }
        Twin { root }
    }

    fn run(&self, bin: &str, side: &str, args: &[&str]) -> (String, String, i32) {
        let out = env(&mut Command::new(bin), &self.root)
            .args(args)
            .current_dir(self.root.join(side))
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    fn same(&self, stock: &str, args: &[&str]) {
        assert_eq!(self.run(ZVCS, "zvcs", args), self.run(stock, "stock", args), "git {args:?}");
    }
}

fn env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    cmd.env("HOME", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("ZVCS_HOME", root.join("zvcs-home"))
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
}

/// `--long` on the commit `outer` sits on names `inner`'s tag object, not `B`.
#[test]
fn an_exact_tag_of_a_tag_names_the_inner_tag() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "nested");
    for args in [
        &["describe", "--long", "HEAD~1"][..],
        &["describe", "--long", "--abbrev=12", "HEAD~1"],
        &["describe", "--long", "--all", "HEAD~1"],
        &["describe", "--long", "--tags", "outer"],
        &["describe", "HEAD~1"],
    ] {
        t.same(stock, args);
    }
}

/// A misnamed tag keeps its tail at depth 0 and under `--abbrev=0`.
#[test]
fn a_misnamed_tag_keeps_its_tail() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "misnamed");
    for args in [
        &["describe", "HEAD~2"][..],
        &["describe", "--exact-match", "HEAD~2"],
        &["describe", "--abbrev=0", "HEAD~2"],
        &["describe", "--all", "HEAD~2"],
        &["describe", "--all", "--abbrev=0", "HEAD~2"],
        &["describe", "--match", "light", "HEAD"],
        &["describe", "--match", "light", "--abbrev=0", "HEAD"],
        &["describe", "--all", "--match", "light", "--abbrev=0", "HEAD"],
    ] {
        t.same(stock, args);
    }
}

/// `--dirty`'s arm opens with `setup_work_tree()` (builtin/describe.c:760), so it
/// dies from a cwd inside `.git`, where `--broken` hands the check to children.
#[test]
fn dirty_needs_a_work_tree() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "dirty");
    for side in ["stock", "zvcs"] {
        std::fs::create_dir_all(t.root.join(side).join(".git/sub")).unwrap();
    }
    let run = |bin: &str, side: &str, args: &[&str]| {
        let out = env(&mut Command::new(bin), &t.root)
            .args(args)
            .current_dir(t.root.join(side).join(".git/sub"))
            .output()
            .unwrap();
        (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code())
    };
    for args in [&["describe", "--dirty"][..], &["describe", "--dirty=-x", "--tags"], &["describe", "--tags"]] {
        assert_eq!(run(ZVCS, "zvcs", args), run(stock, "stock", args), "git {args:?}");
    }
}
