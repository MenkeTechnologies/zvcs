//! `diff_populate_filespec()` takes a side from the work tree instead of the object database when
//! `reuse_worktree_file()` vouches for it (diff.c:4367-4447, 4505-4506): the index is loaded, its
//! stage-0 entry for the path is a regular file naming the side's id, neither assume-unchanged nor
//! skip-worktree, the path converts nothing on its way into git, and `ie_match_stat()` finds the
//! checked-out file clean. `cmd_diff_tree()` always reads the index (builtin/diff-tree.c:133), so
//! over a packed blob whose zlib data does not inflate it diffs the clean checkout and prints no
//! inflate error, in every format and in break detection's `should_break()`
//! (diffcore-break.c:73-80).
//!
//! `git diff <a> <b>`, `show` and `log` never read the index, so they read the blob: break
//! detection gets the `NULL` content `packed_object_info()` hands back without a type
//! (packfile.c:1323-1327) with the size `unpack_entry()` left, and declines a pair that small.
//!
//! Each gate is checked through `--numstat` and `--stat -B`, whose counts tell the worktree read
//! from the `NULL` blob read; `-p` over a `NULL` side is stock's SIGSEGV and is not compared.
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip when
//! it is missing. Each zvcs run gets a fresh `ZVCS_HOME`.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-reuse-worktree-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn cmd(bin: &str, dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    c.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1112911993 -0700")
        .env("GIT_COMMITTER_DATE", "1112911993 -0700")
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    c
}

fn setup(stock: &str, dir: &Path, args: &[&str]) -> String {
    let out = cmd(stock, dir, args).output().unwrap();
    assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn run(bin: &str, dir: &Path, args: &[&str], home: &Path) -> (i32, String, String) {
    let mut c = cmd(bin, dir, args);
    if bin == ZVCS {
        c.env("ZVCS_HOME", home);
    }
    let out = c.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[track_caller]
fn same(stock: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let home = dir.with_extension("zvcs-home");
    let _ = std::fs::remove_dir_all(&home);
    let theirs = run(stock, dir, args, &home);
    let ours = run(ZVCS, dir, args, &home);
    assert_eq!(ours, theirs, "git {args:?} in {} (left zvcs, right stock)", dir.display());
    theirs
}

/// Flip two bytes of the zlib stream of `id`'s entry, past its header, so the `.idx` still names
/// the entry and only the inflate fails.
fn corrupt_entry(stock: &str, repo: &Path, id: &str) {
    let pack_dir = repo.join(".git/objects/pack");
    let idx = std::fs::read_dir(&pack_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "idx"))
        .expect("one pack");
    let listing = Command::new(stock)
        .arg("show-index")
        .stdin(std::fs::File::open(&idx).unwrap())
        .output()
        .unwrap();
    let offset: usize = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .find_map(|l| {
            let mut f = l.split_whitespace();
            let offset = f.next()?;
            (f.next()? == id).then(|| offset.parse().unwrap())
        })
        .expect("object in the pack");
    let pack = idx.with_extension("pack");
    let mut perms = std::fs::metadata(&pack).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&pack, perms).unwrap();
    let mut data = std::fs::read(&pack).unwrap();
    let kind = (data[offset] >> 4) & 7;
    assert!(kind == 3, "the corrupt entry is a whole blob");
    let mut at = offset;
    while data[at] & 0x80 != 0 {
        at += 1;
    }
    at += 1;
    data[at + 4] ^= 0xff;
    data[at + 5] ^= 0xff;
    std::fs::write(&pack, data).unwrap();
}

