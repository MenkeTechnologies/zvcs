//! The two search/replay state machines: `bisect` (with `rev-list`'s bisection
//! plumbing under it) and `replay`.
//!
//! # What the rest of the corpus already owns, and what is left
//!
//! `bisect` is the most heavily covered verb in this harness and this module is
//! deliberately the *thin* half of it. The division:
//!
//!  * `corpus::exit_codes` owns the status codes — `start --nosuchopt` (1),
//!    `start HEAD HEAD` (1), `start main main~4 -- nosuchpath` (**4**, the one
//!    place that code is produced at all), the same start over a dirty worktree
//!    (1), and `reset` with no session (0).
//!  * `corpus::misc_commands` owns the front door: `-h`, `help`, an unknown
//!    subcommand, nine `start` spellings across `Branched`/`Conflicted`/
//!    `Detached`/`Dirty`/`Merged`, the verb-before-`start` set, `terms`,
//!    `visualize`, `run true`, `replay no-such-log`, `log` across the read
//!    shapes, and `reset`/`reset HEAD~1`.
//!  * `corpus::stateful_side_files` owns the **side files**: seven
//!    `bisect replay /dev/stdin` payloads, the `--term-*`/`--first-parent`/
//!    `--no-checkout`/pathspec `start` matrix on `CommitGraph`, the
//!    unrelated-roots and criss-cross starts, the pre-session refusals, and the
//!    linked-worktree pair. Its module header documents that `probe_op_state`
//!    reads all nine `BISECT_*` root files, which is what makes every case below
//!    that ends in a refusal worth writing.
//!  * `corpus::sequences` owns the five multi-step workflows — start to verdict
//!    to reset, custom terms with a `skip`, and three `bisect run` drives.
//!  * `corpus::history_rewrite` owns `replay` in ten forms: `--onto`,
//!    `--advance`, `--contained`, `--ref-action=print`, and three error paths,
//!    all on `Branched`/`Merged`.
//!  * `corpus::history_query` owns `rev-list --bisect`, `--bisect-vars` and
//!    `--bisect-all` — each asked exactly once, as `<flag> HEAD`, on
//!    `Shape::Packed`; `corpus::plumbing_refs` repeats `--bisect HEAD` on
//!    `Merged`.
//!
//! Four things none of them reach, and this module is those four:
//!
//!  1. **The midpoint algorithm over a *range* and over a merge.** Every
//!     existing `rev-list --bisect*` case names one positive ref and no negative
//!     one, so the interval is always "the whole history" and the halving is only
//!     ever measured from a root. `--bisect-vars`' six counters
//!     (`bisect_nr`/`good`/`bad`/`all`/`steps`) and `--bisect-all`'s per-commit
//!     `(dist=N)` are where an off-by-one in that halving prints as a number
//!     rather than as a commit that happens to still be right.
//!  2. **The terminal states of the search.** A `bisect replay` log is the only
//!     single invocation that can drive the whole machine, and the four ways a
//!     session ends — a verdict (`… is the first 'bad' commit` plus a full
//!     `show`), `There are only 'skip'ped commits left to test.` (exit **2**), a
//!     good that is not an ancestor of the bad, and
//!     `Bisecting: a merge base must be tested` — were each reachable only from a
//!     sequence, and only two of them are.
//!  3. **The replay log's own grammar.** `bisect_replay` re-dispatches each
//!     line; what it does with a line it cannot dispatch is a control-flow edge
//!     nothing measured. Operands are `sq_dequote`d, so a *hand-written* log with
//!     unquoted revs is not the same input as the one git writes — and the corpus
//!     had only the quoted form.
//!  4. **`replay`'s remaining half.** `--revert`, `--ref` and the pseudo-ref
//!     ranges (`--all`, `--branches`, `a...b`) are in stock's usage string and in
//!     no case.
//!
//! # What is not measurable here, and why
//!
//!  * **Every `bisect` verb that needs a session already open** — `good`, `bad`,
//!    `skip`, `next`, `terms`, `log`, `visualize`, `run`, `reset <commit>` — is
//!    out of reach of a `Case`, which is one argv against a pristine copy. The
//!    only single invocation that both opens a session and steps it is `replay`,
//!    which is why this module leans on it so hard. The rest is a
//!    [`crate::runner::Sequence`], and **`corpus::sequences::sequences()` is the
//!    only registration point for one** — `corpus::sequences` calls each family
//!    function itself and `corpus.rs` calls only `sequences::sequences()`, so a
//!    module exposing `pub fn cases(&mut Vec<Case>)` structurally cannot add a
//!    sequence. That is a limit of the corpus's shape, not of the cases; the
//!    multi-step coverage this module would otherwise carry is named in the
//!    report instead of faked into a single-shot case.
//!  * **`bisect visualize` with a session open.** `env::harden` pins
//!    `GIT_EDITOR`/`GIT_SEQUENCE_EDITOR` to `true`, but `visualize` consults
//!    neither — it runs `gitk` or `log`. The pins are therefore not what makes it
//!    unreachable; the missing session is, for the reason above.
//!  * **`bisect run` with a session open**, same reason. `sequences.rs` already
//!    records the verdict there (`'bisect run' is not supported`).
//!  * **A shape without a midpoint.** `Linear` is one commit, `Branched` three
//!    and `Merged` four, and `Detached`/`Dirty`/`Conflicted` are shorter still: a
//!    bisect over any of them has an interval of at most one commit, so every
//!    implementation of "halve the range" returns the same answer and the case
//!    measures argument parsing. The bisections below run on `Packed` (nine
//!    commits on one line — the deepest linear history in the fixture set),
//!    `CommitGraph` (eleven commits, a merge, and a fork that is not an
//!    ancestor of the tip) and `CrissCross` (two incomparable merge bases).
//!
//! # Reproducing a payload by hand
//!
//! Every `bisect replay` case takes its log on stdin through `/dev/stdin`, the
//! mechanism `corpus::stateful_side_files` established. Write the payload to a
//! file, run `git bisect replay <file>` in a copy of the named shape under
//! `env::harden`'s environment, and the case reproduces.
//!
//! Object ids in the payloads are `Shape::Packed`'s commits, which are constants
//! of the fixture: `fixture.rs` builds it from fixed content under
//! `env::FIXED_DATE`, and `fixture::tests::shapes_build_reproducibly` fails if
//! any of them moves. They are spelled in full because a replay log line becomes
//! a ref name verbatim (`git bisect good <rev>` -> `refs/bisect/good-<rev>`), so
//! an abbreviation or a `~` suffix would be a refusal rather than an answer —
//! the case `stateful_side_files::REPLAY_BAD_REFNAME` already pins.

