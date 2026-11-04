//! `fast-import`'s temporaries in `objects/pack/` are `odb_mkstemp()` files:
//! `git_mkstemp_mode(…, 0444)` (odb.c:72-91), an `O_CREAT | O_EXCL` open
//! retried on `EEXIST` (wrapper.c:514-572). `start_packfile()` opens
//! `tmp_pack_XXXXXX` (builtin/fast-import.c:790-791) and `write_idx_file()`
//! opens `tmp_idx_XXXXXX` (pack-write.c:86-87), and each is written through the
//! descriptor that created it, since the file is read-only from the start.
//!
//! zvcs named its temporaries from the pid and the clock's low twelve bits and
//! opened them without `O_EXCL`. On macOS the clock has microsecond resolution,
//! so those bits took 512 values, and the name it picked for a kept pack's
//! `.pack` or `.idx` was the run's own 0444 `tmp_pack_` often enough: about one
//! run in 170 of `-c fastimport.unpackLimit=0 fast-import` exited 128 with
//! `fatal: Permission denied (os error 13)`. Each `checkpoint` keeps a pack, so
//! a stream of them makes the collision likely within a few runs. Measured
//! against the pre-fix binary: 4 of 30 runs of this stream failed.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Kept below 32 packs, which is the most gitoxide's object store holds open in
/// one process: a 33rd `checkpoint` fails on zvcs for that reason alone.
const CHECKPOINTS: usize = 24;
const RUNS: usize = 60;

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One blob and one commit per `checkpoint`, so every checkpoint keeps a pack.
fn stream() -> String {
    let mut s = String::new();
    for i in 1..=CHECKPOINTS {
        let body = format!("v{i}\n");
        s += &format!("blob\nmark :{}\ndata {}\n{body}\n", 2 * i - 1, body.len());
        s += &format!(
            "commit refs/heads/main\nmark :{}\ncommitter A U Thor <a@example.com> {} +0000\ndata 0\n",
            2 * i,
            1_700_000_000 + i
        );
        if i > 1 {
            s += &format!("from :{}\n", 2 * i - 2);
        }
        s += &format!("M 100644 :{} f.txt\n\ncheckpoint\n\n", 2 * i - 1);
    }
    s
}

fn git(bin: &str, dir: &Path, home: &Path, args: &[&str], stdin: &str) -> (String, String, i32) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// The run's result plus what it left: the refs, and the names in
/// `objects/pack/` reduced to their kind, since the pack hashes depend on each
/// side's delta choices rather than on the stream.
fn import(bin: &str, stock: &str, root: &Path, tag: &str, stream: &str) -> String {
    let dir = root.join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(stock, &dir, root, &["init", "-q", "."], "");
    let (out, err, code) =
        git(bin, &dir, root, &["-c", "fastimport.unpackLimit=0", "fast-import", "--quiet"], stream);
    let mut kinds: Vec<String> = std::fs::read_dir(dir.join(".git/objects/pack"))
        .unwrap()
        .map(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            match name.strip_prefix("pack-").and_then(|n| n.rsplit_once('.')) {
                Some((_, ext)) => format!("pack-*.{ext}"),
                None => name,
            }
        })
        .collect();
    kinds.sort();
    let refs = git(stock, &dir, root, &["for-each-ref"], "").0;
    format!("code={code}\nstdout={out}\nstderr={err}\nrefs={refs}\npack={kinds:?}\n")
}

#[test]
fn kept_packs_never_reopen_the_read_only_temporary() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = Root(
        std::env::temp_dir().join(format!("zvcs-fast-import-temp-names-{}", std::process::id())),
    );
    std::fs::create_dir_all(&root.0).unwrap();
    let stream = stream();

    let expected = import(stock, stock, &root.0, "stock", &stream);
    assert!(expected.starts_with("code=0\n"), "{expected}");
    for run in 0..RUNS {
        let got = import(BIN, stock, &root.0, "zvcs", &stream);
        assert_eq!(got, expected, "run {run}");
    }
}
