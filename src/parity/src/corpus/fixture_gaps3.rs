//! Cases for the five shapes added in the third wave, each closing a gap an
//! earlier agent recorded as unreachable rather than merely unvisited.
//!
//! Grouped by shape, for the reason [`super::fixture_gaps`] and
//! [`super::fixture_gaps2`] are: the shape *is* what is under test. Every block
//! below asks something no case could ask before, because no fixture carried a
//! name in two ref namespaces, two object ids sharing four characters, a hook
//! that is present and not executable, an `am` hook of any kind, a submodule
//! inside a submodule, or a split index.
//!
//! What each shape supplies, so a reader does not have to rebuild it from
//! `fixture.rs`:
//!
//! * `AmbiguousRef` — `ambi` as a branch (`HEAD~1`) and a lightweight tag
//!   (`HEAD`); `ambi-ann` as a branch and an *annotated* tag; `top` as
//!   `refs/top`, a branch and a tag at three different commits; `rem/ambi` as a
//!   branch and a remote-tracking ref; and `dual` as both a branch and a
//!   tracked path.
//! * `PrefixCollision` — `commit-mate.txt` whose blob shares `edfa` with the
//!   `initial` commit, and `pair-a.txt`/`pair-b.txt` whose blobs share `a366`
//!   with each other. Full ids:
//!   `edfab1b71619a22120a8da1a3d85d68e0200290a` (commit),
//!   `edfaaf1e9919bbb3ea91c4aee0ba9bde868cdbba`,
//!   `a36664d0c037c06c0ee81cfcfb3af000a19a60ed` and
//!   `a3660f2dc25d8d30ea9d1ae52b12eed1d2cd3bd7`.
//! * `AmHooks` — executable `applypatch-msg` (appends a trailer, refuses a
//!   message containing `REJECT`), `pre-applypatch` (refuses when
//!   `veto-preapply.txt` exists), `post-applypatch` (exits 1, which git
//!   ignores); non-executable `pre-commit` (would refuse) and `commit-msg`
//!   (would rewrite); mailboxes `mail/ok.mbox`, `mail/reject.mbox` and
//!   `mail/preveto.mbox`; branches `am-pending`, `am-reject`, `am-preveto`.
//! * `NestedSubmodule` — `.mid.git` and `.leaf.git` as bare upstreams inside
//!   the fixture, `mid` registered at an empty directory and not initialised,
//!   and `mid`'s own `.gitmodules` registering `leaf`.
//! * `SplitIndex` — `.git/sharedindex.<sha>` holding the entries of
//!   `split-index: seed`, `.git/index` holding the `link` extension and
//!   `si-d.txt` on top of it.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this module's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    ambiguous_refs(out);
    ambiguous_rev_or_path(out);
    prefix_collision(out);
    abbreviation_widening(out);
    am_hooks(out);
    inert_hooks(out);
    nested_submodule(out);
    split_index(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

/// Push one case per argv against `shape`, with stderr compared as well.
///
/// Used wherever the diagnostic *is* the answer: an ambiguity warning, an
/// `is ambiguous` refusal and its `hint:` block, and the hint git prints about
/// a hook it declined to run all go to stderr, and a case that compared only
/// stdout would score a port that stays silent as correct.
fn each_strict(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::strict(cmd, args, shape));
    }
}

// ---------------------------------------------------------------------------
// A name in two namespaces
// ---------------------------------------------------------------------------

