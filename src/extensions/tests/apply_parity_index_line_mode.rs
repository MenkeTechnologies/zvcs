//! The mode on a `git diff` `index <old>..<new> <mode>` line.
//!
//! `gitdiff_index()` hands that mode to `gitdiff_oldmode()` (apply.c:1150-1151):
//! it is the pre-image's mode only. `check_preimage()` then warns
//! `<path> has type <st>, expected <old>` when the file on disk has another mode
//! and gives the result the mode the file has (apply.c:3918-3926), so a file made
//! executable after the diff was taken stays executable. zvcs took the mode as the
//! post-image's, wrote the file 0644 and said nothing. With `--index`,
//! `verify_index_match()`'s ie_match_stat() counts a flipped owner x bit as
//! MODE_CHANGED (read-cache.c:316-324), so the file `does not match index`; zvcs
//! compared the contents only and applied.
//! `reverse_patches()` swaps the modes only `if (p->new_mode || p->is_delete)`
//! (apply.c:2348-2349), so under `-R` the index line still names the pre-image;
//! zvcs swapped it onto the post-image.
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

/// `a.txt` holds `line 1` .. `line 30`, committed 0644.
fn repo(label: &str, stock: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-apply-index-mode-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(&work).unwrap();
    git(stock, &work, b"", &["init", "-q", "-b", "main", "."]);
    let a: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    std::fs::write(work.join("a.txt"), a).unwrap();
    git(stock, &work, b"", &["add", "."]);
    git(stock, &work, b"", &["commit", "-q", "-m", "initial"]);
    work
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn mode_of(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o777)
}

/// Make `a.txt` 0755 on disk (and, with `stage`, in the index), run `args` with
/// `patch` on stdin in twin repositories, then compare the output, `a.txt`'s
/// bytes and mode, and the index.
fn same(label: &str, stage: bool, patch: &str, args: &[&str]) -> Out {
    let Some(stock) = stock_git::stock_git() else { return Default::default() };
    let s = repo(&format!("{label}-stock"), stock);
    let z = repo(&format!("{label}-zvcs"), stock);
    for dir in [&s, &z] {
        set_mode(&dir.join("a.txt"), 0o755);
        if stage {
            git(stock, dir, b"", &["add", "a.txt"]);
        }
    }
    let want = git(stock, &s, patch.as_bytes(), args);
    let got = git(ZVCS, &z, patch.as_bytes(), args);
    assert_eq!(got, want, "git {args:?}: left is zvcs, right is stock");
    let a = |d: &Path| (std::fs::read(d.join("a.txt")).ok(), mode_of(&d.join("a.txt")));
    assert_eq!(a(&z), a(&s), "git {args:?}: a.txt differs (left is zvcs)");
    assert_eq!(
        git(stock, &z, b"", &["ls-files", "-s"]),
        git(stock, &s, b"", &["ls-files", "-s"]),
        "git {args:?}: index differs (left is zvcs)"
    );
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
    want
}

const PATCH: &str = "diff --git a/a.txt b/a.txt
index 0000000..1111111 100644
--- a/a.txt
+++ b/a.txt
@@ -2,7 +2,7 @@ line 1
 line 2
 line 3
 line 4
-line 5
+line five
 line 6
 line 7
 line 8
";

#[test]
fn an_index_line_mode_is_the_preimage_mode_and_the_file_keeps_its_own() {
    for (i, args) in [&["apply"][..], &["apply", "-v"], &["apply", "--check"], &["apply", "--summary"], &["apply", "--stat", "--apply"]]
        .iter()
        .enumerate()
    {
        let want = same(&format!("wt-{i}"), false, PATCH, args);
        if stock_git::stock_git().is_some() && args.len() == 1 {
            assert_eq!(want.1, "warning: a.txt has type 100755, expected 100644\n", "{want:?}");
        }
    }
}

#[test]
fn a_flipped_exec_bit_does_not_match_the_index() {
    let want = same("idx", false, PATCH, &["apply", "--index"]);
    if stock_git::stock_git().is_some() {
        assert_eq!(want.1, "error: a.txt: does not match index\n", "{want:?}");
    }
    same("idx-check", false, PATCH, &["apply", "--index", "--check"]);
    // Staged 0755: the index agrees with the disk, so only the warning remains.
    same("idx-staged", true, PATCH, &["apply", "--index"]);
    same("cached-staged", true, PATCH, &["apply", "--cached"]);
}

#[test]
fn reversing_keeps_an_index_line_mode_on_the_preimage() {
    let reversed = PATCH.replace("-line 5\n+line five\n", "-line five\n+line 5\n");
    same("rev", false, &reversed, &["apply", "-R"]);
}
