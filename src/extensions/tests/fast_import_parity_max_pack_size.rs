//! `fast-import` writes git's own pack bytes and cycles to a new pack where
//! git does.
//!
//! `store_object()` deflates each object, or its `OBJ_OFS_DELTA` against the
//! last object of its kind, at `pack_compression_level`, and closes the pack
//! with `cycle_packfile()` when the entry would take it past `max_packsize`,
//! dropping the delta (builtin/fast-import.c:1043-1065); `stream_blob()` checks
//! the inflated length first and deflates in 64 KiB pieces (:1136-1200). Every
//! pack `end_packfile()` closes is kept or exploded on its own object count and
//! named by the hash of its bytes (:888-963). zvcs handed the whole run's
//! objects to the `pack-objects` writer as one pack at the end, so the pack
//! names, the number of packs, the `--export-pack-edges` lines and the loose
//! objects a small pack leaves all differed.
//!
//! Each case imports the same stream into two fresh repositories, one with stock
//! git and one with zvcs, and compares stdout, stderr (less the `pack_report`
//! window counters, which count `packfile.c` mmap windows zvcs does not have),
//! the exit code, the pack file names, the loose objects, the edges file, and
//! what stock `for-each-ref` and `fsck` say afterwards.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str], stdin: &[u8]) -> (Vec<u8>, String, i32) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Written from another thread: `cat-blob` answers on stdout while the stream
    // is still arriving, and a full stdout pipe would stall both sides.
    let mut pipe = child.stdin.take().unwrap();
    let input = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = pipe.write_all(&input);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().unwrap_or(-1))
}

/// A deterministic byte stream, so blobs neither compress away nor repeat.
fn noise(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            b' ' + (x % 90) as u8
        })
        .collect()
}

fn blob(s: &mut Vec<u8>, mark: u32, data: &[u8]) {
    s.extend_from_slice(format!("blob\nmark :{mark}\ndata {}\n", data.len()).as_bytes());
    s.extend_from_slice(data);
    s.push(b'\n');
}

/// Random blobs, then commits that add them one per commit across two
/// branches, with near-identical inline blobs that deltify against each other,
/// a repeated blob, `cat-blob`, `ls` and tags in between.
fn stream(commits: u32, blob_len: usize) -> Vec<u8> {
    let mut s = Vec::new();
    for i in 1..=commits {
        blob(&mut s, i, &noise(i, blob_len));
    }
    let base = noise(9999, 3000);
    for i in 1..=commits {
        let branch = if i % 3 == 0 { "side" } else { "main" };
        s.extend_from_slice(
            format!(
                "commit refs/heads/{branch}\nmark :{}\ncommitter A <a@example.com> {} +0000\ndata 3\nc{:<1}\n",
                1000 + i,
                1_700_000_000 + i,
                i % 10
            )
            .as_bytes(),
        );
        s.extend_from_slice(format!("M 100644 :{i} f{i}\n").as_bytes());
        let mut near = base.clone();
        near[(i as usize * 37) % 2900..][..8].copy_from_slice(format!("{:08}", i).as_bytes());
        s.extend_from_slice(format!("M 100644 inline d/near\ndata {}\n", near.len()).as_bytes());
        s.extend_from_slice(&near);
        s.extend_from_slice(b"\nM 100644 inline same\ndata 5\nsame\n\n\n");
        if i % 4 == 0 {
            s.extend_from_slice(format!("cat-blob :{i}\nls :{} d\n", 1000 + i).as_bytes());
        }
        if i % 5 == 0 {
            s.extend_from_slice(
                format!("tag v{i}\nfrom :{}\ntagger T <t@example.com> 1700000000 +0000\ndata 2\nt\n", 1000 + i)
                    .as_bytes(),
            );
        }
    }
    s
}

