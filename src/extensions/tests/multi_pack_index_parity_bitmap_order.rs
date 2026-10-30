//! `multi-pack-index write --bitmap` over a history where many commits share a
//! committer date and the bitmaps run past one EWAH word, with and without
//! `--refs-snapshot=<path>`.
//!
//! Three things decide the bytes of the `.bitmap` there, and each is compared
//! against stock here:
//!
//! * the order `find_commits_for_midx_bitmap()` (midx-write.c:836-878) hands
//!   the commits over: the pending set in the order it was added (the
//!   snapshot's line order, or ref order) through `prepare_revision_walk()`'s
//!   date queue, whose ties keep that order;
//! * the two libc `QSORT`s over dates in `bitmap_writer_select_commits()` and
//!   `compute_xor_offsets()` (pack-bitmap-write.c:1051, 353), which decide
//!   where commits sharing a date are written;
//! * `ewah_xor()` run on the compressed streams, whose trailing empty words
//!   decide whether `compute_xor_offsets()` takes an XOR at all.
//!
//! The history is written by one stock `fast-import` per pack, so both packs
//! and their pinned mtimes are the same for both binaries; every `multi-pack-index*`
//! file each binary leaves is compared byte for byte, along with exit code and
//! both streams.
#![cfg(unix)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    {
        use std::io::Write;
        let mut input = child.stdin.take().unwrap();
        input.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.code().unwrap_or(-1), text(&out.stdout), text(&out.stderr))
}

/// A `fast-import` stream for commits `from..=to` on `main`, every tenth one a
/// merge of a side branch forked three commits back and every twenty-fifth
/// tagged. Commit `i` is dated by `date(i)`.
fn stream(from: u32, to: u32, date: impl Fn(u32) -> u64) -> String {
    let mut s = String::new();
    let who = "C O Mitter <committer@example.com>";
    for i in from..=to {
        if i % 10 == 0 {
            let _ = write!(
                s,
                "commit refs/heads/side{i}\nmark :{}\ncommitter {who} {} +0000\ndata 4\ns{i:03}\nfrom :{}\nM 100644 inline s{i}\ndata 4\ns{i:03}\n\n",
                10_000 + i,
                date(i),
                i - 3,
            );
        }
        let _ = write!(s, "commit refs/heads/main\nmark :{i}\ncommitter {who} {} +0000\ndata 4\nc{i:03}\n", date(i));
        if i > 1 {
            let _ = write!(s, "from :{}\n", i - 1);
        }
        if i % 10 == 0 {
            let _ = write!(s, "merge :{}\n", 10_000 + i);
        }
        let _ = write!(s, "M 100644 inline f{}\ndata 4\n{i:03}\n\n", i % 7);
        if i % 25 == 0 {
            let _ = write!(s, "tag t{i}\nfrom :{i}\ntagger {who} {} +0000\ndata 2\nt\n\n", date(i));
        }
    }
    s
}

struct World {
    root: PathBuf,
    stock: &'static str,
    /// `main`'s first-parent history, newest first.
    mainline: Vec<String>,
    /// The two `.idx` names, older pack first.
    packs: [String; 2],
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl World {
    fn new(stock: &'static str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-midx-bitmap-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let base = root.join("base");
        std::fs::create_dir_all(&base).unwrap();
        let run = |args: &[&str], stdin: Option<&str>| {
            let out = git(stock, &base, args, stdin);
            assert_eq!(out.0, 0, "{args:?}: {out:?}");
            out.1.trim().to_owned()
        };
        run(&["init", "-q", "-b", "main"], None);
        // The first pack: 60 commits sharing one date. The second: 70 more, in
        // threes per date, so ties and distinct dates both reach the sort.
        run(&["fast-import", "--quiet", "--export-marks=../marks"], Some(&stream(1, 60, |_| 1_700_000_000)));
        run(
            &["fast-import", "--quiet", "--import-marks=../marks"],
            Some(&stream(61, 130, |i| 1_700_000_000 + u64::from(i / 3) * 100)),
        );
        let mainline = run(&["rev-list", "--first-parent", "main"], None)
            .lines()
            .map(str::to_owned)
            .collect();

        let pack_dir = base.join(".git/objects/pack");
        let mut idx: Vec<String> = std::fs::read_dir(&pack_dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".idx"))
            .collect();
        idx.sort();
        assert_eq!(idx.len(), 2, "{idx:?}");
        for (name, stamp) in idx.iter().zip(["202001010000", "202101010000"]) {
            let pack = pack_dir.join(name.replace(".idx", ".pack"));
            let ok = Command::new("touch").arg("-t").arg(stamp).arg(pack).status().unwrap();
            assert!(ok.success());
        }
        let packs = [idx[0].clone(), idx[1].clone()];
        World { root, stock, mainline, packs }
    }

