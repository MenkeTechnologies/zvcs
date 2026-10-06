//! `git sparse-checkout` on a sparse index that stock git collapsed.
//!
//! With `index.sparse=true` stock git writes each wholly-excluded directory as one
//! `040000` entry plus the `sdir` extension. `update_sparsity()` expands those
//! before marking anything (unpack-trees.c:2157-2158, `expand_index(o->src_index,
//! pl)`). zvcs read them as ordinary entries: `add`/`set`/`reapply` died with
//! `Empty path components are not allowed`, `disable` exited 0 without restoring
//! the hidden files, and a write left `040000` entries behind that stock then
//! reported as `index entry is a directory, but not sparse`.
//!
//! The advice follows `expand_index()` (sparse-index.c:363-366): a cone pattern
//! list expands in place silently; `--no-sparse-index` (`update_modes()`'s
//! `ensure_full_index()`, builtin/sparse-checkout.c:432-441) and a non-cone list
//! expand to a full index and print `advice.sparseIndexExpanded`; `disable`
//! clears `give_advice_on_expansion` first (builtin/sparse-checkout.c:1071).
//!
//! The collapsed index is written by the fixture byte for byte as git lays it out
//! (version 3, extended flags for skip-worktree, `sdir`, and a null trailing hash,
//! which `verify_hdr()` accepts, read-cache.c:1721-1722).
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const EXPANDED_ADVICE: &str = "\
hint: The sparse index is expanding to a full index, a slow operation.
hint: Your working directory likely has contents that are outside of
hint: your sparse-checkout patterns. Use 'git sparse-checkout list' to
hint: see your sparse-checkout definition and compare it to your working
hint: directory contents. Cleaning up any merge conflicts or staged
hint: changes before running 'git sparse-checkout clean' or 'git
hint: sparse-checkout reapply' may assist in this cleanup.
hint: Disable this message with \"git config set advice.sparseIndexExpanded false\"
";

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
    /// `top`, `a/x`, `a/b/y`, `c/z`, `d/e/w` committed; cone `a` with
    /// `index.sparse=true`, and an index in which `c/` and `d/` are collapsed.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-sparse-collapsed-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("repo");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for p in ["top", "a/x", "a/b/y", "c/z", "d/e/w"] {
            let full = f.work.join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, format!("{p}\n")).unwrap();
        }
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "one"]);
        assert_eq!(f.run(&["sparse-checkout", "set", "--sparse-index", "a"]).2, 0);
        f.write_collapsed_index();
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

    fn oid(&self, spec: &str) -> [u8; 20] {
        let hex = self.run(&["rev-parse", spec]).0;
        let hex = hex.trim();
        let mut raw = [0u8; 20];
        for (i, byte) in raw.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        raw
    }

    /// The index `git sparse-checkout set --sparse-index a` leaves in stock git.
    fn write_collapsed_index(&self) {
        let entries: [(&str, u32, bool); 5] = [
            ("a/b/y", 0o100644, false),
            ("a/x", 0o100644, false),
            ("c/", 0o040000, true),
            ("d/", 0o040000, true),
            ("top", 0o100644, false),
        ];
        let mut buf = Vec::new();
        buf.extend_from_slice(b"DIRC");
        buf.extend_from_slice(&3u32.to_be_bytes());
        buf.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (path, mode, skip) in entries {
            let start = buf.len();
            // ctime, mtime (sec + nsec each), dev, ino: zero stat data.
            buf.extend_from_slice(&[0u8; 24]);
            buf.extend_from_slice(&mode.to_be_bytes());
            // uid, gid, size.
            buf.extend_from_slice(&[0u8; 12]);
            buf.extend_from_slice(&self.oid(&format!("HEAD:{}", path.trim_end_matches('/'))));
            let mut flags = path.len() as u16;
            if skip {
                flags |= 0x4000; // CE_EXTENDED
            }
            buf.extend_from_slice(&flags.to_be_bytes());
            if skip {
                buf.extend_from_slice(&0x4000u16.to_be_bytes()); // CE_SKIP_WORKTREE >> 16
            }
            buf.extend_from_slice(path.as_bytes());
            // NUL-terminated and padded to a multiple of eight.
            let len = buf.len() - start;
            buf.resize(start + (len + 8) / 8 * 8, 0);
        }
        buf.extend_from_slice(b"sdir");
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&[0u8; 20]);
        std::fs::write(self.work.join(".git/index"), buf).unwrap();
    }

    fn ls_files_t(&self) -> String {
        self.run(&["ls-files", "-t"]).0
    }

    fn assert_full_index(&self) {
        let (out, _, code) = self.run(&["ls-files", "--sparse", "-s"]);
        assert_eq!(code, 0);
        assert!(!out.contains("040000"), "a sparse-directory entry survived:\n{out}");
        let raw = std::fs::read(self.work.join(".git/index")).unwrap();
        assert!(!raw.windows(4).any(|w| w == b"sdir"), "sdir written for a full index");
    }

    /// The index stock git leaves: still sparse (`sdir`), with exactly `dirs` collapsed —
    /// `convert_to_sparse()` collapses every directory the new cone leaves out
    /// (sparse-index.c:201-259), measured on stock git 2.56.0.
    fn assert_collapsed(&self, dirs: &[&str]) {
        let (out, _, code) = self.run(&["ls-files", "--sparse", "-s"]);
        assert_eq!(code, 0);
        let collapsed: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with("040000 "))
            .filter_map(|l| l.split('\t').nth(1))
            .collect();
        assert_eq!(collapsed, dirs, "collapsed directories:\n{out}");
        let raw = std::fs::read(self.work.join(".git/index")).unwrap();
        assert!(raw.windows(4).any(|w| w == b"sdir"), "a sparse index keeps its sdir extension");
    }
}

