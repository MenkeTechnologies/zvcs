//! `receive-pack`'s quarantine: the pushed objects are written into
//! `objects/tmp_objdir-incoming-XXXXXX` (`odb_transaction_begin(...,
//! ODB_TRANSACTION_RECEIVE)`, object-file.c:1483-1517, `tmp_objdir_create()`,
//! tmp-objdir.c:135-184), `pre-receive` alone runs with `GIT_QUARANTINE_PATH`,
//! `GIT_OBJECT_DIRECTORY` and `GIT_ALTERNATE_OBJECT_DIRECTORIES` pointing at it
//! (builtin/receive-pack.c:944-945, :2081), the directory is migrated into the
//! real store only once a ref is about to move (`tmp_objdir_migrate()`,
//! builtin/receive-pack.c:2097-2108), and the kept pack's `.keep` is deleted
//! after the commands ran (`odb_transaction_finalize()`). A declined push, or one
//! `--strict` rejects, leaves nothing behind. zvcs wrote straight into the object
//! store and gave `pre-receive` none of the three variables.
//!
//! Each case pushes the same history with stock `git push`, once into stock
//! `receive-pack` and once into this binary's, with hooks that report what they
//! see, and compares the pusher's output and the receiving repository's object
//! directory afterwards. Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

/// `pre-receive` reports the quarantine variables, its directory listing and
/// that the pushed tip is readable through them, then exits `pre_exit`; `update`
/// and `post-receive` report whether they see any of the variables and what the
/// real `objects/pack` holds. `$S` is stock git, so the store is read the same way
/// under either server.
fn hooks(stock: &str, repo: &Path, pre_exit: i32) {
    let mask = format!("s|{}|R|g; s/tmp_objdir-incoming-\\w{{6}}/Q/g; s/[0-9a-f]{{38,40}}/<H>/g", repo.display());
    let pre = format!(
        "#!/bin/sh\n\
         env | grep -E '^GIT_(QUARANTINE_PATH|OBJECT_DIRECTORY|ALTERNATE_OBJECT_DIRECTORIES)=' | sort | perl -pe '{mask}' >&2\n\
         (cd \"$GIT_QUARANTINE_PATH\" && find . | sort | perl -pe '{mask}') >&2\n\
         ls -l \"$GIT_QUARANTINE_PATH/pack\" | cut -c1-10 | sort >&2\n\
         cat \"$GIT_QUARANTINE_PATH\"/pack/*.keep 2>/dev/null | cut -d' ' -f1 >&2\n\
         while read o n r; do {stock} cat-file -t $n >&2; {stock} rev-list --objects $n | wc -l | tr -d ' ' >&2; done\n\
         exit {pre_exit}\n"
    );
    let later = format!(
        "#!/bin/sh\n\
         env | grep -cE '^GIT_(QUARANTINE_PATH|OBJECT_DIRECTORY|ALTERNATE_OBJECT_DIRECTORIES)=' >&2\n\
         ls \"$GIT_DIR/objects/pack\" | perl -pe '{mask}' >&2\n\
         exit 0\n"
    );
    for (name, body) in [("pre-receive", &pre), ("update", &later), ("post-receive", &later)] {
        let path = repo.join("hooks").join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Everything under `objects/`, with object names masked, and each pack file's mode.
fn objects_state(repo: &Path) -> String {
    let mut out = Vec::new();
    let objects = repo.join("objects");
    let mut stack = vec![objects.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(&objects).unwrap().display().to_string();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            if path.is_dir() {
                stack.push(path.clone());
                out.push(format!("{rel}/"));
            } else {
                out.push(format!("{rel} {mode:o}"));
            }
        }
    }
    out.sort();
    out.join("\n")
}

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
    objects: String,
    refs: String,
}

