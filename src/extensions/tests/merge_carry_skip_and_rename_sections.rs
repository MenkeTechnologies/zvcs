//! Three capabilities that were refused rather than ported, each measured
//! against git 2.55.0 before it was written here.
//!
//! * `git range-diff` renders a commit that renames a file. The inner `git log
//!   -p` carries no `-M`, so it detects with whatever `diff.renames` says — on,
//!   at 50%, unless configured otherwise — and `read_patches()` turns the pair
//!   into ` ## <old> => <new> ##`. The port runs the same `diffcore-rename`
//!   `git diff` and `git log` use. A directory that appears or disappears is not
//!   a section at all: gix reports the tree entry alongside the files inside it,
//!   while git's recursive walk reports only the files.
//!
//! * `git checkout -m` carries local changes across a switch the two-way
//!   `unpack_trees()` refuses. git 2.55 does it by stashing them, switching the
//!   clean worktree and re-applying the stash with a three-way merge, so the
//!   changes come back unstaged and a conflicting re-apply leaves the snapshot in
//!   `refs/stash`. On *paths* the same flag is `checkout_merged()`, which
//!   re-creates a conflicted file from its three index stages.
//!
//! * `git bisect skip` records `refs/bisect/skip-<oid>` and picks a replacement
//!   commit through `find_bisection()`'s distance-sorted list, `filter_skipped()`
//!   and `skip_away()`'s `get_prn`. The pick is deterministic, which is what the
//!   sequence below pins.
#![cfg(unix)]

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
    fn empty(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-carry-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
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
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@e.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@e.com")
            .env("GIT_AUTHOR_DATE", "@1700000000+0000")
            .env("GIT_COMMITTER_DATE", "@1700000000+0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn write(&self, path: &str, body: &str) {
        let full = self.work.join(path);
        if let Some(dir) = full.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(full, body).unwrap();
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.work.join(path)).unwrap()
    }

    fn rev(&self, spec: &str) -> String {
        let (code, out, err) = self.run(&["rev-parse", spec]);
        assert_eq!(code, 0, "rev-parse {spec}: {err}");
        out.trim().to_string()
    }

    fn exists(&self, path: &str) -> bool {
        self.work.join(path).exists()
    }
}

/// Twenty numbered lines, the shape a rename needs to score over 50%.
fn lines(prefix: &str) -> String {
    (1..=20)
        .map(|i| format!("{prefix} {i}\n"))
        .collect::<String>()
}

// ---------------------------------------------------------------------------
// range-diff: rename sections
// ---------------------------------------------------------------------------

/// Two versions of the same renaming commit, differing by one more edit on the
/// second side, so the rename section shows up as context in the diff-of-diffs.
fn rename_ranges(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.write("a.txt", &lines("line"));
    f.write("bin.dat", &"BIN\0".repeat(40));
    f.write("mode.txt", &lines("mode"));
    f.write("sub/deep.txt", "deep\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.git(&["branch", "v1"]);
    f.git(&["branch", "v2"]);
    for (branch, extra) in [("v1", false), ("v2", true)] {
        f.git(&["checkout", "-q", branch]);
        // A rename with an edit, plus a second edit on v2 only.
        std::fs::rename(f.work.join("a.txt"), f.work.join("b.txt")).unwrap();
        let mut body = lines("line").replace("line 3\n", "line three\n");
        if extra {
            body = body.replace("line 7\n", "line seven\n");
        }
        f.write("b.txt", &body);
        // A rename that also changes the mode.
        std::fs::rename(f.work.join("mode.txt"), f.work.join("mode2.txt")).unwrap();
        let mut modes = lines("mode").replace("mode 5\n", "five\n");
        if extra {
            modes = modes.replace("mode 9\n", "nine\n");
        }
        f.write("mode2.txt", &modes);
        {
            use std::os::unix::fs::PermissionsExt;
            let path = f.work.join("mode2.txt");
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        // A binary rename, and a directory that disappears.
        std::fs::rename(f.work.join("bin.dat"), f.work.join("bin2.dat")).unwrap();
        f.write(
            "bin2.dat",
            &format!("{}{}", "BIN\0".repeat(40), if extra { "XX" } else { "YY" }),
        );
        std::fs::remove_dir_all(f.work.join("sub")).unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "rename everything"]);
        f.git(&["checkout", "-q", "main"]);
    }
    f
}

