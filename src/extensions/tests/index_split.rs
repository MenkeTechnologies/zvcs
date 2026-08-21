//! The split index (`link` extension plus `$GIT_DIR/sharedindex.<id>`), in both
//! directions.
//!
//! A split index is two files: `$GIT_DIR/index` carrying a `link` extension that
//! names a checksum, and `$GIT_DIR/sharedindex.<that checksum>` holding the
//! entries. git resolves that name against the **git directory**
//! (read-cache.c:1893) and only falls back to the directory the index file itself
//! sits in (read-cache.c:1901-1902) — a distinction that is invisible for
//! `$GIT_DIR/index` and decisive for `GIT_INDEX_FILE` pointing anywhere else.
//!
//! Both halves are worth pinning:
//!
//! * **reading** a split index stock git wrote, including through
//!   `GIT_INDEX_FILE`, which used to fail outright with
//!   `An IO error occurred while opening the index`;
//! * **writing** one stock git can read — `update-index --split-index` used to
//!   report success and write an ordinary index, so the flag was a silent no-op.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A real git, or `None` when this machine has none to compare against.
///
/// The probe asks a candidate to run a superset verb: zvcs serves `zjobs` itself,
/// a real git does not. That test is only sound with `PATH` **emptied**: git's
/// `execv_dashed_external()` resolves an unknown verb to a `git-<verb>` on `PATH`,
/// and zvcs's own installation puts `~/.zvcs/bin/git-zjobs` there as a symlink to
/// the shadow binary — so with the ambient `PATH` every stock git on this machine
/// answers `zjobs` successfully and would be misread as zvcs, leaving every test
/// in this file to return early while reporting a pass. Candidates are absolute
/// paths for the same reason: `PATH=""` makes a bare `git` unspawnable.
fn stock_git() -> Option<String> {
    for cand in ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"] {
        if !Path::new(cand).exists() {
            continue;
        }
        match Command::new(cand).args(["zjobs"]).env("PATH", "").output() {
            Ok(out) if !out.status.success() => return Some(cand.to_string()),
            _ => continue,
        }
    }
    None
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "zvcs-test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "zvcs-test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap_or_else(|e| panic!("{bin} {args:?}: {e}"))
}

fn ok(bin: &str, dir: &Path, args: &[&str]) -> String {
    let out = run(bin, dir, args);
    assert!(
        out.status.success(),
        "{bin} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-split-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

/// A repository with three tracked files, built entirely by `bin`.
fn populated(bin: &str, dir: &Path) {
    ok(bin, dir, &["init", "-q", "-b", "main", "."]);
    for name in ["a.txt", "b.txt", "sub-c.txt"] {
        std::fs::write(dir.join(name), format!("{name}\n")).unwrap();
    }
    ok(bin, dir, &["add", "a.txt", "b.txt", "sub-c.txt"]);
    ok(bin, dir, &["commit", "-q", "-m", "c0"]);
}

/// Whether an index file carries the `link` extension.
///
/// The signature is looked for past the header, and `link` is lowercase so it
/// cannot collide with a path: index paths are matched against the *whole*
/// extension table only in git, but four lowercase letters preceded by a
/// plausible size is specific enough for a fixture with known file names.
fn has_link_extension(index: &Path) -> bool {
    let data = std::fs::read(index).expect("index is readable");
    data.windows(4).any(|w| w == b"link")
}

/// The one `sharedindex.*` file in a git directory, if there is exactly one.
fn shared_index(git_dir: &Path) -> Option<std::path::PathBuf> {
    let mut found: Vec<_> = std::fs::read_dir(git_dir)
        .expect("git dir is readable")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("sharedindex."))
        })
        .collect();
    (found.len() == 1).then(|| found.pop().unwrap())
}

