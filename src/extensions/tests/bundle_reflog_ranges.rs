//! Range operands and boundary prerequisites for `git bundle create` and
//! `git reflog show` — the two verbs that reach `setup_revisions()` without a
//! `log`-shaped walk behind them, and so each carry their own copy of its
//! grammar.
//!
//! Four rules are pinned here, all of them measured from git 2.55.0 first:
//!
//! 1. **Prerequisite order.** `create_boundary_commit_list()` (revision.c) drains
//!    `revs->boundary_commits` with `commit_list_insert()`, which *prepends*, and
//!    then runs `sort_in_topological_order(&revs->commits, revs->sort_order)`
//!    unconditionally. Both halves are observable and neither implies the other:
//!    two boundary commits with no parent link between them come out in the
//!    reverse of the order the walk met them, while a boundary commit that is
//!    another's parent is dragged behind its child by the sort no matter which
//!    order they were met in.
//! 2. **Tag peeling.** `prepare_revision_walk()` puts every pending entry through
//!    `handle_commit()`, whose tag loop peels to the commit and carries the flags
//!    down (`object->flags |= flags`). A port that walks from the tag object
//!    itself finds no history at all and reports a complete one.
//! 3. **`<a>...<b>`.** `handle_dotdot()` consumes the third dot; a split on `..`
//!    alone reads `<a>...<b>` as `<a>` against `.<b>` and can only fail. The
//!    merge bases are pended *first* and under `oid_to_hex()`, which is what
//!    makes `git reflog show <a>...<b>` name a 40-hex rather than an endpoint —
//!    and what leaves both endpoints interesting, so a pair with no merge base at
//!    all walks two reflogs and exits 0.
//! 4. **A bare `..`.** `handle_revision_arg_1()` refuses it before
//!    `handle_dotdot()` ever runs (revision.c:2164), so it is the pathspec for
//!    the parent directory and the diagnostic comes from `pathspec.c`, not from
//!    the revision parser.
//!
//! Every expectation is asserted against the port unconditionally, so the file is
//! meaningful on a machine with no stock git; where one is installed it is
//! additionally diffed, which is what catches a git that changed the wording out
//! from under the port.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A stock git to cross-check against, or `None` on a machine without one.
///
/// Resolved by absolute path, never through `PATH`: zvcs installs itself as
/// `git`, so a `PATH` lookup would quietly make the port its own oracle.
fn stock_git() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return Path::new(&p).exists().then_some(p);
    }
    ["/opt/homebrew/bin/git", "/usr/local/bin/git", "/usr/bin/git"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .map(str::to_owned)
}

fn run(bin: &str, repo: &Path, home: &Path, date: &str, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("GIT_PAGER", "cat")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap_or_else(|e| panic!("run {bin} {args:?}: {e}"))
}

