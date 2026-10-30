//! `diff-files -M`/`-C`/`-B` run `diffcore_std()`'s break, rename and merge-broken
//! passes over the queue `run_diff_files()` builds (diff.c:7489-7495).
//!
//! A destination is a worktree file that is a creation (an intent-to-add entry), a
//! pair `-B` broke, or an unmerged pair; a source is an index entry, and under
//! `--find-copies-harder` every unchanged one too (`diff_same()`, diff-lib.c:276-281).
//! zvcs accepted the flags and never ran the passes, so it printed `D`+`A` (or
//! `D`+`U`) where stock prints `R100`/`C100`/`U100`, never put a score on a broken
//! pair, and rendered every record with a single path.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
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

fn lines(prefix: &str, n: usize) -> String {
    (1..=n).map(|i| format!("{prefix} {i} of some file content\n")).collect()
}

#[derive(Clone, Copy, Debug)]
enum Shape {
    /// `gone.txt` deleted from the worktree and re-created executable as an
    /// intent-to-add `dir/moved.txt`; `keep.txt` copied into the intent-to-add
    /// `sp/k y.txt`; `rw.txt` two-thirds rewritten.
    Worktree,
    /// `keep.txt` conflicted, its worktree file replaced by `gone.txt`'s content,
    /// and `gone.txt` deleted.
    Unmerged,
}

fn fixture(tag: &str, stock: &str, shape: Shape) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-dfrename-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dir")).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("gone.txt"), lines("gone", 30)).unwrap();
    std::fs::write(dir.join("keep.txt"), lines("keep", 30)).unwrap();
    std::fs::write(dir.join("rw.txt"), lines("rewrite", 30)).unwrap();
    std::fs::write(dir.join("dir/a.txt"), lines("dir", 30)).unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "base"]);
    match shape {
        Shape::Worktree => {
            std::fs::rename(dir.join("gone.txt"), dir.join("dir/moved.txt")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let p = dir.join("dir/moved.txt");
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            std::fs::create_dir_all(dir.join("sp")).unwrap();
            std::fs::copy(dir.join("keep.txt"), dir.join("sp/k y.txt")).unwrap();
            run(stock, &dir, &["add", "-N", "dir/moved.txt", "sp/k y.txt"]);
            let rw: String = (1..=30)
                .map(|i| match i {
                    10..=29 => format!("changed {i}\n"),
                    _ => format!("rewrite {i} of some file content\n"),
                })
                .collect();
            std::fs::write(dir.join("rw.txt"), rw).unwrap();
        }
        Shape::Unmerged => {
            run(stock, &dir, &["checkout", "-qb", "side"]);
            std::fs::write(dir.join("keep.txt"), lines("keep", 30) + "side\n").unwrap();
            run(stock, &dir, &["commit", "-qam", "side"]);
            run(stock, &dir, &["checkout", "-q", "main"]);
            std::fs::write(dir.join("keep.txt"), lines("keep", 30) + "main\n").unwrap();
            run(stock, &dir, &["commit", "-qam", "main"]);
            assert_eq!(run(stock, &dir, &["merge", "-q", "side"]).2, Some(1));
            std::fs::copy(dir.join("gone.txt"), dir.join("keep.txt")).unwrap();
            std::fs::remove_file(dir.join("gone.txt")).unwrap();
        }
    }
    dir
}

