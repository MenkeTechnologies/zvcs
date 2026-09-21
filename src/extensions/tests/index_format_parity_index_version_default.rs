//! Which version number lands in the `DIRC` header of an index nobody named a
//! version for.
//!
//! ```c
//! if (!istate->version)
//!         istate->version = get_index_format_default(the_repository);
//!
//! /* demote version 3 to version 2 when the latter suffices */
//! if (istate->version == 3 || istate->version == 2)
//!         istate->version = extended ? 3 : 2;
//! ```
//!
//! (read-cache.c:2865-2872.) `istate->version` is zero only for a state that was
//! never read off disk, and `do_read_index()` leaves it that way for a
//! `$GIT_DIR/index` that does not exist yet (read-cache.c:2214-2221). So the
//! version an index is *born* in comes from `GIT_INDEX_VERSION` / `index.version`
//! — and the version a rewrite keeps comes from the header it was read from,
//! whatever the configuration says at that moment.
//!
//! Every expectation below was measured against stock git 2.55.0:
//!
//! ```text
//! $ git -c index.version=4 add a        # in a repository with no index
//! $ xxd -s4 -l4 -p .git/index
//! 00000004
//! $ git -c index.version=4 add b        # the index now exists, at version 2
//! $ xxd -s4 -l4 -p .git/index
//! 00000002
//! ```
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
        let root = std::env::temp_dir().join(format!("zvcs-idxver-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&[], &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), b"a\n").unwrap();
        std::fs::write(f.work.join("b"), b"b\n").unwrap();
        assert!(!f.work.join(".git/index").exists(), "fixture must start index-less");
        f
    }

    fn cmd(&self, env: &[(&str, &str)], args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env_remove("GIT_INDEX_VERSION")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for (k, v) in env {
            c.env(k, v);
        }
        c
    }

    fn ok(&self, env: &[(&str, &str)], args: &[&str]) -> String {
        let out = self.cmd(env, args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    /// The version word of the `DIRC` header: bytes 4..8, big-endian.
    fn header_version(&self) -> u32 {
        let bytes = std::fs::read(self.work.join(".git/index")).expect("an index was written");
        assert_eq!(&bytes[..4], b"DIRC", "not an index file");
        u32::from_be_bytes(bytes[4..8].try_into().unwrap())
    }
}

#[test]
fn index_version_config_decides_the_version_of_a_brand_new_index() {
    let f = Fixture::new("cfg4");
    f.ok(&[], &["-c", "index.version=4", "add", "a"]);
    assert_eq!(f.header_version(), 4, "`index.version=4` must reach a fresh index");
}

#[test]
fn git_index_version_env_decides_the_version_of_a_brand_new_index() {
    let f = Fixture::new("env4");
    f.ok(&[("GIT_INDEX_VERSION", "4")], &["add", "a"]);
    assert_eq!(f.header_version(), 4, "`GIT_INDEX_VERSION=4` must reach a fresh index");
}

/// The other half of the same rule, and the one that keeps a repository stable:
/// an index that already exists is rewritten in *its own* version. git only
/// consults the configuration for `!istate->version`.
#[test]
fn an_existing_index_keeps_its_own_version_whatever_the_configuration_says() {
    let f = Fixture::new("keep2");
    f.ok(&[], &["add", "a"]);
    assert_eq!(f.header_version(), 2, "the default for an unconfigured repository");

    f.ok(&[], &["-c", "index.version=4", "add", "b"]);
    assert_eq!(
        f.header_version(),
        2,
        "a version 2 index stays version 2; `index.version` is not a conversion request"
    );
}

/// `INDEX_FORMAT_DEFAULT` is 3 (read-cache.h:11) and an out-of-range request lands
/// on it with a warning — which the writer then demotes to 2, because no entry
/// here needs the extended flag word.
#[test]
fn an_out_of_range_index_version_warns_and_falls_back_to_the_default() {
    let f = Fixture::new("bad99");
    let stderr = f.ok(&[], &["-c", "index.version=99", "add", "a"]);
    assert!(
        stderr.contains("index.version set, but the value is invalid."),
        "expected git's warning, got {stderr:?}"
    );
    assert_eq!(
        f.header_version(),
        2,
        "version 3 with no extended entry is demoted to 2"
    );
}

#[test]
fn an_out_of_range_env_version_warns_and_falls_back_to_the_default() {
    let f = Fixture::new("badenv");
    let stderr = f.ok(&[("GIT_INDEX_VERSION", "bogus")], &["add", "a"]);
    assert!(
        stderr.contains("GIT_INDEX_VERSION set, but the value is invalid."),
        "expected git's warning, got {stderr:?}"
    );
    assert_eq!(f.header_version(), 2);
}

/// `index.version=3` is a request the writer is free to demote, and does: the
/// demotion at read-cache.c:2871 is on the *written* version, not on where the
/// request came from.
#[test]
fn version_three_is_demoted_when_no_entry_needs_the_extended_flags() {
    let f = Fixture::new("cfg3");
    f.ok(&[], &["-c", "index.version=3", "add", "a"]);
    assert_eq!(f.header_version(), 2);
}

/// …and promoted back the moment one does — `--skip-worktree` sets
/// `CE_SKIP_WORKTREE`, which only the version 3 flag word can carry.
#[test]
fn a_skip_worktree_entry_forces_version_three() {
    let f = Fixture::new("promote3");
    f.ok(&[], &["add", "a"]);
    assert_eq!(f.header_version(), 2);
    f.ok(&[], &["update-index", "--skip-worktree", "a"]);
    assert_eq!(f.header_version(), 3);
}
