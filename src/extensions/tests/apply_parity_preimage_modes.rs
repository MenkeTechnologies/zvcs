//! `git apply`'s mode arithmetic, which happens entirely in `check_preimage()`
//! (apply.c:3846-3922) and the block of `check_patch()` that follows it
//! (apply.c:4116-4156).
//!
//! The header's `old mode`/`new mode` lines are *defaults and assertions*, not
//! instructions, and four separate decisions come out of comparing them against
//! what is actually on disk:
//!
//! * `st_mode` — the mode the pre-image really has — fills in a missing
//!   `old mode` and, through `patch->new_mode` (apply.c:3913), a missing
//!   `new mode`. That last assignment is the only reason a content-only patch
//!   leaves an executable file executable.
//! * A *type* disagreement between `st_mode` and `old_mode` is fatal for that
//!   patch: `error(_("%s: wrong type"))` (apply.c:3908).
//! * A *permission* disagreement is only reported:
//!   `warning(_("%s has type %o, expected %o"))` (apply.c:3910), and the patch
//!   still applies.
//! * A type disagreement between the patch's own two modes is refused with
//!   `new mode (%o) of %s does not match old mode (%o)` — and a second wording
//!   that names both paths when they differ (apply.c:4128-4139).
//!
//! Two neighbouring refusals are pinned here for the same reason: they are the
//! other things `check_preimage()`/`check_patch()` decide that nothing else in the
//! suite reaches. `previous_patch()` setting `*gone` yields
//! `path %s has been renamed/deleted` (apply.c:3860) rather than the plain
//! "No such file" a worktree lookup would give, and `path_is_beyond_symlink()`
//! (apply.c:4154) stops a result being deposited through a symlinked directory.
//!
//! Every expectation below was measured against stock git 2.55.0
//! (`/usr/local/bin/git`, not the port).
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A same-path type change: regular file to symlink, in one patch. git itself
/// never emits this shape — `run_diff()` splits a type change into a deletion and
/// a creation — so `check_patch()` refuses it outright.
const TYPE_CHANGE: &str = "\
diff --git a/reg.txt b/reg.txt
old mode 100644
new mode 120000
--- a/reg.txt
+++ b/reg.txt
@@ -1,2 +1 @@
-p
-q
+real
\\ No newline at end of file
";

/// The same type change carried by a rename, which reaches the two-path wording.
const TYPE_CHANGE_RENAME: &str = "\
diff --git a/reg.txt b/dst.lnk
old mode 100644
new mode 120000
similarity index 50%
rename from reg.txt
rename to dst.lnk
--- a/reg.txt
+++ b/dst.lnk
@@ -1,2 +1 @@
-p
-q
+real
\\ No newline at end of file
";

/// A patch whose `old mode` disagrees with the file's permissions but not its
/// type. `x.sh` is committed 0755; the patch claims it was 0644.
const MODE_WARNING: &str = "\
diff --git a/x.sh b/x.sh
old mode 100644
new mode 100755
--- a/x.sh
+++ b/x.sh
@@ -1,3 +1,3 @@
 a
-b
+B
 c
";

/// The same content change with no mode lines at all, against the executable
/// `x.sh`. `patch->new_mode` can only come from `st_mode`.
const CONTENT_ONLY_EXEC: &str = "\
diff --git a/x.sh b/x.sh
--- a/x.sh
+++ b/x.sh
@@ -1,3 +1,3 @@
 a
-b
+B
 c
";

/// A path reached through `lk`, a committed symlink to `real/`.
const BEYOND_SYMLINK: &str = "\
diff --git a/lk/x.txt b/lk/x.txt
--- a/lk/x.txt
+++ b/lk/x.txt
@@ -1,3 +1,3 @@
 a
-b
+B
 c
";