use crate::fixture::Shape;
use crate::runner::Case;

/// [`Case::strict`] with a stdin payload, as `stateful_side_files` spells it.
///
/// Repeated rather than shared because the two modules are edited
/// independently and a helper reaching across them would make one a
/// compile-time dependency of the other for four lines.
fn strict_stdin(cmd: &'static str, args: &[&str], shape: Shape, stdin: &'static [u8]) -> Case {
    Case { compare_stderr: true, ..Case::with_stdin(cmd, args, shape, stdin) }
}

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    bisect_plumbing(out);
    replay_terminal_states(out);
    replay_log_grammar(out);
    start_refusals(out);
    tag_operands(out);
    git_replay(out);
}

// ---------------------------------------------------------------------------
// `Shape::Packed`'s commits, oldest first
// ---------------------------------------------------------------------------
//
// `main` is `initial` plus eight `packed: revision N` commits — nine on one
// line, the deepest single line of history in the fixture set. The replay
// payloads below spell these ids inline rather
// than through constants, because a payload is a `b"…"` literal and a byte
// string cannot interpolate one. The table is here so a reader can name the
// commit an id belongs to without opening the fixture:
//
//   edfab1b71619a22120a8da1a3d85d68e0200290a  initial          (the root)
//   61bfd1614aed634159d7225a3c8a6251ec342f63  packed: revision 0
//   26a757eff25b17a5bab86f3dd8e92e1bd53af516  packed: revision 1
//   4a4a7a961ddcc7ca4cabc74f7969c7ae85ba8739  packed: revision 2
//   959b5ebb1c658cb966b8a06a458a2ca223f83f72  packed: revision 3  (midpoint of the whole line)
//   342199d662e564993f93c075543367b420aa4353  packed: revision 4
//   702b297ba0275a1994cfac22c606f5432e6b95e4  packed: revision 5  (midpoint of `main ^main~4`)
//   68b74d4f7cad1e11fcdb8d5c05ef6532e529b4de  packed: revision 6
//   fc80c5089e77dc377764ea218cc00b88ba12fb7a  packed: pack files … (the tip of `main`)
//
// `Shape::Whitespace`'s six, used by the two dirty-checkout payloads:
//
//   38f94033401a2f7290d6d43f76cccb82e79f3512  whitespace: one edit amid churn (tip)
//   35a528b09e12b349e0b9d9ea3ad22af5d251223c  whitespace: trailing blanks
//
// ---------------------------------------------------------------------------
// rev-list: the midpoint algorithm itself
// ---------------------------------------------------------------------------

/// `rev-list --bisect`, `--bisect-vars` and `--bisect-all` over intervals that
/// are not "the whole history".
///
/// This is where an off-by-one in the search shows up as a number instead of as
/// a commit. `--bisect` prints one id and is therefore a coarse instrument: on a
/// nine-commit line, picking the wrong side of the middle still prints *an* id
/// and only a second bisection would notice. `--bisect-vars` prints the five
/// counters the answer was derived from (`bisect_nr` — how many revisions are
/// left after this one, `bisect_good`/`bisect_bad` — the two sub-interval sizes,
/// `bisect_all`, and `bisect_steps` — the predicted remaining steps), and
/// `--bisect-all` prints *every* candidate with its distance from the ideal
/// halving. Either of those disagrees the moment the weighting does.
///
/// The negative refs are what `history_query`'s three rows do not have: with
/// `<flag> HEAD` alone the interval is the whole history, and on `Packed` stock
/// answers `bisect_all=9`, `bisect_good=4`, `bisect_bad=3`. `main ^main~4` cuts
/// it to `bisect_all=4` with `bisect_good=1` and `bisect_bad=1` — a different
/// weighting of a different set, from the same code.
///
/// `CommitGraph` and `CrissCross` are here because a bisection is a *graph*
/// operation and a line cannot separate the implementations: `main ^cg-loose`
/// spans a fork that is not an ancestor of the tip, `main ^cg-side` cuts under a
/// merge, and `cc-left ^cc-b` crosses two incomparable merge bases. `Octopus`
/// puts a four-parent merge inside the interval, which is the one place the
/// "count the reachable set once per candidate" step has more than two edges to
/// follow out of a commit.
fn bisect_plumbing(out: &mut Vec<Case>) {
    for flag in ["--bisect", "--bisect-vars", "--bisect-all"] {
        // A bounded interval on the deepest linear history: the only case shape
        // in which `bisect_good` and `bisect_bad` are both non-trivial.
        out.push(Case::new("rev-list", &["rev-list", flag, "main", "^main~4"], Shape::Packed));
        out.push(Case::new("rev-list", &["rev-list", flag, "main", "^main~7"], Shape::Packed));
        // The same interval spelled as a range and with `--not`, which reach the
        // same walk through two other paths in `revision.c`'s argument parser.
        out.push(Case::new("rev-list", &["rev-list", flag, "main~4..main"], Shape::Packed));
        out.push(Case::new("rev-list", &["rev-list", flag, "--not", "main~4", "main"], Shape::Packed));
        // A merge inside the interval, and the same interval with the second
        // parent cut away — the pair that separates a real bisection from a walk
        // along the first-parent line.
        out.push(Case::new("rev-list", &["rev-list", flag, "main", "^cg-loose"], Shape::CommitGraph));
        out.push(Case::new(
            "rev-list",
            &["rev-list", flag, "--first-parent", "main", "^cg-loose"],
            Shape::CommitGraph,
        ));
        out.push(Case::new("rev-list", &["rev-list", flag, "main", "^cg-side"], Shape::CommitGraph));
        // Two incomparable merge bases, and a four-parent merge.
        out.push(Case::new("rev-list", &["rev-list", flag, "cc-left", "^cc-b"], Shape::CrissCross));
        out.push(Case::new("rev-list", &["rev-list", flag, "main", "^oct-a"], Shape::Octopus));
        // Narrowed by a pathspec: the candidate set is the commits that touched
        // the path, so the midpoint is a function of the diff and not only of the
        // graph. `big.txt` is rewritten by seven of `Packed`'s nine commits and
        // `packs/` by the tip alone.
        out.push(Case::new("rev-list", &["rev-list", flag, "main", "--", "big.txt"], Shape::Packed));
    }

    // `--bisect-all` prints ref decoration beside the tip (`(HEAD -> main,
    // dist=0)`), so it is the one of the three whose output depends on what else
    // points at a candidate. `AmbiguousRef` holds four names that live in two ref
    // namespaces at once, and `TagChain` a tag pointing at a tag pointing at a
    // tag — two decorations no other bisection case can produce.
    out.push(Case::new("rev-list", &["rev-list", "--bisect-all", "HEAD"], Shape::AmbiguousRef));
    out.push(Case::new("rev-list", &["rev-list", "--bisect-all", "HEAD"], Shape::TagChain));
    // The two degenerate intervals. `main ^main` is empty — stock prints nothing
    // and exits **1**, the only non-zero `--bisect-vars` in the corpus — and
    // `main ^main~1` holds exactly one commit, where stock answers
    // `bisect_good=-1`. A negative counter is the kind of value an
    // implementation written from the description rather than from the code
    // rounds up to zero.
    out.push(Case::new("rev-list", &["rev-list", "--bisect-vars", "main", "^main"], Shape::Packed));
    out.push(Case::new("rev-list", &["rev-list", "--bisect-vars", "main", "^main~1"], Shape::Packed));
    // The three flags combined, which no case has ever spelled. Measured on
    // stock 2.55.0 over `Packed`: `--bisect --bisect-all` prints the
    // `--bisect-all` listing alone, and `--bisect-all --bisect-vars` prints the
    // listing, then a `------` separator, then the whole `bisect_*` block — one
    // output built from both, not a choice between them.
    out.push(Case::new("rev-list", &["rev-list", "--bisect", "--bisect-all", "main"], Shape::Packed));
    out.push(Case::new("rev-list", &["rev-list", "--bisect-all", "--bisect-vars", "main"], Shape::Packed));
    // Bisection beside the counting and ordering modes it has to compose with:
    // `--count` reduces the one-line answer to `1`, and `--reverse` turns the
    // candidate listing round so the `dist=0` tip prints first.
    out.push(Case::new("rev-list", &["rev-list", "--bisect", "--count", "main"], Shape::Packed));
    out.push(Case::new("rev-list", &["rev-list", "--bisect-all", "--reverse", "main"], Shape::Packed));
}

