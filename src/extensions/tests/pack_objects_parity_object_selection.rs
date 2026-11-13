//! Which objects `pack-objects` packs, and what it leaves behind, for the options
//! that steer the set rather than the encoding.
//!
//! * An exclusion marks only what `get_object_list()` marks: the excluded
//!   commits and their history, and of trees and blobs only what
//!   `mark_edges_uninteresting()` reaches from an excluded parent of a shown
//!   commit (list-objects.c:236-306, revision.c:214-252). `--indexed-objects`
//!   after `--not` therefore still lists the index's blobs, where zvcs dropped
//!   everything the excluded history reached.
//! * `--unpacked` ignores a commit some pack holds (revision.c:4225-4241), so
//!   its tree is never walked, and drops any other packed object even when a
//!   loose copy exists (list-objects.c:38-49); `--incremental`,
//!   `--honor-pack-keep` and `--keep-pack` refuse an object a pack (a kept one)
//!   holds whatever loose copy there is (`want_found_object()`,
//!   builtin/pack-objects.c:1622-1712). zvcs judged all of them by looseness and
//!   ignored the two keep options.
//! * `--include-tag` adds the annotated tags whose peeled target is packed
//!   (:3357-3394), `--keep-unreachable` the objects of the unkept local packs
//!   (:4513-4539), and `--unpack-unreachable` writes the unreachable packed
//!   objects out loose (:4549-4577); zvcs did none of the three.
//! * `--index-version=<v>,<off32-limit>` moves every offset past the limit to the
//!   64-bit table (pack-write.c:40-51); zvcs ignored the limit.
//! * A `--filter` only applies to what the traversal discovers, so a bare object
//!   list is packed whole.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Scratch {
    root: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, dir: &Path, args: &[&str], stdin: &[u8], date: u64) -> (Vec<u8>, String, i32) {
    use std::io::Write;
    let stamp = format!("{date} +0000");
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", &stamp)
        .env("GIT_COMMITTER_DATE", &stamp)
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
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

/// Six commits over a 200-line file, a branch two back, an annotated tag three
/// back, a dropped commit only the reflog holds and a stray blob, all packed;
/// then two loose commits and a third that is loose *and* alone in a `.keep`
/// pack.
fn prepare(stock: &str, r: &Path) {
    let mut date = 1_700_000_000;
    let mut git = |args: &[&str], stdin: &[u8]| {
        date += 60;
        let (out, err, code) = run(stock, r, args, stdin, date);
        assert_eq!(code, 0, "{args:?}: {err}");
        String::from_utf8(out).unwrap()
    };
    git(&["init", "-q", "-b", "main", "."], b"");
    for i in 1..=6 {
        let body: String = (1..=200).map(|n| format!("line {n}\n")).collect::<String>() + &format!("rev {i}\n");
        std::fs::write(r.join("big"), body).unwrap();
        std::fs::create_dir_all(r.join("d")).unwrap();
        std::fs::write(r.join("d/f"), format!("dir {i}\n")).unwrap();
        git(&["add", "-A"], b"");
        git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", &format!("c{i}")], b"");
    }
    git(&["branch", "side", "HEAD~2"], b"");
    git(&["tag", "-a", "-m", "ann", "ann", "HEAD~3"], b"");
    std::fs::write(r.join("drop"), "dropped\n").unwrap();
    git(&["add", "drop"], b"");
    git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "dropped"], b"");
    git(&["reset", "-q", "--hard", "HEAD~1"], b"");
    git(&["hash-object", "-w", "--stdin"], b"stray\n");
    git(&["-c", "maintenance.auto=false", "repack", "-q", "-a", "-d"], b"");
    for i in 7..=9 {
        std::fs::write(r.join(format!("n{i}")), format!("new {i}\n")).unwrap();
        git(&["add", "-A"], b"");
        git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", &format!("c{i}")], b"");
    }
    let head = git(&["rev-parse", "HEAD"], b"");
    let name = git(&["pack-objects", "-q", ".git/objects/pack/pack"], head.as_bytes());
    std::fs::write(r.join(format!(".git/objects/pack/pack-{}.keep", name.trim())), "").unwrap();
}

/// The ids a pack holds, by way of stock `index-pack` and `show-index`.
fn ids_in(stock: &str, scratch: &Path, pack: &[u8]) -> Vec<String> {
    let _ = std::fs::remove_dir_all(scratch);
    std::fs::create_dir_all(scratch).unwrap();
    std::fs::write(scratch.join("p.pack"), pack).unwrap();
    let (_, err, code) = run(stock, scratch, &["index-pack", "p.pack"], b"", 0);
    assert_eq!(code, 0, "{err}");
    let idx = std::fs::read(scratch.join("p.idx")).unwrap();
    let (out, _, _) = run(stock, scratch, &["show-index"], &idx, 0);
    let mut ids: Vec<String> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| l.split(' ').nth(1).unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

/// The loose objects of a repository, sorted.
fn loose(r: &Path) -> Vec<String> {
    let objects = r.join(".git/objects");
    let mut out = Vec::new();
    for dir in std::fs::read_dir(&objects).unwrap() {
        let dir = dir.unwrap();
        let name = dir.file_name().to_string_lossy().into_owned();
        if name.len() != 2 {
            continue;
        }
        for file in std::fs::read_dir(dir.path()).unwrap() {
            out.push(format!("{name}{}", file.unwrap().file_name().to_string_lossy()));
        }
    }
    out.sort();
    out
}

struct Case<'a> {
    args: &'a [&'a str],
    stdin: &'a str,
}

