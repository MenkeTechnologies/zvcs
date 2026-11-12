//! `git tag` against stock git on a tag that is a symbolic ref, a loose ref
//! that does not parse, and a held lock.
//!
//! builtin/tag.c reads `refs/tags/<name>` with refs_read_ref(), which follows a
//! symbolic ref to the object it ends at and fails on anything it cannot read
//! (:96, :675-676), and updates the tag without REF_NO_DEREF (:694-696). So a
//! symbolic tag "already exists", `-f` moves the ref it points at and reports
//! `Updated tag`, `-d` and `-v` resolve it, and a dangling one is a new tag
//! written through to its target; a broken loose ref is "not found" to `-d` and
//! `-v`, and creating it fails in lock_raw_ref() with `reference broken`
//! (refs/files-backend.c:956-959). refs_delete_refs() names the single ref it
//! could not delete, with the lock message (refs.c:3108-3115). zvcs treated a
//! symbolic tag as absent or valueless, died on the broken ref, and printed
//! gitoxide's text for a held packed-refs lock.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    // The lock message names the absolute path of the twin repository.
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace(dir.to_str().unwrap(), "<root>");
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// Two commits and an annotated `v1` on the first, built once by stock git.
fn template(stock: &str, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let git = |args: &[&str]| run(stock, root, args);
    git(&["init", "-q", "-b", "main"]);
    git(&["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "one"]);
    git(&["tag", "-a", "-m", "version one", "v1"]);
    git(&["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "two"]);
}

/// A copy of the template with `refs/tags/<name>` written as `content`.
fn fixture(tmpl: &Path, root: &Path, refs: &[(&str, &str)]) {
    let _ = std::fs::remove_dir_all(root);
    let copied = Command::new("cp").arg("-R").arg(tmpl).arg(root).status().unwrap();
    assert!(copied.success());
    for (name, content) in refs {
        std::fs::write(root.join(".git/refs/tags").join(name), content).unwrap();
    }
}

#[test]
fn symbolic_broken_and_locked_tags_match_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-tag-symbolic-broken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (tmpl, s, z) = (base.join("template"), base.join("stock"), base.join("zvcs"));
    template(stock, &tmpl);
    let to_v1 = ("sym", "ref: refs/tags/v1\n");
    let dangling = ("sym", "ref: refs/tags/nosuch\n");
    let broken = ("broken", "x\n");
    let cases: &[(&[(&str, &str)], &[&str])] = &[
        (&[to_v1], &["tag", "sym", "HEAD"]),
        (&[to_v1], &["tag", "-f", "sym", "HEAD"]),
        (&[to_v1], &["tag", "-d", "sym"]),
        (&[to_v1], &["tag", "-v", "sym"]),
        (&[dangling], &["tag", "-a", "-m", "x", "sym", "HEAD"]),
        (&[dangling], &["tag", "-d", "sym"]),
        (&[broken], &["tag", "-d", "broken", "v1"]),
        (&[broken], &["tag", "broken"]),
        (&[broken], &["tag", "-f", "-a", "-m", "x", "broken"]),
        (&[broken], &["tag", "-v", "broken"]),
    ];
    for (refs, args) in cases {
        for root in [&s, &z] {
            fixture(&tmpl, root, refs);
        }
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
        let listing: &[&str] = &["for-each-ref", "--format=%(refname) %(objectname) %(symref)"];
        assert_eq!(run(stock, &z, listing), run(stock, &s, listing), "refs after {args:?}");
        assert_eq!(
            z.join(".git/TAG_EDITMSG").exists(),
            s.join(".git/TAG_EDITMSG").exists(),
            "TAG_EDITMSG after {args:?}"
        );
    }

    // A held packed-refs lock fails the deletion transaction. Both binaries
    // retry it for core.packedRefsTimeout (1 s by default); 0 fails at once.
    let timeout = "core.packedRefsTimeout=0";
    for args in [&["-c", timeout, "tag", "-d", "v1"][..], &["-c", timeout, "tag", "-d", "v1", "nosuch"]] {
        for root in [&s, &z] {
            fixture(&tmpl, root, &[]);
            std::fs::write(root.join(".git/packed-refs.lock"), "").unwrap();
        }
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?} under packed-refs.lock");
    }
    let _ = std::fs::remove_dir_all(&base);
}
