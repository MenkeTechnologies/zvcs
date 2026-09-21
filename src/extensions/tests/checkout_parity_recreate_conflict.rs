//! `git checkout -m <path>` / `git restore --merge <path>` on a conflict that was
//! already resolved.
//!
//! `checkout_paths()` opens with
//!
//! ```c
//! if (opts->merge)
//!         unmerge_index(the_repository->index, &opts->pathspec, CE_MATCHED);
//! ```
//!
//! (builtin/checkout.c:637-638), *before* the pathspec is matched against the
//! index. `unmerge_index()` (resolve-undo.c:155-179) walks the index's resolve-undo
//! (`REUC`) records, and for each matched path `unmerge_index_entry()` drops the
//! stage-0 entry and re-adds one entry per recorded stage — which is what makes an
//! already-resolved conflict come back, markers and all. The record is consumed on
//! the way through (`item->util = NULL`, resolve-undo.c:177), so a second `-m` has
//! nothing left to re-create.
//!
//! The port did none of that: with no unmerged entry left in the index there was
//! nothing to merge, so `git checkout -m a.txt` quietly rewrote the file from the
//! resolved stage-0 blob and reported `Updated 0 paths from the index` — losing the
//! conflict the flag exists to bring back.
//!
//! The reporting is the second half of the same code path. `checkout_merged()`
//! counts into `nr_unmerged`, not `nr_checkouts`, and `checkout_worktree()` ends:
//!
//! ```c
//! if (nr_unmerged)
//!         fprintf_ln(stderr, Q_("Recreated %d merge conflict",
//!                               "Recreated %d merge conflicts", nr_unmerged),
//!                    nr_unmerged);
//! if (opts->source_tree)
//!         …
//! else if (!nr_unmerged || nr_checkouts)
//!         fprintf_ln(stderr, Q_("Updated %d path from the index", …));
//! ```
//!
//! (builtin/checkout.c:494-511) — so a run that only recreated conflicts prints the
//! `Recreated` line and *not* the `Updated` one. `restore` never sets
//! `count_checkout_paths` (it has no `parse_branchname_arg()` to set it,
//! builtin/checkout.c:1471), so it stays silent either way.
//!
//! Every expectation below was measured against stock git 2.55.0 first.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    /// The scratch directory; holds `repo/` and a `home/` kept *outside* the
    /// worktree, because a `HOME` pointing at the repo makes zvcs drop its own
    /// state files into the files under test.
    base: PathBuf,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fixture {
    /// `a.txt` conflicted by a merge of `other` into `main`, then resolved and
    /// staged — so the index holds one stage-0 entry plus a resolve-undo record.
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("zvcs-reconf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let f = Fixture { base, root };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("a.txt", "l1\nl2\nl3\n");
        f.write("k.txt", "base\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["branch", "other"]);
        f.write("a.txt", "main1\nl2\nl3\n");
        f.git(&["commit", "-qam", "ours"]);
        f.git(&["checkout", "-q", "other"]);
        f.write("a.txt", "other1\nl2\nl3\n");
        f.git(&["commit", "-qam", "theirs"]);
        f.git(&["checkout", "-q", "main"]);
        // Conflicts; the merge is expected to fail, which is the point.
        f.git(&["merge", "other"]);
        f
    }

    /// Resolve the conflict and stage it, leaving the resolve-undo record behind.
    fn resolve(&self) {
        self.write("a.txt", "resolved\n");
        self.git(&["add", "a.txt"]);
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn git(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("HOME", self.base.join("home"))
            .env("ZVCS_HOME", self.base.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
            .output()
            .unwrap();
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        (s, out.status.code().unwrap_or(-1))
    }

    /// `git ls-files --stage` lines for one path, as `<mode> <oid> <stage>`.
    fn stages(&self, path: &str) -> Vec<String> {
        let (out, _) = self.git(&["ls-files", "--stage", "--", path]);
        out.lines()
            .filter_map(|l| {
                let (meta, _name) = l.split_once('\t')?;
                let mut it = meta.split_whitespace();
                Some(format!("{} {}", it.next()?, it.nth(1)?))
            })
            .collect()
    }
}

#[test]
fn checkout_dash_m_recreates_a_resolved_conflict_from_resolve_undo() {
    let f = Fixture::new("co");
    f.resolve();
    assert_eq!(f.stages("a.txt").len(), 1, "the conflict must be resolved first");

    let (out, rc) = f.git(&["checkout", "-m", "a.txt"]);
    assert_eq!(rc, 0, "checkout -m should succeed, got: {out}");

    // The three stages are back in the index, in order.
    assert_eq!(
        f.stages("a.txt"),
        vec!["100644 1", "100644 2", "100644 3"],
        "unmerge_index() must restore all three recorded stages: {out}"
    );
    // And the worktree file is the conflicted re-merge, not the resolution.
    let body = f.read("a.txt");
    assert!(
        body.contains("<<<<<<< ours") && body.contains(">>>>>>> theirs"),
        "the conflict must be re-created in the worktree, got: {body:?}"
    );
    assert!(
        body.contains("main1") && body.contains("other1"),
        "both sides must appear in the re-created conflict, got: {body:?}"
    );
    assert!(
        !body.contains("resolved"),
        "the staged resolution must be overwritten, got: {body:?}"
    );
}

#[test]
fn recreating_a_conflict_reports_recreated_and_suppresses_the_updated_line() {
    let f = Fixture::new("count");
    f.resolve();

    let (out, rc) = f.git(&["checkout", "-m", "a.txt"]);
    assert_eq!(rc, 0, "checkout -m should succeed, got: {out}");
    assert!(
        out.contains("Recreated 1 merge conflict"),
        "nr_unmerged must be reported as a recreated conflict, got: {out:?}"
    );
    assert!(
        !out.contains("Updated"),
        "`else if (!nr_unmerged || nr_checkouts)` suppresses the Updated line when only \
         conflicts were recreated, got: {out:?}"
    );

    // `-q` sets count_checkout_paths to 0, so neither line is written.
    f.resolve();
    let (quiet, rc) = f.git(&["checkout", "-q", "-m", "a.txt"]);
    assert_eq!(rc, 0, "checkout -q -m should succeed, got: {quiet}");
    assert_eq!(quiet, "", "--quiet must silence both counters, got: {quiet:?}");
}

#[test]
fn the_resolve_undo_record_is_consumed_so_a_second_dash_m_is_a_no_op() {
    let f = Fixture::new("once");
    f.resolve();

    let (first, rc) = f.git(&["checkout", "-m", "a.txt"]);
    assert_eq!(rc, 0, "first checkout -m should succeed, got: {first}");
    let conflicted = f.read("a.txt");

    // Now the path is genuinely unmerged, so `unmerge_index_entry()`'s "yes, it is
    // already unmerged" arm leaves it alone and `checkout_merged()` re-merges the
    // same three stages — same bytes, and still one recreated conflict.
    let (second, rc) = f.git(&["checkout", "-m", "a.txt"]);
    assert_eq!(rc, 0, "second checkout -m should succeed, got: {second}");
    assert_eq!(
        f.read("a.txt"),
        conflicted,
        "re-merging the same stages must be byte-identical: {second}"
    );
    assert!(
        second.contains("Recreated 1 merge conflict"),
        "the second run still recreates the conflict it re-merged, got: {second:?}"
    );
}

#[test]
fn restore_merge_recreates_the_conflict_without_reporting_counts() {
    let f = Fixture::new("restore");
    f.resolve();

    let (out, rc) = f.git(&["restore", "--merge", "a.txt"]);
    assert_eq!(rc, 0, "restore --merge should succeed, got: {out}");
    assert_eq!(
        out, "",
        "restore never sets count_checkout_paths, so it prints nothing: {out:?}"
    );
    assert_eq!(
        f.stages("a.txt"),
        vec!["100644 1", "100644 2", "100644 3"],
        "restore --merge must go through unmerge_index() too"
    );
    let body = f.read("a.txt");
    assert!(
        body.contains("<<<<<<< ours") && body.contains(">>>>>>> theirs"),
        "restore --merge must write the conflicted re-merge, got: {body:?}"
    );
}

#[test]
fn a_conflict_resolved_to_removal_still_comes_back() {
    let f = Fixture::new("rm");
    // `git rm` resolves the conflict by deleting the path: no index entry at all
    // remains, only the resolve-undo record. `unmerge_index()` runs before the
    // pathspec is matched, which is the only reason the spec can still name it.
    f.git(&["rm", "-q", "-f", "a.txt"]);
    assert!(f.stages("a.txt").is_empty(), "the path must be gone from the index");

    let (out, rc) = f.git(&["checkout", "-m", "a.txt"]);
    assert_eq!(rc, 0, "checkout -m should succeed, got: {out}");
    assert_eq!(
        f.stages("a.txt"),
        vec!["100644 1", "100644 2", "100644 3"],
        "the stages must be re-added for a path resolved to removal: {out}"
    );
    assert!(
        f.read("a.txt").contains("<<<<<<< ours"),
        "the deleted file must be written back as the conflict"
    );
}
