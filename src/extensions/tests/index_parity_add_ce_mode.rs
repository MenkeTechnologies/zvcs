//! Which mode `git add` records when the filesystem cannot be trusted for it.
//!
//! `add_to_index()` settles an entry's mode before it reads a single byte:
//!
//! ```c
//! if (trust_executable_bit && has_symlinks) {
//!         ce->ce_mode = create_ce_mode(st_mode);
//! } else {
//!         /* If there is an existing entry, pick the mode bits and type
//!          * from it, otherwise assume unexecutable regular file.
//!          */
//!         struct cache_entry *ent;
//!         int pos = index_name_pos_also_unmerged(istate, path, namelen);
//!
//!         ent = (0 <= pos) ? istate->cache[pos] : NULL;
//!         ce->ce_mode = ce_mode_from_stat(ent, st_mode);
//! }
//! ```
//!
//! (read-cache.c:745-756, with `ce_mode_from_stat()` at read-cache.h:8-21 and
//! `index_name_pos_also_unmerged()` at read-cache.c:670-689.) Three rules follow,
//! and the port had none of them:
//!
//!   * `core.fileMode=0` with no entry to consult records `100644`, whatever the
//!     filesystem's executable bit says — `create_ce_mode(0666)`.
//!   * `core.fileMode=0` with an entry records *that entry's* `100644`/`100755`,
//!     so a restage cannot flip a recorded mode the filesystem never carried.
//!   * `core.symlinks=0` with a symlink entry records `120000` for a path the
//!     worktree now holds as an ordinary file — that is how a tracked symlink
//!     survives a checkout onto a filesystem without symlinks.
//!
//! The lookup is unmerged-aware and prefers stage 2 over stage 1, so a conflict
//! resolved by writing the file and running `git add` keeps "our" mode.
//!
//! Separately, the port's staging walk decided the mode correctly and then threw
//! the answer away: the pass that writes the blobs re-`stat`ed each path and
//! overwrote `mode` with what the filesystem said, which put the executable bit
//! straight back on. The blob write only ever supplies the object id.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
        let root =
            std::env::temp_dir().join(format!("zvcs-idx-ce-mode-{tag}-{}", std::process::id()));
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
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `git update-index --index-info` fed `spec`, which is how a conflicted
    /// index is built without running a merge.
    fn index_info(&self, spec: &str) {
        let mut child = self
            .cmd(&["update-index", "--index-info"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(spec.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "update-index --index-info failed: {out:?}");
    }

    fn write_exec(&self, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = self.work.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The `<mode> <oid> <stage>\t<path>` line `ls-files --stage` prints, with
    /// the object id blanked so the assertions read as modes.
    fn stage_line(&self, path: &str) -> String {
        let out = self.stdout(&["ls-files", "--stage", path]);
        out.lines()
            .map(|l| {
                let mut f = l.splitn(3, ' ');
                let mode = f.next().unwrap_or("");
                let _oid = f.next();
                let rest = f.next().unwrap_or("");
                format!("{mode} {rest}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn blob(&self, body: &str) -> String {
        let mut child = self
            .cmd(&["hash-object", "-w", "-t", "blob", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(body.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

/// `core.fileMode=0`, nothing in the index: `create_ce_mode(0666)`, so an
/// executable file is recorded `100644`. This is t3700-add.sh's case 7.
#[test]
fn index_parity_add_filemode_off_records_100644_without_an_entry() {
    let f = Fixture::new("nofm");
    f.git(&["config", "core.filemode", "0"]);
    f.write_exec("xfoo1", "foo\n");
    f.git(&["add", "xfoo1"]);
    assert_eq!(f.stage_line("xfoo1"), "100644 0\txfoo1");

    // `-N` runs the same mode block (it is above the `intent_only` split), so an
    // intent-to-add stub answers identically.
    f.write_exec("xfoo2", "foo\n");
    f.git(&["add", "-N", "xfoo2"]);
    assert_eq!(f.stage_line("xfoo2"), "100644 0\txfoo2");
}

/// With `core.fileMode=1` the stat is authoritative again, so the same file
/// records `100755`. Without this the fix above would just be "always 100644".
#[test]
fn index_parity_add_filemode_on_still_records_the_executable_bit() {
    let f = Fixture::new("fm");
    f.git(&["config", "core.filemode", "1"]);
    f.write_exec("xfoo1", "foo\n");
    f.git(&["add", "xfoo1"]);
    assert_eq!(f.stage_line("xfoo1"), "100755 0\txfoo1");
}

/// `core.fileMode=0` with an entry present: the recorded mode wins, so a
/// restage of a tracked `100755` keeps `100755` even though the worktree copy
/// lost its bit, and a restage of a tracked `100644` stays `100644` even though
/// the worktree copy gained one.
#[test]
fn index_parity_add_filemode_off_keeps_the_recorded_mode_on_restage() {
    let f = Fixture::new("keep");
    f.write_exec("keepx", "one\n");
    std::fs::write(f.work.join("keepnox"), "one\n").unwrap();
    f.git(&["add", "keepx", "keepnox"]);
    assert_eq!(f.stage_line("keepx"), "100755 0\tkeepx");
    assert_eq!(f.stage_line("keepnox"), "100644 0\tkeepnox");

    f.git(&["config", "core.filemode", "0"]);
    // Both worktree modes are now the opposite of what the index holds.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            f.work.join("keepx"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        std::fs::set_permissions(
            f.work.join("keepnox"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    std::fs::write(f.work.join("keepx"), "two\n").unwrap();
    std::fs::write(f.work.join("keepnox"), "two\n").unwrap();
    f.git(&["add", "keepx", "keepnox"]);
    assert_eq!(f.stage_line("keepx"), "100755 0\tkeepx");
    assert_eq!(f.stage_line("keepnox"), "100644 0\tkeepnox");
}

/// The unmerged arm: with `core.fileMode=0` and `core.symlinks=0` the mode comes
/// off the conflicted entry, and a stage 1 followed by a stage 2 yields the
/// stage 2 — "order of preference: stage 2, 1, 3". t3700-add.sh's cases 22-23.
#[test]
fn index_parity_add_takes_the_mode_from_the_preferred_unmerged_stage() {
    let f = Fixture::new("unmerged");
    let (s1, s2) = (f.blob("1\n"), f.blob("2\n"));
    let (l1, l2) = (f.blob("1"), f.blob("2"));
    f.index_info(&format!(
        "100644 {s1} 1\tfile\n100755 {s2} 2\tfile\n100644 {l1} 1\tsymlink\n120000 {l2} 2\tsymlink\n"
    ));
    f.git(&["config", "core.filemode", "0"]);
    f.git(&["config", "core.symlinks", "0"]);
    std::fs::write(f.work.join("file"), "new\n").unwrap();
    std::fs::write(f.work.join("symlink"), "new\n").unwrap();
    f.git(&["add", "file", "symlink"]);

    // Stage 2's 100755 beats stage 1's 100644 for `file`; for `symlink` the
    // `!has_symlinks` guard fires on stage 2's 120000.
    assert_eq!(f.stage_line("file"), "100755 0\tfile");
    assert_eq!(f.stage_line("symlink"), "120000 0\tsymlink");
}

/// Only stages 1 and 3 exist, so the first entry under the name is the one
/// consulted — the stage-2 preference must not reach past a missing stage 2.
#[test]
fn index_parity_add_uses_the_first_stage_when_stage_two_is_absent() {
    let f = Fixture::new("nostage2");
    let (s1, s3) = (f.blob("1\n"), f.blob("3\n"));
    f.index_info(&format!("100755 {s1} 1\tfile\n100644 {s3} 3\tfile\n"));
    f.git(&["config", "core.filemode", "0"]);
    std::fs::write(f.work.join("file"), "new\n").unwrap();
    f.git(&["add", "file"]);
    assert_eq!(f.stage_line("file"), "100755 0\tfile");
}