const DIFFS: &[&[&str]] = &[
    &["diff-files"],
    &["diff-files", "-M"],
    &["diff-files", "-C"],
    &["diff-files", "-C", "-C"],
    &["diff-files", "--find-copies-harder", "--abbrev"],
    &["diff-files", "--no-renames", "-C"],
    &["diff-files", "-B"],
    &["diff-files", "-B", "-M"],
    &["diff-files", "-B/70%", "-C", "-C"],
    &["diff-files", "-M90%"],
    &["diff-files", "-l1", "-C", "-C"],
    &["diff-files", "-C", "-C", "--name-status"],
    &["diff-files", "-C", "-C", "--name-only"],
    &["diff-files", "-B", "-C", "-C", "-z"],
    &["diff-files", "-B", "-C", "-C", "-z", "--name-status"],
    &["diff-files", "-B", "-C", "-C", "--stat"],
    &["diff-files", "-C", "-C", "--compact-summary"],
    &["diff-files", "-B", "-C", "-C", "--numstat"],
    &["diff-files", "-C", "-C", "--numstat", "-z"],
    &["diff-files", "-B", "-C", "-C", "--summary"],
    &["diff-files", "-B", "-C", "-C", "--dirstat=files"],
    &["diff-files", "-B", "-C", "-C", "-p"],
    &["diff-files", "-B", "-D", "-p"],
    &["diff-files", "-C", "-C", "-R", "--raw"],
    &["diff-files", "-C", "-C", "-R", "-p"],
    &["diff-files", "-C", "-C", "--relative=sp"],
    &["diff-files", "-B", "--diff-filter=B"],
    &["diff-files", "-B", "--diff-filter=M"],
    &["diff-files", "-C", "-C", "--diff-filter=C"],
    &["diff-files", "-B", "-M", "-2"],
    &["diff-files", "-B", "-M", "-2", "-p"],
    &["diff-files", "-B", "-M", "-2", "--stat"],
    &["diff-files", "-B", "-M", "-2", "--summary"],
];

#[test]
fn rename_copy_and_break_detection_match_stock() {
    let Some(stock) = stock_git() else { return };
    for shape in [Shape::Worktree, Shape::Unmerged] {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(&format!("{shape:?}-{who}"), stock, shape);
            seen.push(DIFFS.iter().map(|args| run(bin, &dir, args)).collect::<Vec<_>>());
            let _ = std::fs::remove_dir_all(&dir);
        }
        for (i, args) in DIFFS.iter().enumerate() {
            assert_eq!(seen[1][i], seen[0][i], "{shape:?}: {args:?}");
        }
    }
    // The fixtures reach the passes at all: stock pairs something in each shape.
    let dir = fixture("probe", stock, Shape::Worktree);
    let raw = run(stock, &dir, &["diff-files", "-B", "-C", "-C"]).0;
    let _ = std::fs::remove_dir_all(&dir);
    assert!(raw.contains("\tR100\t") || raw.contains(" R100\t"), "{raw}");
    assert!(raw.contains(" C100\t"), "{raw}");
}

