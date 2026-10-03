//! gix opening a repository that stores its references in reftables
//! (`extensions.refStorage = reftable`): the ref store is rooted at the git
//! directory and carries the reftable backend, so `git_dir()`/`common_dir()`
//! answer what stock git's `rev-parse --git-dir --git-common-dir` answers, in the
//! main worktree and in a linked one; and the backend's write options are
//! produced from the repository's configuration with git 2.56's checks and
//! messages (`reftable_be_config()`, refs/reftable-backend.c:323-359).
//!
//! The repositories are built by stock git, and every expected refusal is what
//! stock git prints when an `update-ref` makes it read the same configuration.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gix::refs::store::RefStorage;

struct Fixture {
    root: PathBuf,
    stock: &'static str,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `R` (reftable, two commits, branch `side`, annotated tag `v1`) and the
    /// linked worktree `wt` on `side`, or `None` without a stock git that has
    /// the reftable backend.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 45, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-reftable-gix-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, stock };
        for args in [
            &["init", "-q", "-b", "main", "--ref-format=reftable", "R"][..],
            &["-C", "R", "commit", "-q", "--allow-empty", "-m", "one"],
            &["-C", "R", "commit", "-q", "--allow-empty", "-m", "two"],
            &["-C", "R", "branch", "side"],
            &["-C", "R", "tag", "-a", "v1", "-m", "t"],
            &["-C", "R", "worktree", "add", "-q", "../wt", "side"],
        ] {
            let out = f.stock(args);
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        Some(f)
    }

    fn stock(&self, args: &[&str]) -> Output {
        Command::new(self.stock)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_TEST_REFTABLE_AUTOCOMPACTION")
            .env("LC_ALL", "C")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .output()
            .unwrap()
    }

    fn path(&self, dir: &str) -> PathBuf {
        self.root.join(dir)
    }

    /// Stock's absolute git directory and common directory for `dir`.
    fn stock_dirs(&self, dir: &str) -> (PathBuf, PathBuf) {
        let out = self.stock(&[
            "-C",
            dir,
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ]);
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        let mut lines = text.lines().map(canonical);
        (lines.next().unwrap(), lines.next().unwrap())
    }

    /// The first line stock prints on stderr for `args` run in `dir`, which must
    /// be a `fatal:` refusal, without that prefix.
    fn stock_fatal(&self, dir: &str, args: &[&str]) -> String {
        let mut all = vec!["-C", dir];
        all.extend_from_slice(args);
        let out = self.stock(&all);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        let err = String::from_utf8(out.stderr).unwrap();
        err.strip_prefix("fatal: ")
            .unwrap_or_else(|| panic!("not a fatal: {err}"))
            .trim_end()
            .to_owned()
    }
}

fn canonical(p: impl AsRef<Path>) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

fn open(dir: &Path, cli: &[&str]) -> gix::Repository {
    gix::open_opts(dir, gix::open::Options::isolated().cli_overrides(cli.iter().copied())).unwrap()
}

#[test]
fn main_worktree_is_rooted_at_the_git_directory() {
    let Some(f) = Fixture::new("main") else { return };
    let repo = open(&f.path("R"), &[]);
    let (git_dir, common_dir) = f.stock_dirs("R");

    assert_eq!(repo.ref_storage(), RefStorage::Reftable);
    assert_eq!(canonical(repo.git_dir()), git_dir);
    assert_eq!(canonical(repo.common_dir()), common_dir);
    assert_eq!(canonical(repo.index_path()), git_dir.join("index"));
    assert_eq!(repo.kind(), gix::repository::Kind::Common);

    let backend = repo.refs.reftable().expect("a reftable store carries the backend");
    assert_eq!(canonical(backend.git_dir()), git_dir);
    assert_eq!(canonical(backend.common_dir()), common_dir);
    backend.check().expect("the main stack opened");
    assert!(backend.worktree_stack().is_none(), "the main worktree has no stack of its own");

    // HEAD comes from the main stack, not from the `refs/heads/.invalid` stub file.
    let head = f.stock(&["-C", "R", "rev-parse", "HEAD"]);
    assert_eq!(repo.head_name().unwrap().unwrap().as_bstr(), "refs/heads/main");
    assert_eq!(repo.head_id().unwrap().to_string(), String::from_utf8(head.stdout).unwrap().trim());
}

#[test]
fn linked_worktree_keeps_its_own_git_directory() {
    let Some(f) = Fixture::new("linked") else { return };
    let repo = open(&f.path("wt"), &[]);
    let (git_dir, common_dir) = f.stock_dirs("wt");
    assert_eq!(git_dir, canonical(f.path("R/.git/worktrees/wt")));

    assert_eq!(repo.ref_storage(), RefStorage::Reftable);
    assert_eq!(canonical(repo.git_dir()), git_dir);
    assert_eq!(canonical(repo.common_dir()), common_dir);
    assert_eq!(canonical(repo.index_path()), git_dir.join("index"));
    assert_eq!(repo.kind(), gix::repository::Kind::LinkedWorkTree);

    let backend = repo.refs.reftable().unwrap();
    assert_eq!(canonical(backend.git_dir()), git_dir);
    assert_eq!(canonical(backend.common_dir()), common_dir);
    backend.check().expect("both stacks opened");
    assert!(backend.worktree_stack().is_some(), "<git dir>/reftable is opened");

    // The worktree's HEAD comes from its own stack.
    let head = f.stock(&["-C", "wt", "rev-parse", "HEAD"]);
    assert_eq!(repo.head_name().unwrap().unwrap().as_bstr(), "refs/heads/side");
    assert_eq!(repo.head_id().unwrap().to_string(), String::from_utf8(head.stdout).unwrap().trim());
}

