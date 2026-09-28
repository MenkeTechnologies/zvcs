//! `pack-objects --local` and `repack -l` in a repository that borrows from an
//! alternate.
//!
//! `want_object_in_pack()` under `--local` leaves out every object an
//! alternate holds — loose, or in a pack "borrowed from elsewhere", whatever
//! local copy there is (builtin/pack-objects.c:1615-1830) — and `repack -l`
//! passes `--local` to its `pack-objects`. zvcs parsed both flags and ignored
//! them, packing the borrowed objects too.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    alt: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `src`: three commits, packed; `alt`: `clone --shared` of it plus one
    /// commit of its own, left loose.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-pack-objects-local-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let alt = root.join("alt");
        let f = Fixture { root, alt };
        f.git(&src, &["init", "-q", "-b", "main", "."]);
        for i in 1..=3 {
            let body: String = (1..=i * 100).map(|n| format!("{n}\n")).collect();
            std::fs::write(src.join("f"), body).unwrap();
            f.git(&src, &["add", "f"]);
            f.git(&src, &["commit", "-q", "-m", &format!("c{i}")]);
            f.git(&src, &["repack", "-q"]);
        }
        f.git(&f.root, &["clone", "-q", "--shared", "src", "alt"]);
        std::fs::write(f.alt.join("m"), "more\n").unwrap();
        f.git(&f.alt, &["add", "m"]);
        f.git(&f.alt, &["commit", "-q", "-m", "own"]);
        f
    }

    fn git(&self, dir: &PathBuf, args: &[&str]) -> (Vec<u8>, String, i32) {
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
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().unwrap())
    }

    /// The object count in the header of `pack-objects <args> --stdout`.
    fn packed(&self, args: &[&str]) -> u32 {
        let mut argv = vec!["pack-objects"];
        argv.extend_from_slice(args);
        argv.push("--stdout");
        let (pack, err, code) = self.git(&self.alt, &argv);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        u32::from_be_bytes([pack[8], pack[9], pack[10], pack[11]])
    }

    /// The object count of every pack in `alt`, sorted.
    fn packs(&self) -> Vec<u32> {
        let dir = self.alt.join(".git/objects/pack");
        let mut counts: Vec<u32> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "pack"))
            .map(|p| {
                let bytes = std::fs::read(p).unwrap();
                u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]])
            })
            .collect();
        counts.sort();
        counts
    }
}

#[test]
fn local_leaves_out_what_the_alternate_holds() {
    let f = Fixture::new("pack");
    assert_eq!(f.packed(&["--all"]), 12);
    assert_eq!(f.packed(&["--all", "--local"]), 3);
    assert_eq!(f.packed(&["--all", "--local", "--no-local"]), 12);
}

#[test]
fn repack_l_packs_only_the_repositorys_own_objects() {
    let f = Fixture::new("repack");
    let (_, err, code) = f.git(&f.alt, &["repack", "-a", "-d", "-l", "-q"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(f.packs(), [3]);
    let (_, err, code) = f.git(&f.alt, &["repack", "-a", "-d", "-q"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(f.packs(), [12]);
}