#[test]
fn object_selection_matches_stock() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-pack-objects-selection-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let scratch = Scratch { root: root.clone() };
    let base = scratch.root.join("base");
    std::fs::create_dir_all(&base).unwrap();
    prepare(stock, &base);
    let head = String::from_utf8(run(stock, &base, &["rev-parse", "HEAD"], b"", 0).0).unwrap();
    let kept_pack = std::fs::read_dir(base.join(".git/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(".keep"))
        .unwrap()
        .replace(".keep", ".pack");
    let keep_pack = format!("--keep-pack={kept_pack}");
    let objects = String::from_utf8(run(stock, &base, &["rev-list", "--objects", "main"], b"", 0).0).unwrap();

    let cases = [
        Case { args: &["--revs", "--include-tag"], stdin: "main\n" },
        Case { args: &["--revs", "--indexed-objects"], stdin: "--not\nside\nmain\n" },
        Case { args: &["--revs", "--indexed-objects"], stdin: "main~4\n^side\nann\n" },
        Case { args: &["--revs"], stdin: "main\n^side\n" },
        Case { args: &["--revs", "--no-sparse"], stdin: "side..main\n" },
        Case { args: &["--revs", "--unpacked"], stdin: "main\n" },
        Case { args: &["--revs", "--incremental"], stdin: "main\n" },
        Case { args: &["--revs", "--honor-pack-keep"], stdin: "main\n" },
        Case { args: &["--revs", "--all", &keep_pack], stdin: "" },
        Case { args: &["--revs", "--keep-unreachable"], stdin: "main\n" },
        Case { args: &["--incremental"], stdin: &head },
        Case { args: &["--filter=blob:none"], stdin: &objects },
    ];
    for (n, case) in cases.iter().enumerate() {
        let mut argv = vec!["pack-objects", "-q", "--stdout"];
        argv.extend_from_slice(case.args);
        let mut seen = Vec::new();
        for bin in [stock, BIN] {
            let (pack, err, code) = run(bin, &base, &argv, case.stdin.as_bytes(), 0);
            assert_eq!((err.as_str(), code), ("", 0), "{bin} {argv:?}");
            seen.push(ids_in(stock, &scratch.root.join(format!("ids{n}")), &pack));
        }
        assert_eq!(seen[1], seen[0], "{argv:?} <<< {:?}", case.stdin);
    }

    // `--unpack-unreachable` loosens the unreachable packed objects, with `--stdout` too.
    // A date lets the old ones go, but only beside `--reflog` and `--indexed-objects`
    // (builtin/pack-objects.c:5396-5397); every file here is older than "now".
    let extras: [&[&str]; 3] = [
        &["--unpack-unreachable"],
        &["--unpack-unreachable=now"],
        &["--reflog", "--indexed-objects", "--unpack-unreachable=now"],
    ];
    for extra in extras {
        let mut after = Vec::new();
        for (side, bin) in [("stock", stock), ("zvcs", BIN)] {
            let twin = scratch.root.join(format!("unpack-{side}"));
            let _ = std::fs::remove_dir_all(&twin);
            copy_tree(&base, &twin);
            let mut argv = vec!["pack-objects", "-q", "--revs", "--all", "--stdout"];
            argv.extend_from_slice(extra);
            let (_, err, code) = run(bin, &twin, &argv, b"", 0);
            assert_eq!((err.as_str(), code), ("", 0), "{bin} {argv:?}");
            after.push(loose(&twin));
        }
        assert_eq!(after[1], after[0], "{extra:?}");
    }

    // Every offset past 0x40 goes to the 64-bit table, which is what the `.idx` length
    // counts; the packs themselves need not be byte-identical for that.
    let mut idx = Vec::new();
    for (side, bin) in [("stock", stock), ("zvcs", BIN)] {
        let base_name = scratch.root.join(format!("limit-{side}"));
        let base_name = base_name.to_str().unwrap();
        let argv = ["pack-objects", "-q", "--revs", "--threads=1", "--index-version=2,0x40", base_name];
        let (out, err, code) = run(bin, &base, &argv, b"main\n", 0);
        assert_eq!((err.as_str(), code), ("", 0), "{bin}");
        let name = String::from_utf8(out).unwrap();
        idx.push(std::fs::metadata(format!("{base_name}-{}.idx", name.trim())).unwrap().len());
    }
    assert_eq!(idx[1], idx[0], "--index-version=2,0x40 .idx length");
}
