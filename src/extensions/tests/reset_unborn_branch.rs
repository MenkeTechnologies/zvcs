//! `git reset` on an unborn branch — `cmd_reset()`'s third resolution arm.
//!
//! Before it resolves anything, `cmd_reset()` asks whether there is a HEAD at
//! all (builtin/reset.c:405-409):
//!
//! ```c
//! unborn = !strcmp(rev, "HEAD") && repo_get_oid(the_repository, "HEAD", &unused);
//! if (unborn) {
//!         /* reset on unborn branch: treat as reset to empty tree */
//!         oidcpy(&oid, the_repository->hash_algo->empty_tree);
//! } else if (…)
//! ```
//!
//! and the answer changes three things at once, which is why this needs its own
//! file rather than an extra case next to the object-name tests:
//!
//!   * the **target** is the empty tree, so a `--mixed` empties the index and a
//!     `--hard` also empties the worktree — the reset *succeeds*, it does not
//!     refuse;
//!   * `reset_refs()` is skipped (`if (!pathspec.nr && !unborn)`, reset.c:534),
//!     so neither HEAD nor ORIG_HEAD is written and `--hard` prints no
//!     `HEAD is now at` line, while `remove_branch_state()` — outside that guard
//!     — still runs;
//!   * `reset_index()`'s KEEP arm wants HEAD's tree as the first side of its
//!     two-tree merge and reports `You do not have a valid HEAD.` when there is
//!     none (reset.c:97-100), so `--keep` is the one mode that fails here, while
//!     `--merge` takes no such side and fails only on the worktree state.
//!
//! The trap the arm exists to avoid: `HEAD` does not resolve on an unborn
//! branch, so an implementation that resolves first and reports second turns
//! every one of these into `fatal: ambiguous argument 'HEAD'`. That message is
//! nonetheless correct for one shape — a bare `git reset HEAD` — because
//! `parse_args()` decides the single positional is a filename before `unborn` is
//! ever computed, and `verify_filename()` dies there. Both directions are pinned
//! below; a fix that only removes the message breaks the second.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository, comparing stdout, stderr, exit status and the resulting
//! index/worktree/refs separately.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The empty-blob id every `-N` stub carries.
const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

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
    /// `git init` with `f.txt` and `g.txt` staged and **no commit**, so HEAD
    /// points at a branch that does not exist yet.
    ///
    /// `dirty` additionally rewrites `f.txt` after staging it. It is a parameter
    /// because the two-tree modes branch on exactly that: `--merge` aborts on a
    /// worktree that no longer matches the index and succeeds otherwise, so a
    /// fixture that is only ever dirty cannot tell "refuses correctly" from
    /// "refuses always".
    fn new(tag: &str, dirty: bool) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-reset-unborn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.repo.join("f.txt"), "one\n").unwrap();
        std::fs::write(f.repo.join("g.txt"), "gg\n").unwrap();
        f.ok(&["add", "f.txt", "g.txt"]);
        if dirty {
            std::fs::write(f.repo.join("f.txt"), "dirty\n").unwrap();
        }
        assert_ne!(
            f.run(&["rev-parse", "--verify", "HEAD"]).2,
            0,
            "the fixture must start on an unborn branch"
        );
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    fn ok(&self, args: &[&str]) {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "setup `git {args:?}` failed: {out}{err}");
    }

    /// `path <id>` per staged entry, so both the set of entries and the ids they
    /// carry are compared.
    fn index(&self) -> Vec<String> {
        self.run(&["ls-files", "--stage"])
            .0
            .lines()
            .map(|l| {
                let mut it = l.split_whitespace();
                let id = it.nth(1).unwrap_or_default().to_string();
                let path = l.rsplit('\t').next().unwrap_or_default().to_string();
                format!("{path} {id}")
            })
            .collect()
    }

    fn worktree(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.repo)
            .unwrap()
            .filter_map(|e| {
                let e = e.unwrap();
                let n = e.file_name().to_string_lossy().into_owned();
                (n != ".git").then_some(n)
            })
            .collect();
        names.sort();
        names
    }

    /// Still unborn: HEAD names a branch that does not exist, and nothing wrote
    /// an ORIG_HEAD either.
    fn assert_still_unborn(&self) {
        assert_eq!(
            std::fs::read_to_string(self.repo.join(".git/HEAD")).unwrap(),
            "ref: refs/heads/main\n"
        );
        assert_ne!(self.run(&["rev-parse", "--verify", "HEAD"]).2, 0, "HEAD became resolvable");
        assert_ne!(
            self.run(&["rev-parse", "--verify", "ORIG_HEAD"]).2,
            0,
            "ORIG_HEAD was written"
        );
    }
}

