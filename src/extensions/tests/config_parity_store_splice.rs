//! `git config` rewrites a config file by splicing bytes, not by re-serializing it.
//!
//! Every expectation here is a byte-for-byte property of git's
//! `repo_config_set_multivar_in_file_gently()` (config.c:2999-3244) and
//! `repo_config_copy_or_rename_section_in_file()` (config.c:3353-3505), measured against
//! stock git 2.55.0:
//!
//!   * A new variable is written with the case the caller typed, because `write_pair()`
//!     is handed the raw `key` argument and only skips `store->baselen + 1` bytes of it
//!     (config.c:2746-2799) — the lower-cased `store.key` is used for *matching* alone.
//!     A new section header is spelled the same way, by `store_create_section()`.
//!   * Everything outside the replaced span is copied verbatim: comments, a
//!     `[section] key = value` written on one line, an entry indented with spaces rather
//!     than a tab. Only the matched entry's own bytes are replaced, and the whitespace
//!     leading it on the same line is swallowed.
//!   * Unsetting the last variable of a section drops the now-empty header too, but only
//!     when no comment could belong to it (`maybe_remove_section()`, config.c:2811-2884).
//!   * `--replace-all` writes the replacement where the *first* match was, then deletes
//!     the rest, so the surviving order is not "everything else, then the new value".
//!   * A section rename is line-wise: the header line is rewritten and a variable that
//!     shared that line moves to the next one with a tab (config.c:3448-3476).
//!   * The file's permissions survive, because git chmods its lock to `st.st_mode & 07777`
//!     before renaming it over the original (config.c:3155-3159).
#![cfg(unix)]

use std::path::PathBuf;
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-cfg-splice-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn path(&self) -> PathBuf {
        self.root.join("conf")
    }

    fn write(&self, contents: &str) {
        std::fs::write(self.path(), contents).unwrap();
    }

    fn read(&self) -> String {
        std::fs::read_to_string(self.path()).unwrap()
    }

    /// `git config --file=<conf> …`, returning stdout, stderr and the exit status.
    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let file = self.path();
        let mut c = Command::new(BIN);
        c.arg("config")
            .arg("--file")
            .arg(&file)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        let out = c.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn ok(&self, args: &[&str]) {
        let (out, err, code) = self.run(args);
        assert_eq!((out.as_str(), code), ("", 0), "`config {args:?}` stderr: {err}");
    }
}

/// A brand-new variable keeps the caller's spelling, and so does the section header it
/// creates — `write_pair()`/`store_create_section()` both read the raw `key`.
#[test]
fn new_key_and_section_keep_the_callers_case() {
    let f = Fixture::new("case");
    f.write("[section]\n\tpenguin = little blue\n");

    f.ok(&["Section.Movie", "BadPhysics"]);
    f.ok(&["Sections.WhatEver", "Second"]);
    f.ok(&["SECTION.UPPERCASE", "true"]);

    assert_eq!(
        f.read(),
        "[section]\n\
         \tpenguin = little blue\n\
         \tMovie = BadPhysics\n\
         \tUPPERCASE = true\n\
         [Sections]\n\
         \tWhatEver = Second\n"
    );
}

/// An extended subsection is compared byte for byte, so a differently-cased one is a
/// *different* section and gains its own header — while the old-fashioned `[V.A]` form is
/// matched case-insensitively and is written into in place.
#[test]
fn subsection_case_decides_between_reuse_and_a_new_header() {
    let f = Fixture::new("subsec");
    f.write("[V \"A\"]\n\tR = v1\n[d \"e\"]\n\tf = v1\n");

    // Same subsection, different key case: the existing line is replaced, spelled `r`.
    f.ok(&["v.A.r", "v2"]);
    // Different subsection case: no match, so a new `[d "E"]` section is appended.
    f.ok(&["d.E.f", "v2"]);

    assert_eq!(
        f.read(),
        "[V \"A\"]\n\tr = v2\n[d \"e\"]\n\tf = v1\n[d \"E\"]\n\tf = v2\n"
    );
}

/// The bytes around the replaced entry are copied through untouched: the odd header
/// comment, the `[nextSection] noNewline = ouch` one-liner and the blank/comment lines
/// between them all survive a set.
#[test]
fn surrounding_bytes_survive_a_set() {
    let f = Fixture::new("verbatim");
    f.write(
        "[beta] ; silly comment # another comment\n\
         noIndent= sillyValue ; 'nother silly comment\n\
         \n\
         # empty line\n\
         \t\t; comment\n\
         \thaha = beta\n\
         [nextSection] noNewline = ouch\n",
    );

    f.ok(&["beta.haha", "alpha"]);

    assert_eq!(
        f.read(),
        "[beta] ; silly comment # another comment\n\
         noIndent= sillyValue ; 'nother silly comment\n\
         \n\
         # empty line\n\
         \t\t; comment\n\
         \thaha = alpha\n\
         [nextSection] noNewline = ouch\n"
    );

    // Setting the one-liner's variable splits it onto its own line — and only it.
    f.ok(&["nextsection.nonewline", "wow"]);
    assert_eq!(
        f.read().lines().last().unwrap(),
        "\tnonewline = wow"
    );
    assert!(f.read().contains("[nextSection]\n"), "{}", f.read());
}

