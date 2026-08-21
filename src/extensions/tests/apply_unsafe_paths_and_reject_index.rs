//! `git apply`'s path gate, its report-mode ordering, and `--reject` as a mode of
//! the ordinary apply path rather than a separate one.
//!
//! Four behaviours that a plausible implementation gets wrong in a way nothing else
//! here would catch:
//!
//! * `check_unsafe_path()` (apply.c:4036) is decided **per patch, at check time**,
//!   and `--unsafe-paths` waives it. Refusing such a path unconditionally at parse
//!   time gets the flag wrong in one direction and `--stat` in the other, and lets
//!   `verify_path()`'s other refusals (`.git/…`, a `.` component) through entirely.
//!   `check_apply_state()` then cancels the flag again under `--index`/`--cached`
//!   (apply.c:180), and `add_index_entry()` re-checks every name on the way into
//!   the index (read-cache.c:1287).
//! * The report modes print **last** (apply.c:4993), after the check and the write,
//!   so a patch that does not apply produces no `--stat` at all. `--reject` sets
//!   `state->apply` four lines before those same modes clear it (apply.c:165 vs
//!   :169), so `--stat --reject` neither applies nor rejects anything.
//! * `--reject` is not a separate code path in git: it is `apply_with_reject` inside
//!   `apply_fragments()`, so it reads pre-images from the index under `--index`,
//!   honours `-C<n>`'s context reduction, and reports every patch's check before any
//!   patch's rejects (`write_out_results()` walks the list twice, apply.c:4817). A
//!   run that rejected anything also rolls the whole index update back, because
//!   `apply_patch()` returns -1 and `apply_all_patches()` never reaches
//!   `write_locked_index()` (apply.c:5129, :5173).
//! * `--build-fake-ancestor` and `--inaccurate-eof` both change observable output.
//!
//! Every expectation below was measured against stock git 2.55.0
//! (`/opt/homebrew/bin/git`, not the port).
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `f.txt` as committed: nine lines, each ending in a newline.
const F_BEFORE: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n";
/// `g.txt` as committed.
const G_BEFORE: &str = "a\nb\nc\n";
/// The blob id of [`F_BEFORE`], which is what `--build-fake-ancestor` must record.
const F_BEFORE_ID: &str = "aecbbc42373a64d8683ca19f17ff0ca72261a171";
/// The blob id of `f.txt` once [`CLEAN`] has applied.
const F_AFTER_ID: &str = "5a88c4ce03a97ba20df488533fd990f3ea1cee90";

/// stock git's own `diff` for a one-line change to `f.txt`, index line and all.
const CLEAN: &str = "\
diff --git a/f.txt b/f.txt
index aecbbc4..5a88c4c 100644
--- a/f.txt
+++ b/f.txt
@@ -2,7 +2,7 @@ l1
 l2
 l3
 l4
-l5
+L5
 l6
 l7
 l8
";

/// The same hunk with one context line corrupted, so it does not apply as written
/// but does apply once `-C<n>` allows the context to be trimmed.
const FUZZ: &str = "\
diff --git a/f.txt b/f.txt
--- a/f.txt
+++ b/f.txt
@@ -1,7 +1,7 @@
 XXX
 l2
 l3
-l4
+L4
 l5
 l6
 l7
";

/// A path that climbs out of the working tree. `-p1` strips the `a/`, leaving
/// `../outside/t.txt`.
const ESCAPE: &str = "\
diff --git a/../outside/t.txt b/../outside/t.txt
--- a/../outside/t.txt
+++ b/../outside/t.txt
@@ -1 +1 @@
-old
+new
";

/// A creation out of the working tree, so `-N` has a path to record.
const ESCAPE_NEW: &str = "\
diff --git a/../outside/fresh.txt b/../outside/fresh.txt
new file mode 100644
--- /dev/null
+++ b/../outside/fresh.txt
@@ -0,0 +1 @@
+created
";

/// A creation inside the repository's own `.git` directory, which `verify_path()`
/// refuses for a reason that has nothing to do with leaving the working tree.
const DOTGIT: &str = "\
diff --git a/.git/pwned b/.git/pwned
new file mode 100644
--- /dev/null
+++ b/.git/pwned
@@ -0,0 +1 @@
+owned
";

/// Two hunks far enough apart to stay separate: the first applies, the second
/// cannot.
const TWO_HUNKS: &str = "\
diff --git a/wide.txt b/wide.txt
--- a/wide.txt
+++ b/wide.txt
@@ -1,5 +1,5 @@
 l01
 l02
