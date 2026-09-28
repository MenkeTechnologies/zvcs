//! `pack-objects --revs` with exclusions among the revisions on stdin.
//!
//! `get_object_list()` hands each stdin line to `handle_revision_arg()` with
//! the `--not` state in `flags` (builtin/pack-objects.c), so `^<rev>` and
//! `--not` hide what they name — combining by XOR (revision.c:2229) —
//! `<a>..<b>` hides `<a>` and `<a>...<b>` hides their merge bases
//! (`handle_dotdot_1()`, revision.c:2083-2107). zvcs validated the excluded
//! arguments and then dropped them, packing everything reachable.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    /// Three commits growing `f`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-pack-objects-exclusions-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&f.work, &["init", "-q", "-b", "main", "."], None);
        for i in 1..=3 {
            let body: String = (1..=i * 200).map(|n| format!("{n}\n")).collect();
            std::fs::write(f.work.join("f"), body).unwrap();
            f.git(&f.work, &["add", "f"], None);
            f.git(&f.work, &["commit", "-q", "-m", &format!("c{i}")], None);
        }
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
        (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().unwrap())
    }

    /// `pack-objects --revs --stdout` over `input`, and the ids the pack holds.
    fn pack(&self, input: &str) -> (Vec<u8>, Vec<String>) {
        let (pack, err, code) =
            self.git(&self.work, &["pack-objects", "--revs", "--stdout"], Some(input.as_bytes()));
        assert_eq!((err.as_str(), code), ("", 0), "{input:?}");
        let path = self.root.join("p.pack");
        std::fs::write(&path, &pack).unwrap();
        let _ = std::fs::remove_file(self.root.join("p.idx"));
        self.git(&self.work, &["index-pack", path.to_str().unwrap()], None);
        let idx = std::fs::read(self.root.join("p.idx")).unwrap();
        let listing = String::from_utf8(self.git(&self.work, &["show-index"], Some(&idx)).0).unwrap();
        let mut ids: Vec<String> =
            listing.lines().map(|l| l.split(' ').nth(1).unwrap().to_string()).collect();
        ids.sort();
        (pack, ids)
    }
}

const TIP: [&str; 3] = [
    "286e3722e554581c9c2b1f14ebf373ff52eb2297",
    "52c11f89c4dbbbffa079285e5f0241c2f63ad679",
    "5bd1145c37fbb91d887edc24f1ea59f79c0a9e8a",
];

const TWO: [&str; 6] = [
    "286e3722e554581c9c2b1f14ebf373ff52eb2297",
    "3aa5802851e9b74432676b9c6c18cda8bbdd5437",
    "52c11f89c4dbbbffa079285e5f0241c2f63ad679",
    "5bd1145c37fbb91d887edc24f1ea59f79c0a9e8a",
    "7b5d34d5cf4229e05f566b7e2b9f8ea113e2efba",
    "bc891b62efb02d8ef79c472a093e895ec915bcd3",
];

#[test]
fn a_caret_or_a_range_hides_the_parent() {
    let f = Fixture::new("caret");
    let (caret, ids) = f.pack("main\n^main~1\n");
    assert_eq!(ids, TIP);
    let (range, ids) = f.pack("main~1..main\n");
    assert_eq!(ids, TIP);
    assert_eq!(caret, range);
}

#[test]
fn not_hides_what_follows_and_flips_a_caret() {
    let f = Fixture::new("not");
    assert_eq!(f.pack("main\n--not\nmain~2\n").1, TWO);
    assert_eq!(f.pack("--not\nmain~2\n--not\nmain\n").1, TWO);
    assert_eq!(f.pack("main~2...main\n").1, TWO);
    // `--not` then `^main~1`: the two cancel, so everything is packed.
    assert_eq!(f.pack("main\n--not\n^main~1\n").1.len(), 9);
}
