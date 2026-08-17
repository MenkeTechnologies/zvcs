//! The five tree-ish readers handed a well-formed object id the repository does
//! not have: `check-attr --source`, `ls-tree`, `read-tree`, `archive`,
//! `ls-files --with-tree`.
//!
//! `get_oid_basic()` (`object-name.c`) opens with
//!
//! ```c
//! if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid))
//!         return 0;
//! ```
//!
//! so a name of exactly `hexsz` hex digits *is* the object id — decoded and
//! returned before the object database is consulted. Every command here takes a
//! tree-ish off argv, so every one of them sees such a name **resolve**, and
//! then fails (or does not fail at all) further downstream, in code that wanted
//! the object's bytes:
//!
//! | command                         | git 2.55.0                                       | zvcs before                                |
//! |---------------------------------|--------------------------------------------------|--------------------------------------------|
//! | `check-attr --source=<oid> …`   | exit **0**, `f.txt: text: unspecified`           | `fatal: <oid>: not a valid tree-ish source`|
//! | `ls-tree <oid>`                 | `fatal: not a tree object`                       | `fatal: Not a valid object name <oid>`     |
//! | `read-tree <oid>`               | `fatal: failed to unpack tree object <oid>`      | `fatal: Not a valid object name <oid>`     |
//! | `archive <oid>`                 | `fatal: not a tree object: <oid>`                | `fatal: not a valid object name: <oid>`    |
//! | `ls-files --with-tree=<oid>`    | `fatal: bad tree-ish <oid>`                      | `fatal: tree-ish <oid> not found.`         |
//!
//! Five different wordings, from five different C sites, none of them a variant
//! of another:
//!
//! * `builtin/check-attr.c` dies only when `repo_get_oid_tree()` fails. The
//!   `GET_OID_TREE` flag steers short-name disambiguation; it does not require a
//!   tree. `set_git_attr_source()` then keeps whatever id came back, and
//!   `read_attr_from_blob()` (`attr.c`) reads `.gitattributes` out of it with
//!   `get_tree_entry()`, which misses quietly — so the command *succeeds* and
//!   reports every attribute as unspecified, without falling back to the working
//!   tree. This is the one case where the bug turned a stock exit 0 into a hard
//!   failure, which is what breaks a script that passes a commit id to
//!   `--source` against a repository missing that object.
//! * `builtin/ls-tree.c:412` names the *name*; its `repo_parse_tree_indirect()`
//!   failure at line 429 names nothing at all.
//! * `builtin/read-tree.c:213` vs its `list_tree()` failure at line 215, which
//!   names the spelling from argv.
//! * `archive.c:509` (`parse_treeish_arg`) vs line 524, which names the
//!   *resolved* id — `oid_to_hex(&oid)`, so an upper-case argv spelling comes
//!   back lower-cased.
//! * `read-cache.c:3808` (`overlay_tree_on_index`) vs line 3811.
//!
//! Resolving with gitoxide's `rev_parse_single()` alone reaches none of the
//! second column, because it goes through the odb and so rejects the name
//! outright.
//!
//! Each command pins the same three properties, since the fix is a *rule* about
//! name shape and any single assertion can be satisfied by the wrong code:
//!
//! * the absent-but-well-formed id gets the downstream, "object missing"
//!   outcome;
//! * a name that resolves to nothing keeps the "no such name" outcome — the two
//!   must stay distinct, in both directions;
//! * the rule is length-exact and case-insensitive: 39 and 41 hex digits fall
//!   through to the ordinary parser, while upper-case 40 does not.
//!
//! A resolvable non-tree (`HEAD:f.txt`, a blob) is checked alongside, because it
//! reaches the same downstream site as the absent id and so proves the split was
//! made at git's boundary rather than at "is this 40 hex digits".
//!
//! Expectations are stock git 2.55.0's, captured with the parity harness's
//! environment (fixed identity and date, no global or system config, `LC_ALL=C`,
//! `TZ=UTC`).
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Well-formed SHA-1 hex, and not an object any fixture here can contain.
const ABSENT: &str = "0123456789012345678901234567890123456789";

/// The same shape in upper case. `get_oid_hex()` is built on `hexval()`, which
/// is case-insensitive, so this must take exactly the same path as [`ABSENT`].
const ABSENT_UPPER: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

/// One hex digit short of `hexsz`: the first branch does not apply, and the name
/// is handled as an (unmatchable) abbreviation instead.
const SHORT_HEX: &str = "012345678901234567890123456789012345678";

/// One hex digit long. Same reasoning, from the other side of the boundary.
const LONG_HEX: &str = "01234567890123456789012345678901234567890";

/// The control: not hex, not a ref, resolves to nothing at all. Every assertion
/// about an absent id is paired with one about this, because the bug being
/// pinned is precisely that the two were treated alike.
const UNRESOLVABLE: &str = "nosuchthing";