-l03
+L03
 l04
 l05
@@ -16,5 +16,5 @@
 l16
 l17
-XXX
+L18
 l19
 l20
";

/// A creation, for the `-N` cases.
const CREATE: &str = "\
diff --git a/new.txt b/new.txt
new file mode 100644
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,3 @@
+n1
+n2
+n3
";

/// A patch that takes the trailing newline off `f.txt`'s last line.
const NO_NEWLINE: &str = "\
diff --git a/f.txt b/f.txt
index aecbbc4..897e948 100644
--- a/f.txt
+++ b/f.txt
@@ -6,4 +6,4 @@ l5
 l6
 l7
 l8
-l9
+l9
\\ No newline at end of file
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
    /// `<root>/work` is the repository; `<root>/outside` is its sibling, which
    /// [`ESCAPE`] reaches through `..` and nothing else may touch.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-applysafe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::write(root.join("outside/t.txt"), b"old\n").unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("f.txt", F_BEFORE);
        f.write("g.txt", G_BEFORE);
        let wide: String = (1..=20).map(|i| format!("l{i:02}\n")).collect();
        f.write("wide.txt", &wide);
        f.git(&["add", "f.txt", "g.txt", "wide.txt"]);
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

    fn exists(&self, path: &str) -> bool {
        self.work.join(path).exists()
    }

    /// The sibling directory `..` in [`ESCAPE`] points at.
    fn outside(&self, path: &str) -> String {
        String::from_utf8(std::fs::read(self.root.join("outside").join(path)).unwrap()).unwrap()
    }

    fn outside_exists(&self, path: &str) -> bool {
        self.root.join("outside").join(path).exists()
    }

    /// Run `git apply <args> <patch>`, returning `(exit code, stdout, stderr)`. The
    /// patch lives outside the worktree so it cannot show up as an untracked file.
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

    fn stage_lines(&self) -> Vec<String> {
        let out = self.cmd(&["ls-files", "--stage"]).output().unwrap();
        assert!(out.status.success(), "ls-files failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    }

    /// The staged blob id of `path`, from `ls-files --stage`.
    fn staged_id(&self, path: &str) -> String {
        self.stage_lines()
            .into_iter()
            .find(|l| l.ends_with(&format!("\t{path}")))
            .unwrap_or_else(|| panic!("no index entry for {path}"))
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned()
    }
}

// ---------------------------------------------------------------------------
// check_unsafe_path() and --unsafe-paths
// ---------------------------------------------------------------------------

#[test]
fn an_escaping_path_is_refused_by_name_and_written_only_with_unsafe_paths() {
    let f = Fixture::new("escape");

    // Stock: `error: invalid path '../outside/t.txt'`, exit 128, nothing written.
    // Not `fatal: refusing to apply …`, which git never prints.
    let (code, out, err) = f.apply(&[], ESCAPE);
    assert_eq!((code, out.as_str()), (128, ""));
    assert_eq!(err, "error: invalid path '../outside/t.txt'\n");
    assert_eq!(f.outside("t.txt"), "old\n");

    // The flag is what git waives the gate with, so the write must actually happen.
    let (code, out, err) = f.apply(&["--unsafe-paths"], ESCAPE);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
    assert_eq!(f.outside("t.txt"), "new\n");
}

#[test]
fn verify_path_refuses_a_dot_git_component_even_though_it_stays_in_the_tree() {
    let f = Fixture::new("dotgit");

    // `.git/pwned` never leaves the working tree, so a gate that only looks for
    // `..` and leading slashes lets it through and writes into the repository.
    let (code, _, err) = f.apply(&[], DOTGIT);
    assert_eq!(code, 128);
    assert_eq!(err, "error: invalid path '.git/pwned'\n");
    assert!(!f.exists(".git/pwned"), "a plain apply wrote into .git");

    // With the flag git does write it, which is exactly what the flag is for.
    let (code, _, err) = f.apply(&["--unsafe-paths"], DOTGIT);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(f.read(".git/pwned"), "owned\n");
}

#[test]
fn index_mode_cancels_unsafe_paths_and_the_preimage_check_speaks_first() {
    let f = Fixture::new("idxcancel");

    // apply.c:180 clears the flag, and `check_preimage()` runs before
    // `check_unsafe_path()` — so this is exit 1 with an index message, not the
    // exit 128 the path gate would give.
    for flag in ["--index", "--cached", "--3way"] {
        let (code, _, err) = f.apply(&["--unsafe-paths", flag], ESCAPE);
        assert_eq!(code, 1, "{flag}");
        assert_eq!(err, "error: ../outside/t.txt: does not exist in index\n", "{flag}");
        assert_eq!(f.outside("t.txt"), "old\n", "{flag}");
    }
}

#[test]
fn a_report_mode_alone_never_reaches_the_path_gate() {
    let f = Fixture::new("statescape");

    // With neither `--check` nor an apply, git runs no check at all, so the
    // out-of-tree path is reported and not judged.
    let (code, out, err) = f.apply(&["--stat"], ESCAPE);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        " ../outside/t.txt |    2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n"
    );
    assert_eq!(f.outside("t.txt"), "old\n");

    // `--apply` puts the check back, and then the gate fires.
    let (code, out, err) = f.apply(&["--stat", "--apply"], ESCAPE);
    assert_eq!((code, out.as_str()), (128, ""));
    assert_eq!(err, "error: invalid path '../outside/t.txt'\n");
}

