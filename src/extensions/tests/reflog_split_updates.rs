//! The two reflog entries git's files backend synthesises that have no ref edit of
//! their own, and the one it deliberately withholds.
//!
//! `lock_ref_for_update()` (refs/files-backend.c) turns a single caller request into
//! several updates before anything is written:
//!
//! * `split_symref_update()` rewrites an edit of `HEAD` into a real edit of the branch
//!   plus a `REF_LOG_ONLY` edit of `HEAD`;
//! * `split_head_update()` adds a `REF_LOG_ONLY` edit of `HEAD` when the caller edits
//!   the branch `HEAD` points at directly;
//! * and `files_transaction_finish()` writes a reflog entry for an update that is
//!   `REF_NEEDS_COMMIT || REF_LOG_ONLY` — so the log-only halves are logged even when
//!   the value does not move, while the branch itself is not.
//!
//! Each assertion below was measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const NULL_ID: &str = "0000000000000000000000000000000000000000";

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
    /// `main` with two commits.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-reflogsplit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.work.join("f.txt"), b"a\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("f.txt"), b"b\n").unwrap();
        f.git(&["commit", "-q", "-am", "two"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn rev(&self, spec: &str) -> String {
        let out = self.cmd(&["rev-parse", spec]).output().unwrap();
        assert!(out.status.success(), "`git rev-parse {spec}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn log_lines(&self, rel: &str) -> Vec<String> {
        std::fs::read_to_string(self.work.join(".git/logs").join(rel))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn log_exists(&self, rel: &str) -> bool {
        self.work.join(".git/logs").join(rel).exists()
    }
}

/// `(old id, new id, message)`; the message is empty when the entry carries none.
fn split_entry(line: &str) -> (String, String, String) {
    let (ids, message) = match line.split_once('\t') {
        Some((ids, message)) => (ids, message.to_string()),
        None => (line, String::new()),
    };
    let mut fields = ids.split(' ');
    (
        fields.next().unwrap().to_string(),
        fields.next().unwrap().to_string(),
        message,
    )
}

/// `git reset` with no argument moves `HEAD` to where it already is. The branch value
/// does not change, so `REF_NEEDS_COMMIT` stays clear and `refs/heads/main` is not
/// logged — but the log-only `HEAD` half is written regardless.
#[test]
fn reset_to_head_logs_head_only() {
    let f = Fixture::new("reset-noop");
    let head = f.rev("HEAD");
    let branch_before = f.log_lines("refs/heads/main").len();

    std::fs::write(f.work.join("f.txt"), b"dirty\n").unwrap();
    f.git(&["reset"]);

    let (old, new, message) = split_entry(f.log_lines("HEAD").last().expect("a HEAD entry"));
    assert_eq!(old, head, "old and new are both the unmoved HEAD");
    assert_eq!(new, head);
    assert_eq!(message, "reset: moving to HEAD");
    assert_eq!(
        f.log_lines("refs/heads/main").len(),
        branch_before,
        "the branch value never moved, so its log must be untouched"
    );
}

/// Editing the checked-out branch by name is mirrored into `.git/logs/HEAD` with the
/// same ids and message — `split_head_update()`'s whole purpose.
#[test]
fn updating_the_current_branch_mirrors_into_head() {
    let f = Fixture::new("split-head");
    let before = f.rev("HEAD");
    let target = f.rev("HEAD~1");

    f.git(&["update-ref", "-m", "parity update", "refs/heads/main", "HEAD~1"]);

    let head = split_entry(f.log_lines("HEAD").last().expect("a HEAD entry"));
    let branch = split_entry(f.log_lines("refs/heads/main").last().expect("a branch entry"));
    assert_eq!(head, (before.clone(), target.clone(), "parity update".to_string()));
    assert_eq!(branch, (before, target, "parity update".to_string()));
}

/// A branch that `HEAD` does *not* point at gets no mirrored entry.
#[test]
fn updating_another_branch_leaves_head_alone() {
    let f = Fixture::new("split-head-other");
    f.git(&["branch", "side", "HEAD~1"]);
    let head_before = f.log_lines("HEAD");

    f.git(&["update-ref", "refs/heads/side", "HEAD"]);

    assert_eq!(f.log_lines("HEAD"), head_before);
}

/// `--no-deref` writes `HEAD` itself. git still resolves the symref first so the
/// entry's old field is the value `HEAD` pointed at, not the null id.
#[test]
fn no_deref_head_records_the_resolved_previous_value() {
    let f = Fixture::new("no-deref");
    let before = f.rev("HEAD");
    let target = f.rev("HEAD~1");

    f.git(&["update-ref", "--no-deref", "HEAD", "HEAD~1"]);

    let (old, new, _) = split_entry(f.log_lines("HEAD").last().expect("a HEAD entry"));
    assert_ne!(old, NULL_ID, "a symbolic previous value must still resolve");
    assert_eq!(old, before);
    assert_eq!(new, target);
}

/// Deleting through `HEAD` removes the branch and the branch's log, but the log-only
/// `HEAD` half both survives and gains a `<old> <null>` entry.
#[test]
fn deleting_through_head_keeps_the_head_log() {
    let f = Fixture::new("delete-head");
    let before = f.rev("HEAD");

    f.git(&["update-ref", "-d", "HEAD"]);

    assert!(f.log_exists("HEAD"), ".git/logs/HEAD must survive a log-only delete");
    assert!(
        !f.log_exists("refs/heads/main"),
        "the real delete takes the branch log with it"
    );
    let (old, new, message) = split_entry(f.log_lines("HEAD").last().expect("a HEAD entry"));
    assert_eq!(old, before);
    assert_eq!(new, NULL_ID);
    assert_eq!(message, "", "`update-ref -d` passes no message");
    assert!(
        !f.work.join(".git/refs/heads/main").exists(),
        "the branch itself is gone"
    );
}

/// Deleting the checked-out branch *by name* takes the same route as deleting through
/// `HEAD`: `split_head_update()` adds the `REF_LOG_ONLY` half, so `.git/logs/HEAD` gains a
/// `<old> <null>` entry and survives while the branch and its own log are unlinked.
///
/// `--create-reflog` is passed here because it is the spelling that first exposed the gap,
/// but it changes nothing — see [`delete_current_branch_ignores_create_reflog`].
#[test]
fn deleting_the_current_branch_by_name_logs_into_head() {
    let f = Fixture::new("delete-current-create-reflog");
    let before = f.rev("HEAD");

    f.git(&["update-ref", "-d", "--create-reflog", "refs/heads/main"]);

    assert!(f.log_exists("HEAD"), ".git/logs/HEAD must survive the deletion");
    assert!(
        !f.log_exists("refs/heads/main"),
        "the deleted branch takes its own log with it"
    );
    assert!(
        !f.work.join(".git/refs/heads/main").exists(),
        "the branch itself is gone"
    );
    let (old, new, message) = split_entry(f.log_lines("HEAD").last().expect("a HEAD entry"));
    assert_eq!(old, before, "the old side is the value the branch held");
    assert_eq!(new, NULL_ID);
    assert_eq!(message, "", "no -m was given");
}

/// The same deletion without `--create-reflog` produces a byte-identical result.
/// `cmd_update_ref` (builtin/update-ref.c) ORs `create_reflog_flag` into its `update_ref()`
/// call only — `delete_ref()` never sees it — so the flag cannot force a log into existence
/// on this path. Both spellings must agree entry for entry.
#[test]
fn delete_current_branch_ignores_create_reflog() {
    let with = Fixture::new("delete-current-with");
    let without = Fixture::new("delete-current-without");

    with.git(&["update-ref", "-d", "--create-reflog", "refs/heads/main"]);
    without.git(&["update-ref", "-d", "refs/heads/main"]);

    assert_eq!(
        with.log_lines("HEAD").len(),
        without.log_lines("HEAD").len(),
        "--create-reflog must not add an entry a plain delete does not write"
    );
    let a = split_entry(with.log_lines("HEAD").last().expect("a HEAD entry"));
    let b = split_entry(without.log_lines("HEAD").last().expect("a HEAD entry"));
    assert_eq!(a.1, NULL_ID);
    assert_eq!((a.1, a.2), (b.1, b.2), "same new value, same (absent) message");
    assert_eq!(
        with.log_exists("refs/heads/main"),
        without.log_exists("refs/heads/main"),
        "neither spelling keeps the deleted branch's own log"
    );
    assert!(!with.log_exists("refs/heads/main"));
}

/// `-m <reason>` reaches that mirrored entry. It is the only log the reason can land in,
/// since the branch's own log is unlinked by the same transaction —
/// `ref_transaction_add_update(transaction, "HEAD", …, update->msg)` in
/// `split_head_update()` is what carries it across.
#[test]
fn deleting_the_current_branch_records_the_reason() {
    let f = Fixture::new("delete-current-message");
    let before = f.rev("HEAD");

    f.git(&["update-ref", "-m", "parity delete", "-d", "refs/heads/main"]);

    let (old, new, message) = split_entry(f.log_lines("HEAD").last().expect("a HEAD entry"));
    assert_eq!(old, before);
    assert_eq!(new, NULL_ID);
    assert_eq!(message, "parity delete");
}

/// The `--stdin` `delete` command shares the route, and `-m` applies to the whole batch.
#[test]
fn stdin_delete_of_the_current_branch_logs_into_head() {
    use std::io::Write;

    let f = Fixture::new("delete-current-stdin");
    let before = f.rev("HEAD");

    let mut child = f
        .cmd(&["update-ref", "-m", "batch delete", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"delete refs/heads/main\n")
        .unwrap();
    assert!(child.wait().unwrap().success());

    let (old, new, message) = split_entry(f.log_lines("HEAD").last().expect("a HEAD entry"));
    assert_eq!(old, before);
    assert_eq!(new, NULL_ID);
    assert_eq!(message, "batch delete");
    assert!(!f.log_exists("refs/heads/main"));
}

/// Deleting a branch `HEAD` does *not* point at splits off nothing, so `.git/logs/HEAD`
/// must be left exactly as it was — the guard that keeps the fix from over-reaching.
#[test]
fn deleting_another_branch_leaves_the_head_log_alone() {
    let f = Fixture::new("delete-other");
    f.git(&["branch", "side", "HEAD~1"]);
    let head_before = f.log_lines("HEAD");

    f.git(&["update-ref", "-d", "refs/heads/side"]);

    assert_eq!(f.log_lines("HEAD"), head_before);
    assert!(!f.log_exists("refs/heads/side"), "its own log still goes away");
}

/// With `HEAD` detached there is no `head_ref` to compare against, so `split_head_update()`
/// returns early and the deletion is logged nowhere.
#[test]
fn deleting_a_branch_while_detached_logs_nothing() {
    let f = Fixture::new("delete-detached");
    f.git(&["checkout", "-q", "--detach", "HEAD"]);
    let head_before = f.log_lines("HEAD");

    f.git(&["update-ref", "-d", "refs/heads/main"]);

    assert_eq!(f.log_lines("HEAD"), head_before);
    assert!(!f.work.join(".git/refs/heads/main").exists());
}

/// `branch -d` reaches the same mirror, but only where git lets it delete the branch `HEAD`
/// names: `delete_branches()` (builtin/branch.c) refuses on
/// `find_shared_symref(…, "HEAD", name) && !wt->is_bare`, so a bare repository — whose
/// `HEAD` is a default for future clones rather than a checkout — goes through. The entry it
/// leaves carries no message, since `refs_delete_refs()` is called with a null `logmsg`.
#[test]
fn branch_delete_in_a_bare_repo_logs_into_head() {
    let f = Fixture::new("branch-delete-bare");
    let bare = f.root.join("bare.git");
    let tip = f.rev("HEAD");

    let run = |args: &[&str]| {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&f.root)
            .env("HOME", &f.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let bare_str = bare.to_str().unwrap();
    run(&["init", "-q", "--bare", "-b", "main", bare_str]);
    run(&["-C", bare_str, "config", "core.logAllRefUpdates", "true"]);
    let work = f.work.to_str().unwrap();
    run(&["-C", work, "push", "-q", bare_str, "main:main"]);
    run(&["-C", bare_str, "branch", "keep", "main"]);

    // The push created `logs/HEAD` through the same split, so the deletion appends to it.
    run(&["-C", bare_str, "branch", "-D", "main"]);

    let log = std::fs::read_to_string(bare.join("logs/HEAD")).expect("HEAD log");
    let (old, new, message) = split_entry(log.lines().last().expect("a HEAD entry"));
    assert_eq!(old, tip);
    assert_eq!(new, NULL_ID);
    assert_eq!(message, "", "branch deletion passes no reflog message");
    assert!(
        !bare.join("logs/refs/heads/main").exists(),
        "the deleted branch takes its own log with it"
    );
    assert!(!bare.join("refs/heads/main").exists(), "the branch itself is gone");
}

/// Packing refs moves no value. git stamps those updates `REF_SKIP_CREATE_REFLOG`,
/// which `split_head_update()` checks before anything else, so `.git/logs/HEAD` must
/// not grow — and the run must not fail for want of a committer to stamp an entry with.
#[test]
fn packing_refs_adds_no_head_entry() {
    let f = Fixture::new("pack-refs");
    f.git(&["branch", "side"]);
    let head_before = f.log_lines("HEAD");

    f.git(&["pack-refs", "--all"]);

    assert_eq!(f.log_lines("HEAD"), head_before);
    assert_eq!(f.rev("refs/heads/main"), f.rev("HEAD"), "packing preserved the value");
}

// ---------------------------------------------------------------------------
// The other side of the rule: `files_copy_or_rename_ref()` is not a transaction.
// It ends in `commit_ref_update()`, which calls `files_log_ref_write()` outright,
// so a rename or copy onto a branch's own name logs an entry the transaction
// machinery above would have withheld.
// ---------------------------------------------------------------------------

/// `branch -m main` on the checked-out `main`. The branch's own log gains the
/// `<tip> <tip>` entry `commit_ref_update()` writes; `.git/logs/HEAD` gains the pair
/// left by the deletion of the old name and the re-creation of the new one.
#[test]
fn renaming_a_branch_onto_its_own_name_logs_the_branch() {
    let f = Fixture::new("rename-same");
    let tip = f.rev("HEAD");
    let branch_before = f.log_lines("refs/heads/main").len();
    let head_before = f.log_lines("HEAD").len();

    f.git(&["branch", "-m", "main"]);

    let message = "Branch: renamed refs/heads/main to refs/heads/main".to_string();
    let branch = f.log_lines("refs/heads/main");
    assert_eq!(
        branch.len(),
        branch_before + 1,
        "the rename logs the branch even though its value never moved"
    );
    assert_eq!(
        split_entry(branch.last().unwrap()),
        (tip.clone(), tip.clone(), message.clone())
    );

    let head = f.log_lines("HEAD");
    assert_eq!(head.len(), head_before + 2, "the delete half and the create half");
    assert_eq!(
        split_entry(&head[head.len() - 2]),
        (tip.clone(), NULL_ID.to_string(), message.clone()),
        "the old name going away"
    );
    assert_eq!(
        split_entry(&head[head.len() - 1]),
        (tip.clone(), tip, message),
        "the new name arriving"
    );
}

/// `branch -C main main` takes the copy route, which deletes nothing — so `.git/logs/HEAD`
/// gains only the one entry `commit_ref_update()` mirrors into it, not a pair.
#[test]
fn copying_a_branch_onto_its_own_name_logs_the_branch() {
    let f = Fixture::new("copy-same");
    let tip = f.rev("HEAD");
    let branch_before = f.log_lines("refs/heads/main").len();
    let head_before = f.log_lines("HEAD").len();

    f.git(&["branch", "-C", "main", "main"]);

    let message = "Branch: copied refs/heads/main to refs/heads/main".to_string();
    let branch = f.log_lines("refs/heads/main");
    assert_eq!(branch.len(), branch_before + 1);
    assert_eq!(
        split_entry(branch.last().unwrap()),
        (tip.clone(), tip.clone(), message.clone())
    );

    let head = f.log_lines("HEAD");
    assert_eq!(head.len(), head_before + 1, "a copy deletes nothing");
    assert_eq!(split_entry(head.last().unwrap()), (tip.clone(), tip, message));
}

/// Renaming a branch `HEAD` does not point at onto its own name logs that branch and
/// nothing else — `commit_ref_update()`'s mirror only fires when `HEAD` names the ref.
#[test]
fn renaming_an_idle_branch_onto_its_own_name_leaves_the_head_log_alone() {
    let f = Fixture::new("rename-same-idle");
    f.git(&["branch", "side", "HEAD~1"]);
    let tip = f.rev("refs/heads/side");
    let side_before = f.log_lines("refs/heads/side").len();
    let head_before = f.log_lines("HEAD");

    f.git(&["branch", "-m", "side", "side"]);

    let side = f.log_lines("refs/heads/side");
    assert_eq!(side.len(), side_before + 1);
    assert_eq!(
        split_entry(side.last().unwrap()),
        (
            tip.clone(),
            tip,
            "Branch: renamed refs/heads/side to refs/heads/side".to_string()
        )
    );
    assert_eq!(f.log_lines("HEAD"), head_before);
}

/// The line the rename fix must not cross: a *transactional* update to the value a branch
/// already holds still logs nothing of its own. `lock_ref_for_update()` withholds
/// `REF_NEEDS_COMMIT` for it, and only the log-only `HEAD` half — present when the branch
/// is the checked-out one — is written.
#[test]
fn updating_a_branch_to_the_value_it_already_holds_leaves_its_log_alone() {
    let f = Fixture::new("update-noop");
    f.git(&["branch", "side"]);
    let tip = f.rev("HEAD");
    let side_before = f.log_lines("refs/heads/side");
    let main_before = f.log_lines("refs/heads/main");
    let head_before = f.log_lines("HEAD").len();

    f.git(&["update-ref", "-m", "idle side", "refs/heads/side", &tip]);
    assert_eq!(
        f.log_lines("refs/heads/side"),
        side_before,
        "an unchanged value writes no entry, message or not"
    );
    assert_eq!(f.log_lines("HEAD").len(), head_before, "and nothing mirrors into HEAD");

    f.git(&["update-ref", "-m", "idle main", "refs/heads/main", &tip]);
    assert_eq!(
        f.log_lines("refs/heads/main"),
        main_before,
        "the checked-out branch is no different"
    );
    let head = f.log_lines("HEAD");
    assert_eq!(head.len(), head_before + 1, "only the log-only HEAD half is written");
    assert_eq!(
        split_entry(head.last().unwrap()),
        (tip.clone(), tip, "idle main".to_string())
    );
}
