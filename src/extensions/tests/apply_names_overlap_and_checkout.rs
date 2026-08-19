//! Four `git apply` behaviours that only show up once a patch stops being the
//! textbook shape, plus the `mailinfo` charset floor underneath `git am`.
//!
//! Each one was a live divergence, and each fails in a way no other suite here
//! notices — three of the four end at exit 0 with the wrong bytes on disk:
//!
//! * **Header names end at any `isspace()` but a plain space.** `find_name_common()`
//!   (apply.c:666-678) walks under `TERM_TAB`, and `name_terminate()` (apply.c:437)
//!   exempts only `' '` — so a tab, a newline *or a carriage return* ends the name.
//!   Miss the `\r` and `git am --keep-cr` of a CRLF mailbox looks for `f.txt\r`,
//!   which is in neither the index nor the working tree.
//! * **`squash_slash()` (apply.c:448) runs on the `---`/`+++` names and not on the
//!   `diff --git` line.** `git_header_name()` returns `xmemdupz()`/`strbuf_detach()`
//!   with no squash (apply.c:1229, :1325), and its result only ever becomes
//!   `patch->def_name`. So `--- a/sub//g.txt` patches `sub/g.txt`, while a pure mode
//!   change on `a/sub//x b/sub//x` keeps the doubled slash and is refused.
//! * **`LINE_PATCHED` (apply.c:2650, :2969).** A fragment marks the lines it wrote,
//!   and a later fragment of the same patch may not match them. Without it a patch
//!   whose second hunk targets the first hunk's own output applies at exit 0 where
//!   git refuses — and `--allow-overlap`, whose entire job is to stop the marking,
//!   becomes a flag that does nothing.
//! * **`checkout_target()` (apply.c:3485).** Under `--index`, a pre-image path whose
//!   file is *missing* is checked out of the index rather than refused, during the
//!   check pass — so `--check --index` recreates it, mode and all.
//! * **`create_one_file()`'s `EEXIST` recovery (apply.c:4613-4632).** Two patches in
//!   one input against the same path both reach `create_file()` in phase 1; the
//!   second must write `<path>~<pid>` and rename over the target.
//!
//! Every expectation below was measured against stock git 2.55.0
//! (`/opt/homebrew/bin/git`), not derived from the port.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A CRLF patch: every line, header and body alike, ends `\r\n`. This is what
/// `git mailsplit --keep-cr` leaves in `.git/rebase-apply/patch`.
const CRLF: &str = "diff --git a/f.txt b/f.txt\r
index 0000000..1111111 100644\r
--- a/f.txt\r
+++ b/f.txt\r
@@ -1,2 +1,2 @@\r
-one\r
+two\r
 two\r
";

/// Same patch with LF endings, to prove the CRLF case is about the endings and
/// nothing else.
const LF: &str = "diff --git a/f.txt b/f.txt
index 0000000..1111111 100644
--- a/f.txt
+++ b/f.txt
@@ -1,2 +1,2 @@
-one
+ONE
 two
";

/// Doubled slashes on every name line. `squash_slash()` collapses them, so this
/// patches `sub/g.txt`.
const DOUBLE_SLASH: &str = "\
diff --git a/sub//g.txt b/sub//g.txt
index 0000000..1111111 100644
--- a/sub//g.txt
+++ b/sub//g.txt
@@ -1 +1 @@
-x
+X
";

/// A doubled slash with no `---`/`+++` pair at all, so the name can only come from
/// `git_header_name()` — which does not squash.
const DOUBLE_SLASH_MODE: &str = "\
diff --git a/sub//g.txt b/sub//g.txt
old mode 100644
new mode 100755
";

/// Two hunks where the second can only match what the first just wrote.
const OVERLAP: &str = "\
diff --git a/o.txt b/o.txt
--- a/o.txt
+++ b/o.txt
@@ -1,3 +1,3 @@
 a
-b
+B
 c
@@ -1,3 +1,3 @@
 a