// ---------------------------------------------------------------------------
// bisect replay: driving the state machine to each of its terminal states
// ---------------------------------------------------------------------------

/// A whole search, answered down to a verdict: `bad` at the tip, `good` at the
/// root, then three narrowing answers.
///
/// Ends with `<rev> is the first 'bad' commit` followed by a full `show` of that
/// commit — a header, an author line, a date, the subject and a diffstat, none
/// of which any single-invocation case in this corpus has ever printed.
const VERDICT: &[u8] = b"git bisect start\n\
    git bisect bad fc80c5089e77dc377764ea218cc00b88ba12fb7a\n\
    git bisect good edfab1b71619a22120a8da1a3d85d68e0200290a\n\
    git bisect bad 959b5ebb1c658cb966b8a06a458a2ca223f83f72\n\
    git bisect good 26a757eff25b17a5bab86f3dd8e92e1bd53af516\n\
    git bisect bad 4a4a7a961ddcc7ca4cabc74f7969c7ae85ba8739\n";

/// The same search under **`new`/`old`**, git's built-in second spelling of the
/// two terms, which needs no `--term-*` on the start line.
///
/// Every existing custom-terms case renames the terms; this one uses the pair
/// git already knows, which takes a different branch in `bisect_state` (the
/// terms are not written by the user, but `refs/bisect/old-<rev>` still is).
const VERDICT_NEW_OLD: &[u8] = b"git bisect start\n\
    git bisect new fc80c5089e77dc377764ea218cc00b88ba12fb7a\n\
    git bisect old edfab1b71619a22120a8da1a3d85d68e0200290a\n\
    git bisect new 959b5ebb1c658cb966b8a06a458a2ca223f83f72\n\
    git bisect old 26a757eff25b17a5bab86f3dd8e92e1bd53af516\n";

/// The interval narrowed to two commits and then the only untested one skipped,
/// which is the answer git cannot give: `There are only 'skip'ped commits left
/// to test.` / `The first 'bad' commit could be any of:` / `We cannot bisect
/// more!`, at exit **2**.
///
/// Exit **2** is not a code `exit_codes.rs` reaches for this verb: its header
/// singles out `bisect`'s **4** as the verb-specific one and nothing in it
/// produces a 2. The candidate list has an order of its own: measured on stock
/// 2.55.0 over this payload the two candidates come out `68b74d4f…` then
/// `fc80c508…`, oldest first. The order is part of the answer, so a port that
/// lists the same two ids the other way round is caught here and nowhere else.
const ONLY_SKIPPED: &[u8] = b"git bisect start\n\
    git bisect bad fc80c5089e77dc377764ea218cc00b88ba12fb7a\n\
    git bisect good 702b297ba0275a1994cfac22c606f5432e6b95e4\n\
    git bisect skip 68b74d4f7cad1e11fcdb8d5c05ef6532e529b4de\n";