/// Everything about one run that stock and zvcs must agree on.
fn observe(stock: &str, repo: &Path, out: (Vec<u8>, String, i32)) -> String {
    let (stdout, stderr, code) = out;
    let stderr: String = stderr
        .lines()
        .filter(|l| {
            !["pack_used_ctr", "pack_mmap_calls", "pack_open_windows", "pack_mapped"]
                .iter()
                .any(|c| l.starts_with(&format!("pack_report: {c} ")))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    let mut packs: Vec<String> = std::fs::read_dir(repo.join(".git/objects/pack"))
        .map(|rd| rd.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    packs.sort();
    let mut loose = Vec::new();
    for fan in std::fs::read_dir(repo.join(".git/objects")).unwrap().filter_map(Result::ok) {
        let name = fan.file_name().to_string_lossy().into_owned();
        if name.len() == 2 {
            for obj in std::fs::read_dir(fan.path()).unwrap().filter_map(Result::ok) {
                loose.push(format!("{name}{}", obj.file_name().to_string_lossy()));
            }
        }
    }
    loose.sort();
    let edges = std::fs::read_to_string(repo.join("../edges")).unwrap_or_default();
    let refs = run(stock, repo, &["for-each-ref"], b"");
    let fsck = run(stock, repo, &["fsck"], b"");
    format!(
        "stdout: {} bytes, fnv {:016x}\nstderr:\n{stderr}exit {code}\npacks {packs:?}\nloose {loose:?}\nedges:\n{edges}refs:\n{}fsck: {} {}\n",
        stdout.len(),
        stdout.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)),
        String::from_utf8_lossy(&refs.0),
        fsck.1,
        fsck.2
    )
}

struct Twins(PathBuf);

impl Drop for Twins {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Import `input` with `config` and `args` on both sides; `seed`, when given,
/// is imported first by stock git (packed) so the run reads an existing history.
fn compare(tag: &str, config: &[&str], args: &[&str], input: &[u8], seed: Option<&[u8]>) -> Option<String> {
    let stock = stock_git()?;
    let root = std::env::temp_dir().join(format!("zvcs-fi-maxpack-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let twins = Twins(root);
    let mut seen = Vec::new();
    for (side, bin) in [("stock", stock), ("zvcs", ZVCS)] {
        let repo = twins.0.join(side).join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert_eq!(run(stock, &repo, &["init", "-q", "-b", "main"], b"").2, 0);
        if let Some(seed) = seed {
            let seeded = run(stock, &repo, &["-c", "fastimport.unpackLimit=0", "fast-import", "--quiet"], seed);
            assert_eq!(seeded.2, 0, "{}", seeded.1);
        }
        let mut argv: Vec<String> = Vec::new();
        for c in config {
            argv.push("-c".into());
            argv.push((*c).into());
        }
        argv.push("fast-import".into());
        argv.push(format!("--export-pack-edges={}", repo.join("../edges").display()));
        argv.extend(args.iter().map(|a| a.to_string()));
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let out = run(bin, &repo, &argv, input);
        seen.push(observe(stock, &repo, out));
    }
    assert_eq!(seen[1], seen[0], "zvcs (left) vs stock (right) for {tag}");
    Some(seen.swap_remove(0))
}

#[test]
fn pack_size_limit_cycles_where_git_cycles() {
    let input = stream(24, 2500);
    for (tag, config) in [
        ("kept", &["pack.packSizeLimit=12000", "fastimport.unpackLimit=0"][..]),
        ("some-loose", &["pack.packSizeLimit=12000", "fastimport.unpackLimit=4"]),
        ("level0", &["pack.packSizeLimit=40000", "fastimport.unpackLimit=0", "core.compression=0"]),
        ("level9", &["pack.packSizeLimit=20000", "fastimport.unpackLimit=0", "pack.compression=9", "core.compression=1"]),
        ("idx1", &["pack.packSizeLimit=20000", "fastimport.unpackLimit=0", "pack.indexVersion=1"]),
        ("one-pack", &["fastimport.unpackLimit=0"]),
        ("default", &[]),
    ] {
        let Some(seen) = compare(tag, config, &[], &input, None) else { return };
        if tag == "kept" {
            let packs = seen.lines().find(|l| l.starts_with("packs ")).unwrap();
            assert!(packs.matches(".pack\"").count() > 3, "{packs}");
        }
    }
    let Some(_) = compare("depth", &["pack.packSizeLimit=15000", "fastimport.unpackLimit=0"], &["--depth=2"], &input, None)
    else {
        return;
    };
}

#[test]
fn streamed_blobs_and_the_command_line_limit() {
    // 40 blobs of 100 kB cross the 1 MiB floor of --max-pack-size several times.
    let input = stream(40, 100_000);
    for (tag, config, args) in [
        ("cli", &["fastimport.unpackLimit=0"][..], &["--max-pack-size=1m"][..]),
        ("cli-mib", &["fastimport.unpackLimit=0"], &["--max-pack-size=1"]),
        ("streamed", &["fastimport.unpackLimit=0", "core.bigFileThreshold=50000"], &["--max-pack-size=1200000"]),
        ("streamed-loose", &["fastimport.unpackLimit=2", "core.bigFileThreshold=70000", "core.compression=3"], &[
            "--max-pack-size=1200000",
        ]),
    ] {
        if compare(tag, config, args, &input, None).is_none() {
            return;
        }
    }
    let mut option = b"option git max-pack-size=1500000\n".to_vec();
    option.extend_from_slice(&input);
    compare("option", &["fastimport.unpackLimit=5"], &[], &option, None);

    // A streamed duplicate is truncated away again.
    let big = noise(7, 300_000);
    let mut dup = Vec::new();
    blob(&mut dup, 1, &big);
    blob(&mut dup, 2, &big);
    dup.extend_from_slice(b"commit refs/heads/m\ncommitter A <a@example.com> 1700000000 +0000\ndata 2\nc\nM 100644 :1 a\nM 100644 :2 b\n\n");
    compare("dup", &["core.bigFileThreshold=1000", "fastimport.unpackLimit=0"], &[], &dup, None);
}

#[test]
fn cycling_on_top_of_an_existing_history() {
    let seed = stream(6, 2000);
    let mut input = b"commit refs/heads/main\ncommitter A <a@example.com> 1700000100 +0000\ndata 4\nnext\nfrom refs/heads/main^0\nM 100644 inline n\ndata 3\nnew\n\n".to_vec();
    input.extend_from_slice(&stream(12, 2600)[..]);
    compare("seeded", &["pack.packSizeLimit=10000", "fastimport.unpackLimit=0"], &[], &input, Some(&seed));
}
