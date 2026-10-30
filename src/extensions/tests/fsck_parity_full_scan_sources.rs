//! What `git fsck`'s object scan visits when the odb holds an object more than once, and when
//! it has more than one source.
//!
//! `odb_fsck()` asks every source in turn (odb.c:1161-1167). `odb_source_files_fsck()` skips a
//! non-local source unless `--full`, then runs the source's loose half and then its packed half
//! (odb/source-files.c:912-925): `odb_source_loose_fsck()` prints the verbose `Checking object
//! directory` header and walks the loose objects (odb/source-loose.c:1108-1130), and
//! `verify_packs()` hands every packed copy, pack by pack and in pack-offset order, to
//! `fsck_obj_buffer()` (odb/source-packed.c:893-919). So an id held twice — loose and packed,
//! in two packs, or in two sources — is checked once per copy: its `Checking` line, its `root`
//! or `tagged` line and its `lookup_object()` move all repeat. With an alternate the header
//! repeats per source and each source's packs come before the next source's loose objects;
//! under `--no-full` the alternate is not scanned at all, so its loose objects never get
//! `HAS_OBJ` and a reachable one is `missing` unless a pack holds it (`has_object_pack()`,
//! builtin/fsck.c:265-275).
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip
//! when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-sources-{label}-{}", std::process::id()));
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
        .stdin(std::process::Stdio::piped());
    c
}

/// Run stock git for fixture setup, feeding `input`; it must succeed. Returns stdout.
fn setup_in(stock: &str, dir: &Path, args: &[&str], input: &str) -> String {
    use std::io::Write;
    let mut child = cmd(stock, dir, args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn setup(stock: &str, dir: &Path, args: &[&str]) -> String {
    setup_in(stock, dir, args, "")
}

/// `(code, stdout, stderr)` of one run.
fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = cmd(bin, dir, args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Both binaries agree on exit code, stdout and stderr; stock's triple is returned.
#[track_caller]
fn same(stock: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let theirs = run(stock, dir, args);
    let ours = run(ZVCS, dir, args);
    assert_eq!(ours, theirs, "git {args:?} (left zvcs, right stock)");
    theirs
}

fn init(stock: &str, root: &Path, name: &str) -> PathBuf {
    setup(stock, root, &["init", "-q", "-b", "main", name]);
    let repo = root.join(name);
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    repo
}

/// `n` commits of a fresh file each, plus an annotated tag on the first.
fn history(stock: &str, repo: &Path, n: usize) {
    for i in 0..n {
        std::fs::write(repo.join(format!("f{i}")), format!("file {i}\n")).unwrap();
        setup(stock, repo, &["add", "-A"]);
        setup(stock, repo, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    setup(stock, repo, &["tag", "-a", "-m", "t", "v1", &format!("HEAD~{}", n - 1)]);
}

const FORMS: [&[&str]; 7] = [
    &["fsck"],
    &["fsck", "--verbose"],
    &["fsck", "--unreachable", "--root", "--tags"],
    &["fsck", "--no-reflogs", "--root", "--tags", "--unreachable"],
    &["fsck", "--no-full", "--verbose", "--unreachable"],
    &["fsck", "--no-full", "--root"],
    &["fsck", "--connectivity-only", "--unreachable"],
];

#[test]
fn every_copy_of_a_duplicated_object_is_checked() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("dup");

    // Two packs sharing the older half of the history.
    let repo = init(stock, &root, "two-packs");
    history(stock, &repo, 6);
    setup(stock, &repo, &["repack", "-q", "-a", "-d"]);
    let ids = setup(stock, &repo, &["rev-list", "--objects", "--no-object-names", "HEAD~3", "v1"]);
    setup_in(stock, &repo, &["pack-objects", "-q", ".git/objects/pack/pack"], &format!("{ids}\n"));
    setup_in(stock, &repo, &["hash-object", "-w", "--stdin"], "dangling\n");
    for args in FORMS {
        same(stock, &repo, args);
    }
    let (_, out, _) = same(stock, &repo, &["fsck", "--root"]);
    assert_eq!(out.lines().filter(|l| l.starts_with("root ")).count(), 2, "{out}");

    // Loose objects that a pack holds as well: `repack -a` without `-d`.
    let repo = init(stock, &root, "loose-and-packed");
    history(stock, &repo, 5);
    setup(stock, &repo, &["repack", "-q", "-a"]);
    for args in FORMS {
        same(stock, &repo, args);
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn each_source_is_scanned_loose_half_then_packed_half() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("alt");

    let base = init(stock, &root, "base");
    history(stock, &base, 3);
    setup(stock, &base, &["repack", "-q", "-a", "-d"]);
    std::fs::write(base.join("late"), "late\n").unwrap();
    setup(stock, &base, &["add", "late"]);
    setup(stock, &base, &["commit", "-q", "-m", "late"]);

    // A `--shared` clone: its HEAD reaches loose and packed objects of the alternate.
    setup(stock, &root, &["clone", "-q", "--shared", "base", "shared"]);
    let shared = root.join("shared");
    setup(stock, &shared, &["config", "gc.auto", "0"]);
    std::fs::write(shared.join("own"), "own\n").unwrap();
    setup(stock, &shared, &["add", "own"]);
    setup(stock, &shared, &["commit", "-q", "-m", "own"]);
    setup_in(stock, &shared, &["hash-object", "-w", "--stdin"], "own dangling\n");
    for args in FORMS {
        same(stock, &shared, args);
    }
    let (code, out, err) = same(stock, &shared, &["fsck", "--no-full"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(out.lines().any(|l| l.starts_with("missing commit ")), "{out}");
    let (_, _, err) = same(stock, &shared, &["fsck", "--verbose"]);
    assert_eq!(err.matches("Checking object directory\n").count(), 2, "{err}");

    // A clone that copied the alternate's objects into a pack of its own, so every one of them
    // is held by both sources, while the alternate still has them loose as well.
    setup(stock, &base, &["repack", "-q", "-a"]);
    setup(stock, &root, &["clone", "-q", "--shared", "base", "copied"]);
    let copied = root.join("copied");
    setup(stock, &copied, &["config", "gc.auto", "0"]);
    std::fs::write(copied.join("mine"), "mine\n").unwrap();
    setup(stock, &copied, &["add", "mine"]);
    setup(stock, &copied, &["commit", "-q", "-m", "mine"]);
    setup(stock, &copied, &["repack", "-q", "-a"]);
    for args in FORMS {
        same(stock, &copied, args);
    }

    let _ = std::fs::remove_dir_all(&root);
}