/// A `skip` that does **not** end the search: the range still has an untested
/// commit, so git moves to a different one in the same interval and records a
/// `# skip:` line rather than a bound.
const SKIP_CONTINUES: &[u8] = b"git bisect start\n\
    git bisect bad fc80c5089e77dc377764ea218cc00b88ba12fb7a\n\
    git bisect good edfab1b71619a22120a8da1a3d85d68e0200290a\n\
    git bisect skip 959b5ebb1c658cb966b8a06a458a2ca223f83f72\n\
    git bisect skip 26a757eff25b17a5bab86f3dd8e92e1bd53af516\n";

/// An inconsistent session: the `good` rev is a **descendant** of the `bad` one,
/// so the interval is empty from the wrong end.
///
/// `Some 'good' revs are not ancestors of the 'bad' rev.` on stderr at exit 1,
/// with the session's own files left behind — the check runs after the refs are
/// written, so what survives is as much of the answer as the message is.
const GOOD_AFTER_BAD: &[u8] = b"git bisect start\n\
    git bisect bad 26a757eff25b17a5bab86f3dd8e92e1bd53af516\n\
    git bisect good fc80c5089e77dc377764ea218cc00b88ba12fb7a\n";

/// The **merge-base** path: `main` and `cg-loose` fork below the tip, so neither
/// is an ancestor of the other and git has to test their base first —
/// `Bisecting: a merge base must be tested`, at exit 1.
///
/// Quoted operands, because that is the form git's own log carries and the form
/// the replay's `sq_dequote` accepts; the unquoted spelling is a separate case
/// below and a different question.
const MERGE_BASE_FIRST: &[u8] = b"git bisect start 'main' 'cg-loose'\n";

/// The same topology through two incomparable merge bases.
const MERGE_BASE_CRISS_CROSS: &[u8] = b"git bisect start 'cc-left' 'cc-right'\n";

/// The merge-base path with `--first-parent`, which cuts the second parent out
/// of the walk and therefore has a *different* base to test.
const MERGE_BASE_FIRST_PARENT: &[u8] = b"git bisect start '--first-parent' 'main' 'cg-loose'\n";

/// A session that is opened and then never answered: `start` alone with a
/// pathspec, so `BISECT_NAMES` is the only thing in it that is not empty.
///
/// `stateful_side_files::REPLAY_START_ONLY` is bare `start` on `Branched`; the
/// pathspec is what this adds, and it is the field a replay has to carry through
/// `sq_dequote` as three tokens rather than one.
const START_WITH_PATHSPEC: &[u8] = b"git bisect start 'main' 'main~4' '--' 'big.txt'\n";

/// A search whose third step lands on a commit the worktree will not accept:
/// `Whitespace` keeps an unstaged edit to `ws/indent.c`, and every commit in the
/// interval rewrites that file.
///
/// Stock prints `Bisecting: 0 revisions left to test after this (roughly 1
/// step)`, attempts the checkout, is refused, and stops at exit 1 — the
/// `[<oid>] <subject>` line that normally follows is *not* printed, because the
/// move it would report did not happen.
const DIRTY_CHECKOUT_REFUSED: &[u8] = b"git bisect start\n\
    git bisect bad 38f94033401a2f7290d6d43f76cccb82e79f3512\n\
    git bisect good edfab1b71619a22120a8da1a3d85d68e0200290a\n\
    git bisect bad 35a528b09e12b349e0b9d9ea3ad22af5d251223c\n";

/// The same interval opened with `--no-checkout`, the mode that exists so a
/// bisection can run over a worktree it must not touch: `BISECT_HEAD` records
/// where the search is and `HEAD` never moves.
const NO_CHECKOUT_OVER_DIRT: &[u8] =
    b"git bisect start '--no-checkout' '38f94033401a2f7290d6d43f76cccb82e79f3512' 'edfab1b71619a22120a8da1a3d85d68e0200290a'\n";

/// The four terminal states of a bisection, and the two `skip` outcomes, each
/// driven to its end by one invocation.
///
/// `bisect replay` is the only verb that can do this: `builtin/bisect.c`'s
/// `bisect_replay` reads the log line by line and re-dispatches each one
/// internally, so a single case runs `start`, `bisect_state` once per answer,
/// and the `bisect_next` that follows each. Everything below is therefore a
/// *whole search* scored as one row — and the row's value is that the answer at
/// the end is only correct if every halving on the way to it was.
///
/// All of them are `strict`: three of the six put their whole answer on stderr
/// or split it across both streams, and for a search that ends in a refusal the
/// message is the result.
fn replay_terminal_states(out: &mut Vec<Case>) {
    let arg = &["bisect", "replay", "/dev/stdin"];
    out.push(strict_stdin("bisect", arg, Shape::Packed, VERDICT));
    out.push(strict_stdin("bisect", arg, Shape::Packed, VERDICT_NEW_OLD));
    out.push(strict_stdin("bisect", arg, Shape::Packed, ONLY_SKIPPED));
    out.push(strict_stdin("bisect", arg, Shape::Packed, SKIP_CONTINUES));
    out.push(strict_stdin("bisect", arg, Shape::Packed, GOOD_AFTER_BAD));
    out.push(strict_stdin("bisect", arg, Shape::CommitGraph, MERGE_BASE_FIRST));
    out.push(strict_stdin("bisect", arg, Shape::CrissCross, MERGE_BASE_CRISS_CROSS));
    out.push(strict_stdin("bisect", arg, Shape::CommitGraph, MERGE_BASE_FIRST_PARENT));
    out.push(strict_stdin("bisect", arg, Shape::Packed, START_WITH_PATHSPEC));

    // A search whose next step cannot be checked out, because the worktree has
    // an edit to a path that step would overwrite.
    //
    // Nothing else in the corpus can ask this. `exit_codes.rs` covers the
    // *opening* refusal (`bisect start main main~4` over `Whitespace`, exit 1),
    // where nothing has happened yet; here the session is three answers in and
    // the refusal lands on a `bisect_next` that has already decided where to go.
    // The answer has two halves and stock prints only the first: measured on
    // stock 2.55.0 and on git 2.50.1, `Bisecting: 0 revisions left to test after
    // this (roughly 1 step)` is printed, the checkout is refused, and the
    // `[<oid>] <subject>` line that names where the search went is *not*. A port
    // that prints it anyway reports a move it did not make.
    out.push(strict_stdin("bisect", arg, Shape::Whitespace, DIRTY_CHECKOUT_REFUSED));
    // The same interval with `--no-checkout`, which is how the refusal above is
    // supposed to be avoided: `BISECT_HEAD` is written instead of `HEAD` moving,
    // so the edit is never in the way and the search proceeds over dirt.
    out.push(strict_stdin("bisect", arg, Shape::Whitespace, NO_CHECKOUT_OVER_DIRT));
    // A log naming a well-formed object id that this repository does not have.
    // Both sides refuse; they refuse in different *places* — stock resolves a
    // 40-hex string without asking the object store and fails when the ref is
    // written, so its message names the ref — which is why this row is strict.
    out.push(strict_stdin("bisect", arg, Shape::Dirty, VERDICT));
}

