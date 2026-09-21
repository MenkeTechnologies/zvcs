//! `git update-server-info` and `core.sharedRepository`.
//!
//! `update_info_file()` writes each replacement into a temp file created at 0666 (so the
//! umask applies), then widens it before renaming it into place:
//!
//! ```c
//! if (adjust_shared_perm(r, get_tempfile_path(f)) < 0)
//!         goto out;
//! if (rename_tempfile(&f, path) < 0)
//!         goto out;
//! ```
//!
//! (server-info.c:80-84.) `calc_shared_perm()` (path.c) then decides the mode from the
//! parsed `core.sharedRepository`: an explicit `0<nnn>` filemode is stored negated and
//! *forces* the low nine bits, while `group`/`all` are stored positive and only OR bits
//! in. Either way the write bits are dropped when the umask already removed the owner's
//! (`if (mode & S_IWUSR) == 0 → tweak &= ~0222`), which is what makes the same
//! configuration produce a read-only file under `umask 0277` and a writable one under
//! `umask 077`.
//!
//! Every expectation was measured from stock git 2.55.0.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A repository with one commit, so `info/refs` has a line to write.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-cfg-usi-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("a1"), "").unwrap();
        f.git(&["add", "a1"]);
        f.git(&["commit", "-q", "-m", "a1"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env_remove("GIT_CONFIG")
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
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// Run `git update-server-info` under `umask`, and return `info/refs`'s mode bits.
    ///
    /// The umask is process-wide, so the child does it for itself through a shell rather
    /// than the test process changing its own and racing every other test.
    fn refs_mode(&self, umask: &str) -> u32 {
        let refs = self.root.join(".git/info/refs");
        let _ = std::fs::remove_file(&refs);
        let script = format!("umask {umask} && exec \"$1\" update-server-info");
        let out = Command::new("/bin/sh")
            .args(["-c", &script, "sh", BIN])
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(out.status.success(), "update-server-info failed: {out:?}");
        std::fs::metadata(&refs).unwrap().permissions().mode() & 0o7777
    }
}

/// An explicit `0<nnn>` filemode forces the low nine bits — except for the write bits,
/// which the umask can still take away.
#[test]
fn an_explicit_filemode_forces_the_mode() {
    let f = Fixture::new("filemode");

    for (shared, ro, rw) in [(0o660, 0o440, 0o660), (0o640, 0o440, 0o640), (0o666, 0o444, 0o666)] {
        f.git(&["config", "core.sharedrepository", &format!("0{shared:o}")]);
        assert_eq!(f.refs_mode("0277"), ro, "0{shared:o} under umask 0277");
        assert_eq!(f.refs_mode("077"), rw, "0{shared:o} under umask 077");
    }
}

/// `all` is stored positive, so it ORs its bits into what the umask left.
#[test]
fn the_word_forms_widen_rather_than_force() {
    let f = Fixture::new("word");
    f.git(&["config", "core.sharedrepository", "all"]);
    assert_eq!(f.refs_mode("0277"), 0o444);

    f.git(&["config", "core.sharedrepository", "group"]);
    assert_eq!(f.refs_mode("0277"), 0o440);
}

/// With no sharing configured the file is the umask's alone.
#[test]
fn an_unshared_repository_keeps_the_umask() {
    let f = Fixture::new("plain");
    assert_eq!(f.refs_mode("002"), 0o664);
    assert_eq!(f.refs_mode("0277"), 0o400);

    // `umask` and `0` are the spellings of "not shared", not a mode to force.
    f.git(&["config", "core.sharedrepository", "umask"]);
    assert_eq!(f.refs_mode("002"), 0o664);
}