#[test]
fn intent_to_add_still_refuses_the_name_at_the_index_even_with_unsafe_paths() {
    let f = Fixture::new("itaescape");
    // `-N` does not set `check_index`, so `--unsafe-paths` survives and the file is
    // written; `add_index_entry()` then refuses the name and the run ends at 128
    // with the index untouched.
    let before = f.stage_lines();
    // `-N` records only the paths a patch *creates*, so this needs a creation.
    let (code, _, err) = f.apply(&["--unsafe-paths", "-N"], ESCAPE_NEW);
    assert_eq!(code, 128);
    assert_eq!(
        err,
        "error: invalid path '../outside/fresh.txt'\n\
         error: unable to add cache entry for ../outside/fresh.txt\n"
    );
    assert!(f.outside_exists("fresh.txt"), "the write still happened");
    assert_eq!(f.outside("fresh.txt"), "created\n");
    assert_eq!(f.stage_lines(), before, "the index update was rolled back");
}

// ---------------------------------------------------------------------------
// Report modes print last, and --reject does not survive them
// ---------------------------------------------------------------------------

#[test]
fn a_patch_that_does_not_apply_prints_no_report() {
    let f = Fixture::new("statfail");
    let (code, out, err) = f.apply(&["--stat", "--numstat", "--summary", "--apply"], FUZZ);
    assert_eq!(code, 1);
    assert_eq!(out, "", "git reaches its report modes only past the write");
    assert!(err.contains("error: f.txt: patch does not apply\n"), "{err}");
}

#[test]
fn stat_with_reject_neither_applies_nor_rejects() {
    let f = Fixture::new("statreject");
    // `--reject` sets `state->apply`; `--stat` clears it again four lines later.
    let (code, out, err) = f.apply(&["--stat", "--reject"], FUZZ);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        " f.txt |    2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n"
    );
    assert!(!f.exists("f.txt.rej"), "nothing was applied, so nothing rejected");
    assert_eq!(f.read("f.txt"), F_BEFORE);
}

#[test]
fn no_apply_on_its_own_leaves_the_default_alone() {
    let f = Fixture::new("noapply");
    // `--apply` is `OPT_BOOL` on `force_apply`, which starts at 0 — so `--no-apply`
    // only cancels an earlier `--apply` and never turns applying off by itself.
    let (code, _, err) = f.apply(&["--no-apply"], CLEAN);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(f.read("f.txt"), "l1\nl2\nl3\nl4\nL5\nl6\nl7\nl8\nl9\n");
}

// ---------------------------------------------------------------------------
// --reject inside the ordinary path
// ---------------------------------------------------------------------------

#[test]
fn reject_with_index_stages_a_clean_result_and_rolls_back_a_rejected_one() {
    let f = Fixture::new("rejidx");

    let (code, _, err) = f.apply(&["--reject", "--index"], CLEAN);
    assert_eq!(code, 0);
    assert_eq!(err, "Checking patch f.txt...\nApplied patch f.txt cleanly.\n");
    assert_eq!(f.staged_id("f.txt"), F_AFTER_ID, "a clean --reject run stages");

    // A second run with a rejected hunk: the worktree keeps what applied and the
    // `.rej` is written, but `apply_all_patches()` never reaches the index write.
    let f = Fixture::new("rejidxpart");
    let before = f.staged_id("wide.txt");
    let (code, _, err) = f.apply(&["--reject", "--index"], TWO_HUNKS);
    assert_eq!(code, 1);
    assert!(err.contains("Applying patch wide.txt with 1 reject...\n"), "{err}");
    assert!(err.contains("Hunk #1 applied cleanly.\nRejected hunk #2.\n"), "{err}");
    assert!(f.read("wide.txt").contains("L03\n"), "hunk 1 was written");
    assert_eq!(
        f.staged_id("wide.txt"),
        before,
        "a rejected hunk rolls the whole index update back"
    );
    assert_eq!(
        f.read("wide.txt.rej"),
        "diff a/wide.txt b/wide.txt\t(rejected hunks)\n\
         @@ -16,5 +16,5 @@\n l16\n l17\n-XXX\n+L18\n l19\n l20\n"
    );
}

