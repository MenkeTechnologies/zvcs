//! `setup_revisions()`'s filename fallback, which `rev-list` shares with `log`.
//!
//! ```c
//! if (handle_revision_arg(arg, revs, flags, revarg_opt)) {
//!         int j;
//!         if (seen_dashdash || *arg == '^')
//!                 die(_("bad revision '%s'"), arg);
//!
//!         /* If we didn't have a "--":
//!          * (1) all filenames must exist;
//!          * (2) all rev-args must not be interpretable
//!          *     as a valid filename.
//!          * but the latter we have checked in the main loop.
//!          */
//!         for (j = i; j < argc; j++)
//!                 verify_filename(revs->prefix, argv[j], j == i);
//!
//!         append_prune_data(&prune_data, argv + i);
//!         break;
//! }
//! ```
//! (`revision.c:3080-3097`, v2.55.0)
//!
//! Two defects this file pins down.
//!
//! * `rev-list` never ran the fallback at all: it raised the `die()` for every
//!   operand that failed as a revision, so `git rev-list HEAD <path>` — the
//!   spelling t6001 and t6000 use throughout — was `fatal: ambiguous argument`
//!   while `git log <path>` worked.
//! * `verify_filename()` decides with
//!   `looks_like_pathspec(arg) || check_filename(prefix, arg)` (`setup.c:289`),
//!   and neither half is an `exists()` test: `looks_like_pathspec()`
//!   (`setup.c:232-260`) accepts a glob special and the long-form `:(…)` magic
//!   whatever is on disk, and `check_filename()` (`setup.c:173-200`) strips the
//!   short-form `:/`, `:!` and `:^` before it stats — a bare one of those being
//!   accepted outright, since "excluding everything is silly, but allowed".
//!
//! What must keep failing is as much of the rule as what must start working: an
//! operand that resolved *and* names a file is `verify_non_filename()`'s
//! `both revision and filename` die from inside `handle_revision_arg()`, below
//! the branch above, and the fallback may not swallow it.
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
    /// One commit holding `kept`, `sub/f` and a `dual` that is also a branch.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-pospath-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("kept"), "a\n").unwrap();
        std::fs::write(f.work.join("sub/f"), "b\n").unwrap();
        std::fs::write(f.work.join("dual"), "c\n").unwrap();
        f.git(&["add", "kept", "sub", "dual"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.git(&["branch", "dual"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
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
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn oid(&self, spec: &str) -> String {
        let (out, err, code) = self.run(&["rev-parse", spec]);
        assert_eq!((err.as_str(), code), ("", 0), "rev-parse {spec}");
        out.trim().to_string()
    }
}

/// `git rev-list <rev> <path>` with no `--` prunes with the path instead of
/// dying, and so does the two-operand form the graft tests use.
#[test]
fn rev_list_takes_a_trailing_path_without_a_separator() {
    let f = Fixture::new("plain");
    let head = f.oid("HEAD");

    for args in [
        vec!["rev-list", "HEAD", "kept"],
        vec!["rev-list", "HEAD", "sub"],
        vec!["rev-list", "HEAD", "sub/f"],
        vec!["rev-list", "--all", "kept"],
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        assert_eq!(out, format!("{head}\n"), "{args:?}");
    }
}

/// Pathspec magic is a path to `verify_filename()` even when nothing of that
/// name exists: the long form through `looks_like_pathspec()`, the short forms
/// through `check_filename()`'s prefix strip.
#[test]
fn pathspec_magic_is_a_path_not_an_ambiguous_revision() {
    let f = Fixture::new("magic");
    let head = f.oid("HEAD");
    for spec in [":^sub", ":!sub", ":(exclude)sub", ":/"] {
        let (out, err, code) = f.run(&["log", "--format=%H", spec]);
        assert_eq!((err.as_str(), code), ("", 0), "log {spec}");
        assert_eq!(out, format!("{head}\n"), "log {spec}");

        let (out, err, code) = f.run(&["rev-list", "HEAD", spec]);
        assert_eq!((err.as_str(), code), ("", 0), "rev-list {spec}");
        assert_eq!(out, format!("{head}\n"), "rev-list {spec}");
    }

    // A bare short-form exclude is accepted by `check_filename()` ("excluding
    // everything is silly, but allowed") and then excludes everything, so the
    // walk finds no commit that touched a matching path — exit 0, no output.
    for spec in [":^", ":!"] {
        let (out, err, code) = f.run(&["log", "--format=%H", spec]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0), "log {spec}");

        let (out, err, code) = f.run(&["rev-list", "HEAD", spec]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0), "rev-list {spec}");
    }
}

/// Unknown magic reaches the pathspec parser rather than the revision error, so
/// the refusal names the magic.
#[test]
fn unknown_magic_is_refused_by_the_pathspec_parser() {
    let f = Fixture::new("badmagic");
    for args in [
        vec!["log", ":(unknown-magic)"],
        vec!["rev-list", "HEAD", ":(unknown-magic)"],
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!((out.as_str(), code), ("", 128), "{args:?}");
        assert!(err.contains("magic"), "{args:?}: {err}");
    }
}

/// The fallback only covers `handle_revision_arg()` *returning* non-zero. A name
/// that resolves and is also a file dies from inside it, and a `--` anywhere in
/// the vector takes a token that failed to the short `bad revision` instead.
#[test]
fn the_fallback_does_not_swallow_the_dies_below_it() {
    let f = Fixture::new("dies");

    let (out, err, code) = f.run(&["rev-list", "--count", "dual"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err,
        "fatal: ambiguous argument 'dual': both revision and filename\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    );

    // `seen_dashdash` is a scan of the whole vector, so it gates an operand
    // written in front of the separator too.
    let (out, err, code) = f.run(&["rev-list", "nosuchrev", "--", "kept"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: bad revision 'nosuchrev'\n");

    // A `^` operand never becomes prune data either, even naming a real file.
    let (out, err, code) = f.run(&["rev-list", "HEAD", "^kept"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: bad revision '^kept'\n");
}

/// `verify_filename(…, j == i)` — every operand after the one that triggered the
/// fallback is already known to be in path position, so its failure is the
/// shorter `no such path in the working tree`.
#[test]
fn a_later_operand_that_is_no_path_gets_the_short_message() {
    let f = Fixture::new("tail");
    let (out, err, code) = f.run(&["rev-list", "HEAD", "kept", "nosuchpath"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err,
        "fatal: nosuchpath: no such path in the working tree.\n\
         Use 'git <command> -- <path>...' to specify paths that do not exist locally.\n"
    );
}