/// The default `--mixed` (and its explicit spellings): the index is loaded from
/// the empty tree, which drops every entry. The worktree is not touched, so the
/// two files come back as untracked.
///
/// Nothing is printed: `refresh_index()`'s `Unstaged changes after reset:` header
/// is lazy and the emptied index has no entry left to disagree with the worktree.
#[test]
fn a_mixed_reset_empties_the_index_and_succeeds() {
    for args in [
        vec!["reset"],
        vec!["reset", "--mixed"],
        vec!["reset", "-q"],
        vec!["reset", "--no-refresh"],
    ] {
        let f = Fixture::new("mixed", true);
        let (out, err, code) = f.run(&args);
        assert_eq!(err, "", "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 0, "{args:?}");
        assert!(f.index().is_empty(), "{args:?}: index not emptied: {:?}", f.index());
        assert_eq!(f.worktree(), ["f.txt", "g.txt"], "{args:?}: the worktree was touched");
        f.assert_still_unborn();
    }
}

/// `--soft` reaches neither `read_from_tree()` nor `reset_index()`, and the ref
/// move it would otherwise do is the part `unborn` skips — so it is a complete
/// no-op that still exits 0.
#[test]
fn a_soft_reset_changes_nothing_and_succeeds() {
    let f = Fixture::new("soft", true);
    let before = f.index();
    assert_eq!(before.len(), 2, "fixture must have both paths staged");

    let (out, err, code) = f.run(&["reset", "--soft"]);
    assert_eq!(err, "");
    assert_eq!(out, "");
    assert_eq!(code, 0);
    assert_eq!(f.index(), before);
    assert_eq!(f.worktree(), ["f.txt", "g.txt"]);
    f.assert_still_unborn();
}

/// `--hard` runs `reset_index()` with `UNPACK_RESET_OVERWRITE_UNTRACKED` toward
/// the empty tree, so the staged files leave the worktree as well as the index —
/// including the locally modified one, which the mode is defined to discard.
///
/// `print_new_head_line()` sits inside the same `!unborn` guard as
/// `reset_refs()`, so the whole thing happens in silence.
#[test]
fn a_hard_reset_empties_the_index_and_the_worktree() {
    for dirty in [false, true] {
        let f = Fixture::new(if dirty { "hard-dirty" } else { "hard-clean" }, dirty);
        let (out, err, code) = f.run(&["reset", "--hard"]);
        assert_eq!(err, "", "dirty={dirty}");
        assert_eq!(out, "", "dirty={dirty}: no `HEAD is now at` line without a HEAD");
        assert_eq!(code, 0, "dirty={dirty}");
        assert!(f.index().is_empty(), "dirty={dirty}: {:?}", f.index());
        assert!(f.worktree().is_empty(), "dirty={dirty}: {:?}", f.worktree());
        f.assert_still_unborn();
    }
}

/// `--keep` is the one mode an unborn branch cannot satisfy: its two-tree merge
/// needs HEAD's tree, so `reset_index()` returns the `error()` and `cmd_reset()`
/// dies with `rev` — the spec as typed, not an object id, which is the only
/// thing it could name here anyway.
#[test]
fn a_keep_reset_reports_the_missing_head() {
    for dirty in [false, true] {
        let f = Fixture::new(if dirty { "keep-dirty" } else { "keep-clean" }, dirty);
        let before = f.index();
        let (out, err, code) = f.run(&["reset", "--keep"]);
        assert_eq!(
            err,
            "error: You do not have a valid HEAD.\n\
             fatal: Could not reset index file to revision 'HEAD'.\n",
            "dirty={dirty}"
        );
        assert_eq!(out, "", "dirty={dirty}");
        assert_eq!(code, 128, "dirty={dirty}");
        assert_eq!(f.index(), before, "dirty={dirty}: a refusal must not touch the index");
        assert_eq!(f.worktree(), ["f.txt", "g.txt"], "dirty={dirty}");
        f.assert_still_unborn();
    }
}

/// `--merge` needs no HEAD side, so it reaches `unpack_trees()` and is decided by
/// the worktree alone: a file whose worktree copy no longer matches the index is
/// `oneway_merge()`'s `verify_uptodate()` failure, and a clean one is simply
/// removed.
///
/// The two halves share every other input, which is what makes this a test of the
/// uptodate check rather than of the mode.
#[test]
fn a_merge_reset_aborts_only_on_a_dirty_worktree() {
    let dirty = Fixture::new("merge-dirty", true);
    let before = dirty.index();
    let (out, err, code) = dirty.run(&["reset", "--merge"]);
    assert_eq!(
        err,
        "error: Entry 'f.txt' not uptodate. Cannot merge.\n\
         fatal: Could not reset index file to revision 'HEAD'.\n"
    );
    assert_eq!(out, "");
    assert_eq!(code, 128);
    assert_eq!(dirty.index(), before, "an aborted merge must not touch the index");
    assert_eq!(dirty.worktree(), ["f.txt", "g.txt"]);
    dirty.assert_still_unborn();

    let clean = Fixture::new("merge-clean", false);
    let (out, err, code) = clean.run(&["reset", "--merge"]);
    assert_eq!(err, "");
    assert_eq!(out, "");
    assert_eq!(code, 0);
    assert!(clean.index().is_empty(), "{:?}", clean.index());
    assert!(clean.worktree().is_empty(), "{:?}", clean.worktree());
    clean.assert_still_unborn();
}

/// The pathspec form takes the same empty tree, so a named path is simply dropped
/// from the index while the rest stays. All three spellings are checked because
/// `parse_args()` reaches them through different arms — `--` with a rev, `--`
/// without one, and a bare filename.
#[test]
fn the_pathspec_form_drops_the_named_path() {
    for args in [
        vec!["reset", "HEAD", "--", "f.txt"],
        vec!["reset", "--", "f.txt"],
        vec!["reset", "f.txt"],
    ] {
        let f = Fixture::new("paths", true);
        let (out, err, code) = f.run(&args);
        assert_eq!(err, "", "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 0, "{args:?}");
        assert_eq!(f.index().len(), 1, "{args:?}: {:?}", f.index());
        assert!(f.index()[0].starts_with("g.txt "), "{args:?}: {:?}", f.index());
        assert_eq!(f.worktree(), ["f.txt", "g.txt"], "{args:?}");
        f.assert_still_unborn();
    }
}

/// `reset HEAD --` has a rev and no pathspec, so it is the whole-tree form with
/// `rev` explicitly spelled — `unborn` is `!strcmp(rev, "HEAD")` and does not
/// care whether the word was typed or defaulted.
#[test]
fn an_explicit_head_before_a_dashdash_is_still_the_unborn_arm() {
    let f = Fixture::new("explicit-head", true);
    let (out, err, code) = f.run(&["reset", "HEAD", "--"]);
    assert_eq!(err, "");
    assert_eq!(out, "");
    assert_eq!(code, 0);
    assert!(f.index().is_empty(), "{:?}", f.index());
    f.assert_still_unborn();
}

/// The other direction, and the reason the ambiguity message cannot simply be
/// deleted: a lone `HEAD` is tested by `parse_args()` first
/// (`!argv[1] && !repo_get_oid_committish(argv[0], &unused)`), that lookup fails
/// on an unborn branch, and the token falls through to `verify_filename()` —
/// which dies because there is no file called `HEAD`. `unborn` is never reached.
///
/// A name that resolves to nothing keeps the same message with or without a HEAD,
/// so it is checked alongside as the control.
#[test]
fn a_lone_head_is_still_an_ambiguous_argument() {
    let f = Fixture::new("lone-head", true);
    let want = |arg: &str| {
        format!(
            "fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
             Use '--' to separate paths from revisions, like this:\n\
             'git <command> [<revision>...] -- [<file>...]'\n"
        )
    };

    for args in [vec!["reset", "HEAD"], vec!["reset", "--hard", "HEAD"]] {
        let (out, err, code) = f.run(&args);
        assert_eq!(err, want("HEAD"), "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 128, "{args:?}");
    }

    let (_, err, code) = f.run(&["reset", "nosuchrev"]);
    assert_eq!(err, want("nosuchrev"));
    assert_eq!(code, 128);

    assert_eq!(f.index().len(), 2, "nothing was reset");
    f.assert_still_unborn();
}

/// `-N` rides the unborn arm too: every path the empty tree does not have — which
/// is all of them — comes back as `update_index_from_diff()`'s intent-to-add
/// stub, and the header is printed because those stubs never match the worktree.
///
/// The stub's empty blob is written to the object database, not merely named
/// (`set_object_name_for_intent_to_add_entry()`, read-cache.c:704), which is
/// asserted here because an unborn repository has no other way to acquire it.
#[test]
fn intent_to_add_restages_every_path_as_a_stub() {
    let f = Fixture::new("ita", true);
    assert_ne!(
        f.run(&["cat-file", "-e", EMPTY_BLOB]).2,
        0,
        "the fixture must not already hold the empty blob"
    );

    let (out, err, code) = f.run(&["reset", "-N"]);
    assert_eq!(err, "");
    assert_eq!(out, "Unstaged changes after reset:\nA\tf.txt\nA\tg.txt\n");
    assert_eq!(code, 0);
    assert_eq!(
        f.index(),
        [format!("f.txt {EMPTY_BLOB}"), format!("g.txt {EMPTY_BLOB}")]
    );
    assert_eq!(
        f.run(&["cat-file", "-e", EMPTY_BLOB]).2,
        0,
        "the stub's blob was named but never written"
    );
    f.assert_still_unborn();
}
