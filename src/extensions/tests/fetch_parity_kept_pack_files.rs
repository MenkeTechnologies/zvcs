//! A pack `fetch` keeps is finished the way `index-pack` finishes it.
//!
//! `--keep` (and any pack at or over `fetch.unpackLimit`) takes the `index-pack` route in
//! `get_pack()`, which leaves `pack-<hash>.pack` and `.idx` read-only and writes
//! `pack-<hash>.rev` while `pack.writeReverseIndex` is on (the default). zvcs's fetch wrote the pack
//! and the index with the process umask (`-rw-------`) and no reverse index, so the next
//! `ls objects/pack` differed from git's in two ways.
//!
//! Stock git is the oracle (`support/stock_git.rs`); pack names are normalised because the
//! pack bytes themselves are not required to match.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let root = dir.ancestors().find(|p| p.ends_with("work")).and_then(Path::parent).unwrap_or(dir);
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).replace(root.to_str().unwrap(), "<root>"),
        out.status.code().expect("no signal"),
    )
}

/// `root/up` has two commits more than `root/work`, which was cloned from it.
fn world(label: &str, stock: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-fetch-kept-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let run = |dir: &Path, args: &[&str]| {
        let out = git(stock, dir, args);
        assert_eq!(out.2, 0, "{args:?}: {out:?}");
    };
    run(&root, &["init", "-q", "-b", "main", "up"]);
    let up = root.join("up");
    for n in 0..3 {
        std::fs::write(up.join("f"), format!("{n}\n")).unwrap();
        run(&up, &["add", "f"]);
        run(&up, &["commit", "-q", "-m", &format!("c{n}")]);
        if n == 0 {
            run(&root, &["clone", "-q", "up", "work"]);
        }
    }
    root.join("work")
}

/// `(extension, octal mode)` of every file under `objects/pack`, sorted.
fn pack_files(work: &Path) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = std::fs::read_dir(work.join(".git/objects/pack"))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let ext = e.path().extension().unwrap().to_string_lossy().into_owned();
            let mode = format!("{:o}", e.metadata().unwrap().permissions().mode() & 0o777);
            (ext, mode)
        })
        .collect();
    files.sort();
    files
}

fn compare(label: &str, args: &[&str]) -> Vec<(String, String)> {
    let stock = stock_git::stock_git().expect("checked by caller");
    let s = world(&format!("{label}-stock"), stock);
    let z = world(&format!("{label}-zvcs"), stock);
    let want = git(stock, &s, args);
    let got = git(ZVCS, &z, args);
    // The detached `maintenance run --auto` a fetch may start must not race the listing.
    let (want_files, got_files) = (pack_files(&s), pack_files(&z));
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
    assert_eq!(got, want, "git {args:?}: left is zvcs, right is stock");
    assert_eq!(got_files, want_files, "git {args:?}: objects/pack, left is zvcs, right is stock");
    want_files
}

#[test]
fn keep_leaves_a_read_only_pack_with_its_reverse_index() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let files = compare("keep", &["fetch", "--keep"]);
    assert!(files.iter().any(|(ext, mode)| ext == "rev" && mode == "444"), "{files:?}");
}

#[test]
fn a_pack_over_the_unpack_limit_is_finished_alike() {
    if stock_git::stock_git().is_none() {
        return;
    }
    compare("limit", &["-c", "fetch.unpackLimit=1", "fetch"]);
}

#[test]
fn the_reverse_index_follows_pack_write_reverse_index() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let files = compare("norev", &["-c", "pack.writeReverseIndex=false", "fetch", "--keep"]);
    assert!(files.iter().all(|(ext, _)| ext != "rev"), "{files:?}");
}

#[test]
fn a_small_pack_is_still_exploded_into_loose_objects() {
    if stock_git::stock_git().is_none() {
        return;
    }
    compare("small", &["fetch"]);
}
