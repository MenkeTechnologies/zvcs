//! `git fast-export` revision-flag and emission-order shapes the parity
//! grammars cannot reach.
//!
//! Three unrelated pieces of git behaviour, each of which used to diverge:
//!
//! * **The `--not` XOR.** `--not` does not *set* negation, it toggles a flag word
//!   (`*flags ^= UNINTERESTING | BOTTOM`, `revision.c:2907`) that every later
//!   argument is read through, and each argument XORs its own contribution into
//!   it: a leading `^` supplies `local_flags = UNINTERESTING | BOTTOM` and the
//!   object is filed under `flags ^ local_flags` (`revision.c:2210-2213`,
//!   `2229`, `2234`). So `--not ^main` is *positive*, a second `--not` cancels
//!   the first, `--all`/`--branches`/`--tags`/`--remotes` are negative under it
//!   (they pass `*flags` straight to `handle_refs`, `revision.c:2808-2841`), and
//!   `A..B` swaps ends (`revision.c:2083-2086`).
//!
//! * **`M`-line order.** `show_filemodify` re-sorts the whole diff queue with
//!   `depth_first` (`fast-export.c:353-381`, `445`) before rendering it: names
//!   compare over their common length and the **longer** one wins a tie, so
//!   everything below a directory precedes the entry that replaces it — and
//!   prefix-related siblings such as `C2` and `C.a` precede plain `C`. The sort
//!   runs *after* the blob export, so blob marks still follow tree order.
//!
//! * **A tag named twice.** `revs->cmdline` gets one entry per selector pass, so
//!   `--all --tags` files `refs/tags/v1` twice; `tag_refs` is never sorted or
//!   deduplicated (only `extra_refs` gets `string_list_sort_u`,
//!   `fast-export.c:1043`/`1119`) and `handle_tag` has no already-emitted guard,
//!   so the whole `tag` block is written once per entry.
//!
//! Every expectation below was captured from stock git 2.55.0 in the same
//! fixture, under the same pinned identity and dates, before being written down.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run `git <args>` in `repo`, returning stdout, stderr and the exit code.
///
/// Identity and both timestamps are pinned so the fixture — and therefore the
/// exported stream, which spells out author, committer and tagger lines — is the
/// same on every machine and every run.
fn git(repo: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .env("HOME", home)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0200")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0200")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// `git <args>`, failing loudly on a non-zero exit — for fixture construction,
/// where a partial success would silently weaken the premise.
fn must(repo: &Path, home: &Path, args: &[&str]) {
    let (_, stderr, code) = git(repo, home, args);
    assert_eq!(code, 0, "git {args:?} failed: {stderr}");
}

/// An empty repository plus its `HOME`, under a per-test temporary root.
fn empty(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fx-revflags-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    must(&repo, &home, &["init", "-q", "-b", "main"]);
    (root, repo, home)
}

/// A merge with a `base` branch left at the root commit and an annotated tag `v1`
/// on the merge:
///
/// ```text
///   initial commit  <- refs/heads/base
///    |          \
///   main commit  side commit  <- refs/heads/side
///    \          /
///     merge side  <- refs/heads/main, refs/tags/v1
/// ```
///
/// `base` being an ancestor of `main` is what makes `--not ^main base` legible:
/// the `^main` turns positive, `base` turns negative, and the export is exactly
/// `main ^base`.
fn merge_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (root, repo, home) = empty(tag);
    std::fs::write(repo.join("README.md"), "# fixture\n").unwrap();
    must(&repo, &home, &["add", "README.md"]);
    must(&repo, &home, &["commit", "-qm", "initial commit"]);
    must(&repo, &home, &["branch", "base"]);
    must(&repo, &home, &["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.join("side.txt"), "side work\n").unwrap();
    must(&repo, &home, &["add", "side.txt"]);
    must(&repo, &home, &["commit", "-qm", "side commit"]);
    must(&repo, &home, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("main.txt"), "main work\n").unwrap();
    must(&repo, &home, &["add", "main.txt"]);
    must(&repo, &home, &["commit", "-qm", "main commit"]);
    must(&repo, &home, &["merge", "-q", "--no-ff", "side", "-m", "merge side"]);
    must(&repo, &home, &["tag", "-a", "v1", "-m", "annotated one"]);
    (root, repo)
}

/// One commit adding five prefix-related entries: `C`, `C.a`, `C2`, `C3/x`, `Cz`.
/// Tree order (and therefore blob-mark order) is `C`, `C.a`, `C2`, `C3/x`, `Cz`;
/// `depth_first` order is the same list with `C` moved to the end.
fn prefix_names_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (root, repo, home) = empty(tag);
    std::fs::create_dir_all(repo.join("C3")).unwrap();
    std::fs::write(repo.join("C"), "c\n").unwrap();
    std::fs::write(repo.join("C.a"), "c0\n").unwrap();
    std::fs::write(repo.join("C2"), "c2\n").unwrap();
    std::fs::write(repo.join("C3/x"), "c3\n").unwrap();
    std::fs::write(repo.join("Cz"), "cz\n").unwrap();
    must(&repo, &home, &["add", "-A"]);
    must(&repo, &home, &["commit", "-qm", "prefix names"]);
    (root, repo)
}