/// Resolvable, but not a tree: it must share the absent id's outcome, not the
/// control's.
const BLOB: &str = "HEAD:f.txt";

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// One commit holding a `.gitattributes` that sets `text` on `*.txt`, so a
    /// wrong fallback to the working tree during a `--source` run is visible as
    /// `set` where git says `unspecified`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-objname-treeish-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.repo.join(".gitattributes"), "*.txt text\n").unwrap();
        std::fs::write(f.repo.join("f.txt"), "hello\n").unwrap();
        std::fs::create_dir_all(f.repo.join("sub")).unwrap();
        std::fs::write(f.repo.join("sub/g.txt"), "deep\n").unwrap();
        f.ok(&["add", "-A"]);
        f.ok(&["commit", "-q", "-m", "c1"]);
        f
    }

    fn run(&self, args: &[&str]) -> (Vec<u8>, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "@1112911993 +0000")
            .env("GIT_COMMITTER_DATE", "@1112911993 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_ATTR_SOURCE")
            .output()
            .unwrap();
        (
            out.stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    /// stdout as text, for the commands that produce it.
    fn run_text(&self, args: &[&str]) -> (String, String, i32) {
        let (out, err, code) = self.run(args);
        (String::from_utf8_lossy(&out).into_owned(), err, code)
    }

    fn ok(&self, args: &[&str]) {
        let (out, err, code) = self.run_text(args);
        assert_eq!(code, 0, "setup `git {args:?}` failed: {out}{err}");
    }

    /// Assert a run failed with exactly `want` on stderr, nothing on stdout, and
    /// git's 128.
    fn dies(&self, args: &[&str], want: &str) {
        let (out, err, code) = self.run_text(args);
        assert_eq!(err, want, "git {args:?}");
        assert_eq!(out, "", "git {args:?} wrote to stdout");
        assert_eq!(code, 128, "git {args:?}");
    }
}

/// `--source` naming an object that is not there is *not* an error: git keeps
/// the id as the attribute source and every lookup in it misses.
///
/// The fixture's committed `.gitattributes` sets `text`, so `unspecified` here
/// also pins that git does not fall back to the working tree — a fallback would
/// report `set` and still exit 0, passing a weaker assertion.
#[test]
fn check_attr_source_accepts_an_absent_id_and_reports_unspecified() {
    let f = Fixture::new("attr-absent");

    for spec in [ABSENT, ABSENT_UPPER, BLOB] {
        let (out, err, code) =
            f.run_text(&["check-attr", &format!("--source={spec}"), "text", "f.txt"]);
        assert_eq!(err, "", "check-attr --source={spec}");
        assert_eq!(out, "f.txt: text: unspecified\n", "check-attr --source={spec}");
        assert_eq!(code, 0, "check-attr --source={spec}");
    }

    // `-a` finds nothing at all in an unreadable source, and still succeeds.
    let (out, err, code) = f.run_text(&["check-attr", &format!("--source={ABSENT}"), "-a", "f.txt"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));

    // A real tree is unaffected: the attribute is found and reported.
    let (out, _, code) = f.run_text(&["check-attr", "--source=HEAD", "text", "f.txt"]);
    assert_eq!(out, "f.txt: text: set\n");
    assert_eq!(code, 0);
}

/// The control, and the length boundary: these must keep dying, or the fix
/// would read as "never validate `--source`".
#[test]
fn check_attr_source_still_dies_for_a_name_that_resolves_to_nothing() {
    let f = Fixture::new("attr-control");

    for spec in [UNRESOLVABLE, SHORT_HEX, LONG_HEX] {
        f.dies(
            &["check-attr", &format!("--source={spec}"), "text", "f.txt"],
            &format!("fatal: {spec}: not a valid tree-ish source\n"),
        );
    }
}

