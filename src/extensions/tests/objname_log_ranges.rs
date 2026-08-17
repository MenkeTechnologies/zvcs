//! `A..B` / `A...B` whose endpoint is a well-formed object name the repository
//! does not have.
//!
//! `get_oid_basic()` (object-name.c) returns an object id for any string of
//! exactly `hexsz` hex digits **without consulting the object database**, so
//! `handle_dotdot_1()` (revision.c) gets past `get_oid_with_context()` on both
//! endpoints and only fails at `parse_object()`. That is a different death from
//! an endpoint that never resolved at all:
//!
//! | endpoint                    | stock 2.55.0                                          |
//! |-----------------------------|-------------------------------------------------------|
//! | full-length hex, absent     | `fatal: Invalid revision range <token>`                |
//! | ditto, `...`                | `fatal: Invalid symmetric difference expression <tok>` |
//! | anything that never resolves| `fatal: ambiguous argument '<token>': …`               |
//!
//! Both name the WHOLE token, never the endpoint that failed — which is why the
//! control cases below pin the token text as written and not just the wording.
//!
//! Resolving through the object database alone collapses the first two rows into
//! the third; these cases exist to keep them apart. Every expectation here is the
//! verbatim output of git 2.55.0, and is asserted against the port unconditionally
//! so the file is meaningful on a machine (CI) with no stock git installed; where
//! one *is* installed it is additionally diffed, which is what catches a git that
//! changed the wording out from under the port.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A well-formed, absent object name: 40 hex digits, nothing behind them.
const ABSENT: &str = "0123456789012345678901234567890123456789";
/// The control token: not an object name at all, so it must keep taking the
/// "ambiguous argument" path no matter what happens to the full-hex rule.
const UNRESOLVABLE: &str = "nosuchthing";

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

