//! What `git add` reports, in which order, and which paths it refuses to stage.
//!
//! Three rules from `builtin/add.c` and `convert.c`:
//!
//! * `get_conv_flags()` ties the `core.safecrlf` round-trip check to
//!   `HASH_WRITE_OBJECT`, so a dry run, `-N` and `--refresh` say nothing about
//!   pending EOL conversion, and `--renormalize` (`HASH_RENORMALIZE`) doesn't either.
//! * Staging runs index-first (`update_files_in_cache()`), then the *sorted*
//!   `dir->entries` (`add_files()`), and the report follows that order.
//! * Without `--sparse`, a path outside the sparse-checkout definition is skipped,
//!   named by `advise_on_updating_sparse_paths()`, and turns the exit code into 1 —
//!   while the paths inside the definition are still staged.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-addsparse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn write(&self, path: &str, body: &[u8]) {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(self.work.join(parent)).unwrap();
        }
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn status(&self) -> String {
        self.run(&["status", "--porcelain"]).1
    }

    /// A committed `seed.txt`, `core.autocrlf=true`, and three fresh files whose
    /// walk order (top level first) differs from their path order.
    fn crlf_repo(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.write("seed.txt", b"seed\n");
        f.git(&["-c", "core.autocrlf=false", "add", "seed.txt"]);
        f.git(&["commit", "-q", "-m", "init"]);
        f.git(&["config", "core.autocrlf", "true"]);
        f.write("zeta.txt", b"z\n");
        f.write("alpha.txt", b"a\n");
        f.write("sub/nested.txt", b"n\n");
        f.write("seed.txt", b"seed\nmore\n");
        f
    }
}

const WARN: &str = "warning: in the working copy of ";

/// Only a real add writes an object, so only a real add can warn.
#[test]
fn the_modes_that_write_no_object_stay_silent() {
    let f = Fixture::crlf_repo("silent");
    assert_eq!(f.run(&["add", "-n", "zeta.txt"]).2, "");
    assert_eq!(f.run(&["add", "-N", "zeta.txt"]).2, "");
    assert_eq!(f.run(&["add", "--refresh", "seed.txt"]).2, "");
    assert_eq!(f.run(&["add", "--renormalize", "seed.txt"]).2, "");
    // The real thing does warn, which is what makes the silence above meaningful.
    assert!(f.run(&["add", "zeta.txt"]).2.starts_with(WARN));
}

/// Tracked matches first in path order, then the new files in path order — not in
/// the order the directory walk reached them.
#[test]
fn the_report_follows_gits_staging_order() {
    let f = Fixture::crlf_repo("order");
    let (code, out, _) = f.run(&["add", "-n", "."]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "add 'seed.txt'\nadd 'alpha.txt'\nadd 'sub/nested.txt'\nadd 'zeta.txt'\n"
    );
}

/// `prefix_path()` normalizes the path before anything else looks at it.
#[test]
fn dot_components_are_normalized_away() {
    let f = Fixture::new("dots");
    f.write("README.md", b"r\n");
    f.write("src/lib.rs", b"l\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "init"]);
    f.write("README.md", b"r2\n");
    f.write("src/lib.rs", b"l2\n");

    assert_eq!(f.run(&["add", "src/."]), (0, String::new(), String::new()));
    assert_eq!(f.status(), " M README.md\nM  src/lib.rs\n");
    assert_eq!(f.run(&["add", "./."]), (0, String::new(), String::new()));
    assert_eq!(f.status(), "M  README.md\nM  src/lib.rs\n");
}

/// The paths inside the definition are staged, the ones outside are named and skipped,
/// and the exit code says so.
#[test]
fn paths_outside_the_sparse_definition_are_reported_not_staged() {
    let f = Fixture::new("sparse");
    f.write("inside/a.txt", b"a\n");
    f.write("outside/b.txt", b"b\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "init"]);
    f.git(&["sparse-checkout", "set", "inside"]);
    f.write("outside/stray.txt", b"s\n");
    f.write("inside/new.txt", b"i\n");

    let (code, _, err) = f.run(&["add", "."]);
    assert_eq!(code, 1, "stderr: {err}");
    assert_eq!(
        err,
        "The following paths and/or pathspecs matched paths that exist\n\
         outside of your sparse-checkout definition, so will not be\n\
         updated in the index:\n\
         outside/stray.txt\n\
         hint: If you intend to update such entries, try one of the following:\n\
         hint: * Use the --sparse option.\n\
         hint: * Disable or modify the sparsity rules.\n\
         hint: Disable this message with \"git config set advice.updateSparsePath false\"\n"
    );
    // The in-cone file was still staged, and the `skip-worktree` entry that has no
    // worktree copy was not mistaken for a deletion.
    assert_eq!(f.status(), "A  inside/new.txt\n?? outside/stray.txt\n");
}

/// `--sparse` is the documented way through, and then everything stages.
#[test]
fn the_sparse_option_stages_the_skipped_paths() {
    let f = Fixture::new("sparse-opt");
    f.write("inside/a.txt", b"a\n");
    f.write("outside/b.txt", b"b\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "init"]);
    f.git(&["sparse-checkout", "set", "inside"]);
    f.write("outside/stray.txt", b"s\n");

    assert_eq!(f.run(&["add", "--sparse", "."]), (0, String::new(), String::new()));
    assert_eq!(f.status(), "A  outside/stray.txt\n");
}

/// `-u` only ever looks at tracked paths, so an untracked file outside the definition
/// is not something it skipped — and must not be reported.
#[test]
fn update_only_reports_nothing_about_untracked_sparse_paths() {
    let f = Fixture::new("sparse-u");
    f.write("inside/a.txt", b"a\n");
    f.write("outside/b.txt", b"b\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "init"]);
    f.git(&["sparse-checkout", "set", "inside"]);
    f.write("outside/stray.txt", b"s\n");

    assert_eq!(f.run(&["add", "-u"]), (0, String::new(), String::new()));
}