/// `f1` committed three times (too short for `repack` to deltify) and `f2` once, repacked, the
/// newest `f1` corrupt in the pack and checked out clean; `tweak` then runs in the repository.
fn fixture(stock: &str, root: &Path, name: &str, tweak: impl FnOnce(&Path)) -> PathBuf {
    setup(stock, root, &["init", "-q", "-b", "main", name]);
    let repo = root.join(name);
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    std::fs::write(repo.join("f2"), "other\n").unwrap();
    for i in 0..3 {
        std::fs::write(repo.join("f1"), format!("one\ntwo {i}\nthree\n").repeat(1 + i)).unwrap();
        setup(stock, &repo, &["add", "-A"]);
        setup(stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    setup(stock, &repo, &["repack", "-adfq"]);
    let id = setup(stock, &repo, &["rev-parse", "HEAD:f1"]);
    corrupt_entry(stock, &repo, &id);
    tweak(&repo);
    repo
}

#[test]
fn diff_tree_reads_a_clean_checkout_instead_of_the_blob() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("clean");
    let repo = fixture(stock, &root, "clean", |_| {});
    let (code, out, err) = same(stock, &repo, &["diff-tree", "-p", "HEAD~", "HEAD"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains("+two 2\n"), "{out}");
    for args in [
        &["diff-tree", "-p", "HEAD"][..],
        &["diff-tree", "-p", "-R", "HEAD~", "HEAD"],
        &["diff-tree", "-p", "HEAD~2", "HEAD"],
        &["diff-tree", "--stat", "HEAD~", "HEAD"],
        &["diff-tree", "--numstat", "HEAD~", "HEAD"],
        &["diff-tree", "--shortstat", "HEAD~", "HEAD"],
        &["diff-tree", "--compact-summary", "HEAD~", "HEAD"],
        &["diff-tree", "--dirstat", "HEAD~", "HEAD"],
        &["diff-tree", "-p", "--binary", "HEAD~", "HEAD"],
        &["diff-tree", "-p", "--word-diff", "HEAD~", "HEAD"],
        &["diff-tree", "-p", "-S", "three", "HEAD~", "HEAD"],
        &["diff-tree", "--check", "HEAD~", "HEAD"],
        // `should_break()` populates both sides, the reused one from the checkout.
        &["diff-tree", "-r", "--stat", "-B", "HEAD~", "HEAD"],
        &["diff-tree", "-p", "-B", "HEAD~", "HEAD"],
        &["diff-tree", "-p", "-M", "HEAD~", "HEAD"],
        &["diff-tree", "--stat", "-B", "-M", "HEAD~2", "HEAD"],
    ] {
        let (code, _, err) = same(stock, &repo, args);
        assert_eq!((code, err.as_str()), (0, ""), "{args:?}");
    }
}

#[test]
fn a_side_the_index_does_not_vouch_for_is_read_from_the_pack() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("gates");
    let git = |repo: &Path, args: &[&str]| {
        setup(stock, repo, args);
    };
    let gates: Vec<(&str, Box<dyn FnOnce(&Path)>)> = vec![
        ("assume-unchanged", Box::new(|r: &Path| git(r, &["update-index", "--assume-unchanged", "f1"]))),
        ("skip-worktree", Box::new(|r: &Path| git(r, &["update-index", "--skip-worktree", "f1"]))),
        // `would_convert_to_git()`: a conversion makes the reuse not worth it.
        ("autocrlf", Box::new(|r: &Path| git(r, &["config", "core.autocrlf", "true"]))),
        ("text", Box::new(|r: &Path| std::fs::write(r.join(".gitattributes"), "f1 text\n").unwrap())),
        ("ident", Box::new(|r: &Path| std::fs::write(r.join(".git/info/attributes"), "f1 ident\n").unwrap())),
        ("sparse", Box::new(|r: &Path| {
            git(r, &["config", "core.sparseCheckout", "true"]);
            std::fs::write(r.join(".git/info/sparse-checkout"), "/f2\n").unwrap();
        })),
        // `ie_match_stat()`: a mode change is `MODE_CHANGED` unless `core.fileMode` is off.
        ("chmod", Box::new(|r: &Path| chmod_x(&r.join("f1")))),
        ("index-gone", Box::new(|r: &Path| std::fs::remove_file(r.join(".git/index")).unwrap())),
    ];
    for (name, tweak) in gates {
        let repo = fixture(stock, &root, name, tweak);
        for args in [&["diff-tree", "--numstat", "HEAD~", "HEAD"][..], &["diff-tree", "--stat", "-B", "HEAD~", "HEAD"]] {
            let (code, out, err) = same(stock, &repo, args);
            assert_eq!(code, 0, "{name} {args:?}");
            assert!(err.contains("error: inflate:"), "{name} {args:?}: {out}{err}");
        }
    }
    // With `core.fileMode` off the executable bit is no stat difference by itself.
    let repo = fixture(stock, &root, "filemode-off", |r| {
        git(r, &["config", "core.fileMode", "false"]);
        chmod_x(&r.join("f1"));
    });
    same(stock, &repo, &["diff-tree", "--numstat", "HEAD~", "HEAD"]);
}

#[test]
fn break_detection_over_a_blob_read_as_no_content_does_not_die() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("break");
    let repo = fixture(stock, &root, "break", |_| {});
    // `should_break()`'s populate, `diff_filespec_is_binary()`'s and `fill_mmfile()`'s.
    let (code, _, err) = same(stock, &repo, &["diff", "--stat", "-B", "HEAD~", "HEAD"]);
    assert_eq!((code, err.matches("error: inflate:").count()), (0, 3), "{err}");
    for args in [
        &["diff", "--numstat", "-B", "HEAD~", "HEAD"][..],
        &["diff", "--shortstat", "-B", "-M", "HEAD~", "HEAD"],
        &["diff", "--stat", "-B", "HEAD~2", "HEAD"],
        &["show", "--stat", "-B"],
        &["log", "--stat", "-B", "-1"],
        &["log", "--numstat", "-B", "--format=%s"],
    ] {
        same(stock, &repo, args);
    }
}

fn chmod_x(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}
