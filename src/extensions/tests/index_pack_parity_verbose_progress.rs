//! `index-pack -v` and `bundle unbundle --progress` draw git's meters.
//!
//! `parse_pack_objects()` starts `progress_title ? progress_title : from_stdin
//! ? "Receiving objects" : "Indexing objects"` under `-v`
//! (builtin/index-pack.c:1258-1263) and `Resolving deltas` after it
//! (:1340-1343); `unbundle()` runs its child as `index-pack -v
//! --progress-title "Unbundling objects"` under `--progress` (bundle.c). A
//! second `--progress-title` is a usage error (:1974-1977). zvcs accepted the
//! flags and drew nothing.
//!
//! Every read of a pack this small happens before the meter starts, so no
//! throughput appears. Expectations measured from stock git 2.55.0 under the
//! same environment.

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
    /// `src`: three commits growing `f` (so the pack has deltas), packed into
    /// `../p.pack` and bundled into `../b.bundle`; `dst`: empty.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-index-pack-progress-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let f = Fixture { root, src, dst };
        f.run_in(&f.src, &["init", "-q", "-b", "main", "."], None);
        f.run_in(&f.dst, &["init", "-q", "-b", "main", "."], None);
        for i in 1..=3 {
            let body: String = (1..=i * 200).map(|n| format!("{n}\n")).collect();
            std::fs::write(f.src.join("f"), body).unwrap();
            f.run_in(&f.src, &["add", "f"], None);
            f.run_in(&f.src, &["commit", "-q", "-m", &format!("c{i}")], None);
        }
        let pack = Command::new(BIN)
            .args(["pack-objects", "--all", "--stdout"])
            .current_dir(&f.src)
            .env("HOME", &f.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .stdout;
        std::fs::write(f.root.join("p.pack"), pack).unwrap();
        f.run_in(&f.src, &["bundle", "create", "-q", "../b.bundle", "main"], None);
        f
    }

    fn run_in(&self, dir: &PathBuf, args: &[&str], stdin: Option<&str>) -> (String, String, i32) {
        let input = match stdin {
            Some(name) => Stdio::from(std::fs::File::open(self.root.join(name)).unwrap()),
            None => Stdio::null(),
        };
        let out = Command::new(BIN)
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
            .stdin(input)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// git's meter for `n` items under `title`, redrawn at every whole percent and
/// closed with `, done.`.
fn meter(title: &str, n: usize, from_zero: bool) -> String {
    let mut out = String::new();
    let start = if from_zero { 0 } else { 1 };
    for i in start..=n {
        out.push_str(&format!("{title}: {:>3}% ({i}/{n})\r", i * 100 / n));
    }
    out.push_str(&format!("{title}: 100% ({n}/{n}), done.\n"));
    out
}

#[test]
fn stdin_and_named_packs_draw_their_titles() {
    let f = Fixture::new("titles");
    let deltas = meter("Resolving deltas", 2, true);

    let (out, err, code) = f.run_in(&f.dst, &["index-pack", "-v", "--stdin"], Some("p.pack"));
    assert_eq!((err, code), (meter("Receiving objects", 9, false) + &deltas, 0));
    assert!(out.starts_with("pack\t"), "{out}");

    let (_, err, code) = f.run_in(
        &f.dst,
        &["index-pack", "-v", "--stdin", "--progress-title", "Hello"],
        Some("p.pack"),
    );
    assert_eq!((err, code), (meter("Hello", 9, false) + &deltas, 0));

    std::fs::copy(f.root.join("p.pack"), f.root.join("n.pack")).unwrap();
    let (_, err, code) = f.run_in(&f.dst, &["index-pack", "-v", "../n.pack"], None);
    assert_eq!((err, code), (meter("Indexing objects", 9, false) + &deltas, 0));

    let (_, err, code) = f.run_in(&f.dst, &["index-pack", "--stdin"], Some("p.pack"));
    assert_eq!((err.as_str(), code), ("", 0));
}

#[test]
fn unbundle_progress_is_unbundling_objects() {
    let f = Fixture::new("unbundle");
    let (out, err, code) = f.run_in(&f.dst, &["bundle", "unbundle", "--progress", "../b.bundle"], None);
    assert_eq!(
        (err, code),
        (meter("Unbundling objects", 9, false) + &meter("Resolving deltas", 2, true), 0)
    );
    assert!(out.ends_with(" refs/heads/main\n"), "{out}");
}

#[test]
fn a_second_progress_title_is_a_usage_error() {
    let f = Fixture::new("twice");
    let (out, err, code) = f.run_in(
        &f.dst,
        &["index-pack", "--progress-title", "a", "--progress-title", "b", "--stdin"],
        Some("p.pack"),
    );
    assert_eq!((out.as_str(), code), ("", 129));
    assert!(err.starts_with("usage: git index-pack "), "{err}");
}