// ---------------------------------------------------------------------------
// bisect replay: the log's own grammar
// ---------------------------------------------------------------------------

/// A `start` line carrying **unquoted** operands.
///
/// The corpus had only the quoted form, which is what git itself writes, so the
/// dequoting was never separable from the parsing. It is a real input: a replay
/// log is a plain text file people edit and hand-write, and `git bisect start
/// <bad> <good>` without quotes is the spelling a person types.
///
/// Both stock 2.55.0 and git 2.50.1 replay this as a **bare** `start` — the
/// operands are dropped, no `refs/bisect/*` is written, and the session is left
/// `waiting for both 'good' and 'bad' commits`. Verified by hand against both.
const START_UNQUOTED: &[u8] =
    b"git bisect start fc80c5089e77dc377764ea218cc00b88ba12fb7a edfab1b71619a22120a8da1a3d85d68e0200290a\n";

/// The same, for the `--term-*` options: unquoted, so the custom terms are
/// dropped, so the `broken` line two lines later is not a verb the session
/// knows and the replay stops with `'broken'?? what are you talking about?`.
///
/// The quoted spelling of exactly this log is a working custom-terms session,
/// which is what makes the pair worth having: one byte of quoting is the whole
/// difference between a search and a refusal.
const TERMS_UNQUOTED: &[u8] = b"git bisect start --term-old=fine --term-new=broken\n\
    git bisect broken fc80c5089e77dc377764ea218cc00b88ba12fb7a\n\
    git bisect fine edfab1b71619a22120a8da1a3d85d68e0200290a\n";

/// The quoted twin of [`TERMS_UNQUOTED`]: the same three lines, and a session
/// that runs to a midpoint under the renamed terms.
const TERMS_QUOTED: &[u8] = b"git bisect start '--term-old=fine' '--term-new=broken'\n\
    git bisect broken fc80c5089e77dc377764ea218cc00b88ba12fb7a\n\
    git bisect fine edfab1b71619a22120a8da1a3d85d68e0200290a\n";

/// A verb that is not a bisect verb at all. Stock stops the replay:
/// `error: 'frobnicate'?? what are you talking about?`, exit 1, with the log
/// holding only what the lines before it wrote.
const UNKNOWN_VERB: &[u8] = b"git bisect start\n\
    git bisect frobnicate fc80c5089e77dc377764ea218cc00b88ba12fb7a\n";

/// `reset` inside a log. It is a real bisect subcommand and not a replayable
/// one, so stock refuses it with the state-machine's own message —
/// `error: Invalid command: you're currently in a bad/good bisect` — rather than
/// with the unknown-verb one.
const RESET_IN_LOG: &[u8] = b"git bisect start\n\
    git bisect bad fc80c5089e77dc377764ea218cc00b88ba12fb7a\n\
    git bisect reset\n";

/// `terms` inside a log, before any term has been recorded: stock answers
/// `error: no terms defined` and stops.
const TERMS_IN_LOG: &[u8] = b"git bisect start\n\
    git bisect terms --term-good\n\
    git bisect bad fc80c5089e77dc377764ea218cc00b88ba12fb7a\n";

/// `old` used while the session is running under `good`/`bad`. Mixing the two
/// built-in term sets is the one refusal that is neither an unknown verb nor a
/// bad rev: `error: Invalid command: you're currently in a bad/good bisect`.
const MIXED_TERM_SETS: &[u8] = b"git bisect start\n\
    git bisect bad fc80c5089e77dc377764ea218cc00b88ba12fb7a\n\
    git bisect old edfab1b71619a22120a8da1a3d85d68e0200290a\n";

/// A log written in git's own output format, comment lines and all: `# status:`,
/// `# bad: [<oid>] <subject>` and `# good: …` interleaved with the commands.
///
/// This is what `bisect log` actually prints and therefore what a user actually
/// feeds back in, and no case had ever replayed one — every existing payload is
/// the command lines alone. The comment lines must be skipped, and the log
/// written back out must be git's, not the input echoed.
const LOG_ROUND_TRIP: &[u8] = b"git bisect start\n\
    # status: waiting for both 'good' and 'bad' commits\n\
    # bad: [fc80c5089e77dc377764ea218cc00b88ba12fb7a] packed: pack files at stable paths\n\
    git bisect bad fc80c5089e77dc377764ea218cc00b88ba12fb7a\n\
    # good: [edfab1b71619a22120a8da1a3d85d68e0200290a] initial\n\
    git bisect good edfab1b71619a22120a8da1a3d85d68e0200290a\n";

/// The `git-bisect` spelling, which is how the log looked before the command was
/// a builtin and which the parser still accepts.
const HYPHENATED: &[u8] = b"git-bisect start\n\
    git-bisect bad fc80c5089e77dc377764ea218cc00b88ba12fb7a\n\
    git-bisect good edfab1b71619a22120a8da1a3d85d68e0200290a\n";

/// A log that is not a log: blank lines, leading whitespace, a line naming
/// another command entirely, and a bare word.
const NOT_A_LOG: &[u8] = b"\n   \ngit status\nbisect start\nhello\n";

/// A log whose only line is a comment, and one that is empty. The degenerate
/// inputs: nothing is dispatched, so whether a session is opened at all is the
/// whole answer.
const COMMENT_ONLY: &[u8] = b"# bad: [fc80c5089e77dc377764ea218cc00b88ba12fb7a] tip\n";
/// Zero bytes.
const EMPTY_LOG: &[u8] = b"";

