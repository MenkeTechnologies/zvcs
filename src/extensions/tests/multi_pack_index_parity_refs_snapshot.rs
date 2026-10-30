//! `multi-pack-index write --refs-snapshot=<path>` (builtin/multi-pack-index.c:168-169,
//! midx-write.c:803-878): with `--bitmap` the file replaces the ref walk that picks the
//! bitmapped commits. Each line is `[+]<oid>`, the `+` setting `NEEDS_BITMAP`; a line that
//! is not one object name dies `could not parse line:`, one with anything after the name
//! dies `malformed line:`, an object that is not there dies `unable to parse object:`, and
//! a file `xfopen()` cannot open dies `could not open '<path>' for reading:` with the
//! prefixed name — each leaving no MIDX behind. Without `--bitmap`, or when an up-to-date
//! MIDX already has its bitmap, the file is never opened. The value is an `OPT_FILENAME`,
//! so `:(optional)` naming no file and an empty value both mean no snapshot.
//!
//! Both binaries run on byte-identical copies of one stock-built repository, and the
//! `multi-pack-index*` files they leave are compared byte for byte. Histories with tied
//! dates and bitmaps longer than one EWAH word are covered by
//! `multi_pack_index_parity_bitmap_order.rs`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, env: &[(&str, &str)], args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
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