    fn rev_parse(&self, spec: &str) -> String {
        git(self.stock, &self.root.join("base"), &["rev-parse", spec], None).1.trim().to_owned()
    }

    /// Run `multi-pack-index write <args>` on a fresh copy for each binary, with
    /// `snapshot` written to `snap` in it first. Asserts the two agree on exit
    /// code, both streams and every `multi-pack-index*` file, and that a MIDX
    /// and its bitmap were written.
    fn case(&self, label: &str, snapshot: &str, args: &[&str], stdin: Option<&str>) {
        let mut seen = Vec::new();
        for (side, bin) in [("stock", self.stock), ("zvcs", ZVCS)] {
            let copy = self.root.join(format!("{label}-{side}"));
            let ok = Command::new("cp").arg("-Rp").arg(self.root.join("base")).arg(&copy).status().unwrap();
            assert!(ok.success());
            std::fs::write(copy.join("snap"), snapshot).unwrap();
            let mut full = vec!["multi-pack-index", "write"];
            full.extend_from_slice(args);
            let out = git(bin, &copy, &full, stdin);
            let pack_dir = copy.join(".git/objects/pack");
            let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(&pack_dir)
                .unwrap()
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| n.starts_with("multi-pack-index"))
                .map(|n| {
                    let bytes = std::fs::read(pack_dir.join(&n)).unwrap();
                    (n, bytes)
                })
                .collect();
            files.sort();
            seen.push((out, files));
        }
        let z = seen.pop().unwrap();
        let s = seen.pop().unwrap();
        assert_eq!(z.0, s.0, "{label}: {args:?}");
        assert_eq!((s.0 .0, s.0 .2.as_str()), (0, ""), "{label}");
        let names = |f: &[(String, Vec<u8>)]| f.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        assert_eq!(names(&z.1), names(&s.1), "{label}: {args:?}");
        assert_eq!(s.1.len(), 2, "{label}: {:?}", names(&s.1));
        for ((name, want), (_, got)) in s.1.iter().zip(&z.1) {
            assert!(want == got, "{label}: {name} differs");
        }
    }
}

#[test]
fn tied_dates_and_long_bitmaps_write_stocks_bitmap() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let w = World::new(stock, "ties");
    let m = &w.mainline;
    let bitmap = &["--bitmap", "--refs-snapshot=snap"][..];

    // No snapshot: every ref, in ref order.
    w.case("refs", "", &["--bitmap"], None);

    // Tips listed newest-branch first and out of name order, every third one
    // marked `+`: the pending order is the file's, not a sorted one.
    let mut tips = String::new();
    for (n, i) in (1..=13).rev().map(|k| k * 10).enumerate() {
        let plus = if n % 3 == 0 { "+" } else { "" };
        let _ = writeln!(tips, "{plus}{}", w.rev_parse(&format!("side{i}")));
    }
    let _ = writeln!(tips, "{}", m[0]);
    w.case("tips", &tips, bitmap, None);

    // Every fourth mainline commit, oldest first — a list of commits that all
    // share a date in the first pack.
    let spread: String = m.iter().rev().step_by(4).map(|id| format!("{id}\n")).collect();
    w.case("spread", &spread, bitmap, None);

    // Tags behind `+`, then one plain commit.
    let tags: String = [25, 50, 75, 100, 125].iter().map(|i| format!("+{}\n", w.rev_parse(&format!("t{i}")))).collect();
    let tags = format!("{tags}{}\n", m[5]);
    w.case("tags", &tags, bitmap, None);

    // The same walk under a preferred pack and an explicit pack list.
    let pref = format!("--preferred-pack={}", w.packs[1]);
    w.case("preferred", &tags, &["--bitmap", "--refs-snapshot=snap", &pref], None);
    let both = format!("{}\n{}\n", w.packs[1], w.packs[0]);
    w.case("stdin", &tips, &["--bitmap", "--stdin-packs", "--refs-snapshot=snap"], Some(&both));
}