/// Which `refs/…/<name>` a bare `<name>` means.
///
/// `ref_rev_parse_rules` is six patterns tried in order, and until this shape
/// existed the corpus could not tell them apart: with no name in two
/// namespaces every rule resolves the same ref. Measured on stock 2.55.0 over
/// this shape, `rev-parse` answers the tag for `ambi`, the annotated tag
/// *object* for `ambi-ann`, `refs/top` for `top` and the branch for
/// `rem/ambi` — four different rules winning, and every one of them warning
/// `refname '<name>' is ambiguous.` on stderr first.
///
/// The `--symbolic-full-name` pair is the one place the warning becomes a
/// refusal: git prints `error: refname 'ambi' is ambiguous` and produces no
/// name at all, because there is no single full name to print.
fn ambiguous_refs(out: &mut Vec<Case>) {
    each_strict(
        Shape::AmbiguousRef,
        "rev-parse",
        &[
            &["rev-parse", "ambi"],
            &["rev-parse", "ambi-ann"],
            &["rev-parse", "top"],
            &["rev-parse", "rem/ambi"],
            &["rev-parse", "--verify", "ambi"],
            &["rev-parse", "--verify", "ambi-ann"],
            &["rev-parse", "--verify", "top"],
            &["rev-parse", "--symbolic-full-name", "ambi"],
            &["rev-parse", "--symbolic-full-name", "top"],
            &["rev-parse", "--symbolic-full-name", "rem/ambi"],
            &["rev-parse", "--abbrev-ref", "ambi"],
            &["rev-parse", "--symbolic", "ambi"],
            // The peel asks the same question one object further on: the
            // lightweight tag is already a commit, the annotated one is not.
            &["rev-parse", "ambi^{}"],
            &["rev-parse", "ambi-ann^{}"],
            &["rev-parse", "ambi-ann^{commit}"],
            &["rev-parse", "ambi^{tree}"],
            &["rev-parse", "ambi~1"],
            &["rev-parse", "ambi..main"],
            // The unambiguous spellings, so the pair shows what the warning
            // costs: same repository, no warning, a different answer.
            &["rev-parse", "refs/heads/ambi"],
            &["rev-parse", "refs/tags/ambi"],
            &["rev-parse", "heads/ambi"],
            &["rev-parse", "tags/ambi"],
            &["rev-parse", "refs/remotes/rem/ambi"],
        ],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "cat-file",
        &[
            &["cat-file", "-t", "ambi"],
            &["cat-file", "-t", "ambi-ann"],
            &["cat-file", "-t", "top"],
            &["cat-file", "-p", "ambi-ann"],
            &["cat-file", "-s", "ambi"],
        ],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "log",
        &[
            &["log", "--oneline", "ambi"],
            &["log", "--oneline", "ambi-ann"],
            &["log", "--oneline", "top"],
            &["log", "--oneline", "rem/ambi"],
            &["log", "--oneline", "ambi..main"],
            &["log", "--format=%d", "-3", "--decorate=full"],
            &["log", "--format=%d", "-3", "--decorate=short"],
        ],
        out,
    );

    each(
        Shape::AmbiguousRef,
        "show-ref",
        &[
            &["show-ref"],
            &["show-ref", "ambi"],
            &["show-ref", "-d", "ambi"],
            &["show-ref", "--heads"],
            &["show-ref", "--tags"],
            &["show-ref", "--verify", "refs/heads/ambi"],
            &["show-ref", "--verify", "refs/tags/ambi"],
            &["show-ref", "top"],
            &["show-ref", "rem/ambi"],
        ],
        out,
    );

    each(
        Shape::AmbiguousRef,
        "for-each-ref",
        &[
            &["for-each-ref", "--format=%(refname) %(objecttype) %(objectname)"],
            &["for-each-ref", "--format=%(refname:short)"],
            &["for-each-ref", "--format=%(refname:short) %(*objecttype)", "refs/tags"],
            &["for-each-ref", "--format=%(refname)", "refs/heads"],
            &["for-each-ref", "--format=%(refname)", "refs/remotes"],
            &["for-each-ref", "--format=%(refname)", "--points-at", "ambi"],
            &["for-each-ref", "--format=%(refname)", "--contains", "ambi"],
        ],
        out,
    );

    each(
        Shape::AmbiguousRef,
        "branch",
        &[
            &["branch", "--list"],
            &["branch", "--list", "-a"],
            &["branch", "-v"],
            &["branch", "--contains", "ambi"],
            &["branch", "--points-at", "ambi"],
            &["branch", "--show-current"],
        ],
        out,
    );

    each(
        Shape::AmbiguousRef,
        "tag",
        &[
            &["tag", "-l"],
            &["tag", "-l", "--format=%(refname) %(objecttype)"],
            &["tag", "--contains", "ambi"],
            &["tag", "--points-at", "ambi"],
        ],
        out,
    );

    // `describe` and `name-rev` have to *print* a name rather than resolve one,
    // so the ambiguity reaches them from the other side: git disambiguates its
    // own output with a `tags/` prefix where the short name would be unclear.
    each(
        Shape::AmbiguousRef,
        "describe",
        &[
            &["describe"],
            &["describe", "--tags"],
            &["describe", "--all"],
            &["describe", "--all", "ambi"],
            &["describe", "--tags", "--abbrev=0"],
        ],
        out,
    );

    each(
        Shape::AmbiguousRef,
        "name-rev",
        &[
            &["name-rev", "--name-only", "HEAD"],
            &["name-rev", "--name-only", "HEAD~1"],
            &["name-rev", "--all"],
            &["name-rev", "--refs=refs/heads/*", "--name-only", "HEAD"],
            &["name-rev", "--refs=refs/tags/*", "--name-only", "HEAD"],
        ],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "merge-base",
        &[
            &["merge-base", "ambi", "main"],
            &["merge-base", "--is-ancestor", "ambi", "main"],
            &["merge-base", "top", "ambi"],
        ],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "rev-list",
        &[
            &["rev-list", "--count", "ambi"],
            &["rev-list", "--count", "top"],
            &["rev-list", "--oneline", "ambi"],
        ],
        out,
    );

    // The mutating half. `checkout` and `switch` have a DWIM of their own that
    // runs *before* `ref_rev_parse_rules`, so which of the two answers each
    // gives is a second question the shape makes askable — measured on stock,
    // `checkout ambi` warns and lands on the *branch* where `rev-parse ambi`
    // answers the tag.
    each_strict(
        Shape::AmbiguousRef,
        "checkout",
        &[
            &["checkout", "ambi"],
            &["checkout", "ambi-ann"],
            &["checkout", "top"],
            &["checkout", "rem/ambi"],
            &["checkout", "--detach", "ambi"],
        ],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "switch",
        &[&["switch", "ambi"], &["switch", "top"], &["switch", "--detach", "ambi"]],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "branch",
        &[
            &["branch", "-d", "ambi"],
            &["branch", "-f", "ambi", "main"],
            &["branch", "--set-upstream-to=rem/ambi", "main"],
        ],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "tag",
        &[&["tag", "-d", "ambi"], &["tag", "-f", "ambi", "main"]],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "update-ref",
        &[&["update-ref", "-d", "refs/top"], &["update-ref", "top", "main"]],
        out,
    );
}

