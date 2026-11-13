//! `--filter=<spec>` runs inside the object walk (`list_objects_filter__filter_object()`,
//! list-objects-filter.c:799-822), for `pack-objects`, for `rev-list` and for the
//! `pack-objects` child of `repack --filter`, all through one port of
//! list-objects-filter.c.
//!
//! * `pack-objects` filtered its object list after the walk and ignored
//!   `sparse:oid=`, `combine:`, `tree:<n>` for n > 0 and every `--filter` but the
//!   last; a filter now decides each object as the walk reaches it, commits
//!   included (`object:type=blob` keeps only the named commit), `--stdin-packs`
//!   refuses one, `--cruft` ignores it, and `--missing=allow-any` leaves a
//!   missing object out instead of dying on it.
//! * `NOT_USER_GIVEN` is a flag on the object, set on each tree entry and on
//!   each commit's root tree before the named objects are walked, so a tree
//!   `--indexed-objects` names is filtered once the walk has reached it; zvcs
//!   exempted every named object.
//! * `repack --filter` decided `object:type=`, `sparse:oid=` and `combine:` with
//!   them in its own approximation and kept every commit and index object.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(label: &str) -> Scratch {
    let root = std::env::temp_dir().join(format!("zvcs-filter-walk-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    Scratch(std::fs::canonicalize(&root).unwrap())
}

/// Exit status, stdout and stderr of `bin <args>` run in `dir` with `stdin`.
fn run(bin: &str, dir: &Path, args: &[&str], stdin: &[u8]) -> (i32, Vec<u8>, String) {
    use std::io::Write;
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.status.code().expect("no signal"), out.stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Nested directories with a subtree repeated deeper down (`tree:<n>` meets it at
/// two depths), two commits, an annotated tag, a sparse pattern blob, and a
/// staged directory whose cache-tree nodes `--indexed-objects` names.
fn prepare(stock: &str, r: &Path) {
    let git = |args: &[&str]| {
        let (code, _, err) = run(stock, r, args, b"");
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    let write = |rel: &str, body: &str| {
        let path = r.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    git(&["init", "-q", "-b", "main", "."]);
    write("a/s", "small\n");
    write("a/b/big", &"x".repeat(3000));
    write("a/b/c/f", "deep\n");
    write("d/e/b/big", &"x".repeat(3000));
    write("d/e/b/c/f", "deep\n");
    write("t", "top\n");
    write("sparse", "/a/\n!/a/b/\n*.txt\n");
    git(&["add", "."]);
    git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
    write("a/b/c/g", "more\n");
    write("x/a/s", "small\n");
    write("x/a/b/c/g", "more\n");
    write("n.txt", "n\n");
    git(&["add", "."]);
    git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "two"]);
    git(&["tag", "-a", "-m", "tagged", "v1", "HEAD~1"]);
    write("q/r/z.txt", "staged\n");
    write("q/a/s", "small\n");
    git(&["add", "q"]);
    git(&["write-tree"]);
}

#[test]
fn pack_objects_runs_every_filter_inside_its_walk() {
    let Some(stock) = stock_git::stock_git() else { return };
    let dir = scratch("pack-objects");
    let r = dir.0.join("r");
    std::fs::create_dir_all(&r).unwrap();
    prepare(stock, &r);
    let cases: &[(&str, &[&str])] = &[
        ("main\n", &["--revs", "--filter=tree:1"]),
        ("main\n^main~1\n", &["--revs", "--filter=tree:2"]),
        ("v1\nmain^{tree}\n", &["--revs", "--filter=tree:1"]),
        ("main\n", &["--revs", "--filter=sparse:oid=HEAD:sparse"]),
        ("main:x\n", &["--revs", "--filter=sparse:oid=main:sparse"]),
        ("main\n", &["--revs", "--filter=combine:blob:none+tree:2"]),
        ("main\n", &["--revs", "--filter=combine:sparse:oid=HEAD%3asparse+blob:limit=10"]),
        ("main\n", &["--revs", "--filter=tree:1", "--filter=blob:none"]),
        ("main\n", &["--revs", "--filter=object:type=blob"]),
        ("v1\nmain^{tree}\n", &["--revs", "--filter=object:type=tag"]),
        ("", &["--all", "--indexed-objects", "--filter=tree:1"]),
        ("", &["--indexed-objects", "--filter=tree:0"]),
        ("main\n", &["--revs", "--filter=sparse:oid=nope"]),
        ("main\n", &["--revs", "--filter=sparse:oid=HEAD"]),
        ("", &["--stdin-packs", "--filter=blob:none"]),
    ];
    for (stdin, args) in cases {
        let argv: Vec<&str> = ["pack-objects", "--stdout", "-q"].iter().chain(args.iter()).copied().collect();
        let want = run(stock, &r, &argv, stdin.as_bytes());
        assert_eq!(run(BIN, &r, &argv, stdin.as_bytes()), want, "{args:?} <<< {stdin:?}");
    }

    // A missing blob is left out under `--missing=allow-any` and fails the run
    // without it.
    let (_, blob, _) = run(stock, &r, &["rev-parse", "main:a/b/c/g"], b"");
    let blob = String::from_utf8(blob).unwrap();
    std::fs::remove_file(r.join(format!(".git/objects/{}/{}", &blob[..2], blob[2..].trim()))).unwrap();
    for args in [&["--missing=allow-any", "--filter=tree:3"][..], &["--filter=tree:3"]] {
        let argv: Vec<&str> = ["pack-objects", "--stdout", "-q", "--revs"].iter().chain(args.iter()).copied().collect();
        let want = run(stock, &r, &argv, b"main\n");
        assert_eq!(run(BIN, &r, &argv, b"main\n"), want, "{args:?}");
    }
}

#[test]
fn rev_list_filters_a_named_tree_the_walk_already_reached() {
    let Some(stock) = stock_git::stock_git() else { return };
    let dir = scratch("rev-list");
    let r = dir.0.join("r");
    std::fs::create_dir_all(&r).unwrap();
    prepare(stock, &r);
    for args in [
        &["rev-list", "--objects", "--filter=tree:0", "--all", "--indexed-objects"][..],
        &["rev-list", "--objects", "--filter=tree:1", "--indexed-objects"],
        &["rev-list", "--objects", "--filter=object:type=blob", "--in-commit-order", "--all", "--indexed-objects"],
    ] {
        let want = run(stock, &r, args, b"");
        assert_eq!(run(BIN, &r, args, b""), want, "{args:?}");
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Every object id of every `.idx` in `dirs`, one sorted list per pack, the
/// lists sorted.
fn packs(stock: &str, dirs: &[PathBuf]) -> Vec<Vec<String>> {
    let mut all = Vec::new();
    for dir in dirs {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "idx") {
                continue;
            }
            let idx = std::fs::read(&path).unwrap();
            let (_, out, _) = run(stock, dir, &["show-index", "--object-format=sha1"], &idx);
            let mut ids: Vec<String> =
                String::from_utf8(out).unwrap().lines().map(|l| l.split(' ').nth(1).unwrap().to_owned()).collect();
            ids.sort();
            all.push(ids);
        }
    }
    all.sort();
    all
}

#[test]
fn repack_filter_to_splits_where_the_pack_objects_walk_does() {
    let Some(stock) = stock_git::stock_git() else { return };
    let dir = scratch("repack");
    let origin = dir.0.join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    prepare(stock, &origin);
    for spec in ["object:type=blob", "sparse:oid=HEAD:sparse", "combine:tree:1+object:type=tree"] {
        let mut sides = Vec::new();
        for (name, bin) in [("stock", stock), ("zvcs", BIN)] {
            let side = dir.0.join(name);
            let _ = std::fs::remove_dir_all(&side);
            std::fs::create_dir_all(side.join("ft")).unwrap();
            copy_tree(&origin, &side.join("r"));
            let filter = format!("--filter={spec}");
            let out = run(bin, &side.join("r"), &["-c", "maintenance.auto=false", "repack", "-q", "-a", "-d", &filter, "--filter-to=../ft/pack"], b"");
            sides.push((out, packs(stock, &[side.join("r/.git/objects/pack"), side.join("ft")])));
        }
        assert_eq!(sides[1], sides[0], "{spec}");
    }
}