/// A directory `d/` (with a nested `d/deep/`) replaced by a *file* named `d` —
/// the case `depth_first`'s comment is about, where every `D` under `d/` has to
/// precede the `M` that recreates the name.
fn dir_to_file_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (root, repo, home) = empty(tag);
    std::fs::create_dir_all(repo.join("d/deep")).unwrap();
    std::fs::write(repo.join("d/e"), "1\n").unwrap();
    std::fs::write(repo.join("d/deep/f"), "2\n").unwrap();
    std::fs::write(repo.join("d.x"), "3\n").unwrap();
    std::fs::write(repo.join("d2"), "4\n").unwrap();
    must(&repo, &home, &["add", "-A"]);
    must(&repo, &home, &["commit", "-qm", "tree"]);
    std::fs::remove_dir_all(repo.join("d")).unwrap();
    std::fs::write(repo.join("d"), "now a file\n").unwrap();
    must(&repo, &home, &["add", "-A"]);
    must(&repo, &home, &["commit", "-qm", "flip"]);
    (root, repo)
}

const AUTHOR: &str = "author A U Thor <author@example.com> 1112904793 +0200\n\
                      committer C O Mitter <committer@example.com> 1112904793 +0200\n";

// ---------------------------------------------------------------------------
// The `--not` / `^rev` XOR
// ---------------------------------------------------------------------------

