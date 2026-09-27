//! `fsck` over a damaged pack: `verify_pack()`'s own `error:` lines.
//!
//! `verify_pack()` (pack-check.c:183-196) checks the `.idx` trailer
//! (`Packfile index for <pack> hash mismatch`), the `.pack` trailer
//! (`<pack> pack checksum mismatch`), then walks the objects in offset order:
//! the stored CRC (`index CRC mismatch for object …`), `unpack_entry()`
//! (packfile.c:1784-2000) — whose delta phase reports `failed to read delta base
//! object …` for every delta over a base it could not produce and marks that
//! base bad, and `failed to unpack compressed delta …` after `git_inflate()`'s
//! own `inflate:` line — and `cannot unpack <oid> from <pack> at offset <n>`.
//! An object it cannot produce never reaches `fsck_obj_buffer()`, so it never
//! gets `HAS_OBJ`; a reachable one is `missing` only when `has_object_pack()`
//! fails too, which a base marked bad does (builtin/fsck.c:265-275).
//! zvcs printed gix's integrity error and `object corrupt or missing` for each
//! unreadable object, and exited 5 instead of 6.
//!
//! Expectations measured from stock git 2.55.0 run over the very pack this
//! fixture builds.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

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
    /// Five commits growing `f` to 1000 lines, repacked: the last `f` is a
    /// full blob at offset 741 and the four earlier ones are deltas on it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fsck-corrupt-pack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for i in 1..=5 {
            let body: String = (1..=i * 200).map(|n| format!("{n}\n")).collect();
            std::fs::write(f.work.join("f"), body).unwrap();
            f.run(&["add", "f"]);
            f.run(&["commit", "-q", "-m", &format!("c{i}")]);
        }
        f.run(&["repack", "-adq"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// The pack's path as git names it, with the layout the expectations
    /// were measured against checked first.
    fn pack(&self) -> String {
        let dir = self.work.join(".git/objects/pack");
        let name = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .find(|n| n.ends_with(".pack"))
            .unwrap();
        let idx = dir.join(name.replace(".pack", ".idx"));
        let out = Command::new(BIN)
            .arg("show-index")
            .current_dir(&self.work)
            .stdin(std::fs::File::open(idx).unwrap())
            .output()
            .unwrap();
        let listing = String::from_utf8(out.stdout).unwrap();
        for (offset, id) in [
            (741, "1179824569dcb14413904cb2b5cb036a9551024d"),
            (2660, "aa5e3f802c6a6d3eb7eac845d2293dec38ccfff1"),
        ] {
            assert!(listing.contains(&format!("{offset} {id} ")), "{listing}");
        }
        format!(".git/objects/pack/{name}")
    }

    /// Flip every bit of the byte at `at` in `file`.
    fn flip(&self, file: &str, at: i64) {
        self.patch(file, at, |b| b ^ 0xff);
    }

    /// Rewrite the byte at `at` in `file` (negative counts from the end).
    fn patch(&self, file: &str, at: i64, change: impl Fn(u8) -> u8) {
        let path = self.work.join(file);
        let mut bytes = std::fs::read(&path).unwrap();
        let at = if at < 0 { bytes.len() - (-at) as usize } else { at as usize };
        bytes[at] = change(bytes[at]);
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
        std::fs::write(&path, bytes).unwrap();
    }
}

#[test]
fn a_broken_base_fails_every_delta_on_it_and_goes_missing() {
    let f = Fixture::new("base");
    let pack = f.pack();
    // Zero the last byte of the base's three-byte object header: its size changes,
    // so the inflate ends short without a zlib complaint.
    f.patch(&pack, 743, |_| 0);
    let base = "1179824569dcb14413904cb2b5cb036a9551024d";
    let mut want = format!(
        "error: {pack} pack checksum mismatch\n\
         error: index CRC mismatch for object {base} from {pack} at offset 741\n\
         error: cannot unpack {base} from {pack} at offset 741\n"
    );
    for (id, offset) in [
        ("aa5e3f802c6a6d3eb7eac845d2293dec38ccfff1", 2660),
        ("7b5d34d5cf4229e05f566b7e2b9f8ea113e2efba", 2718),
        ("5bd1145c37fbb91d887edc24f1ea59f79c0a9e8a", 2776),
        ("f35f7045be9d195b48c312cf831a141f3861f1bc", 2834),
    ] {
        want.push_str(&format!(
            "error: failed to read delta base object {base} at offset 741 from {pack}\n\
             error: cannot unpack {id} from {pack} at offset {offset}\n"
        ));
    }
    let (out, err, code) = f.run(&["fsck"]);
    assert_eq!(err, want);
    assert_eq!((out.as_str(), code), (format!("missing blob {base}\n").as_str(), 6));
}

#[test]
fn a_broken_delta_reports_the_inflate_failure() {
    let f = Fixture::new("delta");
    let pack = f.pack();
    // The first byte of the delta's zlib stream, after its one-byte header and
    // two-byte base offset.
    f.flip(&pack, 2663);
    let id = "aa5e3f802c6a6d3eb7eac845d2293dec38ccfff1";
    let (out, err, code) = f.run(&["fsck"]);
    assert_eq!(
        err,
        format!(
            "error: {pack} pack checksum mismatch\n\
             error: index CRC mismatch for object {id} from {pack} at offset 2660\n\
             error: inflate: data stream error (incorrect header check)\n\
             error: failed to unpack compressed delta at offset 2663 from {pack}\n\
             error: cannot unpack {id} from {pack} at offset 2660\n"
        )
    );
    assert_eq!((out.as_str(), code), ("", 4));
}

#[test]
fn a_damaged_index_trailer_is_a_hash_mismatch() {
    let f = Fixture::new("idx");
    let pack = f.pack();
    f.flip(&pack.replace(".pack", ".idx"), -1);
    let (out, err, code) = f.run(&["fsck"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", format!("error: Packfile index for {pack} hash mismatch\n").as_str(), 4)
    );
}
