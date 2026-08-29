//! The index may never claim a file matches content it does not hold.
//!
//! Every command that writes the index after touching the worktree reuses the stat data of the
//! files it just wrote, so a following `status` does not have to re-hash the tree. That reuse is
//! only sound for the entry naming the *same content*: a stat stamped onto an entry that names a
//! different blob asserts "the worktree matches the index" about a file that does not match, and
//! nothing downstream ever questions it — `status` prints nothing, `diff` is empty, `add` stages
//! nothing, `update-index --refresh` has no work to do, and a commit made in that state silently
//! leaves the change out. It is invisible in review because the diff genuinely does not contain
//! it.
//!
//! The invariant asserted here is therefore stated on the *result*, not on any one code path:
//! after each of these sequences, if the file on disk hashes to something other than the entry
//! the index holds, `git status` must say so.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn repo(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-statlie-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    let p = p.canonicalize().unwrap();
    git(&p, &["init", "-q", "-b", "main"]);
    let body: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    std::fs::write(p.join("f.txt"), &body).unwrap();
    std::fs::write(p.join("other.txt"), "other\n").unwrap();
    git(&p, &["add", "f.txt", "other.txt"]);
    git(&p, &["commit", "-q", "-m", "base"]);
    p
}

/// Rewrite one line of `f.txt`, leaving the rest alone.
fn edit(dir: &Path, line: usize, text: &str) {
    let body = std::fs::read_to_string(dir.join("f.txt")).unwrap();
    let out: String = body
        .lines()
        .enumerate()
        .map(|(i, l)| if i + 1 == line { format!("{text}\n") } else { format!("{l}\n") })
        .collect();
    std::fs::write(dir.join("f.txt"), out).unwrap();
}

/// The one thing that must always hold: a worktree file whose content differs from its index entry
/// is reported by `status`.
fn assert_difference_is_visible(dir: &Path, what: &str) {
    let worktree = git(dir, &["hash-object", "f.txt"]).trim().to_owned();
    let staged: String = git(dir, &["ls-files", "-s", "f.txt"])
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    if worktree == staged {
        return;
    }
    let status = git(dir, &["status", "--porcelain", "f.txt"]);
    assert!(
        status.contains("f.txt"),
        "{what}: the index holds {staged} while the worktree holds {worktree}, \
         and `status` reported nothing — the change is invisible"
    );
    // `add` has to be able to stage it, too: a lying stat also silences the add.
    git(dir, &["add", "f.txt"]);
    let after: String = git(dir, &["ls-files", "-s", "f.txt"])
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    assert_eq!(after, worktree, "{what}: `add` did not stage the worktree content");
}

#[test]
fn stash_push_staged_leaves_the_unstaged_work_visible() {
    let p = repo("stash-staged");
    edit(&p, 2, "STAGED EDIT");
    git(&p, &["add", "f.txt"]);
    edit(&p, 38, "WORKTREE EDIT");
    let out = run(&p, &["stash", "push", "-q", "--staged"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_difference_is_visible(&p, "stash push --staged");
}

#[test]
fn stash_push_keep_index_leaves_the_stashed_work_visible() {
    let p = repo("stash-keep-index");
    edit(&p, 2, "STAGED EDIT");
    git(&p, &["add", "f.txt"]);
    let out = run(&p, &["stash", "push", "-q", "--keep-index"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_difference_is_visible(&p, "stash push --keep-index");
}

#[test]
fn stash_pop_leaves_the_restored_work_visible() {
    let p = repo("stash-pop");
    edit(&p, 2, "WORKTREE EDIT");
    git(&p, &["stash", "push", "-q"]);
    git(&p, &["stash", "pop"]);
    assert_difference_is_visible(&p, "stash pop");
}

#[test]
fn read_tree_update_leaves_the_difference_visible() {
    let p = repo("read-tree");
    edit(&p, 2, "SECOND COMMIT");
    git(&p, &["commit", "-q", "-am", "second"]);
    // Read the parent tree into the index *without* updating the worktree: the worktree keeps the
    // newer content while the index goes back, which is exactly the state a stale stat hides.
    git(&p, &["read-tree", "HEAD~1"]);
    assert_difference_is_visible(&p, "read-tree HEAD~1");
}

#[test]
fn reset_mixed_leaves_the_worktree_difference_visible() {
    let p = repo("reset-mixed");
    edit(&p, 2, "SECOND COMMIT");
    git(&p, &["commit", "-q", "-am", "second"]);
    git(&p, &["reset", "-q", "HEAD~1"]);
    assert_difference_is_visible(&p, "reset --mixed HEAD~1");
}

#[test]
fn reset_keep_leaves_a_new_staged_file_untracked() {
    let p = repo("reset-keep");
    std::fs::write(p.join("fresh.txt"), "new\n").unwrap();
    git(&p, &["add", "fresh.txt"]);
    git(&p, &["reset", "--keep"]);
    let status = git(&p, &["status", "--porcelain", "fresh.txt"]);
    assert_eq!(
        status.trim(),
        "?? fresh.txt",
        "`reset --keep` runs a second, MIXED pass over the index (builtin/reset.c:522-524), \
         so a file staged from nowhere is unstaged rather than carried across"
    );
}

/// The racy-clean case, which is the shape the incident took: a file written in the same second as
/// the index that recorded it, and rewritten to the *same length* within that second. The entry's
/// stat still matches the file, so nothing re-hashes it — unless the next index write smudges it,
/// which is what `ce_smudge_racily_clean_entry()` exists for (read-cache.c:2560, called from
/// `do_write_index()` at :2902).
///
/// Without the smudge the change is invisible for good: the next index write moves the index
/// timestamp past the entry's mtime, the entry stops looking racy, and its stat goes on matching a
/// file it no longer describes.
#[test]
fn a_racily_clean_entry_is_smudged_when_the_index_is_written() {
    let p = repo("racy");
    // Ten bytes, and a rewrite of exactly ten bytes in the same second as the `add` below.
    std::fs::write(p.join("r.txt"), "AAAAAAAAA\n").unwrap();
    git(&p, &["add", "r.txt"]);
    std::fs::write(p.join("r.txt"), "BBBBBBBBB\n").unwrap();

    // Any later index write is where git closes the window.
    std::fs::write(p.join("t.txt"), "t\n").unwrap();
    git(&p, &["add", "t.txt"]);

    // The recorded size must now be zero for the racy entry: no file can match it, so every later
    // comparison reads the file.
    let debug = git(&p, &["ls-files", "--debug", "r.txt"]);
    let size = debug
        .lines()
        .find_map(|l| l.trim().strip_prefix("size: "))
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or("<none>")
        .to_owned();
    assert_eq!(
        size, "0",
        "the racily-clean entry kept its recorded size, so its stat still matches a file it no \
         longer describes:\n{debug}"
    );

    let status = git(&p, &["status", "--porcelain", "r.txt"]);
    assert!(
        status.contains("r.txt"),
        "a same-second, same-length rewrite is invisible to `status`"
    );
}
