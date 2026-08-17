//! The parts of `setup_revisions()` `format-patch` inherits but does not spell
//! itself: the `--all` family of pseudo-revisions, `cmd_format_patch`'s
//! single-endpoint promotion, and what `log_tree_diff()` does with a merge the
//! parent-count bounds let through.
//!
//! None of these are format-patch options. `parse_options()` keeps them
//! (`PARSE_OPT_KEEP_UNKNOWN_OPT`) and hands them to `setup_revisions()`, which
//! seeds each selector into the same pending list the revision arguments feed —
//! so `--all` and `^main~1` end up in one list, in command-line order, and the
//! rules below all read off the *size* of that list rather than off the argument
//! count.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

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
    /// ```text
    ///   main:  A --- B --- C ------- M
    ///           \                   /
    ///   b2:      \--- D --- E ------/
    /// ```
    /// with commit dates A=05 B=10 D=20 E=30 C=40 M=50, plus a lightweight tag
    /// `v1` on C and a remote-tracking `refs/remotes/origin/main` on B, so each
    /// namespace selector picks a different tip.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fppseudo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.commit("A", 5, 5);
        f.git(&["branch", "b2"]);
        f.commit("B", 10, 40);
        f.commit("C", 40, 10);
        f.git(&["checkout", "-q", "b2"]);
        f.commit("D", 20, 30);
        f.commit("E", 30, 20);
        f.git(&["checkout", "-q", "main"]);
        f.at(&["merge", "-q", "--no-ff", "-m", "M", "b2"], 50, 50);
        f.git(&["tag", "v1", "main~1"]);
        f.git(&["update-ref", "refs/remotes/origin/main", "main~2"]);
        f
    }

    /// A repository sitting on an unborn branch, for the `s_r_opt.def` fallback.
    /// `orphan` also gives it one commit and a tag to name, so a lone endpoint
    /// can be promoted against a HEAD that does not resolve.
    fn unborn(tag: &str, orphan: bool) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fppseudo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        if orphan {
            f.git(&["config", "user.email", "t@e.co"]);
            f.git(&["config", "user.name", "t"]);
            f.commit("A", 5, 5);
            f.git(&["tag", "v1"]);
            f.git(&["checkout", "-q", "--orphan", "fresh"]);
            f.git(&["rm", "-q", "--cached", "A.txt"]);
        }
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
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL")
            .env_remove("GIT_AUTHOR_DATE")
            .env_remove("GIT_COMMITTER_DATE");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// Run a setup command with both timestamps pinned to a second of 2020-01-01.
    fn at(&self, args: &[&str], commit_sec: u32, author_sec: u32) {
        let out = self
            .cmd(args)
            .env("GIT_COMMITTER_DATE", stamp(commit_sec))
            .env("GIT_AUTHOR_DATE", stamp(author_sec))
            .output()
            .unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn commit(&self, name: &str, commit_sec: u32, author_sec: u32) {
        std::fs::write(self.work.join(format!("{name}.txt")), format!("{name}\n")).unwrap();
        self.git(&["add", "-A"]);
        self.at(&["commit", "-q", "-m", name], commit_sec, author_sec);
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn text(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The `Subject:` line of every patch the invocation emitted, in order, with
    /// the `[PATCH n/m]` bracket dropped so only the series is asserted.
    fn subjects(&self, args: &[&str]) -> Vec<String> {
        let out = self.run(args);
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix("Subject: "))
            .map(|s| s.rsplit("] ").next().unwrap_or(s).to_owned())
            .collect()
    }

    /// `format-patch --stdout <extra>`, as the series of subjects it produced.
    fn series(&self, extra: &[&str]) -> Vec<String> {
        let mut args = vec!["format-patch", "--stdout"];
        args.extend_from_slice(extra);
        self.subjects(&args)
    }
}

fn stamp(sec: u32) -> String {
    format!("2020-01-01T00:00:{sec:02} +0000")
}

/// `cmd_format_patch` promotes a lone endpoint into `<since>..HEAD` on
/// `rev.pending.nr == 1`, and `rev.pending` counts UNINTERESTING objects too.
///
/// So `^<rev>` alone is promoted exactly like `<rev>` — marking an object that is
/// already UNINTERESTING changes nothing, and HEAD still joins it — while
/// `^<a> <b>` is two pending objects and is walked as the range it spells.
#[test]
fn promotion_counts_every_pending_object() {
    let f = Fixture::new("promote");

    // Two pending objects: no promotion, just the range. `main~1` is C.
    assert_eq!(f.series(&["^main~1", "main"]), ["D", "E"]);
    assert_eq!(f.series(&["^b2", "main"]), ["B", "C"]);
    assert_eq!(f.series(&["main", "^b2"]), ["B", "C"]);
    // Three of them, likewise: `main~2` is B, so only C survives.
    assert_eq!(f.series(&["^main~2", "^b2", "main"]), ["C"]);

    // One pending object, excluded: promoted to `main~1..HEAD` all the same.
    assert_eq!(f.series(&["^main~1"]), ["D", "E"]);
    assert_eq!(f.series(&["main~1"]), ["D", "E"]);

    // `-<n>` and `--root` opt out of the promotion, which leaves a lone
    // exclusion with no interesting tip at all — an empty series, not HEAD's.
    assert!(f.series(&["--root", "^main~1"]).is_empty());
    assert!(f.series(&["-2", "^main~1"]).is_empty());
    // The same two flags still walk HEAD when nothing was named, because that is
    // `s_r_opt.def` putting HEAD on the pending list rather than the promotion.
    assert_eq!(f.series(&["-2"]), ["E", "C"]);
    assert_eq!(f.series(&["--root"]), ["A", "B", "D", "E", "C"]);
}

/// `--not` reverses the sense of every revision after it and toggles again at the
/// next one, so it decides which side of the pending list each endpoint lands on
/// — and two of them are still two, so nothing is promoted.
#[test]
fn not_flips_the_sense_of_what_follows() {
    let f = Fixture::new("not");

    assert_eq!(f.series(&["main", "--not", "b2"]), ["B", "C"]);
    assert_eq!(f.series(&["--not", "b2", "--not", "main"]), ["B", "C"]);
    // Everything named is excluded: two pending objects, no interesting tip.
    assert!(f.series(&["--not", "b2", "main"]).is_empty());
    // Toggled back off, `main` is a lone endpoint again — and being promoted to
    // `main..HEAD` with HEAD *at* main, it formats nothing.
    assert!(f.series(&["--not", "--not", "main"]).is_empty());
    // `^` flips whatever `--not` set, as git XORs both: two positive tips again.
    assert_eq!(
        f.series(&["--not", "^main", "^b2"]),
        ["A", "B", "D", "E", "C"]
    );
}

/// The `--all` family seeds refs into the pending list where the option stands.
///
/// Each namespace form walks its own prefix and adds no HEAD, so in this fixture
/// `--tags` and `--remotes` each name exactly one object — and are therefore
/// promoted to `<tag>..HEAD` and `<remote>..HEAD`.
#[test]
fn the_all_family_seeds_refs_into_the_pending_list() {
    let f = Fixture::new("family");
    let all = ["A", "B", "D", "E", "C"];

    // `--all` is every ref plus HEAD: five pending objects, so no promotion.
    assert_eq!(f.series(&["--all"]), all);
    assert_eq!(f.series(&["--branches"]), all);
    // One tag (`v1` on C) and one remote-tracking ref (on B), each promoted.
    assert_eq!(f.series(&["--tags"]), ["D", "E"]);
    assert_eq!(f.series(&["--remotes"]), ["D", "E", "C"]);

    // `=<pattern>` matches the name with the namespace stripped, and a pattern
    // with no wildcard gains an implied `/*`.
    assert_eq!(f.series(&["--branches=b*"]), ["B", "C"]);
    assert_eq!(f.series(&["--tags=v*"]), ["D", "E"]);
    assert_eq!(f.series(&["--remotes=ori*"]), ["D", "E", "C"]);

    // `--glob` matches full ref names and prepends `refs/` when the pattern does
    // not carry it, stuck or separate.
    assert_eq!(f.series(&["--glob=refs/heads/*"]), all);
    assert_eq!(f.series(&["--glob=heads"]), all);
    assert_eq!(f.series(&["--glob", "refs/heads/*"]), all);

    // A selector counts as revision input even when it matched nothing, so
    // `s_r_opt.def` does not quietly substitute HEAD.
    assert!(f.series(&["--glob=refs/nope/*"]).is_empty());

    // A selector under `--not` excludes what it names: everything but the tag's
    // ancestry survives.
    assert_eq!(f.series(&["--all", "--not", "--tags"]), ["D", "E"]);
}

/// `--exclude` accumulates into `revs->ref_excludes`, which the *next* selector
/// consumes and then empties with `clear_ref_exclusions()`.
///
/// It is matched against the name `handle_one_ref()` receives, which the
/// namespace forms have already trimmed — so `--exclude=b2` bites `--branches`
/// while `--exclude=refs/heads/b2` bites `--all`, and neither works the other way
/// round.
#[test]
fn exclude_feeds_exactly_one_following_selector() {
    let f = Fixture::new("exclude");
    let all = ["A", "B", "D", "E", "C"];

    // With b2 gone, `--branches` is the single ref `main`, promoted to
    // `main..HEAD` — and HEAD is main, so nothing is formatted.
    assert!(f.series(&["--exclude=b2", "--branches"]).is_empty());
    assert!(f.series(&["--exclude", "b2", "--branches"]).is_empty());
    // The full name does not match the trimmed one `--branches` hands over.
    assert_eq!(f.series(&["--exclude=refs/heads/b2", "--branches"]), all);
    // `--all` sees full names, so the two swap round.
    assert_eq!(f.series(&["--exclude=b2", "--all"]), all);

    // The list is emptied by the selector that consumed it: `--tags` sees none of
    // it, so `v1` joins `main` and the two of them are not promoted.
    assert_eq!(f.series(&["--exclude=b2", "--branches", "--tags"]), all);
    // A selector left with nothing is still revision input.
    assert!(f.series(&["--exclude=v1", "--tags"]).is_empty());
    // A `--exclude` that no selector follows is simply never read.
    assert_eq!(f.series(&["--branches", "--exclude=b2"]), all);
}

/// `parse_long_opt()` (diff.c) reads `--glob`/`--exclude` values, and it `die()`s
/// with its own wording — not parse-options' usage block — when the separate form
/// has nothing after it.
#[test]
fn glob_without_a_value_is_fatal() {
    let f = Fixture::new("globval");

    for opt in ["--glob", "--exclude"] {
        let out = f.run(&["format-patch", "--stdout", opt]);
        assert_eq!(out.status.code(), Some(128), "{opt}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("fatal: Option '{opt}' requires a value\n")
        );
    }
}

/// A merge that the parent-count bounds admit is emitted as its header alone.
///
/// `log_tree_diff()` returns before producing anything for a commit with more
/// than one parent unless `separate_merges` (`-m`) or `combine_merges` (`-c`)
/// is set, and format-patch sets neither. `log_tree_commit()` then falls back to
/// `always_show_header`, so there is no three-dash separator, no diffstat and no
/// patch — whatever diff format was asked for.
#[test]
fn a_formatted_merge_carries_no_diff_body() {
    let f = Fixture::new("merge");
    let head = f.text(&["rev-parse", "main"]).trim().to_owned();

    for extra in [
        vec!["--max-parents=2", "-1", "main"],
        vec!["--min-parents=2", "--max-parents=2", "--root", "main"],
        vec!["--numstat", "--max-parents=2", "-1", "main"],
        vec!["--stat", "--max-parents=2", "-1", "main"],
        vec!["-p", "--max-parents=2", "-1", "main"],
    ] {
        let mut args = vec!["format-patch", "--stdout"];
        args.extend_from_slice(&extra);
        let body = f.text(&args);
        assert_eq!(
            body,
            format!(
                "From {head} Mon Sep 17 00:00:00 2001\n\
                 From: t <t@e.co>\n\
                 Date: Wed, 1 Jan 2020 00:00:50 +0000\n\
                 Subject: [PATCH] M\n\
                 \n\
                 -- \n\
                 2.55.0\n\n"
            ),
            "{extra:?}"
        );
    }

    // A non-merge in the very same series still carries its patch, so this is
    // the merge's parent count deciding and not the flag.
    let body = f.text(&["format-patch", "--stdout", "--max-parents=2", "-2", "main"]);
    assert_eq!(f.series(&["--max-parents=2", "-2", "main"]), ["C", "M"]);
    assert!(body.contains("diff --git a/C.txt b/C.txt"), "{body}");
    assert_eq!(body.matches("\ndiff --git ").count(), 1, "{body}");

    // `--notes` opens its own commentary block, which is the one three-dash line
    // a merge can still show.
    f.git(&["notes", "add", "-m", "n", "main"]);
    let body = f.text(&[
        "format-patch",
        "--stdout",
        "--notes",
        "--max-parents=2",
        "-1",
        "main",
    ]);
    assert!(body.contains("---\n\nNotes:\n    n\n"), "{body}");
    assert!(!body.contains("diff --git"), "{body}");
}

/// `s_r_opt.def` only fires for a command line that named no revision at all, and
/// an unborn HEAD there is `diagnose_missing_default()` — a `die()`, not an empty
/// series. A pseudo-revision selector counts as input even when the repository
/// has no refs for it to find, so it exits quietly instead.
#[test]
fn an_unborn_head_is_reported_only_when_it_was_the_default() {
    let f = Fixture::unborn("unborn", false);

    let out = f.run(&["format-patch", "--stdout"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: your current branch 'main' does not have any commits yet\n"
    );

    for selector in ["--all", "--branches", "--tags", "--remotes"] {
        let out = f.run(&["format-patch", "--stdout", selector]);
        assert_eq!(out.status.code(), Some(0), "{selector}: {out:?}");
        assert!(out.stdout.is_empty(), "{selector}: {out:?}");
        assert!(out.stderr.is_empty(), "{selector}: {out:?}");
    }

    // With a commit somewhere else in the repository a lone endpoint reaches the
    // promotion, and `add_head_to_pending()` gives up quietly on a HEAD that does
    // not resolve — leaving the endpoint excluded and nothing to format.
    let g = Fixture::unborn("orphan", true);
    for extra in [vec!["v1"], vec!["^v1"], vec!["--tags"], vec!["--branches"]] {
        let mut args = vec!["format-patch", "--stdout"];
        args.extend_from_slice(&extra);
        let out = g.run(&args);
        assert_eq!(out.status.code(), Some(0), "{extra:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{extra:?}: {out:?}");
        assert!(out.stderr.is_empty(), "{extra:?}: {out:?}");
    }
    // `-<n>` names no revision of its own, so it is still the default that fails.
    let out = g.run(&["format-patch", "--stdout", "-3"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: your current branch 'fresh' does not have any commits yet\n"
    );
    // `--all` still finds the ref that HEAD does not.
    assert_eq!(g.series(&["--all"]), ["A"]);
}

/// `handle_revision_arg_1()` type-checks nothing, so a tree, a blob or a tag of
/// either goes on the pending list exactly like a commit — and is dropped, in
/// silence, by `handle_commit()` (`revision.c`) once `prepare_revision_walk()`
/// runs, because `rev.tree_objects` and `rev.blob_objects` are off:
///
/// ```c
/// if (object->type == OBJ_TREE) {
///         struct tree *tree = (struct tree *)object;
///         if (!revs->tree_objects)
///                 return NULL;
/// ```
///
/// The two facts have to hold together, and the order between them is what makes
/// this worth pinning: `cmd_format_patch` counts `rev.pending.nr` *before* the
/// drop, so a lone `<tree>` is one pending object, is promoted to `<tree>..HEAD`,
/// and only then loses the tree — leaving HEAD as an unbounded tip and the whole
/// history formatted. A port that peels each operand to a commit while resolving
/// it instead calls the same command line `fatal: ambiguous argument`.
///
/// The annotated-tag spellings are here because they separate the operand's id
/// from the peeled one: a bare `HEAD^{tree}` makes the two coincide, so a port
/// that confuses them still passes.
#[test]
fn a_non_commit_tip_is_dropped_after_the_pending_count() {
    let f = Fixture::new("nontip");

    // Every non-merge commit, oldest first — what an unbounded walk from HEAD
    // formats once the endpoint has disappeared.
    let full = f.series(&["--root"]);
    assert_eq!(full, ["A", "B", "D", "E", "C"]);

    f.git(&["tag", "-a", "treetag", "-m", "t", "HEAD^{tree}"]);
    f.git(&["tag", "-a", "blobtag", "-m", "b", "HEAD:A.txt"]);

    // One pending object of a type the walk has no use for: promoted, then
    // dropped, so HEAD is walked with nothing excluded.
    for tip in ["HEAD^{tree}", "HEAD:A.txt", "treetag", "blobtag"] {
        assert_eq!(f.series(&[tip]), full, "{tip}");
    }
    // `^<tree>` is one pending object too, so it is promoted the same way; the
    // exclusion it carries dies with the tree.
    assert_eq!(f.series(&["^HEAD^{tree}"]), full);

    // Two pending objects: no promotion. HEAD is a tip in its own right and the
    // tree simply vanishes, whichever side of it the tree stands on.
    assert_eq!(f.series(&["HEAD^{tree}", "main"]), full);
    assert_eq!(f.series(&["main", "HEAD^{tree}"]), full);

    // Without the promotion there is no HEAD to fall back on, so a walk whose
    // only positive endpoint was the tree formats nothing — and still exits 0.
    assert!(f.series(&["main~2..HEAD^{tree}"]).is_empty());
    assert!(f.series(&["-1", "HEAD^{tree}"]).is_empty());
    assert!(f.series(&["--root", "HEAD^{tree}"]).is_empty());
}

/// `verify_non_filename()` (`setup.c`), which `handle_revision_arg_1()` runs
/// between resolving a name and pending it:
///
/// ```c
/// if (get_oid_with_context(revs->repo, arg, get_sha1_flags, &oid, &oc))
///         return revs->ignore_missing ? 0 : -1;
/// if (!cant_be_filename)
///         verify_non_filename(revs->prefix, arg);
/// object = get_reference(revs, arg, &oid, flags ^ local_flags);
/// ```
///
/// A word that is simultaneously a revision and a working-tree path is refused
/// rather than guessed at, and the two things this test pins are *where* in that
/// sequence the refusal sits:
///
/// * before `get_reference()`, so a full-length hex naming an object the
///   repository does not have is `both revision and filename` and not
///   `bad object` — the file wins the race against the missing object;
/// * after the `^` strip, so `^<name>` is refused under the bare name, and
///   `cant_be_filename` (any `--` on the line) turns the check off entirely,
///   which is what makes `git format-patch <name> --` the way to say "the
///   revision, please".
#[test]
fn a_word_that_is_both_a_revision_and_a_path_is_refused() {
    let f = Fixture::new("bothrevfile");
    // An object id the repository does not have, so the ordering above is
    // observable: without the check this word is `fatal: bad object`.
    let absent = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    std::fs::write(f.work.join("b2"), "x\n").unwrap();
    std::fs::write(f.work.join(absent), "x\n").unwrap();

    let refusal = |args: &[&str], named: &str| {
        let out = f.run(args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!(
                "fatal: ambiguous argument '{named}': both revision and filename\n\
                 Use '--' to separate paths from revisions, like this:\n\
                 'git <command> [<revision>...] -- [<file>...]'\n"
            ),
            "{args:?}"
        );
    };

    refusal(&["format-patch", "--stdout", "b2"], "b2");
    // The `^` is a flag, not part of the name, so the message names what is left.
    refusal(&["format-patch", "--stdout", "^b2", "main"], "b2");
    // Whichever position it takes, since every operand goes through the check.
    refusal(&["format-patch", "--stdout", "b2", "v1"], "b2");
    refusal(&["format-patch", "--stdout", "v1", "b2"], "b2");
    // `--root` opts out of the single-endpoint promotion, not out of this.
    refusal(&["format-patch", "--stdout", "--root", "b2"], "b2");
    // Ahead of `get_reference()`'s `die("bad object %s")`.
    refusal(&["format-patch", "--stdout", absent], absent);

    // `REVARG_CANNOT_BE_FILENAME`: a `--` anywhere on the line skips the check,
    // so the word is read as the revision it also is — here promoted to
    // `b2..HEAD`, exactly as the explicit spelling walks it.
    assert_eq!(f.series(&["b2", "--"]), ["B", "C"]);
    // …and after the `--` it is only ever a path, which limits the default walk.
    assert!(f.series(&["--", "b2"]).is_empty());

    // A revision no file shadows is untouched by any of this.
    assert_eq!(f.series(&["v1"]), ["D", "E"]);
}
