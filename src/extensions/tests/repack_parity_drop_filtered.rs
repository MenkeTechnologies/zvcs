//! `git repack --drop-filtered [--dry-run]`, new in git 2.56.
//!
//! With `-a --filter=blob:limit=<n>` and a promisor remote, the promisor blobs
//! the filter rejects (`size >= n`) are left out of the rebuilt promisor pack and
//! `-d` is implied, so they are gone until fetched again
//! (builtin/repack.c:285-395, repack-filtered.c:88-133, repack-promisor.c:28-35).
//! `--dry-run` lists them instead — in the order git's `oidset` iterates — and
//! the repack still runs, dropping nothing.
//!
//! The checks run right after parse-options, ahead of every other conflict, in
//! the order pinned below.
//!
//! The fixture makes a promisor pack by hand: every blob is committed, removed
//! from the tree, packed, and the pack marked with a `.promisor` file under a
//! `remote.origin.promisor` remote. Expectations measured from stock git 2.56.0
//! on the same fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@e")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@e")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = run(dir, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `big1`..`big8` of 101..801 bytes and a 2-byte `small`, all committed; the
/// `big*` files are then removed so only `small` stays in the index.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-repack-drop-filtered-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    for i in 1..=8 {
        std::fs::write(repo.join(format!("big{i}")), format!("{}\n", "x".repeat(100 * i))).unwrap();
    }
    std::fs::write(repo.join("small"), "s\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "i"]);
    let bigs: Vec<String> = (1..=8).map(|i| format!("big{i}")).collect();
    let mut rm = vec!["rm", "-q"];
    rm.extend(bigs.iter().map(String::as_str));
    git(&repo, &rm);
    git(&repo, &["commit", "-q", "-m", "rm"]);
    git(&repo, &["repack", "-a", "-d", "-q"]);
    for entry in std::fs::read_dir(repo.join(".git/objects/pack")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "pack") {
            std::fs::write(path.with_extension("promisor"), "").unwrap();
        }
    }
    git(&repo, &["config", "remote.origin.url", "https://example.invalid/x"]);
    git(&repo, &["config", "remote.origin.promisor", "true"]);
    repo
}

fn fatal(out: &Output) -> (Option<i32>, String) {
    (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn pack_files(repo: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_checks_run_in_git_order() {
    let repo = fixture("checks");
    for (args, message) in [
        (&["--drop-filtered"][..], "--drop-filtered requires --filter"),
        (&["--dry-run"], "--dry-run only takes effect with --drop-filtered"),
        (&["--drop-filtered", "--filter=blob:limit=1"], "--drop-filtered requires -a"),
        // `--cruft` implies everything-into-one only after these checks.
        (&["--cruft", "--drop-filtered", "--filter=blob:limit=1"], "--drop-filtered requires -a"),
        (&["-a", "--drop-filtered", "--filter=blob:none"], "--drop-filtered only supports --filter=blob:limit=<n> for now"),
        (
            &["-a", "--drop-filtered", "--filter=blob:limit=1", "-b"],
            "options '--drop-filtered' and '--write-bitmap-index' cannot be used together",
        ),
        (
            &["-a", "--drop-filtered", "--filter=blob:limit=1", "--filter-to=x"],
            "options '--drop-filtered' and '--filter-to' cannot be used together",
        ),
        // Ahead of the `-A`/`-k` conflict: the index check fires first.
        (
            &["-a", "--drop-filtered", "--filter=blob:limit=1", "-k", "-A", "--dry-run"],
            "cannot drop 'small' (b4785957bc986dc39c629de9fac9df46972c00fc): it is referenced by the current index",
        ),
    ] {
        let mut argv = vec!["repack"];
        argv.extend_from_slice(args);
        let out = run(&repo, &argv);
        assert_eq!(fatal(&out), (Some(128), format!("fatal: {message}\n")), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }

    std::fs::write(repo.join(".git/MERGE_HEAD"), "").unwrap();
    let out = run(&repo, &["repack", "-a", "--filter=blob:limit=250", "--drop-filtered"]);
    assert_eq!(
        fatal(&out),
        (
            Some(128),
            "fatal: --drop-filtered cannot be used while another operation (merge, rebase, am, \
             cherry-pick, revert, or bisect) is in progress\n"
                .to_string()
        )
    );
    std::fs::remove_file(repo.join(".git/MERGE_HEAD")).unwrap();

    git(&repo, &["config", "--unset", "remote.origin.promisor"]);
    let out = run(&repo, &["repack", "-a", "--filter=blob:limit=250", "--drop-filtered"]);
    assert_eq!(fatal(&out), (Some(128), "fatal: --drop-filtered requires a promisor remote\n".to_string()));

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn dry_run_lists_in_oidset_order_and_drops_nothing() {
    let repo = fixture("dry");
    let out = run(&repo, &["repack", "-a", "--filter=blob:limit=250", "--drop-filtered", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    // big3..big8, in khash bucket order across one resize of the set.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a10621506383ae26d078a647b4a5263bb5cd3993\n\
         da0875b775d6a4f30a6b65a158e3de1834fcec80\n\
         3428a1268f4434055c20eaecbb9507dcc4513ebe\n\
         54ab0854198734130261d3b9362958b81429a93a\n\
         46699e626fdfba9d41d11b834791b21f2baf4264\n\
         b6fca446b502e862f37253b2bb8af2f22a451b02\n"
    );
    // The repack still ran, without `-d`: the old pack is still there beside the new.
    assert_eq!(pack_files(&repo).iter().filter(|n| n.ends_with(".pack")).count(), 2);
    assert!(run(&repo, &["cat-file", "-e", "HEAD~1:big8"]).status.success());

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn a_real_run_drops_the_large_promisor_blobs() {
    let repo = fixture("real");
    let out = run(&repo, &["repack", "-a", "-q", "--filter=blob:limit=250", "--drop-filtered"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty());
    let files = pack_files(&repo);
    let kinds: Vec<&str> = files.iter().map(|n| n.rsplit('.').next().unwrap()).collect();
    assert_eq!(kinds, ["idx", "pack", "promisor", "rev"], "{files:?}");
    // Two commits, two trees, `small`, `big1` and `big2` survive.
    let count = String::from_utf8_lossy(&run(&repo, &["count-objects", "-v"]).stdout).into_owned();
    assert!(count.contains("in-pack: 7\n"), "{count}");
    assert!(run(&repo, &["cat-file", "-e", "HEAD~1:big2"]).status.success());
    assert!(!run(&repo, &["cat-file", "-e", "HEAD~1:big3"]).status.success());

    let _ = std::fs::remove_dir_all(&repo);
}
