//! `git replace --edit` re-opens its scratch file, and every replace ref is
//! written under `ref_namespace[NAMESPACE_REPLACE]`.
//!
//! Measured against stock git 2.55.0:
//!
//! * `export_object()` opens `$GIT_DIR/REPLACE_EDITOBJ` with
//!   `O_WRONLY | O_CREAT | O_TRUNC` (builtin/replace.c:239) and nothing ever
//!   unlinks it, so the second `--edit` in a repository overwrites the leftover.
//!   Opening it `O_EXCL` instead makes every `--edit` after the first one fail
//!   with `unable to create …: File exists`, which is what this port did.
//! * `setup.c:1057-1060` seeds that namespace from `GIT_REPLACE_REF_BASE`, so
//!   `GIT_REPLACE_REF_BASE=refs/alt/ git replace <a> <b>` writes
//!   `refs/alt/<a>` — and `git replace -l` then lists it, while
//!   `refs/replace/<a>` stays untouched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
        .env_remove("GIT_REPLACE_REF_BASE")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Three commits, so there is something to replace and something to replace it
/// with.
fn fixture(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-replace-parity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    for n in ["one", "two", "three"] {
        std::fs::write(repo.join("f"), format!("{n}\n")).unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-q", "-m", n]);
    }
    repo
}

#[test]
fn a_second_edit_overwrites_the_leftover_scratch_file() {
    let repo = fixture("edit");
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let parent = git(&repo, &["rev-parse", "HEAD~1"]).trim().to_owned();

    // The first `--edit` writes REPLACE_EDITOBJ and leaves it behind.
    let mut first = Command::new(BIN);
    first
        .args(["replace", "--edit", &head])
        .current_dir(&repo)
        .env("GIT_EDITOR", "perl -i -pe 's/^three$/edited/'");
    let out = first.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        repo.join(".git/REPLACE_EDITOBJ").exists(),
        "git leaves the scratch file behind; this test only means something if it is there"
    );

    // A second `--edit`, on a different object, has to truncate it rather than
    // trip over it. `-p` output of a blob is its content, so this rewrites the
    // parent commit's blob.
    let blob = git(&repo, &["rev-parse", &format!("{parent}:f")]).trim().to_owned();
    let out = Command::new(BIN)
        .args(["replace", "--edit", &blob])
        .current_dir(&repo)
        .env("GIT_EDITOR", "perl -i -pe 's/^two$/second/'")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("File exists"),
        "the second --edit refused to reuse the scratch file: {stderr}"
    );
    assert!(out.status.success(), "second --edit failed: {stderr}");
    assert_eq!(
        git(&repo, &["cat-file", "blob", &blob]),
        "second\n",
        "the second --edit did not take effect"
    );
}

#[test]
fn git_replace_ref_base_moves_the_namespace() {
    let repo = fixture("base");
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let parent = git(&repo, &["rev-parse", "HEAD~1"]).trim().to_owned();

    let out = Command::new(BIN)
        .args(["replace", &head, &parent])
        .current_dir(&repo)
        .env("GIT_REPLACE_REF_BASE", "refs/alt/")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let refs = git(&repo, &["for-each-ref", "--format=%(refname)"]);
    assert!(
        refs.lines().any(|l| l == format!("refs/alt/{head}")),
        "the replace ref was not written under GIT_REPLACE_REF_BASE:\n{refs}"
    );
    assert!(
        !refs.lines().any(|l| l == format!("refs/replace/{head}")),
        "the replace ref was written under refs/replace/ despite GIT_REPLACE_REF_BASE:\n{refs}"
    );

    // Listing reads the same namespace, so the entry is visible only with the
    // variable set.
    let listed = Command::new(BIN)
        .args(["replace", "-l"])
        .current_dir(&repo)
        .env("GIT_REPLACE_REF_BASE", "refs/alt/")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout).trim(),
        head,
        "git replace -l did not list the ref from the moved namespace"
    );
    assert!(
        git(&repo, &["replace", "-l"]).trim().is_empty(),
        "git replace -l listed the moved ref while reading refs/replace/"
    );
}