#[test]
fn add_expands_the_directory_it_adds_silently() {
    let f = Fixture::new("add");
    let (out, err, code) = f.run(&["sparse-checkout", "add", "c"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(f.ls_files_t(), "H a/b/y\nH a/x\nH c/z\nS d/e/w\nH top\n");
    assert_eq!(std::fs::read_to_string(f.work.join("c/z")).unwrap(), "c/z\n");
    assert!(!f.work.join("d").exists());
    f.assert_collapsed(&["d/"]);
}

#[test]
fn reapply_and_set_keep_the_hidden_directories_hidden() {
    let f = Fixture::new("reapply");
    let (out, err, code) = f.run(&["sparse-checkout", "reapply"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(f.ls_files_t(), "H a/b/y\nH a/x\nS c/z\nS d/e/w\nH top\n");
    f.assert_collapsed(&["c/", "d/"]);

    let f = Fixture::new("set");
    assert_eq!(f.run(&["sparse-checkout", "set", "d"]), (String::new(), String::new(), 0));
    assert_eq!(f.ls_files_t(), "S a/b/y\nS a/x\nS c/z\nH d/e/w\nH top\n");
    assert_eq!(std::fs::read_to_string(f.work.join("d/e/w")).unwrap(), "d/e/w\n");
    assert!(!f.work.join("a").exists());
    f.assert_collapsed(&["a/", "c/"]);
}

#[test]
fn disable_restores_every_file_without_advice() {
    let f = Fixture::new("disable");
    let (out, err, code) = f.run(&["sparse-checkout", "disable"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(f.ls_files_t(), "H a/b/y\nH a/x\nH c/z\nH d/e/w\nH top\n");
    for p in ["c/z", "d/e/w"] {
        assert_eq!(std::fs::read_to_string(f.work.join(p)).unwrap(), format!("{p}\n"));
    }
    f.assert_full_index();
}

#[test]
fn a_full_expansion_prints_the_advice() {
    for (tag, args) in [
        ("nosi", &["sparse-checkout", "reapply", "--no-sparse-index"][..]),
        ("nocone", &["sparse-checkout", "set", "--no-cone", "d"][..]),
    ] {
        let f = Fixture::new(tag);
        let (out, err, code) = f.run(args);
        assert_eq!((out.as_str(), err.as_str(), code), ("", EXPANDED_ADVICE, 0), "{tag}");
        f.assert_full_index();
    }
}
