//! The two halves of `get_oid_with_context_1()`'s path arm, and the pending
//! objects that never become commits.
//!
//! Both families share the property that makes them worth a suite: the failure is
//! quiet. A `<rev>:<path>` that git diagnoses precisely was answered here with the
//! generic `ambiguous argument` text, and a tree-ish handed to `rev-list` was a
//! `fatal` where stock exits 0 — but `git rev-list --objects main^{tree}` is a
//! *wrong object list at exit 0*, which no status check can see.
//!
//! * **Resolution.** `main:./f`, `:./f` and `:0:./f` are relative to the current
//!   directory, and `resolve_relative_path()` → `prefix_path()`
//!   (`object-name.c:1702-1714`) rewrites them before the index or the tree is
//!   consulted. Without that step every one of them was refused.
//!
//! * **Diagnosis.** `verify_filename()` → `die_verify_filename()`
//!   (`setup.c:202-225`) resolves the operand a *second* time, with
//!   `GET_OID_ONLY_TO_DIE`, so `diagnose_invalid_oid_path()` and
//!   `diagnose_invalid_index_path()` can name the failure. The same second
//!   resolution is why `peel_onion()`'s `error:` line comes out twice for
//!   `git rev-parse main^{blob}` and once for `git cat-file -t main^{blob}`.
//!
//! * **Pending non-commits.** `handle_commit()`'s tree and blob arms pend rather
//!   than fail, and `traverse_non_commits()` (`list-objects.c:344-375`) walks that
//!   list after every commit. `--indexed-objects` fills the same list from the
//!   index and its cache-tree.
//!
//! * **Ref-set options.** `handle_ref_opt()` (`builtin/rev-parse.c:615-634`)
//!   matches its pattern against the *whole* refname and then hands the callback
//!   a name with the namespace trimmed — so `--exclude` is tested against the
//!   trimmed name — and clears the exclusion list afterwards.
//!
//! Every expectation was measured against stock git 2.55.0 on the fixture this
//! file builds.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "zvcs test")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "zvcs test")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(out.status.success(), "git {args:?} failed: {}", err_of(&out));
}

fn out_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn err_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn code_of(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn oid(repo: &Path, home: &Path, spec: &str) -> String {
    let out = run(repo, home, &["rev-parse", spec]);
    assert!(out.status.success(), "rev-parse {spec}: {}", err_of(&out));
    out_of(&out).trim_end().to_string()
}

/// Two commits, one subdirectory, one annotated tag. The subdirectory is what
/// gives the relative-path forms somewhere to be relative *to*, and the tag is
/// what puts a non-tree, non-blob entry in the pending list.
///
/// ```text
/// c1 ── c2   (main, HEAD, atag)
/// ```
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-objpath-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(repo.join("sub")).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    std::fs::write(repo.join("sub/s.txt"), "sub\n").unwrap();
    git(&repo, &home, &["add", "base.txt", "sub/s.txt"]);
    git(&repo, &home, &["commit", "-q", "-m", "c1"]);
    std::fs::write(repo.join("second.txt"), "second\n").unwrap();
    git(&repo, &home, &["add", "second.txt"]);
    git(&repo, &home, &["commit", "-q", "-m", "c2"]);
    git(&repo, &home, &["tag", "-a", "-m", "annot", "atag"]);
    (repo, home)
}

