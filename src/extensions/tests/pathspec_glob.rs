//! Glob pathspecs, and the magic prefixes that change how a glob is read.
//!
//! Every expectation here was taken from stock git 2.55.0 run against the same
//! fixture, and is hardcoded rather than compared at runtime so the suite does
//! not depend on a `git` being installed on the machine running it.
//!
//! The distinctions worth holding still are the ones a naive implementation
//! collapses:
//!
//! * A bare `*` in a pathspec is *not* a shell glob — it matches `/` too, so
//!   `*.md` reaches `docs/guide.md` and `bin/*` reaches `bin/nested/deep.sh`.
//! * `:(glob)` switches to `FNM_PATHNAME` semantics, where `*` stops at a slash
//!   and only `**` spans directories. `:(glob)*.md` therefore drops the nested
//!   file that plain `*.md` returns — the two must not agree.
//! * `:(icase)` folds case, `:!` inverts the match.
//!
//! The fixture is built with `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` pointed at
//! `/dev/null`. That is not decoration: a developer's global `core.excludesFile`
//! ignoring `bin/` or `docs/` silently keeps those paths out of the index, and
//! every directory expectation below then passes vacuously with an empty list.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A repository with files chosen so each pathspec form has something to match
/// at the top level *and* something nested underneath it.
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
        let root = std::env::temp_dir().join(format!("zvcs-pathspec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };

        for dir in ["bin/nested", "docs/sub", "src", ".github/workflows"] {
            std::fs::create_dir_all(f.work.join(dir)).unwrap();
        }
        // `notes.md` and `bin/tool.sh` carry the needle the grep cases search for.
        for (path, body) in [
            ("README.md", "top\n"),
            ("notes.md", "chmod -R 755 x\n"),
            ("UPPER.MD", "UPPER\n"),
            ("bin/tool.sh", "#!/bin/sh\nchmod +x\n"),
            ("bin/nested/deep.sh", "#!/bin/sh\ndeep\n"),
            ("docs/guide.md", "nested md\n"),
            ("docs/index.html", "<html>\n"),
            ("docs/sub/page.html", "<html>\n"),
            ("src/main.rs", "fn main() {}\n"),
            ("src/lib.rs", "pub fn f() {}\n"),
            (".github/workflows/ci.yml", "on: push\n"),
        ] {
            std::fs::write(f.work.join(path), body).unwrap();
        }

        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "fixture"]);
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

    /// The paths a verb reports, as a plain `Vec` for comparison.
    fn paths(&self, args: &[&str]) -> Vec<String> {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn ls_files(&self, spec: &str) -> Vec<String> {
        self.paths(&["ls-files", "--", spec])
    }
}

/// Everything the fixture tracks, in git's index order — the baseline the
/// filtered cases are carved out of.
#[test]
fn fixture_tracks_the_expected_paths() {
    let f = Fixture::new("all");
    assert_eq!(
        f.paths(&["ls-files"]),
        [
            ".github/workflows/ci.yml",
            "README.md",
            "UPPER.MD",
            "bin/nested/deep.sh",
            "bin/tool.sh",
            "docs/guide.md",
            "docs/index.html",
            "docs/sub/page.html",
            "notes.md",
            "src/lib.rs",
            "src/main.rs",
        ]
    );
}

/// A bare `*` is not a shell glob: it matches across `/`, so an extension
/// pattern reaches nested files and a directory prefix reaches nested files.
#[test]
fn bare_wildcard_crosses_directories() {
    let f = Fixture::new("bare");
    assert_eq!(f.ls_files("*.md"), ["README.md", "docs/guide.md", "notes.md"]);
    assert_eq!(f.ls_files("bin/*"), ["bin/nested/deep.sh", "bin/tool.sh"]);
    assert_eq!(f.ls_files("src/*.rs"), ["src/lib.rs", "src/main.rs"]);
    // Case is significant without `:(icase)`.
    assert_eq!(f.ls_files("*.MD"), ["UPPER.MD"]);
}

/// `:(glob)` switches to pathname semantics: `*` stops at a slash. Each of
/// these must return strictly less than its bare counterpart above — that
/// inequality is the whole point of the magic prefix.
#[test]
fn glob_magic_stops_at_a_slash() {
    let f = Fixture::new("globmagic");
    assert_eq!(f.ls_files(":(glob)*.md"), ["README.md", "notes.md"]);
    assert_eq!(f.ls_files(":(glob)bin/*"), ["bin/tool.sh"]);

    // Stated as the contrast, so a change that made `:(glob)` behave like the
    // bare form would fail here even if the literal lists above were updated.
    assert_ne!(f.ls_files(":(glob)*.md"), f.ls_files("*.md"));
    assert_ne!(f.ls_files(":(glob)bin/*"), f.ls_files("bin/*"));
}

/// `**` spans directories under both readings.
#[test]
fn double_star_spans_directories() {
    let f = Fixture::new("doublestar");
    assert_eq!(f.ls_files("**/*.yml"), [".github/workflows/ci.yml"]);
    assert_eq!(f.ls_files(":(glob)**/*.yml"), [".github/workflows/ci.yml"]);
    assert_eq!(
        f.ls_files("docs/**"),
        ["docs/guide.md", "docs/index.html", "docs/sub/page.html"]
    );
}

/// `:(icase)` folds case, and still crosses `/` the way a bare pattern does.
#[test]
fn icase_magic_folds_case() {
    let f = Fixture::new("icase");
    assert_eq!(
        f.ls_files(":(icase)*.MD"),
        ["README.md", "UPPER.MD", "docs/guide.md", "notes.md"]
    );
}

/// `:!` inverts: everything the pattern would have matched is dropped.
#[test]
fn exclude_magic_inverts_the_match() {
    let f = Fixture::new("exclude");
    assert_eq!(
        f.ls_files(":!docs/*"),
        [
            ".github/workflows/ci.yml",
            "README.md",
            "UPPER.MD",
            "bin/nested/deep.sh",
            "bin/tool.sh",
            "notes.md",
            "src/lib.rs",
            "src/main.rs",
        ]
    );
}

/// Pathspecs limit `grep` the same way they limit `ls-files` — the search is
/// confined to matching paths, and the magic prefixes apply there too.
#[test]
fn grep_honors_glob_pathspecs() {
    let f = Fixture::new("grep");
    assert_eq!(f.paths(&["grep", "-l", "chmod", "--", "*.md"]), ["notes.md"]);
    assert_eq!(f.paths(&["grep", "-l", "chmod", "--", "bin/*"]), ["bin/tool.sh"]);
    assert_eq!(f.paths(&["grep", "-l", "chmod", "--", ":(glob)bin/*"]), ["bin/tool.sh"]);
    // A pathspec that matches no tracked path finds nothing, rather than
    // falling back to searching everything.
    let out = f.cmd(&["grep", "-l", "chmod", "--", "*.nomatch"]).output().unwrap();
    assert!(String::from_utf8(out.stdout).unwrap().is_empty());
}
