//! A `git unpack-objects` input cut off inside an entry is `fatal: early EOF`,
//! and `-r` does not excuse it.
//!
//! ```c
//! for (;;) {
//!         int ret = git_inflate(&stream, 0);
//!         use(len - stream.avail_in);
//!         if (stream.total_out == size && ret == Z_STREAM_END)
//!                 break;
//!         if (ret != Z_OK) {
//!                 error("inflate returned %d", ret);
//!                 FREE_AND_NULL(buf);
//!                 if (!recover)
//!                         exit(1);
//!                 has_errors = 1;
//!                 break;
//!         }
//!         stream.next_in = fill(1);
//!         stream.avail_in = len;
//!         …
//! }
//! ```
//!
//! (`get_data()`, builtin/unpack-objects.c:131-153.) Only the `inflate returned
//! %d` branch is what `-r` recovers from. Running out of *input* goes through
//! `fill(1)` on the line below it, and `fill()` dies `early EOF`
//! (`:78-86`) whatever `recover` asked for — as does the trailer comparison
//! `cmd_unpack_objects()` makes once the loop is over (`:684-686`).
//!
//! `gix-pack` reports both a truncated entry and a zlib stream that merely ended
//! short of its declared size as one `input::Error::IncompletePack`
//! (gix-pack/src/data/input/bytes_to_entries.rs:121-126), which reached stderr
//! as `fatal: A pack entry could not be extracted`, and under `-r` its
//! `Mode::Restore` salvaged the prefix and exited 1 in silence. Only the
//! truncated case reads past the end of the input, so that is what separates
//! them.
//!
//! The pack here is built by the binary under test and then cut at byte offsets
//! well past its 12-byte header, so nothing depends on how any particular zlib
//! build encodes the entries — only on the input ending before the pack does.
//!
//! Every expectation here was captured from stock git 2.55.0.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const EARLY_EOF: &str = "fatal: early EOF\n";

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        // Several of these tests build their own fixtures and cargo runs them in
        // parallel, so the pid alone is not a unique name.
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "zvcs-upmid-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work };
        fx.ok(&["init", "-q", "-b", "main", "."]);
        fx
    }

    fn feed(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = Command::new(BIN)
            .args(["-c", "user.email=t@e.co", "-c", "user.name=t"])
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.root.join("gitconfig-system"))
            .env("GIT_AUTHOR_DATE", "@1000000000 +0000")
            .env("GIT_COMMITTER_DATE", "@1000000000 +0000")
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run binary");
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.feed(args, b"");
        assert!(out.status.success(), "setup git {args:?}: {out:?}");
        out
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A pack of several commits, big enough that a cut well past the header still
/// lands inside an entry rather than in the trailer.
fn sample_pack() -> (Fixture, Vec<u8>) {
    let fx = Fixture::new("src");
    for i in 0..6 {
        let mut all = String::new();
        for j in 0..=i {
            all.push_str(&format!("line {j}\nmore content so the entries are not tiny\n"));
        }
        std::fs::write(fx.work.join("f.txt"), all).unwrap();
        fx.ok(&["add", "f.txt"]);
        fx.ok(&["commit", "-q", "-m", &format!("c{i}")]);
    }
    let head = String::from_utf8_lossy(&fx.ok(&["rev-parse", "HEAD"]).stdout)
        .trim_end()
        .to_string();
    let out = fx.feed(&["pack-objects", "--revs", "--stdout"], format!("{head}\n").as_bytes());
    assert!(out.status.success(), "pack-objects: {out:?}");
    assert!(out.stdout.len() > 400, "pack too small to cut inside an entry: {}", out.stdout.len());
    (fx, out.stdout)
}

#[test]
fn a_cut_inside_an_entry_is_early_eof_with_and_without_recover() {
    let (_src, pack) = sample_pack();
    let fx = Fixture::new("cut");

    // Four cuts spread across the body: past the header, so the header checks
    // cannot be what answers, and short of the trailer, so the loop is what runs
    // out. `-n` keeps each case a pure decode.
    for fraction in [4, 3, 2, 1] {
        let at = 12 + (pack.len() - 12) * fraction / 5;
        for args in [
            vec!["unpack-objects", "-n"],
            vec!["unpack-objects", "-n", "-r"],
            vec!["unpack-objects", "-n", "-q"],
            vec!["unpack-objects", "-n", "--strict"],
            vec!["unpack-objects", "-n", "-r", "--strict"],
        ] {
            let out = fx.feed(&args, &pack[..at]);
            assert_eq!(out.status.code(), Some(128), "{args:?} at {at}: {out:?}");
            assert_eq!(stderr(&out), EARLY_EOF, "{args:?} at {at}");
        }
    }
}

#[test]
fn a_cut_inside_the_trailer_is_early_eof_too() {
    let (_src, pack) = sample_pack();
    let fx = Fixture::new("trailer");

    // Every object decodes; only `fill(the_hash_algo->rawsz)` for the trailing
    // hash comes up short. `-r` still dies, because `recover` never covered that
    // read.
    for missing in [1usize, 5, 19] {
        let at = pack.len() - missing;
        for args in [vec!["unpack-objects", "-n"], vec!["unpack-objects", "-n", "-r"]] {
            let out = fx.feed(&args, &pack[..at]);
            assert_eq!(out.status.code(), Some(128), "{args:?} missing {missing}: {out:?}");
            assert_eq!(stderr(&out), EARLY_EOF, "{args:?} missing {missing}");
        }
    }
}

#[test]
fn a_wrong_trailer_still_dies_under_recover() {
    let (_src, pack) = sample_pack();
    let fx = Fixture::new("badhash");

    // `if (!hasheq(fill(the_hash_algo->rawsz), oid.hash, …)) die("final sha1 did
    // not match")` is unconditional, so flipping the last byte is fatal with or
    // without `-r`. `Mode::Restore` does not even look at the trailer it was
    // given, which is why this needs its own classification pass.
    let mut damaged = pack.clone();
    let last = damaged.len() - 1;
    damaged[last] ^= 0xff;

    for args in [vec!["unpack-objects", "-n"], vec!["unpack-objects", "-n", "-r"]] {
        let out = fx.feed(&args, &damaged);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {out:?}");
        assert_eq!(stderr(&out), "fatal: final sha1 did not match\n", "{args:?}");
    }
}

#[test]
fn the_whole_pack_still_unpacks_under_every_flag() {
    let (_src, pack) = sample_pack();

    // The classification must only fire on real damage: an intact pack stays
    // silent and exits 0, and `-r` does not turn it into a loss.
    for args in [
        vec!["unpack-objects"],
        vec!["unpack-objects", "-r"],
        vec!["unpack-objects", "-q"],
        vec!["unpack-objects", "--strict"],
        vec!["unpack-objects", "-r", "--strict"],
    ] {
        let fx = Fixture::new("good");
        let out = fx.feed(&args, &pack);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        assert_eq!(stderr(&out), "", "{args:?}");

        // And the objects really landed, so nothing above was skipped.
        let count = fx.ok(&["cat-file", "--batch-all-objects", "--batch-check=%(objectname)"]);
        assert!(
            String::from_utf8_lossy(&count.stdout).lines().count() >= 6,
            "{args:?} wrote nothing: {count:?}"
        );
    }
}