/// `GIT_ATTR_SOURCE` goes through the same `repo_get_oid_tree()`, so it inherits
/// the rule — and `compute_default_attr_source()` dies with its own message when
/// the name does not resolve at all.
#[test]
fn check_attr_env_source_follows_the_same_rule() {
    let f = Fixture::new("attr-env");

    let run = |val: &str| {
        let out = Command::new(BIN)
            .args(["check-attr", "text", "f.txt"])
            .current_dir(&f.repo)
            .env("HOME", &f.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_ATTR_SOURCE", val)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    };

    for val in [ABSENT, BLOB] {
        let (out, err, code) = run(val);
        assert_eq!(err, "", "GIT_ATTR_SOURCE={val}");
        assert_eq!(out, "f.txt: text: unspecified\n", "GIT_ATTR_SOURCE={val}");
        assert_eq!(code, 0, "GIT_ATTR_SOURCE={val}");
    }

    let (out, err, code) = run("HEAD");
    assert_eq!((out.as_str(), err.as_str(), code), ("f.txt: text: set\n", "", 0));

    let (out, err, code) = run(UNRESOLVABLE);
    assert_eq!(out, "");
    assert_eq!(err, "fatal: bad --attr-source or GIT_ATTR_SOURCE\n");
    assert_eq!(code, 128);
}

/// `cmd_ls_tree`: `repo_get_oid_with_flags()` accepts the id,
/// `repo_parse_tree_indirect()` then has nothing to parse — and its message
/// names no object at all, which is what separates the two sites.
#[test]
fn ls_tree_reports_the_absent_object_as_not_a_tree() {
    let f = Fixture::new("ls-tree");

    for spec in [ABSENT, ABSENT_UPPER, BLOB] {
        f.dies(&["ls-tree", spec], "fatal: not a tree object\n");
        // `-r` walks recursively but resolves identically.
        f.dies(&["ls-tree", "-r", spec], "fatal: not a tree object\n");
    }

    for spec in [UNRESOLVABLE, SHORT_HEX, LONG_HEX] {
        f.dies(
            &["ls-tree", spec],
            &format!("fatal: Not a valid object name {spec}\n"),
        );
    }

    // A real tree still lists.
    let (out, _, code) = f.run_text(&["ls-tree", "HEAD"]);
    assert_eq!(code, 0);
    assert!(out.contains("\tf.txt\n"), "ls-tree HEAD: {out}");
}

/// `cmd_read_tree`: `repo_get_oid()` accepts the id, `list_tree()` fails, and
/// its message names the spelling from argv — upper case stays upper case.
#[test]
fn read_tree_reports_the_absent_object_as_a_failed_unpack() {
    let f = Fixture::new("read-tree");

    for spec in [ABSENT, ABSENT_UPPER, BLOB] {
        f.dies(
            &["read-tree", spec],
            &format!("fatal: failed to unpack tree object {spec}\n"),
        );
    }

    for spec in [UNRESOLVABLE, SHORT_HEX, LONG_HEX] {
        f.dies(
            &["read-tree", spec],
            &format!("fatal: Not a valid object name {spec}\n"),
        );
    }

    // The read loop resolves each tree-ish in turn, so a good one first does not
    // change what the bad one reports.
    f.dies(
        &["read-tree", "HEAD", ABSENT],
        &format!("fatal: failed to unpack tree object {ABSENT}\n"),
    );

    // A real tree still reads, and the index it produced still lists.
    f.ok(&["read-tree", "HEAD"]);
    let (out, _, code) = f.run_text(&["ls-files"]);
    assert_eq!(code, 0);
    assert!(out.contains("f.txt\n"), "ls-files after read-tree: {out}");
}

/// `parse_treeish_arg()` (`archive.c`): the "not a tree object" message names
/// `oid_to_hex(&oid)`, the *resolved* id — so the upper-case spelling comes back
/// lower-cased, and the blob is named by its own id rather than by `HEAD:f.txt`.
#[test]
fn archive_reports_the_absent_object_as_not_a_tree() {
    let f = Fixture::new("archive");

    let blob_id = f.run_text(&["rev-parse", BLOB]).0.trim().to_string();
    assert_eq!(blob_id.len(), 40, "rev-parse {BLOB} produced {blob_id:?}");

    for (spec, named) in [
        (ABSENT, ABSENT.to_string()),
        (ABSENT_UPPER, ABSENT_UPPER.to_ascii_lowercase()),
        (BLOB, blob_id),
    ] {
        f.dies(
            &["archive", spec],
            &format!("fatal: not a tree object: {named}\n"),
        );
        f.dies(
            &["archive", "--format=tar", spec],
            &format!("fatal: not a tree object: {named}\n"),
        );
    }

    for spec in [UNRESOLVABLE, SHORT_HEX, LONG_HEX] {
        f.dies(
            &["archive", spec],
            &format!("fatal: not a valid object name: {spec}\n"),
        );
    }

    // A real tree still archives, and to a non-empty stream — an implementation
    // that failed silently would satisfy only the assertions above.
    let (out, err, code) = f.run(&["archive", "--format=tar", "HEAD"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert!(out.len() > 512, "archive HEAD produced {} bytes", out.len());
    assert!(
        out.windows(5).any(|w| w == b"f.txt"),
        "archive HEAD does not mention f.txt"
    );
}

/// `overlay_tree_on_index()` (`read-cache.c`): "tree-ish %s not found." is only
/// for a name that does not resolve; a resolved id with no tree behind it is
/// "bad tree-ish %s", which names the argv spelling.
#[test]
fn ls_files_with_tree_reports_the_absent_object_as_a_bad_tree_ish() {
    let f = Fixture::new("ls-files");

    for spec in [ABSENT, ABSENT_UPPER, BLOB] {
        f.dies(
            &["ls-files", &format!("--with-tree={spec}")],
            &format!("fatal: bad tree-ish {spec}\n"),
        );
    }

    for spec in [UNRESOLVABLE, SHORT_HEX, LONG_HEX] {
        f.dies(
            &["ls-files", &format!("--with-tree={spec}")],
            &format!("fatal: tree-ish {spec} not found.\n"),
        );
    }

    // A real tree still overlays.
    let (out, _, code) = f.run_text(&["ls-files", "--with-tree=HEAD"]);
    assert_eq!(code, 0);
    assert_eq!(out, ".gitattributes\nf.txt\nsub/g.txt\n");
}
