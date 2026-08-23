//! Which operations need `packed-refs.lock`, and which only look like they do.
//!
//! git takes that lock in exactly two places. `files_pack_refs()` takes it for the whole run
//! (`refs/files-backend.c:1478`), and `files_transaction_prepare()` takes it only if it built a
//! `packed_transaction` (`:3031-3036`) — which it does only for updates that delete a reference
//! (`:2982-3007`, "This reference has to be deleted from packed-refs if it exists there").
//! Everything else that has to consult `packed-refs` *reads* it unlocked, on purpose
//! (`:3010-3023`): "introducing such a lock now would probably do more harm than good as users
//! rely on there not being a global lock with the "files" backend. […] So instead, we accept the
//! race for now."
//!
//! This port took the lock for those reads too, keyed off the mere existence of
//! `packed-refs.lock` (`gix-ref/src/store/file/transaction/prepare.rs`). The result was an
//! availability failure with no counterpart in git: one held lock — a concurrent `pack-refs`, or
//! one a killed process left behind — and *every* reference creation in the repository failed
//! with `fatal: update_ref failed for ref '…': The lock for the packed-ref file could not be
//! obtained`, while stock git wrote the loose file and exited 0. On a worktree with many
//! concurrent writers that is every branch creation, every `commit`, every `fetch`.
//!
//! The lock is simulated by writing the file, which is what a stale one is. Nothing here spawns
//! a second process or races, so it behaves the same on any filesystem and in CI.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A repository with one commit and the branches `packed` and `loose`, where `packed` has
    /// been moved into `packed-refs` and `loose` has not.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-prlock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let f = Fixture { root: root.clone(), repo: root.join("repo") };
        std::fs::create_dir_all(&f.repo).unwrap();
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.repo.join("a"), "one\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "c0"]);
        f.git(&["branch", "packed"]);
        f.git(&["pack-refs", "--all"]);
        f.git(&["branch", "loose"]);
        f
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(&self.repo, args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let repo = self.repo.clone();
        let out = self.cmd(&repo, args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn lock_path(&self) -> PathBuf {
        self.repo.join(".git/packed-refs.lock")
    }

    /// Leave a `packed-refs.lock` behind, exactly as a killed `pack-refs` would.
    fn hold_packed_refs_lock(&self) {
        std::fs::write(self.lock_path(), "held\n").unwrap();
    }

    fn head_oid(&self) -> String {
        self.run(&["rev-parse", "HEAD"]).1.trim().to_owned()
    }
}

/// The case that made this urgent: creating a reference writes a loose file, which git does
/// without ever consulting `packed-refs.lock`.
#[test]
fn creating_a_ref_ignores_a_held_packed_refs_lock() {
    let f = Fixture::new("create");
    f.hold_packed_refs_lock();

    let (code, out, err) = f.run(&["update-ref", "refs/heads/new", "HEAD"]);
    assert_eq!(code, 0, "creating a ref must not need packed-refs.lock: {out}{err}");
    assert_eq!(err, "", "and must say nothing");
    assert!(
        f.repo.join(".git/refs/heads/new").is_file(),
        "the loose ref has to exist afterwards, not merely be reported as created"
    );
    assert_eq!(f.run(&["rev-parse", "refs/heads/new"]).1.trim(), f.head_oid());
    assert!(f.lock_path().is_file(), "and the lock we did not take is still there");
}

/// Updating a reference whose current value lives in `packed-refs` reads that file — a read git
/// deliberately performs unlocked. The new value lands in a loose file that shadows the packed
/// entry, so `packed-refs` is never written and its lock is never needed.
#[test]
fn updating_a_packed_ref_ignores_a_held_packed_refs_lock() {
    let f = Fixture::new("update-packed");
    let packed_oid = f.run(&["rev-parse", "refs/heads/packed"]).1.trim().to_owned();
    // Move `HEAD` on so the update is a real change; git short-circuits a write that would
    // store the value a ref already has, and then no loose file appears at all.
    std::fs::write(f.repo.join("a"), "two\n").unwrap();
    f.git(&["add", "a"]);
    f.git(&["commit", "-q", "-m", "c1"]);
    let head = f.head_oid();
    assert_ne!(head, packed_oid);
    f.hold_packed_refs_lock();

    // `<old-oid>` forces the compare-and-swap read of `packed-refs` that used to take the lock.
    let (code, out, err) = f.run(&["update-ref", "refs/heads/packed", &head, &packed_oid]);
    assert_eq!(code, 0, "a CAS read of packed-refs must not need its lock: {out}{err}");
    assert_eq!(err, "");
    assert!(
        f.repo.join(".git/refs/heads/packed").is_file(),
        "the update is written loose, shadowing the packed entry"
    );
    assert_eq!(f.run(&["rev-parse", "refs/heads/packed"]).1.trim(), head);
    assert!(
        std::fs::read_to_string(f.repo.join(".git/packed-refs"))
            .unwrap()
            .contains(&packed_oid),
        "and packed-refs itself is left untouched — it was only read"
    );
}

/// The same, through the porcelain that matters most on a busy worktree.
#[test]
fn branch_and_commit_survive_a_held_packed_refs_lock() {
    let f = Fixture::new("porcelain");
    f.hold_packed_refs_lock();

    let (code, out, err) = f.run(&["branch", "from-porcelain"]);
    assert_eq!(code, 0, "`branch` creates a loose ref: {out}{err}");
    assert!(f.repo.join(".git/refs/heads/from-porcelain").is_file());

    let before = f.head_oid();
    std::fs::write(f.repo.join("a"), "two\n").unwrap();
    f.git(&["add", "a"]);
    let (code, out, err) = f.run(&["commit", "-q", "-m", "c1"]);
    assert_eq!(code, 0, "`commit` moves a loose ref: {out}{err}");
    assert_ne!(f.head_oid(), before, "and the branch actually moved");
}

/// Deleting a reference is the one `update-ref` form git *does* lock for, and it reports the
/// failure with `unable_to_lock_message()` — `error:` and exit 1, since
/// `files_transaction_prepare()` passes no `LOCK_DIE_ON_ERROR` (`refs/files-backend.c:3032`).
#[test]
fn deleting_a_ref_still_needs_the_packed_refs_lock() {
    let f = Fixture::new("delete");
    f.hold_packed_refs_lock();

    for name in ["refs/heads/packed", "refs/heads/loose"] {
        let (code, out, err) = f.run(&["update-ref", "-d", name]);
        assert_eq!(code, 1, "deleting {name} must fail while the lock is held: {out}{err}");
        assert!(
            err.starts_with("error: Unable to create '"),
            "git's own wording, not gitoxide's: {err}"
        );
        assert!(
            err.contains(".git/packed-refs.lock': File exists."),
            "the message names the lock file and the errno: {err}"
        );
        assert!(
            err.contains("Another git process seems to be running in this repository"),
            "and carries the holder diagnostic: {err}"
        );
    }
    assert!(
        f.repo.join(".git/refs/heads/loose").is_file(),
        "a refused deletion must not have removed the ref"
    );
}

/// `pack-refs` locks unconditionally — `should_pack_refs()` returns 1 outright for anything but
/// `--auto` (`refs/files-backend.c:1405-1406`), so even a run with nothing left to pack dies.
/// `LOCK_DIE_ON_ERROR` makes it a `die()`, hence `fatal:` and 128 rather than 1.
#[test]
fn pack_refs_dies_on_a_held_lock_even_with_nothing_to_pack() {
    let f = Fixture::new("pack");
    // Nothing is left loose to pack after this one.
    f.git(&["pack-refs", "--all"]);
    f.hold_packed_refs_lock();

    let (code, out, err) = f.run(&["pack-refs", "--all"]);
    assert_eq!(code, 128, "a failed packed_refs_lock() is a die(): {out}{err}");
    assert!(err.starts_with("fatal: Unable to create '"), "stderr: {err}");
    assert!(err.contains(".git/packed-refs.lock': File exists."), "stderr: {err}");
    assert!(
        err.contains("Another git process seems to be running in this repository"),
        "stderr: {err}"
    );
}

/// And the same when there is work to do, where the failure comes out of the packed transaction
/// one layer down instead.
#[test]
fn pack_refs_dies_on_a_held_lock_with_refs_to_pack() {
    let f = Fixture::new("pack-work");
    f.hold_packed_refs_lock();

    let (code, out, err) = f.run(&["pack-refs", "--all"]);
    assert_eq!(code, 128, "stdout: {out}");
    assert!(err.starts_with("fatal: Unable to create '"), "stderr: {err}");
    assert!(err.contains(".git/packed-refs.lock': File exists."), "stderr: {err}");
    assert!(
        f.repo.join(".git/refs/heads/loose").is_file(),
        "a refused pack must not have pruned the loose ref"
    );
}

/// With no lock held, none of this changes what the commands do: `pack-refs` still packs, and
/// it leaves no lock file behind even on the run that had nothing to do.
#[test]
fn the_unlocked_paths_are_unchanged() {
    let f = Fixture::new("unlocked");

    let (code, out, err) = f.run(&["pack-refs", "--all"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(!f.lock_path().exists(), "no stray lock after a packing run");
    assert!(
        !f.repo.join(".git/refs/heads/loose").exists(),
        "`loose` was packed and pruned"
    );

    let (code, out, err) = f.run(&["pack-refs", "--all"]);
    assert_eq!(code, 0, "a second run has nothing to pack: {out}{err}");
    assert!(!f.lock_path().exists(), "and still leaves no lock behind");

    let packed = std::fs::read_to_string(f.repo.join(".git/packed-refs")).unwrap();
    for name in ["refs/heads/main", "refs/heads/packed", "refs/heads/loose"] {
        assert!(packed.contains(name), "{name} missing from packed-refs:\n{packed}");
    }

    // Deleting a packed ref still rewrites `packed-refs` when the lock is free.
    let (code, out, err) = f.run(&["update-ref", "-d", "refs/heads/loose"]);
    assert_eq!(code, 0, "{out}{err}");
    let packed = std::fs::read_to_string(f.repo.join(".git/packed-refs")).unwrap();
    assert!(!packed.contains("refs/heads/loose"), "packed-refs:\n{packed}");
    assert!(packed.contains("refs/heads/packed"), "packed-refs:\n{packed}");
}
