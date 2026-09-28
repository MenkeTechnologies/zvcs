//! `unpack-objects` reading the pack as a stream, the way git does.
//!
//! `unpack_all()` decodes one object at a time straight off stdin
//! (builtin/unpack-objects.c:595-626). `get_data()` reports a zlib failure as
//! `git_inflate()`'s `inflate: …` line and `inflate returned <n>`, and exits 1
//! unless `-r`, which carries on from wherever the stream stopped — so a bad
//! entry can make the following bytes read as `bad object type <n>` (:584-591)
//! before the trailer check dies `final sha1 did not match`. `--strict` writes
//! blobs as they come and holds everything else until `write_rest()`, whose
//! `check_object()` writes each held object after `fsck_object()` and after
//! walking its links: a link the pack and the odb lack is `object of
//! unexpected type` (:216-262), and a finding is `fsck error in packed object`.
//! Bytes after the trailer are copied to stdout (:689-690). zvcs indexed the
//! whole stream through gix first and reported none of these.
//!
//! Expectations measured from stock git 2.55.0 run over the very packs this
//! fixture builds.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    src: PathBuf,
    dst: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `src`: three commits growing `f`, packed whole into `../p.pack`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-unpack-objects-stream-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        let f = Fixture { root, src, dst };
        f.git(&f.src, &["init", "-q", "-b", "main", "."], None);
        for i in 1..=3 {
            let body: String = (1..=i * 200).map(|n| format!("{n}\n")).collect();
            std::fs::write(f.src.join("f"), body).unwrap();
            f.git(&f.src, &["add", "f"], None);
            f.git(&f.src, &["commit", "-q", "-m", &format!("c{i}")], None);
        }
        let pack = f.git(&f.src, &["pack-objects", "--all", "--stdout"], Some(b"")).0;
        std::fs::write(f.root.join("p.pack"), pack).unwrap();
        f
    }

    fn git(&self, dir: &PathBuf, args: &[&str], stdin: Option<&[u8]>) -> (Vec<u8>, String, i32) {
        use std::io::Write;
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(data) = stdin {
            child.stdin.take().unwrap().write_all(data).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        (
            out.stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// A fresh empty `dst`, then `unpack-objects <args>` there with `pack`
    /// on stdin; the result plus the objects it left, sorted.
    fn unpack(&self, args: &[&str], pack: &[u8]) -> (String, String, i32, Vec<String>) {
        let _ = std::fs::remove_dir_all(&self.dst);
        std::fs::create_dir_all(&self.dst).unwrap();
        self.git(&self.dst, &["init", "-q", "."], None);
        let mut argv = vec!["unpack-objects"];
        argv.extend_from_slice(args);
        let (out, err, code) = self.git(&self.dst, &argv, Some(pack));
        let listing = self
            .git(&self.dst, &["cat-file", "--batch-all-objects", "--batch-check"], None)
            .0;
        let mut objects: Vec<String> =
            String::from_utf8(listing).unwrap().lines().map(str::to_string).collect();
        objects.sort();
        (String::from_utf8_lossy(&out).into_owned(), err, code, objects)
    }

    fn pack(&self) -> Vec<u8> {
        std::fs::read(self.root.join("p.pack")).unwrap()
    }
}

const BAD_HEADER: &str = "error: inflate: data stream error (incorrect header check)\n\
                          error: inflate returned -3\n";

/// The pack with the first byte of its first object's zlib stream inverted.
fn first_zlib_byte_flipped(mut pack: Vec<u8>) -> Vec<u8> {
    let mut at = 12;
    while pack[at] & 0x80 != 0 {
        at += 1;
    }
    at += 1;
    pack[at] ^= 0xff;
    pack
}

#[test]
fn an_inflate_failure_exits_1_and_r_reads_on() {
    let f = Fixture::new("inflate");
    let pack = first_zlib_byte_flipped(f.pack());

    let (out, err, code, objects) = f.unpack(&[], &pack);
    assert_eq!((out.as_str(), err.as_str(), code), ("", BAD_HEADER, 1));
    assert!(objects.is_empty(), "{objects:?}");

    let (out, err, code, _) = f.unpack(&["-r"], &pack);
    let want = format!(
        "{BAD_HEADER}error: bad object type 0\n{BAD_HEADER}{BAD_HEADER}{BAD_HEADER}{BAD_HEADER}\
         error: bad object type 5\n{BAD_HEADER}error: bad object type 0\n\
         fatal: final sha1 did not match\n"
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128));
}

#[test]
fn bytes_after_the_trailer_go_to_stdout() {
    let f = Fixture::new("trailing");
    let mut pack = f.pack();
    pack.extend_from_slice(b"TRAILING");
    let (out, err, code, objects) = f.unpack(&[], &pack);
    assert_eq!((out.as_str(), err.as_str(), code), ("TRAILING", "", 0));
    assert_eq!(objects.len(), 9);
}

#[test]
fn strict_writes_the_tree_before_the_missing_parent_stops_it() {
    let f = Fixture::new("thin");
    let ids = f.git(&f.src, &["rev-parse", "HEAD", "HEAD^{tree}", "HEAD:f"], None).0;
    let thin = f.git(&f.src, &["pack-objects", "--stdout"], Some(&ids)).0;
    let (out, err, code, objects) = f.unpack(&["--strict"], &thin);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "fatal: object of unexpected type\n", 128));
    assert_eq!(
        objects,
        [
            "286e3722e554581c9c2b1f14ebf373ff52eb2297 tree 29",
            "5bd1145c37fbb91d887edc24f1ea59f79c0a9e8a blob 2292",
        ]
    );
}

#[test]
fn strict_reports_fsck_findings_and_writes_nothing_held() {
    let f = Fixture::new("fsck");
    let blob = String::from_utf8(f.git(&f.src, &["rev-parse", "HEAD:f"], None).0).unwrap();
    let raw = gix_hex(blob.trim_end());
    let mut tree = Vec::new();
    for name in ["b", "a"] {
        tree.extend_from_slice(format!("100644 {name}\0").as_bytes());
        tree.extend_from_slice(&raw);
    }
    let path = f.root.join("unsorted-tree");
    std::fs::write(&path, tree).unwrap();
    let id = f
        .git(&f.src, &["hash-object", "-t", "tree", "-w", "--literally", path.to_str().unwrap()], None)
        .0;
    let pack = f.git(&f.src, &["pack-objects", "--stdout"], Some(&id)).0;
    let id = String::from_utf8(id).unwrap();
    let id = id.trim_end();
    let (out, err, code, objects) = f.unpack(&["--strict"], &pack);
    let want = format!(
        "error: object {id}: treeNotSorted: not properly sorted\nfatal: fsck error in packed object\n"
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128));
    assert!(objects.is_empty(), "{objects:?}");
}

#[test]
fn a_dry_run_writes_nothing() {
    let f = Fixture::new("dry");
    let (out, err, code, objects) = f.unpack(&["-n"], &f.pack());
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert!(objects.is_empty(), "{objects:?}");
}

/// Hex to raw bytes.
fn gix_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
