//! merge-ort's informational messages, one fixture per conflict class.
//!
//! Every expectation below was measured from git 2.55.0 on the fixture the test
//! builds, not derived from the port. The classes are the ones the port used to
//! render four different ways — `git merge`, `merge-tree`, `merge-recursive` and
//! `merge-subtree` each carried their own renderer, and the two porcelain ones
//! had drifted into opposite bugs — so these pin the shared renderer's decisions
//! rather than any one caller's:
//!
//!   * which of the two operands each message names, recovered from tree
//!     membership because `gix-merge` normalizes *ours*/*theirs* independently of
//!     operand order;
//!   * when `Auto-merging` is emitted at all — merge-ort's trivial-oid shortcut
//!     skips `ll_merge()` entirely, and its symlink and gitlink arms never reach
//!     the line;
//!   * `content` vs `add/add`, which merge-ort decides from the missing ancestor
//!     stage and not from the shape of the two changes;
//!   * the order the messages come out in, which is a sort on the primary path;
//!   * that `merge-tree` still refuses a class it cannot name while `git merge`
//!     completes past it — the strict/permissive split, and the reason routing
//!     the porcelain through the complete renderer did not make it die.
//!
//! The index is asserted alongside the text wherever the two can disagree: a
//! merge that prints the right thing and writes the wrong blob is the failure
//! this file exists to catch.
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
        let root = std::env::temp_dir().join(format!("zvcs-msgclass-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        // Line endings, conflict-marker size and rerere all rewrite what a
        // conflict looks like; pin them so a developer's own config cannot.
        f.git(&["config", "core.autocrlf", "false"]);
        f.git(&["config", "core.eol", "lf"]);
        f.git(&["config", "rerere.enabled", "false"]);
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

    fn symlink(&self, target: &str, path: &str) {
        let full = self.work.join(path);
        let _ = std::fs::remove_file(&full);
        std::os::unix::fs::symlink(target, full).unwrap();
    }

    /// `git ls-files -s`, which carries both the stage number and the blob id —
    /// the two halves a merge can get wrong independently of what it printed.
    fn stages(&self) -> String {
        let (code, out, err) = self.run(&["ls-files", "-s"]);
        assert_eq!(code, 0, "ls-files -s: {err}");
        out
    }

    fn rev(&self, spec: &str) -> String {
        let (code, out, err) = self.run(&["rev-parse", spec]);
        assert_eq!(code, 0, "rev-parse {spec}: {err}");
        out.trim().to_string()
    }
}

/// Forty numbered lines: enough for `diffcore-rename` to score a rename well
/// over its 50% default even after an edit.
fn lines(prefix: &str) -> String {
    (1..=40).map(|i| format!("{prefix} {i}\n")).collect()
}

/// Replace only the six random characters `mkstemp()` puts in a
/// `git-merge-one-file` temporary name. Any other label — a branch name, a
/// revision, `HEAD` — survives, so a renderer that starts spelling those
/// differently still fails.
fn mask_mkstemp(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find(".merge_file_") {
        out.push_str(&rest[..at]);
        let after = &rest[at + ".merge_file_".len()..];
        let (suffix, tail) = after.split_at(after.len().min(6));
        if suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_alphanumeric()) {
            out.push_str(".merge_file_XXXXXX");
            rest = tail;
        } else {
            out.push_str(".merge_file_");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// `main` and `side` both carry `f.txt`; `side` deletes it and `main` edits it.
fn modify_delete_fixture(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.write("f.txt", "a\nb\nc\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.git(&["rm", "-q", "f.txt"]);
    f.git(&["commit", "-qm", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("f.txt", "a\nB\nc\n");
    f.git(&["commit", "-qam", "main"]);
    f
}

/// `side` renames `old.txt` to `new.txt`, `main` deletes it. `edit` also
/// appends a line on the renaming side, which is what turns one message into
/// two.
fn rename_delete_fixture(tag: &str, edit: bool) -> Fixture {
    let f = Fixture::empty(tag);
    f.write("old.txt", &lines("line"));
    f.git(&["add", "old.txt"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.git(&["mv", "old.txt", "new.txt"]);
    if edit {
        f.write("new.txt", &format!("{}extra\n", lines("line")));
    }
    f.git(&["commit", "-qam", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["rm", "-q", "old.txt"]);
    f.git(&["commit", "-qm", "main"]);
    f
}

/// Both operands rename `old.txt`, to different names. `edit` makes each side
/// also change a different line, so the two destinations need content-merging.
fn rename_rename_fixture(tag: &str, edit: bool) -> Fixture {
    let f = Fixture::empty(tag);
    f.write("old.txt", &lines("line"));
    f.git(&["add", "old.txt"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.git(&["mv", "old.txt", "side.txt"]);
    if edit {
        f.write("side.txt", &lines("line").replace("line 5\n", "SIDE 5\n"));
    }
    f.git(&["commit", "-qam", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["mv", "old.txt", "main.txt"]);
    if edit {
        f.write("main.txt", &lines("line").replace("line 5\n", "MAIN 5\n"));
    }
    f.git(&["commit", "-qam", "main"]);
    f
}

// ---------------------------------------------------------------------------
// The tree-conflict classes: which operand each message names
// ---------------------------------------------------------------------------

/// merge-ort.c:4404-4410. `delete_branch` and `modify_branch` are decided by
/// which operand still has the file, and the sentence repeats `modify_branch`
/// twice — with **two** spaces after the first full stop.
#[test]
fn modify_delete_names_the_deleting_and_modifying_operands() {
    let f = modify_delete_fixture("moddel");
    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "CONFLICT (modify/delete): f.txt deleted in side and modified in HEAD.  \
         Version HEAD of f.txt left in tree.\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    // The modified side is kept at stage 2 and the base at stage 1; there is no
    // stage 3, because that side deleted the path.
    assert_eq!(
        f.stages(),
        "100644 de980441c3ab03a8c07dda1ad27b8a11f39deb1e 1\tf.txt\n\
         100644 7be73ce3c1b1cdaea86e8168dfee8575175953bf 2\tf.txt\n"
    );
}

/// merge-ort.c:3206-3211 keys the message on the **new** name and prints the
/// old one inside it. A rename that did not change the blob stops there:
/// `process_entry()` suppresses the follow-up modify/delete when the content
/// equals the base (merge-ort.c:4396-4402).
#[test]
fn rename_delete_without_a_content_change_prints_one_message() {
    let f = rename_delete_fixture("rendel", false);
    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "CONFLICT (rename/delete): old.txt renamed to new.txt in side, but deleted in HEAD.\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    // Both surviving stages sit under the *new* name, and stage 1 is present:
    // the base blob is carried across the rename.
    assert_eq!(
        f.stages(),
        "100644 bab081fdb7372d4e471fcbb12b886e1a7cddcae2 1\tnew.txt\n\
         100644 bab081fdb7372d4e471fcbb12b886e1a7cddcae2 3\tnew.txt\n"
    );
}

/// The same rename with an edit on top earns the modify/delete as well, second,
/// because both messages key on `new.txt` and insertion order breaks the tie.
#[test]
fn rename_delete_with_a_content_change_adds_the_modify_delete() {
    let f = rename_delete_fixture("rendel2", true);
    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "CONFLICT (rename/delete): old.txt renamed to new.txt in side, but deleted in HEAD.\n\
         CONFLICT (modify/delete): new.txt deleted in HEAD and modified in side.  \
         Version side of new.txt left in tree.\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    assert_eq!(
        f.stages(),
        "100644 bab081fdb7372d4e471fcbb12b886e1a7cddcae2 1\tnew.txt\n\
         100644 b0aad2c02ca23559b63dcc0a2d7ca87e19e475c7 3\tnew.txt\n"
    );
}

/// merge-ort.c:3060-3066 prints the destinations **positionally**: `<d1> in
/// <branch1>` is whichever destination operand 1 carries, whatever order
/// `gix-merge` reports the two rewrites in. With identical content on both
/// sides there is no blob merge, so no `Auto-merging` line.
#[test]
fn rename_rename_is_silent_when_neither_side_changed_the_blob() {
    let f = rename_rename_fixture("ren1to2", false);
    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "CONFLICT (rename/rename): old.txt renamed to main.txt in HEAD and to side.txt in side.\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    // The source stays at stage 1 under its old name (merge-ort.c:3047-3058
    // keeps it deliberately), with one destination per side.
    assert_eq!(
        f.stages(),
        "100644 bab081fdb7372d4e471fcbb12b886e1a7cddcae2 2\tmain.txt\n\
         100644 bab081fdb7372d4e471fcbb12b886e1a7cddcae2 1\told.txt\n\
         100644 bab081fdb7372d4e471fcbb12b886e1a7cddcae2 3\tside.txt\n"
    );
}

/// With content changes the two destinations *are* merged, and merge-ort names
/// that merge after `pair->one->path` — the **source** (merge-ort.c:3011), not
/// either destination. The conflict record's own stage entries hold the merged
/// blob on both sides, so a renderer that reads them instead of the two changes
/// concludes the sides are identical and drops this line.
#[test]
fn rename_rename_auto_merges_under_the_source_path() {
    let f = rename_rename_fixture("ren1to2mod", true);
    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "Auto-merging old.txt\n\
         CONFLICT (rename/rename): old.txt renamed to main.txt in HEAD and to side.txt in side.\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    assert_eq!(
        f.stages(),
        "100644 c8157bf96b3f445688de1fce2d2a0a9aa7ea39cd 2\tmain.txt\n\
         100644 bab081fdb7372d4e471fcbb12b886e1a7cddcae2 1\told.txt\n\
         100644 c8157bf96b3f445688de1fce2d2a0a9aa7ea39cd 3\tside.txt\n"
    );
}

/// merge-ort.c:4238-4269. `rename_a`/`rename_b` are set from `S_ISREG` alone,
/// so "one" is renamed when either side is a regular file and "both" when
/// neither is — here a symlink against a gitlink.
#[test]
fn distinct_types_counts_the_renamed_sides() {
    for (tag, both, expect) in [("typemismatch", false, "one"), ("linkvssub", true, "both")] {
        let f = Fixture::empty(tag);
        f.write("seed.txt", "seed\n");
        f.git(&["add", "seed.txt"]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["checkout", "-q", "-b", "side"]);
        f.symlink("somewhere", "t");
        f.git(&["add", "t"]);
        f.git(&["commit", "-qm", "side"]);
        f.git(&["checkout", "-q", "main"]);
        if both {
            // A gitlink against a symlink: neither side is a regular file.
            f.git(&[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,0000000000000000000000000000000000000009,t",
            ]);
        } else {
            f.write("t", "regular\n");
            f.git(&["add", "t"]);
        }
        f.git(&["commit", "-qm", "main"]);

        let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
        assert_eq!(code, 1, "{tag}: {out}{err}");
        assert_eq!(
            out,
            format!(
                "CONFLICT (distinct types): t had different types on each side; \
                 renamed {expect} of them so each can be recorded somewhere.\n\
                 Automatic merge failed; fix conflicts and then commit the result.\n"
            ),
            "{tag}"
        );
    }
}

/// merge-ort.c:4170-4174. The branch named is the one holding the *file*, which
/// is the opposite of the side that turned the path into a directory, and the
/// message is keyed on the relocated path rather than the original.
#[test]
fn file_directory_names_the_side_holding_the_file() {
    let f = Fixture::empty("dirfile");
    f.write("seed.txt", "seed\n");
    f.git(&["add", "seed.txt"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("thing", "plain file\n");
    f.git(&["add", "thing"]);
    f.git(&["commit", "-qm", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("thing/inner.txt", "inside\n");
    f.git(&["add", "thing/inner.txt"]);
    f.git(&["commit", "-qm", "main"]);

    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "CONFLICT (file/directory): directory in the way of thing from side; \
         moving it to thing~side instead.\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    // The directory keeps the name and the file is the one that moved.
    assert_eq!(
        f.stages(),
        "100644 e31de1f3a235fd5e8f97207b8e43cd2aa06a6417 0\tseed.txt\n\
         100644 5be24b7e8f4ff445fb089b101bb4f0f4909d84d5 0\tthing/inner.txt\n\
         100644 c6817f7c36d32d75fff8837032f15342c7e01bae 3\tthing~side\n"
    );
}

// ---------------------------------------------------------------------------
// When `Auto-merging` is emitted at all
// ---------------------------------------------------------------------------

/// merge-ort.c:2233-2236: when either side's blob equals the base, the result is
/// picked outright and `ll_merge()` never runs — so no `Auto-merging`, even
/// though the merge still has real work to do on the mode. Here `side` only
/// flips the executable bit while `main` rewrites the content.
#[test]
fn a_mode_only_change_on_one_side_never_auto_merges() {
    let f = Fixture::empty("modeconflict");
    f.write("s.sh", "a\nb\nc\n");
    f.git(&["add", "s.sh"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    let exec = f.work.join("s.sh");
    let mut perms = std::fs::metadata(&exec).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&exec, perms).unwrap();
    f.git(&["add", "s.sh"]);
    f.git(&["commit", "-qm", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("s.sh", "a\nB\nc\n");
    f.git(&["commit", "-qam", "main"]);

    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        !out.contains("Auto-merging"),
        "the trivial-oid shortcut should have skipped ll_merge(): {out}"
    );
    // Both halves still land: `main`'s content under `side`'s mode.
    assert_eq!(
        f.stages(),
        "100755 7be73ce3c1b1cdaea86e8168dfee8575175953bf 0\ts.sh\n"
    );
}

/// `handle_content_merge()`'s `S_ISLNK` arm (merge-ort.c:2291) resolves symlinks
/// without `ll_merge()`, so a conflicting symlink gets the notice and nothing
/// else — no `Auto-merging`, and no binary warning either.
#[test]
fn a_conflicting_symlink_prints_only_the_notice() {
    let f = Fixture::empty("symlink");
    f.symlink("base_target", "link");
    f.git(&["add", "link"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.symlink("side_target", "link");
    f.git(&["commit", "-qam", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.symlink("main_target", "link");
    f.git(&["commit", "-qam", "main"]);

    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "CONFLICT (content): Merge conflict in link\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    assert_eq!(
        f.stages(),
        "120000 3b96195ae77799148198061c939071f697f22d01 1\tlink\n\
         120000 c8a603bb8728d463cbcf2eb25da2dcd73f657abe 2\tlink\n\
         120000 59c81f05f0a7c074d4504bcb414889862e7bd049 3\tlink\n"
    );
}

/// `merge_3way()` emits the binary warning from inside its `ll_merge()` return
/// check (merge-ort.c:2154-2158), i.e. **before** the `Auto-merging` line its
/// caller adds afterwards (merge-ort.c:2278). The labels are `opt->branch1`/`2`
/// verbatim, because all three of git's `pathnames[]` are equal here.
#[test]
fn the_binary_warning_precedes_auto_merging() {
    let f = Fixture::empty("binary");
    f.write("b.bin", "x\0y\0base\n");
    f.git(&["add", "b.bin"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("b.bin", "x\0y\0side\n");
    f.git(&["commit", "-qam", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("b.bin", "x\0y\0main\n");
    f.git(&["commit", "-qam", "main"]);

    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "warning: Cannot merge binary files: b.bin (HEAD vs. side)\n\
         Auto-merging b.bin\n\
         CONFLICT (content): Merge conflict in b.bin\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    assert_eq!(
        f.stages(),
        "100644 179002af4c4b017a9003ccdb58a7461ad78c52cc 1\tb.bin\n\
         100644 ce616f9aa7c1685204663377d2455b52bd7cf2b0 2\tb.bin\n\
         100644 153576e51f36702cd29fced9eb9a42c38fd596a9 3\tb.bin\n"
    );
}

// ---------------------------------------------------------------------------
// Ordering, classification, and the strict/permissive split
// ---------------------------------------------------------------------------

/// One merge carrying four classes at once. `merge_display_update_messages()`
/// sorts the primary paths with `string_list_sort()` (merge-ort.c:4837-4847), so
/// the rename/delete comes last under `new.txt` even though its text opens with
/// `old.txt`, and the pairs that share a path keep their insertion order.
#[test]
fn messages_come_out_sorted_by_primary_path() {
    let f = Fixture::empty("mixed");
    f.write("c.txt", "a\nb\nc\n");
    f.write("d.txt", "x\ny\nz\n");
    f.write("old.txt", &lines("line"));
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("c.txt", "a\nSIDE\nc\n");
    f.git(&["rm", "-q", "d.txt"]);
    f.git(&["mv", "old.txt", "new.txt"]);
    f.write("added.txt", "side add\n");
    f.git(&["add", "added.txt"]);
    f.git(&["commit", "-qam", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("c.txt", "a\nMAIN\nc\n");
    f.write("d.txt", "x\nMAIN\nz\n");
    f.git(&["rm", "-q", "old.txt"]);
    f.write("added.txt", "main add\n");
    f.git(&["add", "added.txt"]);
    f.git(&["commit", "-qam", "main"]);

    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "Auto-merging added.txt\n\
         CONFLICT (add/add): Merge conflict in added.txt\n\
         Auto-merging c.txt\n\
         CONFLICT (content): Merge conflict in c.txt\n\
         CONFLICT (modify/delete): d.txt deleted in side and modified in HEAD.  \
         Version HEAD of d.txt left in tree.\n\
         CONFLICT (rename/delete): old.txt renamed to new.txt in side, but deleted in HEAD.\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    assert_eq!(
        f.stages(),
        "100644 81532b1a85204c33becebe31e89afda76f527052 2\tadded.txt\n\
         100644 ebad96a2beca459d36f517f8af65ef1a9efe3c8e 3\tadded.txt\n\
         100644 de980441c3ab03a8c07dda1ad27b8a11f39deb1e 1\tc.txt\n\
         100644 af703352c64a2d88d4f62818fa68e6ae91241dfd 2\tc.txt\n\
         100644 f794161ca7f359f1bc311e2276a9a3d89a5bbec8 3\tc.txt\n\
         100644 04ec35a6dc0776b83fdb3d9d238007c7dea360c8 1\td.txt\n\
         100644 4e6509465b1dc987e65b11313e24c197e4a57d26 2\td.txt\n\
         100644 bab081fdb7372d4e471fcbb12b886e1a7cddcae2 1\tnew.txt\n\
         100644 bab081fdb7372d4e471fcbb12b886e1a7cddcae2 3\tnew.txt\n"
    );
}

/// `reason` is `add/add` when `ci->filemask == 6` (merge-ort.c:4355-4356), i.e.
/// when there is no ancestor stage — **not** when both changes happen to be
/// additions. One side renaming a file onto a name the other side added has one
/// `Addition` and one `Rewrite`, and git still says `add/add`.
#[test]
fn rename_onto_an_added_path_is_reported_as_add_add() {
    let f = Fixture::empty("renadd");
    f.write("old.txt", &lines("line"));
    f.git(&["add", "old.txt"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.git(&["mv", "old.txt", "new.txt"]);
    f.git(&["commit", "-qm", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("new.txt", "squatter\n");
    f.git(&["add", "new.txt"]);
    f.git(&["commit", "-qm", "main"]);

    // Both the porcelain and `merge-tree` have to agree; they disagreed here for
    // as long as they had separate renderers.
    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "Auto-merging new.txt\n\
         CONFLICT (add/add): Merge conflict in new.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );

    let f2 = Fixture::empty("renadd-tree");
    // Rebuild rather than reuse: the merge above left conflicted state behind.
    f2.write("old.txt", &lines("line"));
    f2.git(&["add", "old.txt"]);
    f2.git(&["commit", "-qm", "base"]);
    f2.git(&["checkout", "-q", "-b", "side"]);
    f2.git(&["mv", "old.txt", "new.txt"]);
    f2.git(&["commit", "-qm", "side"]);
    f2.git(&["checkout", "-q", "main"]);
    f2.write("new.txt", "squatter\n");
    f2.git(&["add", "new.txt"]);
    f2.git(&["commit", "-qm", "main"]);
    let (code, out, err) = f2.run(&["merge-tree", "main", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(
        out.ends_with(
            "\nAuto-merging new.txt\nCONFLICT (add/add): Merge conflict in new.txt\n"
        ),
        "merge-tree message block: {out}"
    );
}

/// The reason the porcelain could be routed through the complete renderer at
/// all: `merge-tree` keeps refusing a class it cannot name, while `git merge`
/// degrades and finishes. A conflicting gitlink is such a class — git's text
/// comes from `merge_submodule()` plus an advice block that is not ported.
///
/// What the merge *writes* is asserted against stock either way: the three
/// gitlink stages have to be there whatever the message said.
#[test]
fn merge_tree_refuses_a_class_that_merge_completes_past() {
    let build = |tag: &str| {
        let f = Fixture::empty(tag);
        f.write("seed.txt", "seed\n");
        f.git(&["add", "seed.txt"]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,0000000000000000000000000000000000000001,sub",
        ]);
        f.git(&["commit", "-qm", "basesub"]);
        f.git(&["checkout", "-q", "-b", "side"]);
        f.git(&[
            "update-index",
            "--cacheinfo",
            "160000,0000000000000000000000000000000000000002,sub",
        ]);
        f.git(&["commit", "-qm", "side"]);
        f.git(&["checkout", "-q", "main"]);
        f.git(&[
            "update-index",
            "--cacheinfo",
            "160000,0000000000000000000000000000000000000003,sub",
        ]);
        f.git(&["commit", "-qm", "main"]);
        f
    };

    let f = build("submod-merge");
    let head_tree = f.rev("HEAD^{tree}");
    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "merge should conflict, not die: {out}{err}");
    assert!(
        out.ends_with("Automatic merge failed; fix conflicts and then commit the result.\n"),
        "merge must reach its tail: {out}"
    );
    assert_eq!(
        f.stages(),
        "100644 e31de1f3a235fd5e8f97207b8e43cd2aa06a6417 0\tseed.txt\n\
         160000 0000000000000000000000000000000000000001 1\tsub\n\
         160000 0000000000000000000000000000000000000003 2\tsub\n\
         160000 0000000000000000000000000000000000000002 3\tsub\n"
    );
    // A conflicted merge never advances HEAD.
    assert_eq!(f.rev("HEAD^{tree}"), head_tree);

    let f2 = build("submod-tree");
    let (code, out, _) = f2.run(&["merge-tree", "main", "side"]);
    assert_ne!(code, 0, "merge-tree must not claim a clean merge");
    assert!(
        !out.contains("CONFLICT ("),
        "the strict renderer must print no conflict line it cannot spell: {out}"
    );
    // `--quiet` asks for no messages at all, which is the documented way past
    // the refusal, and it still reports the conflict through its exit code.
    let (code, out, err) = f2.run(&["merge-tree", "--quiet", "main", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(out, "");
}

/// `merge-recursive` renders through the same strict half, so the tree-conflict
/// family it used to refuse outright now comes out with git's text — modulo the
/// operand labels, which are this command's `<head>`/`<remote>` arguments rather
/// than `HEAD`.
#[test]
fn merge_recursive_renders_the_tree_conflict_family() {
    let f = rename_delete_fixture("rendel-recursive", true);
    let base = f.rev("main~1");
    let (code, out, err) = f.run(&["merge-recursive", &base, "--", "main", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "CONFLICT (rename/delete): old.txt renamed to new.txt in side, but deleted in main.\n\
         CONFLICT (modify/delete): new.txt deleted in main and modified in side.  \
         Version side of new.txt left in tree.\n"
    );
    assert_eq!(
        f.stages(),
        "100644 bab081fdb7372d4e471fcbb12b886e1a7cddcae2 1\tnew.txt\n\
         100644 b0aad2c02ca23559b63dcc0a2d7ca87e19e475c7 3\tnew.txt\n"
    );
}

/// `-s resolve` does not go through merge-ort at all: `git-merge-resolve.sh`
/// ends in `merge-index -o git-merge-one-file`, whose `git merge-file` is called
/// without `-L`, so the markers carry the run's `mkstemp()` temporary names.
/// Only those six random characters are masked here — a renderer that started
/// spelling the labels any other way would still fail.
#[test]
fn the_resolve_strategy_labels_conflicts_with_mkstemp_names() {
    let f = Fixture::empty("resolve");
    f.write("f.txt", "a\nb\nc\nd\ne\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("f.txt", "a\nb\nSIDE\nd\ne\n");
    f.git(&["commit", "-qam", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("f.txt", "a\nb\nMAIN\nd\ne\n");
    f.git(&["commit", "-qam", "main"]);

    let (code, out, err) = f.run(&["merge", "--no-edit", "-s", "resolve", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(
        out,
        "Trying really trivial in-index merge...\n\
         Nope.\n\
         Trying simple merge.\n\
         Simple merge failed, trying Automatic merge.\n\
         Auto-merging f.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    assert_eq!(
        err,
        "error: Merge requires file-level merging\n\
         ERROR: content conflict in f.txt\n\
         fatal: merge program failed\n"
    );
    let body = std::fs::read_to_string(f.work.join("f.txt")).unwrap();
    assert_eq!(
        mask_mkstemp(&body),
        "a\nb\n<<<<<<< .merge_file_XXXXXX\nMAIN\n=======\nSIDE\n>>>>>>> .merge_file_XXXXXX\nd\ne\n"
    );
    assert_eq!(
        f.stages(),
        "100644 940532533944dd159bfd11136fac2ee35872de38 1\tf.txt\n\
         100644 455c853690cd7aaa2d45085d0bacf30064c50bb8 2\tf.txt\n\
         100644 e778227e639f9be685d0252e37045f763726b8e6 3\tf.txt\n"
    );
}

// ---------------------------------------------------------------------------
// merge-octopus: the fast-forward branch is a `read-tree`, and refuses like one
// ---------------------------------------------------------------------------

/// `git-merge-octopus.sh:90` runs `git read-tree -u -m $head $SHA1 || exit`.
/// The old tree is the original `$head`, so a *second* consecutive fast-forward
/// hands read-tree an index that no longer matches it and read-tree dies (128).
/// Reading `$MRT` there instead made the whole octopus succeed at exit 0 with a
/// tree stock refuses to write.
#[test]
fn octopus_fast_forward_refuses_like_the_read_tree_it_is() {
    let f = Fixture::empty("octo-ff");
    f.write("f.txt", "base\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "b"]);
    f.write("f.txt", "base\nb\n");
    f.git(&["commit", "-qam", "b"]);
    f.git(&["checkout", "-q", "-b", "a"]);
    f.write("f.txt", "base\nb\na\n");
    f.git(&["commit", "-qam", "a"]);
    f.git(&["checkout", "-q", "main"]);

    let (head, b, a) = (f.rev("HEAD"), f.rev("b"), f.rev("a"));
    let (code, out, err) = f.run(&["merge-octopus", "--", &head, &b, &a]);
    assert_eq!(code, 128, "{out}{err}");
    assert_eq!(
        out,
        format!("Fast-forwarding to: {b}\nFast-forwarding to: {a}\n")
    );
    assert_eq!(
        err,
        "error: Entry 'f.txt' would be overwritten by merge. Cannot merge.\n"
    );
    // The refused read-tree wrote nothing, so the index is still the first
    // fast-forward's result.
    assert_eq!(
        f.stages(),
        "100644 4476ab7ee5ad94f431d174d74337a62531a343f4 0\tf.txt\n"
    );
}

/// The same guard, on the *first* head: an untracked file sitting where the head
/// wants one stops the octopus before the second head is looked at. Without the
/// guard the untracked file was silently overwritten.
#[test]
fn octopus_fast_forward_refuses_to_clobber_an_untracked_file() {
    let f = Fixture::empty("octo-untracked");
    f.write("f.txt", "base\n");
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "b"]);
    f.write("b.txt", "b\n");
    f.git(&["add", "b.txt"]);
    f.git(&["commit", "-qm", "b"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["checkout", "-q", "-b", "c"]);
    f.write("c.txt", "c\n");
    f.git(&["add", "c.txt"]);
    f.git(&["commit", "-qm", "c"]);
    f.git(&["checkout", "-q", "main"]);

    f.write("b.txt", "squat\n");
    let (head, b, c) = (f.rev("HEAD"), f.rev("b"), f.rev("c"));
    let (code, out, err) = f.run(&["merge-octopus", "--", &head, &b, &c]);
    assert_eq!(code, 128, "{out}{err}");
    assert_eq!(out, format!("Fast-forwarding to: {b}\n"));
    assert_eq!(
        err,
        "error: Untracked working tree file 'b.txt' would be overwritten by merge.\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.work.join("b.txt")).unwrap(),
        "squat\n",
        "the untracked file must survive the refusal"
    );
    assert_eq!(
        f.stages(),
        "100644 df967b96a579e45a18b8251732d16804b2e56a55 0\tf.txt\n"
    );
}

// ---------------------------------------------------------------------------
// The diff algorithm the merge runs on
// ---------------------------------------------------------------------------

/// A twelve-line shape on which Myers and Histogram anchor the same change
/// differently: Histogram produces one conflict spanning the whole middle,
/// Myers three smaller ones around a shared run. Measured from git 2.55.0.
const ALGO_BASE: &str = "1\n1\n0\n0\n1\n1\n0\n0\n1\n1\n1\n1\n";
const ALGO_OURS: &str = "1\n0\nO-3\n1\n1\n0\n0\nO*1\n1\n1\n1\n";
const ALGO_THEIRS: &str = "T*1\n0\n0\n1\n1\n0\nT*0\n1\n1\n1\n1\n";

/// What git writes for that fixture under Histogram — one conflict.
const ALGO_HISTOGRAM: &str = "<<<<<<< HEAD\n1\n0\nO-3\n1\n1\n0\n0\nO*1\n=======\n\
     T*1\n0\n0\n1\n1\n0\nT*0\n1\n>>>>>>> side\n1\n1\n1\n";
/// …and under Myers — three.
const ALGO_MYERS: &str = "<<<<<<< HEAD\n1\n=======\nT*1\n0\n>>>>>>> side\n0\nO-3\n1\n1\n0\n\
     <<<<<<< HEAD\n0\nO*1\n=======\nT*0\n1\n>>>>>>> side\n1\n1\n1\n";

fn algo_fixture(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.write("f.txt", ALGO_BASE);
    f.git(&["add", "f.txt"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["branch", "-q", "side"]);
    f.write("f.txt", ALGO_OURS);
    f.git(&["commit", "-qam", "ours"]);
    f.git(&["checkout", "-q", "side"]);
    f.write("f.txt", ALGO_THEIRS);
    f.git(&["commit", "-qam", "theirs"]);
    f.git(&["checkout", "-q", "main"]);
    f
}

/// `init_merge_options()` seeds `opt->xdl_opts = DIFF_WITH_ALG(opt,
/// HISTOGRAM_DIFF)` (merge-ort.c:5502), so a merge nobody configured is a
/// **histogram** merge. Inheriting gitoxide's configuration default made every
/// such merge a Myers merge instead — the same conflict, different bytes, at the
/// same exit code, on every merge in the repository.
#[test]
fn an_unconfigured_merge_is_a_histogram_merge() {
    let f = algo_fixture("algo-default");
    let (code, out, err) = f.run(&["merge", "--no-edit", "side"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(std::fs::read_to_string(f.work.join("f.txt")).unwrap(), ALGO_HISTOGRAM);

    // The same fixture proves the constants are not both the same thing.
    let f = algo_fixture("algo-xmyers");
    f.run(&["merge", "--no-edit", "-Xdiff-algorithm=myers", "side"]);
    assert_eq!(std::fs::read_to_string(f.work.join("f.txt")).unwrap(), ALGO_MYERS);

    let f = algo_fixture("algo-xhist");
    f.run(&["merge", "--no-edit", "-Xhistogram", "side"]);
    assert_eq!(std::fs::read_to_string(f.work.join("f.txt")).unwrap(), ALGO_HISTOGRAM);
}

/// `diff.algorithm` replaces that default only for the `ui` callers
/// (merge-ort.c:5472-5480): `git merge` reads it, `merge-recursive` — which
/// takes `init_basic_merge_options()` (builtin/merge-recursive.c:37) — does not.
/// `patience` in particular has to reach the driver rather than being folded
/// into histogram on the way.
#[test]
fn diff_algorithm_config_moves_git_merge_but_not_merge_recursive() {
    for value in ["myers", "patience"] {
        let f = algo_fixture(&format!("algo-cfg-{value}"));
        let (code, out, err) = f.run(&["-c", &format!("diff.algorithm={value}"), "merge", "--no-edit", "side"]);
        assert_eq!(code, 1, "{value}: {out}{err}");
        assert_eq!(
            std::fs::read_to_string(f.work.join("f.txt")).unwrap(),
            ALGO_MYERS,
            "diff.algorithm={value} must reach git merge"
        );
    }

    // The plumbing ignores it: same bytes configured or not, and they are the
    // histogram ones (with this command's own `<head>`/`<remote>` labels).
    let recursive_histogram = ALGO_HISTOGRAM.replace("<<<<<<< HEAD", "<<<<<<< main");
    for value in ["", "myers", "patience", "histogram"] {
        let f = algo_fixture(&format!("algo-mr-{}", if value.is_empty() { "none" } else { value }));
        let base = f.rev("main~1");
        let mut args: Vec<String> = Vec::new();
        if !value.is_empty() {
            args.push("-c".into());
            args.push(format!("diff.algorithm={value}"));
        }
        args.extend(["merge-recursive".into(), base, "--".into(), "main".into(), "side".into()]);
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, out, err) = f.run(&borrowed);
        assert_eq!(code, 1, "{value}: {out}{err}");
        assert_eq!(
            std::fs::read_to_string(f.work.join("f.txt")).unwrap(),
            recursive_histogram,
            "merge-recursive must ignore diff.algorithm={value:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// merge-recursive's dirty-state gates
// ---------------------------------------------------------------------------

/// The two gates git runs are not the same check, and neither of them is "is the
/// worktree dirty at all": `merge_start()` refuses a *staged* change anywhere,
/// and `merge_switch_to_result()`'s `checkout()` refuses **per path**. A blanket
/// `is_dirty()` both rejected merges git performs and, because it never looked
/// at untracked files, let one be silently overwritten.
#[test]
fn merge_recursive_guards_per_path_not_per_worktree() {
    let build = |tag: &str, extra_on_side: bool| {
        let f = Fixture::empty(tag);
        f.write("f.txt", "a\nb\nc\nd\ne\n");
        f.write("g.txt", "g base\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["checkout", "-q", "-b", "side"]);
        f.write("f.txt", "a\nb\nSIDE\nd\ne\n");
        if extra_on_side {
            f.write("new.txt", "n\n");
            f.git(&["add", "new.txt"]);
        }
        f.git(&["commit", "-qam", "side"]);
        f.git(&["checkout", "-q", "main"]);
        f.write("f.txt", "MAIN\nb\nc\nd\ne\n");
        f.git(&["commit", "-qam", "main"]);
        f
    };
    let recursive = |f: &Fixture| -> (i32, String, String) {
        let base = f.rev("main~1");
        f.run(&["merge-recursive", &base, "--", "main", "side"])
    };

    // A local edit outside the merge's footprint is not a reason to refuse.
    let f = build("mr-untouched", false);
    f.write("g.txt", "g dirty\n");
    let (code, out, err) = recursive(&f);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(out, "Auto-merging f.txt\n");
    assert_eq!(std::fs::read_to_string(f.work.join("g.txt")).unwrap(), "g dirty\n");

    // A local edit *inside* it is — with `unpack_trees`' porcelain block.
    let f = build("mr-touched", false);
    f.write("f.txt", "DIRTY\nb\nc\nd\ne\n");
    let (code, out, err) = recursive(&f);
    assert_eq!(code, 128, "{out}{err}");
    assert_eq!(out, "", "a refused checkout displays no merge messages");
    assert_eq!(
        err,
        "error: Your local changes to the following files would be overwritten by merge:\n\
         \tf.txt\n\
         Please commit your changes or stash them before you merge.\n\
         Aborting\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.work.join("f.txt")).unwrap(),
        "DIRTY\nb\nc\nd\ne\n",
        "the refusal must leave the local edit alone"
    );

    // An untracked file standing where the merge wants to write one.
    let f = build("mr-untracked", true);
    f.write("new.txt", "squat\n");
    let (code, out, err) = recursive(&f);
    assert_eq!(code, 128, "{out}{err}");
    assert_eq!(
        err,
        "error: The following untracked working tree files would be overwritten by merge:\n\
         \tnew.txt\n\
         Please move or remove them before you merge.\n\
         Aborting\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.work.join("new.txt")).unwrap(),
        "squat\n",
        "the untracked file must survive the refusal"
    );

    // A staged change anywhere is `merge_start()`'s gate: two spaces, no advice.
    let f = build("mr-staged", false);
    f.write("g.txt", "g staged\n");
    f.git(&["add", "g.txt"]);
    let (code, out, err) = recursive(&f);
    assert_eq!(code, 128, "{out}{err}");
    assert_eq!(out, "");
    assert_eq!(
        err,
        "error: Your local changes to the following files would be overwritten by merge:\n  g.txt\n"
    );
}