/// What `bisect_replay` does with a line it cannot dispatch.
///
/// Each of these is one line's worth of grammar, and the whole group exists
/// because the answer to "what happens to a line the replay does not understand"
/// is a *control-flow* answer — stop and report, or skip and carry on — that
/// changes the outcome of every log with a typo in it. `stateful_side_files`'s
/// seven payloads are all well-formed, so the edge was unmeasured in both
/// directions.
///
/// Every row is `strict`: the answer is a diagnostic on stderr and an exit code,
/// and the state left behind is how far the replay got before it stopped.
fn replay_log_grammar(out: &mut Vec<Case>) {
    let arg = &["bisect", "replay", "/dev/stdin"];
    out.push(strict_stdin("bisect", arg, Shape::Packed, START_UNQUOTED));
    out.push(strict_stdin("bisect", arg, Shape::Packed, TERMS_UNQUOTED));
    out.push(strict_stdin("bisect", arg, Shape::Packed, TERMS_QUOTED));
    out.push(strict_stdin("bisect", arg, Shape::Packed, UNKNOWN_VERB));
    out.push(strict_stdin("bisect", arg, Shape::Packed, RESET_IN_LOG));
    out.push(strict_stdin("bisect", arg, Shape::Packed, TERMS_IN_LOG));
    out.push(strict_stdin("bisect", arg, Shape::Packed, MIXED_TERM_SETS));
    out.push(strict_stdin("bisect", arg, Shape::Packed, LOG_ROUND_TRIP));
    out.push(strict_stdin("bisect", arg, Shape::Packed, HYPHENATED));
    out.push(strict_stdin("bisect", arg, Shape::Packed, NOT_A_LOG));
    out.push(strict_stdin("bisect", arg, Shape::Packed, COMMENT_ONLY));
    out.push(strict_stdin("bisect", arg, Shape::Packed, EMPTY_LOG));
}

// ---------------------------------------------------------------------------
// bisect start: the term validator, and what it leaves behind when it refuses
// ---------------------------------------------------------------------------

/// `bisect start` refusals whose *whole* difference is the repository left
/// behind.
///
/// Stock checks a term's *word* only after the session has been opened, and the
/// refusal does not undo what it opened. Measured directly on stock 2.55.0 over
/// `Shape::Packed`: `bisect start --term-new=start main main~4` exits 1 with
/// `error: can't use the builtin command 'start' as a term` and leaves behind
/// `refs/bisect/start`, `refs/bisect/good-959b5ebb…`, `BISECT_START=main`, an
/// empty `BISECT_NAMES` and a two-line `BISECT_LOG`. git 2.50.1 leaves the same
/// five things, so the residue is the answer and not a 2.55-only accident.
///
/// Every row here therefore agrees on stdout, on stderr and on the exit code,
/// and the only thing that can separate an implementation is `probe_op_state`
/// plus the ref listing. That is precisely the surface
/// `corpus::stateful_side_files` was built for — its header names "a port that
/// writes `BISECT_START` and *then* notices there is no session" as the defect
/// class — and it holds only the mirror image of these (`start main main`,
/// where both operands are the same rev). The refusals below come out of the
/// *term* validator, which nothing reaches.
///
/// The rows are `strict` because two of them differ in the message as well: an
/// empty term and `..` are rejected on stock by ref-name validation, *after* the
/// session is open (`error: update_ref failed for ref 'refs/bisect/': refusing
/// to update ref with bad name 'refs/bisect/'`), which is a different sentence
/// about a different thing from a term check that runs before it.
fn start_refusals(out: &mut Vec<Case>) {
    // Three different subcommand names, because a port that recognises a shorter
    // list of them passes for one word and not another. All three are valid ref
    // name components, so none of these refusals can be the ref layer's — the
    // rows below where the term is *not* a valid name are separate.
    out.push(Case::strict("bisect", &["bisect", "start", "--term-new=start", "main", "main~4"], Shape::Packed));
    out.push(Case::strict("bisect", &["bisect", "start", "--term-old=terms", "main", "main~4"], Shape::Packed));
    out.push(Case::strict("bisect", &["bisect", "start", "--term-new=log", "main", "main~4"], Shape::Packed));
    // The same word for both terms, in two spellings: an arbitrary one, and one
    // that collides with a built-in term. `please use two different terms`.
    out.push(Case::strict(
        "bisect",
        &["bisect", "start", "--term-new=same", "--term-old=same", "main", "main~4"],
        Shape::Packed,
    ));
    out.push(Case::strict(
        "bisect",
        &["bisect", "start", "--term-new=bad", "--term-old=bad", "main", "main~4"],
        Shape::Packed,
    ));
    // Two terms that are not ref-name material: the empty string and `..`. These
    // fail in the ref layer on stock, after the session is open, so the message
    // names a ref rather than a term.
    out.push(Case::strict("bisect", &["bisect", "start", "--term-new=", "main", "main~4"], Shape::Packed));
    out.push(Case::strict("bisect", &["bisect", "start", "--term-new=..", "main", "main~4"], Shape::Packed));
    // A `--term-new` whose value is split by the shell into a second positional,
    // so the leftovers land in `BISECT_NAMES` as a pathspec: the one row where
    // the refusal's residue is a *pathspec* rather than a ref.
    out.push(Case::strict("bisect", &["bisect", "start", "--term-new=has\\", "space", "main", "main~4"], Shape::Packed));

    // ---- the option spellings that are *not* refusals ----
    // `--term-good`/`--term-bad` are the synonyms of `--term-old`/`--term-new`,
    // and no `start` case in the corpus uses either: the matrix in
    // `stateful_side_files` is written entirely in the `old`/`new` spelling, and
    // the only other appearance of these two words is `misc_commands`' `bisect
    // terms --term-good`, which is an option of a different subcommand. A port
    // that implemented one pair of names passes every existing case and fails
    // this one.
    out.push(Case::new(
        "bisect",
        &["bisect", "start", "--term-good=fine", "--term-bad=broken", "main", "main~4"],
        Shape::Packed,
    ));
    out.push(Case::new(
        "bisect",
        &["bisect", "start", "--term-good", "fine", "--term-bad", "broken", "main", "main~4"],
        Shape::Packed,
    ));
    // Renaming the terms to git's *other* built-in pair, which is legal and is
    // not the same as leaving them unset — `BISECT_TERMS` is written either way,
    // and `refs/bisect/old-<rev>` rather than `refs/bisect/good-<rev>` is what
    // separates the two.
    out.push(Case::new(
        "bisect",
        &["bisect", "start", "--term-new=new", "--term-old=old", "main", "main~4"],
        Shape::Packed,
    ));
    // More than one `good` on the start line, and a start with `--` and no
    // pathspec after it: the two operand shapes `misc_commands`'s nine `start`
    // rows do not have.
    out.push(Case::new("bisect", &["bisect", "start", "main", "main~4", "main~6"], Shape::Packed));
    out.push(Case::new("bisect", &["bisect", "start", "main", "main~4", "--"], Shape::Packed));
    // A start over the whole of the deepest history there is: measured on stock
    // 2.55.0, `Bisecting: 3 revisions left to test after this (roughly 2 steps)`
    // — the only start in the corpus whose prediction is not 0 or 1.
    out.push(Case::new("bisect", &["bisect", "start", "main", "main~8"], Shape::Packed));
    out.push(Case::new("bisect", &["bisect", "start", "--no-checkout", "main", "main~8"], Shape::Packed));
}