struct World {
    root: PathBuf,
    stock: &'static str,
    /// `HEAD~0` .. `HEAD~11` of `main`.
    commits: Vec<String>,
    tag: String,
    tree: String,
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
        let root = std::env::temp_dir().join(format!("zvcs-midx-refs-snapshot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let base = root.join("base");
        std::fs::create_dir_all(base.join("sub")).unwrap();
        let run = |args: &[&str], date: &str| {
            let env = [("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)];
            let out = git(stock, &base, &env, args, None);
            assert_eq!(out.0, 0, "{args:?}: {out:?}");
            out.1.trim().to_owned()
        };
        run(&["init", "-q", "-b", "main"], "1700000000 +0000");
        for i in 1..=12 {
            if i == 9 {
                run(&["repack", "-qad"], "1700000000 +0000");
            }
            std::fs::write(base.join(format!("f{}", i % 5)), format!("{i}\n")).unwrap();
            let date = format!("{} +0000", 1_700_000_000 + i * 100);
            run(&["add", "."], &date);
            run(&["commit", "-qm", &format!("c{i}")], &date);
            if i == 6 {
                run(&["tag", "-a", "-m", "t", "v1"], &date);
            }
        }
        run(&["repack", "-qd"], "1700000000 +0000");
        let commits = (0..12).map(|n| run(&["rev-parse", &format!("HEAD~{n}")], "")).collect();
        let tag = run(&["rev-parse", "v1"], "");
        let tree = run(&["rev-parse", "HEAD^{tree}"], "");

        // Distinct pack mtimes, so the oldest-pack choice a bitmap write makes for
        // itself does not fall to the directory order.
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
        World { root, stock, commits, tag, tree, packs }
    }

    /// Run `multi-pack-index write <args>` on a fresh copy for each binary, after
    /// `prep` has written into that copy, from `cwd` inside it. Asserts the two
    /// agree on exit code, both streams and every `multi-pack-index*` file, and
    /// returns stock's answer with the names it left.
    fn case(&self, label: &str, cwd: &str, prep: &dyn Fn(&Path), args: &[&str], stdin: Option<&str>) -> (i32, String, Vec<String>) {
        let mut seen = Vec::new();
        for (side, bin) in [("stock", self.stock), ("zvcs", ZVCS)] {
            let copy = self.root.join(format!("{label}-{side}"));
            let ok = Command::new("cp").arg("-Rp").arg(self.root.join("base")).arg(&copy).status().unwrap();
            assert!(ok.success());
            prep(&copy);
            let mut full = vec!["multi-pack-index", "write"];
            full.extend_from_slice(args);
            let out = git(bin, &copy.join(cwd), &[], &full, stdin);
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
        let names = |f: &[(String, Vec<u8>)]| f.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        assert_eq!(names(&z.1), names(&s.1), "{label}: {args:?}");
        for ((name, want), (_, got)) in s.1.iter().zip(&z.1) {
            assert!(want == got, "{label}: {name} differs");
        }
        (s.0 .0, s.0 .2, names(&s.1))
    }
}

fn snap(body: String) -> impl Fn(&Path) {
    move |repo: &Path| std::fs::write(repo.join("snap"), &body).unwrap()
}

fn nothing(_: &Path) {}

#[test]
fn a_snapshot_chooses_the_bitmapped_commits() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let w = World::new(stock, "choose");
    let c = &w.commits;
    let bitmap = &["--bitmap", "--refs-snapshot=snap"][..];
    let cases: Vec<(&str, String)> = vec![
        ("one", format!("{}\n", c[5])),
        ("plus", format!("+{}\n", c[5])),
        ("two", format!("{}\n+{}\n", c[7], c[0])),
        ("tag", format!("+{}\n", w.tag)),
        ("tree", format!("{}\n", w.tree)),
        ("empty", String::new()),
        ("crlf", format!("{}\r\n", c[3])),
        ("no-newline", c[3].clone()),
        ("upper", format!("{}\n", c[3].to_ascii_uppercase())),
    ];
    for (label, body) in cases {
        let (code, stderr, names) = w.case(label, "", &snap(body), bitmap, None);
        assert_eq!((code, stderr.as_str()), (0, ""), "{label}");
        assert_eq!(names.len(), 2, "{label}: {names:?}");
    }

    // The snapshot composes with the pack selection a write makes.
    let one = snap(format!("{}\n", c[5]));
    let pref = format!("--preferred-pack={}", w.packs[1]);
    w.case("preferred", "", &one, &["--bitmap", "--refs-snapshot=snap", &pref], None);
    let older = format!("{}\n", w.packs[0]);
    let (code, _, _) = w.case(
        "stdin-older",
        "",
        &snap(format!("{}\n", c[11])),
        &["--bitmap", "--stdin-packs", "--refs-snapshot=snap"],
        Some(&older),
    );
    assert_eq!(code, 0);
    let both = format!("{}\n{}\n", w.packs[0], w.packs[1]);
    w.case("stdin-both", "", &one, &["--bitmap", "--stdin-packs", "--refs-snapshot", "snap"], Some(&both));
}

#[test]
fn a_bad_snapshot_dies_and_leaves_no_midx() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let w = World::new(stock, "bad");
    let c = &w.commits;
    let bitmap = &["--bitmap", "--refs-snapshot=snap"][..];
    let cases: Vec<(&str, String, String)> = vec![
        ("garbage", "xyz\n".into(), "fatal: could not parse line: xyz\n".into()),
        ("second-line", format!("{}\nzz\n", c[0]), "fatal: could not parse line: zz\n".into()),
        ("blank", format!("{}\n\n", c[0]), "fatal: could not parse line: \n".into()),
        ("short", format!("{}\n", &c[0][..39]), format!("fatal: could not parse line: {}\n", &c[0][..39])),
        ("plus-only", "+\n".into(), "fatal: could not parse line: +\n".into()),
        ("plus-plus", format!("++{}\n", c[0]), format!("fatal: could not parse line: ++{}\n", c[0])),
        ("trailing", format!("{} x\n", c[0]), format!("fatal: malformed line: {} x\n", c[0])),
        (
            "unknown",
            "1234567890123456789012345678901234567890\n".into(),
            "fatal: unable to parse object: 1234567890123456789012345678901234567890\n".into(),
        ),
    ];
    for (label, body, want) in cases {
        let (code, stderr, names) = w.case(label, "", &snap(body), bitmap, None);
        assert_eq!((code, stderr.as_str()), (128, want.as_str()), "{label}");
        assert!(names.is_empty(), "{label}: {names:?}");
    }

    let (code, stderr, _) = w.case("missing", "", &nothing, &["--bitmap", "--refs-snapshot=nope"], None);
    assert_eq!((code, stderr.as_str()), (128, "fatal: could not open 'nope' for reading: No such file or directory\n"));
    let mkdir = |repo: &Path| std::fs::create_dir(repo.join("snapd")).unwrap();
    let (code, stderr, _) = w.case("directory", "", &mkdir, &["--bitmap", "--refs-snapshot=snapd"], None);
    assert_eq!((code, stderr.as_str()), (128, "fatal: could not open 'snapd' for reading: Is a directory\n"));

    // From a subdirectory the name is the prefixed one, and `-` is the file of
    // that name at the top rather than standard input.
    let (code, stderr, _) = w.case("subdir", "sub", &nothing, &["--bitmap", "--refs-snapshot=nope"], None);
    assert_eq!((code, stderr.as_str()), (128, "fatal: could not open 'sub/nope' for reading: No such file or directory\n"));
    let dash = |repo: &Path| {
        std::fs::write(repo.join("-"), "zz\n").unwrap();
        std::fs::write(repo.join("sub/-"), "00\n").unwrap();
    };
    let (code, stderr, _) = w.case("dash", "sub", &dash, &["--bitmap", "--refs-snapshot=-"], None);
    assert_eq!((code, stderr.as_str()), (128, "fatal: could not parse line: zz\n"));

    // A MIDX already in place stays when the bitmap write dies.
    let midx_first = |repo: &Path| {
        let out = git(w.stock, repo, &[], &["multi-pack-index", "write"], None);
        assert_eq!(out.0, 0, "{out:?}");
        std::fs::write(repo.join("snap"), "zz\n").unwrap();
    };
    let (code, _, names) = w.case("kept-midx", "", &midx_first, bitmap, None);
    assert_eq!((code, names), (128, vec!["multi-pack-index".to_string()]));
}

#[test]
fn a_snapshot_that_is_never_read_changes_nothing() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let w = World::new(stock, "unread");
    // No `--bitmap`: the file is not opened at all.
    let (code, _, names) = w.case("no-bitmap", "", &nothing, &["--refs-snapshot=nope"], None);
    assert_eq!((code, names), (0, vec!["multi-pack-index".to_string()]));
    // `:(optional)` naming no file, an empty value and `--no-` after a real one.
    let one = snap(format!("{}\n", w.commits[5]));
    for (label, args) in [
        ("optional-missing", &["--bitmap", "--refs-snapshot=:(optional)nope"][..]),
        ("optional-present", &["--bitmap", "--refs-snapshot=:(optional)snap"]),
        ("emptied", &["--bitmap", "--refs-snapshot=snap", "--refs-snapshot="]),
        ("negated", &["--bitmap", "--refs-snapshot=snap", "--no-refs-snapshot"]),
    ] {
        let (code, stderr, _) = w.case(label, "", &one, args, None);
        assert_eq!((code, stderr.as_str()), (0, ""), "{label}");
    }
    // An up-to-date MIDX with its bitmap is left alone before the snapshot is read.
    let bitmap_first = |repo: &Path| {
        let out = git(w.stock, repo, &[], &["multi-pack-index", "write", "--bitmap"], None);
        assert_eq!(out.0, 0, "{out:?}");
        std::fs::write(repo.join("snap"), "zz\n").unwrap();
    };
    let (code, _, _) = w.case("up-to-date", "", &bitmap_first, &["--bitmap", "--refs-snapshot=snap"], None);
    assert_eq!(code, 0);
}
