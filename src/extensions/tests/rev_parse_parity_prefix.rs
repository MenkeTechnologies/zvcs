//! `git rev-parse --prefix <dir>` and the prefix the rest of the scan reads.
//!
//! ```c
//! if (!strcmp(arg, "--prefix")) {
//!         prefix = argv[++i];
//!         if (!prefix)
//!                 die(_("--prefix requires an argument"));
//!         startup_info->prefix = prefix;
//!         output_prefix = 1;
//!         continue;
//! }
//! ```
//!
//! (builtin/rev-parse.c:838-845.) The local `prefix` feeds `print_path()`,
//! `--show-prefix` and `--show-cdup` (:1011-1032) and `verify_filename()`;
//! the global one feeds `show_file()` (:253-266), `check_filename()`
//! (setup.c:173-198) and `resolve_relative_path()` (object-name.c:1702-1714).
//! zvcs refused the option outright. The same path check also ignored the
//! computed prefix, so from a subdirectory `git rev-parse f` died about a file
//! sitting next to the user and accepted a name only the top level has.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
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

const AMBIGUOUS: &str = "unknown revision or path not in the working tree.\n\
Use '--' to separate paths from revisions, like this:\n\
'git <command> [<revision>...] -- [<file>...]'\n";

impl Fixture {
    /// `sub/f` and `top`, committed.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rev-parse-prefix-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub/deep")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("work");
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("sub/f"), "a\n").unwrap();
        std::fs::write(f.work.join("top"), "t\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0
    }
}

#[test]
fn paths_are_echoed_and_checked_under_the_given_prefix() {
    let f = Fixture::new("paths");
    assert_eq!(f.run(&["rev-parse", "--prefix", "sub/", "f"]), ("sub/f\n".into(), String::new(), 0));
    // Checked from the top of the work tree, so a name only the top has fails.
    let (out, err, code) = f.run(&["rev-parse", "--prefix", "sub/", "top"]);
    assert_eq!((out.as_str(), code), ("sub/top\n", 128));
    assert_eq!(err, format!("fatal: ambiguous argument 'top': {AMBIGUOUS}"));
    // Concatenated verbatim: no slash, no directory.
    let (out, _, code) = f.run(&["rev-parse", "--prefix", "sub", "f"]);
    assert_eq!((out.as_str(), code), ("subf\n", 128));
    // After `--` nothing is checked, and `--` itself is not prefixed.
    assert_eq!(
        f.run(&["rev-parse", "--prefix", "sub/", "--", "f", "nothere"]),
        ("--\nsub/f\nsub/nothere\n".into(), String::new(), 0)
    );
    // `:!` keeps the prefix for the check, `:/` drops it; the echo is prefixed either way.
    assert_eq!(
        f.run(&["rev-parse", "--prefix", "sub/", ":!f", ":/top"]),
        ("sub/:!f\nsub/:/top\n".into(), String::new(), 0)
    );
    // Only paths after the option are prefixed: the first `f` is checked at the top.
    let (out, err, code) = f.run(&["rev-parse", "f", "--prefix", "sub/", "f"]);
    assert_eq!((out.as_str(), code), ("f\n", 128));
    assert_eq!(err, format!("fatal: ambiguous argument 'f': {AMBIGUOUS}"));
    // `--prefix=<dir>` is not the option and is echoed as a flag.
    let (out, _, code) = f.run(&["rev-parse", "--prefix=sub/", "f"]);
    assert_eq!((out.as_str(), code), ("--prefix=sub/\nf\n", 128));
    assert_eq!(
        f.run(&["rev-parse", "--prefix"]),
        (String::new(), "fatal: --prefix requires an argument\n".into(), 128)
    );
}

#[test]
fn relative_object_names_resolve_under_the_given_prefix() {
    let f = Fixture::new("objname");
    let blob_f = f.rev("HEAD:sub/f");
    let blob_top = f.rev("HEAD:top");
    for spec in ["HEAD:./f", ":./f"] {
        assert_eq!(f.run(&["rev-parse", "--prefix", "sub/", spec]), (blob_f.clone(), String::new(), 0), "{spec}");
    }
    assert_eq!(f.run(&["rev-parse", "--prefix", "sub/", ":../top"]), (blob_top, String::new(), 0));
    let top = f.work.display().to_string();
    assert_eq!(
        f.run(&["rev-parse", "--prefix", "sub/", ":../../x"]),
        (String::new(), format!("fatal: '../../x' is outside repository at '{top}'\n"), 128)
    );
    // `diagnose_invalid_oid_path()` offers the prefixed spelling.
    assert_eq!(
        f.run(&["rev-parse", "--prefix", "sub/", "HEAD:f"]),
        (
            "sub/HEAD:f\n".into(),
            "fatal: path 'sub/f' exists, but not 'f'\nhint: Did you mean 'HEAD:sub/f' aka 'HEAD:./f'?\n".into(),
            128
        )
    );
}

#[test]
fn prefix_relative_queries_read_the_given_prefix() {
    let f = Fixture::new("queries");
    let top = f.work.display().to_string();
    assert_eq!(
        f.run(&["rev-parse", "--prefix", "sub/deep/", "--show-prefix", "--show-cdup"]),
        ("sub/deep/\n../../\n".into(), String::new(), 0)
    );
    // One `../` per slash.
    assert_eq!(f.run(&["rev-parse", "--prefix", "sub", "--show-cdup"]), ("\n".into(), String::new(), 0));
    assert_eq!(
        f.run(&["rev-parse", "--prefix", "sub/", "--git-dir", "--git-common-dir", "--git-path", "HEAD"]),
        (format!("{top}/.git\n../.git\n../.git/HEAD\n"), String::new(), 0)
    );
    // `""` is a prefix, not NULL: `--git-dir` takes the `<cwd>/.git` arm.
    assert_eq!(
        f.run(&["rev-parse", "--prefix", "", "--git-dir", "--git-common-dir", "--show-prefix"]),
        (format!("{top}/.git\n.git\n\n"), String::new(), 0)
    );
    assert_eq!(
        f.run(&[
            "rev-parse", "--prefix", "sub/deep/", "--path-format=relative", "--git-dir", "--git-path", "HEAD",
            "--show-toplevel",
        ]),
        ("../../.git\n../../.git/HEAD\n../../\n".into(), String::new(), 0)
    );
}

#[test]
fn a_subdirectory_checks_paths_against_its_own_prefix() {
    let f = Fixture::new("subdir");
    let sub = f.work.join("sub");
    assert_eq!(f.run_in(&sub, &["rev-parse", "f", "../top"]), ("f\n../top\n".into(), String::new(), 0));
    let (out, err, code) = f.run_in(&sub, &["rev-parse", "top"]);
    assert_eq!((out.as_str(), code), ("top\n", 128));
    assert_eq!(err, format!("fatal: ambiguous argument 'top': {AMBIGUOUS}"));
    // A replaced prefix is measured from the top, not from `sub/`.
    let (out, _, code) = f.run_in(&sub, &["rev-parse", "--prefix", "", "f"]);
    assert_eq!((out.as_str(), code), ("f\n", 128));
}
