//! `git apply --3way` over a **binary** patch, and the merge driver it lands in.
//!
//! `try_threeway()` (apply.c:3716) has a short list of patches it declines to
//! merge — deletions, gitlinks, a creation that is not `direct_to_threeway`, a
//! pure rename — and a binary patch is not on it. It rebuilds the post-image with
//! `apply_fragments()`, which dispatches to `apply_binary()` before it ever looks
//! at a fragment, so `--3way` on a binary patch is a real three-way merge of three
//! blobs and reports as one (`Applied patch to '%s' cleanly.`, apply.c:3797).
//!
//! When the three blobs then reach `ll_merge()`, `ll_xdl_merge()` (merge-ll.c:117)
//! checks all three for a NUL byte and hands them to `ll_binary_merge()`
//! (merge-ll.c:58) instead of xdiff. That function does not merge: it takes one
//! whole side and says so.
//!
//! * default — `LL_MERGE_BINARY_CONFLICT`: `warning: Cannot merge binary files:
//!   <path> (ours vs. theirs)` from `three_way_merge()` (apply.c:3656), then
//!   `Applied patch to '<path>' with conflicts.`, the result is *ours*, the path is
//!   left at stage 1/2/3, exit 1.
//! * `--ours` / `--theirs` — `LL_MERGE_OK`: that side whole, no warning, clean,
//!   exit 0.
//! * `--union` — not a case label in that switch, so it falls through `default`
//!   and conflicts exactly like a plain run.
//!
//! Skipping `try_threeway()` for binary patches loses all of it: the report line,
//! the ability to merge a binary patch onto a moved-on tree at all, and the
//! conflict stages. Running the *text* merge on binary content instead is worse —
//! it writes `<<<<<<<` markers into a binary file.
//!
//! Every expectation below was measured against stock git 2.55.0
//! (`/usr/local/bin/git`, not the port).
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The committed content of `b.bin`: NUL-bearing, so every merge of it is binary.
const BASE: &[u8] = b"\x00\x01\x02\x03bin\n";
/// What the patch produces.
const THEIRS: &[u8] = b"\x00\x01\x02\xffzzz\n";
/// What the tree moved on to, so the merge is not a trivial resolution.
const OURS: &[u8] = b"\x00\x01\x02\x03OURS\n";

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
    /// Commit [`BASE`], record `git diff --binary` for [`THEIRS`] against it, then
    /// commit [`OURS`] on top — so the patch's `index` line names a blob that is
    /// still reachable but is no longer what `b.bin` holds. That is the only shape
    /// in which `--3way` does any work.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-apply3bin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("b.bin", BASE);
        f.git(&["add", "b.bin"]);
        f.git(&["commit", "-qm", "base"]);
        f.write("b.bin", THEIRS);
        let out = f.cmd(&["diff", "--binary"]).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        std::fs::write(f.root.join("p.patch"), &out.stdout).unwrap();
        f.git(&["checkout", "-q", "--", "b.bin"]);
        f
    }

    /// Move the tree on to [`OURS`], committed so the index matches the worktree
    /// and `--index` has nothing to object to.
    fn diverge(&self) {
        self.write("b.bin", OURS);
        self.git(&["commit", "-qam", "ours"]);
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn read(&self, path: &str) -> Vec<u8> {
        std::fs::read(self.work.join(path)).unwrap()
    }

    /// `git apply --3way <args> p.patch`, returning `(exit code, stderr)`.
    fn apply3(&self, args: &[&str]) -> (i32, String) {
        let patch = self.root.join("p.patch");
        let patch = patch.to_str().unwrap();
        let mut argv = vec!["apply", "--3way"];
        argv.extend_from_slice(args);
        argv.push(patch);
        let out = self.cmd(&argv).output().unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// `git ls-files -s b.bin`, one line per stage.
    fn stages(&self) -> Vec<String> {
        let out = self.cmd(&["ls-files", "-s", "b.bin"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// The trivial resolution: the tree has not moved, so `three_way_merge()` resolves
/// to theirs without reaching any driver (apply.c:3641-3642). Stock, measured:
/// `Applied patch to 'b.bin' cleanly.` on stderr, exit 0, file equal to [`THEIRS`].
///
/// A port that declines `try_threeway()` for binary patches still produces the
/// right bytes here — via the direct binary path — but says nothing, which is the
/// only way this case shows the difference.
#[test]
fn a_binary_patch_under_threeway_reports_the_merge_it_did() {
    let f = Fixture::new("clean");
    let (code, err) = f.apply3(&[]);
    assert_eq!(code, 0, "{err:?}");
    assert_eq!(err.trim_end(), "Applied patch to 'b.bin' cleanly.");
    assert_eq!(f.read("b.bin"), THEIRS);
}

/// The non-trivial merge, which is where `ll_binary_merge()` actually runs.
/// Stock, measured:
///   warning: Cannot merge binary files: b.bin (ours vs. theirs)
///   Applied patch to 'b.bin' with conflicts.
/// at exit 1, with the worktree left holding *ours* untouched and `b.bin` recorded
/// at stages 1, 2 and 3.
///
/// This is the test that catches a text merge being run on binary content: that
/// path writes conflict markers into `b.bin` and leaves no `Cannot merge` warning.
#[test]
fn a_conflicting_binary_merge_takes_ours_whole_and_says_so() {
    let f = Fixture::new("conflict");
    f.diverge();

    let (code, err) = f.apply3(&[]);
    assert_eq!(code, 1, "{err:?}");
    assert_eq!(
        err.trim_end(),
        "warning: Cannot merge binary files: b.bin (ours vs. theirs)\n\
         Applied patch to 'b.bin' with conflicts.\n\
         U b.bin"
    );
    assert_eq!(
        f.read("b.bin"),
        OURS,
        "ll_binary_merge takes one side whole — no markers, no splice"
    );
    assert!(
        !f.read("b.bin").starts_with(b"<<<<<<<"),
        "a text merge would have written markers into a binary file"
    );

    let stages = f.stages();
    assert_eq!(stages.len(), 3, "stage 1/2/3 recorded: {stages:?}");
    assert!(stages[0].contains(" 1\tb.bin"), "{stages:?}");
    assert!(stages[1].contains(" 2\tb.bin"), "{stages:?}");
    assert!(stages[2].contains(" 3\tb.bin"), "{stages:?}");
}

/// merge-ll.c:85-92. The two one-side variants return `LL_MERGE_OK`, so the same
/// conflicting merge becomes clean and silent apart from the report line. Stock,
/// measured: `Applied patch to 'b.bin' cleanly.`, exit 0, and the named side's
/// bytes verbatim — `--ours` leaves the file exactly as it was.
#[test]
fn the_one_side_variants_resolve_a_binary_conflict_cleanly() {
    let f = Fixture::new("ours");
    f.diverge();
    let (code, err) = f.apply3(&["--ours"]);
    assert_eq!(code, 0, "{err:?}");
    assert_eq!(err.trim_end(), "Applied patch to 'b.bin' cleanly.");
    assert_eq!(f.read("b.bin"), OURS);
    assert_eq!(f.stages().len(), 1, "no conflict stages");

    let g = Fixture::new("theirs");
    g.diverge();
    let (code, err) = g.apply3(&["--theirs"]);
    assert_eq!(code, 0, "{err:?}");
    assert_eq!(err.trim_end(), "Applied patch to 'b.bin' cleanly.");
    assert_eq!(g.read("b.bin"), THEIRS);
    assert_eq!(g.stages().len(), 1, "no conflict stages");
}

/// `XDL_MERGE_FAVOR_UNION` is absent from `ll_binary_merge()`'s switch
/// (merge-ll.c:80-93), so it takes the `default` arm: warning, ours, conflict —
/// identical to a plain `--3way`. Stock, measured: exit 1 with both lines.
///
/// Grouping union with the other two variants (the reading the option name
/// invites) turns a conflict into a silent clean apply.
#[test]
fn union_is_not_one_of_the_binary_merge_variants() {
    let f = Fixture::new("union");
    f.diverge();
    let (code, err) = f.apply3(&["--union"]);
    assert_eq!(code, 1, "{err:?}");
    assert_eq!(
        err.trim_end(),
        "warning: Cannot merge binary files: b.bin (ours vs. theirs)\n\
         Applied patch to 'b.bin' with conflicts.\n\
         U b.bin"
    );
    assert_eq!(f.read("b.bin"), OURS);
    assert_eq!(f.stages().len(), 3);
}
