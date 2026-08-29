//! Differential corpus cases for git's **exit codes**, taken as the subject
//! rather than as a side effect of whatever else a case was measuring.
//!
//! Every case here is compared against stock git for stdout, exit code and
//! post-command repository state, and — because a refusal's code and its text
//! are read by the same scripts — almost all of them compare stderr too.
//!
//! # Why a module for a single integer
//!
//! An exit code is the one part of git's interface that has no formatting, no
//! locale and no `--porcelain` variant: `if git diff --quiet; then` is the
//! documented way to ask a question, and a shell reads nothing else. Yet no
//! corpus module had the code as its *subject*. Every other module asks a verb
//! for its output and takes whatever code came back, so a port that answers
//! every verb correctly and normalises every refusal to `1` scores well
//! everywhere and breaks every script in the wild.
//!
//! The defect that motivated this is not hypothetical and is not one verb's.
//! `foreign_lock.rs` reports that the port maps **every lock failure to exit 1**
//! where git exits 128 — `add`, `commit`, `rm --cached`, `reset --hard`,
//! `checkout`, `fetch --depth` — and that the same uniform mapping is
//! accidentally right for two of the verbs it covers, because git answers 1 for
//! a locked `stash push` and 128 for a locked `update-ref`. A corpus organised
//! by verb hits the right answers and the wrong ones in no particular order.
//! Only an enumeration of the *codes* shows the shape of it.
//!
//! # How this divides territory with the per-verb modules
//!
//! The per-verb modules own their verb's ordinary surface, including the
//! refusals that are part of that surface: `config_cmd.rs` pins
//! `config --unset` on a multi-valued key (5) and `config invalidkey v` (2),
//! `stdin_plumbing.rs` pins `mktree`'s malformed-record `die()`s, `mail_patch.rs`
//! pins `apply`'s corrupt-patch path. Those are not repeated here, and where the
//! obvious spelling for a code was already taken this module reaches the same
//! code by a different route and says so above the group.
//!
//! What is left over — and what this module is — is the *taxonomy*:
//!
//!  * **128 — `die()`.** Every `die()` in git exits 128 (`usage.c`). The
//!    ordinary fatal, reached here from six unrelated directions: a bad rev, a
//!    ref that points at an object that is not there, a refusal to overwrite a
//!    name that already exists, an option pair the *builtin body* rejects after
//!    the option table accepted both, an operand where none is allowed, and a
//!    config file that will not parse when written to.
//!  * **129 — `usage()`.** `usage_with_options` (`parse-options.c`) exits 129. A
//!    different *layer* said no: the option table, before the verb's own code
//!    ran. The distinction is real and a port blurs it in both directions — see
//!    [`layer_boundary`], where five spellings of "bad option" produce four
//!    different codes on stock git and two spellings of "these two options are
//!    incompatible" produce two.
//!  * **1 — an ordinary "no".** Not an error at all: the question was asked and
//!    the answer was negative. `diff --exit-code` with differences,
//!    `grep` with no match, `check-ignore` on a path no rule covers,
//!    `merge-base --is-ancestor` on a pair that is not, a cherry-pick that
//!    conflicted. Stdout is often the whole answer and the code merely repeats
//!    it; sometimes, as with `--quiet`, the code *is* the answer.
//!  * **The verb-specific codes**, which are the ones a reader guesses wrong.
//!    `config` answers 1, 2, 3 and 5 for four different kinds of no;
//!    `diff --check` answers 2 alone and **3** combined with `--exit-code`,
//!    because the contributions add rather than compete; `fsck` answers **3**
//!    over a broken repository and **128** over the same one with
//!    `--connectivity-only`; `bisect` has a **4**; `remote` has a **2** for a
//!    name it cannot find, where every other verb in the corpus says 1 or 128.
//!  * **0 — the successes that read like failures.** The other half of the
//!    subject and the half a defensive port loses: `format-patch` over an empty
//!    range, `tag -d` with nothing to delete, `reset --soft --hard`,
//!    `checkout --ours --theirs`, `bisect reset` with no bisect in progress.
//!    Each is 0 with no complaint, and a port that has learned to refuse
//!    loudly answers non-zero and breaks a script that never tested for it.
//!
//! # Codes this module deliberately does not reach
//!
//!  * **A lock failure's 128.** The defect quoted above needs a `.lock` file to
//!    already exist when the command starts, and a case is one argv against a
//!    pristine copy — no fixture ships a lock, and creating one is exactly what
//!    `foreign_lock.rs` exists to do as its own dimension. Every lock case
//!    belongs there; none is duplicated here.
//!  * **`config`'s 255.** Documented as "config file cannot be locked", so it
//!    is the same unreachable premise. Its 3 (invalid config file) *is* reached,
//!    from a `--file` naming a directory.
//!  * **141 (SIGPIPE) and any abort.** `runner.rs:classify` records a killed
//!    child as `code: None` and scores `zvcs.code.is_none()` as
//!    [`crate::runner::Verdict::Crash`] — outside the content comparison
//!    entirely, and a stock side killed the same way falls into the exit-code
//!    branch against a port that exited normally. A case whose *expected*
//!    outcome is a signal therefore cannot be scored as parity in either
//!    direction, so nothing here closes a pipe early or provokes a `BUG()`.
//!
//! # Every code below was observed
//!
//! Stock git 2.55.0, run against the fixture the case names, in the harness's
//! own hardened environment. The code in each group's comment is what that run
//! printed, not what the documentation promises — the two differ more often than
//! is comfortable, which is the point of writing it down.