/// `resolve_relative_path()`: the `./`/`../` spellings resolve, and they resolve
/// *against the current directory* rather than the top of the work tree.
///
/// Both endpoints matter. From the root `main:./base.txt` is `main:base.txt`;
/// from `sub/` the same operand is `main:sub/base.txt`, which does not exist —
/// so a port that merely stripped the `./` would pass the first half and answer
/// the wrong object in the second.
#[test]
fn relative_path_arms_resolve_against_the_current_directory() {
    let (repo, home) = fixture("rel");
    let sub = repo.join("sub");
    let base_blob = oid(&repo, &home, "main:base.txt");
    let s_blob = oid(&repo, &home, "main:sub/s.txt");

    for spec in [":./base.txt", ":0:./base.txt", "main:./base.txt", "HEAD:./base.txt"] {
        let out = run(&repo, &home, &["rev-parse", spec]);
        assert_eq!(out_of(&out).trim_end(), base_blob, "from the root, {spec}");
    }
    // From `sub/`: `./s.txt` is `sub/s.txt`, and `../base.txt` climbs back out.
    // `./../base.txt` exercises the collapse rather than a plain prefix strip.
    for (spec, want) in [
        (":./s.txt", &s_blob),
        (":0:./s.txt", &s_blob),
        ("main:./s.txt", &s_blob),
        (":../base.txt", &base_blob),
        ("main:../base.txt", &base_blob),
        (":./../base.txt", &base_blob),
    ] {
        let out = run(&sub, &home, &["rev-parse", spec]);
        assert_eq!(out_of(&out).trim_end(), *want, "from sub/, {spec}");
    }
    // `main:./base.txt` names `sub/base.txt` from here, which does not exist.
    let out = run(&sub, &home, &["rev-parse", "main:./base.txt"]);
    assert_eq!(code_of(&out), 128);
    assert_eq!(err_of(&out), "fatal: path 'sub/base.txt' does not exist in 'main'\n");
}

/// `prefix_path()` dies while the *resolution* is still running, not from
/// `die_verify_filename()`. Two consequences, both checked: the operand is never
/// echoed on stdout, and the magic-pathspec guard that silences `:./nosuch` does
/// not silence this.
#[test]
fn a_relative_path_that_leaves_the_work_tree_dies_before_the_echo() {
    let (repo, home) = fixture("escape");
    let sub = repo.join("sub");
    let top = std::fs::canonicalize(&repo).unwrap();
    let want = format!("fatal: '../../escape' is outside repository at '{}'\n", top.display());

    for args in [
        vec!["rev-parse", ":../../escape"],
        vec!["rev-parse", "main:../../escape"],
        vec!["rev-list", ":../../escape"],
        vec!["log", "--oneline", ":../../escape"],
    ] {
        let out = run(&sub, &home, &args);
        assert_eq!(code_of(&out), 128, "{args:?}");
        assert_eq!(err_of(&out), want, "{args:?}");
        assert_eq!(out_of(&out), "", "{args:?} must die before show_file()");
    }
}