-B
+BB
 c
";

/// Two whole patches, in one input, against the same path: the second's pre-image
/// is the first's post-image.
const CHAINED: &str = "\
diff --git a/f.txt b/f.txt
index 0000000..1111111 100644
--- a/f.txt
+++ b/f.txt
@@ -1,2 +1,2 @@
-one
+GOOD
 two
diff --git a/f.txt b/f.txt
index 1111111..2222222 100644
--- a/f.txt
+++ b/f.txt
@@ -1,2 +1,2 @@
-GOOD
+GOOD2
 two
";

/// A plain modification of `sub/g.txt`, used with the file deleted from the tree.
const TOUCH_SUB: &str = "\
diff --git a/sub/g.txt b/sub/g.txt
index 0000000..1111111 100755
--- a/sub/g.txt
+++ b/sub/g.txt
@@ -1 +1 @@
-x
+X
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-applynames-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("f.txt", "one\ntwo\n");
        f.write("o.txt", "a\nb\nc\n");
        f.write("sub/g.txt", "x\n");
        // `sub/g.txt` is executable so `checkout_target()` has a mode to preserve.
        std::fs::set_permissions(
            f.work.join("sub/g.txt"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        f.git(&["add", "f.txt", "o.txt", "sub/g.txt"]);
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
            .env("LC_ALL", "C")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
            .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn write(&self, path: &str, body: &str) {
        std::fs::write(self.work.join(path), body.as_bytes()).unwrap();
    }

    fn write_bytes(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn read(&self, path: &str) -> String {
        String::from_utf8(std::fs::read(self.work.join(path)).unwrap()).unwrap()
    }

    /// `(exit code, stdout, stderr)` of `git <args>`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// `git <args>` with stdout captured as raw bytes, for the cases where the
    /// point *is* that git does not touch them.
    fn run_raw(&self, args: &[&str]) -> (i32, Vec<u8>) {
        let out = self.cmd(args).output().unwrap();
        (out.status.code().unwrap_or(-1), out.stdout)
    }

    fn mode(&self, path: &str) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(self.work.join(path)).unwrap().permissions().mode() & 0o777
    }
}

/// The `\r` arm of `name_terminate()`. A CRLF patch names `f.txt`, not `f.txt\r`,
/// so it reaches the file and fails on the *content* — which is what stock reports.
///
/// Measured from stock: `error: patch failed: f.txt:1` then
/// `error: f.txt: patch does not apply`, exit 1, working tree untouched. Reading the
/// name with the `\r` still on gives `f.txt\r: does not exist in index` instead —
/// the same exit code with a diagnostic that sends the reader after the wrong bug.
#[test]
fn a_crlf_patch_names_the_file_without_its_carriage_return() {
    let f = Fixture::new("crlf");
    f.write("p.diff", CRLF);
    let (code, out, err) = f.run(&["apply", "--index", "p.diff"]);
    assert_eq!(code, 1, "stdout={out:?} stderr={err:?}");
    assert!(
        err.contains("error: patch failed: f.txt:1"),
        "the name resolved and the *content* was what failed: {err:?}"
    );
    assert!(
        !err.contains("does not exist in index"),
        "a `\\r` left on the name turns a content failure into a lookup failure: {err:?}"
    );
    assert_eq!(f.read("f.txt"), "one\ntwo\n", "nothing was written");
}

/// The same patch's whitespace check. `check_old_for_crlf()` (apply.c:1716) sets
/// `WS_CR_AT_EOL` from the `-one\r\n` line, so the `+two\r\n` line that follows it is
/// *not* a trailing-whitespace error. Stock prints no whitespace diagnostic at all
/// here; flagging it is how a per-run (rather than per-patch, order-dependent)
/// `ws_rule` shows up.
#[test]
fn a_crlf_preimage_line_stops_the_added_line_being_a_whitespace_error() {
    let f = Fixture::new("crlfws");
    f.write("p.diff", CRLF);
    let (_, _, err) = f.run(&["apply", "p.diff"]);
    assert!(
        !err.contains("trailing whitespace"),
        "the removed line's CRLF relaxes the rule for the added line: {err:?}"
    );
    // The LF form of the same patch has no CRLF anywhere, so nothing relaxes and
    // nothing is flagged either — this pins that the arm above is about the `\r`.
    f.write("q.diff", LF);
    let (code, _, err) = f.run(&["apply", "q.diff"]);
    assert_eq!(code, 0, "{err:?}");
    assert_eq!(f.read("f.txt"), "ONE\ntwo\n");
}

/// `squash_slash()` on the `---`/`+++` names, and its *absence* on `diff --git`.
///
/// Measured from stock: the first applies at exit 0 and writes `sub/g.txt`; the
/// second is refused with `error: invalid path 'sub//g.txt'` at exit 128, because
/// `git_header_name()` never squashes and `verify_path()` then rejects the empty
/// component. Squashing in both places loses the second refusal; squashing in
/// neither loses the first apply.
#[test]
fn a_doubled_slash_is_squashed_on_the_name_lines_only() {
    let f = Fixture::new("slash");
    f.write("p.diff", DOUBLE_SLASH);
    let (code, out, err) = f.run(&["apply", "p.diff"]);
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    assert_eq!(f.read("sub/g.txt"), "X\n");

    f.git(&["checkout", "-q", "--", "sub/g.txt"]);
    f.write("m.diff", DOUBLE_SLASH_MODE);
    let (code, _, err) = f.run(&["apply", "m.diff"]);
    assert_eq!(code, 128, "{err:?}");
    assert_eq!(err.trim_end(), "error: invalid path 'sub//g.txt'");

    // `--stat` reports the squashed name, which is the other half of the same
    // rule: the report reads `patch->new_name`, not the header line.
    f.write("p.diff", DOUBLE_SLASH);
    let (code, out, _) = f.run(&["apply", "--stat", "p.diff"]);
    assert_eq!(code, 0);
    assert!(out.contains(" sub/g.txt "), "{out:?}");
    assert!(!out.contains("sub//g.txt"), "{out:?}");
}

/// `LINE_PATCHED`. The second hunk's pre-image `a / B / c` exists only because the
/// first hunk wrote it, so git refuses at exit 1 with the tree untouched, and
/// `--allow-overlap` is what lets it through.
///
/// Both halves matter: without the flag the wrong outcome is *silent success* with
/// `BB` on disk, and with the flag a naive "hunks are placed sequentially" reading
/// gives the right answer for the wrong reason.
#[test]
fn a_hunk_may_not_match_what_an_earlier_hunk_of_the_same_patch_wrote() {
    let f = Fixture::new("overlap");
    f.write("p.diff", OVERLAP);

    let (code, out, err) = f.run(&["apply", "p.diff"]);
    assert_eq!(code, 1, "stdout={out:?} stderr={err:?}");
    assert!(err.contains("error: patch failed: o.txt:1"), "{err:?}");
    assert!(err.contains("error: o.txt: patch does not apply"), "{err:?}");
    assert_eq!(f.read("o.txt"), "a\nb\nc\n", "nothing was written");

    let (code, _, err) = f.run(&["apply", "--allow-overlap", "p.diff"]);
    assert_eq!(code, 0, "{err:?}");
    assert_eq!(f.read("o.txt"), "a\nBB\nc\n");

    // `--no-allow-overlap` is the negation of an `OPT_BOOL`, so it puts the marking
    // back rather than being ignored.
    f.write("o.txt", "a\nb\nc\n");
    let (code, _, _) = f.run(&["apply", "--allow-overlap", "--no-allow-overlap", "p.diff"]);
    assert_eq!(code, 1);
    assert_eq!(f.read("o.txt"), "a\nb\nc\n");
}

/// `create_one_file()`'s `EEXIST` arm. Two patches for one path in a single input:
/// phase 1 calls `create_file()` twice, and the second create finds the file the
/// first just wrote. Stock renames a temporary over it and exits 0 with the second
/// patch's result. Opening `O_CREAT|O_EXCL` and giving up leaves `GOOD` on disk and
/// the run dead in the middle of its write phase.
#[test]
fn two_patches_for_one_path_in_one_input_both_land() {
    let f = Fixture::new("chain");
    f.write("p.diff", CHAINED);
    let (code, out, err) = f.run(&["apply", "p.diff"]);
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    assert_eq!(f.read("f.txt"), "GOOD2\ntwo\n");
    // No `~<pid>` scratch file may be left behind.
    let leftovers: Vec<String> = std::fs::read_dir(&f.work)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains('~'))
        .collect();
    assert!(leftovers.is_empty(), "scratch files left: {leftovers:?}");

    // The same input staged: the index must end on the second patch's blob too.
    f.git(&["checkout", "-q", "--", "f.txt"]);
    let (code, _, err) = f.run(&["apply", "--index", "p.diff"]);
    assert_eq!(code, 0, "{err:?}");
    let (_, out, _) = f.run(&["ls-files", "--stage", "f.txt"]);
    let staged = out.split_whitespace().nth(1).unwrap_or_default().to_string();
    let (_, shown, _) = f.run(&["cat-file", "blob", &staged]);
    assert_eq!(shown, "GOOD2\ntwo\n", "the index holds the second result");
}