#[test]
fn files_repositories_carry_no_backend() {
    let Some(f) = Fixture::new("files") else { return };
    let out = f.stock(&["init", "-q", "F"]);
    assert!(out.status.success());
    let repo = open(&f.path("F"), &[]);
    assert_eq!(repo.ref_storage(), RefStorage::Files);
    assert!(repo.refs.reftable().is_none());
    assert_eq!(canonical(repo.git_dir()), canonical(f.path("F/.git")));
    assert!(matches!(
        repo.reftable_optimize(false),
        Err(gix::repository::reftable::Error::NotReftable)
    ));
}

/// The die hook of this test binary: unwind with the message instead of exiting.
fn die_by_panic(message: &str) -> ! {
    std::panic::panic_any(message.to_owned())
}

/// The backend's write options, or the message git would die with.
fn write_config(repo: &gix::Repository) -> Result<gix::refs::reftable::WriteConfig, String> {
    gix::config::reftable::set_die_hook(die_by_panic);
    let backend = repo.refs.reftable().unwrap();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| backend.write_config().clone())).map_err(|payload| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "not a die".into())
    })
}

#[test]
fn invalid_write_options_are_refused_with_gits_messages() {
    let Some(f) = Fixture::new("refuse") else { return };
    let update = ["update-ref", "refs/heads/x", "HEAD"];
    for kv in [
        "reftable.blockSize=20000000",
        "reftable.blockSize=abc",
        "reftable.blockSize=99999999999999999999",
        "reftable.restartInterval=65536",
        "reftable.geometricFactor=256",
        "reftable.lockTimeout=-2",
        "reftable.indexObjects=bogus",
        "core.logAllRefUpdates=bogus",
        "core.sharedRepository=0444",
    ] {
        let mut args = vec!["-c", kv];
        args.extend_from_slice(&update);
        let expected = f.stock_fatal("R", &args);
        assert_eq!(write_config(&open(&f.path("R"), &[kv])).err(), Some(expected), "{kv}");
    }

    // In the linked worktree too, and the first bad value wins even when a
    // later one would be valid.
    let expected = f.stock_fatal(
        "wt",
        &["-c", "reftable.blockSize=abc", "-c", "reftable.blockSize=4096", "update-ref", "refs/heads/x", "HEAD"],
    );
    let repo = open(&f.path("wt"), &["reftable.blockSize=abc", "reftable.blockSize=4096"]);
    assert_eq!(write_config(&repo).err(), Some(expected));
}

#[test]
fn a_refusal_names_the_file_the_value_came_from() {
    let Some(f) = Fixture::new("file") else { return };
    let out = f.stock(&["-C", "R", "config", "reftable.blockSize", "12abc"]);
    assert!(out.status.success());
    let git_dir = canonical(f.path("R/.git"));
    let git_dir_arg = format!("--git-dir={}", git_dir.display());
    let expected = f.stock_fatal(".", &[&git_dir_arg, "update-ref", "refs/heads/x", "HEAD"]);
    assert!(expected.contains(" in file /"), "{expected}");
    assert_eq!(write_config(&open(&git_dir, &[])).err(), Some(expected));
}

#[test]
fn valid_write_options_reach_the_backend() {
    let Some(f) = Fixture::new("accept") else { return };
    let repo = open(
        &f.path("R"),
        &[
            "reftable.blockSize=1k",
            "reftable.restartInterval=65535",
            "reftable.geometricFactor=255",
            "reftable.lockTimeout=-1",
            "reftable.indexObjects=false",
            "core.logAllRefUpdates=always",
            "core.sharedRepository=0640",
        ],
    );
    let config = write_config(&repo).unwrap();
    assert_eq!(config.opts.block_size, 1024);
    assert_eq!(config.opts.restart_interval, 65535);
    assert_eq!(config.opts.auto_compaction_factor, 255);
    assert_eq!(config.opts.lock_timeout_ms, -1);
    assert!(config.opts.skip_index_objects);
    assert_eq!(config.log_all_ref_updates, Some(gix::refs::store::WriteReflog::Always));
    // Stock 2.56.0 writes its tables and `tables.list` with mode 0640 under this setting.
    assert_eq!(config.opts.default_permissions, Some(0o640));

    // Without `reftable.*` configuration: git's defaults; `git init` wrote
    // `core.logAllRefUpdates = true` into the non-bare repository.
    let config = write_config(&open(&f.path("R"), &[])).unwrap();
    assert_eq!(config.opts.block_size, 4096);
    assert_eq!(config.opts.lock_timeout_ms, 100);
    assert_eq!(config.log_all_ref_updates, Some(gix::refs::store::WriteReflog::Normal));
}