#[test]
fn intent_to_add_stages_nothing_when_the_same_run_rejected_a_hunk() {
    let f = Fixture::new("itarej");
    // The creation applies cleanly and `-N` would record it — but the second patch
    // rejects a hunk, `apply_patch()` returns -1, and the index is never written.
    let both = format!("{CREATE}{TWO_HUNKS}");
    let before = f.stage_lines();
    let (code, _, err) = f.apply(&["--reject", "-N"], &both);
    assert_eq!(code, 1);
    assert_eq!(f.read("new.txt"), "n1\nn2\nn3\n", "the creation was still written");
    assert!(f.exists("wide.txt.rej"));
    assert_eq!(f.stage_lines(), before, "no intent-to-add entry survived the run");
    // The `Applied patch … cleanly.` for the first patch belongs to the write phase,
    // so it lands after the *second* patch's check.
    let applied = err.find("Applied patch new.txt cleanly.").expect("clean report");
    let check2 = err.find("Checking patch wide.txt...").expect("second check");
    assert!(check2 < applied, "{err}");
}

#[test]
fn reject_with_index_refuses_a_worktree_that_does_not_match_and_writes_no_rej() {
    let f = Fixture::new("rejdirty");
    f.write("f.txt", "DIRTY\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n");
    let (code, _, err) = f.apply(&["--reject", "--index"], CLEAN);
    assert_eq!(code, 1);
    assert_eq!(err, "Checking patch f.txt...\nerror: f.txt: does not match index\n");
    assert!(!f.exists("f.txt.rej"));
    assert_eq!(f.read("f.txt"), "DIRTY\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n");

    // `--cached` never looks at the worktree, so the same patch applies there.
    let f = Fixture::new("rejdirtycached");
    f.write("f.txt", "DIRTY\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n");
    let (code, _, err) = f.apply(&["--reject", "--cached"], CLEAN);
    assert_eq!((code, err.as_str()), (0, "Checking patch f.txt...\nApplied patch f.txt cleanly.\n"));
    assert_eq!(f.staged_id("f.txt"), F_AFTER_ID);
    assert_eq!(f.read("f.txt"), "DIRTY\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n");
}

#[test]
fn reject_checks_every_patch_before_it_rejects_any() {
    let f = Fixture::new("rejorder");
    // `write_out_results()` walks the list twice, so the whole check phase is over
    // before the first "Applying patch … with N rejects".
    let both = format!("{TWO_HUNKS}{}", TWO_HUNKS.replace("wide.txt", "wide2.txt"));
    std::fs::copy(f.work.join("wide.txt"), f.work.join("wide2.txt")).unwrap();
    f.git(&["add", "wide2.txt"]);
    f.git(&["commit", "-qm", "second"]);

    let (code, _, err) = f.apply(&["--reject"], &both);
    assert_eq!(code, 1);
    let check2 = err.find("Checking patch wide2.txt...").expect("second check");
    let apply1 = err.find("Applying patch wide.txt").expect("first reject report");
    assert!(
        check2 < apply1,
        "both checks must precede both reject reports:\n{err}"
    );
}

#[test]
fn context_reduction_reaches_the_reject_path_too() {
    // Without `-C<n>` git keeps every context line, so the corrupted one fails the
    // hunk; with it the hunk is trimmed and lands two lines off.
    let f = Fixture::new("ctxplain");
    let (code, _, _) = f.apply(&["--reject"], FUZZ);
    assert_eq!(code, 1);
    assert!(f.exists("f.txt.rej"));
    assert_eq!(f.read("f.txt"), F_BEFORE);

    let f = Fixture::new("ctxc1");
    let (code, _, err) = f.apply(&["--reject", "-C1"], FUZZ);
    assert_eq!(code, 0, "{err}");
    assert!(!f.exists("f.txt.rej"));
    assert_eq!(f.read("f.txt"), "l1\nl2\nl3\nL4\nl5\nl6\nl7\nl8\nl9\n");
    assert_eq!(
        err,
        "Checking patch f.txt...\n\
         Hunk #1 succeeded at 2 (offset 2 lines).\n\
         Context reduced to (2/2) to apply fragment at 2\n\
         Applied patch f.txt cleanly.\n"
    );
}