/// `checkout_target()`. `--index` on a path whose file was deleted checks the blob
/// back out instead of refusing, and does it in the *check* pass — so `--check`
/// recreates the file and its leading directory without applying anything.
///
/// Measured from stock: `--check --index` exits 0, recreates `sub/g.txt` with mode
/// 755 and content `x`, and leaves it unpatched. Refusing with `does not match
/// index` is the shape a `verify_index_match()` that cannot tell "absent" from
/// "different" produces.
#[test]
fn index_mode_checks_a_missing_preimage_out_of_the_index() {
    let f = Fixture::new("checkout");
    f.write("p.diff", TOUCH_SUB);

    std::fs::remove_dir_all(f.work.join("sub")).unwrap();
    let (code, out, err) = f.run(&["apply", "--check", "--index", "p.diff"]);
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    assert_eq!(f.read("sub/g.txt"), "x\n", "checked out, not patched");
    assert_eq!(f.mode("sub/g.txt"), 0o755, "the index entry's mode came with it");

    std::fs::remove_dir_all(f.work.join("sub")).unwrap();
    let (code, _, err) = f.run(&["apply", "--index", "p.diff"]);
    assert_eq!(code, 0, "{err:?}");
    assert_eq!(f.read("sub/g.txt"), "X\n");
    assert_eq!(f.mode("sub/g.txt"), 0o755);

    // A file that is present but *different* is still a refusal — the recovery is
    // for absence only.
    f.git(&["checkout", "-q", "--", "sub/g.txt"]);
    f.write("sub/g.txt", "tampered\n");
    let (code, _, err) = f.run(&["apply", "--index", "p.diff"]);
    assert_eq!(code, 1);
    assert!(err.contains("sub/g.txt: does not match index"), "{err:?}");
}