#[test]
fn a_renamed_file_is_one_section_named_old_to_new() {
    let f = rename_ranges("rd-rename");
    let (code, out, err) = f.run(&[
        "range-diff",
        "-U100",
        "--creation-factor=999",
        "main..v1",
        "main..v2",
    ]);
    assert_eq!(code, 0, "range-diff refused: {err}");
    assert!(
        out.contains(" ## a.txt => b.txt ##"),
        "no rename section; got:\n{out}"
    );
    // The mode change rides along on the same header line.
    assert!(
        out.contains(" ## mode.txt => mode2.txt (mode change 100644 => 100755) ##"),
        "no mode-changing rename section; got:\n{out}"
    );
    // Both `Binary files … differ` labels name their own side of the rename.
    assert!(
        out.contains(" ## bin.dat => bin2.dat ##"),
        "no binary rename section; got:\n{out}"
    );
    assert!(
        out.contains("Binary files bin.dat and bin2.dat differ"),
        "binary labels do not name both sides; got:\n{out}"
    );
    // A whole directory disappeared, and git's recursive walk reports only the
    // file inside it — never a section for the tree entry itself.
    assert!(
        out.contains(" ## sub/deep.txt (deleted) ##"),
        "the file inside the dropped directory is missing; got:\n{out}"
    );
    assert!(
        !out.contains(" ## sub (deleted) ##"),
        "the directory itself became a section; got:\n{out}"
    );
}

