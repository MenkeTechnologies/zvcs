//! `index.skipHash` reaches the index `git stash apply` writes.
//!
//! git restores that index through `unpack_trees()` and `write_locked_index()`,
//! i.e. the same `do_write_index()` every other index writer goes through, and
//! that is where the setting is read:
//!
//! ```c
//! f = hashfd_check(newfd, tempfile->filename.buf);
//! else
//!         f = hashfd(the_repository->hash_algo, newfd, tempfile->filename.buf);
//! ```
//!
//! (`read-cache.c:2830-2831`, selected by `istate->repo->settings.index_skip_hash`).
//! With the setting on, the twenty trailing bytes of `.git/index` are zeroes
//! instead of the file's own hash — that is the whole point of the option, and a
//! writer that computes the hash anyway silently opts the repository back into the
//! cost the user turned off.
//!
//! `feature.manyFiles` is the macro that turns it on without naming it, so it is
//! checked here too: a repository configured that way must get the same index.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The trailer `hashfd_check()` writes when `index.skipHash` is on.
const ZEROED_TRAILER: [u8; 20] = [0u8; 20];

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
    /// A stash holding one modified tracked file, ready to apply.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-stashskiphash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.work.join("a.txt"), "base\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("a.txt"), "modified\n").unwrap();
        f.git(&["stash", "push", "-q", "-m", "s"]);
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
            .env("GIT_AUTHOR_DATE", "1112904793 +0200")
            .env("GIT_COMMITTER_DATE", "1112904793 +0200")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "`git {args:?}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The last twenty bytes of `.git/index`, which is `do_write_index()`'s trailer.
    fn index_trailer(&self) -> Vec<u8> {
        let bytes = std::fs::read(self.work.join(".git/index")).unwrap();
        assert!(bytes.len() > 20, "index is too short to have a trailer");
        bytes[bytes.len() - 20..].to_vec()
    }
}

#[test]
fn stash_apply_honours_index_skip_hash() {
    let f = Fixture::new("on");
    f.git(&["config", "index.skipHash", "true"]);
    f.git(&["stash", "apply", "--index"]);
    assert_eq!(
        f.index_trailer(),
        ZEROED_TRAILER,
        "index.skipHash=true must leave the trailer zeroed"
    );
}

#[test]
fn stash_apply_still_hashes_when_the_setting_is_off() {
    let f = Fixture::new("off");
    f.git(&["config", "index.skipHash", "false"]);
    f.git(&["stash", "apply", "--index"]);
    assert_ne!(
        f.index_trailer(),
        ZEROED_TRAILER,
        "the default writer must still hash the index it wrote"
    );
}

/// `feature.manyFiles` is a macro over `index.version`, `index.skipHash` and
/// `core.untrackedCache`, so it turns the trailer off without naming the setting.
#[test]
fn feature_many_files_reaches_the_stash_index_too() {
    let f = Fixture::new("many");
    f.git(&["config", "feature.manyFiles", "true"]);
    f.git(&["stash", "apply", "--index"]);
    assert_eq!(f.index_trailer(), ZEROED_TRAILER);

    // An explicit `index.skipHash=false` beats the macro, which is the whole
    // reason the macro is a default rather than an override.
    let f = Fixture::new("many-override");
    f.git(&["config", "feature.manyFiles", "true"]);
    f.git(&["config", "index.skipHash", "false"]);
    f.git(&["stash", "apply", "--index"]);
    assert_ne!(f.index_trailer(), ZEROED_TRAILER);
}
