//! `git apply --recount`, miscounted hunk headers, and a copy whose source the
//! same patch modifies.
//!
//! * `recount_diff()` (apply.c:1524-1572) counts the body only up to the end of
//!   the input, the next `@@ ` or the next `diff `; any other line ends the scan
//!   with `warning: recount: unexpected line: <line>` and keeps the header's
//!   counts, so the count-driven `parse_fragment()` loop (apply.c:1819-1895)
//!   still reports `corrupt patch at <file>:<line>`. zvcs read the body until the
//!   first non-body line instead, so a mangled line was silently dropped and an
//!   mbox signature (`-- `) was taken as a deleted line.
//! * The fragment counters are `unsigned long` (apply.c:1781): a body line past
//!   the header's count wraps them, and the patch is corrupt. zvcs saturated
//!   them at zero and applied the hunk.
//! * `previous_patch()` returns NULL for a copy (apply.c:3517-3518), so
//!   `copy from <path>` copies `<path>` as it was before the patch, even when an
//!   earlier part of the same patch modifies it. zvcs copied the modified file.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in identical
//! repositories.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

type Out = (String, String, i32);

fn git(bin: &str, dir: &Path, stdin: &[u8], args: &[&str]) -> Out {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// `a.txt` holds `line 1` .. `line 30`, `c.txt` holds `c1`, `c2`; both committed.
fn repo(label: &str, stock: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-apply-recount-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(&work).unwrap();
    git(stock, &work, b"", &["init", "-q", "-b", "main", "."]);
    let a: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    std::fs::write(work.join("a.txt"), a).unwrap();
    std::fs::write(work.join("c.txt"), "c1\nc2\n").unwrap();
    git(stock, &work, b"", &["add", "."]);
    git(stock, &work, b"", &["commit", "-q", "-m", "initial"]);
    work
}

/// Run `args` with `patch` on stdin in twin repositories; compare the output,
/// then the worktree files and the index the run leaves behind.
fn same(label: &str, patch: &str, args: &[&str]) -> Out {
    let Some(stock) = stock_git::stock_git() else { return Default::default() };
    let s = repo(&format!("{label}-stock"), stock);
    let z = repo(&format!("{label}-zvcs"), stock);
    let want = git(stock, &s, patch.as_bytes(), args);
    let got = git(ZVCS, &z, patch.as_bytes(), args);
    assert_eq!(got, want, "git {args:?}: left is zvcs, right is stock");
    for f in ["a.txt", "c.txt", "copy.txt"] {
        assert_eq!(
            std::fs::read(z.join(f)).ok(),
            std::fs::read(s.join(f)).ok(),
            "git {args:?}: {f} differs (left is zvcs)"
        );
    }
    assert_eq!(
        git(stock, &z, b"", &["ls-files", "-s"]),
        git(stock, &s, b"", &["ls-files", "-s"]),
        "git {args:?}: index differs (left is zvcs)"
    );
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
    want
}

/// A hunk changing `line 5` under `header`; `mangle` breaks its third line.
fn hunk(header: &str, mangle: bool) -> String {
    let ctx4 = if mangle { "XXline 4" } else { " line 4" };
    format!(
        "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n{header}\n line 2\n line 3\n{ctx4}\n-line 5\n+line five\n line 6\n line 7\n line 8\n"
    )
}

#[test]
fn recount_warns_on_an_unexpected_line_and_keeps_the_header_counts() {
    let patch = hunk("@@ -2,7 +2,7 @@", true);
    for args in [
        &["apply", "--recount"][..],
        &["apply", "--recount", "--check"],
        &["apply", "--recount", "--stat"],
        &["apply", "--recount", "--index"],
    ] {
        let want = same(&format!("mangled-{}", args.len()), &patch, args);
        if stock_git::stock_git().is_some() {
            assert!(want.1.starts_with("warning: recount: unexpected line: XXline 4\n\n"), "{want:?}");
            assert_eq!(want.2, 128, "{want:?}");
        }
    }
}

#[test]
fn recount_stops_at_an_mbox_signature_line_it_cannot_place() {
    let patch = format!("{}-- \n2.56.0\n\n", hunk("@@ -2,7 +2,7 @@", false));
    for args in [&["apply", "--recount"][..], &["apply", "--recount", "--numstat"], &["apply", "--recount", "-R", "--check"]] {
        same(&format!("signature-{}", args.join("")), &patch, args);
    }
}

#[test]
fn recount_corrects_wrong_header_counts() {
    for (label, header) in [("under", "@@ -2,6 +2,6 @@"), ("over", "@@ -2,9 +2,9 @@"), ("skew", "@@ -2,6 +2,7 @@")] {
        let patch = hunk(header, false);
        same(&format!("rc-{label}"), &patch, &["apply", "--recount"]);
        same(&format!("rc-{label}-stat"), &patch, &["apply", "--recount", "--stat"]);
    }
}

#[test]
fn a_body_line_past_the_header_count_wraps_it_and_is_corrupt() {
    // One side's count is satisfied a line before the other's: the next context
    // line wraps the satisfied counter, and the scan runs off the input's end.
    for (label, header) in [("old", "@@ -2,6 +2,7 @@"), ("new", "@@ -2,7 +2,6 @@")] {
        let patch = hunk(header, false);
        for (i, args) in [&["apply"][..], &["apply", "--check"], &["apply", "--stat"], &["apply", "--index"]].iter().enumerate() {
            let want = same(&format!("wrap-{label}-{i}"), &patch, args);
            if stock_git::stock_git().is_some() {
                assert!(want.1.contains("corrupt patch at <stdin>:"), "{want:?}");
            }
        }
    }
    // Both counts satisfied together: the extra context line is not the hunk's.
    same("short-both", &hunk("@@ -2,6 +2,6 @@", false), &["apply"]);
}

/// `c.txt` gains a line, and `copy.txt` is copied from `c.txt` with another.
const MODIFY_THEN_COPY: &str = "diff --git a/c.txt b/c.txt
--- a/c.txt
+++ b/c.txt
@@ -1,2 +1,3 @@
 c1
 c2
+trailing
diff --git a/c.txt b/copy.txt
similarity index 66%
copy from c.txt
copy to copy.txt
--- a/c.txt
+++ b/copy.txt
@@ -1,2 +1,3 @@
 c1
 c2
+c3
";

#[test]
fn a_copy_reads_its_source_from_before_the_patch() {
    for args in [
        &["apply"][..],
        &["apply", "--index"],
        &["apply", "--cached"],
        &["apply", "--check"],
        &["apply", "-C1"],
        &["apply", "--unidiff-zero"],
    ] {
        let want = same("copy", MODIFY_THEN_COPY, args);
        if stock_git::stock_git().is_some() {
            assert_eq!(want.2, 0, "git {args:?}: {want:?}");
        }
    }
}
