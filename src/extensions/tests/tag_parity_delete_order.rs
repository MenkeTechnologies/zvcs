//! `git tag -d` reports in passes, not per name.
//!
//! `delete_tags()` (builtin/tag.c:118-140) first walks every name through
//! `for_each_tag_name()`, which prints `tag '<name>' not found.` for each one that
//! is not a tag; then deletes the rest in one `refs_delete_refs()` transaction;
//! and only then prints `Deleted tag` for each collected ref that no longer
//! exists. So with both streams on one pipe every `not found` line precedes every
//! `Deleted` line, whatever order the names came in — and a name given twice
//! fails the transaction after the `not found` lines, deleting nothing. zvcs
//! deleted and reported name by name, interleaving the two, and returned on the
//! duplicate before reporting the missing names. Expectations captured from
//! stock git 2.56.0.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// stdout and stderr merged in arrival order, and the exit code.
fn git(dir: &Path, args: &str) -> (String, i32) {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("\"$BIN\" {args} 2>&1"))
        .env("BIN", BIN)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run the binary under test");
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code().expect("no signal"))
}

#[test]
fn not_found_lines_precede_deleted_lines_and_a_duplicate_deletes_nothing() {
    let root = std::env::temp_dir().join(format!("zvcs-tag-delete-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, "init -q -b main");
    git(&root, "commit -q --allow-empty -m x");
    git(&root, "tag tt");
    git(&root, "tag bt");

    assert_eq!(
        git(&root, "tag -d nope2 tt nope bt tt"),
        (
            "error: tag 'nope2' not found.\n\
             error: tag 'nope' not found.\n\
             error: could not delete references: multiple updates for ref 'refs/tags/tt' not allowed\n"
                .to_owned(),
            1
        )
    );
    assert_eq!(git(&root, "tag"), ("bt\ntt\n".to_owned(), 0));

    let (out, code) = git(&root, "tag -d tt nope bt");
    assert_eq!(code, 1);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert_eq!(lines[0], "error: tag 'nope' not found.");
    assert!(lines[1].starts_with("Deleted tag 'tt' (was "), "{out}");
    assert!(lines[2].starts_with("Deleted tag 'bt' (was "), "{out}");
    assert_eq!(git(&root, "tag"), (String::new(), 0));
    let _ = std::fs::remove_dir_all(&root);
}