/// `--not ^main base` exports `main ^base`, not nothing.
///
/// The `^` and the `--not` cancel, so `main` is the positive tip and `base` — a
/// bare name under `--not` — is the negative one. The label is the bare `main`
/// rather than `refs/heads/main` because `add_rev_cmdline` records the argument
/// *with* its caret, `repo_dwim_ref("^main")` finds no ref, and the fallback in
/// `handle_commit` (`revision.c:437-442`) uses the pending entry's name, which is
/// the argument minus the caret.
#[test]
fn not_flips_a_caret_revision_back_to_positive() {
    let (_root, repo) = merge_fixture("not-caret");
    let home = repo.parent().unwrap().join("home");

    let (stdout, stderr, code) = git(&repo, &home, &["fast-export", "--not", "^main", "base"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    assert_eq!(
        stdout,
        format!(
            "blob\nmark :1\ndata 10\n# fixture\n\n\
             blob\nmark :2\ndata 10\nmain work\n\n\
             commit main\nmark :3\n{AUTHOR}data 12\nmain commit\n\
             M 100644 :1 README.md\nM 100644 :2 main.txt\n\n\
             blob\nmark :4\ndata 10\nside work\n\n\
             commit main\nmark :5\n{AUTHOR}data 12\nside commit\n\
             M 100644 :1 README.md\nM 100644 :4 side.txt\n\n\
             commit main\nmark :6\n{AUTHOR}data 11\nmerge side\n\
             from :3\nmerge :5\nM 100644 :4 side.txt\n\n"
        )
    );
}

/// The control the fix must not break: without `--not`, a `^` is still negative.
///
/// `base ^main` is empty history — `base` is an ancestor of `main` — and the
/// stream is just the trailing `reset` for the ref whose commits were all
/// excluded. If the XOR were applied unconditionally this would export `main`.
#[test]
fn a_caret_revision_without_not_is_still_negative() {
    let (_root, repo) = merge_fixture("caret-alone");
    let home = repo.parent().unwrap().join("home");

    let (stdout, stderr, code) = git(&repo, &home, &["fast-export", "base", "^main"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    assert_eq!(
        stdout,
        "reset refs/heads/base\nfrom 0000000000000000000000000000000000000000\n\n"
    );
}

/// `--not` is a toggle, so a second one restores the positive sense.
///
/// `main --not --not base` has to be byte-identical to `main base`; a
/// set-once flag would have left `base` excluded and dropped the root commit
/// (and, with it, every `from :2` that hangs off it).
#[test]
fn a_second_not_toggles_negation_back_off() {
    let (_root, repo) = merge_fixture("double-not");
    let home = repo.parent().unwrap().join("home");

    let (toggled, stderr, code) =
        git(&repo, &home, &["fast-export", "main", "--not", "--not", "base"]);
    let (plain, _, plain_code) = git(&repo, &home, &["fast-export", "main", "base"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(plain_code, 0);
    assert_eq!(toggled, plain);
    assert!(toggled.contains("commit refs/heads/base\n"), "{toggled}");
    assert!(toggled.contains("data 15\ninitial commit\n"), "{toggled}");
}

/// A `--not` in force applies to the ref pseudo-options too.
///
/// `handle_refs` is handed `*flags` as it stands (`revision.c:2809`), so
/// `--not --all` makes every ref a negative tip; `main` is one of them, so the
/// whole history is excluded and only the ref-deletion `reset` remains.
#[test]
fn not_makes_the_ref_pseudo_options_negative() {
    let (_root, repo) = merge_fixture("not-all");
    let home = repo.parent().unwrap().join("home");

    let (all, stderr, code) = git(&repo, &home, &["fast-export", "main", "--not", "--all"]);
    let (branches, _, branches_code) =
        git(&repo, &home, &["fast-export", "main", "--not", "--branches"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(branches_code, 0);
    assert_eq!(
        all,
        "reset refs/heads/main\nfrom 0000000000000000000000000000000000000000\n\n"
    );
    // `--branches` covers `main` as well, so it excludes exactly as much.
    assert_eq!(branches, all);
}

/// `--not A..B` swaps the range's ends.
///
/// `handle_dotdot_1` gives B the current `flags` and A their inverse, so
/// `--not side..main` is `main..side` — here an empty export plus the trailing
/// `reset` for `side`.
#[test]
fn not_swaps_the_ends_of_a_range() {
    let (_root, repo) = merge_fixture("not-range");
    let home = repo.parent().unwrap().join("home");

    let (negated, stderr, code) = git(&repo, &home, &["fast-export", "--not", "side..main"]);
    let (mirrored, _, mirrored_code) = git(&repo, &home, &["fast-export", "main..side"]);
    let (plain, _, plain_code) = git(&repo, &home, &["fast-export", "side..main"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(mirrored_code, 0);
    assert_eq!(plain_code, 0);
    assert_eq!(negated, mirrored);
    assert_eq!(
        negated,
        "reset refs/heads/side\nfrom 0000000000000000000000000000000000000000\n\n"
    );
    // The un-negated range is the opposite selection and does export a commit,
    // so the two are not accidentally equal for some unrelated reason.
    assert!(plain.contains("data 11\nmerge side\n"), "{plain}");
}

// ---------------------------------------------------------------------------
// `M`-line order
// ---------------------------------------------------------------------------

/// `depth_first` puts the longer of two names that share a prefix first, so the
/// bare `C` is emitted last — behind `C.a`, `C2`, `C3/x` and `Cz`.
///
/// The blob marks in the same stanza are still in tree order (`C` is `:1`),
/// which pins the other half of the rule: the sort happens in `show_filemodify`,
/// after `handle_commit` has already walked the queue exporting blobs.
#[test]
fn m_lines_are_sorted_depth_first_not_by_name() {
    let (_root, repo) = prefix_names_fixture("prefix");
    let home = repo.parent().unwrap().join("home");

    let (stdout, stderr, code) = git(&repo, &home, &["fast-export", "--all"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    assert_eq!(
        stdout,
        format!(
            "blob\nmark :1\ndata 2\nc\n\n\
             blob\nmark :2\ndata 3\nc0\n\n\
             blob\nmark :3\ndata 3\nc2\n\n\
             blob\nmark :4\ndata 3\nc3\n\n\
             blob\nmark :5\ndata 3\ncz\n\n\
             reset refs/heads/main\ncommit refs/heads/main\nmark :6\n{AUTHOR}\
             data 13\nprefix names\n\
             M 100644 :2 C.a\nM 100644 :3 C2\nM 100644 :4 C3/x\n\
             M 100644 :5 Cz\nM 100644 :1 C\n\n"
        )
    );
}

/// The case the comment above `depth_first` describes: a directory whose
/// contents are all deleted while a file of the same name appears.
///
/// Both `D` lines have to come out before the `M` that creates `d`, or an
/// importer would try to hold a file and a directory under one name.
#[test]
fn deletions_below_a_directory_precede_the_file_that_replaces_it() {
    let (_root, repo) = dir_to_file_fixture("dir-to-file");
    let home = repo.parent().unwrap().join("home");

    let (stdout, stderr, code) = git(&repo, &home, &["fast-export", "--all"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    let flip = stdout.split("data 5\nflip\n").nth(1).unwrap_or_default();
    assert_eq!(flip, "from :5\nD d/deep/f\nD d/e\nM 100644 :6 d\n\n");
    // And the first commit's own ordering, where `d.x` and `d2` bracket `d/`.
    assert!(
        stdout.contains(
            "data 5\ntree\nM 100644 :1 d.x\nM 100644 :2 d/deep/f\nM 100644 :3 d/e\nM 100644 :4 d2\n"
        ),
        "{stdout}"
    );
}

// ---------------------------------------------------------------------------
// A tag reached by two selectors
// ---------------------------------------------------------------------------

/// `--all --tags` files `refs/tags/v1` twice, and each entry emits the block.
///
/// Nothing between the two selectors deduplicates: `tag_refs` is appended to per
/// entry and walked back to front without a sort, and `handle_tag` never asks
/// whether the tag has been written already.
#[test]
fn a_tag_reached_by_two_selectors_is_emitted_twice() {
    let (_root, repo) = merge_fixture("tag-twice");
    let home = repo.parent().unwrap().join("home");

    let (both, stderr, code) = git(&repo, &home, &["fast-export", "--all", "--tags"]);
    let (only_all, _, all_code) = git(&repo, &home, &["fast-export", "--all"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(all_code, 0);
    let block = format!(
        "tag v1\nfrom :7\n\
         tagger C O Mitter <committer@example.com> 1112904793 +0200\n\
         data 14\nannotated one\n"
    );
    assert_eq!(both.matches(&block).count(), 2, "{both}");
    assert_eq!(only_all.matches(&block).count(), 1, "{only_all}");
    // The commits themselves are not duplicated — only the cmdline entries are,
    // so the `--all --tags` stream is the `--all` one with one more block glued
    // on the end, blank separator and all.
    assert_eq!(both.matches("\nmark :7\n").count(), 1, "{both}");
    assert_eq!(both, format!("{only_all}{block}\n"));
}

/// Selector order decides which name labels a commit, so `--tags --all` is not
/// `--all --tags` with the same bytes.
///
/// The `--tags` pass runs first, claims `revision_sources` for the tagged commit
/// under `refs/tags/v1`, and leaves `refs/heads/main` to be re-pointed by a
/// trailing `reset`. A selection folded into one sorted pass cannot express this.
#[test]
fn selector_order_decides_the_commit_label() {
    let (_root, repo) = merge_fixture("selector-order");
    let home = repo.parent().unwrap().join("home");

    let (tags_first, stderr, code) = git(&repo, &home, &["fast-export", "--tags", "--all"]);
    let (all_first, _, all_code) = git(&repo, &home, &["fast-export", "--all", "--tags"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(all_code, 0);
    assert!(tags_first.contains("commit refs/tags/v1\nmark :7\n"), "{tags_first}");
    assert!(tags_first.contains("reset refs/heads/main\nfrom :7\n"), "{tags_first}");
    assert!(all_first.contains("commit refs/heads/main\nmark :7\n"), "{all_first}");
    assert!(!all_first.contains("commit refs/tags/v1"), "{all_first}");
    assert_ne!(tags_first, all_first);
}

/// With `--mark-tags` the second copy gets a *fresh* mark: `mark_next_object`
/// is called per emission, and the tag's existing mark is never consulted.
#[test]
fn the_second_copy_of_a_tag_takes_a_new_mark() {
    let (_root, repo) = merge_fixture("tag-marks");
    let home = repo.parent().unwrap().join("home");

    let (stdout, stderr, code) =
        git(&repo, &home, &["fast-export", "--all", "--tags", "--mark-tags"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    assert!(stdout.contains("tag v1\nmark :8\nfrom :7\n"), "{stdout}");
    assert!(stdout.contains("tag v1\nmark :9\nfrom :7\n"), "{stdout}");
}