/// `git am` of a mail whose body is in a charset that is neither UTF-8 nor
/// US-ASCII. `parse_mail()` sets `mi.metainfo_charset = get_commit_output_encoding()`
/// under the default `-u` (builtin/am.c:1220), so the body reaches the commit
/// message re-coded.
///
/// Measured from stock for a `charset=KOI8-R` body holding the single byte `0xE9`
/// (KOI8-R for U+0418): the commit message reads `body \xd0\x98 text`.
///
/// `--no-utf8` is deliberately *not* the control here. It leaves the raw `0xE9` in
/// `state->msg`, and `commit_tree_extended()` then runs `ensure_utf8()` over the
/// whole object (commit.c:1770), rewriting it to `\xc3\xa9` and warning — so that
/// arm measures `commit-tree`, not this flag.
#[test]
fn am_recodes_a_koi8r_body_under_the_default_utf8_flag() {
    let f = Fixture::new("amkoi8");
    let mut mbox: Vec<u8> = Vec::new();
    mbox.extend_from_slice(b"From 1111111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001\n");
    mbox.extend_from_slice(b"From: t <t@e.co>\nDate: Thu, 7 Apr 2005 15:13:13 -0700\n");
    mbox.extend_from_slice(b"Subject: [PATCH] ascii subject\nMIME-Version: 1.0\n");
    mbox.extend_from_slice(b"Content-Type: text/plain; charset=KOI8-R\n");
    mbox.extend_from_slice(b"Content-Transfer-Encoding: 8bit\n\n");
    mbox.extend_from_slice(b"body \xe9 text\n\n---\n f.txt | 2 +-\n\n");
    mbox.extend_from_slice(
        b"diff --git a/f.txt b/f.txt\nindex 0000000..1111111 100644\n--- a/f.txt\n+++ b/f.txt\n\
          @@ -1,2 +1,2 @@\n-one\n+two\n two\n",
    );
    f.write_bytes("m.mbox", &mbox);

    let (code, out, err) = f.run(&["am", "m.mbox"]);
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    let (_, commit) = f.run_raw(&["cat-file", "commit", "HEAD"]);
    let needle: &[u8] = b"body \xd0\x98 text";
    assert!(
        commit.windows(needle.len()).any(|w| w == needle),
        "KOI8-R 0xE9 must reach the message as U+0418: {:?}",
        String::from_utf8_lossy(&commit)
    );
    // The `Applying:` line is the *subject*, which is plain ASCII here, so it is a
    // clean check that stdout carried the mail's bytes rather than a lossy render.
    assert_eq!(out, "Applying: ascii subject\n");
}

