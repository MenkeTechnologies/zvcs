//! `git diagnose` — the zip members are deflated the way `git archive
//! --format=zip` deflates them.
//!
//! `create_diagnostics_archive()` (diagnose.c:181-262) drives `write_archive()`
//! with `--format=zip` and no `-<level>`, so `compression_level` is
//! `Z_DEFAULT_COMPRESSION` (archive.c:732). `write_zip_entry()` then marks every
//! non-empty regular member `ZIP_METHOD_DEFLATE` (archive-zip.c:346-347), deflates
//! it raw, and stores it after all when the deflated form is not strictly smaller
//! (archive-zip.c:373-381). The method, sizes, CRC and compressed bytes below
//! were read out of the archive stock git 2.56.0 wrote for the same two files.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// 416 bytes of repetitive text: deflates to 77.
fn text_content() -> String {
    (1..=8).map(|n| format!("line {n}: the quick brown fox jumps over the lazy dog\n")).collect()
}

/// Two bytes: the raw deflate of `x\n` is no smaller, so it stays stored.
const TINY_CONTENT: &str = "x\n";

/// Stock 2.56.0's deflated payload for [`text_content`].
const TEXT_DEFLATED_HEX: &str = "cbc9cc4b5530b45228c94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d024be72456552aa4e4a773e580f41891a1c7980c3d2664e83125438f19197accc9d063419a1e00";

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diagnose-deflate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn git(&self, cwd: &Path, args: &[&str]) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed: {out:?}");
    }

    /// `git diagnose --mode=all` over a fresh repository carrying the two
    /// measured files in `.git/info`, which `--mode=all` archives.
    fn archive(&self) -> Vec<u8> {
        self.git(&self.root, &["init", "-q", "repo"]);
        let repo = self.root.join("repo");
        std::fs::write(repo.join(".git/info/parity-text"), text_content()).unwrap();
        std::fs::write(repo.join(".git/info/parity-tiny"), TINY_CONTENT).unwrap();
        self.git(&repo, &["diagnose", "--mode=all", "-s", "t", "-o", "out"]);
        std::fs::read(repo.join("out/git-diagnostics-t.zip")).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// One member as both of its headers describe it.
#[derive(Debug, PartialEq)]
struct Member {
    method: u16,
    crc: u32,
    compressed: u32,
    size: u32,
    payload: Vec<u8>,
    dir_method: u16,
    dir_compressed: u32,
    dir_size: u32,
}

fn le16(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn le32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

/// The local header + payload and the central-directory record of `name`.
fn member(zip: &[u8], name: &str) -> Member {
    let mut p = 0;
    let mut local = None;
    while le32(zip, p) == 0x0403_4b50 {
        let (method, crc, compressed, size) = (le16(zip, p + 8), le32(zip, p + 14), le32(zip, p + 18), le32(zip, p + 22));
        let (nlen, xlen) = (le16(zip, p + 26) as usize, le16(zip, p + 28) as usize);
        let data = p + 30 + nlen + xlen;
        if &zip[p + 30..p + 30 + nlen] == name.as_bytes() {
            local = Some((method, crc, compressed, size, zip[data..data + compressed as usize].to_vec()));
        }
        p = data + compressed as usize;
    }
    let (method, crc, compressed, size, payload) = local.unwrap_or_else(|| panic!("no local entry {name}"));
    while le32(zip, p) == 0x0201_4b50 {
        let (nlen, xlen, clen) = (le16(zip, p + 28) as usize, le16(zip, p + 30) as usize, le16(zip, p + 32) as usize);
        if &zip[p + 46..p + 46 + nlen] == name.as_bytes() {
            return Member {
                method,
                crc,
                compressed,
                size,
                payload,
                dir_method: le16(zip, p + 10),
                dir_compressed: le32(zip, p + 20),
                dir_size: le32(zip, p + 24),
            };
        }
        p += 46 + nlen + xlen + clen;
    }
    panic!("no central-directory entry {name}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn compressible_member_is_deflated_byte_for_byte() {
    let zip = Fixture::new("text").archive();
    let m = member(&zip, ".git/info/parity-text");
    assert_eq!((m.method, m.compressed, m.size, m.crc), (8, 77, 416, 0x03be_a2a2));
    assert_eq!((m.dir_method, m.dir_compressed, m.dir_size), (8, 77, 416));
    assert_eq!(hex(&m.payload), TEXT_DEFLATED_HEX);
}

#[test]
fn member_deflate_cannot_shrink_stays_stored() {
    let zip = Fixture::new("tiny").archive();
    let m = member(&zip, ".git/info/parity-tiny");
    assert_eq!((m.method, m.compressed, m.size, m.crc), (0, 2, 2, 0x46ea_081f));
    assert_eq!((m.dir_method, m.dir_compressed, m.dir_size), (0, 2, 2));
    assert_eq!(m.payload, TINY_CONTENT.as_bytes());
}