/// `--replace-all` does not invent a newline where the file had none: `[abc]key` on one
/// line keeps `key` as a valueless entry of `[abc]` and the replacement lands at the
/// first match.
#[test]
fn replace_all_does_not_invent_newlines() {
    let f = Fixture::new("noinvent");
    f.write("[abc]key\n\tkeepSection\n[xyz]\n\tkey = 1\n[abc]\n\tkey = a\n");

    f.ok(&["--replace-all", "abc.key", "b"]);

    assert_eq!(
        f.read(),
        "[abc]\n\tkeepSection\n[xyz]\n\tkey = 1\n[abc]\n\tkey = b\n"
    );
}

/// The replacement goes where the first match was; later matches are deleted, so the
/// value that did not match keeps its position *after* it.
#[test]
fn replace_all_writes_at_the_first_match() {
    let f = Fixture::new("order");
    f.write("");
    for v in ["one", "two", "three"] {
        f.ok(&["--add", "abc.key", v]);
    }

    f.ok(&["--replace-all", "abc.key", "four", "o+"]);

    let (out, _, code) = f.run(&["--get-all", "abc.key"]);
    assert_eq!((out.as_str(), code), ("four\nthree\n", 0));
}

/// Unsetting the last variable of a section removes the header too — unless a comment
/// sits where it could be about the section, in which case the header stays.
#[test]
fn unsetting_the_last_key_removes_a_bare_section_only() {
    let f = Fixture::new("unset");

    f.write("[section]\nkey = value\n[next-section]\n");
    f.ok(&["--unset", "section.key"]);
    assert_eq!(f.read(), "[next-section]\n");

    f.write(
        "# a generic comment\n\
         # a comment about this \"section\" section.\n\
         [section]\n\
         # an intervening line\n\
         \n\
         key = value\n\
         # be careful when you update the above\n",
    );
    f.ok(&["--unset", "section.key"]);
    assert_eq!(
        f.read(),
        "# a generic comment\n\
         # a comment about this \"section\" section.\n\
         [section]\n\
         # an intervening line\n\
         \n\
         # be careful when you update the above\n"
    );

    f.write("[section]\nkey = value1\nkey = value2\n");
    f.ok(&["--unset-all", "section.key"]);
    assert_eq!(f.read(), "");
}

/// A rename rewrites the header line in place. `[branch.eins]` and `[branch "eins"]` both
/// match `branch.eins`, and a variable that shared the header's line moves to the next
/// one, indented with a tab.
#[test]
fn rename_section_is_line_wise() {
    let f = Fixture::new("rename");
    f.write(
        "# Hallo\n\
         \t#Bello\n\
         [branch \"eins\"]\n\
         \tx = 1\n\
         [branch.eins]\n\
         \ty = 1\n\
         \t[branch \"1 234 blabl/a\"]\n\
         weird\n",
    );

    f.ok(&["--rename-section", "branch.eins", "branch.zwei"]);
    assert_eq!(
        f.read(),
        "# Hallo\n\
         \t#Bello\n\
         [branch \"zwei\"]\n\
         \tx = 1\n\
         [branch \"zwei\"]\n\
         \ty = 1\n\
         \t[branch \"1 234 blabl/a\"]\n\
         weird\n"
    );

    f.write("[branch \"vier\"] z = 1\n");
    f.ok(&["--rename-section", "branch.vier", "branch.zwei"]);
    assert_eq!(f.read(), "[branch \"zwei\"]\n\tz = 1\n");
}

/// A rename that matches nothing is `die(_("no such section: %s"))` — exit 128 — and the
/// file is left alone.
#[test]
fn rename_of_an_absent_section_is_fatal() {
    let f = Fixture::new("norename");
    f.write("[a]\n\tb = c\n");

    let (out, err, code) = f.run(&["--rename-section", "branch.nope", "branch.drei"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(err.contains("no such section: branch.nope"), "{err}");
    assert_eq!(f.read(), "[a]\n\tb = c\n");
}

/// A `git config` write does not widen a config the user tightened: the lock file is
/// chmod'ed to the original's mode before it replaces it.
#[test]
fn a_write_preserves_the_files_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::new("perm");
    f.write("[imap]\n\tpass = x\n");
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

    f.ok(&["imap.pass", "Hunter2"]);
    let mode = std::fs::metadata(f.path()).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o600, "set widened the mode");

    f.ok(&["--rename-section", "imap", "pop"]);
    let mode = std::fs::metadata(f.path()).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o600, "rename widened the mode");
}

/// `git config --file=<symlink>` edits the file the link points at — git's lock resolves
/// the symlink first — and the link itself is left in place.
#[test]
fn a_write_follows_a_symlinked_config() {
    let f = Fixture::new("symlink");
    let real = f.root.join("real.conf");
    std::os::unix::fs::symlink("real.conf", f.path()).unwrap();

    f.ok(&["test.frotz", "nitfol"]);
    f.ok(&["test.xyzzy", "rezrov"]);

    assert!(f.path().symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "[test]\n\tfrotz = nitfol\n\txyzzy = rezrov\n"
    );
}

/// `--comment` is a trailer on the line as it is written, and `--replace-all` rewrites
/// the line, so the comment lands on it.
#[test]
fn a_comment_rides_the_line_that_is_written() {
    let f = Fixture::new("comment");
    f.write("[section]\n\tpenguin = little blue\n");

    f.ok(&["--replace-all", "--comment=Pygoscelis papua", "section.penguin", "gentoo"]);
    f.ok(&["--comment=find fish", "section.disposition", "peckish"]);
    f.ok(&["--comment=#abc", "section.foo", "bar"]);

    assert_eq!(
        f.read(),
        "[section]\n\
         \tpenguin = gentoo # Pygoscelis papua\n\
         \tdisposition = peckish # find fish\n\
         \tfoo = bar #abc\n"
    );
}
