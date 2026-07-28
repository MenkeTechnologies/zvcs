//! Differential corpus cases for the ref-plumbing subsystem.
//!
//! Covers the commands scripts and tooling reach for when they need to read or
//! move a ref without going through porcelain: `show-ref`, `for-each-ref`,
//! `update-ref`, `symbolic-ref`, `pack-refs`, `reflog`, `refs`, `name-rev`,
//! `merge-base`, `check-ref-format`, `format-rev`, `repo`, `version`, and the
//! `rev-list` flags the base corpus does not reach.
//!
//! Determinism notes, since two of these commands look non-deterministic and
//! are not:
//!
//! * Reflog entries embed a timestamp, but `env::harden` pins
//!   `GIT_COMMITTER_DATE`, and the fixture's existing entries are baked into the
//!   template and copied byte-for-byte per case. Absolute date formats
//!   (`--date=iso`, `%(committerdate:iso8601)`, `%(creatordate:unix)`) are
//!   therefore stable. Relative formats (`--date=relative`,
//!   `%(committerdate:relative)`) read the wall clock and are deliberately
//!   absent.
//! * `refs migrate --dry-run` prints a temp path whose suffix is re-rolled every
//!   run, so it is absent too; the two `refs migrate` cases below use the
//!   error paths, which are stable.
//!
//! **Known harness limits, recorded rather than worked around:**
//!
//! * `runner::run_side` attaches `Stdio::null()` to both sides, so no case can
//!   feed a command input. `update-ref --stdin`, `rev-list --stdin`,
//!   `for-each-ref --stdin`, `name-rev --annotate-stdin` and `format-rev` are
//!   therefore exercised only on the empty-input path — enough to catch a flag
//!   that is rejected or a read that hangs, not enough to compare a transaction.
//!   Real `--stdin` coverage needs a runner change, which is out of scope here.
//! * `runner::probe_state` does not read reflogs, so a command that updates a
//!   ref correctly while writing the wrong reflog scores `Match`. That blind
//!   spot is live: `update-ref refs/heads/main HEAD~1` is byte-identical on
//!   stdout and in every probe, yet stock also appends to `.git/logs/HEAD` when
//!   the updated ref is HEAD's symref target and zvcs does not. The cases are
//!   kept because they are the right invocations; the gap is in the probe.

use crate::corpus::read_only;
use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    show_ref(out);
    for_each_ref(out);
    update_ref(out);
    symbolic_ref(out);
    pack_refs(out);
    reflog(out);
    refs(out);
    name_rev(out);
    merge_base(out);
    rev_list_gaps(out);
    misc(out);
}

/// `show-ref`: the cheapest way to ask "what refs exist and where do they point".
fn show_ref(out: &mut Vec<Case>) {
    read_only("show-ref", &["show-ref"], out);
    read_only("show-ref", &["show-ref", "--heads"], out);
    read_only("show-ref", &["show-ref", "--tags"], out);
    read_only("show-ref", &["show-ref", "--head"], out);
    read_only("show-ref", &["show-ref", "--hash"], out);
    read_only("show-ref", &["show-ref", "--dereference"], out);
    out.push(Case::new("show-ref", &["show-ref", "-d", "--tags"], Shape::Branched));
    out.push(Case::new("show-ref", &["show-ref", "--hash", "--heads"], Shape::Branched));
    out.push(Case::new("show-ref", &["show-ref", "--abbrev=8", "--heads"], Shape::Branched));
    out.push(Case::new("show-ref", &["show-ref", "main"], Shape::Branched));
    out.push(Case::new("show-ref", &["show-ref", "--verify", "refs/heads/main"], Shape::Branched));
    out.push(Case::new(
        "show-ref",
        &["show-ref", "--quiet", "--verify", "refs/heads/main"],
        Shape::Branched,
    ));
    out.push(Case::new("show-ref", &["show-ref", "--exists", "refs/heads/main"], Shape::Branched));

    // Error paths. `--verify` on a missing ref exits 1 with a message; `--exists`
    // has its own distinct exit code for "absent" versus "malformed name", and
    // getting those confused breaks every script that branches on them.
    read_only("show-ref", &["show-ref", "--verify", "refs/heads/nope"], out);
    read_only("show-ref", &["show-ref", "--exists", "refs/heads/nope"], out);
    read_only("show-ref", &["show-ref", "--exists", "not-a-full-refname"], out);
    read_only("show-ref", &["show-ref", "no-such-pattern"], out);
}