/// `update-index --split-index` must really split, and stock git must be able to
/// use the result without repairing it.
///
/// The port used to write an ordinary index and report success, so the flag was a
/// silent no-op — the failure mode the parity harness reports as
/// `linear::update-index::update-index --split-index`.
#[test]
fn zvcs_writes_a_split_index_stock_git_can_use() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found — skipping split-index interop test");
        return;
    };

    let repo = tmp("write");
    populated(BIN, &repo);
    let before = ok(BIN, &repo, &["write-tree"]);

    ok(BIN, &repo, &["update-index", "--split-index"]);

    let git_dir = repo.join(".git");
    assert!(
        has_link_extension(&git_dir.join("index")),
        "the split half must carry the `link` extension"
    );
    let shared = shared_index(&git_dir).expect("exactly one sharedindex.<id> must have been written");
    assert!(
        shared.metadata().unwrap().len() > 0,
        "the shared half must not be empty"
    );

    // The `link` extension names the shared half by its own trailing checksum
    // (`si->base_oid = si->base->oid`, read-cache.c:2371), so the file name and
    // the bytes must agree — otherwise git looks up a name nothing answers to.
    let named = shared.file_name().unwrap().to_str().unwrap();
    let checksum = &named["sharedindex.".len()..];
    let bytes = std::fs::read(&shared).unwrap();
    let trailer: String = bytes[bytes.len() - 20..].iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        trailer, checksum,
        "the shared index must be stored under its own checksum"
    );

    let fsck = run(&git, &repo, &["fsck", "--strict"]);
    assert!(
        fsck.status.success(),
        "git fsck --strict must pass on a split index zvcs wrote:\n{}",
        String::from_utf8_lossy(&fsck.stderr)
    );
    assert_eq!(
        ok(&git, &repo, &["ls-files", "-s"]).lines().count(),
        3,
        "stock git must see all three entries through the link"
    );

    // The load-bearing check: stock git answers the same tree, and answering it
    // does not make stock rewrite the index — i.e. it accepted the structure as
    // written rather than repairing it. This is the assertion that caught the
    // difference between "decodes to the same index" and "is the file git would
    // have written": a split half with no stand-in entries reads fine everywhere
    // and is still rewritten on sight by git 2.50.
    let index_before = std::fs::read(git_dir.join("index")).unwrap();
    assert_eq!(
        ok(&git, &repo, &["write-tree"]),
        before,
        "stock git must build the same tree from the split index"
    );
    assert_eq!(
        std::fs::read(git_dir.join("index")).unwrap(),
        index_before,
        "stock git must not have to repair the split index to use it"
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// Splitting the *same* index with each binary must produce the same two files.
///
/// Both halves are fully determined by the entries — the shared one is a plain
/// index of them, the split one a stand-in per entry plus the bitmaps — so given
/// one starting repository there is exactly one right answer, and the shared
/// index's name is its own checksum, so a content difference shows up as a name
/// difference too. Copying a finished repository and splitting each copy holds the
/// stat data fixed, which is the only part that could differ for uninteresting
/// reasons.
#[test]
fn splitting_the_same_index_gives_the_same_bytes_as_stock_git() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found — skipping split-index byte comparison");
        return;
    };

    let root = tmp("bytes");
    let source = root.join("source");
    std::fs::create_dir_all(&source).unwrap();
    populated(&git, &source);

    let (stock_repo, zvcs_repo) = (root.join("stock"), root.join("zvcs"));
    for dest in [&stock_repo, &zvcs_repo] {
        copy_tree(&source, dest);
    }
    ok(&git, &stock_repo, &["update-index", "--split-index"]);
    ok(BIN, &zvcs_repo, &["update-index", "--split-index"]);

    let stock_shared = shared_index(&stock_repo.join(".git")).expect("stock git wrote a shared index");
    let zvcs_shared = shared_index(&zvcs_repo.join(".git")).expect("zvcs wrote a shared index");
    assert_eq!(
        zvcs_shared.file_name(),
        stock_shared.file_name(),
        "the shared index is named by its checksum, so the names agree only if the bytes do"
    );
    assert_eq!(
        std::fs::read(&zvcs_shared).unwrap(),
        std::fs::read(&stock_shared).unwrap(),
        "the shared half must be byte-for-byte stock git's"
    );
    assert_eq!(
        std::fs::read(zvcs_repo.join(".git/index")).unwrap(),
        std::fs::read(stock_repo.join(".git/index")).unwrap(),
        "the split half must be byte-for-byte stock git's"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Recursively copy `from` to `to`, preserving nothing but the bytes — enough for
/// a git directory, whose only mode that matters is the executable bit no fixture
/// here uses.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// `--no-split-index` puts it back together.
///
/// Reading a split index already dissolves `link` into the entries, so the
/// ordinary write is git's un-split; this pins that the flag leaves no `link`
/// behind and that stock git still sees every entry.
#[test]
fn no_split_index_writes_one_whole_index_again() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found — skipping un-split test");
        return;
    };

    let repo = tmp("unsplit");
    populated(BIN, &repo);
    ok(BIN, &repo, &["update-index", "--split-index"]);
    assert!(has_link_extension(&repo.join(".git/index")));

    ok(BIN, &repo, &["update-index", "--no-split-index"]);
    assert!(
        !has_link_extension(&repo.join(".git/index")),
        "--no-split-index must leave no `link` extension behind"
    );
    assert_eq!(
        ok(&git, &repo, &["ls-files", "-s"]).lines().count(),
        3,
        "every entry must have come back into the one index"
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// `--no-split-index` on an index that was never split must not rewrite it.
///
/// git tests `istate->split_index` before it does anything
/// (builtin/update-index.c:1188-1194), so the command is a no-op on an ordinary
/// index. Reading a split index dissolves its `link` into the entries, so
/// "was it split" has to be remembered from decode time rather than read off the
/// in-memory state — this pins that it is.
#[test]
fn no_split_index_leaves_an_unsplit_index_alone() {
    let repo = tmp("nosplit-noop");
    populated(BIN, &repo);
    let index = repo.join(".git/index");
    let before = std::fs::read(&index).unwrap();
    let mtime_before = std::fs::metadata(&index).unwrap().modified().unwrap();

    ok(BIN, &repo, &["update-index", "--no-split-index"]);

    assert_eq!(
        std::fs::read(&index).unwrap(),
        before,
        "--no-split-index must not change an index that was never split"
    );
    assert_eq!(
        std::fs::metadata(&index).unwrap().modified().unwrap(),
        mtime_before,
        "--no-split-index must not even rewrite an index that was never split"
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// zvcs must read a split index stock git wrote — in place, and through
/// `GIT_INDEX_FILE` pointing outside `$GIT_DIR`.
///
/// The second half is the regression this pins. git resolves
/// `sharedindex.<id>` against the git directory first (read-cache.c:1893);
/// resolving it only against the directory holding the index file made every
/// such repository unreadable, with
/// `An IO error occurred while opening the index`.
#[test]
fn zvcs_reads_a_split_index_stock_git_wrote() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found — skipping split-index read test");
        return;
    };

    let repo = tmp("read");
    populated(&git, &repo);
    ok(&git, &repo, &["update-index", "--split-index"]);
    assert!(
        shared_index(&repo.join(".git")).is_some(),
        "stock git must have written a shared index for this test to mean anything"
    );

    let expected = ok(&git, &repo, &["write-tree"]);
    assert_eq!(
        ok(BIN, &repo, &["write-tree"]),
        expected,
        "zvcs must build the same tree from stock git's split index"
    );
    assert_eq!(
        ok(BIN, &repo, &["ls-files", "-s"]),
        ok(&git, &repo, &["ls-files", "-s"]),
        "zvcs must list the same entries as stock git through the link"
    );

    // The same index file, named from outside the git directory. The shared half
    // stays where it is, so only a git-directory-relative lookup can find it.
    let elsewhere = repo.join("copied-index");
    std::fs::copy(repo.join(".git/index"), &elsewhere).unwrap();
    let index_env = elsewhere.to_str().unwrap();

    let via_stock = Command::new(&git)
        .args(["write-tree"])
        .current_dir(&repo)
        .env("GIT_INDEX_FILE", index_env)
        .output()
        .unwrap();
    assert!(via_stock.status.success(), "stock git must read its own copied split index");

    let via_zvcs = Command::new(BIN)
        .args(["write-tree"])
        .current_dir(&repo)
        .env("GIT_INDEX_FILE", index_env)
        .output()
        .unwrap();
    assert!(
        via_zvcs.status.success(),
        "zvcs must read a split index named through GIT_INDEX_FILE: {}",
        String::from_utf8_lossy(&via_zvcs.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&via_zvcs.stdout),
        String::from_utf8_lossy(&via_stock.stdout),
        "both must answer the same tree for the same split index"
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// A split index zvcs wrote must survive zvcs reading it back.
///
/// This one needs no stock git: it catches a writer that produces something only
/// stock can decode, or a reader that mis-applies its own bitmaps.
#[test]
fn zvcs_round_trips_its_own_split_index() {
    let repo = tmp("roundtrip");
    populated(BIN, &repo);
    let entries = ok(BIN, &repo, &["ls-files", "-s"]);
    let tree = ok(BIN, &repo, &["write-tree"]);

    ok(BIN, &repo, &["update-index", "--split-index"]);

    assert_eq!(
        ok(BIN, &repo, &["ls-files", "-s"]),
        entries,
        "the entries must survive the split"
    );
    assert_eq!(
        ok(BIN, &repo, &["write-tree"]),
        tree,
        "the tree must survive the split"
    );

    // And a mutation through the split index must still land.
    std::fs::write(repo.join("d.txt"), b"d\n").unwrap();
    ok(BIN, &repo, &["add", "d.txt"]);
    assert_eq!(
        ok(BIN, &repo, &["ls-files", "-s"]).lines().count(),
        4,
        "a file added while split must be in the index"
    );

    let _ = std::fs::remove_dir_all(&repo);
}