// ---------------------------------------------------------------------------
// --build-fake-ancestor and --inaccurate-eof
// ---------------------------------------------------------------------------

#[test]
fn build_fake_ancestor_records_the_preimage_blob_and_does_not_apply() {
    let f = Fixture::new("fakeanc");
    let (code, out, err) = f.apply(&["--build-fake-ancestor", "fa"], CLEAN);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
    assert_eq!(f.read("f.txt"), F_BEFORE, "naming a fake ancestor turns applying off");

    let listed = f
        .cmd(&["ls-files", "--stage"])
        .env("GIT_INDEX_FILE", "fa")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout),
        format!("100644 {F_BEFORE_ID} 0\tf.txt\n")
    );

    // A patch with no usable pre-image id has nowhere to take the blob from.
    let f = Fixture::new("fakeancbad");
    let (code, _, err) = f.apply(&["--build-fake-ancestor", "fa"], FUZZ);
    assert_eq!(code, 128);
    assert_eq!(err, "error: sha1 information is lacking or useless (f.txt).\n");
}

#[test]
fn inaccurate_eof_lets_a_newline_terminated_hunk_meet_a_file_without_one() {
    // The file's last line has no newline; the patch's context says it does.
    let body = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9";

    let f = Fixture::new("eofplain");
    f.write("f.txt", body);
    let (code, _, err) = f.apply(&[], CLEAN);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(f.read("f.txt"), "l1\nl2\nl3\nl4\nL5\nl6\nl7\nl8\nl9");

    // With the flag both images lose their final newline, so the hunk's last line
    // is spliced in without one and runs straight into the file's own last line.
    let f = Fixture::new("eoffudge");
    f.write("f.txt", body);
    let (code, _, err) = f.apply(&["--inaccurate-eof"], CLEAN);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(f.read("f.txt"), "l1\nl2\nl3\nl4\nL5\nl6\nl7\nl8l9");

    // A hunk whose post-image already ends without a newline is left alone.
    let f = Fixture::new("eofnonl");
    let (code, _, err) = f.apply(&["--inaccurate-eof"], NO_NEWLINE);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(f.read("f.txt"), body);
}

// ---------------------------------------------------------------------------
// --directory is normalised where git normalises it
// ---------------------------------------------------------------------------

#[test]
fn a_directory_that_climbs_above_the_root_is_a_usage_error() {
    let f = Fixture::new("dirnorm");
    let (code, _, err) = f.apply(&["--directory=.."], CLEAN);
    assert_eq!(code, 129, "`strbuf_normalize_path()` fails before any patch is opened");
    assert_eq!(err, "error: unable to normalize directory: '..'\n");

    // One that resolves back to the root is not an error, and leaves paths alone.
    let f = Fixture::new("dirnormok");
    let (code, _, err) = f.apply(&["--directory=sub/.."], CLEAN);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(f.read("f.txt"), "l1\nl2\nl3\nl4\nL5\nl6\nl7\nl8\nl9\n");

    // An absolute root normalises fine, and what happens next depends on whether
    // the patch has a preimage to read. This case asserted the creation outcome
    // while handing `apply` a *modification*, so it demanded of zvcs something
    // stock git does not do — verified against git 2.55.0 in the same fixture.
    //
    // A modification reads the preimage first (`load_preimage()` ->
    // `read_old_data()`), so it never reaches the name check and reports the open
    // failure at 1.
    let f = Fixture::new("dirabs");
    let (code, _, err) = f.apply(&["--directory=/tmp"], CLEAN);
    assert_eq!((code, err.as_str()), (1, "error: /tmp/f.txt: No such file or directory\n"));

    // A creation has nothing to read, so `verify_path()` refuses the absolute
    // name — which is the behaviour this case was written to pin.
    let f = Fixture::new("dirabsnew");
    let (code, _, err) = f.apply(&["--directory=/tmp"], CREATE);
    assert_eq!((code, err.as_str()), (128, "error: invalid path '/tmp/new.txt'\n"));
}