/// ```text
/// * O1          (branch other, a second root)
/// * M           (branch main, merge of C and S1)
/// |\
/// | * S1        (branch side)
/// * | C
/// |/
/// * B
/// * A
/// ```
///
/// plus an annotated tag `v1` on `M`. Every commit touches a file of its own, so
/// the merge is clean, and every commit gets a distinct timestamp, so the
/// date-ordered walk the boundary scan runs has no ties to break.
struct Repo {
    dir: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-bundle-reflog-ranges-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = Repo { dir: root.join("repo"), home: root.join("home") };
        std::fs::create_dir_all(&repo.dir).unwrap();
        std::fs::create_dir_all(&repo.home).unwrap();
        repo.ok(0, &["init", "-q", "-b", "main", "."]);
        repo.commit(1, "A");
        repo.commit(2, "B");
        repo.commit(3, "C");
        repo.ok(4, &["checkout", "-q", "-b", "side", "main~1"]);
        repo.commit(5, "S1");
        repo.ok(6, &["checkout", "-q", "main"]);
        repo.ok(7, &["merge", "-q", "--no-ff", "-m", "M", "side"]);
        repo.ok(8, &["tag", "-a", "v1", "-m", "ann", "main"]);
        repo.ok(9, &["checkout", "-q", "--orphan", "other"]);
        for name in ["A", "B", "C", "S1"] {
            let _ = std::fs::remove_file(repo.dir.join(format!("f_{name}")));
        }
        repo.commit(10, "O1");
        repo.ok(11, &["checkout", "-q", "main"]);
        repo
    }

    /// `1700000000 + 60 * step`, so the commits are ordered and reproducible.
    fn at(step: u32) -> String {
        format!("{} +0000", 1_700_000_000u64 + u64::from(step) * 60)
    }

    fn git(&self, args: &[&str]) -> Output {
        run(BIN, &self.dir, &self.home, &Self::at(99), args)
    }

    fn ok(&self, step: u32, args: &[&str]) {
        let out = run(BIN, &self.dir, &self.home, &Self::at(step), args);
        assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
    }

    fn commit(&self, step: u32, name: &str) {
        std::fs::write(self.dir.join(format!("f_{name}")), format!("{name}\n")).unwrap();
        let file = format!("f_{name}");
        self.ok(step, &["add", &file]);
        self.ok(step, &["commit", "-q", "-m", name]);
    }

    fn rev(&self, spec: &str) -> String {
        let out = self.git(&["rev-parse", spec]);
        assert!(out.status.success(), "rev-parse {spec}: {}", text(&out.stderr));
        text(&out.stdout).trim().to_owned()
    }

    /// The worktree as `absolute_path(repo_get_work_tree())` renders it —
    /// symlinks resolved, because git's copy came from `setup_work_tree()`'s
    /// `xgetcwd()`. On macOS that is what turns `/var/…` into `/private/var/…`.
    fn worktree(&self) -> String {
        std::fs::canonicalize(&self.dir).unwrap().display().to_string()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The bundle header: everything `create_bundle()` writes before the blank line
/// that separates it from the pack.
fn header(stdout: &[u8]) -> String {
    let end = stdout
        .windows(2)
        .position(|w| w == b"\n\n")
        .unwrap_or_else(|| panic!("no bundle header terminator in {} bytes", stdout.len()));
    text(&stdout[..=end + 1])
}

/// Assert the port's bundle header, and that stock writes the same one.
fn expect_header(repo: &Repo, args: &[&str], want: &str) {
    let ours = repo.git(args);
    assert!(
        ours.status.success(),
        "git {args:?} failed: {}",
        text(&ours.stderr)
    );
    assert_eq!(header(&ours.stdout), want, "bundle header of `git {}`", args.join(" "));
    if let Some(stock) = stock_git() {
        let theirs = run(&stock, &repo.dir, &repo.home, &Repo::at(99), args);
        assert_eq!(
            header(&theirs.stdout),
            want,
            "the pinned header no longer matches the installed stock git for `git {}`",
            args.join(" ")
        );
    }
}

/// Assert the port's stdout/stderr/exit, and that stock agrees.
fn expect(repo: &Repo, args: &[&str], stdout: &str, stderr: &str, code: i32) {
    let ours = repo.git(args);
    let got = (text(&ours.stdout), text(&ours.stderr), ours.status.code().unwrap_or(-1));
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        (stdout, stderr, code),
        "`git {}`",
        args.join(" ")
    );
    if let Some(stock) = stock_git() {
        let theirs = run(&stock, &repo.dir, &repo.home, &Repo::at(99), args);
        assert_eq!(
            (
                text(&theirs.stdout).as_str(),
                text(&theirs.stderr).as_str(),
                theirs.status.code().unwrap_or(-1)
            ),
            (stdout, stderr, code),
            "the pinned expectation no longer matches the installed stock git for `git {}`",
            args.join(" ")
        );
    }
}

/// `create_boundary_commit_list()`'s two steps, one case each.
///
/// `main ^main~1 ^side` excludes `C` and `S1`, which are `M`'s two parents and
/// have no parent link *between* them: the walk meets them in `M`'s parent order,
/// `C` then `S1`, and git prints them the other way round. Nothing but
/// `commit_list_insert()`'s prepend produces that.
///
/// `main side ^main~1` excludes `C`, which makes the boundary `C` (met from `M`)
/// and `B` (met from `S1`) — and `B` is `C`'s parent. Now the reversal alone
/// would print `B` before its own child; `sort_in_topological_order()` is what
/// puts them back. So the two cases fail on opposite halves of the port.
#[test]
fn boundary_prerequisites_are_reversed_then_topologically_sorted() {
    let repo = Repo::new("boundary");
    let (b, c, s1, m) =
        (repo.rev("main~2"), repo.rev("main~1"), repo.rev("side"), repo.rev("main"));

    expect_header(
        &repo,
        &["bundle", "create", "-", "main", "^main~1", "^side"],
        &format!("# v2 git bundle\n-{s1} S1\n-{c} C\n{m} refs/heads/main\n\n"),
    );
    expect_header(
        &repo,
        &["bundle", "create", "-", "main", "side", "^main~1"],
        &format!("# v2 git bundle\n-{c} C\n-{b} B\n{m} refs/heads/main\n{s1} refs/heads/side\n\n"),
    );
}

/// `handle_commit()`'s tag loop, from both ends of the operand.
///
/// `v1` is an annotated tag on `M`; `v1^` is `C`. Walking from the tag object
/// rather than from `M` reaches no commit at all, so the whole exclusion is lost
/// and the bundle claims to record a complete history. The ref line is the other
/// half of the same rule: `revs_copy.pending` keeps the *tag*, so the header
/// carries the tag's own id under `refs/tags/v1` and not `M`.
#[test]
fn annotated_tag_tip_is_peeled_before_the_boundary_walk() {
    let repo = Repo::new("tagpeel");
    let (b, c, v1) = (repo.rev("main~2"), repo.rev("main~1"), repo.rev("v1"));
    assert_ne!(v1, repo.rev("main"), "v1 must be an annotated tag, not a lightweight one");

    expect_header(
        &repo,
        &["bundle", "create", "-", "v1", "^v1^"],
        &format!("# v2 git bundle\n-{c} C\n-{b} B\n{v1} refs/tags/v1\n\n"),
    );
}