/// `for-each-ref`: the format engine tooling leans on hardest. Each case pins one
/// atom or modifier, so a failure names the atom rather than "the formatter".
fn for_each_ref(out: &mut Vec<Case>) {
    read_only("for-each-ref", &["for-each-ref"], out);
    read_only("for-each-ref", &["for-each-ref", "--format=%(refname)"], out);
    read_only("for-each-ref", &["for-each-ref", "--format=%(refname:short)"], out);
    read_only("for-each-ref", &["for-each-ref", "--format=%(objectname) %(objecttype)"], out);

    // Ref-name modifiers.
    let branched = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("for-each-ref", args, Shape::Branched));
    };
    branched(&["for-each-ref", "--format=%(refname:lstrip=2)"], out);
    branched(&["for-each-ref", "--format=%(refname:rstrip=1)"], out);
    branched(&["for-each-ref", "--format=%(refname:strip=1)"], out);
    branched(&["for-each-ref", "--format=%(objectname:short)"], out);
    branched(&["for-each-ref", "--format=%(objectname:short=8)"], out);
    branched(&["for-each-ref", "--format=%(objectsize)"], out);
    branched(&["for-each-ref", "--format=%(objectsize:disk)"], out);

    // Peeling: `*` dereferences an annotated tag to the object it names. The
    // Branched fixture has both a lightweight and an annotated tag, so this
    // distinguishes "peels" from "always prints the tag object".
    branched(&["for-each-ref", "--format=%(*objectname)"], out);
    branched(&["for-each-ref", "--format=%(*objecttype)"], out);
    branched(&["for-each-ref", "--format=%(taggername) %(taggeremail)"], out);
    branched(&["for-each-ref", "--format=%(taggerdate:iso)"], out);

    // Commit metadata atoms.
    branched(&["for-each-ref", "--format=%(subject)"], out);
    branched(&["for-each-ref", "--format=%(contents:subject)"], out);
    branched(&["for-each-ref", "--format=%(contents:body)"], out);
    branched(&["for-each-ref", "--format=%(contents)"], out);
    branched(&["for-each-ref", "--format=%(authorname) %(authoremail)"], out);
    branched(&["for-each-ref", "--format=%(creator)"], out);
    branched(&["for-each-ref", "--format=%(committerdate:iso8601)"], out);
    branched(&["for-each-ref", "--format=%(committerdate:short)"], out);
    branched(&["for-each-ref", "--format=%(creatordate:unix)"], out);
    branched(&["for-each-ref", "--format=%(trailers)"], out);
    branched(&["for-each-ref", "--format=%(raw:size)"], out);
    branched(&["for-each-ref", "--format=%(signature)"], out);

    // Ref-state atoms. These are what `git branch -vv` and every prompt script
    // are built from.
    branched(&["for-each-ref", "--format=%(HEAD)%(refname:short)"], out);
    branched(&["for-each-ref", "--format=%(upstream)"], out);
    branched(&["for-each-ref", "--format=%(upstream:short)"], out);
    branched(&["for-each-ref", "--format=%(push)"], out);
    branched(&["for-each-ref", "--format=%(symref)"], out);
    branched(&["for-each-ref", "--format=%(flag)"], out);
    branched(&["for-each-ref", "--format=%(worktreepath)"], out);
    branched(&["for-each-ref", "--format=%(describe)"], out);
    branched(&["for-each-ref", "--format=%(ahead-behind:HEAD)"], out);

    // Interpolation constructs, not atoms: conditionals, alignment, colour, NUL.
    branched(
        &["for-each-ref", "--format=%(if)%(HEAD)%(then)*%(else) %(end)%(refname:short)"],
        out,
    );
    branched(&["for-each-ref", "--format=%(align:20)%(refname)%(end)|"], out);
    branched(&["for-each-ref", "--format=%(align:width=10,position=right)%(refname:short)%(end)|"], out);
    branched(&["for-each-ref", "--format=%(padright:12)x"], out);
    branched(&["for-each-ref", "--format=%(color:red)%(refname)"], out);
    branched(&["for-each-ref", "--format=%(refname)%00%(objectname)"], out);

    // Quoting modes: each shell's escaping rules are a separate code path.
    branched(&["for-each-ref", "--shell", "--format=%(refname)"], out);
    branched(&["for-each-ref", "--perl", "--format=%(refname)"], out);
    branched(&["for-each-ref", "--python", "--format=%(refname)"], out);
    branched(&["for-each-ref", "--tcl", "--format=%(refname)"], out);

    // Selection and ordering.
    branched(&["for-each-ref", "--count=2"], out);
    branched(&["for-each-ref", "--sort=-refname"], out);
    branched(&["for-each-ref", "--sort=committerdate", "--format=%(refname)"], out);
    branched(&["for-each-ref", "--sort=version:refname", "--format=%(refname)"], out);
    branched(&["for-each-ref", "--sort=objecttype", "--format=%(objecttype) %(refname)"], out);
    branched(&["for-each-ref", "refs/heads"], out);
    branched(&["for-each-ref", "refs/tags/*"], out);
    branched(&["for-each-ref", "--exclude", "refs/tags/*", "--format=%(refname)"], out);
    branched(&["for-each-ref", "--start-after", "refs/heads/feature", "--format=%(refname)"], out);
    branched(&["for-each-ref", "--ignore-case", "--format=%(refname)", "REFS/HEADS/*"], out);
    branched(&["for-each-ref", "--include-root-refs", "--format=%(refname)"], out);
    branched(&["for-each-ref", "--points-at", "HEAD"], out);
    branched(&["for-each-ref", "--points-at", "v0.2.0"], out);
    branched(&["for-each-ref", "--merged", "HEAD", "--format=%(refname)"], out);
    branched(&["for-each-ref", "--no-merged", "HEAD", "--format=%(refname)"], out);
    branched(&["for-each-ref", "--contains", "HEAD", "--format=%(refname)"], out);
    out.push(Case::new("for-each-ref", &["for-each-ref", "--points-at", "HEAD"], Shape::Merged));

    // Error paths: an unknown atom and an unknown sort key must both be rejected
    // the same way, and `%(rest)` is legal in `cat-file --batch` but not here.
    read_only("for-each-ref", &["for-each-ref", "--format=%(badatom)"], out);
    read_only("for-each-ref", &["for-each-ref", "--format=%(objectname)%(rest)"], out);
    branched(&["for-each-ref", "--sort=nonexistent-key", "--format=%(objecttype)"], out);
    // Empty stdin; see the module note on the runner's null stdin.
    branched(&["for-each-ref", "--stdin"], out);
}

