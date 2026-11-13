//! What `write_idx_file()` decides about the index `index-pack` writes.
//!
//! `--index-version=<v>[,<off32-limit>]` (builtin/index-pack.c:1991-1999) picks
//! the layout: version 1, or version 2 with every offset past the limit moved to
//! the 64-bit table (pack-write.c:40-51, :140-170). zvcs refused every value but
//! a plain `2`. Under `--strict` (`WRITE_IDX_STRICT`, builtin/index-pack.c:2058-2059)
//! an object stored twice dies `The same object <id> appears twice in the pack`
//! as the index is written (pack-write.c:131-137), leaving the index git had just
//! opened behind empty; zvcs wrote an index holding both copies.

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

fn run(bin: &str, dir: &Path, args: &[&str], stdin: &[u8]) -> (Vec<u8>, String, i32) {
    use std::io::Write;
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
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
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

/// A pack of two commits' worth of objects, and one holding the same blob twice.
fn packs(stock: &str, r: &Path) -> (Vec<u8>, Vec<u8>) {
    let ok = |args: &[&str], stdin: &[u8]| {
        let (out, err, code) = run(stock, r, args, stdin);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    };
    ok(&["init", "-q", "-b", "main", "."], b"");
    for i in 1..=2 {
        let body: String = (1..=200).map(|n| format!("line {n}\n")).collect::<String>() + &format!("rev {i}\n");
        std::fs::write(r.join("f"), body).unwrap();
        ok(&["add", "f"], b"");
        ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", &format!("c{i}")], b"");
    }
    let full = ok(&["pack-objects", "-q", "--revs", "--stdout"], b"main\n");
    let blob = ok(&["hash-object", "-w", "--stdin"], b"hello dup\n");
    let one = ok(&["pack-objects", "-q", "--stdout"], &blob);
    // The single entry, twice, under a header counting two and a fresh trailer.
    let entry = &one[12..one.len() - 20];
    let mut dup = b"PACK\0\0\0\x02\0\0\0\x02".to_vec();
    dup.extend_from_slice(entry);
    dup.extend_from_slice(entry);
    let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
    hasher.update(&dup);
    dup.extend_from_slice(hasher.try_finalize().unwrap().as_slice());
    (full, dup)
}

/// The names and sizes of the files in `dir`, a `tmp_*_XXXXXX` suffix masked.
fn listing(dir: &Path) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let mut name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("tmp_") {
                name.truncate(name.len() - 6);
            }
            (name, e.metadata().unwrap().len())
        })
        .collect();
    out.sort();
    out
}

#[test]
fn index_layout_and_strict_duplicates_match_stock() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-index-pack-idx-options-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let scratch = Scratch { root };
    let base = scratch.root.join("base");
    std::fs::create_dir_all(&base).unwrap();
    let (full, dup) = packs(stock, &base);

    // (arguments, pack, whether the pack comes on stdin)
    let cases: [(&[&str], &[u8], bool); 7] = [
        (&["--index-version=1"], &full, false),
        (&["--index-version=2,0x10"], &full, false),
        (&["--index-version=1,64"], &full, false),
        (&["--stdin", "--index-version=1"], &full, true),
        (&["--strict"], &dup, false),
        (&["--check-self-contained-and-connected", "-o", "x.idx"], &dup, false),
        (&["--stdin", "--strict"], &dup, true),
    ];
    for (n, (args, pack, stdin)) in cases.iter().enumerate() {
        let mut seen = Vec::new();
        for (side, bin) in [("stock", stock), ("zvcs", BIN)] {
            let twin = scratch.root.join(format!("{n}-{side}"));
            std::fs::create_dir_all(&twin).unwrap();
            let (_, err, code) = run(stock, &twin, &["init", "-q", "."], b"");
            assert_eq!(code, 0, "{err}");
            let mut argv = vec!["index-pack"];
            argv.extend_from_slice(args);
            let input: &[u8] = if *stdin {
                pack
            } else {
                std::fs::write(twin.join("p.pack"), pack).unwrap();
                argv.push("p.pack");
                b""
            };
            let (out, err, code) = run(bin, &twin, &argv, input);
            let mut files = listing(&twin.join(".git/objects/pack"));
            files.extend(listing(&twin).into_iter().filter(|(name, _)| name != ".git"));
            // The index bytes are a function of the pack alone.
            let idx: Vec<Vec<u8>> = files
                .iter()
                .filter(|(name, _)| name.ends_with(".idx"))
                .map(|(name, _)| {
                    let here = twin.join(name);
                    std::fs::read(if here.exists() { here } else { twin.join(".git/objects/pack").join(name) })
                        .unwrap()
                })
                .collect();
            seen.push((String::from_utf8_lossy(&out).into_owned(), err, code, files, idx));
        }
        assert_eq!(seen[1], seen[0], "index-pack {args:?}");
    }
}