fn run(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> Output {
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
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
        .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
        .output()
        .unwrap_or_else(|e| panic!("run {bin} {args:?}: {e}"))
}

struct Repo {
    dir: PathBuf,
    home: PathBuf,
}

impl Repo {
    /// Two commits, which is enough for `HEAD~1..HEAD` to be a range that walks.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-objname-ranges-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = Repo { dir: root.join("repo"), home: root.join("home") };
        std::fs::create_dir_all(&repo.dir).unwrap();
        std::fs::create_dir_all(&repo.home).unwrap();
        assert!(repo.git(&["init", "-q", "-b", "main", "."]).status.success(), "init");
        for name in ["one", "two"] {
            std::fs::write(repo.dir.join(format!("{name}.txt")), format!("{name}\n")).unwrap();
            let file = format!("{name}.txt");
            assert!(repo.git(&["add", &file]).status.success(), "add {name}");
            let out = repo.git(&["commit", "-q", "-m", name]);
            assert!(out.status.success(), "commit {name}: {}", text(&out.stderr));
        }
        repo
    }

    fn git(&self, args: &[&str]) -> Output {
        run(BIN, &self.dir, &self.home, args)
    }

    /// `(stderr, exit code)` of the port, and the same from stock when installed.
    fn both(&self, args: &[&str]) -> (String, i32, Option<(String, i32)>) {
        let ours = self.git(args);
        let theirs = stock_git().map(|g| {
            let o = run(&g, &self.dir, &self.home, args);
            (text(&o.stderr), o.status.code().unwrap_or(-1))
        });
        (text(&ours.stderr), ours.status.code().unwrap_or(-1), theirs)
    }

    fn rev_parse(&self, spec: &str) -> String {
        let out = self.git(&["rev-parse", spec]);
        assert!(out.status.success(), "rev-parse {spec}: {}", text(&out.stderr));
        text(&out.stdout).trim().to_owned()
    }

    /// An annotated tag whose target is `HEAD^{tree}`, and its own id.
    ///
    /// This is the shape that tells the two halves of
    /// `error("object %s is a %s, not a %s")` apart: `oid_to_hex(oid)` is the
    /// operand git was handed — the **tag** — while `type_name(type)` is what
    /// `peel_object_ext()` arrived at — a **tree**. A bare `HEAD^{tree}` makes
    /// the two ids coincide and so cannot catch a port that names the peeled
    /// object instead of the operand.
    fn tree_tag(&self, name: &str) -> String {
        let tree = self.rev_parse("HEAD^{tree}");
        let out = self.git(&["tag", "-a", name, &tree, "-m", "x"]);
        assert!(out.status.success(), "tag {name}: {}", text(&out.stderr));
        self.rev_parse(name)
    }

    /// `(stdout, stderr, exit code)` of the port.
    fn run3(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.git(args);
        (text(&out.stdout), text(&out.stderr), out.status.code().unwrap_or(-1))
    }

    /// The working tree as `absolute_path(repo_get_work_tree())` renders it —
    /// symlinks resolved, because git's copy came from `setup_work_tree()`'s
    /// `xgetcwd()`. On macOS that is what turns `/var/…` into `/private/var/…`.
    fn worktree(&self) -> String {
        std::fs::canonicalize(&self.dir).unwrap().display().to_string()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Assert the port's stderr and exit code, and that stock agrees when present.
fn expect(repo: &Repo, args: &[&str], stderr: &str, code: i32) {
    let (got, got_code, stock) = repo.both(args);
    assert_eq!(got, stderr, "stderr of `git {}`", args.join(" "));
    assert_eq!(got_code, code, "exit of `git {}`", args.join(" "));
    if let Some((want, want_code)) = stock {
        assert_eq!(
            (want.as_str(), want_code),
            (stderr, code),
            "the pinned expectation no longer matches the installed stock git for `git {}`",
            args.join(" ")
        );
    }
}

/// The three-line `verify_filename()` text, for a token that resolves to nothing.
fn ambiguous(token: &str) -> String {
    format!(
        "fatal: ambiguous argument '{token}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    )
}

#[test]
fn absent_full_hex_endpoint_is_an_invalid_range_not_an_ambiguous_argument() {
    let repo = Repo::new("range");
    for token in [
        format!("{ABSENT}..HEAD"),
        format!("HEAD..{ABSENT}"),
        format!("{ABSENT}..{ABSENT}"),
        // Empty endpoints default to HEAD inside `handle_dotdot_1`, so these are
        // ranges too — and the message still names the token as written.
        format!("..{ABSENT}"),
        format!("{ABSENT}.."),
    ] {
        let want = format!("fatal: Invalid revision range {token}\n");
        expect(&repo, &["log", &token], &want, 128);
    }
}

#[test]
fn absent_full_hex_endpoint_of_a_symmetric_difference_has_its_own_wording() {
    let repo = Repo::new("symmetric");
    for token in [format!("{ABSENT}...HEAD"), format!("HEAD...{ABSENT}")] {
        let want = format!("fatal: Invalid symmetric difference expression {token}\n");
        expect(&repo, &["log", &token], &want, 128);
    }
}

/// `cmd_show`, `cmd_rev_list` and `cmd_bundle` all reach `setup_revisions()`, so
/// the range diagnosis has to be the same in every one of them.
#[test]
fn every_command_sharing_setup_revisions_reports_the_range_the_same_way() {
    let repo = Repo::new("commands");
    let range = format!("{ABSENT}..HEAD");
    let want = format!("fatal: Invalid revision range {range}\n");
    expect(&repo, &["log", &range], &want, 128);
    expect(&repo, &["log", "--oneline", &range], &want, 128);
    expect(&repo, &["rev-list", &range], &want, 128);
    expect(&repo, &["show", &range], &want, 128);
    expect(&repo, &["bundle", "create", "-", &range], &want, 128);
    expect(&repo, &["whatchanged", &range], &want, 128);
    expect(&repo, &["shortlog", &range], &want, 128);
}

/// CONTROL. A token that resolves to nothing makes `handle_dotdot_1()` return -1
/// before `parse_object()`, so it keeps the ordinary "ambiguous argument" fatal —
/// including when it is paired with an absent full hex on the other side. This is
/// what proves the fix keys on the full-hex rule and not on "contains `..`".
#[test]
fn a_token_that_resolves_to_nothing_keeps_the_ambiguous_argument_fatal() {
    let repo = Repo::new("control-unresolvable");
    for token in [
        format!("{UNRESOLVABLE}..HEAD"),
        format!("HEAD..{UNRESOLVABLE}"),
        format!("{UNRESOLVABLE}...HEAD"),
        format!("{UNRESOLVABLE}..{ABSENT}"),
        format!("{ABSENT}..{UNRESOLVABLE}"),
        UNRESOLVABLE.to_owned(),
    ] {
        expect(&repo, &["log", &token], &ambiguous(&token), 128);
    }
}

/// CONTROL. `get_oid_basic()`'s first branch needs *exactly* `hexsz` digits, so a
/// 7-digit prefix of the same absent id is an ordinary unresolvable name.
#[test]
fn a_short_hex_prefix_is_not_the_full_hex_rule() {
    let repo = Repo::new("control-short-hex");
    for token in ["0123456..HEAD", "0123456...HEAD", "0123456"] {
        expect(&repo, &["log", token], &ambiguous(token), 128);
    }
}

/// CONTROL. `handle_dotdot()` runs on the argument as written and `get_oid_basic()`
/// has no reading for a leading `^`, so `^<hex>..HEAD` fails endpoint resolution
/// and falls through to `die("bad revision '%s'")` — while a bare `^<hex>` strips
/// the caret first and dies inside `get_reference()` naming the id alone.
#[test]
fn a_caret_keeps_a_range_out_of_the_dotdot_path() {
    let repo = Repo::new("control-caret");
    let token = format!("^{ABSENT}..HEAD");
    expect(&repo, &["log", &token], &format!("fatal: bad revision '{token}'\n"), 128);

    let bare = format!("fatal: bad object {ABSENT}\n");
    expect(&repo, &["log", ABSENT], &bare, 128);
    expect(&repo, &["log", &format!("^{ABSENT}")], &bare, 128);
    expect(&repo, &["rev-list", ABSENT], &bare, 128);
    expect(&repo, &["show", ABSENT], &bare, 128);
}

/// CONTROL. `A...B` additionally runs both ends through `lookup_commit_reference()`,
/// whose `object_as_type()` writes an `error:` line of its own before the fatal —
/// and `A..B` does not, so a tree on the left of a plain range still walks.
#[test]
fn a_symmetric_difference_rejects_a_present_non_commit_endpoint() {
    let repo = Repo::new("control-non-commit");
    let tree = repo.rev_parse("HEAD^{tree}");
    let token = format!("{tree}...HEAD");
    let want = format!(
        "error: object {tree} is a tree, not a commit\n\
         fatal: Invalid symmetric difference expression {token}\n"
    );
    expect(&repo, &["log", &token], &want, 128);
    expect(&repo, &["rev-list", &token], &want, 128);

    let plain = format!("{tree}..HEAD");
    let out = repo.git(&["log", "--oneline", &plain]);
    assert_eq!(text(&out.stderr), "", "`git log {plain}` must not diagnose the tree");
    assert!(out.status.success(), "`git log {plain}` exits 0");
}

/// CONTROL. Ranges that are entirely fine are untouched, and the leftmost bad
/// argument is still the one reported when several are present.
#[test]
fn valid_ranges_and_argument_order_are_unaffected() {
    let repo = Repo::new("control-valid");
    for token in ["HEAD~1..HEAD", "HEAD~1...HEAD"] {
        let out = repo.git(&["log", "--oneline", token]);
        assert_eq!(text(&out.stderr), "", "`git log --oneline {token}`");
        assert_eq!(text(&out.stdout).lines().count(), 1, "`git log --oneline {token}` walks one");
    }
    // `setup_revisions()` reads argv left to right and dies on the first argument
    // it cannot use, so an unresolvable word before a bad range wins.
    expect(
        &repo,
        &["log", UNRESOLVABLE, &format!("{ABSENT}...HEAD")],
        &ambiguous(UNRESOLVABLE),
        128,
    );
}

/// The `A...B` type check names the **operand**, not what it peeled to.
///
/// `handle_dotdot_1()` hands `lookup_commit_reference()` the id
/// `get_oid_with_context()` produced, and that function reports
/// `oid_to_hex(oid)` — the id it was given — alongside `type_name(type)`, the
/// type it peeled *to*. An annotated tag pointing at a tree is the only shape
/// where those two disagree, which is why it is the one used here: two of the
/// private copies this module replaced had drifted, one naming the peeled id and
/// the other the unpeeled type.
///
/// Every command that reaches `setup_revisions()` has to say the same thing.
#[test]
fn a_tag_of_a_tree_is_named_by_the_tag_id_in_every_command() {
    let repo = Repo::new("tree-tag");
    let tag = repo.tree_tag("treetag");
    let tree = repo.rev_parse("HEAD^{tree}");
    assert_ne!(tag, tree, "the fixture must distinguish the operand from the peel");

    for token in ["treetag...HEAD", "HEAD...treetag"] {
        let want = format!(
            "error: object {tag} is a tree, not a commit\n\
             fatal: Invalid symmetric difference expression {token}\n"
        );
        expect(&repo, &["log", token], &want, 128);
        expect(&repo, &["rev-list", token], &want, 128);
        expect(&repo, &["show", token], &want, 128);
        expect(&repo, &["format-patch", token], &want, 128);
        expect(&repo, &["whatchanged", token], &want, 128);
        expect(&repo, &["shortlog", token], &want, 128);
        expect(&repo, &["replay", "--onto", "HEAD", token], &want, 128);
    }
}

/// `git show` must not *succeed* on an operand git rejects.
///
/// This is the sharpest form of the bug: `show` resolved `<tag-of-a-tree>...HEAD`
/// through gitoxide's `rev_parse()`, which peels a symmetric difference on its
/// own and so never met the type check at all — the command printed a commit and
/// exited 0 where git prints two diagnostics and exits 128. A stderr-only
/// assertion cannot see that, so stdout is pinned empty here.
#[test]
fn show_produces_no_output_for_a_symmetric_difference_git_rejects() {
    let repo = Repo::new("show-symmetric");
    let tag = repo.tree_tag("treetag");
    for token in ["treetag...HEAD", "HEAD...treetag", "HEAD^{tree}...HEAD"] {
        let (out, err, code) = repo.run3(&["show", token]);
        assert_eq!(out, "", "`git show {token}` must print nothing");
        assert_eq!(code, 128, "`git show {token}` exit");
        assert!(
            err.starts_with("error: object ") && err.contains(" is a tree, not a commit\n"),
            "`git show {token}` stderr: {err}"
        );
        assert!(
            err.ends_with(&format!("fatal: Invalid symmetric difference expression {token}\n")),
            "`git show {token}` stderr: {err}"
        );
    }
    // The tag id, not the tree it points at, for the spelling that distinguishes them.
    let (_, err, _) = repo.run3(&["show", "treetag...HEAD"]);
    assert!(err.contains(&tag), "the operand id is what git names: {err}");
}

/// CONTROL for the row above. `range-diff` does **not** reach that check.
///
/// `cmd_range_diff` never hands `<a>...<b>` to `setup_revisions()`: it rewrites
/// the operand into the two plain ranges `<b>..<a>` and `<a>..<b>` and runs a
/// `git log` over each (`builtin/range-diff.c`). `handle_dotdot_1()` type-checks
/// only the symmetric form, so a tree endpoint sails through both of those and
/// the command *succeeds* — stock 2.55.0 exits 0 and prints the range-diff.
///
/// The two commands therefore disagree on the same argument, which is exactly
/// why a port must not share one "reject a non-commit endpoint" rule between
/// them.
#[test]
fn range_diff_rewrites_a_symmetric_range_and_so_accepts_a_tree_endpoint() {
    let repo = Repo::new("range-diff-symmetric");
    repo.tree_tag("treetag");

    let (out, err, code) = repo.run3(&["range-diff", "treetag...HEAD"]);
    assert_eq!(err, "", "`git range-diff treetag...HEAD` stderr");
    assert_eq!(code, 0, "`git range-diff treetag...HEAD` exit");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "both commits are right-only: {out}");
    assert!(lines[0].starts_with("-:") && lines[0].ends_with(" one"), "{out}");
    assert!(lines[1].starts_with("-:") && lines[1].ends_with(" two"), "{out}");

    // The other order swaps which side is missing, and is still not an error.
    let (out, err, code) = repo.run3(&["range-diff", "HEAD...treetag"]);
    assert_eq!(err, "", "`git range-diff HEAD...treetag` stderr");
    assert_eq!(code, 0, "`git range-diff HEAD...treetag` exit");
    assert_eq!(out.lines().count(), 2, "{out}");
    assert!(out.lines().all(|l| l.contains("-:")), "both are left-only: {out}");
}

/// `<a>..<b>` does not type-check its endpoints, so a tree or a blob on either
/// side is dropped by `handle_commit()` and the walk runs anyway.
///
/// ```c
/// if (object->type == OBJ_TREE) {
///         struct tree *tree = (struct tree *)object;
///         if (!revs->tree_objects)
///                 return NULL;
/// ```
///
/// `git log`, `git rev-list` and `git format-patch` all leave `tree_objects` off,
/// so the pending entry disappears without a word and the command exits 0. A
/// port that insists every endpoint be a commit turns each of these into a hard
/// failure — which is what `git log HEAD..<tag-of-a-tree>` used to do here.
#[test]
fn a_plain_range_drops_a_non_commit_endpoint_instead_of_failing() {
    let repo = Repo::new("plain-range-non-commit");
    repo.tree_tag("treetag");
    let blob = repo.rev_parse("HEAD:one.txt");

    // A non-commit on the *excluded* side hides nothing, so the whole history walks.
    for token in ["treetag..HEAD", "HEAD^{tree}..HEAD", &format!("{blob}..HEAD")] {
        let (out, err, code) = repo.run3(&["log", "--oneline", token]);
        assert_eq!(err, "", "`git log --oneline {token}` stderr");
        assert_eq!(code, 0, "`git log --oneline {token}` exit");
        assert_eq!(out.lines().count(), 2, "`git log --oneline {token}` walks both: {out}");
    }
    // …and on the *included* side it is the only tip, so nothing walks — but the
    // argument still counted as revision input, so `revs->def` does not put HEAD
    // back and the output stays empty rather than becoming the whole history.
    for token in ["HEAD..treetag", "HEAD..HEAD^{tree}", "treetag", "HEAD^{tree}", &blob] {
        let (out, err, code) = repo.run3(&["log", "--oneline", token]);
        assert_eq!(err, "", "`git log --oneline {token}` stderr");
        assert_eq!(code, 0, "`git log --oneline {token}` exit");
        assert_eq!(out, "", "`git log --oneline {token}` walks nothing");
    }
    // `format-patch` counts `rev.pending.nr` *before* the drop, so the tree keeps
    // `<tree>..HEAD` out of the one-object `<since>` shorthand and two patches
    // are formatted. Dropping the endpoint any earlier formats nothing at all.
    let (out, err, code) = repo.run3(&["format-patch", "--stdout", "HEAD^{tree}..HEAD"]);
    assert_eq!(err, "", "`git format-patch --stdout HEAD^{{tree}}..HEAD` stderr");
    assert_eq!(code, 0, "exit");
    assert_eq!(out.matches("\nSubject: [PATCH ").count(), 2, "two patches: {out}");
}

/// A bare `..` is a pathspec, not `HEAD..HEAD`.
///
/// `handle_revision_arg_1()` refuses it before `handle_dotdot()` is ever called
/// (`revision.c`):
///
/// ```c
/// if (!cant_be_filename && !strcmp(arg, "..")) {
///         /*
///          * Just ".."?  That is not a range but the
///          * pathspec for the parent directory.
///          */
///         ret = -1;
///         goto out;
/// }
/// ```
///
/// `setup_revisions()` then takes its `verify_filename()` branch, `..` lstats
/// fine, and it becomes prune data — where `init_pathspec_item()` rejects it for
/// leaving the repository. So the diagnostic is the *pathspec* layer's and not a
/// revision error, and it is identical whether the token was written bare or
/// behind an explicit `--`.
#[test]
fn a_bare_dotdot_is_the_parent_directory_pathspec_not_an_empty_range() {
    let repo = Repo::new("bare-dotdot");
    let want = format!("fatal: ..: '..' is outside repository at '{}'\n", repo.worktree());
    for args in [
        vec!["log", ".."],
        vec!["log", "--oneline", ".."],
        vec!["rev-list", ".."],
        vec!["shortlog", ".."],
        vec!["diff", ".."],
        vec!["show", ".."],
        vec!["format-patch", ".."],
        vec!["whatchanged", ".."],
        // Written as an explicit pathspec it is the same element and the same death.
        vec!["log", "--oneline", "--", ".."],
        vec!["diff", "--", ".."],
        vec!["shortlog", "--", ".."],
    ] {
        expect(&repo, &args, &want, 128);
    }
}

/// CONTROL for the row above, and the reason `cant_be_filename` is a parameter
/// rather than a bare string comparison.
///
/// `setup_revisions()` scans the whole argument vector for a `--` before it
/// resolves anything, so a separator *anywhere* on the line puts
/// `REVARG_CANNOT_BE_FILENAME` in force for the arguments in front of it. The
/// guard does not fire, `..` is the ordinary `HEAD..HEAD`, and it walks nothing
/// while the pathspec after the separator is the one that limits.
#[test]
fn a_dashdash_anywhere_makes_a_bare_dotdot_an_ordinary_range_again() {
    let repo = Repo::new("dotdot-after-dashdash");
    let (out, err, code) = repo.run3(&["log", "--oneline", "..", "--", "one.txt"]);
    assert_eq!(err, "", "`git log --oneline .. -- one.txt` stderr");
    assert_eq!(code, 0, "exit");
    assert_eq!(out, "", "`HEAD..HEAD` walks nothing");

    // The guard is `!strcmp(arg, "..")`, so only the *whole* token qualifies.
    // These hold a `..` too and still take the ordinary range reading, where an
    // omitted side defaults to HEAD and `HEAD..HEAD` walks nothing.
    for token in ["..HEAD", "HEAD.."] {
        let (out, err, code) = repo.run3(&["log", "--oneline", token]);
        assert_eq!(err, "", "`git log --oneline {token}` must stay a range");
        assert_eq!(code, 0, "`git log --oneline {token}` exit");
        assert_eq!(out, "", "`{token}` is `HEAD..HEAD`");
    }

    // `../..` is not the guarded token either, so it goes to `handle_dotdot()` —
    // which splits it into `HEAD` and `/..`, fails to resolve the second, and
    // returns non-zero. `setup_revisions()` then prunes with `argv + i`, the
    // *argument*, so the pathspec layer names `../..` and never the `/..` the
    // split produced.
    let want = format!("fatal: ../..: '../..' is outside repository at '{}'\n", repo.worktree());
    expect(&repo, &["log", "--oneline", "../.."], &want, 128);
}
