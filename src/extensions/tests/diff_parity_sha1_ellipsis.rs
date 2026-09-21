//! `GIT_PRINT_SHA1_ELLIPSIS=yes` and the raw diff formats.
//!
//! `diff_flush_raw()` (diff.c:6477-6479) and `show_raw_diff()`
//! (combine-diff.c:1257-1259) render their object-name columns through
//! `diff_aligned_abbrev()`, which pads an abbreviated name with dots when the
//! environment asks for it:
//!
//! ```c
//! if (!print_sha1_ellipsis())
//!         return abbrev;
//! abblen = strlen(abbrev);
//! if (abblen < the_hash_algo->hexsz - 3) {
//!         if (len < abblen && abblen <= len + 2)
//!                 xsnprintf(hex, sizeof(hex), "%s%.*s", abbrev, len+3-abblen, "..");
//!         else
//!                 xsnprintf(hex, sizeof(hex), "%s...", abbrev);
//!         return hex;
//! }
//! return oid_to_hex(oid);
//! ```
//! (diff.c:6433-6466), with `print_sha1_ellipsis()` reading
//! `GIT_PRINT_SHA1_ELLIPSIS` and accepting only `"yes"`, case-insensitively
//! (environment.c:205-217).
//!
//! The port printed the bare abbreviation everywhere, which is why every raw
//! expectation in t4013-diff-various.sh's default (`# magic is (not used)`)
//! variant — the one the suite runs with the variable set — disagreed.
//!
//! Two details the padding is easy to get wrong, both asserted below:
//!
//!   * The column is meant to keep its width. A name that had to widen past the
//!     requested `--abbrev=<n>` because its prefix collided loses one dot per
//!     extra character, so `a2f5...` and `a2f54..` are both seven wide. The
//!     fixture's two blobs really do share four hex characters, so `--abbrev=4`
//!     widens to five on both sides.
//!   * Nothing else is padded: not the patch `index` line, which shares the
//!     abbreviation but not `diff_aligned_abbrev()`, and not a name within three
//!     characters of the full hash, which gives up and prints the whole id.
//!
//! Every expectation was measured from stock git 2.55.0 over the same fixture.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Two blob bodies whose SHA-1 object names share the four hex characters
/// `a2f5` — `a2f547f9779a…` and `a2f5637ea052…`. Any pair would do; these are
/// the first the search found, and their names are fixed by SHA-1.
const COLLIDE_A: &str = "p28\n";
const COLLIDE_B: &str = "p104\n";

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
    /// `x` and `y` committed with the two colliding bodies, then `x` staged with
    /// the other one — so the raw record names both colliding objects.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diff-ell-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("x"), COLLIDE_A).unwrap();
        std::fs::write(f.work.join("y"), COLLIDE_B).unwrap();
        f.git(&["add", "x", "y"]);
        f.git(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("x"), COLLIDE_B).unwrap();
        f.git(&["add", "x"]);
        f
    }

    fn cmd(&self, ellipsis: Option<&str>, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join("zvcs"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat")
            .env_remove("GIT_PRINT_SHA1_ELLIPSIS");
        if let Some(v) = ellipsis {
            c.env("GIT_PRINT_SHA1_ELLIPSIS", v);
        }
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(None, args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, ellipsis: Option<&str>, args: &[&str]) -> String {
        let out = self.cmd(ellipsis, args).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "`git {args:?}`: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// The widened-and-trimmed case, across every verb that writes a raw record.
/// `--abbrev=4` has to grow to five characters on both sides, and the padding
/// shrinks to two dots to compensate.
#[test]
fn a_widened_abbreviation_loses_a_dot_so_the_column_keeps_its_width() {
    let f = Fixture::new("widen");
    let want = ":100644 100644 a2f54.. a2f56.. M\tx\n";
    for verb in [
        vec!["diff", "--cached", "--raw", "--abbrev=4", "HEAD"],
        vec!["diff-index", "--cached", "--raw", "--abbrev=4", "HEAD"],
    ] {
        assert_eq!(f.stdout(Some("yes"), &verb), want, "{verb:?}");
    }
    // Without the variable the same names print bare — and still widened, since
    // the widening is `repo_find_unique_abbrev()`'s, not the padding's.
    assert_eq!(
        f.stdout(None, &["diff", "--cached", "--raw", "--abbrev=4", "HEAD"]),
        ":100644 100644 a2f54 a2f56 M\tx\n"
    );
}

/// Only `"yes"` turns the padding on, and case does not matter
/// (`!strcasecmp(v, "yes")`).
#[test]
fn only_yes_turns_the_padding_on() {
    let f = Fixture::new("value");
    let args = ["diff", "--cached", "--raw", "--abbrev=4", "HEAD"];
    let padded = ":100644 100644 a2f54.. a2f56.. M\tx\n";
    let bare = ":100644 100644 a2f54 a2f56 M\tx\n";
    assert_eq!(f.stdout(Some("yes"), &args), padded);
    assert_eq!(f.stdout(Some("YES"), &args), padded);
    assert_eq!(f.stdout(Some("Yes"), &args), padded);
    for off in ["no", "1", "true", ""] {
        assert_eq!(f.stdout(Some(off), &args), bare, "GIT_PRINT_SHA1_ELLIPSIS={off:?}");
    }
}

/// An abbreviation within three characters of the full hash has no room for a
/// marker, so `diff_aligned_abbrev()` prints the whole name instead — and
/// `--no-abbrev` lands in the same arm.
#[test]
fn a_nearly_complete_name_is_printed_whole_instead_of_padded() {
    let f = Fixture::new("wide");
    let full_a = "a2f547f9779a76283be5eba18774a02eaa00d7e7";
    let full_b = "a2f5637ea052a77dc3937c7412a0cb87883c9597";

    // 36 + 3 dots still fits under the 40-character hash.
    assert_eq!(
        f.stdout(Some("yes"), &["diff", "--cached", "--raw", "--abbrev=36", "HEAD"]),
        format!(":100644 100644 {}... {}... M\tx\n", &full_a[..36], &full_b[..36])
    );
    // 38 does not.
    assert_eq!(
        f.stdout(Some("yes"), &["diff", "--cached", "--raw", "--abbrev=38", "HEAD"]),
        format!(":100644 100644 {full_a} {full_b} M\tx\n")
    );
    assert_eq!(
        f.stdout(Some("yes"), &["diff", "--cached", "--raw", "--no-abbrev", "HEAD"]),
        format!(":100644 100644 {full_a} {full_b} M\tx\n")
    );
}

/// The patch `index` line shares the abbreviation but not `diff_aligned_abbrev()`,
/// so it is never padded — nor is `--stat`, `--numstat` or `--name-status`.
#[test]
fn nothing_but_the_raw_column_is_padded() {
    let f = Fixture::new("scope");
    let patch = f.stdout(Some("yes"), &["diff", "--cached", "--abbrev=4", "HEAD", "--", "x"]);
    assert!(patch.contains("\nindex a2f54..a2f56 100644\n"), "{patch}");
    assert!(!patch.contains("..."), "{patch}");

    let names = f.stdout(Some("yes"), &["diff", "--cached", "--name-status", "HEAD"]);
    assert_eq!(names, "M\tx\n");
}

/// The combined raw record (`show_raw_diff()`) takes the same padding: one
/// column per parent and one for the result.
#[test]
fn the_combined_raw_record_is_padded_too() {
    let f = Fixture::new("combined");
    f.git(&["commit", "-q", "-m", "second"]);
    f.git(&["checkout", "-q", "-b", "side", "HEAD~1"]);
    std::fs::write(f.work.join("x"), "side\n").unwrap();
    f.git(&["commit", "-q", "-a", "-m", "side"]);
    f.git(&["checkout", "-q", "main"]);
    let out = f.cmd(None, &["merge", "-q", "side"]).output().unwrap();
    assert!(!out.status.success(), "the merge is meant to conflict: {out:?}");
    std::fs::write(f.work.join("x"), "merged\n").unwrap();
    f.git(&["commit", "-q", "-a", "-m", "merge"]);

    let padded = f.stdout(Some("yes"), &["diff-tree", "-c", "--raw", "--abbrev=4", "HEAD"]);
    let record = padded.lines().find(|l| l.starts_with("::")).expect("a combined record");
    // `::<mode> <mode> <mode> <oid> <oid> <oid> <status><TAB><path>`
    let columns: Vec<&str> = record.split_whitespace().collect();
    assert_eq!(columns.len(), 8, "{record}");
    for oid in &columns[3..6] {
        // Each column is the same seven wide however far its name had to widen.
        assert!(oid.ends_with('.'), "unpadded combined column {oid:?} in {record}");
        assert_eq!(oid.len(), 7, "{record}");
    }

    let bare = f.stdout(None, &["diff-tree", "-c", "--raw", "--abbrev=4", "HEAD"]);
    let record = bare.lines().find(|l| l.starts_with("::")).expect("a combined record");
    for oid in record.split_whitespace().skip(3).take(3) {
        assert!(!oid.contains('.'), "padded combined column {oid:?} in {record}");
    }
}
