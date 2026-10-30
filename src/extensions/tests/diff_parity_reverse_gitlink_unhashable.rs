//! Three corners of the worktree diffs that zvcs and stock git disagreed on.
//!
//! * `diff_unmerge()` (diff.c:7722-7736) builds a conflicted path's `U` pair without
//!   looking at `-R`, so under `git diff -R -C` its worktree file stays `p->two`, a
//!   rename destination that `hash_filespec()` names. A moved submodule's worktree
//!   side, which `-R` does swap into `p->one`, was queued with the null id
//!   (diff-lib.c:290) and hashes as `Subproject commit <null>`, while the patch still
//!   names the commit `diff_fill_oid_info()` resolves.
//! * `git diff -c <rev>` combines a gitlink whose `HEAD` moved off the index entry,
//!   since `get_stat_data()` then hands `show_modified()` the null id
//!   (diff-lib.c:324-343, :408-424).
//! * A worktree file that cannot be opened is `DATA_CHANGED` to `ce_compare_data()`
//!   (read-cache.c:231-245), so the raw formats list it; a patch, a stat or `--check`
//!   reach `diff_fill_oid_info()`, which dies `cannot hash` at that pair after the
//!   output of the pairs ahead of it (diff.c:4969-4985, object-file.c:996-999).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

