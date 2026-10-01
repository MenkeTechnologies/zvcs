//! `init --ref-format=reftable` lays down a reftable repository.
//!
//! `initialize_repository_version()` (setup.c:2444-2490) writes
//! `extensions.refstorage = reftable` after any object format, with the
//! version-1 bump; `create_reference_database()` (setup.c:2527-2563) runs
//! `ref_store_create_on_disk()` — `reftable/` (refs/reftable-backend.c:497-511)
//! plus the `refs_create_refdir_stubs()` `HEAD` and `refs/heads` stubs
//! (refs.c:2202-2223) — and then `refs_update_symref(HEAD → refs/heads/<b>)`,
//! which is the stack's first table.
//!
//! zvcs refused the format outright. Expectations measured from stock git
//! 2.56.0; stock also reads the repository zvcs writes.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-init-reftable-format-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root: root.canonicalize().unwrap() }
    }
}

fn run(bin: &str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .envs(env.iter().copied())
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DEFAULT_REF_FORMAT")
        .env_remove("GIT_DEFAULT_HASH")
        .env("LC_ALL", "C")
        .output()
        .unwrap_or_else(|e| panic!("{bin} {args:?}: {e}"));
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The one table `tables.list` names, checked for the name shape
/// `<min>-<max>-<random>.ref` with both update indices 1.
fn assert_single_first_table(git_dir: &Path) {
    let list = read(&git_dir.join("reftable/tables.list"));
    let names: Vec<&str> = list.lines().collect();
    assert_eq!(names.len(), 1, "{list:?}");
    let name = names[0];
    assert!(
        name.starts_with("0x000000000001-0x000000000001-") && name.ends_with(".ref"),
        "{name}"
    );
    assert!(git_dir.join("reftable").join(name).is_file());
}

#[test]
fn a_new_repository_gets_the_reftable_layout() {
    let f = Fixture::new("plain");
    let got = run(BIN, &f.root, &["init", "-q", "--ref-format=reftable", "-b", "topic", "sub"], &[]);
    assert_eq!((got.1.as_str(), got.2), ("", 0));
    let git_dir = f.root.join("sub/.git");
    assert_eq!(read(&git_dir.join("HEAD")), "ref: refs/heads/.invalid\n");
    assert_eq!(read(&git_dir.join("refs/heads")), "this repository uses the reftable format\n");
    assert!(!git_dir.join("refs/tags").exists());
    assert!(read(&git_dir.join("config"))
        .starts_with("[extensions]\n\trefstorage = reftable\n[core]\n\trepositoryformatversion = 1\n"));
    assert_single_first_table(&git_dir);

    let sub = f.root.join("sub");
    if let Some(stock) = stock_git() {
        assert_eq!(run(stock, &sub, &["symbolic-ref", "HEAD"], &[]).0, "refs/heads/topic\n");
        assert_eq!(run(stock, &sub, &["rev-parse", "--show-ref-format"], &[]).0, "reftable\n");
        assert_eq!(run(stock, &sub, &["for-each-ref"], &[]).0, "");
        assert_eq!(run(stock, &sub, &["fsck", "--no-progress"], &[]).2, 0);
    }
}

#[test]
fn the_message_names_the_new_git_directory() {
    let f = Fixture::new("message");
    let got = run(BIN, &f.root, &["init", "-b", "main", "--ref-format=reftable", "rtsub"], &[]);
    let want = format!("Initialized empty Git repository in {}/rtsub/.git/\n", f.root.display());
    assert_eq!((got.0.as_str(), got.1.as_str(), got.2), (want.as_str(), "", 0));
}

#[test]
fn sha256_and_reftable_share_the_extensions_section() {
    let f = Fixture::new("sha256");
    let got = run(
        BIN,
        &f.root,
        &["init", "-q", "-b", "main", "--object-format=sha256", "--ref-format=reftable", "sub"],
        &[],
    );
    assert_eq!((got.1.as_str(), got.2), ("", 0));
    let git_dir = f.root.join("sub/.git");
    assert!(read(&git_dir.join("config")).starts_with(
        "[extensions]\n\tobjectformat = sha256\n\trefstorage = reftable\n[core]\n\trepositoryformatversion = 1\n"
    ));
    assert_single_first_table(&git_dir);
    if let Some(stock) = stock_git() {
        assert_eq!(run(stock, &f.root.join("sub"), &["symbolic-ref", "HEAD"], &[]).0, "refs/heads/main\n");
    }
}

#[test]
fn feature_experimental_selects_reftable_unless_a_known_default_precedes_it() {
    let f = Fixture::new("experimental");
    run(BIN, &f.root, &["-c", "feature.experimental=true", "init", "-q", "-b", "main", "a"], &[]);
    assert!(f.root.join("a/.git/reftable").is_dir());

    let got = run(
        BIN,
        &f.root,
        &["-c", "init.defaultRefFormat=bogus", "-c", "feature.experimental=true", "init", "-q", "-b", "main", "b"],
        &[],
    );
    assert_eq!((got.1.as_str(), got.2), ("warning: unknown ref storage format 'bogus'\n", 0));
    assert!(f.root.join("b/.git/reftable").is_dir());

    run(
        BIN,
        &f.root,
        &["-c", "init.defaultRefFormat=files", "-c", "feature.experimental=true", "init", "-q", "-b", "main", "c"],
        &[],
    );
    assert!(f.root.join("c/.git/refs/heads").is_dir());
    assert!(!f.root.join("c/.git/reftable").exists());
}

#[test]
fn an_invalid_initial_branch_leaves_the_stubs_behind() {
    let f = Fixture::new("badname");
    let got = run(BIN, &f.root, &["init", "-q", "--ref-format=reftable", "-b", "a..b", "sub"], &[]);
    assert_eq!((got.1.as_str(), got.2), ("fatal: invalid initial branch name: 'a..b'\n", 128));
    let git_dir = f.root.join("sub/.git");
    assert_eq!(read(&git_dir.join("HEAD")), "ref: refs/heads/.invalid\n");
    assert!(git_dir.join("reftable").is_dir());
    assert!(!git_dir.join("reftable/tables.list").exists());
}