/// `src` holds two commits and an annotated tag; the push sends `main` and `v1`
/// into a fresh bare repository configured with `config`, whose `pre-receive`
/// exits `pre_exit`.
fn push(tag: &str, bin: &str, config: &[(&str, &str)], pre_exit: i32, bad: bool) -> Run {
    let stock = stock_git().unwrap();
    let root = std::env::temp_dir().join(format!("zvcs-rp-quarantine-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let f = Fixture { root: root.clone() };
    let ok = |cwd: &Path, args: &[&str]| -> String {
        let out = git(stock, cwd, &root, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let src = root.join("src");
    ok(&root, &["init", "-q", "-b", "main", src.to_str().unwrap()]);
    for i in 1..=2 {
        std::fs::write(src.join("f"), format!("{i}\n")).unwrap();
        ok(&src, &["add", "f"]);
        ok(&src, &["commit", "-qm", &format!("c{i}")]);
    }
    ok(&src, &["tag", "-a", "-m", "t", "v1"]);
    if bad {
        // A commit whose author line `fsck` rejects (badEmail).
        let tree = ok(&src, &["rev-parse", "HEAD^{tree}"]);
        let parent = ok(&src, &["rev-parse", "HEAD"]);
        let body = format!(
            "tree {tree}\nparent {parent}\nauthor A <a@example.com 1700000000 +0000\ncommitter A <a@example.com> 1700000000 +0000\n\nbad\n"
        );
        let file = root.join("bad-commit");
        std::fs::write(&file, body).unwrap();
        let id = ok(&src, &["hash-object", "--literally", "-t", "commit", "-w", file.to_str().unwrap()]);
        ok(&src, &["update-ref", "refs/heads/main", &id]);
    }
    let dst = root.join("dst.git");
    ok(&root, &["init", "-q", "--bare", dst.to_str().unwrap()]);
    for (k, v) in config {
        ok(&dst, &["config", k, v]);
    }
    // `receive.autoGc` off: the detached `maintenance run --auto` would race the
    // listing of `objects/` below.
    ok(&dst, &["config", "receive.autoGc", "false"]);
    hooks(stock, &dst, pre_exit);

    let receive_pack = format!("{bin} receive-pack");
    let out = git(
        stock,
        &src,
        &root,
        &["push", &format!("--receive-pack={receive_pack}"), dst.to_str().unwrap(), "main", "v1"],
    );
    let mask = |s: &[u8]| String::from_utf8_lossy(s).replace(dst.to_str().unwrap(), "R");
    let run = Run {
        stdout: mask(&out.stdout),
        stderr: mask(&out.stderr),
        code: out.status.code().unwrap(),
        objects: objects_state(&dst),
        refs: ok(&dst, &["for-each-ref"]),
    };
    drop(f);
    run
}

fn compare(tag: &str, config: &[(&str, &str)], pre_exit: i32, bad: bool) -> Run {
    let want = push(&format!("{tag}-stock"), stock_git().unwrap(), config, pre_exit, bad);
    let got = push(&format!("{tag}-zvcs"), ZVCS, config, pre_exit, bad);
    assert_eq!(want.stderr, got.stderr, "{tag}: stderr");
    assert_eq!(want.stdout, got.stdout, "{tag}: stdout");
    assert_eq!(want.code, got.code, "{tag}: exit code");
    assert_eq!(want.objects, got.objects, "{tag}: objects/");
    assert_eq!(want.refs, got.refs, "{tag}: refs");
    want
}

/// Under `receive.unpackLimit`, the objects arrive loose in the quarantine, are
/// readable from `pre-receive`, and are migrated before `update` runs.
#[test]
fn receive_pack_parity_quarantine_loose_accepted() {
    if stock_git().is_none() {
        return;
    }
    let want = compare("loose-ok", &[], 0, false);
    assert!(want.stderr.contains("GIT_QUARANTINE_PATH=R/./objects/Q"), "{}", want.stderr);
}

/// A kept pack sits in the quarantine with its `.keep`, `.rev` and modes; the
/// `.keep` follows it into `objects/pack` and is gone once the commands ran.
#[test]
fn receive_pack_parity_quarantine_kept_pack_accepted() {
    if stock_git().is_none() {
        return;
    }
    let want = compare("pack-ok", &[("receive.unpackLimit", "1")], 0, false);
    assert!(want.stderr.contains(".keep"), "{}", want.stderr);
}

/// A declined push leaves neither loose objects nor a pack behind.
#[test]
fn receive_pack_parity_quarantine_declined() {
    if stock_git().is_none() {
        return;
    }
    compare("loose-declined", &[], 1, false);
    compare("pack-declined", &[("receive.unpackLimit", "1")], 1, false);
}

/// Every command refused before `pre-receive` returns: there is nothing to
/// migrate, so the quarantine is thrown away.
#[test]
fn receive_pack_parity_quarantine_all_refused() {
    if stock_git().is_none() {
        return;
    }
    compare(
        "hidden",
        &[("receive.unpackLimit", "1"), ("receive.hideRefs", "refs/")],
        0,
        false,
    );
}

/// `--strict` rejecting an object in either child leaves the store as it was.
#[test]
fn receive_pack_parity_quarantine_fsck_failure() {
    if stock_git().is_none() {
        return;
    }
    compare("fsck-loose", &[("receive.fsckObjects", "true")], 0, true);
    compare(
        "fsck-pack",
        &[("receive.fsckObjects", "true"), ("receive.unpackLimit", "1")],
        0,
        true,
    );
}