/// `diagnose_invalid_oid_path()` and `diagnose_invalid_index_path()`, one case
/// per branch of the C. These are the messages `die_verify_filename()` prefers
/// over `ambiguous argument …`, and they are shared by every verb that splits
/// revisions from paths.
#[test]
fn a_failed_path_arm_gets_gits_own_diagnosis() {
    let (repo, home) = fixture("diag");
    let sub = repo.join("sub");

    for (spec, want) in [
        // `get_tree_entry()` missed and the file is not on disk either.
        ("main:nosuch", "fatal: path 'nosuch' does not exist in 'main'\n"),
        // The scan for the splitting `:` stops at the first unbracketed one, so
        // the peel suffix is part of the *path*, not a peel of the blob.
        (
            "main:base.txt^{tree}",
            "fatal: path 'base.txt^{tree}' does not exist in 'main'\n",
        ),
        // Only `0`..`3` are stages: `:4:` is stage 0 over the path `4:base.txt`.
        (
            ":4:base.txt",
            "fatal: path '4:base.txt' does not exist (neither on disk nor in the index)\n",
        ),
        (
            ":nosuch",
            "fatal: path 'nosuch' does not exist (neither on disk nor in the index)\n",
        ),
    ] {
        let out = run(&repo, &home, &["rev-parse", spec]);
        assert_eq!(code_of(&out), 128, "{spec}");
        assert_eq!(err_of(&out), want, "{spec}");
    }

    // The wrong-stage branch: the path *is* in the index, at another stage.
    for stage in ["1", "2", "3"] {
        let spec = format!(":{stage}:base.txt");
        let out = run(&repo, &home, &["rev-parse", &spec]);
        assert_eq!(
            err_of(&out),
            format!(
                "fatal: path 'base.txt' is in the index, but not at stage {stage}\n\
                 hint: Did you mean ':0:base.txt'?\n"
            ),
            "{spec}"
        );
    }

    // The relative/absolute confusion branch, which needs a prefix to exist at
    // all: from `sub/`, a bare `s.txt` is not an index path but `sub/s.txt` is.
    // git has already chdir'd to the top of the work tree by this point, so the
    // on-disk test that precedes this one looks at `<top>/s.txt` and misses —
    // even though `s.txt` does exist in the directory the user is standing in.
    let out = run(&sub, &home, &["rev-parse", ":s.txt"]);
    assert_eq!(
        err_of(&out),
        "fatal: path 'sub/s.txt' is in the index, but not 's.txt'\n\
         hint: Did you mean ':0:sub/s.txt' aka ':0:./s.txt'?\n"
    );
    let out = run(&sub, &home, &["rev-parse", "main:s.txt"]);
    assert_eq!(
        err_of(&out),
        "fatal: path 'sub/s.txt' exists, but not 's.txt'\n\
         hint: Did you mean 'main:sub/s.txt' aka 'main:./s.txt'?\n"
    );

    // `if (!(arg[0] == ':' && !isalnum(arg[1]))) maybe_die_on_misspelt_object_name(…)`
    // — a leading `:` followed by a non-alphanumeric is magic-pathspec shaped and
    // is never diagnosed, so `:./nosuch` keeps the generic text while `:nosuch`
    // above does not.
    let out = run(&sub, &home, &["rev-parse", ":./nosuch"]);
    assert_eq!(
        err_of(&out),
        "fatal: ambiguous argument ':./nosuch': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    );
}

/// The same diagnosis reaches the walkers, because they share
/// `verify_filename()`. A regression that fixed only `rev-parse` would leave
/// `git log main:nosuch` on the generic message.
#[test]
fn the_walkers_share_the_path_diagnosis() {
    let (repo, home) = fixture("walkers");
    for args in [
        vec!["log", "--oneline", "main:nosuch"],
        vec!["rev-list", "main:nosuch"],
        vec!["show", "main:nosuch"],
        vec!["shortlog", "main:nosuch"],
    ] {
        let out = run(&repo, &home, &args);
        assert_eq!(code_of(&out), 128, "{args:?}");
        assert_eq!(err_of(&out), "fatal: path 'nosuch' does not exist in 'main'\n", "{args:?}");
    }
}

/// `repo_peel_to_type()`'s `error()` (`object-name.c:897-903`), and the count of
/// it, which is a direct read-out of how many times the operand was resolved.
///
/// Two for `git rev-parse main^{blob}` — once for the failed resolution, once for
/// `die_verify_filename()`'s second pass — and one wherever that second pass does
/// not happen: under `--verify`, after a `--`, and behind a leading `^`, all of
/// which die before `verify_filename()` is reached.
///
/// The message always says `dereferences to tree type` because the chain ends at
/// a tree: a tag peels to its target, a commit to its tree, and a tree has
/// nowhere left to go.
#[test]
fn an_unreachable_peel_type_reports_once_per_resolution() {
    let (repo, home) = fixture("peel");
    let blob_err = "error: main^{blob}: expected blob type, but the object dereferences to tree type\n";

    let out = run(&repo, &home, &["rev-parse", "main^{blob}"]);
    assert_eq!(code_of(&out), 128);
    assert_eq!(
        err_of(&out),
        format!(
            "{blob_err}{blob_err}fatal: ambiguous argument 'main^{{blob}}': \
             unknown revision or path not in the working tree.\n\
             Use '--' to separate paths from revisions, like this:\n\
             'git <command> [<revision>...] -- [<file>...]'\n"
        )
    );

    // `if (verify) die_no_single_rev(quiet);` comes before `verify_filename()`.
    let out = run(&repo, &home, &["rev-parse", "--verify", "main^{blob}"]);
    assert_eq!(err_of(&out), format!("{blob_err}fatal: Needed a single revision\n"));
    // `has_dashdash` is decided by a scan of the whole vector, so a separator
    // *after* the operand still makes it revision-only — and it dies before the
    // echo, leaving stdout empty.
    let out = run(&repo, &home, &["rev-parse", "main^{blob}", "--"]);
    assert_eq!(err_of(&out), format!("{blob_err}fatal: bad revision 'main^{{blob}}'\n"));
    assert_eq!(out_of(&out), "");
    // `handle_revision_arg_1()` strips the exclusion mark before resolving, so
    // the message names the operand without it — and `^main^{blob}` on the second
    // pass has a base `^main` that does not resolve, so there is no second line.
    let out = run(&repo, &home, &["rev-list", "^main^{blob}"]);
    assert_eq!(err_of(&out), format!("{blob_err}fatal: bad revision '^main^{{blob}}'\n"));

    // `^{}`, `^{object}` and a reachable type never produce the line.
    for spec in ["main^{}", "main^{object}", "main^{commit}", "main^{tree}", "atag^{tag}"] {
        let out = run(&repo, &home, &["rev-parse", spec]);
        assert!(out.status.success(), "{spec}: {}", err_of(&out));
        assert_eq!(err_of(&out), "", "{spec}");
    }
}

/// A tree-ish or a blob named on the command line is a *pending object*, not an
/// error: it contributes no commits, and under `--objects` it contributes its own
/// line plus everything under it.
///
/// The exit-0-with-the-wrong-set case is the first assertion: `rev-list
/// main^{tree}` prints nothing and succeeds. A port that peeled the tree back to
/// a commit would print a commit here and no one would notice.
#[test]
fn a_tree_ish_operand_pends_instead_of_walking() {
    let (repo, home) = fixture("treeish");
    let tree = oid(&repo, &home, "main^{tree}");
    let base = oid(&repo, &home, "main:base.txt");
    let second = oid(&repo, &home, "main:second.txt");
    let subtree = oid(&repo, &home, "main:sub");
    let s = oid(&repo, &home, "main:sub/s.txt");

    // No `--objects`: `handle_commit()`'s `if (!revs->tree_objects) return NULL;`
    // drops the object and the walk has nothing to do. Exit 0, no output.
    for spec in ["main^{tree}", "main:base.txt", "main:sub"] {
        let out = run(&repo, &home, &["rev-list", spec]);
        assert!(out.status.success(), "{spec}: {}", err_of(&out));
        assert_eq!(out_of(&out), "", "{spec}");
    }

    // With `--objects` the tree is walked, rooted at `pending->path` — empty for
    // a peel, the path itself for `main:sub`.
    let out = run(&repo, &home, &["rev-list", "--objects", "main^{tree}"]);
    assert_eq!(
        out_of(&out),
        format!("{tree} \n{base} base.txt\n{second} second.txt\n{subtree} sub\n{s} sub/s.txt\n")
    );
    let out = run(&repo, &home, &["rev-list", "--objects", "main:sub"]);
    assert_eq!(out_of(&out), format!("{subtree} sub\n{s} sub/s.txt\n"));
    let out = run(&repo, &home, &["rev-list", "--objects", "main:base.txt"]);
    assert_eq!(out_of(&out), format!("{base} base.txt\n"));

    // `traverse_non_commits()` runs after every commit, and the pending list is
    // in argument order — so the argv tree comes ahead of the commits' own trees
    // even when it is written first.
    let out = run(&repo, &home, &["rev-list", "--objects", "main^{tree}", "main"]);
    let text = out_of(&out);
    let lines: Vec<&str> = text.lines().collect();
    let commits = out_of(&run(&repo, &home, &["rev-list", "main"]));
    let commit_count = commits.lines().count();
    assert_eq!(lines.len(), commit_count + 6, "commits, then the object list:\n{text}");
    assert_eq!(lines[commit_count], format!("{tree} "), "the argv tree leads the object list");

    // An excluded tree marks its contents, so nothing survives — and it does so
    // whichever side of the interesting one it was written on, because
    // `mark_tree_contents_uninteresting()` runs while the list is still built.
    for args in [
        vec!["rev-list", "--objects", "^main^{tree}"],
        vec!["rev-list", "--objects", "^main^{tree}", "main^{tree}"],
        vec!["rev-list", "--objects", "main^{tree}", "^main^{tree}"],
    ] {
        let out = run(&repo, &home, &args);
        assert!(out.status.success(), "{args:?}: {}", err_of(&out));
        assert_eq!(out_of(&out), "", "{args:?}");
    }

    // `mark_edges_uninteresting()` walks the list `prepare_revision_walk()` left
    // behind. `^main main` leaves it empty, so nothing is marked and a tree named
    // alongside survives — while `^main main` itself still prints nothing.
    let out = run(&repo, &home, &["rev-list", "--objects", "^main", "main"]);
    assert_eq!(out_of(&out), "");
    let out = run(&repo, &home, &["rev-list", "--objects", "^main", "main^{tree}"]);
    assert_eq!(
        out_of(&out),
        format!("{tree} \n{base} base.txt\n{second} second.txt\n{subtree} sub\n{s} sub/s.txt\n")
    );
}

/// `add_index_objects_to_pending()`: every index blob under its index path, in
/// index order, and then — from the `TREE` extension — the cache-tree, root
/// first. The order is the assertion: a set comparison would pass on a port that
/// emitted the cache-tree ahead of the blobs, and a `HashMap` walk would pass on
/// one that emitted the blobs in an arbitrary order.
///
/// Only the blob half is pinned exactly. `do_add_index_objects_to_pending()`
/// reads the cache-tree out of the index's `TREE` extension, and this port's
/// `commit` does not write one — stock git run against a zvcs-built repository
/// prints the same three lines and no trees, so an exact expectation here would
/// be asserting a property of `commit`, not of this walk. What *is* checked is
/// that any tree lines follow every blob line, which is the ordering rule.
#[test]
fn indexed_objects_pends_the_index_then_its_cache_tree() {
    let (repo, home) = fixture("indexed");
    let base = oid(&repo, &home, "main:base.txt");
    let second = oid(&repo, &home, "main:second.txt");
    let s = oid(&repo, &home, "main:sub/s.txt");

    // Without `--objects` there is nothing to print, but the pending list is not
    // empty either — so this is not the "no revision given" usage error, which
    // would be exit 129 and a usage block.
    let out = run(&repo, &home, &["rev-list", "--indexed-objects"]);
    assert!(out.status.success(), "{}", err_of(&out));
    assert_eq!(out_of(&out), "");

    let out = run(&repo, &home, &["rev-list", "--objects", "--indexed-objects"]);
    let text = out_of(&out);
    let blobs = format!("{base} base.txt\n{second} second.txt\n{s} sub/s.txt\n");
    assert!(text.starts_with(&blobs), "index blobs, in index order, first:\n{text}");
    // Anything after them is cache-tree, and a cache-tree line is a bare id with
    // a directory path (or none, for the root) — never one of the index paths.
    for line in text[blobs.len()..].lines() {
        let name = line.split_once(' ').map(|(_, n)| n).unwrap_or("");
        assert!(
            !name.contains('.'),
            "only cache-tree entries follow the blobs, got {line:?}"
        );
    }
    // The index blobs are pending whether or not a revision was also named, and
    // they come after every commit.
    let out = run(&repo, &home, &["rev-list", "--objects", "--indexed-objects", "main"]);
    let text = out_of(&out);
    let commits = out_of(&run(&repo, &home, &["rev-list", "main"]));
    assert!(text.starts_with(&commits), "commits first:\n{text}");
    assert!(text[commits.len()..].starts_with(&blobs), "then the index blobs:\n{text}");
}

/// An annotated tag pends under the tag object's own *name field*, ahead of any
/// tree — the one pending shape that was already right, kept as a guard on the
/// generalised list.
#[test]
fn an_annotated_tag_still_pends_under_its_name() {
    let (repo, home) = fixture("tagpend");
    let tag = oid(&repo, &home, "atag");
    let out = run(&repo, &home, &["rev-list", "--objects", "atag"]);
    let text = out_of(&out);
    let tag_line = format!("{tag} atag\n");
    assert!(text.contains(&tag_line), "{text}");
    let tree = oid(&repo, &home, "main^{tree}");
    assert!(
        text.find(&tag_line).unwrap() < text.find(&format!("{tree} \n")).unwrap(),
        "the tag comes before the trees:\n{text}"
    );
}

/// `--exclude-hidden=<section>`: the section is validated, a second one is
/// refused, and the three *narrowed* ref selectors are refused outright because
/// their callback never sees the `refs/…` half a hideRefs pattern matches on.
#[test]
fn exclude_hidden_reads_hide_refs_and_refuses_the_narrowed_selectors() {
    let (repo, home) = fixture("hidden");
    git(&repo, &home, &["checkout", "-q", "-b", "hidden"]);
    std::fs::write(repo.join("h.txt"), "h\n").unwrap();
    git(&repo, &home, &["add", "h.txt"]);
    git(&repo, &home, &["commit", "-q", "-m", "hidden-only"]);
    git(&repo, &home, &["checkout", "-q", "main"]);
    git(&repo, &home, &["config", "--add", "receive.hideRefs", "refs/heads/hidden"]);

    // The whole point: the hidden branch's commit disappears from `--all`. That
    // is a different *commit set* at exit 0, not a different message.
    let all = out_of(&run(&repo, &home, &["rev-list", "--all"]));
    let filtered = out_of(&run(&repo, &home, &["rev-list", "--exclude-hidden=receive", "--all"]));
    assert_eq!(all.lines().count(), filtered.lines().count() + 1, "all:\n{all}");
    let hidden_tip = oid(&repo, &home, "hidden");
    assert!(all.contains(&hidden_tip));
    assert!(!filtered.contains(&hidden_tip));
    // `uploadpack.hideRefs` is a different key, so the same walk is unfiltered.
    let other = out_of(&run(&repo, &home, &["rev-list", "--exclude-hidden=uploadpack", "--all"]));
    assert_eq!(other.lines().count(), all.lines().count());

    for (selector, name) in [("--branches", "--branches"), ("--tags", "--tags"), ("--remotes", "--remotes")] {
        let out = run(&repo, &home, &["rev-list", "--exclude-hidden=receive", selector]);
        assert_eq!(code_of(&out), 129, "{selector}");
        assert!(
            err_of(&out).starts_with(&format!(
                "error: options '--exclude-hidden' and '{name}' cannot be used together\n"
            )),
            "{selector}: {}",
            err_of(&out)
        );
    }
    // `--all` and `--glob=` are *not* refused: they see the full refname.
    let out = run(&repo, &home, &["rev-list", "--exclude-hidden=receive", "--glob=refs/heads/*"]);
    assert!(out.status.success(), "{}", err_of(&out));
    assert!(!out_of(&out).contains(&hidden_tip));

    let out = run(&repo, &home, &["rev-list", "--exclude-hidden=bogus", "--all"]);
    assert_eq!(code_of(&out), 128);
    assert_eq!(err_of(&out), "fatal: unsupported section for hidden refs: bogus\n");
    let out = run(
        &repo,
        &home,
        &["rev-list", "--exclude-hidden=receive", "--exclude-hidden=receive", "--all"],
    );
    assert_eq!(code_of(&out), 128);
    assert_eq!(err_of(&out), "fatal: --exclude-hidden= passed more than once\n");
}

/// `handle_ref_opt()`'s pattern and exclusion rules in `rev-parse`.
///
/// The two assertions that catch real mistakes are the trimming and the
/// clearing: `--exclude` is matched against the name *after* the namespace is
/// stripped, so `--exclude=refs/heads/side` excludes nothing from `--branches`;
/// and the list is cleared by the walk that consumed it, so a second `--branches`
/// sees no exclusions at all.
#[test]
fn rev_parse_ref_options_trim_before_excluding_and_clear_after() {
    let (repo, home) = fixture("refopts");
    git(&repo, &home, &["branch", "side"]);
    git(&repo, &home, &["update-ref", "refs/remotes/origin/main", "main"]);

    for (args, want) in [
        (vec!["--symbolic", "--branches"], "main\nside\n"),
        (vec!["--symbolic", "--tags"], "atag\n"),
        (vec!["--symbolic", "--remotes"], "origin/main\n"),
        // A pattern with no glob special gains an implied `/*`, so `--glob=heads`
        // is `refs/heads/*` and `--branches=nope` selects the branches *below*
        // `nope/` rather than a branch called `nope`.
        (vec!["--symbolic", "--glob=heads"], "refs/heads/main\nrefs/heads/side\n"),
        (vec!["--symbolic", "--glob=refs/heads/*"], "refs/heads/main\nrefs/heads/side\n"),
        (vec!["--symbolic", "--branches=nope"], ""),
        // `wildmatch(pattern, full_refname, 0)` — no `WM_PATHNAME`, so `*` crosses
        // a `/` and `--remotes=or*` really does reach `origin/main`.
        (vec!["--symbolic", "--remotes=or*"], "origin/main\n"),
        // The exclusion is tested against the trimmed name.
        (vec!["--symbolic", "--exclude=side", "--branches"], "main\n"),
        (vec!["--symbolic", "--exclude=refs/heads/side", "--branches"], "main\nside\n"),
        // …and against the full name for `--all`, which trims nothing.
        (
            vec!["--symbolic", "--exclude=refs/heads/side", "--all"],
            "refs/heads/main\nrefs/remotes/origin/main\nrefs/tags/atag\n",
        ),
        // `handle_ref_opt()` ends in `clear_ref_exclusions()`.
        (vec!["--symbolic", "--exclude=side", "--branches", "--branches"], "main\nmain\nside\n"),
    ] {
        let mut argv = vec!["rev-parse"];
        argv.extend(args.iter().copied());
        let out = run(&repo, &home, &argv);
        assert!(out.status.success(), "{argv:?}: {}", err_of(&out));
        assert_eq!(out_of(&out), want, "{argv:?}");
    }

    // `--tags` reports an annotated tag's own object id, not the commit.
    let tag = oid(&repo, &home, "atag");
    assert_eq!(out_of(&run(&repo, &home, &["rev-parse", "--tags"])), format!("{tag}\n"));
}

/// `branch.<name>.remote = .`: the upstream is `branch.<name>.merge` itself, a
/// local ref, so there is no remote-tracking name to look up. All three of the
/// resolution, the full name and the shortened name have to come from the same
/// place.
#[test]
fn a_dot_remote_resolves_the_upstream_to_a_local_ref() {
    let (repo, home) = fixture("dotremote");
    git(&repo, &home, &["branch", "side"]);
    git(&repo, &home, &["config", "branch.main.remote", "."]);
    git(&repo, &home, &["config", "branch.main.merge", "refs/heads/side"]);
    let side = oid(&repo, &home, "side");

    for spec in ["main@{u}", "main@{upstream}", "main@{U}"] {
        let out = run(&repo, &home, &["rev-parse", spec]);
        assert!(out.status.success(), "{spec}: {}", err_of(&out));
        assert_eq!(out_of(&out).trim_end(), side, "{spec}");
    }
    assert_eq!(
        out_of(&run(&repo, &home, &["rev-parse", "--symbolic-full-name", "main@{u}"])),
        "refs/heads/side\n"
    );
    assert_eq!(
        out_of(&run(&repo, &home, &["rev-parse", "--abbrev-ref", "main@{u}"])),
        "side\n"
    );
    // A branch with no upstream at all still says so.
    let out = run(&repo, &home, &["rev-parse", "side@{u}"]);
    assert_eq!(code_of(&out), 128);
    assert_eq!(err_of(&out), "fatal: no upstream configured for branch 'side'\n");
}

/// The excluded side of the same rule, which is where the *quiet* failure lives.
///
/// `handle_revision_arg()` sets `revs->rev_input_given` as soon as the operand
/// resolves, whichever side it lands on; `handle_commit()` then drops the tree
/// and leaves nothing behind. So `git log ^main^{tree}` has revision input, no
/// tips, and prints nothing — where a port that forgot the flag falls back to
/// `revs->def` and prints the *whole history* at exit 0.
#[test]
fn an_excluded_tree_is_revision_input_with_no_tip() {
    let (repo, home) = fixture("negtree");
    for args in [
        vec!["log", "--oneline", "^main^{tree}"],
        vec!["log", "--oneline", "^main:base.txt"],
        vec!["shortlog", "^main^{tree}"],
        vec!["shortlog", "main^{tree}"],
        vec!["shortlog", "main:base.txt"],
        vec!["rev-list", "^main^{tree}"],
    ] {
        let out = run(&repo, &home, &args);
        assert!(out.status.success(), "{args:?}: {}", err_of(&out));
        assert_eq!(out_of(&out), "", "{args:?} must not fall back to HEAD");
    }
    // Sanity: the same verbs do print when the default really is in play, so the
    // assertions above are not passing for want of any output at all.
    assert_ne!(out_of(&run(&repo, &home, &["log", "--oneline"])), "");
    assert_ne!(out_of(&run(&repo, &home, &["shortlog", "HEAD"])), "");
}