#[test]
fn diff_renames_false_splits_the_rename_back_into_a_delete_and_an_add() {
    let f = rename_ranges("rd-norename");
    f.git(&["config", "diff.renames", "false"]);
    let (code, out, err) = f.run(&[
        "range-diff",
        "-U100",
        "--creation-factor=999",
        "main..v1",
        "main..v2",
    ]);
    assert_eq!(code, 0, "range-diff refused: {err}");
    assert!(
        out.contains(" ## a.txt (deleted) ##") && out.contains(" ## b.txt (new) ##"),
        "detection stayed on with diff.renames=false; got:\n{out}"
    );
    assert!(
        !out.contains("=>"),
        "a rename survived diff.renames=false; got:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// checkout -m: the autostash carry
// ---------------------------------------------------------------------------

/// `main` and `other`, where `other` rewrites the last line of `f.txt` and adds
/// `other.txt`.
fn switch_fixture(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.write("f.txt", &lines("line"));
    f.write("keep.txt", "keep\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.git(&["checkout", "-q", "-b", "other"]);
    f.write("f.txt", &lines("line").replace("line 20\n", "line twenty on other\n"));
    f.write("other.txt", "other\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "other"]);
    f.git(&["checkout", "-q", "main"]);
    f
}

#[test]
fn a_mergeable_local_change_is_stashed_and_merged_back_unstaged() {
    let f = switch_fixture("co-merge");
    // Edits the *first* line, which `other` does not touch.
    f.write("f.txt", &lines("line").replace("line 1\n", "line one local\n"));

    let (code, out, err) = f.run(&["checkout", "-m", "other"]);
    assert_eq!(code, 0, "-m refused a mergeable carry: {err}");
    assert!(
        out.starts_with("The following paths have local changes:\n"),
        "missing the autostash header; got: {out:?}"
    );
    assert!(err.contains("Applied autostash."), "stderr: {err:?}");
    assert!(err.contains("Switched to branch 'other'"), "stderr: {err:?}");

    // Both edits survive in the worktree.
    let body = f.read("f.txt");
    assert!(body.contains("line one local\n"), "local edit lost:\n{body}");
    assert!(
        body.contains("line twenty on other\n"),
        "the branch's own change is missing:\n{body}"
    );
    // The index is the branch's tree — the carried change comes back unstaged —
    // and a clean re-apply keeps no stash entry.
    let (_, staged, _) = f.run(&["diff", "--cached", "--name-only"]);
    assert_eq!(staged, "", "the carried change was staged");
    let (code, _, _) = f.run(&["rev-parse", "--verify", "refs/stash"]);
    assert_ne!(code, 0, "a clean re-apply left a stash entry behind");
}

#[test]
fn a_conflicting_carry_keeps_the_stash_and_marks_the_file_up() {
    let f = switch_fixture("co-conflict");
    // Edits the same line `other` rewrote.
    f.write("f.txt", &lines("line").replace("line 20\n", "line twenty local\n"));

    let (code, out, err) = f.run(&["checkout", "-m", "other"]);
    assert_eq!(code, 0, "a conflicting carry is still a successful switch: {err}");
    assert!(out.starts_with("The following paths have local changes:\n"), "{out:?}");
    assert!(
        err.contains("Your local changes are stashed, however applying them"),
        "stderr: {err:?}"
    );
    assert!(err.contains("Switched to branch 'other'"), "stderr: {err:?}");

    // `--label-ours` is the switch target as spelled, `--label-theirs` is `local`.
    let body = f.read("f.txt");
    assert!(body.contains("<<<<<<< other\n"), "ours label wrong:\n{body}");
    assert!(body.contains(">>>>>>> local\n"), "theirs label wrong:\n{body}");
    // The conflicted index survives, and the snapshot is recoverable.
    let (_, stages, _) = f.run(&["ls-files", "-u", "f.txt"]);
    assert_eq!(stages.lines().count(), 3, "expected stages 1/2/3, got:\n{stages}");
    let (_, list, _) = f.run(&["stash", "list"]);
    assert!(
        list.contains("autostash while switching to 'other'"),
        "stash list: {list:?}"
    );
}

#[test]
fn an_untracked_file_in_the_way_is_not_a_carry_and_still_refuses() {
    let f = switch_fixture("co-untracked");
    f.write("other.txt", "untracked\n");
    let (code, _, err) = f.run(&["checkout", "-m", "other"]);
    assert_eq!(code, 1, "an untracked clash must still refuse: {err}");
    assert!(
        err.contains("The following untracked working tree files would be overwritten"),
        "stderr: {err:?}"
    );
    assert_eq!(f.rev("HEAD"), f.rev("main"), "HEAD moved on a refused switch");
    let (code, _, _) = f.run(&["rev-parse", "--verify", "refs/stash"]);
    assert_ne!(code, 0, "a refused switch stashed something");
}

#[test]
fn reading_from_a_tree_refuses_the_three_way_and_stage_picking_forms() {
    let f = switch_fixture("co-treepath");
    for args in [
        vec!["checkout", "-m", "other", "--", "f.txt"],
        vec!["checkout", "--ours", "other", "--", "f.txt"],
        vec!["checkout", "-m", "other", "f.txt"],
    ] {
        let (code, _, err) = f.run(&args);
        assert_eq!(code, 128, "{args:?} was accepted: {err}");
        assert!(
            err.contains(
                "fatal: '--merge', '--ours', or '--theirs' cannot be used when checking out of a tree"
            ),
            "{args:?} stderr: {err:?}"
        );
        // Nothing was written: the file still holds the branch's version.
        assert!(f.read("f.txt").contains("line 20\n"), "the path was restored anyway");
    }
}

/// A repository whose index really holds stages 1/2/3 for `f.txt`.
fn conflicted_index(tag: &str) -> Fixture {
    let f = switch_fixture(tag);
    f.write("f.txt", &lines("line").replace("line 20\n", "line twenty on main\n"));
    f.git(&["commit", "-q", "-am", "main change"]);
    let out = f.cmd(&["merge", "other"]).output().unwrap();
    assert!(!out.status.success(), "the fixture merge did not conflict: {out:?}");
    let (_, stages, _) = f.run(&["ls-files", "-u", "f.txt"]);
    assert_eq!(
        stages.lines().count(),
        3,
        "the fixture must leave three stages, got:\n{stages}"
    );
    f
}

#[test]
fn an_unmerged_path_is_refused_without_merge_and_re_merged_with_it() {
    let f = conflicted_index("co-unmerged");
    let before = f.read("f.txt");

    // Without `-m` the path is refused and nothing is written.
    let (code, _, err) = f.run(&["checkout", "--", "f.txt"]);
    assert_eq!(code, 1, "an unmerged path was restored: {err}");
    assert!(err.contains("error: path 'f.txt' is unmerged"), "stderr: {err:?}");
    assert_eq!(f.read("f.txt"), before, "the conflicted file was overwritten");

    // With `-m` it is re-created from the three stages, under git's own labels.
    let (code, _, err) = f.run(&["checkout", "-m", "--", "f.txt"]);
    assert_eq!(code, 0, "-m could not re-merge the stages: {err}");
    let body = f.read("f.txt");
    assert!(body.contains("<<<<<<< ours\n"), "ours label wrong:\n{body}");
    assert!(body.contains(">>>>>>> theirs\n"), "theirs label wrong:\n{body}");
    assert!(!body.contains("||||||| "), "the default style grew a base section:\n{body}");

    // `--conflict=diff3` adds the base section, and the index keeps its stages
    // either way — `checkout_merged()` writes a transient entry, never the index.
    let (code, _, err) = f.run(&["checkout", "-m", "--conflict=diff3", "--", "f.txt"]);
    assert_eq!(code, 0, "diff3 was refused on the path form: {err}");
    let body = f.read("f.txt");
    assert!(body.contains("||||||| base\n"), "no diff3 base section:\n{body}");
    let (_, stages, _) = f.run(&["ls-files", "-u", "f.txt"]);
    assert_eq!(stages.lines().count(), 3, "the stages were resolved:\n{stages}");
}

#[test]
fn the_re_merged_file_is_written_with_stage_twos_mode() {
    use std::os::unix::fs::PermissionsExt;

    let f = switch_fixture("co-modes");
    // `main` makes `f.txt` executable as well as changing the line `other`
    // changed, so the conflicted index carries mode 100755 at stage 2 and
    // 100644 at stage 3.
    f.write("f.txt", &lines("line").replace("line 20\n", "line twenty on main\n"));
    let path = f.work.join("f.txt");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    f.git(&["commit", "-q", "-am", "main change"]);
    let out = f.cmd(&["merge", "other"]).output().unwrap();
    assert!(!out.status.success(), "the fixture merge did not conflict: {out:?}");
    let (_, stages, _) = f.run(&["ls-files", "-u", "f.txt"]);
    assert!(
        stages.contains("100755") && stages.contains("100644"),
        "the fixture needs two different stage modes, got:\n{stages}"
    );

    // Clear the bit in the worktree, so only the index can decide what the
    // re-merge writes: `checkout_merged()` builds its transient entry from stage
    // 2 (`if (stage == 2) mode = create_ce_mode(ce->ce_mode)`), never stage 3.
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&path, perms).unwrap();

    let (code, _, err) = f.run(&["checkout", "-m", "--", "f.txt"]);
    assert_eq!(code, 0, "{err}");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755, "the re-merged file did not take stage 2's mode");
}

// ---------------------------------------------------------------------------
// bisect skip
// ---------------------------------------------------------------------------

/// `n` commits tagged `c1`..`c<n>`, each adding one file.
fn linear(tag: &str, n: usize) -> Fixture {
    let f = Fixture::empty(tag);
    for i in 1..=n {
        f.write(&format!("f{i}.txt"), &format!("content {i}\n"));
        f.write("flag", if i <= 8 { "good\n" } else { "bad\n" });
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", &format!("c{i}")]);
        f.git(&["tag", &format!("c{i}")]);
    }
    f
}

#[test]
fn skip_records_its_ref_and_walks_gits_own_replacement_sequence() {
    let f = linear("bi-skip", 15);
    let (code, out, err) = f.run(&["bisect", "start", "c15", "c1"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Bisecting: 6 revisions left to test after this (roughly 3 steps)"));
    assert_eq!(f.rev("HEAD"), f.rev("c8"), "the first step is the midpoint");

    // Six skips in a row. The exact commit each one lands on is `skip_away()`'s
    // `get_prn` arithmetic, pinned by the unit test in `porcelain::bisect`; what
    // is pinned here is that the walk never re-offers a skipped commit, never
    // leaves the candidate range, and does not move the step line — `reaches` is
    // the *best* candidate's weight, not the picked one's, so the count stays
    // put while the pick walks away from it.
    let candidates: Vec<String> = (2..=15).map(|i| f.rev(&format!("c{i}"))).collect();
    let mut skipped = vec![f.rev("HEAD")];
    for step in 1..=6 {
        let (code, out, err) = f.run(&["bisect", "skip"]);
        assert_eq!(code, 0, "skip {step} failed: {err}");
        assert!(
            out.contains("Bisecting: 6 revisions left to test after this (roughly 3 steps)"),
            "step line changed at skip {step}: {out:?}"
        );
        let head = f.rev("HEAD");
        assert!(
            !skipped.contains(&head),
            "skip {step} re-offered an already skipped commit"
        );
        assert!(
            candidates.contains(&head),
            "skip {step} left the candidate range"
        );
        // The commit it printed is the commit it checked out.
        assert!(out.contains(&format!("[{head}]")), "step {step} stdout: {out:?}");
        skipped.push(head);
    }

    // Every skipped commit left a ref named for it — `skip-`, never the term.
    let (_, refs, _) = f.run(&["for-each-ref", "--format=%(refname)", "refs/bisect/"]);
    for id in &skipped[..skipped.len() - 1] {
        let expected = format!("refs/bisect/skip-{id}");
        assert!(refs.contains(&expected), "missing {expected} in:\n{refs}");
    }
    // …and two log lines, which `bisect log` prints back.
    let (_, log, _) = f.run(&["bisect", "log"]);
    let c8 = f.rev("c8");
    assert!(log.contains(&format!("# skip: [{c8}] c8\n")), "log:\n{log}");
    assert!(log.contains(&format!("git bisect skip {c8}\n")), "log:\n{log}");
}

#[test]
fn nothing_left_but_skips_reports_the_candidates_and_exits_two() {
    let f = linear("bi-exhaust", 4);
    f.run(&["bisect", "start", "c4", "c1"]);
    let (code, _, err) = f.run(&["bisect", "skip", "c2"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = f.run(&["bisect", "skip", "c3"]);
    assert_eq!(code, 2, "the exhausted search must exit 2: {err}");
    assert!(
        out.starts_with("There are only 'skip'ped commits left to test.\n"),
        "stdout: {out:?}"
    );
    assert!(out.contains("The first 'bad' commit could be any of:\n"), "stdout: {out:?}");
    assert!(out.trim_end().ends_with("We cannot bisect more!"), "stdout: {out:?}");
    // The bad end is part of the answer, because a skipped commit could still be
    // hiding the real first bad one.
    for want in ["c2", "c3", "c4"] {
        assert!(out.contains(&f.rev(want)), "{want} missing from:\n{out}");
    }
    // The same set is appended to the log, in the revision walk's order.
    let (_, log, _) = f.run(&["bisect", "log"]);
    assert!(log.contains("# only skipped commits left to test\n"), "log:\n{log}");
    assert!(
        log.contains(&format!("# possible first 'bad' commit: [{}] c4\n", f.rev("c4"))),
        "log:\n{log}"
    );
}

#[test]
fn a_range_operand_is_refused_before_any_state_is_written() {
    let f = linear("bi-range", 15);
    f.run(&["bisect", "start", "c15", "c1"]);
    let before = std::fs::read_to_string(f.work.join(".git/BISECT_LOG")).unwrap();

    let (code, _, err) = f.run(&["bisect", "skip", "c4..c11"]);
    assert_ne!(code, 0, "the range form must not be silently approximated");
    assert!(
        err.contains("`bisect skip <a>..<b>` is not supported"),
        "stderr: {err:?}"
    );
    // Refused up front: no ref, no log line, no move.
    let (_, refs, _) = f.run(&["for-each-ref", "--format=%(refname)", "refs/bisect/"]);
    assert!(!refs.contains("skip-"), "a refused range still wrote refs:\n{refs}");
    assert_eq!(
        std::fs::read_to_string(f.work.join(".git/BISECT_LOG")).unwrap(),
        before,
        "a refused range appended to the log"
    );
    assert_eq!(f.rev("HEAD"), f.rev("c8"), "a refused range moved HEAD");

    // The individual revisions are not refused.
    let (code, _, err) = f.run(&["bisect", "skip", "c8", "c9"]);
    assert_eq!(code, 0, "explicit revisions must still work: {err}");
    assert!(f.exists(".git/BISECT_LOG"));
}