// ---------------------------------------------------------------------------
// git replay
// ---------------------------------------------------------------------------

/// `replay`'s half that `corpus::history_rewrite` does not have.
///
/// That module's ten rows cover `--onto`, `--advance`, `--contained` and
/// `--ref-action=print` on `Branched` and `Merged`, plus three error paths. Two
/// whole options in stock's usage string are absent from it — `--revert=<branch>`
/// and `--ref=<ref>` — and so is every *pseudo-ref* revision range, which is the
/// half of `<revision-range>` that is not `a..b`.
///
/// The command writes nothing to the index or the worktree, so with
/// `--ref-action=update` (the default) every one of these is judged on
/// `for-each-ref` alone: the reflog and the ref values are the entire answer,
/// and a row that prints nothing at all is not a row that did nothing.
fn git_replay(out: &mut Vec<Case>) {
    // ---- --revert, in both spellings ----
    // The mode that has no case at all. `--revert=<branch>` replays the range as
    // *reverts* on top of that branch, so the resulting commit ids are functions
    // of the reverse patches and a port that reuses the `--advance` code path
    // produces the range's own trees instead.
    out.push(Case::new("replay", &["replay", "--revert=main", "main~1..feature"], Shape::Branched));
    out.push(Case::new("replay", &["replay", "--revert", "main", "main~1..feature"], Shape::Branched));
    out.push(Case::new("replay", &["replay", "--revert=feature", "main~1..feature"], Shape::Branched));
    out.push(Case::new("replay", &["replay", "--revert=main", "main~4..main"], Shape::Packed));
    // A revert whose range is a single commit, printed rather than written.
    out.push(Case::new(
        "replay",
        &["replay", "--ref-action=print", "--revert=main", "main~1..main"],
        Shape::Packed,
    ));

    // ---- --ref ----
    // Where the result lands when it is not the range's own branch: a name that
    // does not exist yet, one that does, and a fully-qualified spelling beside a
    // bare one. `--ref` is in stock's usage and in no case.
    out.push(Case::new(
        "replay",
        &["replay", "--ref=refs/heads/landed", "--onto", "main~1", "main..feature"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "replay",
        &["replay", "--ref", "refs/heads/main", "--onto", "main~1", "main..feature"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "replay",
        &["replay", "--ref=landed", "--onto", "main~1", "main..feature"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "replay",
        &["replay", "--ref=refs/heads/landed", "--ref-action=print", "--onto", "main~1", "main..feature"],
        Shape::Branched,
    ));

    // ---- pseudo-ref ranges ----
    // `--all` and `--branches` select the range from the ref namespace instead of
    // naming it, which is how `replay` is used to rewrite a whole repository at
    // once. `Cherry` is the shape where it has something to do: `topic` forks
    // below `main`, so `--branches --not main` is a non-empty range and the ref
    // that moves is one the argv never named.
    out.push(Case::new("replay", &["replay", "--onto", "main", "--branches", "--not", "main"], Shape::Cherry));
    out.push(Case::new("replay", &["replay", "--onto", "main", "--all", "--not", "main"], Shape::Cherry));
    out.push(Case::new("replay", &["replay", "--onto", "main~1", "--branches"], Shape::Branched));
    out.push(Case::new("replay", &["replay", "--onto", "main~1", "--all"], Shape::Branched));
    // The symmetric difference, which selects commits on *both* sides of the fork
    // and is a different set from either `a..b` or `b..a`.
    out.push(Case::new("replay", &["replay", "--onto", "main~1", "main...feature"], Shape::Branched));
    out.push(Case::new("replay", &["replay", "--onto", "main", "main...topic"], Shape::Cherry));
    // The `^a b` spelling of `a..b`, which reaches the same set through the other
    // half of `revision.c`'s parser.
    out.push(Case::new("replay", &["replay", "--onto", "main~1", "^main", "feature"], Shape::Branched));

    // ---- ranges that are not straight lines ----
    // A merge inside the range: `fatal: replaying merge commits is not supported
    // yet!`, which the corpus has never asked over a merge with more than two
    // parents.
    out.push(Case::strict("replay", &["replay", "--onto", "oct-a", "main~1..main"], Shape::Octopus));
    out.push(Case::strict("replay", &["replay", "--onto", "cc-a", "cc-b..cc-left"], Shape::CrissCross));
    // A range that crosses a duplicated patch: `topic` and `main` each carry a
    // commit with the other's patch id, so the replay has to decide whether to
    // reapply it.
    out.push(Case::new("replay", &["replay", "--onto", "main", "topic"], Shape::Cherry));
    out.push(Case::new("replay", &["replay", "--advance", "main", "main..topic"], Shape::Cherry));
    // The whole of a deep line replayed onto its own tip, which is the identity
    // rewrite: nine commits re-created with the same trees and new parents.
    out.push(Case::new("replay", &["replay", "--onto", "main", "main~4..main"], Shape::Packed));
    out.push(Case::new("replay", &["replay", "--onto", "main~8", "main~4..main"], Shape::Packed));

    // ---- where it runs, and over what ----
    // `replay` never opens the index, so a worktree with staged, unstaged and
    // untracked changes must come out of it untouched. (`Shape::Worktree` is not
    // here: it holds one commit, so a linked worktree has no range to replay and
    // the row would measure argument parsing.)
    out.push(Case::new("replay", &["replay", "--advance", "main", "main~1..main"], Shape::MergeableDirty));
    // Discovery: the same rewrite driven from a subdirectory and through
    // `GIT_DIR`, the two ways a caller reaches a repository it is not standing in.
    out.push(Case::new("replay", &["replay", "--onto", "main~1", "main..feature"], Shape::Branched).in_dir("src"));
    out.push(
        Case::new("replay", &["replay", "--onto", "main~1", "main..feature"], Shape::Branched)
            .with_env(&[("GIT_DIR", "{repo}/.git")]),
    );

    // ---- refusals ----
    // Two modes at once, an unknown `--ref-action`, and a `--ref` that is not a
    // valid ref name: the argument checks that have no case.
    out.push(Case::strict(
        "replay",
        &["replay", "--onto", "main~1", "--advance", "main", "main..feature"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "replay",
        &["replay", "--onto", "main~1", "--revert", "main", "main..feature"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "replay",
        &["replay", "--ref-action=bogus", "--onto", "main~1", "main..feature"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "replay",
        &["replay", "--ref=refs/heads/..", "--onto", "main~1", "main..feature"],
        Shape::Branched,
    ));
    out.push(Case::strict("replay", &["replay", "--advance", "no-such-branch", "main..feature"], Shape::Branched));
    out.push(Case::strict("replay", &["replay", "--revert=no-such-branch", "main..feature"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// bisect: what a term's operand is allowed to be
// ---------------------------------------------------------------------------

/// `bad` given an **annotated tag**. `v0.2.0` is a tag object whose target is
/// `main`; `v0.1.0` beside it is lightweight and points straight at the same
/// commit, which is what makes the pair separable.
///
/// Stock writes the **tag object's** id into `refs/bisect/bad` — it does not
/// peel — and the session that follows cannot run: it reports
/// `d7277ea97518c8631ff11851f616d1ca422aeef0 was both 'good' and 'bad'` and
/// exits 1. Verified by hand against stock 2.55.0 and git 2.50.1, which agree on
/// the id, the message and the exit code.
const BAD_IS_AN_ANNOTATED_TAG: &[u8] = b"git bisect start\n\
    git bisect bad v0.2.0\n\
    git bisect good edfab1b71619a22120a8da1a3d85d68e0200290a\n";

/// The same tag on the **`good`** side, where the search does run: the boundary
/// is a real commit, and the only thing the tag decides is what
/// `refs/bisect/good-v0.2.0` holds.
const GOOD_IS_AN_ANNOTATED_TAG: &[u8] = b"git bisect start\n\
    git bisect bad feature\n\
    git bisect good v0.2.0\n";

/// A tag pointing at a tag pointing at a tag, on the `good` side.
/// `refs/bisect/good-outermost` holds `outermost`'s own object on stock —
/// the outermost tag, not the middle one and not the commit — so a peel of the
/// wrong depth is as visible here as no peel at all.
const GOOD_IS_A_TAG_CHAIN: &[u8] = b"git bisect start\n\
    git bisect bad main\n\
    git bisect good outermost\n";

/// The lightweight tag as a control: it names the commit directly, so peeling
/// and not peeling are the same answer and this row must pass either way.
const GOOD_IS_A_LIGHTWEIGHT_TAG: &[u8] = b"git bisect start\n\
    git bisect bad feature\n\
    git bisect good v0.1.0\n";

/// What `refs/bisect/<term>[-<name>]` is allowed to point at.
///
/// The whole corpus answers `good`/`bad` with a branch name, a full object id or
/// `HEAD`, all of which are commits already, so "does the implementation peel
/// its operand" had no case: peeling and not peeling produce the same ref
/// everywhere it was asked.
///
/// It is not a detail. `refs/bisect/bad` is read back by every later step, and
/// stock stores the operand's own object — so a `bad` given an annotated tag
/// makes the session **unrunnable** on stock (`… was both 'good' and 'bad'`,
/// exit 1) while an implementation that peels reaches a verdict instead. The two
/// answers are not a formatting difference; they are a search that happens and a
/// search that does not.
///
/// `bisect start <tag> <good>` is deliberately here beside them and is *not* a
/// divergence: `start` resolves its operands through a different path and both
/// sides peel there. Keeping the pair is what localises the question to
/// `bisect_state` rather than to rev parsing in general.
fn tag_operands(out: &mut Vec<Case>) {
    let arg = &["bisect", "replay", "/dev/stdin"];
    out.push(strict_stdin("bisect", arg, Shape::Branched, BAD_IS_AN_ANNOTATED_TAG));
    out.push(strict_stdin("bisect", arg, Shape::Branched, GOOD_IS_AN_ANNOTATED_TAG));
    out.push(strict_stdin("bisect", arg, Shape::Branched, GOOD_IS_A_LIGHTWEIGHT_TAG));
    out.push(strict_stdin("bisect", arg, Shape::TagChain, GOOD_IS_A_TAG_CHAIN));
    // The `start` control: the same annotated tag, resolved by the other path.
    out.push(Case::new(
        "bisect",
        &["bisect", "start", "v0.2.0", "edfab1b71619a22120a8da1a3d85d68e0200290a"],
        Shape::Branched,
    ));
    out.push(Case::new("bisect", &["bisect", "start", "main", "outermost"], Shape::TagChain));
}
