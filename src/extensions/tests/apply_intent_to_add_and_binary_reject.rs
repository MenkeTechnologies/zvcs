//! `git apply -N`/`--intent-to-add` and `--reject` over binary and removal patches.
//!
//! Two behaviours that are easy to get plausibly wrong and that no other test covers:
//!
//! * `-N` (`state->ita_only`) writes the worktree normally but stages *only* the paths
//!   the patch creates, as an intent-to-add entry: the empty blob, a zeroed stat, and
//!   `CE_INTENT_TO_ADD` (apply.c:4443-4460, read-cache.c:704). A deletion deliberately
//!   leaves its index entry standing (apply.c:4431), a modification stages nothing, and
//!   `--index`/`--cached` silently cancels the flag outright (apply.c:178).
//! * `--reject` has no effect on a binary patch: `apply_fragments()` hands one straight
//!   to `apply_binary()` (apply.c:3364), so there are no fragments to reject one at a
//!   time. It either rebuilds the whole file or fails the whole patch — leaving *no*
//!   `*.rej` file, since `check_patch()` marks the patch itself rejected and
//!   `write_out_results()` then skips it (apply.c:4820). The same holds for a removal
//!   patch whose hunks left contents behind (apply.c:3826).
//!
//! Every expectation below was measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `bin.dat` as committed, and the post-image `BIN_PATCH` rebuilds.
const BIN_BEFORE: &[u8] = b"\x00\x01\x02\x03BINARY-ORIGINAL\x04\x05\n";
const BIN_AFTER: &[u8] = b"\x00\x01\x02\x03BINARY-MODIFIED-LONGER\x04\x05\x06\n";
/// Contents that hash to neither end of `BIN_PATCH`, so it cannot apply.
const BIN_DIRTY: &[u8] = b"\x00\x99DIFFERENT-CONTENT\x07\n";
/// The blob id of [`BIN_DIRTY`] — the id git reports back, being the one it computes
/// from the contents it found rather than the one the patch asked for.
const BIN_DIRTY_ID: &str = "4127f1f717834983865af23e7929eda1ff8461e9";

const TEXT_BEFORE: &str = "a\nb\nc\nd\ne\nf\ng\nh\n";
const TEXT_AFTER: &str = "a\nb\nCHANGED\nd\ne\nf\ng\nh\n";

/// stock git's own `diff --binary` for `bin.dat`: forward payload then reverse.
const BIN_PATCH: &str = "\
diff --git a/bin.dat b/bin.dat
index 6476d4e03b3be6c5189d23d4ee6800fcc1b756dc..7a8ffadb0c19f6e9739365a56d24aaef01a0f921 100644
GIT binary patch
literal 30
lcmZQzWMX#m^m7b~)b;gu@pSWab<y?l_j7j*Vqs<D0svq;21ft@

literal 22
bcmZQzWMX#m^m7b~)b$VYbO*A0SXj9LFP{Wq

";