/// The other ambiguity: a name that is both a revision and a path.
///
/// `dual` is a branch and a tracked file, which is
/// `fatal: ambiguous argument 'dual': both revision and filename` plus the
/// two-line hint about `--`. Every verb that takes `<rev>… [--] <path>…` shares
/// that refusal and no shape could produce it, so the whole family was pinned
/// on arguments that happened to be unambiguous.
///
/// Each refusal is paired with the two disambiguated spellings, because the
/// refusal alone does not say whether a port knows *why*: `dual --` names the
/// revision, `-- dual` names the path, and a port that treats the separator as
/// decoration gets a different one of the three wrong.
fn ambiguous_rev_or_path(out: &mut Vec<Case>) {
    each_strict(
        Shape::AmbiguousRef,
        "log",
        &[
            &["log", "--oneline", "dual"],
            &["log", "--oneline", "dual", "--"],
            &["log", "--oneline", "--", "dual"],
            &["log", "--oneline", "HEAD", "--", "dual"],
        ],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "diff",
        &[
            &["diff", "dual"],
            &["diff", "dual", "--"],
            &["diff", "--", "dual"],
            &["diff", "--stat", "ambi", "--", "dual"],
        ],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "rev-list",
        &[&["rev-list", "--count", "dual"], &["rev-list", "--count", "dual", "--"]],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "blame",
        &[&["blame", "--porcelain", "dual"], &["blame", "--porcelain", "--", "dual"]],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "checkout",
        &[&["checkout", "dual"], &["checkout", "--", "dual"], &["checkout", "dual", "--"]],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "restore",
        &[&["restore", "dual"], &["restore", "--source=ambi", "dual"]],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "show",
        &[&["show", "--oneline", "--stat", "dual"], &["show", "--oneline", "--stat", "dual", "--"]],
        out,
    );

    each_strict(
        Shape::AmbiguousRef,
        "grep",
        &[&["grep", "-n", "branch", "dual"], &["grep", "-n", "branch", "--", "dual"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// Two objects at one four-character prefix
// ---------------------------------------------------------------------------

/// The full ids of the four objects the collisions are between.
///
/// Spelled out rather than resolved at run time because they are constants of
/// this shape — the fixture asserts them at build time — and a case is one argv
/// that cannot ask a question before asking its own.
const COMMIT_AT_EDFA: &str = "edfab1b71619a22120a8da1a3d85d68e0200290a";
const BLOB_AT_EDFA: &str = "edfaaf1e9919bbb3ea91c4aee0ba9bde868cdbba";
const BLOB_A_AT_A366: &str = "a36664d0c037c06c0ee81cfcfb3af000a19a60ed";

/// An abbreviation short enough to be ambiguous.
///
/// `core.disambiguate` decides which candidate a short id means *by type*, and
/// with no collision anywhere in the corpus its five values selected between
/// identical answers. Over this shape they do not: `edfa` is a commit and a
/// blob, so `commit` and `committish` answer one and `blob` answers the other,
/// while `a366` is two blobs and no value resolves it.
///
/// Strict throughout, because the unresolved answer is entirely on stderr —
/// `error: short object ID edfa is ambiguous`, then a `hint:` line per
/// candidate naming its type, then the `fatal:` that ends it. A port that
/// exits 128 silently matches on stdout alone.
fn prefix_collision(out: &mut Vec<Case>) {
    each_strict(
        Shape::PrefixCollision,
        "rev-parse",
        &[
            &["rev-parse", "edfa"],
            &["rev-parse", "a366"],
            &["rev-parse", "--verify", "edfa"],
            &["rev-parse", "--verify", "a366"],
            &["rev-parse", "--disambiguate=edfa"],
            &["rev-parse", "--disambiguate=a366"],
            &["rev-parse", "--disambiguate=edfab"],
            // Five characters resolves each of them, which is what makes the
            // four-character refusal a statement about ambiguity rather than
            // about the id being unknown.
            &["rev-parse", "edfab"],
            &["rev-parse", "edfaa"],
            &["rev-parse", "a3666"],
            &["rev-parse", "a3660"],
        ],
        out,
    );

    for value in ["commit", "committish", "tree", "treeish", "blob", "none"] {
        for prefix in ["edfa", "a366"] {
            out.push(
                Case::strict("rev-parse", &["rev-parse", prefix], Shape::PrefixCollision)
                    .with_config(&[("core.disambiguate", value)]),
            );
        }
    }

    each_strict(
        Shape::PrefixCollision,
        "cat-file",
        &[
            &["cat-file", "-t", "edfa"],
            &["cat-file", "-t", "a366"],
            &["cat-file", "-e", "edfa"],
            &["cat-file", "-s", "a366"],
            &["cat-file", "-p", "edfaa"],
            &["cat-file", "-t", "edfab"],
        ],
        out,
    );

    for value in ["commit", "blob"] {
        out.push(
            Case::strict("cat-file", &["cat-file", "-t", "edfa"], Shape::PrefixCollision)
                .with_config(&[("core.disambiguate", value)]),
        );
    }

    each_strict(
        Shape::PrefixCollision,
        "log",
        &[&["log", "--oneline", "edfa"], &["log", "--oneline", "edfab"]],
        out,
    );

    each_strict(
        Shape::PrefixCollision,
        "show",
        &[&["show", "--stat", "--oneline", "edfa"], &["show", "--stat", "--oneline", "a366"]],
        out,
    );
}

/// Abbreviating an id that four characters no longer identify.
///
/// `find_unique_abbrev` widens until the prefix is unique, and no fixture could
/// make it widen: every id in every other shape is unique at four. Over this
/// shape the initial commit needs five, so one listing carries both widths —
/// `log --oneline --abbrev=4` prints `edfab` for that row and four characters
/// for the rest, which is a difference a port that truncates cannot produce.
///
/// Asked through every printer that abbreviates, because they do not share one
/// implementation: the `%h` placeholder, `--raw`'s two id columns, `ls-tree`,
/// `blame`, `describe --abbrev`, `branch -v` and `rev-parse --short` each call
/// it for themselves.
fn abbreviation_widening(out: &mut Vec<Case>) {
    each(
        Shape::PrefixCollision,
        "rev-parse",
        &[
            &["rev-parse", "--short=4", COMMIT_AT_EDFA],
            &["rev-parse", "--short=5", COMMIT_AT_EDFA],
            &["rev-parse", "--short=4", BLOB_AT_EDFA],
            &["rev-parse", "--short=4", BLOB_A_AT_A366],
            &["rev-parse", "--short=4", "HEAD"],
            &["rev-parse", "--short", COMMIT_AT_EDFA],
        ],
        out,
    );

    each(
        Shape::PrefixCollision,
        "log",
        &[
            &["log", "--oneline", "--abbrev=4"],
            &["log", "--abbrev=4", "--format=%h %s"],
            &["log", "--abbrev=4", "--format=%h %t %p"],
            &["log", "--abbrev=4", "--raw", "-1", "HEAD~1"],
            &["log", "--abbrev=4", "--abbrev-commit", "--pretty=short"],
            &["log", "--abbrev=40", "--format=%h"],
        ],
        out,
    );

    for value in ["4", "5", "8", "40"] {
        out.push(
            Case::new("log", &["log", "--oneline"], Shape::PrefixCollision)
                .with_config(&[("core.abbrev", value)]),
        );
    }

    each(
        Shape::PrefixCollision,
        "diff-tree",
        &[
            &["diff-tree", "--abbrev=4", "-r", "HEAD~1", "HEAD"],
            &["diff-tree", "--abbrev=4", "--raw", "HEAD~2", "HEAD~1"],
        ],
        out,
    );

    each(
        Shape::PrefixCollision,
        "diff",
        &[
            &["diff", "--abbrev=4", "--raw", "HEAD~2", "HEAD~1"],
            &["diff", "--abbrev=4", "--summary", "HEAD~2", "HEAD~1"],
        ],
        out,
    );

    each(
        Shape::PrefixCollision,
        "ls-tree",
        &[&["ls-tree", "--abbrev=4", "HEAD"], &["ls-tree", "--abbrev=4", "-r", "HEAD"]],
        out,
    );

    each(
        Shape::PrefixCollision,
        "blame",
        &[&["blame", "--abbrev=4", "README.md"], &["blame", "--abbrev=4", "pair-a.txt"]],
        out,
    );

    each(
        Shape::PrefixCollision,
        "describe",
        &[
            &["describe", "--always", "--abbrev=4"],
            &["describe", "--always", "--abbrev=4", "HEAD~2"],
        ],
        out,
    );

    each(
        Shape::PrefixCollision,
        "branch",
        &[&["branch", "-v", "--abbrev=4"], &["branch", "-v", "--abbrev=0"]],
        out,
    );

    each(
        Shape::PrefixCollision,
        "rev-list",
        &[&["rev-list", "--abbrev=4", "--abbrev-commit", "HEAD"]],
        out,
    );

    each(
        Shape::PrefixCollision,
        "for-each-ref",
        &[&["for-each-ref", "--format=%(objectname:short=4) %(refname)"]],
        out,
    );

    each(
        Shape::PrefixCollision,
        "show-branch",
        &[&["show-branch", "--sha1-name", "--all"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// The am hooks, and the hooks that do not run
// ---------------------------------------------------------------------------

/// `applypatch-msg`, `pre-applypatch` and `post-applypatch`.
///
/// Installed by no other shape, so all four `am` hook spellings could only ever
/// be measured as argument parsing. Here each of the three is reachable and one
/// of them refuses, which is what turns `--no-verify` from a flag into an
/// observable: `am mail/reject.mbox` exits 1 having applied nothing, and
/// `am --no-verify mail/reject.mbox` commits the same patch.
///
/// Where the hooks left their marks is as much of the answer as the exit code,
/// and the state probe reads it: `hook-applypatch-msg.txt`,
/// `hook-pre-applypatch.txt` and `hook-post-applypatch.txt` are untracked files
/// that exist only if the hook they are named for ran, and the trailer
/// `applypatch-trailer` appears in the resulting commit message only if the
/// first one was allowed to rewrite it.
fn am_hooks(out: &mut Vec<Case>) {
    each(
        Shape::AmHooks,
        "am",
        &[
            &["am", "mail/ok.mbox"],
            &["am", "--no-verify", "mail/ok.mbox"],
            &["am", "mail/reject.mbox"],
            &["am", "--no-verify", "mail/reject.mbox"],
            &["am", "mail/preveto.mbox"],
            &["am", "--no-verify", "mail/preveto.mbox"],
            // The flags that change what the message the hook is handed looks
            // like, so a port that runs the hook at the wrong point in the
            // sequence produces a different file.
            &["am", "--signoff", "mail/ok.mbox"],
            &["am", "--keep", "mail/ok.mbox"],
            &["am", "--keep-non-patch", "mail/ok.mbox"],
            &["am", "--message-id", "mail/ok.mbox"],
            &["am", "--quiet", "mail/ok.mbox"],
            &["am", "-3", "mail/ok.mbox"],
            &["am", "--committer-date-is-author-date", "mail/ok.mbox"],
            &["am", "--whitespace=fix", "mail/ok.mbox"],
            &["am", "--no-verify", "--signoff", "mail/reject.mbox"],
            // One mailbox with two patches: the hooks run once per patch, so
            // the marker files carry two lines and a port that runs them once
            // per invocation is caught by the state probe rather than by stdout.
            &["am", "--quiet", "--no-verify", "mail/ok.mbox"],
        ],
        out,
    );

    each(
        Shape::AmHooks,
        "apply",
        &[
            // `apply` shares the patch machinery and runs *no* hook, which is
            // the control: the same content, none of the marker files.
            &["apply", "--stat", "mail/ok.mbox"],
            &["apply", "--check", "mail/ok.mbox"],
        ],
        out,
    );

    each(
        Shape::AmHooks,
        "hook",
        &[
            &["hook", "run", "applypatch-msg"],
            &["hook", "run", "pre-applypatch"],
            &["hook", "run", "post-applypatch"],
        ],
        out,
    );
}

/// A hook that is present and not executable.
///
/// `install_hooks` chmods every hook 0755, so this had no fixture at all — and
/// it is a decision git makes out loud: it skips the file and says
/// `hint: The '.git/hooks/pre-commit' hook was ignored because it's not set as
/// executable.` under `advice.ignoredHook`. Both non-executable hooks here
/// would be unmissable if they ran, one refusing the commit and the other
/// rewriting its message, so "did it run" needs no separate probe.
///
/// Strict, because the hint is the whole visible difference: the commit
/// succeeds either way and its stdout is the same.
fn inert_hooks(out: &mut Vec<Case>) {
    each_strict(
        Shape::AmHooks,
        "commit",
        &[
            &["commit", "--allow-empty", "-m", "inert hooks"],
            &["commit", "--allow-empty", "--no-verify", "-m", "inert hooks"],
            &["commit", "--allow-empty", "-m", "inert hooks", "--no-post-rewrite"],
        ],
        out,
    );

    // The advice that names the skipped hook is configuration, so a port may
    // print it unconditionally and still match the default. Turning it off is
    // how the case separates "does not run the hook" from "does not know the
    // hook is there".
    out.push(
        Case::strict(
            "commit",
            &["commit", "--allow-empty", "-m", "advice off"],
            Shape::AmHooks,
        )
        .with_config(&[("advice.ignoredHook", "false")]),
    );

    each_strict(
        Shape::AmHooks,
        "hook",
        &[&["hook", "run", "pre-commit"], &["hook", "run", "commit-msg"]],
        out,
    );

    each_strict(
        Shape::AmHooks,
        "merge",
        &[
            // `merge --no-ff` runs `commit-msg` and not `pre-commit`; with
            // `commit-msg` inert the merge message must come through unedited.
            &["merge", "--no-ff", "-m", "merge am-pending", "am-pending"],
            &["merge", "--no-verify", "--no-ff", "-m", "merge am-pending", "am-pending"],
        ],
        out,
    );

    // `core.hooksPath` pointed at a directory with nothing in it is the other
    // way a present hook stops running, and it answers differently: no hint,
    // because there is no file to have ignored.
    out.push(
        Case::strict("commit", &["commit", "--allow-empty", "-m", "redirected"], Shape::AmHooks)
            .with_config(&[("core.hooksPath", "mail")]),
    );
}

// ---------------------------------------------------------------------------
// A submodule inside a submodule
// ---------------------------------------------------------------------------

/// `--recursive` measured by what it builds.
///
/// One level of nesting makes `--recursive` a synonym for its absence. Two
/// levels do not: `submodule update --init` leaves `mid/leaf` empty and
/// `submodule update --init --recursive` fills it, so the post-command state is
/// the measurement and a port that ignores the flag cannot match both.
///
/// `protocol.file.allow=always` is named on every case that clones rather than
/// left to the default, for the reason [`crate::fixture::Shape::Shallow`]
/// gives: the default is `user`, it happens to permit this today, and a case
/// that depends on that is measuring the default rather than the verb.
fn nested_submodule(out: &mut Vec<Case>) {
    for args in [
        ["submodule", "update", "--init"].as_slice(),
        ["submodule", "update", "--init", "--recursive"].as_slice(),
        ["submodule", "update", "--init", "--recursive", "--depth", "1"].as_slice(),
        ["submodule", "update", "--init", "--", "mid"].as_slice(),
        ["submodule", "update", "--recursive"].as_slice(),
        ["submodule", "update", "--init", "--checkout"].as_slice(),
        ["submodule", "update", "--init", "--recursive", "--no-fetch"].as_slice(),
    ] {
        out.push(
            Case::new("submodule", args, Shape::NestedSubmodule)
                .with_config(&[("protocol.file.allow", "always")]),
        );
    }

    each(
        Shape::NestedSubmodule,
        "submodule",
        &[
            &["submodule", "status"],
            &["submodule", "status", "--recursive"],
            &["submodule", "status", "--cached"],
            &["submodule", "init"],
            &["submodule", "init", "mid"],
            &["submodule", "sync"],
            &["submodule", "sync", "--recursive"],
            &["submodule", "summary"],
            &["submodule", "foreach", "echo $displaypath"],
            &["submodule", "foreach", "--recursive", "echo $displaypath"],
            &["submodule", "absorbgitdirs"],
            &["submodule", "deinit", "--all"],
            &["submodule", "set-url", "mid", "./.mid.git"],
            &["submodule", "set-branch", "--branch", "main", "mid"],
        ],
        out,
    );

    // The verbs that only *read* the registration, which is where a port that
    // stores the gitlink correctly and the `.gitmodules` entry incorrectly
    // shows the difference.
    each(
        Shape::NestedSubmodule,
        "ls-files",
        &[&["ls-files", "--stage"], &["ls-files", "--stage", "mid"]],
        out,
    );

    each(
        Shape::NestedSubmodule,
        "status",
        &[
            &["status", "--porcelain=v1"],
            &["status", "--porcelain=v2"],
            &["status", "--porcelain=v1", "--ignore-submodules=none"],
            &["status", "--long"],
        ],
        out,
    );

    each(
        Shape::NestedSubmodule,
        "config",
        &[
            &["config", "-f", ".gitmodules", "--list"],
            &["config", "-f", ".gitmodules", "--get", "submodule.mid.url"],
            &["config", "--list", "--local"],
        ],
        out,
    );

    each(
        Shape::NestedSubmodule,
        "diff",
        &[
            &["diff", "--submodule=short", "HEAD~1"],
            &["diff", "--submodule=log", "HEAD~1"],
            &["diff", "--cached", "--raw"],
            &["diff-tree", "-r", "HEAD~1", "HEAD"],
        ],
        out,
    );

    each(
        Shape::NestedSubmodule,
        "ls-tree",
        &[&["ls-tree", "HEAD"], &["ls-tree", "-r", "HEAD"], &["ls-tree", "-r", "-t", "HEAD"]],
        out,
    );

    each(
        Shape::NestedSubmodule,
        "cat-file",
        &[&["cat-file", "-p", "HEAD:.gitmodules"], &["cat-file", "-t", "HEAD:mid"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// A split index
// ---------------------------------------------------------------------------

/// Entries in `.git/sharedindex.<sha>` rather than in `.git/index`.
///
/// No shape had one, so the whole feature was argument parsing: `core.splitIndex`
/// chose between two spellings of the same file, `--split-index` had nothing to
/// split and `--no-split-index` nothing to fold back in. The distinguishing
/// facts are all in the post-command state — how many `sharedindex.*` files
/// there are, which one the `link` extension names, and whether an entry moved
/// between the two halves — and the harness's index probe already reports the
/// version, entry count and extension chain of both files.
///
/// Every case here is one that has to *decide* about the split: read it, refresh
/// it, add to it, fold it back, or leave it alone.
fn split_index(out: &mut Vec<Case>) {
    each(
        Shape::SplitIndex,
        "update-index",
        &[
            &["update-index", "--split-index"],
            &["update-index", "--no-split-index"],
            &["update-index", "--refresh"],
            &["update-index", "--really-refresh"],
            &["update-index", "--force-remove", "si-a.txt"],
            &["update-index", "--add", "--cacheinfo", "100644,e69de29bb2d1d6434b8b29ae775ad8c2e48c5391,empty.txt"],
            &["update-index", "--assume-unchanged", "si-a.txt"],
            &["update-index", "--skip-worktree", "si-a.txt"],
        ],
        out,
    );

    for value in ["0", "20", "100"] {
        out.push(
            Case::new("update-index", &["update-index", "--split-index"], Shape::SplitIndex)
                .with_config(&[("splitIndex.maxPercentChange", value)]),
        );
    }

    for value in ["true", "false"] {
        out.push(
            Case::new("status", &["status", "--porcelain=v1"], Shape::SplitIndex)
                .with_config(&[("core.splitIndex", value)]),
        );
        out.push(
            Case::new("add", &["add", "sub"], Shape::SplitIndex)
                .with_config(&[("core.splitIndex", value)]),
        );
    }

    each(
        Shape::SplitIndex,
        "ls-files",
        &[
            &["ls-files", "-v"],
            &["ls-files", "--stage"],
            &["ls-files", "--debug", "si-a.txt"],
            &["ls-files", "--others"],
        ],
        out,
    );

    each(
        Shape::SplitIndex,
        "status",
        &[
            &["status", "--porcelain=v1"],
            &["status", "--porcelain=v2"],
            &["status", "--short"],
        ],
        out,
    );

    // Every verb that writes the index has to choose whether the entry it wrote
    // lands in the shared half or the split one, and whether the shared half is
    // rewritten at all. The state probe reads which.
    each(
        Shape::SplitIndex,
        "add",
        &[&["add", "si-a.txt"], &["add", "-A"], &["add", "--renormalize", "."]],
        out,
    );

    each(
        Shape::SplitIndex,
        "commit",
        &[&["commit", "--allow-empty", "-m", "split"], &["commit", "-am", "split"]],
        out,
    );

    each(
        Shape::SplitIndex,
        "reset",
        &[&["reset", "--hard", "HEAD~1"], &["reset", "--mixed", "HEAD~1"], &["reset", "HEAD"]],
        out,
    );

    each(
        Shape::SplitIndex,
        "read-tree",
        &[&["read-tree", "HEAD"], &["read-tree", "-m", "-u", "HEAD"], &["read-tree", "--empty"]],
        out,
    );

    each(
        Shape::SplitIndex,
        "checkout",
        &[&["checkout", "--", "si-a.txt"], &["checkout", "-b", "si-side"]],
        out,
    );

    each(
        Shape::SplitIndex,
        "stash",
        &[&["stash", "list"], &["stash", "push", "-u", "-m", "over a split index"]],
        out,
    );

    each(
        Shape::SplitIndex,
        "write-tree",
        &[&["write-tree"]],
        out,
    );

    each(
        Shape::SplitIndex,
        "fsck",
        &[&["fsck", "--no-progress"]],
        out,
    );

    each(
        Shape::SplitIndex,
        "gc",
        &[&["gc", "--quiet", "--no-prune"]],
        out,
    );
}
