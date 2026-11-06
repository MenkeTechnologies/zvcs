//! `fsck` over a pack `is_pack_valid()` refuses: a truncated pack whose trailing checksum no
//! longer matches its `.idx`, reported as `packfile %s does not match index` on every open
//! (packfile.c:529-603, :1820-1838).
//!
//! The full scan's `verify_packfile()` gives up on such a pack before its walk
//! (pack-check.c:69-70), so none of its objects gets `HAS_OBJ`: the reflog entries naming them
//! are `invalid reflog entry`, the commit naming one is a `broken link` whose
//! `odb_has_object(..., ODB_HAS_OBJECT_RECHECK_PACKED)` opens the pack twice, and
//! `check_reachable_object()`'s `has_object_pack()` opens it once more for each `missing`
//! object (builtin/fsck.c:113-175, :246-262, :454-475). Under `--verbose` the refused pack's
//! lines sit between the objects of the packs on either side of it in list order.
//!
//! `--connectivity-only` lists the pack's objects from the `.idx` and gives every one
//! `HAS_OBJ` (builtin/fsck.c:795-803); the walk then reads them: a typed one is `Could not
//! read`, an untyped one `parse_object()`d into `Unknown object type for` (fsck.c:482-503,
//! commit.c:638-645), and `printable_type()` names one it cannot read `unknown`
//! (builtin/fsck.c:56-70), as in `dangling unknown <oid>`.
//!
//! A pack failing makes the full scan's exit code `ERROR_PACK` in this port and
//! `ERROR_OBJECT` in git 2.56; that difference is a pending owner decision (see
//! fsck_parity_corrupt_pack.rs), so the scans that verify packs compare stdout and stderr only.
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip
//! when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-unusable-pack-{label}-{}", std::process::id()));
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
        .stdin(std::process::Stdio::null());
    c
}