/// Runs every invocation through both binaries on twin fixtures built by `setup`.
fn compare(
    tag: &str,
    stock: &str,
    invocations: &[&[&str]],
    setup: fn(&str, &Path),
) -> Vec<(String, String, Option<i32>)> {
    let mut seen = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let dir = std::env::temp_dir().join(format!("zvcs-diffrg-{tag}-{who}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        setup(stock, &dir);
        seen.push(invocations.iter().map(|args| run(bin, &dir, args)).collect::<Vec<_>>());
        restore_permissions(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }
    for (i, args) in invocations.iter().enumerate() {
        assert_eq!(seen[1][i], seen[0][i], "{tag}: {args:?}");
    }
    seen.swap_remove(0)
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn restore_permissions(dir: &Path) {
    #[cfg(unix)]
    for name in ["f.txt"] {
        if dir.join(name).exists() {
            chmod(&dir.join(name), 0o644);
        }
    }
}

/// A submodule `inner` whose `HEAD` moved and whose worktree is dirty, beside
/// `del.txt` deleted from the worktree.
fn submodule_fixture(stock: &str, dir: &Path) {
    let inner = dir.join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    run(stock, &inner, &["init", "-q", "-b", "main"]);
    std::fs::write(inner.join("x"), "x\n").unwrap();
    run(stock, &inner, &["add", "x"]);
    run(stock, &inner, &["commit", "-qm", "i"]);
    run(stock, dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("del.txt"), "unrelated\n").unwrap();
    std::fs::write(dir.join("g.txt"), "g 1\ng 2\ng 3\n").unwrap();
    run(stock, dir, &["add", "g.txt", "del.txt", "inner"]);
    std::fs::write(dir.join(".gitmodules"), "[submodule \"inner\"]\n\tpath = inner\n\turl = ./inner\n").unwrap();
    run(stock, dir, &["add", ".gitmodules"]);
    run(stock, dir, &["commit", "-qm", "base"]);
}

fn move_and_dirty_submodule(stock: &str, dir: &Path) {
    let inner = dir.join("inner");
    std::fs::write(inner.join("y"), "y\n").unwrap();
    run(stock, &inner, &["add", "y"]);
    run(stock, &inner, &["commit", "-qm", "j"]);
    std::fs::write(inner.join("x"), "dirt\n").unwrap();
    std::fs::remove_file(dir.join("del.txt")).unwrap();
}

#[test]
fn a_reversed_unmerged_pair_stays_a_destination_beside_a_moved_submodule() {
    let Some(stock) = stock_git() else { return };
    let stock_out = compare(
        "unmerged",
        stock,
        &[
            &["diff", "-R", "--raw"],
            &["diff", "-C", "-C", "-R", "--raw"],
            &["diff", "-C", "-C", "-R", "--raw", "-p"],
            &["diff", "-C", "-C", "-R"],
            &["diff", "-C", "-C", "-R", "--stat"],
            &["diff", "-B", "-C", "-C", "-R", "--raw"],
            &["diff", "--find-copies-harder", "-R", "--raw", "--abbrev=10"],
            &["diff", "--cached", "-R", "-C", "-C"],
            &["diff", "-C", "-C", "--raw"],
            &["diff-index", "HEAD"],
            &["diff-index", "HEAD", "-C", "-C", "-R"],
            &["diff-index", "HEAD", "-C", "-C", "-R", "--raw", "-p"],
            &["diff-index", "HEAD", "--find-copies-harder", "-R", "--abbrev"],
        ],
        |stock, dir| {
            submodule_fixture(stock, dir);
            run(stock, dir, &["checkout", "-qb", "side"]);
            std::fs::write(dir.join("g.txt"), "g 1\ng 2\ng 3\nside\n").unwrap();
            run(stock, dir, &["commit", "-qam", "side"]);
            run(stock, dir, &["checkout", "-q", "main"]);
            std::fs::write(dir.join("g.txt"), "g 1\ng 2\ng 3\nmain\n").unwrap();
            run(stock, dir, &["commit", "-qam", "main"]);
            assert_eq!(run(stock, dir, &["merge", "-q", "side"]).2, Some(1));
            move_and_dirty_submodule(stock, dir);
        },
    );
    // The fixture reaches both behaviours: the `U` pair keeps mode 000000 on its
    // pre-image, and the submodule's worktree side is the null-id gitlink line.
    assert!(stock_out[0].0.contains(":000000 100644 0000000 0000000 U\tg.txt"), "{}", stock_out[0].0);
    assert!(stock_out[1].0.contains(":160000 160000 46405a1 "), "{}", stock_out[1].0);
}

#[test]
fn a_moved_submodule_combines_against_one_revision() {
    let Some(stock) = stock_git() else { return };
    let stock_out = compare(
        "combined",
        stock,
        &[
            &["diff", "HEAD", "-c", "--raw"],
            &["diff", "HEAD", "-c"],
            &["diff", "HEAD", "--cc"],
            &["diff", "HEAD", "-c", "--stat"],
            &["diff", "HEAD", "-c", "--name-status"],
            &["diff-index", "HEAD", "-C", "-C", "-R"],
            &["diff-index", "HEAD", "-C", "-C", "-R", "-p"],
        ],
        |stock, dir| {
            submodule_fixture(stock, dir);
            move_and_dirty_submodule(stock, dir);
        },
    );
    assert!(stock_out[0].0.starts_with("::160000 160000 160000 "), "{}", stock_out[0].0);
}

/// Only dirty, `HEAD` where the index says: no null id, so the ordinary pair.
#[test]
fn a_merely_dirty_submodule_stays_a_pair_under_combined() {
    let Some(stock) = stock_git() else { return };
    compare(
        "dirty",
        stock,
        &[&["diff", "HEAD", "-c", "--raw"], &["diff", "HEAD", "-c"]],
        |stock, dir| {
            submodule_fixture(stock, dir);
            std::fs::write(dir.join("inner/x"), "dirt\n").unwrap();
        },
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_worktree_file_is_listed_and_unhashable() {
    let Some(stock) = stock_git() else { return };
    // `f.txt` keeps its size and is written within the index's second, so the stat
    // data cannot settle it and the walk has to open the file.
    let setup: fn(&str, &Path) = |stock, dir| {
        run(stock, dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        std::fs::write(dir.join("z.txt"), "z\n").unwrap();
        run(stock, dir, &["add", "."]);
        run(stock, dir, &["commit", "-qm", "base"]);
        // Writing within the second is a race the fixture can lose: with the index
        // written a second later than `two` and `owt` landing in `two`'s second, the
        // stat matches, the entry is not racy, and stock itself reports the index id
        // (read-cache.c:353-375, :434-438). Both files take one mtime a minute ahead,
        // so every index stock writes is older than the entry and it stays racy.
        let ahead = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        let stamp = |path: &Path| {
            let f = std::fs::File::options().write(true).open(path).unwrap();
            f.set_modified(ahead).unwrap();
        };
        std::fs::write(dir.join("f.txt"), "two\n").unwrap();
        stamp(&dir.join("f.txt"));
        run(stock, dir, &["add", "f.txt"]);
        std::fs::write(dir.join("f.txt"), "owt\n").unwrap();
        stamp(&dir.join("f.txt"));
        std::fs::write(dir.join("a.txt"), "aa\n").unwrap();
        std::fs::write(dir.join("z.txt"), "zz\n").unwrap();
        chmod(&dir.join("f.txt"), 0);
    };
    // Running as root opens it anyway, and then there is nothing to compare.
    let probe = std::env::temp_dir().join(format!("zvcs-diffrg-probe-{}", std::process::id()));
    std::fs::write(&probe, "x").unwrap();
    chmod(&probe, 0);
    let readable = std::fs::File::open(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    if readable {
        return;
    }
    compare(
        "eperm",
        stock,
        &[
            &["diff", "--raw"],
            &["diff", "HEAD", "--raw"],
            &["diff", "HEAD", "-c", "--raw"],
            &["diff", "HEAD", "--name-only"],
            &["diff", "HEAD", "-c"],
            &["diff"],
            &["diff", "HEAD"],
            &["diff", "--stat"],
            &["diff", "--raw", "-p"],
            &["diff", "--check"],
            &["diff", "-w", "--raw"],
            &["diff", "-R"],
            &["diff-files"],
            &["diff-files", "-p"],
            &["diff-files", "--stat"],
            &["diff-files", "--raw", "-p"],
            &["diff-index", "HEAD"],
            &["diff-index", "-p", "HEAD"],
            &["diff-index", "--stat", "HEAD"],
            &["diff-index", "-R", "-p", "HEAD"],
        ],
        setup,
    );
}
