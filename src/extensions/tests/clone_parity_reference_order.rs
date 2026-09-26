//! The order, and the uniqueness, of the lines a clone writes to
//! `objects/info/alternates`.
//!
//! `setup_reference()` walks `option_required_reference` and then
//! `option_optional_reference` (builtin/clone.c:182-190), so every `--reference`
//! is added — or dies — before any `--reference-if-able` is looked at, whatever
//! order they were typed in. `-s` adds the source's store afterwards, from
//! `clone_local()` (:380-402), which runs later (:1314 vs :1354). Every line goes
//! through `odb_add_to_alternates_file()`, whose writer skips a store the file
//! already names (odb/source-files.c:227-249). zvcs wrote the lines in command-line
//! order with `-s` first and wrote duplicates, and printed the `info:` line for a
//! bad `--reference-if-able` before dying on a bad `--reference` typed after it.
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
    /// `src` with one commit, and two plain clones of it, `r1` and `r2`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-clone-reference-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        let src = f.root.join("src");
        f.run(&src, &["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("a"), "a\n").unwrap();
        f.run(&src, &["add", "a"]);
        f.run(&src, &["commit", "-q", "-m", "a"]);
        f.run(&f.root, &["clone", "-q", "src", "r1"]);
        f.run(&f.root, &["clone", "-q", "src", "r2"]);
        f
    }

    /// Clone `src` into `dst` with `opts`, and return the alternates file with
    /// the fixture root abbreviated to `$R`.
    fn alternates(&self, opts: &[&str]) -> String {
        let mut args = vec!["clone", "-q"];
        args.extend_from_slice(opts);
        args.extend_from_slice(&["src", "dst"]);
        let _ = std::fs::remove_dir_all(self.root.join("dst"));
        assert_eq!(self.run(&self.root, &args), (String::new(), String::new(), 0), "{opts:?}");
        std::fs::read_to_string(self.root.join("dst/.git/objects/info/alternates"))
            .unwrap()
            .replace(self.root.to_str().unwrap(), "$R")
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
fn required_references_come_before_optional_ones() {
    let f = Fixture::new("order");
    assert_eq!(
        f.alternates(&["--reference-if-able", "r1", "--reference", "r2"]),
        "$R/r2/.git/objects\n$R/r1/.git/objects\n"
    );
}

#[test]
fn shared_source_is_listed_after_the_references() {
    let f = Fixture::new("shared");
    assert_eq!(
        f.alternates(&["-s", "--reference", "r1"]),
        "$R/r1/.git/objects\n$R/src/.git/objects\n"
    );
}

#[test]
fn a_store_named_twice_is_recorded_once() {
    let f = Fixture::new("dedup");
    assert_eq!(
        f.alternates(&["--reference", "r1", "-s", "--reference", "r1/.git"]),
        "$R/r1/.git/objects\n$R/src/.git/objects\n"
    );
    assert_eq!(f.alternates(&["--reference", "src", "-s"]), "$R/src/.git/objects\n");
}

#[test]
fn a_bad_required_reference_dies_before_optional_ones_are_tried() {
    let f = Fixture::new("die");
    let (out, err, code) = f.run(
        &f.root,
        &["clone", "-q", "--reference-if-able", "bad", "--reference", "bad2", "src", "dst"],
    );
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: reference repository 'bad2' is not a local repository.\n", 128)
    );
    assert!(!f.root.join("dst").exists());
}