/// A charset nothing can decode. `convert_to_utf8()` (mailinfo.c:468-472) sets
/// `mi->input_error` and reports `cannot convert from %s to %s`; `git am` then dies
/// `could not parse patch`.
///
/// This is the pair to the test above: a re-coder that silently passes unknown
/// charsets through would apply the patch at exit 0 with mojibake in the message.
#[test]
fn an_unknown_charset_stops_the_run_rather_than_guessing() {
    let f = Fixture::new("ambogus");
    let mut mbox: Vec<u8> = Vec::new();
    mbox.extend_from_slice(b"From 1111111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001\n");
    mbox.extend_from_slice(b"From: t <t@e.co>\nDate: Thu, 7 Apr 2005 15:13:13 -0700\n");
    mbox.extend_from_slice(b"Subject: [PATCH] ascii subject\nMIME-Version: 1.0\n");
    mbox.extend_from_slice(b"Content-Type: text/plain; charset=NOSUCHSET-9999\n");
    mbox.extend_from_slice(b"Content-Transfer-Encoding: 8bit\n\n");
    mbox.extend_from_slice(b"body \xe9 text\n\n---\n f.txt | 2 +-\n\n");
    mbox.extend_from_slice(
        b"diff --git a/f.txt b/f.txt\nindex 0000000..1111111 100644\n--- a/f.txt\n+++ b/f.txt\n\
          @@ -1,2 +1,2 @@\n-one\n+two\n two\n",
    );
    f.write_bytes("m.mbox", &mbox);

    let (code, out, err) = f.run(&["am", "m.mbox"]);
    assert_eq!(code, 128, "stdout={out:?} stderr={err:?}");
    assert!(
        err.contains("error: cannot convert from NOSUCHSET-9999 to UTF-8"),
        "{err:?}"
    );
    assert!(err.contains("fatal: could not parse patch"), "{err:?}");
    assert_eq!(f.read("f.txt"), "one\ntwo\n", "no patch was applied");
}