/// A creation of a binary file: the pre-image side is the null id, so `apply_binary()`
/// takes its `patch->old_name == NULL` branch and only requires an empty pre-image.
const NEWBIN_PATCH: &str = "\
diff --git a/newbin.dat b/newbin.dat
new file mode 100644
index 0000000000000000000000000000000000000000..7245361e8e189d41a8404e52cb5f4960d96f278e
GIT binary patch
literal 13
UcmezWPr=VM+{x2Vfq{_=04NUx3;+NC

literal 0
HcmV?d00001

";
const NEWBIN_CONTENT: &[u8] = b"\xff\xfe NEWBIN \x00\x01\n";

const NEWFILE_PATCH: &str = "\
diff --git a/brand.txt b/brand.txt
new file mode 100644
index 0000000000..e14074d769
--- /dev/null
+++ b/brand.txt
@@ -0,0 +1,2 @@
+newline1
+newline2
";

const MOD_PATCH: &str = "\
diff --git a/text.txt b/text.txt
index 71ac1b5791..9bc3aacc20 100644
--- a/text.txt
+++ b/text.txt
@@ -1,6 +1,6 @@
 a
 b
-c
+CHANGED
 d
 e
 f
";

const DEL_PATCH: &str = "\
diff --git a/text.txt b/text.txt
deleted file mode 100644
index 71ac1b5791..0000000000
--- a/text.txt
+++ /dev/null
@@ -1,8 +0,0 @@
-a
-b
-c
-d
-e
-f
-g
-h
";

/// A deletion whose single hunk removes only the first four of eight lines, so the
/// result still holds contents and `apply_data()` fails the patch after the hunk has
/// already been placed.
const PARTIAL_DEL_PATCH: &str = "\
diff --git a/text.txt b/text.txt
deleted file mode 100644
index 71ac1b5791204c80666ab1a4f9886b79e982739c..0000000000000000000000000000000000000000
--- a/text.txt
+++ /dev/null
@@ -1,4 +0,0 @@
-a
-b
-c
-d
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
    /// A repository holding `text.txt` and the binary `bin.dat`, both committed.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-applyita-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("text.txt", TEXT_BEFORE.as_bytes());
        f.write("bin.dat", BIN_BEFORE);
        f.git(&["add", "text.txt", "bin.dat"]);
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

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn read(&self, path: &str) -> Vec<u8> {
        std::fs::read(self.work.join(path)).unwrap()
    }

    fn exists(&self, path: &str) -> bool {
        self.work.join(path).exists()
    }

    /// Run `git apply <args> <patch>`, returning `(exit code, stderr)`. The patch is
    /// written *outside* the worktree so it cannot show up in `status --porcelain`.
    fn apply(&self, args: &[&str], body: &str) -> (i32, String) {
        let patch = self.root.join("p.patch");
        std::fs::write(&patch, body.as_bytes()).unwrap();
        let patch = patch.to_str().unwrap();
        let mut argv = vec!["apply"];
        argv.extend_from_slice(args);
        argv.push(patch);
        let out = self.cmd(&argv).output().unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn stage_lines(&self) -> Vec<String> {
        let out = self.cmd(&["ls-files", "--stage"]).output().unwrap();
        assert!(out.status.success(), "ls-files failed: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn status(&self) -> Vec<String> {
        let out = self.cmd(&["status", "--porcelain"]).output().unwrap();
        assert!(out.status.success(), "status failed: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The on-disk flag words of the index entry for `path`, read straight out of
    /// `.git/index`: the 16-bit flags, then the 16-bit extended flags that follow when
    /// the `EXTENDED` bit is set. Reading the file itself keeps the assertion honest —
    /// nothing else in this port has to be right for it to hold.
    fn index_entry_flags(&self, path: &str) -> (u16, Option<u16>) {
        let raw = std::fs::read(self.work.join(".git/index")).unwrap();
        let mut needle = path.as_bytes().to_vec();
        needle.push(0);
        let at = raw
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| panic!("no index entry for {path}"));
        // A v2 entry lays `flags` (2 bytes) immediately before the path; a v3 entry
        // inserts `extended flags` (2 more bytes) between them. Both carry the path
        // length in their low 12 bits, which is what tells the two apart here.
        let last = u16::from_be_bytes([raw[at - 2], raw[at - 1]]);
        if last & 0x4000 == 0 && (last & 0x0fff) as usize == path.len() {
            return (last, None);
        }
        let flags = u16::from_be_bytes([raw[at - 4], raw[at - 3]]);
        assert_eq!(
            (flags & 0x4000, (flags & 0x0fff) as usize),
            (0x4000, path.len()),
            "could not locate the flag words of the index entry for {path}"
        );
        (flags, Some(last))
    }

    /// The offset of the NUL-terminated `path` inside `.git/index`, which is where the
    /// entry's fixed-size prefix ends.
    fn index_path_offset(&self, path: &str) -> (Vec<u8>, usize) {
        let raw = std::fs::read(self.work.join(".git/index")).unwrap();
        let mut needle = path.as_bytes().to_vec();
        needle.push(0);
        let at = raw
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| panic!("no index entry for {path}"));
        (raw, at)
    }
}

/// The empty blob, which is what an intent-to-add entry names until the real content
/// is staged.
const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

// ---------------------------------------------------------------------------
// -N / --intent-to-add
// ---------------------------------------------------------------------------

/// A creation under `-N` lands in the worktree in full but is staged as the empty blob
/// with `CE_INTENT_TO_ADD` set, so the addition still reads as unstaged.
#[test]
fn intent_to_add_stages_a_creation_as_an_empty_placeholder() {
    let f = Fixture::new("ita-new");
    let (code, stderr) = f.apply(&["-N"], NEWFILE_PATCH);
    assert_eq!((code, stderr.as_str()), (0, ""));

    assert_eq!(f.read("brand.txt"), b"newline1\nnewline2\n");
    assert!(
        f.stage_lines()
            .contains(&format!("100644 {EMPTY_BLOB} 0\tbrand.txt")),
        "expected an empty-blob entry, got {:?}",
        f.stage_lines()
    );
    // ` A` (not `A ` or `AM`): the index says nothing is staged for this path yet.
    assert_eq!(f.status(), vec![" A brand.txt"]);

    // EXTENDED | pathlen 9, then CE_INTENT_TO_ADD (1 << 29, stored as 1 << 13).
    let (flags, extended) = f.index_entry_flags("brand.txt");
    assert_eq!((flags, extended), (0x4000 | 9, Some(0x2000)));
    // The placeholder must never look up to date, so its stat stays zeroed. Counting
    // back from the path: extended flags 2, flags 2, oid 20, size 4, gid 4, uid 4,
    // mode 4, ino 4, dev 4, mtime 8, ctime 8.
    let (raw, at) = f.index_path_offset("brand.txt");
    assert!(
        raw[at - 64..at - 40].iter().all(|b| *b == 0) && raw[at - 36..at - 24].iter().all(|b| *b == 0),
        "ctime/mtime/dev/ino/uid/gid/size must be zeroed for an intent-to-add entry"
    );
    // The mode still comes from the patch, and it sits between those two runs.
    assert_eq!(&raw[at - 40..at - 36], &0o100644u32.to_be_bytes());
}

/// `-N` only ever *adds*. A deletion's entry survives (apply.c:4431 skips the index
/// removal under `ita_only`) even though the file is gone from the worktree, and a
/// modification stages nothing at all.
#[test]
fn intent_to_add_leaves_deletions_and_modifications_unstaged() {
    let f = Fixture::new("ita-del");
    let before = f.stage_lines();

    let (code, stderr) = f.apply(&["-N"], DEL_PATCH);
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert!(!f.exists("text.txt"), "the worktree file must be removed");
    assert_eq!(f.stage_lines(), before, "the index entry must survive");
    assert_eq!(f.status(), vec![" D text.txt"]);

    let f = Fixture::new("ita-mod");
    let before = f.stage_lines();
    let (code, stderr) = f.apply(&["-N"], MOD_PATCH);
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert_eq!(f.read("text.txt"), TEXT_AFTER.as_bytes());
    assert_eq!(f.stage_lines(), before, "a modification stages nothing");
    assert_eq!(f.status(), vec![" M text.txt"]);
}

/// `check_apply_state()` drops `ita_only` when `--index` is also given (apply.c:178),
/// so the real blob is staged and the addition reads as staged — the placeholder must
/// not survive into that combination.
#[test]
fn index_cancels_intent_to_add() {
    let f = Fixture::new("ita-index");
    let (code, stderr) = f.apply(&["-N", "--index"], NEWFILE_PATCH);
    assert_eq!((code, stderr.as_str()), (0, ""));

    assert!(
        f.stage_lines()
            .contains(&"100644 e14074d7694fb45a58eb7a68ed6ab98a48991182 0\tbrand.txt".to_owned()),
        "expected the real blob, got {:?}",
        f.stage_lines()
    );
    assert_eq!(f.status(), vec!["A  brand.txt"]);
    assert_eq!(f.index_entry_flags("brand.txt"), (9, None));
}

/// `-N` reaches the `--reject` path too: a patch that creates a file is still staged
/// as intent-to-add there, and a binary creation is staged the same way.
#[test]
fn intent_to_add_applies_under_reject_and_to_binary_creations() {
    let f = Fixture::new("ita-reject");
    let (code, stderr) = f.apply(&["--reject", "-N"], NEWFILE_PATCH);
    assert_eq!(code, 0);
    // `--reject` forces verbose, so the progress lines appear without `-v`.
    assert_eq!(
        stderr,
        "Checking patch brand.txt...\nApplied patch brand.txt cleanly.\n"
    );
    assert_eq!(f.index_entry_flags("brand.txt"), (0x4000 | 9, Some(0x2000)));
    assert_eq!(f.status(), vec![" A brand.txt"]);

    let f = Fixture::new("ita-newbin");
    let (code, stderr) = f.apply(&["-N"], NEWBIN_PATCH);
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert_eq!(f.read("newbin.dat"), NEWBIN_CONTENT);
    assert!(
        f.stage_lines()
            .contains(&format!("100644 {EMPTY_BLOB} 0\tnewbin.dat")),
        "got {:?}",
        f.stage_lines()
    );
    assert_eq!(f.index_entry_flags("newbin.dat"), (0x4000 | 10, Some(0x2000)));
}

// ---------------------------------------------------------------------------
// --reject over binary and removal patches
// ---------------------------------------------------------------------------

/// A binary patch that applies is applied under `--reject` exactly as it is without it.
#[test]
fn reject_applies_a_binary_patch_that_fits() {
    let f = Fixture::new("rej-bin-ok");
    let (code, stderr) = f.apply(&["--reject"], BIN_PATCH);
    assert_eq!(code, 0);
    assert_eq!(
        stderr,
        "Checking patch bin.dat...\nApplied patch bin.dat cleanly.\n"
    );
    assert_eq!(f.read("bin.dat"), BIN_AFTER);
    assert!(!f.exists("bin.dat.rej"));

    // The same payload also creates a file from nothing.
    let f = Fixture::new("rej-bin-new");
    let (code, stderr) = f.apply(&["--reject"], NEWBIN_PATCH);
    assert_eq!(code, 0);
    assert_eq!(
        stderr,
        "Checking patch newbin.dat...\nApplied patch newbin.dat cleanly.\n"
    );
    assert_eq!(f.read("newbin.dat"), NEWBIN_CONTENT);
}

/// A binary patch that does not apply rejects the *patch*, not a hunk: no `*.rej` file
/// is written, the target is left alone, and the id reported back is the one hashed
/// from the contents actually found.
#[test]
fn reject_writes_no_rej_file_for_a_binary_patch() {
    let f = Fixture::new("rej-bin-fail");
    f.write("bin.dat", BIN_DIRTY);

    let (code, stderr) = f.apply(&["--reject"], BIN_PATCH);
    assert_eq!(code, 1);
    assert_eq!(
        stderr,
        format!(
            "Checking patch bin.dat...\n\
             error: the patch applies to 'bin.dat' ({BIN_DIRTY_ID}), which does not match the current contents.\n\
             error: bin.dat: patch does not apply\n"
        )
    );
    assert!(
        !f.exists("bin.dat.rej"),
        "a binary patch has no fragments to reject, so no *.rej is written"
    );
    assert_eq!(f.read("bin.dat"), BIN_DIRTY, "the target must be untouched");
}

/// Without `--reject` the same failure carries the same two lines — `apply_binary()`
/// fails `apply_data()`, and `check_patch()` adds its own line under it (apply.c:4158).
#[test]
fn a_failed_binary_patch_reports_the_found_id_and_the_check_patch_line() {
    let f = Fixture::new("bin-fail");
    f.write("bin.dat", BIN_DIRTY);

    let (code, stderr) = f.apply(&[], BIN_PATCH);
    assert_eq!(code, 1);
    assert_eq!(
        stderr,
        format!(
            "error: the patch applies to 'bin.dat' ({BIN_DIRTY_ID}), which does not match the current contents.\n\
             error: bin.dat: patch does not apply\n"
        )
    );
    assert_eq!(f.read("bin.dat"), BIN_DIRTY);
}

/// `--reject` applies each file independently, so a binary patch failing in a
/// multi-file input does not stop the text patch beside it from landing.
#[test]
fn reject_keeps_applying_after_a_binary_patch_fails() {
    let f = Fixture::new("rej-mixed");
    f.write("bin.dat", BIN_DIRTY);

    let (code, stderr) = f.apply(&["--reject"], &format!("{BIN_PATCH}{MOD_PATCH}"));
    assert_eq!(code, 1);
    assert_eq!(
        stderr,
        format!(
            "Checking patch bin.dat...\n\
             error: the patch applies to 'bin.dat' ({BIN_DIRTY_ID}), which does not match the current contents.\n\
             error: bin.dat: patch does not apply\n\
             Checking patch text.txt...\n\
             Applied patch text.txt cleanly.\n"
        )
    );
    assert_eq!(f.read("bin.dat"), BIN_DIRTY);
    assert_eq!(f.read("text.txt"), TEXT_AFTER.as_bytes());
}

/// `-R` on a binary patch must consume the *reverse* payload and check the two ids the
/// other way round (`reverse_patches()` swaps them, apply.c:2340). Getting this wrong is
/// silent: the forward payload rebuilds a post-image that hashes to neither end, and
/// without the id check the worktree ends up holding it at exit 0.
#[test]
fn reverse_applying_a_binary_patch_undoes_it() {
    let f = Fixture::new("bin-rev");
    f.write("bin.dat", BIN_AFTER);

    let (code, stderr) = f.apply(&["-R"], BIN_PATCH);
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert_eq!(f.read("bin.dat"), BIN_BEFORE);

    // Reversed onto the pre-image it was made from, the ids no longer line up and the
    // id reported is the one hashed from what was found.
    let f = Fixture::new("bin-rev-fail");
    let (code, stderr) = f.apply(&["-R"], BIN_PATCH);
    assert_eq!(code, 1);
    assert_eq!(
        stderr,
        "error: the patch applies to 'bin.dat' (6476d4e03b3be6c5189d23d4ee6800fcc1b756dc), \
         which does not match the current contents.\n\
         error: bin.dat: patch does not apply\n"
    );
    assert_eq!(f.read("bin.dat"), BIN_BEFORE, "the target must be untouched");
}

/// A binary deletion names the null id as its post-image, which `apply_binary()` returns
/// an empty result for outright (apply.c:3316) rather than running the payload and
/// checking the hash. The result must be *no* content, not one empty line, or the
/// removal check downstream refuses the patch.
#[test]
fn a_binary_deletion_removes_the_file() {
    const PATCH: &str = "\
diff --git a/bin.dat b/bin.dat
deleted file mode 100644
index 6476d4e03b3be6c5189d23d4ee6800fcc1b756dc..0000000000000000000000000000000000000000
GIT binary patch
literal 0
HcmV?d00001

literal 22
bcmZQzWMX#m^m7b~)b$VYbO*A0SXj9LFP{Wq

";
    for args in [&["--index"][..], &["--reject"][..], &[][..]] {
        let f = Fixture::new("bin-del");
        let (code, stderr) = f.apply(args, PATCH);
        assert_eq!(code, 0, "`apply {args:?}` failed: {stderr}");
        assert!(!f.exists("bin.dat"), "`apply {args:?}` left the file behind");
        let staged = f.stage_lines().iter().any(|l| l.ends_with("\tbin.dat"));
        assert_eq!(
            staged,
            args != ["--index"],
            "`apply {args:?}` staged the wrong thing: {:?}",
            f.stage_lines()
        );
    }
}

/// A removal patch whose hunk lands but leaves contents behind fails the whole patch in
/// `apply_data()` (apply.c:3826) — after the hunk was placed, so `--reject` must not
/// mistake it for a clean apply and must not write a `*.rej` for a hunk that fit.
#[test]
fn reject_fails_a_removal_patch_that_leaves_contents() {
    let f = Fixture::new("rej-partdel");

    let (code, stderr) = f.apply(&["--reject"], PARTIAL_DEL_PATCH);
    assert_eq!(code, 1);
    assert_eq!(
        stderr,
        "Checking patch text.txt...\n\
         error: while searching for:\n\
         a\nb\nc\nd\n\n\
         error: patch failed: text.txt:1\n\
         error: removal patch leaves file contents\n\
         error: text.txt: patch does not apply\n"
    );
    assert!(
        !f.exists("text.txt.rej"),
        "the patch is rejected as a whole, so no *.rej is written"
    );
    assert_eq!(
        f.read("text.txt"),
        TEXT_BEFORE.as_bytes(),
        "the target must be untouched"
    );
}
