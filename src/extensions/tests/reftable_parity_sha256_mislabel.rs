//! A SHA-1 repository on the reftable backend whose config declares
//! `extensions.objectFormat = sha256`. Two things go wrong in git at once:
//!
//! * The reftable stack will not open: its tables are version 1, implicitly SHA-1, and
//!   `reftable_merged_table_new()` refuses a table of another hash (reftable/merged.c:206),
//!   so `refs->err` is set and every ref read reports it (refs/reftable-backend.c:445-447).
//!   No ref resolves, and the verbs that iterate refs simply see none.
//! * The index is decoded with 32-byte object ids, so `create_from_disk()` finds an entry's
//!   flags twelve bytes late (read-cache.c:1805). After `README.md`'s 72-byte entry that is
//!   the next entry's ctime, whose upper half carries `CE_EXTENDED`, and git dies on the
//!   extended word with `unknown index entry format 0x%08x` (:1826). With
//!   `verify_index_checksum` on, `fsck` stops earlier, at the trailer (`verify_hdr()`, :1742).
//!
//! zvcs stopped at the refs instead — `The reference 'HEAD' did not exist` from `fetch`,
//! `pull` and `status`, `corrupt reftable file` from `gc` and `fsck`, all at exit 1 — and
//! never reached the index. Both sides run on byte-identical copies of one fixture, since
//! the format word is the fixture's own ctime.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git_at_least;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_DEFAULT_REF_FORMAT", "reftable")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1112911993 -0700")
        .env("GIT_COMMITTER_DATE", "1112911993 -0700")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A clone, one commit behind its origin, then relabelled SHA-256. Built by `stock`
/// so the index bytes are git's own.
fn template(stock: &str, root: &Path) -> PathBuf {
    let origin = root.join("origin");
    std::fs::create_dir_all(origin.join("src")).unwrap();
    run(stock, &origin, &["init", "-q", "-b", "main"]);
    std::fs::write(origin.join("README.md"), "# fixture\n").unwrap();
    std::fs::write(origin.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\n").unwrap();
    run(stock, &origin, &["add", "."]);
    run(stock, &origin, &["commit", "-q", "-m", "initial"]);
    run(stock, root, &["clone", "-q", "origin", "repo"]);
    std::fs::write(origin.join("README.md"), "# fixture\nahead\n").unwrap();
    run(stock, &origin, &["commit", "-q", "-am", "ahead"]);
    let repo = root.join("repo");
    run(stock, &repo, &["config", "core.repositoryFormatVersion", "1"]);
    run(stock, &repo, &["config", "extensions.objectFormat", "sha256"]);
    repo
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Run `args` with both binaries, each on its own copy of the template. The clone's
/// remote is the template's `origin` by absolute path, which neither side writes to.
fn both(stock: &str, root: &Path, repo: &Path, args: &[&str]) -> [(i32, String, String); 2] {
    [stock, ZVCS].map(|bin| {
        let side = root.join(format!("run-{}", if bin == stock { "stock" } else { "zvcs" }));
        let _ = std::fs::remove_dir_all(&side);
        copy_tree(repo, &side.join("repo"));
        let result = run(bin, &side.join("repo"), args);
        let _ = std::fs::remove_dir_all(&side);
        result
    })
}

fn scratch(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-reftable-sha256-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(&root).unwrap()
}

#[test]
fn the_index_entry_format_die_is_reached_past_the_unopenable_stack() {
    let Some(stock) = stock_git_at_least((2, 45, 0)) else { return };
    let root = scratch("die");
    let repo = template(stock, &root);
    for args in [
        &["status", "--porcelain"][..],
        &["fetch", "origin"],
        &["fetch", "--dry-run", "origin"],
        &["pull", "origin", "main"],
        &["gc", "--quiet"],
    ] {
        let [stock_side, zvcs_side] = both(stock, &root, &repo, args);
        assert!(
            stock_side.2.contains("fatal: unknown index entry format 0x"),
            "git {args:?}: the fixture must reach git's index die, got {stock_side:?}"
        );
        assert_eq!(zvcs_side, stock_side, "git {args:?}: left is zvcs, right is stock");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// `fsck` checks the trailer first and dies on it. Its first line is left out: stock's
/// `git refs verify` child dereferences the NULL stack in `reftable_be_fsck()`
/// (refs/reftable-backend.c:2792) and fsck reports `error: refs died of signal 11`.
#[test]
fn fsck_dies_on_the_index_trailer_after_finding_no_refs() {
    let Some(stock) = stock_git_at_least((2, 45, 0)) else { return };
    let root = scratch("fsck");
    let repo = template(stock, &root);
    for args in [&["fsck"][..], &["fsck", "--connectivity-only"]] {
        let [stock_side, zvcs_side] = both(stock, &root, &repo, args);
        let tail = |stderr: &str| stderr.lines().skip(1).map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(zvcs_side.0, stock_side.0, "git {args:?}: exit code");
        assert_eq!(zvcs_side.1, stock_side.1, "git {args:?}: stdout");
        assert_eq!(tail(&zvcs_side.2), tail(&stock_side.2), "git {args:?}: stderr past the refs check");
        assert!(
            stock_side.2.ends_with("error: bad index file sha1 signature\nfatal: index file corrupt\n"),
            "git {args:?}: the fixture must reach the trailer check, got {stock_side:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