fn setup(stock: &str, dir: &Path, args: &[&str]) -> String {
    let out = cmd(stock, dir, args).output().unwrap();
    assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = cmd(bin, dir, args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// stdout, stderr and the exit code.
#[track_caller]
fn same(stock: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let theirs = run(stock, dir, args);
    let ours = run(ZVCS, dir, args);
    assert_eq!(ours, theirs, "git {args:?} in {} (left zvcs, right stock)", dir.display());
    theirs
}

/// stdout and stderr; the exit code is the pending `ERROR_PACK` question.
#[track_caller]
fn same_streams(stock: &str, dir: &Path, args: &[&str]) -> (String, String) {
    let (_, theirs_out, theirs_err) = run(stock, dir, args);
    let (_, ours_out, ours_err) = run(ZVCS, dir, args);
    assert_eq!(
        (&ours_out, &ours_err),
        (&theirs_out, &theirs_err),
        "git {args:?} in {} (left zvcs, right stock)",
        dir.display()
    );
    (theirs_out, theirs_err)
}

/// Three commits of a file each, each repacked into a pack of its own and the loose copies
/// pruned, then the first commit's pack cut short by nine bytes, which leaves its trailing
/// checksum unequal to the one its `.idx` records. `midx` writes a multi-pack-index first.
fn truncated_first_pack(stock: &str, root: &Path, name: &str, midx: bool) -> (PathBuf, String) {
    setup(stock, root, &["init", "-q", "-b", "main", name]);
    let repo = root.join(name);
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    let mut first = String::new();
    for i in 0..3 {
        std::fs::write(repo.join(format!("f{i}")), format!("file {i}\n")).unwrap();
        setup(stock, &repo, &["add", "-A"]);
        setup(stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
        setup(stock, &repo, &["repack", "-q"]);
        if i == 0 {
            first = setup(stock, &repo, &["rev-parse", "HEAD"]);
        }
    }
    setup(stock, &repo, &["prune-packed"]);
    if midx {
        setup(stock, &repo, &["multi-pack-index", "write"]);
    }
    let pack = pack_of(stock, &repo, &first);
    let mut perms = std::fs::metadata(&pack).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&pack, perms).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&pack).unwrap();
    let len = file.metadata().unwrap().len();
    file.set_len(len - 9).unwrap();
    (repo, first)
}

/// The `.pack` whose own `.idx` lists `id`.
fn pack_of(stock: &str, repo: &Path, id: &str) -> PathBuf {
    for entry in std::fs::read_dir(repo.join(".git/objects/pack")).unwrap() {
        let idx = entry.unwrap().path();
        if idx.extension().is_none_or(|x| x != "idx") {
            continue;
        }
        let out = Command::new(stock)
            .arg("show-index")
            .stdin(std::fs::File::open(&idx).unwrap())
            .output()
            .unwrap();
        if String::from_utf8_lossy(&out.stdout).lines().any(|l| l.split_whitespace().nth(1) == Some(id)) {
            return idx.with_extension("pack");
        }
    }
    panic!("{id} is in no pack");
}

#[test]
fn full_scan_reports_what_the_refused_pack_held() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("full");
    let (repo, first) = truncated_first_pack(&stock, &root, "r", false);

    let (out, err) = same_streams(&stock, &repo, &["fsck"]);
    assert!(out.contains(&format!("missing commit {first}")), "{out}");
    assert!(err.contains(&format!("error: HEAD: invalid reflog entry {first}")), "{err}");
    assert_eq!(err.matches("does not match index").count(), 5, "{err}");

    for args in [
        &["fsck", "--verbose"][..],
        &["fsck", "--name-objects"],
        &["fsck", "--unreachable"],
        &["fsck", "--lost-found"],
        &["fsck", "--no-reflogs"],
    ] {
        same_streams(&stock, &repo, args);
    }
    // `--no-full` verifies no pack, so the exit code is git's too.
    let (_, out, _) = same(&stock, &repo, &["fsck", "--no-full"]);
    assert!(out.contains("missing blob"), "{out}");
    same(&stock, &repo, &["fsck", "--no-full", "--verbose"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn full_scan_places_the_refused_pack_among_the_multi_pack_index_packs() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("midx");
    let (repo, _) = truncated_first_pack(&stock, &root, "r", true);
    same_streams(&stock, &repo, &["fsck", "--verbose"]);
    same_streams(&stock, &repo, &["fsck"]);
    same(&stock, &repo, &["fsck", "--no-full"]);
    same(&stock, &repo, &["fsck", "--connectivity-only"]);
    same(&stock, &repo, &["fsck", "--connectivity-only", "--verbose"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn connectivity_only_reads_what_the_index_lists() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("conn");
    let (repo, first) = truncated_first_pack(&stock, &root, "r", false);

    let (_, out, err) = same(&stock, &repo, &["fsck", "--connectivity-only"]);
    assert!(out.starts_with("dangling unknown "), "{out}");
    assert!(err.contains(&format!("error: Could not read {first}")), "{err}");
    assert!(err.contains("error: Unknown object type for "), "{err}");

    let (_, out, _) = same(&stock, &repo, &["fsck", "--connectivity-only", "--unreachable"]);
    assert!(out.starts_with("unreachable unknown "), "{out}");
    for args in [
        &["fsck", "--connectivity-only", "--verbose"][..],
        &["fsck", "--connectivity-only", "--name-objects"],
        &["fsck", "--connectivity-only", "--no-dangling"],
        &["fsck", "--connectivity-only", "--lost-found"],
    ] {
        same(&stock, &repo, args);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_ref_tip_in_the_refused_pack_is_an_invalid_sha1_pointer() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("tip");
    setup(&stock, &root, &["init", "-q", "-b", "main", "r"]);
    let repo = root.join("r");
    for i in 0..3 {
        std::fs::write(repo.join(format!("f{i}")), format!("file {i}\n")).unwrap();
        setup(&stock, &repo, &["add", "-A"]);
        setup(&stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    setup(&stock, &repo, &["repack", "-q", "-a", "-d"]);
    let old: Vec<PathBuf> = std::fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    // The tip again, alone in a pack of its own that is then cut short, and only there.
    let tip = setup(&stock, &repo, &["rev-parse", "HEAD"]);
    let mut c = cmd(&stock, &repo, &["pack-objects", "-q", ".git/objects/pack/tip"]);
    c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null());
    let mut child = c.spawn().unwrap();
    use std::io::Write;
    writeln!(child.stdin.take().unwrap(), "{tip}").unwrap();
    assert!(child.wait().unwrap().success());
    let all = setup(&stock, &repo, &["rev-list", "--objects", "--all"]);
    let mut c = cmd(&stock, &repo, &["pack-objects", "-q", ".git/objects/pack/pack"]);
    c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null());
    let mut child = c.spawn().unwrap();
    let rest: Vec<&str> = all.lines().map(|l| &l[..40]).filter(|id| *id != tip).collect();
    writeln!(child.stdin.take().unwrap(), "{}", rest.join("\n")).unwrap();
    assert!(child.wait().unwrap().success());
    for path in old {
        std::fs::remove_file(path).unwrap();
    }
    let pack = pack_of(&stock, &repo, &tip);
    let mut perms = std::fs::metadata(&pack).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&pack, perms).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&pack).unwrap();
    let len = file.metadata().unwrap().len();
    file.set_len(len - 9).unwrap();

    let (_, _, err) = same(&stock, &repo, &["fsck", "--no-full"]);
    assert!(err.contains(&format!("error: refs/heads/main: invalid sha1 pointer {tip}")), "{err}");
    same(&stock, &repo, &["fsck", "--connectivity-only"]);
    same_streams(&stock, &repo, &["fsck"]);
    let _ = std::fs::remove_dir_all(&root);
}