#[test]
fn a_cut_short_matrix_warns_after_the_listing() {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let dir = fixture(&format!("limit-{who}"), stock, Shape::Worktree);
        // Inexact candidates only: perturb each destination so no exact match exists.
        for p in ["dir/moved.txt", "sp/k y.txt"] {
            let mut s = std::fs::read_to_string(dir.join(p)).unwrap();
            s.push_str("tail\n");
            std::fs::write(dir.join(p), s).unwrap();
        }
        seen.push(run(bin, &dir, &["diff-files", "-l1", "-C", "-C", "--stat"]));
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert_eq!(seen[1], seen[0]);
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
        let dir = std::env::temp_dir().join(format!("zvcs-dfrename-{tag}-{who}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        setup(stock, &dir);
        seen.push(invocations.iter().map(|args| run(bin, &dir, args)).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&dir);
    }
    for (i, args) in invocations.iter().enumerate() {
        assert_eq!(seen[1][i], seen[0][i], "{tag}: {args:?}");
    }
    seen.swap_remove(0)
}

/// An exact binary rename shows its header alone (diff.c:3949-3957), with or without
/// `--binary`; a rewrite `-B` broke keeps `a/`/`b/` on its `---`/`+++` lines under `-R`,
/// since `emit_rewrite_diff()` swaps them only for `diff.mnemonicPrefix` (diff.c:1891).
#[test]
fn exact_binary_rename_and_reversed_rewrite_match_stock() {
    let Some(stock) = stock_git() else { return };
    compare(
        "binary",
        stock,
        &[
            &["diff-files", "-M", "-p"],
            &["diff-files", "-M", "-p", "--binary"],
            &["diff-files", "-B", "-M", "--patch-with-stat"],
            &["diff-files", "-B", "-M", "-R", "-p"],
            &["diff-files", "-B", "-C", "-C", "-R", "-p", "--src-prefix=s/", "--dst-prefix=d/"],
        ],
        |stock, dir| {
            run(stock, dir, &["init", "-q", "-b", "main"]);
            std::fs::write(dir.join("gone.txt"), lines("gone", 30)).unwrap();
            std::fs::write(dir.join("mod.txt"), lines("mod", 30)).unwrap();
            std::fs::write(dir.join("bin.dat"), b"\0\x01\x02binary".repeat(50)).unwrap();
            run(stock, dir, &["add", "."]);
            run(stock, dir, &["commit", "-qm", "base"]);
            std::fs::rename(dir.join("gone.txt"), dir.join("mod.txt")).unwrap();
            std::fs::rename(dir.join("bin.dat"), dir.join("bin2.dat")).unwrap();
            run(stock, dir, &["add", "-N", "bin2.dat"]);
        },
    );
}

/// Under `-R` a submodule's worktree side is `p->one`, so `--find-copies-harder`
/// registers it as a source and `hash_filespec()` hashes its
/// `diff_populate_gitlink()` line, `Subproject commit <null id>`; `-dirty` joins it
/// only when a patch raised `flags.dirty_submodules` (diff.c:5314-5315,
/// diff-lib.c:100-103), because the submodule's `HEAD` moved.
#[test]
fn a_reversed_submodule_copy_source_hashes_its_gitlink_line() {
    let Some(stock) = stock_git() else { return };
    let stock_out = compare(
        "gitlink",
        stock,
        &[
            &["diff-files", "-C", "-C", "-R"],
            &["diff-files", "-C", "-C", "-R", "--raw", "-p"],
            &["diff-files", "-C", "-C", "-R", "--raw", "--stat"],
            &["diff-files", "-C", "-C", "--raw", "-p"],
        ],
        |stock, dir| {
            let inner = dir.join("inner");
            std::fs::create_dir_all(&inner).unwrap();
            run(stock, &inner, &["init", "-q", "-b", "main"]);
            std::fs::write(inner.join("x"), "x\n").unwrap();
            run(stock, &inner, &["add", "x"]);
            run(stock, &inner, &["commit", "-qm", "i"]);
            run(stock, dir, &["init", "-q", "-b", "main"]);
            std::fs::write(dir.join("del.txt"), "unrelated\n").unwrap();
            std::fs::write(dir.join("g.txt"), lines("g", 30)).unwrap();
            run(stock, dir, &["add", "g.txt", "del.txt", "inner"]);
            // Registered in `.gitmodules`, as `submodule add` would leave it.
            std::fs::write(dir.join(".gitmodules"), "[submodule \"inner\"]\n\tpath = inner\n\turl = ./inner\n").unwrap();
            run(stock, dir, &["add", ".gitmodules"]);
            run(stock, dir, &["commit", "-qm", "base"]);
            std::fs::write(inner.join("y"), "y\n").unwrap();
            run(stock, &inner, &["add", "y"]);
            run(stock, &inner, &["commit", "-qm", "j"]);
            std::fs::write(inner.join("x"), "dirt\n").unwrap();
            std::fs::write(dir.join("g.txt"), lines("g", 30) + "more\n").unwrap();
            // A deletion, which `-R` turns into the destination without which
            // `diffcore_rename()` returns before hashing anything.
            std::fs::remove_file(dir.join("del.txt")).unwrap();
        },
    );
    // The fixture reaches the hashing: stock's `-C -C -R` names the blob id of
    // `Subproject commit <40 zeros>\n` for the submodule's worktree side.
    assert!(stock_out[0].0.contains(":160000 160000 46405a19abd27d2eec72b56696423d49097a5d6a "), "{}", stock_out[0].0);
}
