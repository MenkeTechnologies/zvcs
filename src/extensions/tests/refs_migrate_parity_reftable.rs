//! `git refs migrate --ref-format=reftable` on a files repository
//! (`repo_migrate_ref_storage_format()`, refs.c:3344-3542, v2.56.0).
//!
//! The table is compared byte for byte with the one stock git 2.56.0 wrote for
//! this exact fixture: every reference at update index 1 (the root ref
//! `ORIG_HEAD`, the symref `HEAD`, branches and a tag), and each reflog entry at
//! its own index, so the limits run 1..=5. Only the table's file name differs
//! between runs — its last eight hex digits are random.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Stock 2.56.0's table with the reflogs.
const TABLE: &str = "524546540100100000000000000000010000000000000005720000bd002348454144000f726566732f68656164732f6d61696e00494f5249475f484541440057ef6b409b72f867c799347c562551b1a76536d60079726566732f68656164732f6d61696e00635eb4d81d8cc414e6b86e5ff6d78a6d22d97f780b21736964650057ef6b409b72f867c799347c562551b1a76536d60539746167732f76310057ef6b409b72f867c799347c562551b1a76536d600001c0000330000530003670001b978da63c8f470757461f80f017fc3df673bcc2efa917e7ca6494d986ae0c6e5a966d792e3b6dc90ed3922f26c475efcb7eb5db94a37eb2b181d99131d2a5a579e7bc8c0c0c0939c9f9b9b5962a550529ecfc5c3f98f010bc066308a21621043143432f3324b32137334ad14f2f352b9181a1c8b52d38af5335213538af5731333f3606efd4db95bc539ff50d3addc99c59929a930f7fd22cb6899a4a2c4bce40c2b05e7a2d4c492d41485b4a2fc5c05501cd519723130b030302c616002002bb3a31452454654010010000000000000000001000000000000000500000000000000000000000000000000000000000000000000000000000000bd000000000000000019289e46";

/// Stock 2.56.0's table under `--no-reflog`.
const TABLE_NO_REFLOG: &str = "524546540100100000000000000000010000000000000001720000bd002348454144000f726566732f68656164732f6d61696e00494f5249475f484541440057ef6b409b72f867c799347c562551b1a76536d60079726566732f68656164732f6d61696e00635eb4d81d8cc414e6b86e5ff6d78a6d22d97f780b21736964650057ef6b409b72f867c799347c562551b1a76536d60539746167732f76310057ef6b409b72f867c799347c562551b1a76536d600001c000033000053000352454654010010000000000000000001000000000000000100000000000000000000000000000000000000000000000000000000000000000000000000000000b6bff78a";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-refs-migrate-rt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["commit", "-q", "--allow-empty", "-m", "one"],
            &["tag", "v1"],
            &["commit", "-q", "--allow-empty", "-m", "two"],
            &["branch", "side", "HEAD~1"],
            &["update-ref", "ORIG_HEAD", "HEAD~1"],
        ] {
            assert!(f.run(args).status.success(), "{args:?}");
        }
        std::fs::create_dir_all(f.root.join("sub")).unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in("", args)
    }

    fn run_in(&self, dir: &str, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(self.root.join(dir))
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()
            .unwrap()
    }

    /// The single table in `<dir>/reftable`, after checking `tables.list` names it.
    fn table(&self, dir: &str) -> String {
        let reftable = self.root.join(dir).join("reftable");
        let list = std::fs::read_to_string(reftable.join("tables.list")).unwrap();
        let names: Vec<&str> = list.lines().collect();
        assert_eq!(names.len(), 1, "{list}");
        let bytes = std::fs::read(reftable.join(names[0])).unwrap();
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[test]
fn files_to_reftable_writes_git_s_table_and_retires_the_files_store() {
    let f = Fixture::new("full");
    let out = f.run(&["refs", "migrate", "--ref-format=reftable"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, b"");
    assert_eq!(out.stderr, b"");

    assert_eq!(f.table(".git"), TABLE);
    let list = std::fs::read_to_string(f.root.join(".git/reftable/tables.list")).unwrap();
    assert!(list.starts_with("0x000000000001-0x000000000005-") && list.ends_with(".ref\n"), "{list}");

    // The stubs that keep a files-only client away, and nothing of the old store.
    let git = f.root.join(".git");
    assert_eq!(std::fs::read_to_string(git.join("HEAD")).unwrap(), "ref: refs/heads/.invalid\n");
    assert_eq!(
        std::fs::read_to_string(git.join("refs/heads")).unwrap(),
        "this repository uses the reftable format\n"
    );
    assert!(!git.join("logs").exists());
    assert!(!git.join("ORIG_HEAD").exists());
    assert!(!git.join("refs/tags").exists());
    assert!(std::fs::read_dir(&git).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with("ref_migration.")));

    let config = std::fs::read_to_string(git.join("config")).unwrap();
    assert!(config.contains("\trepositoryformatversion = 1\n"), "{config}");
    assert!(config.ends_with("[extensions]\n\trefstorage = reftable\n"), "{config}");
}

#[test]
fn no_reflog_leaves_the_logs_behind() {
    let f = Fixture::new("noreflog");
    let out = f.run(&["refs", "migrate", "--ref-format=reftable", "--no-reflog"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(f.table(".git"), TABLE_NO_REFLOG);
    assert!(!f.root.join(".git/logs").exists());
}

#[test]
fn dry_run_names_the_directory_as_git_spells_the_git_dir() {
    let f = Fixture::new("dry");
    let out = f.run_in("sub", &["refs", "migrate", "--ref-format=reftable", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let dir = stdout
        .strip_prefix("Finished dry-run migration of refs, the result can be found at '")
        .and_then(|s| s.strip_suffix("'\n"))
        .unwrap_or_else(|| panic!("{stdout}"));
    let suffix = dir.strip_prefix(".git/ref_migration.").unwrap_or_else(|| panic!("{dir}"));
    assert_eq!(suffix.len(), 6);
    assert!(suffix.bytes().all(|b| b.is_ascii_alphanumeric()));

    assert_eq!(f.table(dir), TABLE);
    // The repository itself is untouched.
    let git = f.root.join(".git");
    assert_eq!(std::fs::read_to_string(git.join("HEAD")).unwrap(), "ref: refs/heads/main\n");
    assert!(git.join("logs/HEAD").exists());
    assert!(!std::fs::read_to_string(git.join("config")).unwrap().contains("refstorage"));
}