/// A creation whose *leading directory* this same run turns into a symlink, which
/// is the `kept_symlinks` half of `prepare_symlink_changes()` (apply.c:3983) —
/// nothing is on disk for an `lstat()` to catch.
const SYMLINK_THEN_THROUGH_IT: &str = "\
diff --git a/pit b/pit
new file mode 120000
index 0000000..3e2f18b
--- /dev/null
+++ b/pit
@@ -0,0 +1 @@
+real
\\ No newline at end of file
diff --git a/pit/caught b/pit/caught
new file mode 100644
index 0000000..d95f3ad
--- /dev/null
+++ b/pit/caught
@@ -0,0 +1 @@
+content
";

/// A deletion of `y.txt` followed, in the same input, by a modification of it.
const DELETE_THEN_MODIFY: &str = "\
diff --git a/y.txt b/y.txt
deleted file mode 100644
--- a/y.txt
+++ /dev/null
@@ -1,3 +0,0 @@
-a
-b
-c
diff --git a/y.txt b/y.txt
--- a/y.txt
+++ b/y.txt
@@ -1,3 +1,3 @@
 a
-b
+B
 c
";

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
    /// `reg.txt` 0644, `x.sh` 0755, `y.txt` 0644, `real/x.txt` behind the committed
    /// symlink `lk -> real`.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-applymodes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("real")).unwrap();
        let f = Fixture { root, work };
        f.write("reg.txt", "p\nq\n");
        f.write("x.sh", "a\nb\nc\n");
        f.write("y.txt", "a\nb\nc\n");
        f.write("real/x.txt", "a\nb\nc\n");
        f.chmod("x.sh", 0o755);
        std::os::unix::fs::symlink("real", f.work.join("lk")).unwrap();
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "base"]);
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

    fn write(&self, path: &str, body: &str) {
        std::fs::write(self.work.join(path), body.as_bytes()).unwrap();
    }

    fn read(&self, path: &str) -> String {
        String::from_utf8(std::fs::read(self.work.join(path)).unwrap()).unwrap()
    }

    fn chmod(&self, path: &str, mode: u32) {
        std::fs::set_permissions(
            self.work.join(path),
            std::os::unix::fs::PermissionsExt::from_mode(mode),
        )
        .unwrap();
    }

    fn mode(&self, path: &str) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(self.work.join(path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    fn is_symlink(&self, path: &str) -> bool {
        std::fs::symlink_metadata(self.work.join(path))
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    }

    /// `git apply <args> <patch>`, with the patch kept outside the worktree.
    fn apply(&self, args: &[&str], body: &str) -> (i32, String, String) {
        let patch = self.root.join("p.patch");
        std::fs::write(&patch, body.as_bytes()).unwrap();
        let patch = patch.to_str().unwrap();
        let mut argv = vec!["apply"];
        argv.extend_from_slice(args);
        argv.push(patch);
        let out = self.cmd(&argv).output().unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// apply.c:4128-4133. Stock, measured:
///   error: new mode (120000) of reg.txt does not match old mode (100644)
/// at exit 1, with `reg.txt` still the two-line regular file. Applying the patch
/// anyway replaces a tracked file with a symlink, which is the shape of a
/// type-change smuggling bug; silently skipping the mode lines loses the refusal.
///
/// `--check` reaches the same refusal, and `--stat` does not reach it at all
/// (apply.c:4962's `state->check || state->apply` is false for a report mode), so
/// this also pins that the gate lives in the check pass and nowhere earlier.
#[test]
fn a_same_path_type_change_is_refused_by_the_two_header_modes() {
    let f = Fixture::new("typechange");

    let (code, _, err) = f.apply(&[], TYPE_CHANGE);
    assert_eq!(code, 1, "{err:?}");
    assert_eq!(
        err.trim_end(),
        "error: new mode (120000) of reg.txt does not match old mode (100644)"
    );
    assert_eq!(f.read("reg.txt"), "p\nq\n", "nothing was written");
    assert!(!f.is_symlink("reg.txt"), "the file did not become a link");

    let (code, _, err) = f.apply(&["--check"], TYPE_CHANGE);
    assert_eq!(code, 1, "{err:?}");
    assert_eq!(
        err.trim_end(),
        "error: new mode (120000) of reg.txt does not match old mode (100644)"
    );

    // A report mode never runs the check, so it prints the stat and exits 0.
    let (code, out, err) = f.apply(&["--stat"], TYPE_CHANGE);
    assert_eq!(code, 0, "{err:?}");
    assert!(out.contains("reg.txt"), "{out:?}");
    assert!(err.is_empty(), "{err:?}");
}

/// apply.c:4134-4139 — the other wording, reached only when `old_name` and
/// `new_name` differ. Stock, measured:
///   error: new mode (120000) of dst.lnk does not match old mode (100644) of reg.txt
/// Collapsing the two arms into one message drops the source path exactly where a
/// reader needs it, since neither path is the one they typed.
#[test]
fn a_renaming_type_change_names_both_paths_in_the_refusal() {
    let f = Fixture::new("renametype");
    let (code, _, err) = f.apply(&[], TYPE_CHANGE_RENAME);
    assert_eq!(code, 1, "{err:?}");
    assert_eq!(
        err.trim_end(),
        "error: new mode (120000) of dst.lnk does not match old mode (100644) of reg.txt"
    );
    assert_eq!(f.read("reg.txt"), "p\nq\n", "the source is untouched");
    assert!(
        !f.work.join("dst.lnk").exists(),
        "the destination was not created"
    );
}

/// apply.c:3904-3912, the *reverse* direction of the same comparison: here it is
/// `st_mode` that disagrees with the patch, not the patch with itself. Reversing
/// [`TYPE_CHANGE`] makes the patch expect a symlink where a regular file sits.
/// Stock, measured: `error: reg.txt: wrong type`, exit 1.
///
/// Without the `S_IFMT` test in `check_preimage()` this reaches the hunk placer
/// instead and reports a content failure (`patch failed: reg.txt:1`), which names
/// the wrong problem.
#[test]
fn a_preimage_of_the_wrong_type_is_refused_before_any_hunk_runs() {
    let f = Fixture::new("wrongtype");
    let (code, _, err) = f.apply(&["-R"], TYPE_CHANGE);
    assert_eq!(code, 1, "{err:?}");
    assert_eq!(err.trim_end(), "error: reg.txt: wrong type");
    assert!(
        !err.contains("patch failed"),
        "the type is what is wrong, not the content: {err:?}"
    );
    assert_eq!(f.read("reg.txt"), "p\nq\n");
}

/// apply.c:3910. A *permission* disagreement is not fatal: stock warns and applies.
/// Measured: `warning: x.sh has type 100755, expected 100644`, exit 0, file content
/// updated. Treating it as an error refuses a patch git accepts; dropping it
/// entirely hides the one signal that the patch was made against a different tree.
#[test]
fn a_permission_disagreement_is_warned_about_and_then_applied() {
    let f = Fixture::new("modewarn");
    let (code, _, err) = f.apply(&[], MODE_WARNING);
    assert_eq!(code, 0, "{err:?}");
    assert_eq!(
        err.trim_end(),
        "warning: x.sh has type 100755, expected 100644"
    );
    assert_eq!(f.read("x.sh"), "a\nB\nc\n", "the patch still applied");
}

/// apply.c:3913-3914: `if (!patch->new_mode && !patch->is_delete) patch->new_mode
/// = st_mode;`. A content-only patch carries no mode line at all, so the mode the
/// result is written with can only come from the pre-image. Stock, measured: `x.sh`
/// stays 0755 and the run says nothing.
///
/// Defaulting to 0644 instead silently strips the executable bit off every script a
/// patch touches — a corruption with no diagnostic anywhere.
#[test]
fn a_content_only_patch_keeps_the_preimage_executable_bit() {
    let f = Fixture::new("execbit");
    assert_eq!(f.mode("x.sh"), 0o755, "fixture precondition");

    let (code, _, err) = f.apply(&[], CONTENT_ONLY_EXEC);
    assert_eq!(code, 0, "{err:?}");
    assert!(err.is_empty(), "{err:?}");
    assert_eq!(f.read("x.sh"), "a\nB\nc\n");
    assert_eq!(f.mode("x.sh"), 0o755, "the executable bit survived");

    // Under `--index` the same default reaches the index entry too.
    f.git(&["checkout", "-q", "--", "x.sh"]);
    f.chmod("x.sh", 0o755);
    let (code, _, err) = f.apply(&["--index"], CONTENT_ONLY_EXEC);
    assert_eq!(code, 0, "{err:?}");
    let out = f.cmd(&["ls-files", "-s", "x.sh"]).output().unwrap();
    let staged = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(staged.starts_with("100755 "), "{staged:?}");
}

/// apply.c:4154-4156 with the `lstat()` arm of `path_is_beyond_symlink_1()`
/// (apply.c:4015-4017). Stock, measured:
///   error: affected file 'lk/x.txt' is beyond a symbolic link
/// at exit 1, with `real/x.txt` untouched. Without the gate the write follows the
/// link and edits a file the patch never named.
///
/// The `--index` arm is pinned alongside it because it refuses *earlier*, on the
/// index lookup (`lk/x.txt: does not exist in index`) — the same patch, a different
/// diagnostic, and a regression in either one is invisible from the other.
#[test]
fn a_result_is_not_deposited_through_a_symlinked_directory() {
    let f = Fixture::new("beyond");
    let (code, _, err) = f.apply(&[], BEYOND_SYMLINK);
    assert_eq!(code, 1, "{err:?}");
    assert_eq!(
        err.trim_end(),
        "error: affected file 'lk/x.txt' is beyond a symbolic link"
    );
    assert_eq!(f.read("real/x.txt"), "a\nb\nc\n", "the target is untouched");

    let (code, _, err) = f.apply(&["--index"], BEYOND_SYMLINK);
    assert_eq!(code, 1, "{err:?}");
    assert_eq!(err.trim_end(), "error: lk/x.txt: does not exist in index");
    assert_eq!(f.read("real/x.txt"), "a\nb\nc\n");
}

/// The `kept_symlinks` half of the same gate (apply.c:3983, :3997). The symlink
/// `pit` does not exist yet — this very patch creates it — so only
/// `prepare_symlink_changes()`, run over the whole list before the first check,
/// can see it coming. Stock, measured:
///   error: affected file 'pit/caught' is beyond a symbolic link
/// at exit 1, and because `check_patch_list()` failing means `write_out_results()`
/// never runs, `pit` itself is not created either.
#[test]
fn a_symlink_this_run_creates_blocks_a_later_path_through_it() {
    let f = Fixture::new("keptlink");
    let (code, _, err) = f.apply(&[], SYMLINK_THEN_THROUGH_IT);
    assert_eq!(code, 1, "{err:?}");
    assert_eq!(
        err.trim_end(),
        "error: affected file 'pit/caught' is beyond a symbolic link"
    );
    assert!(
        !f.work.join("pit").exists(),
        "a failed check writes nothing at all"
    );
    assert!(
        !f.work.join("real/caught").exists(),
        "and nothing landed behind the link"
    );
}

/// apply.c:3857-3860. The second patch's pre-image was deleted by the first patch
/// in the same input, so `previous_patch()` reports it `*gone` rather than letting
/// the worktree answer. Stock, measured: `error: path y.txt has been
/// renamed/deleted`, exit 1, and `y.txt` still on disk because the run rolls back.
///
/// Reading the worktree instead produces `y.txt: No such file or directory` only
/// once the deletion has been written — and here it has not been, so a port that
/// skips the fn-table lookup either succeeds wrongly or names the wrong cause.
#[test]
fn a_path_an_earlier_patch_deleted_is_reported_as_renamed_or_deleted() {
    let f = Fixture::new("gone");
    let (code, _, err) = f.apply(&[], DELETE_THEN_MODIFY);
    assert_eq!(code, 1, "{err:?}");
    assert_eq!(err.trim_end(), "error: path y.txt has been renamed/deleted");
    assert_eq!(
        f.read("y.txt"),
        "a\nb\nc\n",
        "the whole run rolled back, deletion included"
    );
}