/// `update-ref`: the only sanctioned way for a script to move a ref.
fn update_ref(out: &mut Vec<Case>) {
    out.push(Case::new("update-ref", &["update-ref", "refs/heads/newref", "HEAD"], Shape::Linear));
    out.push(Case::new("update-ref", &["update-ref", "refs/heads/newref", "HEAD"], Shape::Branched));
    out.push(Case::new("update-ref", &["update-ref", "refs/heads/main", "HEAD~1"], Shape::Branched));
    out.push(Case::new("update-ref", &["update-ref", "refs/heads/feature", "main"], Shape::Branched));
    out.push(Case::new("update-ref", &["update-ref", "--no-deref", "HEAD", "HEAD~1"], Shape::Branched));
    out.push(Case::new(
        "update-ref",
        &["update-ref", "-m", "parity update", "refs/heads/main", "HEAD~1"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "update-ref",
        &["update-ref", "--create-reflog", "refs/heads/logged", "HEAD"],
        Shape::Linear,
    ));

    // Deletion, with and without the old-value guard.
    out.push(Case::new("update-ref", &["update-ref", "-d", "refs/heads/feature"], Shape::Branched));
    out.push(Case::new("update-ref", &["update-ref", "-d", "refs/tags/v0.1.0"], Shape::Branched));
    out.push(Case::new("update-ref", &["update-ref", "-d", "HEAD"], Shape::Branched));

    // Old-value guards. The zero oid means "must not already exist", so this must
    // fail against an existing branch; a wrong non-zero oid must fail too, and
    // neither may leave the ref moved.
    out.push(Case::new(
        "update-ref",
        &["update-ref", "refs/heads/feature", "HEAD", "0000000000000000000000000000000000000000"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "update-ref",
        &[
            "update-ref",
            "refs/heads/feature",
            "main",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        ],
        Shape::Branched,
    ));
    out.push(Case::new(
        "update-ref",
        &["update-ref", "-d", "refs/heads/feature", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"],
        Shape::Branched,
    ));

    // Error paths.
    out.push(Case::new("update-ref", &["update-ref"], Shape::Linear));
    out.push(Case::new("update-ref", &["update-ref", "refs/heads/main"], Shape::Linear));
    out.push(Case::new("update-ref", &["update-ref", "refs/heads/bad~name", "HEAD"], Shape::Linear));
    out.push(Case::new("update-ref", &["update-ref", "refs/heads/bad..name", "HEAD"], Shape::Linear));
    out.push(Case::new("update-ref", &["update-ref", "refs/heads/x", "no-such-rev"], Shape::Linear));
    out.push(Case::new("update-ref", &["update-ref", "-d", "refs/heads/nope"], Shape::Linear));

    // Empty stdin; see the module note on the runner's null stdin.
    out.push(Case::new("update-ref", &["update-ref", "--stdin"], Shape::Linear));
    out.push(Case::new("update-ref", &["update-ref", "--stdin", "-z"], Shape::Linear));
}

/// `symbolic-ref`: reading and rewriting HEAD itself.
fn symbolic_ref(out: &mut Vec<Case>) {
    read_only("symbolic-ref", &["symbolic-ref", "HEAD"], out);
    read_only("symbolic-ref", &["symbolic-ref", "--short", "HEAD"], out);
    read_only("symbolic-ref", &["symbolic-ref", "-q", "HEAD"], out);
    out.push(Case::new("symbolic-ref", &["symbolic-ref", "HEAD", "refs/heads/feature"], Shape::Branched));
    out.push(Case::new(
        "symbolic-ref",
        &["symbolic-ref", "-m", "parity retarget", "HEAD", "refs/heads/feature"],
        Shape::Branched,
    ));
    out.push(Case::new("symbolic-ref", &["symbolic-ref", "-d", "HEAD"], Shape::Linear));

    // Error paths: a non-symbolic ref, a missing one, and a target that is not a
    // well-formed refname. Detached HEAD is the interesting read: it is a real
    // ref that is not symbolic, and `-q` changes only the message, not the code.
    read_only("symbolic-ref", &["symbolic-ref", "refs/heads/main"], out);
    read_only("symbolic-ref", &["symbolic-ref", "no-such-ref"], out);
    read_only("symbolic-ref", &["symbolic-ref", "-q", "no-such-ref"], out);
    out.push(Case::new("symbolic-ref", &["symbolic-ref", "HEAD", "not-a-ref"], Shape::Linear));
    out.push(Case::new("symbolic-ref", &["symbolic-ref", "HEAD", "refs/heads/nonexistent"], Shape::Linear));
}

/// `pack-refs`: moves loose refs into `packed-refs`. Nothing observable changes
/// in the ref set, which is exactly why it is worth comparing — the probe reads
/// the refs back through stock git, so a pack that loses or corrupts a ref shows
/// up even though the command prints nothing.
fn pack_refs(out: &mut Vec<Case>) {
    out.push(Case::new("pack-refs", &["pack-refs"], Shape::Branched));
    out.push(Case::new("pack-refs", &["pack-refs", "--all"], Shape::Linear));
    out.push(Case::new("pack-refs", &["pack-refs", "--all"], Shape::Branched));
    out.push(Case::new("pack-refs", &["pack-refs", "--all"], Shape::Merged));
    out.push(Case::new("pack-refs", &["pack-refs", "--all", "--prune"], Shape::Branched));
    out.push(Case::new("pack-refs", &["pack-refs", "--no-prune", "--all"], Shape::Branched));
    out.push(Case::new("pack-refs", &["pack-refs", "--include", "refs/tags/*"], Shape::Branched));
    out.push(Case::new("pack-refs", &["pack-refs", "--exclude", "refs/tags/*"], Shape::Branched));
    out.push(Case::new(
        "pack-refs",
        &["pack-refs", "--all", "--include", "refs/heads/*"],
        Shape::Branched,
    ));
    out.push(Case::new("pack-refs", &["pack-refs", "--bogus-flag"], Shape::Linear));
}

/// `reflog`: `show` is read-only, `expire`/`delete` rewrite the log in place.
fn reflog(out: &mut Vec<Case>) {
    read_only("reflog", &["reflog"], out);
    read_only("reflog", &["reflog", "show"], out);
    read_only("reflog", &["reflog", "show", "HEAD"], out);
    out.push(Case::new("reflog", &["reflog", "show", "refs/heads/main"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "show", "--all"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "-n", "2"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "show", "--no-abbrev", "HEAD"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "show", "--oneline", "HEAD"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "show", "--format=%H", "HEAD"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "show", "--format=%gd %gs", "HEAD"], Shape::Branched));
    // Absolute date format only — `--date=relative` reads the wall clock.
    out.push(Case::new("reflog", &["reflog", "show", "--date=iso", "HEAD"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "show", "--grep-reflog=commit", "HEAD"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "list"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "exists", "refs/heads/main"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "exists", "refs/heads/nope"], Shape::Branched));

    // Rewriting subcommands.
    out.push(Case::new("reflog", &["reflog", "expire", "--all", "--expire=now"], Shape::Branched));
    out.push(Case::new(
        "reflog",
        &["reflog", "expire", "--dry-run", "--all", "--expire=now"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "reflog",
        &["reflog", "expire", "--expire-unreachable=now", "refs/heads/main"],
        Shape::Branched,
    ));
    out.push(Case::new("reflog", &["reflog", "delete", "HEAD@{1}"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "delete", "refs/heads/main@{0}"], Shape::Branched));

    // Error paths.
    read_only("reflog", &["reflog", "show", "no-such-ref"], out);
    out.push(Case::new("reflog", &["reflog", "delete", "HEAD@{99}"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "bogus-subcommand"], Shape::Linear));
}

/// `refs`: the newer front-end. `verify` was ported recently, so it gets the
/// widest coverage here; `migrate` has no backend to migrate to and is tested on
/// its error paths only (the `--dry-run` path prints a randomly named temp file,
/// which no implementation can reproduce).
fn refs(out: &mut Vec<Case>) {
    read_only("refs", &["refs", "verify"], out);
    read_only("refs", &["refs", "verify", "--strict"], out);
    read_only("refs", &["refs", "verify", "--verbose"], out);
    out.push(Case::new("refs", &["refs", "verify", "--verbose", "--strict"], Shape::Branched));
    out.push(Case::new("refs", &["refs", "verify"], Shape::Conflicted));
    out.push(Case::new("refs", &["refs", "verify"], Shape::Submodule));

    read_only("refs", &["refs", "list"], out);
    out.push(Case::new("refs", &["refs", "list", "--format=%(refname)"], Shape::Branched));
    out.push(Case::new("refs", &["refs", "list", "--count=1"], Shape::Branched));
    out.push(Case::new("refs", &["refs", "list", "refs/heads/*"], Shape::Branched));
    out.push(Case::new("refs", &["refs", "list", "--include-root-refs"], Shape::Branched));
    out.push(Case::new("refs", &["refs", "list", "--points-at", "HEAD"], Shape::Branched));

    out.push(Case::new("refs", &["refs", "exists", "refs/heads/main"], Shape::Branched));
    out.push(Case::new("refs", &["refs", "exists", "refs/heads/nope"], Shape::Branched));

    // Error paths.
    out.push(Case::new("refs", &["refs"], Shape::Linear));
    out.push(Case::new("refs", &["refs", "bogus-subcommand"], Shape::Linear));
    out.push(Case::new("refs", &["refs", "migrate"], Shape::Branched));
    out.push(Case::new("refs", &["refs", "migrate", "--ref-format=files"], Shape::Branched));
}

/// `name-rev`: the reverse lookup behind `describe --contains` and `bisect`.
fn name_rev(out: &mut Vec<Case>) {
    read_only("name-rev", &["name-rev", "HEAD"], out);
    read_only("name-rev", &["name-rev", "--name-only", "HEAD"], out);
    read_only("name-rev", &["name-rev", "--all"], out);
    out.push(Case::new("name-rev", &["name-rev", "--tags", "HEAD"], Shape::Branched));
    out.push(Case::new("name-rev", &["name-rev", "HEAD~1"], Shape::Branched));
    out.push(Case::new("name-rev", &["name-rev", "--all", "--name-only"], Shape::Branched));
    out.push(Case::new("name-rev", &["name-rev", "--refs=refs/heads/*", "HEAD"], Shape::Branched));
    out.push(Case::new("name-rev", &["name-rev", "--refs=refs/tags/*", "HEAD"], Shape::Branched));
    out.push(Case::new("name-rev", &["name-rev", "--exclude=refs/tags/*", "--all"], Shape::Branched));
    out.push(Case::new("name-rev", &["name-rev", "--all"], Shape::Merged));

    // Error paths: an object that is not in the repo at all, and the same with
    // `--no-undefined`, which turns the "undefined" line into a failure.
    read_only("name-rev", &["name-rev", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"], out);
    read_only(
        "name-rev",
        &["name-rev", "--no-undefined", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"],
        out,
    );
    // Empty stdin; see the module note on the runner's null stdin.
    out.push(Case::new("name-rev", &["name-rev", "--annotate-stdin"], Shape::Branched));
    out.push(Case::new("name-rev", &["name-rev", "--stdin"], Shape::Branched));
}

/// `merge-base`: three distinct algorithms behind one command (best base, all
/// bases, independent set), plus the exit-code-only `--is-ancestor`.
fn merge_base(out: &mut Vec<Case>) {
    out.push(Case::new("merge-base", &["merge-base", "main", "feature"], Shape::Branched));
    out.push(Case::new("merge-base", &["merge-base", "--all", "main", "feature"], Shape::Branched));
    out.push(Case::new("merge-base", &["merge-base", "--independent", "main", "feature"], Shape::Branched));
    out.push(Case::new("merge-base", &["merge-base", "--octopus", "main", "feature", "HEAD"], Shape::Branched));
    out.push(Case::new("merge-base", &["merge-base", "--fork-point", "feature"], Shape::Branched));
    out.push(Case::new("merge-base", &["merge-base", "main", "side"], Shape::Merged));
    out.push(Case::new("merge-base", &["merge-base", "HEAD^1", "HEAD^2"], Shape::Merged));
    out.push(Case::new("merge-base", &["merge-base", "--all", "HEAD^1", "HEAD^2"], Shape::Merged));
    out.push(Case::new("merge-base", &["merge-base", "HEAD", "HEAD"], Shape::Linear));

    // `--is-ancestor` is pure exit code, and both directions matter: one is a
    // true ancestor relation, the reverse is not.
    out.push(Case::new("merge-base", &["merge-base", "--is-ancestor", "main", "feature"], Shape::Branched));
    out.push(Case::new("merge-base", &["merge-base", "--is-ancestor", "feature", "main"], Shape::Branched));

    // Error paths.
    out.push(Case::new("merge-base", &["merge-base", "main", "no-such-rev"], Shape::Branched));
    out.push(Case::new("merge-base", &["merge-base", "HEAD"], Shape::Linear));
}

/// `rev-list` flags the base corpus does not reach. It only covers `HEAD`,
/// `--count` and `--max-count`, which leaves the traversal-shaping and
/// output-shaping flags — the ones packfile and CI tooling actually pass —
/// entirely unmeasured.
fn rev_list_gaps(out: &mut Vec<Case>) {
    let merged = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("rev-list", args, Shape::Merged));
    };
    let branched = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("rev-list", args, Shape::Branched));
    };

    // Ref-set selectors.
    merged(&["rev-list", "--all"], out);
    branched(&["rev-list", "--branches"], out);
    branched(&["rev-list", "--tags"], out);
    branched(&["rev-list", "--all", "--count"], out);

    // Traversal shaping.
    merged(&["rev-list", "--reverse", "HEAD"], out);
    merged(&["rev-list", "--topo-order", "HEAD"], out);
    merged(&["rev-list", "--date-order", "HEAD"], out);
    merged(&["rev-list", "--first-parent", "HEAD"], out);
    merged(&["rev-list", "--merges", "HEAD"], out);
    merged(&["rev-list", "--no-merges", "HEAD"], out);
    merged(&["rev-list", "--max-parents=1", "HEAD"], out);
    merged(&["rev-list", "--min-parents=2", "HEAD"], out);
    merged(&["rev-list", "--no-walk", "HEAD", "main"], out);
    merged(&["rev-list", "--ancestry-path", "HEAD~1..HEAD"], out);
    merged(&["rev-list", "--simplify-by-decoration", "HEAD"], out);
    merged(&["rev-list", "--bisect", "HEAD"], out);
    branched(&["rev-list", "HEAD", "--", "src/lib.rs"], out);

    // Range syntax. `A..B` is in the base corpus by way of `log`; the symmetric
    // difference and explicit `--not` forms are not.
    branched(&["rev-list", "main..feature"], out);
    branched(&["rev-list", "main...feature"], out);
    branched(&["rev-list", "feature", "--not", "main"], out);
    branched(&["rev-list", "--left-right", "main...feature"], out);
    branched(&["rev-list", "--count", "--left-right", "main...feature"], out);
    merged(&["rev-list", "--cherry-mark", "main...side"], out);
    merged(&["rev-list", "--boundary", "HEAD~1..HEAD"], out);

    // Output shaping.
    merged(&["rev-list", "--parents", "HEAD"], out);
    merged(&["rev-list", "--children", "HEAD"], out);
    merged(&["rev-list", "--header", "HEAD"], out);
    merged(&["rev-list", "--pretty=oneline", "HEAD"], out);
    merged(&["rev-list", "--format=%H", "HEAD"], out);
    merged(&["rev-list", "--quiet", "HEAD"], out);
    merged(&["rev-list", "--disk-usage", "HEAD"], out);

    // Object walks — what `pack-objects` is fed.
    merged(&["rev-list", "--objects", "HEAD"], out);
    merged(&["rev-list", "--in-commit-order", "--objects", "HEAD"], out);
    branched(&["rev-list", "--objects", "--filter=blob:none", "HEAD"], out);
    branched(&["rev-list", "--missing=allow-any", "HEAD"], out);

    // Commit filters. Dates are safe because `env::harden` pins the committer
    // clock, so both bounds land on a fixed side of the fixture's timestamps.
    branched(&["rev-list", "--grep=feature", "--all"], out);
    branched(&["rev-list", "--author=parity", "--count", "HEAD"], out);
    branched(&["rev-list", "--since=2000-01-01", "HEAD"], out);
    branched(&["rev-list", "--until=2000-01-01", "HEAD"], out);

    // Empty stdin; see the module note on the runner's null stdin.
    branched(&["rev-list", "--stdin"], out);
}

/// The small commands that report on the binary or the repository itself.
fn misc(out: &mut Vec<Case>) {
    // check-ref-format: pure string validation, no repository access, so every
    // divergence here is a rule the port did not implement.
    let lin = |cmd: &'static str, args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new(cmd, args, Shape::Linear));
    };
    lin("check-ref-format", &["check-ref-format", "refs/heads/main"], out);
    lin("check-ref-format", &["check-ref-format", "heads/main"], out);
    lin("check-ref-format", &["check-ref-format", "refs/heads/bad..name"], out);
    lin("check-ref-format", &["check-ref-format", "refs/heads/name.lock"], out);
    lin("check-ref-format", &["check-ref-format", "refs/heads/"], out);
    lin("check-ref-format", &["check-ref-format", "refs/heads/x y"], out);
    lin("check-ref-format", &["check-ref-format", "refs/heads/back\\slash"], out);
    lin("check-ref-format", &["check-ref-format", "--allow-onelevel", "main"], out);
    lin("check-ref-format", &["check-ref-format", "--allow-onelevel", "HEAD"], out);
    lin("check-ref-format", &["check-ref-format", "--refspec-pattern", "refs/heads/*"], out);
    lin("check-ref-format", &["check-ref-format", "--normalize", "refs/heads//main"], out);
    lin("check-ref-format", &["check-ref-format", "--normalize", "refs/heads/foo/../bar"], out);
    lin("check-ref-format", &["check-ref-format", "--normalize", "--allow-onelevel", "//foo"], out);
    lin("check-ref-format", &["check-ref-format", "--branch", "main"], out);
    lin("check-ref-format", &["check-ref-format", "--branch", "@{-1}"], out);
    lin("check-ref-format", &["check-ref-format", "--branch", "nonexistent-branch"], out);
    lin("check-ref-format", &["check-ref-format"], out);

    // version: the string every build script greps.
    lin("version", &["version"], out);
    lin("version", &["version", "--build-options"], out);

    // repo: structured repository facts, added in recent git.
    lin("repo", &["repo"], out);
    lin("repo", &["repo", "info"], out);
    lin("repo", &["repo", "info", "--keys"], out);
    lin("repo", &["repo", "info", "--all"], out);
    lin("repo", &["repo", "info", "references.format"], out);
    lin("repo", &["repo", "info", "layout.bare"], out);
    lin("repo", &["repo", "info", "--format=nul", "references.format"], out);
    lin("repo", &["repo", "structure"], out);
    lin("repo", &["repo", "structure", "--format=lines"], out);
    lin("repo", &["repo", "info", "no.such.key"], out);
    lin("repo", &["repo", "bogus-subcommand"], out);
    out.push(Case::new("repo", &["repo", "info", "--all"], Shape::Submodule));

    // format-rev is stdin-driven and the runner supplies none, so these compare
    // only the argument parsing and the empty-input path.
    lin("format-rev", &["format-rev"], out);
    out.push(Case::new("format-rev", &["format-rev", "--format=%H"], Shape::Branched));
    out.push(Case::new("format-rev", &["format-rev", "--stdin-mode=oid", "--format=%H"], Shape::Branched));
    out.push(Case::new("format-rev", &["format-rev", "--bogus-flag"], Shape::Linear));
}
