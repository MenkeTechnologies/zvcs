//! What `git diff` prints when the whitespace options ignore every change.
//!
//! `builtin_diff()` keeps the `diff --git` block in a strbuf and lets
//! `fn_out_consume()` emit it with the first hunk line, so a modification that
//! compares equal under `-w`/`-b` prints nothing at all — not even the header and
//! `index` line. `builtin_diffstat()` drops the same pair from the diffstat
//! ("omit diffstats of modified files where nothing changed"), `diff_flush()` runs
//! every pair of the raw/name formats through `diff_flush_patch_quietly()` first
//! and drops the silent ones, and the exit status follows: the whitespace options
//! set `diff_from_contents`, which makes `diff_flush()` derive `has_changes` from
//! what was actually emitted.
//!
//! A creation, a deletion, a rename, a mode change or a binary pair still shows its
//! header, because those force `must_show_header`.
//!
//! Every expectation here is stock git 2.55.0's output for the same repository —
//! the version the port tracks. 2.50 still listed such a pair in `--name-only` and
//! `--raw`, so measuring against an OS-vendored git disagrees here.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-wsdiff-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout)`; stderr is asserted to be empty, since none of these
    /// runs has anything to report there.
    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = self.cmd(args).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(err, "", "unexpected stderr from `git {args:?}`");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    /// A committed `f.txt` plus a worktree copy that differs only in whitespace.
    fn whitespace_only_edit(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.write("f.txt", b"a\nb\nc\n");
        f.git(&["add", "f.txt"]);
        f.git(&["commit", "-q", "-m", "init"]);
        f.write("f.txt", b"a \nb\t\nc\n");
        f
    }
}

/// No hunk survives `-w`, so no file section is emitted — the `diff --git` and
/// `index` lines go with it.
#[test]
fn a_whitespace_only_modification_emits_no_file_section() {
    let f = Fixture::whitespace_only_edit("patch");
    assert_eq!(f.run(&["diff", "-w"]), (0, String::new()));
    assert_eq!(f.run(&["diff", "-w", "HEAD"]), (0, String::new()));
    assert_eq!(f.run(&["diff", "-b"]), (0, String::new()));
    // The same edit without the option is a normal patch.
    let (_, full) = f.run(&["diff"]);
    assert!(full.starts_with("diff --git a/f.txt b/f.txt\n"), "{full}");
}

/// The diffstat entry is dropped with it, so no ` | 0` row and no summary line.
#[test]
fn the_stat_formats_drop_the_entry_entirely() {
    let f = Fixture::whitespace_only_edit("stat");
    assert_eq!(f.run(&["diff", "-w", "--stat"]), (0, String::new()));
    assert_eq!(f.run(&["diff", "-w", "--numstat"]), (0, String::new()));
    assert_eq!(f.run(&["diff", "-w", "--shortstat"]), (0, String::new()));
    assert_eq!(f.run(&["diff", "-w", "--stat", "HEAD"]), (0, String::new()));
}

/// Every format reports "no changes", including the ones that list the queue: under
/// `diff_from_contents` they too render the pair quietly first and drop it.
#[test]
fn the_exit_status_follows_what_was_emitted() {
    let f = Fixture::whitespace_only_edit("exit");
    assert_eq!(f.run(&["diff", "-w", "--exit-code"]).0, 0);
    assert_eq!(f.run(&["diff", "-w", "--quiet"]).0, 0);
    assert_eq!(f.run(&["diff", "-w", "--stat", "--exit-code"]).0, 0);
    assert_eq!(f.run(&["diff", "-w", "-s", "--exit-code"]).0, 0);
    assert_eq!(f.run(&["diff", "-w", "--name-only", "--exit-code"]), (0, String::new()));
    assert_eq!(f.run(&["diff", "-w", "--raw", "--exit-code"]), (0, String::new()));
    assert_eq!(f.run(&["diff", "-w", "--name-status"]), (0, String::new()));
    // Without a whitespace option the queue alone decides, as before.
    assert_eq!(f.run(&["diff", "--exit-code"]).0, 1);
    assert_eq!(f.run(&["diff", "--name-only"]), (0, "f.txt\n".to_string()));
}

/// A mode change forces the header out even though the content compares equal, and
/// keeps the pair in the diffstat with a zero count.
#[test]
fn a_mode_change_still_shows_its_header_and_stat_row() {
    let f = Fixture::whitespace_only_edit("mode");
    let path = f.work.join("f.txt");
    let mut perm = std::fs::metadata(&path).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perm.set_mode(0o755);
    }
    std::fs::set_permissions(&path, perm).unwrap();

    let (code, out) = f.run(&["diff", "-w"]);
    assert_eq!(code, 0);
    assert!(
        out.starts_with("diff --git a/f.txt b/f.txt\nold mode 100644\nnew mode 100755\n"),
        "{out}"
    );
    assert!(!out.contains("@@"), "no hunk may survive `-w`: {out}");
    assert_eq!(f.run(&["diff", "-w", "--numstat"]).1, "0\t0\tf.txt\n");
    assert_eq!(
        f.run(&["diff", "-w", "--shortstat"]).1,
        " 1 file changed, 0 insertions(+), 0 deletions(-)\n"
    );
}

/// Creations, deletions and binary pairs are unaffected: their headers are forced.
#[test]
fn creations_deletions_and_binaries_are_unaffected() {
    let f = Fixture::new("forced");
    f.write("gone.txt", b"a\nb\n");
    f.write("bin", b"\x00\x01binary\n");
    f.git(&["add", "gone.txt", "bin"]);
    f.git(&["commit", "-q", "-m", "init"]);
    std::fs::remove_file(f.work.join("gone.txt")).unwrap();
    f.write("bin", b"\x00\x02binary\n");
    f.write("new.txt", b"n\n");
    f.git(&["add", "-A"]);

    let (code, out) = f.run(&["diff", "-w", "--cached"]);
    assert_eq!(code, 0);
    assert!(out.contains("deleted file mode 100644\n"), "{out}");
    assert!(out.contains("new file mode 100644\n"), "{out}");
    assert!(out.contains("Binary files a/bin and b/bin differ\n"), "{out}");
}