use crate::fixture::Shape;
use crate::runner::Case;

pub fn cases(out: &mut Vec<Case>) {
    die_128(out);
    usage_129(out);
    layer_boundary(out);
    ordinary_no_1(out);
    not_found_is_not_one_code(out);
    remote_returns_two(out);
    config_documented_codes(out);
    diff_check_bits(out);
    verb_specific_codes(out);
    value_refusals_after_parsing(out);
    zero_that_reads_like_failure(out);
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// A case whose exit code *and* refusal text are both compared.
///
/// The default for this module. A code is a contract and so is the one-line
/// `fatal:`/`error:` beside it: a script branches on the number, a human reads
/// the sentence, and a port that gets the number right by printing something
/// else has half a bug.
fn strict(out: &mut Vec<Case>, shape: Shape, args: &[&'static str]) {
    out.push(Case::strict(args[0], args, shape));
}

/// A case whose exit code is compared and whose stderr is not.
///
/// Reserved for the invocations that answer with a `usage:` block. The
/// harness's standing policy is that a `parse_options()` dump tracks git's own
/// option table rather than its behaviour (see `stdin_plumbing.rs`'s header),
/// so the 129 is pinned and the block is not.
fn loose(out: &mut Vec<Case>, shape: Shape, args: &[&'static str]) {
    out.push(Case::new(args[0], args, shape));
}

// ---------------------------------------------------------------------------
// 128 — die()
// ---------------------------------------------------------------------------

/// The ordinary fatal, reached from four unrelated directions: a name that
/// resolves to nothing, a name that already exists, an option pair the builtin
/// body rejects, and an argument value the builtin body cannot use.
///
/// Observed on stock git 2.55.0, all **128**:
///
/// | invocation | shape | message |
/// |---|---|---|
/// | `cat-file -p deadbeef…` | damaged | `fatal: Not a valid object name deadbeef…` |
/// | `log dangling` | damaged | `fatal: bad object dangling` |
/// | `rev-parse --verify broken-symref` | damaged | `warning: ignoring dangling symref …` + `fatal: Needed a single revision` |
/// | `branch main` | branched | `fatal: a branch named 'main' already exists` |
/// | `checkout -b main` | branched | `fatal: a branch named 'main' already exists` |
/// | `worktree add wt main` | branched | `fatal: 'main' is already used by worktree at …` |
/// | `log --graph --no-walk` | linear | `fatal: options '--no-walk' and '--graph' cannot be used together` |
/// | `update-ref refs/heads/x nosuchobj` | linear | `fatal: nosuchobj: not a valid SHA1` |
/// | `describe --exact-match` | linear | `fatal: No names found, cannot describe anything.` |
/// | `merge-base --is-ancestor HEAD nosuchrev` | linear | `fatal: Not a valid object name nosuchrev` |
/// | `archive --format=nosuch HEAD` | linear | `fatal: Unknown archive format 'nosuch'` |
/// | `branch --set-upstream-to=nosuch/x` | linear | `fatal: the requested upstream branch … does not exist` |
///
/// What a port gets wrong without these, in three sentences. A name that
/// already exists is a `die()` and not a 1 — `branch main`, `checkout -b main`
/// and `worktree add wt main` are the three verbs that refuse it, and the third
/// refuses *after* printing `Preparing worktree` to stdout, so the case pins
/// that a 128 can arrive mid-output. An option pair the revision walker rejects
/// (`log --graph --no-walk`) is a `die()`, not the 129 the same-shaped rejection
/// gets when the option table owns it — see [`layer_boundary`] for the pair that
/// makes that concrete. And `merge-base --is-ancestor`, whose whole contract is
/// to answer 0 or 1, must still exit 128 when an argument does not resolve:
/// "not an ancestor" and "not a revision" are different answers and a script
/// that cannot tell them apart is the reason the code is 128.
///
/// The three `damaged` cases are the object-store half. `deadbeef…` is a
/// well-formed id naming nothing, `dangling` is a *ref* whose target is that
/// id, and `broken-symref` is a symref to a branch that does not exist — three
/// distinct failures that a port reading refs through one helper answers
/// identically.
fn die_128(out: &mut Vec<Case>) {
    strict(out, Shape::Damaged, &["cat-file", "-p", crate::fixture::MISSING_OBJECT]);
    strict(out, Shape::Damaged, &["log", "dangling"]);
    strict(out, Shape::Damaged, &["rev-parse", "--verify", "broken-symref"]);

    strict(out, Shape::Branched, &["branch", "main"]);
    strict(out, Shape::Branched, &["checkout", "-b", "main"]);
    strict(out, Shape::Branched, &["worktree", "add", "wt", "main"]);

    strict(out, Shape::Linear, &["log", "--graph", "--no-walk"]);
    strict(out, Shape::Linear, &["update-ref", "refs/heads/x", "nosuchobj"]);
    strict(out, Shape::Linear, &["describe", "--exact-match"]);
    strict(out, Shape::Linear, &["merge-base", "--is-ancestor", "HEAD", "nosuchrev"]);
    strict(out, Shape::Linear, &["archive", "--format=nosuch", "HEAD"]);
    strict(out, Shape::Linear, &["branch", "--set-upstream-to=nosuch/x"]);
}

// ---------------------------------------------------------------------------
// 129 — usage()
// ---------------------------------------------------------------------------

/// `parse-options` refusing before the verb runs.
///
/// Three shapes of refusal — an option missing its value, a pair the option
/// table will not take together, and a missing operand — all **129** on stock
/// git 2.55.0, listed here split by whether the answer is one line or a whole
/// usage block:
///
/// | invocation | stderr |
/// |---|---|
/// | `config --file` | `error: option \`file' requires a value` |
/// | `for-each-ref --format` | `error: option \`format' requires a value` |
/// | `checkout -b` | `error: switch \`b' requires a value` |
/// | `grep -e` | `error: switch \`e' requires a value` |
/// | `config --get-all --unset a.b` | `error: options '--unset' and '--get-all' cannot be used together` |
/// | `config --list --local --global` | `error: only one config file at a time` |
/// | `symbolic-ref HEAD refs/heads/x y` | `usage: git symbolic-ref …` |
/// | `cat-file` | `usage: git cat-file …` |
/// | `ls-tree` | `usage: git ls-tree …` |
/// | `merge-base --is-ancestor HEAD` | `usage: git merge-base …` |
/// | `branch -d -m foo` | `usage: git branch …` |
/// | `worktree add` | `usage: git worktree add …` |
/// | `notes add --nosuchopt` | `error: unknown option \`nosuchopt'` + block |
/// | `mktag --nosuchopt` | `error: unknown option \`nosuchopt'` + block |
/// | `check-attr diff` | `error: No file specified` + block |
///
/// The first six carry no block and are compared on stderr. The rest end in a
/// `usage:` dump and are not, per the standing policy — but their **129** is
/// pinned either way, and that is the half a port loses: a missing operand is
/// not a `die()`, and `--is-ancestor` given one commit is a usage error while
/// the same option given an unresolvable name is a 128 ([`die_128`]).
///
/// `branch -d -m foo` is the mutually-exclusive case that does not *say* it is
/// one. `builtin/branch.c` counts its mode flags itself instead of asking for
/// the one-line diagnostic, so where `tag -d -l` answers
/// `error: options '-l' and '-d' cannot be used together`, `branch` answers with
/// nothing but the usage block — same code, no sentence. A port that emits the
/// sentence here is not wrong on the code and is wrong on the stream, and a port
/// that treats "no sentence" as "not an error" is wrong on both.
fn usage_129(out: &mut Vec<Case>) {
    // One-line refusals: code and text both pinned.
    strict(out, Shape::Linear, &["config", "--file"]);
    strict(out, Shape::Linear, &["for-each-ref", "--format"]);
    strict(out, Shape::Linear, &["checkout", "-b"]);
    strict(out, Shape::Linear, &["grep", "-e"]);
    strict(out, Shape::Linear, &["config", "--get-all", "--unset", "a.b"]);
    strict(out, Shape::Linear, &["config", "--list", "--local", "--global"]);

    // Usage blocks: code pinned, prose not.
    loose(out, Shape::Linear, &["symbolic-ref", "HEAD", "refs/heads/x", "y"]);
    loose(out, Shape::Linear, &["cat-file"]);
    loose(out, Shape::Linear, &["ls-tree"]);
    loose(out, Shape::Linear, &["merge-base", "--is-ancestor", "HEAD"]);
    loose(out, Shape::Linear, &["branch", "-d", "-m", "foo"]);
    loose(out, Shape::Linear, &["worktree", "add"]);
    loose(out, Shape::Linear, &["notes", "add", "--nosuchopt"]);
    loose(out, Shape::Linear, &["mktag", "--nosuchopt"]);
    loose(out, Shape::Linear, &["check-attr", "diff"]);
}


// ---------------------------------------------------------------------------
// The 128/129 boundary
// ---------------------------------------------------------------------------

/// Two questions asked of several verbs each, where the answer depends on which
/// *layer* refused rather than on what was wrong.
///
/// **"What happens to an option nobody recognises?"** — observed on stock git
/// 2.55.0:
///
/// | invocation | code | what refused |
/// |---|---|---|
/// | `status --nosuchopt` | 129 | `parse-options`: `error: unknown option` + block |
/// | `grep --nosuchopt fn` | 129 | `parse-options`: `error: unknown option` + block |
/// | `log --nosuchopt` | **128** | `revision.c`: `fatal: unrecognized argument: --nosuchopt` |
/// | `rev-parse --nosuchopt` | **0** | nothing refused; it is echoed to stdout |
/// | `bisect start --nosuchopt` | **1** | `error: unrecognized option: '--nosuchopt'` |
///
/// **"What happens to two options that cannot be combined?"** — same sentence,
/// two codes:
///
/// | invocation | code | message |
/// |---|---|---|
/// | `tag -d -l` | 129 | `error: options '-l' and '-d' cannot be used together` |
/// | `clean -n -x -X` | **128** | `fatal: options '-x' and '-X' cannot be used together` |
/// | `log --oneline --graph --reverse` | **128** | `fatal: options '--graph' and '--reverse' cannot be used together` |
/// | `switch --detach -c foo` | **128** | `fatal: '--detach' cannot be used with '-b/-B/--orphan'` |
/// | `branch -d --contains HEAD` | 129 | usage block, no sentence at all |
///
/// This is the group most likely to catch a port and it is unreachable by any
/// per-verb module, because the finding is the disagreement *between* verbs. A
/// port that funnels every rejected option through one handler produces one code
/// for all ten and matches at most four of them, whichever code it picks.
///
/// Each answer has a reason the message itself gives away. `log`'s says
/// `fatal:`, so what refused was a `die()` and not the option table: the token
/// survived `parse-options` and was handed to the revision machinery, which does
/// not know it. `rev-parse` is a filter and not a parser — the unrecognised
/// argument comes back on *stdout* — so the code is 0 and a port that
/// "helpfully" rejects it fails the case twice over, on the code and on the
/// output. `bisect`'s sentence (`error: unrecognized option: '--nosuchopt'`) is
/// spelled like neither of the other two, and so is its status: 1, from the
/// builtin's own return rather than from either standard exit. And the two incompatibility refusals say which layer
/// caught them in their own first word, which is the cheapest possible check on
/// a port: `tag -d -l` writes `error: options '-l' and '-d' cannot be used
/// together` and exits 129, `clean -n -x -X` writes `fatal: options '-x' and
/// '-X' cannot be used together` and exits 128. Same sentence, one word apart,
/// and the word tells you the code.
fn layer_boundary(out: &mut Vec<Case>) {
    loose(out, Shape::Linear, &["status", "--nosuchopt"]);
    loose(out, Shape::Linear, &["grep", "--nosuchopt", "fn"]);
    strict(out, Shape::Linear, &["log", "--nosuchopt"]);
    strict(out, Shape::Linear, &["rev-parse", "--nosuchopt"]);
    strict(out, Shape::Linear, &["bisect", "start", "--nosuchopt"]);

    strict(out, Shape::Linear, &["tag", "-d", "-l"]);
    strict(out, Shape::Linear, &["clean", "-n", "-x", "-X"]);
    strict(out, Shape::Linear, &["log", "--oneline", "--graph", "--reverse"]);
    strict(out, Shape::Linear, &["switch", "--detach", "-c", "foo"]);
    loose(out, Shape::Linear, &["branch", "-d", "--contains", "HEAD"]);
}

// ---------------------------------------------------------------------------
// 1 — the ordinary no
// ---------------------------------------------------------------------------

/// A negative answer that is not an error, from the verbs whose contract is to
/// give one.
///
/// Observed on stock git 2.55.0, all **1**:
///
/// | invocation | shape | output |
/// |---|---|---|
/// | `diff --exit-code` | dirty | the diff |
/// | `diff --quiet` | dirty | *(empty — the code is the whole answer)* |
/// | `grep -q nosuchpatternzz` | linear | empty |
/// | `check-ignore README.md` | attributes | empty |
/// | `check-ignore --quiet nosuch` | linear | empty |
/// | `check-ignore --verbose --no-index README.md` | attributes | empty |
/// | `ls-files --error-unmatch nosuch.txt` | linear | `error: pathspec … did not match` on stderr |
/// | `show-ref nosuchref` | linear | empty |
/// | `rev-parse --verify --quiet nosuchref` | linear | empty, **and stderr empty** |
/// | `cherry-pick alien-clash` | unrelated | conflict report |
/// | `bisect start HEAD HEAD` | branched | `… was both 'good' and 'bad'` |
///
/// What a port gets wrong without these: `--quiet` is two promises, an empty
/// output and a status, and a port can keep either one without the other.
/// `diff --quiet` must print
/// *nothing* and answer 1; a port that leaves the diff on stdout matches the
/// code and fails the case. `rev-parse --verify --quiet` on a bad name must
/// print nothing on **either** stream and answer 1, where the identical
/// invocation without `--quiet` is a 128 with `fatal: Needed a single revision`
/// — one flag moving a case between two of this module's three classes, which
/// is why both spellings are in the module.
///
/// `cherry-pick` on the `unrelated` shape is the conflicting-pick 1, and it is
/// deliberately the add/add collision on `README.md` rather than the
/// modify/delete one: the state probe compares what the refusal left behind, so
/// the case also pins that a 1 here still wrote `CHERRY_PICK_HEAD` and a
/// conflicted index.
fn ordinary_no_1(out: &mut Vec<Case>) {
    strict(out, Shape::Dirty, &["diff", "--exit-code"]);
    strict(out, Shape::Dirty, &["diff", "--quiet"]);
    strict(out, Shape::Linear, &["grep", "-q", "nosuchpatternzz"]);
    strict(out, Shape::Attributes, &["check-ignore", "README.md"]);
    strict(out, Shape::Linear, &["check-ignore", "--quiet", "nosuch"]);
    strict(out, Shape::Attributes, &["check-ignore", "--verbose", "--no-index", "README.md"]);
    strict(out, Shape::Linear, &["ls-files", "--error-unmatch", "nosuch.txt"]);
    strict(out, Shape::Linear, &["show-ref", "nosuchref"]);
    strict(out, Shape::Linear, &["rev-parse", "--verify", "--quiet", "nosuchref"]);
    strict(out, Shape::Unrelated, &["cherry-pick", "alien-clash"]);
    strict(out, Shape::Branched, &["bisect", "start", "HEAD", "HEAD"]);
}

// ---------------------------------------------------------------------------
// "Not found" is not one code
// ---------------------------------------------------------------------------

/// The same premise — *the thing you named is not there* — put to eighteen
/// invocations across fifteen verbs, which answer with two different codes here
/// and a third in [`remote_returns_two`].
///
/// The spellings are chosen to be the ones the corpus does not already ask.
/// `corpus.rs` pins `branch -d no-such-branch`, `cat-file -t does-not-exist`,
/// `log does-not-exist` and `show deadbeef…` across the read shapes, and other
/// modules pin `tag -d nosuchtag`, `verify-tag nosuchtag`, `replace -d nosuch`
/// and `mv nosuch.txt other.txt` on their own; none of those is repeated. Every
/// case below reaches its code by a different route — a name that exists in the
/// *wrong* namespace, a branch with no upstream, a valid object with no note, a
/// commit's third parent.
///
/// Observed on stock git 2.55.0:
///
/// | invocation | code | message |
/// |---|---|---|
/// | `merge nosuchrev` | **1** | `merge: nosuchrev - not something we can merge` |
/// | `revert nosuchrev` | **128** | `fatal: bad revision 'nosuchrev'` |
/// | `checkout nosuchbranch` | **1** | `error: pathspec 'nosuchbranch' did not match any file(s) known to git` |
/// | `tag -d main` | **1** | `error: tag 'main' not found.` |
/// | `branch -m nosuchbranch other` | **128** | `fatal: no branch named 'nosuchbranch'` |
/// | `branch --unset-upstream` | **128** | `fatal: branch 'main' has no upstream information` |
/// | `replace -d main` | **1** | `error: replace ref 'edfab1b…' not found` |
/// | `notes copy HEAD HEAD` | **1** | `error: missing notes on source object …` |
/// | `bundle verify nosuchfile.bundle` | **1** | `error: could not open 'nosuchfile.bundle'` |
/// | `stash pop` | **1** | `No stash entries found.` |
/// | `stash drop` | **1** | `No stash entries found.` |
/// | `reflog exists refs/heads/nosuch` | **1** | empty — a predicate, like `--is-ancestor` |
/// | `show nosuchrev` | **128** | `fatal: ambiguous argument 'nosuchrev': unknown revision…` |
/// | `rev-parse HEAD^3` | **128** | `fatal: ambiguous argument 'HEAD^3': unknown revision…` |
/// | `rev-parse --verify HEAD:nosuchfile` | **128** | `fatal: Needed a single revision` |
/// | `cat-file blob HEAD` | **128** | `fatal: git cat-file HEAD: bad file` |
/// | `ls-remote nosuchremote` | **128** | `fatal: 'nosuchremote' does not appear to be a git repository` |
/// | `worktree remove nosuchwt` | **128** | `fatal: 'nosuchwt' is not a working tree` |
///
/// The first two lines are the whole argument for this group: *the same
/// argument, `nosuchrev`, given to two verbs on the same fixture* is an
/// `error:` at 1 from `merge` and a `fatal:` at 128 from `revert`. So is the
/// pair below them: `main` is a branch and not a tag, and asking the wrong
/// namespace for it is 1 from `tag -d` and 128 from `branch -m`. No verb-by-verb corpus can state that,
/// because neither module is wrong on its own — the pair is the finding, and a
/// port with one "not found" error type gets exactly one of each pair right.
///
/// The rule underneath, worth stating because it predicts the rest: a name that
/// failed to *resolve* dies (128); a name that resolved and simply is not
/// *present in the collection being asked about* is an ordinary no (1). That is
/// why `replace -d main` and `notes copy HEAD HEAD` are 1 — the object resolved
/// perfectly and merely has no replacement and no note — while
/// `rev-parse HEAD^3` is 128, because `HEAD` has no third parent to resolve to.
/// `cat-file blob HEAD` is the awkward member: `HEAD` resolves and is the wrong
/// *type*, and git puts a type mismatch on the fatal side.
fn not_found_is_not_one_code(out: &mut Vec<Case>) {
    // Resolution failures: 128.
    strict(out, Shape::Linear, &["revert", "nosuchrev"]);
    strict(out, Shape::Linear, &["show", "nosuchrev"]);
    strict(out, Shape::Linear, &["rev-parse", "HEAD^3"]);
    strict(out, Shape::Linear, &["rev-parse", "--verify", "HEAD:nosuchfile"]);
    strict(out, Shape::Linear, &["cat-file", "blob", "HEAD"]);
    strict(out, Shape::Linear, &["ls-remote", "nosuchremote"]);
    strict(out, Shape::Linear, &["branch", "-m", "nosuchbranch", "other"]);
    strict(out, Shape::Linear, &["branch", "--unset-upstream"]);
    strict(out, Shape::Linear, &["worktree", "remove", "nosuchwt"]);

    // Absent from the collection: 1.
    strict(out, Shape::Linear, &["checkout", "nosuchbranch"]);
    strict(out, Shape::Linear, &["merge", "nosuchrev"]);
    strict(out, Shape::Linear, &["tag", "-d", "main"]);
    strict(out, Shape::Linear, &["replace", "-d", "main"]);
    strict(out, Shape::Linear, &["notes", "copy", "HEAD", "HEAD"]);
    strict(out, Shape::Linear, &["bundle", "verify", "nosuchfile.bundle"]);
    strict(out, Shape::Linear, &["stash", "pop"]);
    strict(out, Shape::Linear, &["stash", "drop"]);
    strict(out, Shape::Linear, &["reflog", "exists", "refs/heads/nosuch"]);
}

/// `remote`'s own code, which is neither 1 nor 128.
///
/// Observed on stock git 2.55.0, all **2**:
///
/// | invocation | message |
/// |---|---|
/// | `remote rm nosuch` | `error: No such remote: 'nosuch'` |
/// | `remote rename nosuch other` | `error: No such remote: 'nosuch'` |
/// | `remote get-url nosuch` | `error: No such remote 'nosuch'` |
///
/// `git-remote(1)` documents no exit status at all, and 2 is not one of the
/// codes any other verb in this module produces for a name it cannot find —
/// [`not_found_is_not_one_code`] measures 1 and 128 for exactly the same
/// premise. It cannot be derived from a rule, so a port that reasons about what
/// the code *should* be gets it wrong and only an enumeration finds it. Note
/// also that the three messages are not one string — `get-url` omits the colon
/// after `remote` — which is why these compare stderr.
fn remote_returns_two(out: &mut Vec<Case>) {
    strict(out, Shape::Linear, &["remote", "rm", "nosuch"]);
    strict(out, Shape::Linear, &["remote", "rename", "nosuch", "other"]);
    strict(out, Shape::Linear, &["remote", "get-url", "nosuch"]);
}

// ---------------------------------------------------------------------------
// config's documented codes
// ---------------------------------------------------------------------------

/// `git config`'s exit codes, reached from angles `config_cmd.rs` does not use.
///
/// That module already pins the two headline refusals — `--unset` on a
/// multi-valued key (5) and `config invalidkey v` (2) — so nothing here repeats
/// them. What is here is the rest of the documented table plus the boundaries
/// between its entries. Observed on stock git 2.55.0:
///
/// | invocation | code | stderr |
/// |---|---|---|
/// | `config --file src --get a.b` | **1** | `warning: unable to access 'src': Is a directory` |
/// | `config --file src a.b c` | **3** | the same warning, then `error: invalid config file src` |
/// | `config --file nosuchfile.cfg --get a.b` | **1** | empty |
/// | `config --blob HEAD:README.md --get a.b` | **1** | empty |
/// | `config --get-regexp ^nosuch` | **1** | empty |
/// | `config --unset nosuch.key` | **5** | empty |
/// | `config --unset-all nosuch.key` | **5** | empty |
/// | `config --get a` | **1** | `error: key does not contain a section: a` |
/// | `config --get a.` | **1** | `error: key does not contain variable name: a.` |
/// | `config --rename-section nosuch other` | **128** | `fatal: no such section: nosuch` |
/// | `config --get-urlmatch http nosuchurl` | **128** | `fatal: invalid URL scheme name or missing '://' suffix` |
/// | `config --get-color nosuch.color` | **0** | nothing on either stream |
///
/// Four things a port gets wrong without this group, none of them guessable:
///
///  * **The same broken file is 1 to a reader and 3 to a writer.** `--file src`
///    names a directory. Reading through it is a miss (1) with a *warning*;
///    writing through it is `error: invalid config file` and the documented
///    **3**. So code 3 is not "the file is malformed" — it is "the file is
///    malformed and you asked me to change it".
///  * **5 is not only the multi-value case.** `git-config(1)` lists 5 as "you
///    try to unset an option which does not exist", and `--unset nosuch.key`
///    reaches it on a key that exists nowhere at all. A port that treats a
///    missing key as the ordinary miss answers 1 and is wrong in the direction
///    a script notices, because 5 is the value `--unset` reserves for "nothing
///    happened".
///  * **A malformed key is 1 when read and 2 when written.** `--get a` and
///    `--get a.` print a diagnostic and answer 1. The identical keys given a
///    value — `config a v`, `config a. v` — print the *same* diagnostic and
///    answer 2, the documented "invalid key". One key, one message, two codes,
///    decided only by which side of the command it is on. The write half is
///    `config_cmd.rs`'s (`config invalidkey v`) and is not repeated here; the
///    read half had no case anywhere.
///  * **A missing colour is 0.** `--get-color` substitutes its default and
///    succeeds, so the one `config` query that *cannot* fail to find something
///    is the one a port is most likely to make answer 1.
fn config_documented_codes(out: &mut Vec<Case>) {
    strict(out, Shape::Linear, &["config", "--file", "src", "--get", "a.b"]);
    strict(out, Shape::Linear, &["config", "--file", "src", "a.b", "c"]);
    strict(out, Shape::Linear, &["config", "--file", "nosuchfile.cfg", "--get", "a.b"]);
    strict(out, Shape::Linear, &["config", "--blob", "HEAD:README.md", "--get", "a.b"]);
    strict(out, Shape::Linear, &["config", "--get-regexp", "^nosuch"]);
    strict(out, Shape::Linear, &["config", "--unset", "nosuch.key"]);
    strict(out, Shape::Linear, &["config", "--unset-all", "nosuch.key"]);
    strict(out, Shape::Linear, &["config", "--get", "a"]);
    strict(out, Shape::Linear, &["config", "--get", "a."]);
    strict(out, Shape::Linear, &["config", "--rename-section", "nosuch", "other"]);
    strict(out, Shape::Linear, &["config", "--get-urlmatch", "http", "nosuchurl"]);
    strict(out, Shape::Linear, &["config", "--get-color", "nosuch.color"]);
}

// ---------------------------------------------------------------------------
// diff --check: a bitmask, not a code
// ---------------------------------------------------------------------------

/// `diff`'s three status bits, over one pair of commits.
///
/// `diff`'s status is a set of bits and not a choice between codes: measured,
/// `--exit-code` contributes 1 for "there were differences", `--check`
/// contributes 2 for "there were whitespace errors", and asking for both
/// produces 3. Observed on stock git 2.55.0, all against `whitespace`'s
/// `main~3..main~2` (the `whitespace: trailing blanks` commit):
///
/// | invocation | code | stdout |
/// |---|---|---|
/// | `diff --check main~3 main~2` | **2** | `ws/indent.c:3: trailing whitespace.` + the line |
/// | `diff --exit-code --check main~3 main~2` | **3** | the same report |
/// | `diff --check --quiet main~3 main~2` | **1** | *empty* |
///
/// What a port gets wrong without these: 3 appears in no table and cannot be
/// arrived at by implementing the documented codes one at a time — it exists
/// only because the two contributions are combined rather than selected between,
/// so an implementation that returns "the" code returns 1 or 2 and never 3. And
/// `--quiet` does not merely silence the report: with it the whitespace report
/// is gone from stdout *and* the 2 is gone from the status, leaving 1. A port
/// that treats `--quiet` as a print suppressor keeps the 2 and fails on the code
/// while matching the (empty) output.
fn diff_check_bits(out: &mut Vec<Case>) {
    strict(out, Shape::Whitespace, &["diff", "--check", "main~3", "main~2"]);
    strict(out, Shape::Whitespace, &["diff", "--exit-code", "--check", "main~3", "main~2"]);
    strict(out, Shape::Whitespace, &["diff", "--check", "--quiet", "main~3", "main~2"]);
}

// ---------------------------------------------------------------------------
// The verb-specific codes
// ---------------------------------------------------------------------------

/// Each verb's own status contract, on the error paths where the code is the
/// documented interface — including the two codes (3 and 4) no other verb in
/// this corpus produces.
///
/// Observed on stock git 2.55.0:
///
/// | invocation | shape | code | note |
/// |---|---|---|---|
/// | `fsck` | damaged | **3** | `error: refs/heads/broken-symref: invalid sha1 pointer 000…` |
/// | `fsck --strict` | damaged | **3** | same |
/// | `fsck --connectivity-only` | damaged | **128** | same errors, different exit |
/// | `fsck --connectivity-only --strict` | damaged | **128** | same |
/// | `bisect start main main~4 -- nosuchpath` | whitespace | **4** | `No testable commit found.` |
/// | `bisect start main main~4` | whitespace | **1** | `error: Your local changes … would be overwritten` |
/// | `apply --check --directory=zz patches/valid.patch` | patches | **1** | `error: zz/app/main.c: No such file or directory` |
/// | `am --show-current-patch` | patches | **128** | `fatal: Resolve operation not in progress…` |
/// | `merge --abort` | linear | **128** | `fatal: There is no merge to abort (MERGE_HEAD missing).` |
/// | `rebase --abort` | conflicted | **128** | `fatal: no rebase in progress` |
/// | `stash push` | conflicted | **1** | `error: could not write index` |
/// | `commit -m x` | conflicted | **128** | `error: Committing is not possible because you have unmerged files.` |
/// | `merge theirs` | conflicted | **128** | `error: Merging is not possible…` |
/// | `worktree lock .` | linear | **128** | `fatal: The main working tree cannot be locked or unlocked` |
/// | `check-ignore --stdin README.md` | linear | **128** | `fatal: cannot specify pathnames with --stdin` |
/// | `grep --cached needle dangling` | damaged | **128** | `fatal: unable to parse object: dangling` |
/// | `grep needle nosuchrev` | linear | **128** | `fatal: ambiguous argument 'nosuchrev'…` |
///
/// Four of these are worth their own sentence.
///
/// **`fsck` answers 3, not 1.** `git-fsck(1)` documents nothing about its exit
/// status and the obvious guess is 1. Measured, it is 3 on this fixture, and it
/// is 3 whether or not `--strict` is given — so the number is neither a count of
/// the four `error:` lines it printed nor a severity that `--strict` raises. A
/// port that derives some other number from the same findings lands on 1 or 2
/// and looks plausible until both spellings are compared against one fixture,
/// which is why both are here.
///
/// **`fsck --connectivity-only` answers 128 over the identical repository.**
/// One flag moves the same premise from the 3 above into the fatal class, and
/// the `--strict` variant of it is 128 as well. This is the pair the port is
/// reported to answer **2** for — a code stock git produces for neither
/// spelling — and it is only visible if both spellings are asked of one
/// fixture, because either one alone reads as a plausible whole answer.
///
/// **`bisect`'s 4 needs a pathspec to reach.** `bisect start main main~4 --
/// nosuchpath` prints `No testable commit found.` and exits 4. Its neighbour in
/// the table is the same command *without* the pathspec, which gets as far as
/// `Bisecting: 1 revision left to test` and then fails at checkout over the
/// shape's dirty worktree — exit 1. So 4 is not "bisect could not start"; it is
/// specifically "the range is empty once the pathspec has been applied", and the
/// two cases together are what say so.
///
/// **`grep` reaches 128 two ways and 1 one way.** A pattern that matches
/// nothing is [`ordinary_no_1`]'s 1; a *tree* that cannot be read is 128,
/// whether the name fails to resolve at all (`nosuchrev`) or resolves to a ref
/// whose object is missing (`dangling`, on the damaged shape). The three
/// together are the whole of `grep`'s status contract.
fn verb_specific_codes(out: &mut Vec<Case>) {
    strict(out, Shape::Damaged, &["fsck"]);
    strict(out, Shape::Damaged, &["fsck", "--strict"]);
    strict(out, Shape::Damaged, &["fsck", "--connectivity-only"]);
    strict(out, Shape::Damaged, &["fsck", "--connectivity-only", "--strict"]);

    strict(out, Shape::Whitespace, &["bisect", "start", "main", "main~4", "--", "nosuchpath"]);
    strict(out, Shape::Whitespace, &["bisect", "start", "main", "main~4"]);

    strict(out, Shape::Patches, &["apply", "--check", "--directory=zz", "patches/valid.patch"]);
    strict(out, Shape::Patches, &["am", "--show-current-patch"]);

    strict(out, Shape::Linear, &["merge", "--abort"]);
    strict(out, Shape::Conflicted, &["rebase", "--abort"]);
    strict(out, Shape::Conflicted, &["stash", "push"]);
    strict(out, Shape::Conflicted, &["commit", "-m", "x"]);
    strict(out, Shape::Conflicted, &["merge", "theirs"]);

    strict(out, Shape::Linear, &["worktree", "lock", "."]);
    strict(out, Shape::Linear, &["check-ignore", "--stdin", "README.md"]);
    strict(out, Shape::Damaged, &["grep", "--cached", "needle", "dangling"]);
    strict(out, Shape::Linear, &["grep", "needle", "nosuchrev"]);
}

/// The refusals `parse-options` never sees, in the verbs whose argument *is* a
/// value it has to interpret.
///
/// Observed on stock git 2.55.0, all **128**:
///
/// | invocation | message |
/// |---|---|
/// | `commit --author=nobrackets -m x` | `fatal: --author 'nobrackets' is not 'Name <email>' and matches no existing author` |
/// | `archive --list HEAD` | `fatal: extra command line parameter 'HEAD'` |
///
/// Two of the shape a port most reliably gets wrong, because both *look* like
/// argument errors: an option whose value is malformed, and an operand where
/// none is allowed. Neither is refused by the option table — both messages say
/// `fatal:` rather than `error:` and neither prints a usage block — so the
/// refusal comes from the builtin body and carries the builtin's code, 128, not
/// 129. `--author` is the case already known to be wrong in this port, and
/// wrong in the least visible way: it does not refuse at all, so the malformed
/// value is dropped and the command proceeds to whatever the *rest* of the argv
/// asked for.
fn value_refusals_after_parsing(out: &mut Vec<Case>) {
    strict(out, Shape::Linear, &["commit", "--author=nobrackets", "-m", "x"]);
    strict(out, Shape::Linear, &["archive", "--list", "HEAD"]);
}

// ---------------------------------------------------------------------------
// 0 — the successes that read like failures
// ---------------------------------------------------------------------------

/// Invocations that look like errors, ask for nothing, or do nothing, and exit
/// **0**.
///
/// Observed on stock git 2.55.0:
///
/// | invocation | shape | stdout |
/// |---|---|---|
/// | `format-patch HEAD..HEAD` | linear | empty — an empty range is a success |
/// | `rev-list --all --not` | linear | the full list; a `--not` with no operand is not an error |
/// | `tag -d` | linear | empty — a delete with nothing to delete |
/// | `diff --unified` | linear | empty — the option is accepted with no value |
/// | `check-ref-format --branch @` | linear | `@` |
/// | `check-ignore --no-index build/x` | attributes | `build/x` — a *match* is 0, a miss is 1 |
/// | `merge-base --is-ancestor HEAD HEAD` | linear | empty — the predicate's true |
/// | `reflog exists refs/heads/main` | linear | empty — the other predicate's true |
/// | `reset --soft --hard HEAD` | linear | `HEAD is now at edfab1b initial` |
/// | `checkout --ours --theirs README.md` | linear | `Updated 0 paths from the index` |
/// | `update-index --refresh --unmerged` | linear | empty |
/// | `bisect reset` | linear | empty — resetting a bisect that never started |
///
/// This group is the other half of the module and it catches the opposite
/// defect: a port that has learned "refuse loudly" answers non-zero here and
/// breaks scripts in the direction nobody tests for. Three are worth naming.
///
/// **`format-patch` over an empty range is 0 with no output.** The plausible
/// alternative is 1 for "nothing to format", and an implementation that reasons
/// its way to a code rather than measuring one picks it; the shell that notices
/// is `git format-patch @{u}.. >series && …`, which stops running.
///
/// **`reset --soft --hard` and `checkout --ours --theirs` are not conflicts.**
/// Both are pairs of options that select a single mode, and both are accepted:
/// `reset --soft --hard HEAD` performs the `--hard` and prints
/// `HEAD is now at edfab1b initial`, `checkout --ours --theirs README.md` prints
/// `Updated 0 paths from the index`. Neither is an error at all — unlike the
/// four genuinely-exclusive pairs in [`layer_boundary`], which are refused with
/// two different codes. A port that rejects mode options pairwise on a blanket
/// rule fails here while passing there, which is why both shapes of pair live in
/// this module.
///
/// **`check-ignore` inverts the usual polarity.** A path that *is* ignored is 0
/// and a path that is not is 1, the reverse of every "did you find it" verb
/// beside it, and a port that returns 0 for "the query ran" gets both directions
/// wrong at once.
fn zero_that_reads_like_failure(out: &mut Vec<Case>) {
    strict(out, Shape::Linear, &["format-patch", "HEAD..HEAD"]);
    strict(out, Shape::Linear, &["rev-list", "--all", "--not"]);
    strict(out, Shape::Linear, &["tag", "-d"]);
    strict(out, Shape::Linear, &["diff", "--unified"]);
    strict(out, Shape::Linear, &["check-ref-format", "--branch", "@"]);
    strict(out, Shape::Attributes, &["check-ignore", "--no-index", "build/x"]);
    strict(out, Shape::Linear, &["merge-base", "--is-ancestor", "HEAD", "HEAD"]);
    strict(out, Shape::Linear, &["reflog", "exists", "refs/heads/main"]);
    strict(out, Shape::Linear, &["reset", "--soft", "--hard", "HEAD"]);
    strict(out, Shape::Linear, &["checkout", "--ours", "--theirs", "README.md"]);
    strict(out, Shape::Linear, &["update-index", "--refresh", "--unmerged"]);
    strict(out, Shape::Linear, &["bisect", "reset"]);
}