/// `<a>...<b>` for `bundle create`: the merge bases are excluded and the two
/// endpoints are not.
///
/// `side...main` has `S1` as its only merge base, so `S1` is both endpoint `a`
/// and a base — `UNINTERESTING` wins, which is why `refs/heads/side` is missing
/// from a header that still lists `refs/heads/main`.
///
/// `other...main` crosses two roots and so has no merge base at all: nothing is
/// excluded, there are no prerequisites, and both refs are written in
/// `handle_dotdot_1()`'s order, left endpoint first.
#[test]
fn symmetric_difference_is_a_range_for_bundle() {
    let repo = Repo::new("symmetric");
    let (b, s1, m, other) =
        (repo.rev("main~2"), repo.rev("side"), repo.rev("main"), repo.rev("other"));

    expect_header(
        &repo,
        &["bundle", "create", "-", "side...main"],
        &format!("# v2 git bundle\n-{s1} S1\n-{b} B\n{m} refs/heads/main\n\n"),
    );
    expect_header(
        &repo,
        &["bundle", "create", "-", "other...main"],
        &format!("# v2 git bundle\n{other} refs/heads/other\n{m} refs/heads/main\n\n"),
    );
}

/// `add_reflog_for_walk()`'s refusal, reached through `handle_dotdot_1()`:
///
/// ```c
/// if (commit->object.flags & UNINTERESTING)
///         die("cannot walk reflogs for %s", name);
/// ```
///
/// `<a>..<b>` excludes its left endpoint, so the name in the message is that
/// endpoint as typed — `"HEAD"` when it was written empty. `<a>...<b>` excludes
/// the merge bases instead and pends them *before* either endpoint, under
/// `oid_to_hex()`, so the name is a full-length object id even though both
/// operands were ref names.
#[test]
fn reflog_range_dies_naming_the_excluded_endpoint() {
    let repo = Repo::new("reflogrange");
    let s1 = repo.rev("side");

    for (token, name) in [
        ("main~1..main", "main~1".to_owned()),
        ("side..main", "side".to_owned()),
        ("..main", "HEAD".to_owned()),
        ("main..", "main".to_owned()),
        // The merge base of `side` and `main` is `S1` itself.
        ("side...main", s1.clone()),
    ] {
        expect(
            &repo,
            &["reflog", "show", token],
            "",
            &format!("fatal: cannot walk reflogs for {name}\n"),
            128,
        );
    }
}

/// The branch of `handle_dotdot_1()` that is easy to mistake for dead code:
/// `a_flags = flags | SYMMETRIC_LEFT` and `b_flags = flags` leave *both*
/// endpoints interesting, and only `get_merge_bases()`' output carries
/// `flags_exclude`. Across two roots there is no such output, so nothing is
/// excluded, nothing dies, and `git reflog show <a>...<b>` walks two reflogs in
/// pending order — left endpoint first.
#[test]
fn reflog_symmetric_without_a_merge_base_walks_both_reflogs() {
    let repo = Repo::new("nobase");
    let (o1, s1, b) = (repo.rev("other"), repo.rev("side"), repo.rev("main~2"));
    let short = |id: &String| id[..7].to_owned();

    expect(
        &repo,
        &["reflog", "show", "other...side"],
        &format!(
            "{} other@{{0}}: commit (initial): O1\n\
             {} side@{{0}}: commit: S1\n\
             {} side@{{1}}: branch: Created from main~1\n",
            short(&o1),
            short(&s1),
            short(&b),
        ),
        "",
        0,
    );
}

/// `if (!cant_be_filename && !strcmp(arg, "..")) return -1;` (revision.c:2164),
/// and then `setup_revisions()`' filename fallback:
///
/// ```c
/// for (j = i; j < argc; j++)
///         verify_filename(revs->prefix, argv[j], j == i);
/// strvec_pushv(&prune_data, argv + i);
/// ```
///
/// So a bare `..` is never a range: it becomes prune data and dies in
/// `pathspec.c`, and a second operand behind it dies in `verify_filename()` with
/// the *non*-`diagnose_misspelt_rev` wording, because only the first token was
/// ever tried as a revision.
#[test]
fn bare_dotdot_is_the_parent_directory_pathspec() {
    let repo = Repo::new("dotdot");
    let outside = format!(
        "fatal: ..: '..' is outside repository at '{}'\n",
        repo.worktree()
    );
    let missing = "fatal: nosuchfile: no such path in the working tree.\n\
                   Use 'git <command> -- <path>...' to specify paths that do not exist locally.\n";

    expect(&repo, &["reflog", "show", ".."], "", &outside, 128);
    expect(&repo, &["bundle", "create", "-", ".."], "", &outside, 128);
    expect(&repo, &["reflog", "show", "..", "nosuchfile"], "", missing, 128);
    expect(&repo, &["bundle", "create", "-", "..", "nosuchfile"], "", missing, 128);
}
