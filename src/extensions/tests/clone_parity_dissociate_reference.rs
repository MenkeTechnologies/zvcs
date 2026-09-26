//! `clone --reference <repo> --dissociate` deleted the reference's packs.
//!
//! `dissociate_from_references()` runs `repack -a -d` in the new clone and then
//! unlinks `objects/info/alternates` (builtin/clone.c:845-861). `repack` builds its
//! list of packs `-d` may remove in `existing_packs_collect()`, which skips every
//! pack that is not `pack_local` (repack.c:133-141), so the packs of a borrowed
//! object store are copied from — `pack-objects` runs without `--local` — and
//! never deleted. zvcs's `repack` put the alternate's packs on the `-d` list, so a
//! `--dissociate` clone emptied the reference repository's `objects/pack`: the
//! reference failed `fsck`, and the next `--reference` clone from it died with
//! `Object <oid> ... could not be found`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `src` holds two commits; `ref.git` is a `--no-local` bare clone of it, so
    /// its objects live in a pack of its own.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-clone-dissociate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let f = Fixture { root };
        let src = f.root.join("src");
        f.run(&src, &["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("a"), "a\n").unwrap();
        f.run(&src, &["add", "a"]);
        f.run(&src, &["commit", "-q", "-m", "a"]);
        std::fs::write(src.join("b"), "b\n").unwrap();
        f.run(&src, &["add", "b"]);
        f.run(&src, &["commit", "-q", "-m", "b"]);
        f.run(&f.root, &["clone", "-q", "--bare", "--no-local", "src", "ref.git"]);
        f
    }

    fn src_url(&self) -> String {
        format!("file://{}", self.root.join("src").display())
    }

    fn packs(&self, git_dir: &str) -> usize {
        std::fs::read_dir(self.root.join(git_dir).join("objects/pack"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".pack"))
            .count()
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(cwd)
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
}

#[test]
fn dissociate_leaves_the_reference_whole_and_reusable() {
    let f = Fixture::new("clone");
    let url = f.src_url();
    let ref_packs = f.packs("ref.git");
    assert_eq!(ref_packs, 1);

    let (out, err, code) =
        f.run(&f.root, &["clone", "-q", "--reference", "ref.git", "--dissociate", &url, "dst"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(f.packs("ref.git"), ref_packs);
    assert!(!f.root.join("dst/.git/objects/info/alternates").exists());
    assert_eq!(f.run(&f.root.join("ref.git"), &["fsck"]), (String::new(), String::new(), 0));
    assert_eq!(f.run(&f.root.join("dst"), &["fsck"]), (String::new(), String::new(), 0));

    // The reference serves a second clone just as well as the first.
    let (out, err, code) = f.run(
        &f.root,
        &["clone", "-q", "--reference-if-able", "ref.git", "--dissociate", &url, "dst2"],
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    let log = f.run(&f.root.join("dst2"), &["log", "--format=%s"]);
    assert_eq!(log, ("b\na\n".to_string(), String::new(), 0));
}

#[test]
fn repack_all_copies_borrowed_objects_and_deletes_only_local_packs() {
    let f = Fixture::new("repack");
    let plain = f.root.join("plain");
    f.run(&f.root, &["init", "-q", "-b", "main", "plain"]);
    std::fs::write(
        plain.join(".git/objects/info/alternates"),
        format!("{}\n", f.root.join("ref.git/objects").display()),
    )
    .unwrap();
    f.run(&plain, &["fetch", "-q", "../ref.git", "main:main"]);

    let (out, err, code) = f.run(&plain, &["repack", "-a", "-d", "-q"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(f.packs("ref.git"), 1);
    assert_eq!(f.packs("plain/.git"), 1);

    // Every object is now local: without the alternate the repository is whole.
    std::fs::remove_file(plain.join(".git/objects/info/alternates")).unwrap();
    assert_eq!(f.run(&plain, &["fsck"]), (String::new(), String::new(), 0));
    assert_eq!(f.run(&f.root.join("ref.git"), &["fsck"]), (String::new(), String::new(), 0));
}
