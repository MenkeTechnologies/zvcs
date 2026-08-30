//! Sparse checkout: the pattern language, the two modes it is written in, and
//! the working tree that is missing half its files.
//!
//! A sparse checkout is three separate pieces of state that git keeps in
//! agreement — a pattern list in `$GIT_COMMON_DIR/info/sparse-checkout`, two
//! booleans (`core.sparseCheckout`, `core.sparseCheckoutCone`) that live in the
//! *worktree* config scope, and one `SKIP_WORKTREE` bit per index entry — plus
//! an optional `sdir` index extension that collapses whole excluded directories
//! into a single entry. Every one of those can be written by one subcommand and
//! read by a different one, which is what makes the surface worth a module of
//! its own: a port can produce the right worktree from the wrong pattern file,
//! or the right pattern file with the wrong booleans, and both mistakes are
//! invisible until the *next* command reads what the last one wrote.
//!
//! # How this divides territory with the four adjacent modules
//!
//! * **`worktree_index.rs`** owns `sparse-checkout` on repositories that are
//!   **not sparse** — its `sparse_checkout` group is twenty-one `Case::new`
//!   cases on `Linear`, `Dirty`, `Detached`, `Branched` and `AwkwardPaths`
//!   covering `list`, the five `init` spellings, `set src`, `set src nested`,
//!   `set --no-cone /src/`, `set --no-cone /* !/src/`, `set nested`, `disable`,
//!   and the four error paths (`add`/`reapply`/`clean` with no sparse-checkout,
//!   plus `bogus` and the bare verb). Its own header records why it stops
//!   there: "no fixture shape ships sparse and nothing here can make one sparse
//!   first". That is no longer true — [`Shape::Sparse`] exists — but the
//!   division still holds, and this file takes the other half: the subcommands
//!   run **against** an established cone, every option those cases never spell
//!   (`--skip-checks`, `--stdin`, `-z`, `--rules-file`, `--sparse-index`,
//!   `check-rules`), and every case whose answer is on **stderr**, since every
//!   case there is `Case::new` and none of them compares it.
//! * **`shape_reach.rs`** owns the breadth sweep over [`Shape::Sparse`]: forty
//!   `Case::new` cases asking eight `sparse-checkout` subcommands and seven
//!   other verbs (`ls-files`, `status`, `rm`, `add`, `checkout`, `mv`, `clean`,
//!   `read-tree`, `update-index`, `diff`, `grep`, `stash`) the plain form of
//!   their question. It establishes that the fixture is reachable at all. It
//!   spells no option that changes the *mode* (no `--no-cone`, no
//!   `--sparse-index`, no `--rules-file`), it feeds nothing on stdin — its
//!   `check-rules outside/drop.txt` case runs with stdin closed, so it measures
//!   argument parsing and not the matcher — and, being non-strict throughout,
//!   it cannot see a warning. Every argv here is distinct from every argv
//!   there; where the plain form was taken this file asks a modified one.
//! * **`add_rm_mv_clean.rs`** owns the `--sparse` pathspec flag on
//!   [`Shape::Sparse`]: five `add` cases (three strict), five `rm`, three `mv`
//!   and three `clean`, which together are the `advice.updateSparsePath`
//!   corner. This file does not repeat them; it asks `--no-sparse` — the
//!   spelling that module never uses — and takes the verbs it does not cover at
//!   all (`restore`, `switch`, `commit`, `worktree add`).
//! * **`index_plumbing.rs`** owns the plumbing verbs on [`Shape::Sparse`]:
//!   `ls-files` selectors (`-m -d`, `-k`, `-f`, `--format=%(objectname)`,
//!   `-o --directory`, a strict `--error-unmatch`), `update-index`
//!   (`--no-skip-worktree outside/nested/deep.txt`, `--skip-worktree root.txt`,
//!   `--assume-unchanged`, the three refresh spellings), `read-tree`
//!   (`--reset -u`, `-m -u --no-sparse-checkout`, `--prefix=`),
//!   `checkout-index` (`-a`, `-a -f`, `-f outside/drop.txt`, `-a --prefix=out/`,
//!   `-u -a -f`), `status --porcelain=v2 --branch -uall`, and `diff-index`/
//!   `diff-files`. Every argv it spells is left alone here; what this file adds
//!   on those verbs is the `--sparse` listing flag and the same verbs run with
//!   `index.sparse=true`, neither of which appears there.
//! * **`sequences.rs`** owns everything multi-step: `set`/`reapply`/`add`/
//!   `disable` in order, and the cone→non-cone→cone mode switch, both on
//!   [`Shape::Sparse`], plus a `config core.sparseCheckout` first / `set`
//!   second sequence on `Branched`. That module is the **only** place the
//!   pattern *file* can be read back (see "What the state digest cannot see"
//!   below), so any question of the form "what exactly did `set` write" belongs
//!   there and not here.
//!
//! # The fixture, and what a single case can reach from it
//!
//! [`Shape::Sparse`] (`fixture.rs:1383`) is the only shape that ships sparse.
//! Verified against stock 2.55.0 in a hand-built copy:
//!
//! ```text
//! $ git ls-files --stage -v
//! H 100644 …9741694 0  README.md
//! H 100644 …47af6bb 0  inside/keep.txt
//! H 100644 …da7fbdc 0  inside/nested/also.txt
//! S 100644 …9e16e1a 0  outside/drop.txt
//! S 100644 …1cac16b 0  outside/nested/deep.txt
//! H 100644 …b926fda 0  root.txt
//! S 100644 …46e89a2 0  src/lib.rs
//! $ cat .git/info/sparse-checkout
//! /*
//! !/*/
//! /inside/
//! ```
//!
//! Three facts about it are load-bearing for the groups below:
//!
//! 1. **`src/` is excluded too.** The base fixture's `src/lib.rs` predates the
//!    cone, so `set inside` dropped it. That gives two independent excluded
//!    directories, `outside/` and `src/`, which is what lets a pattern be wrong
//!    in a way that moves one and not the other.
//! 2. **`outside/` still exists on disk**, holding only the untracked
//!    `outside/stray.txt`. That is what makes stock emit `warning: directory
//!    'outside/' contains untracked files, but is not in the sparse-checkout
//!    cone` on essentially every re-application of the patterns — see the
//!    warning group below.
//! 3. **`inside/nested/` and `outside/nested/` are two directories with the
//!    same basename at different depths.** In cone mode a pattern is always a
//!    full path from the root; in non-cone (gitignore) mode a pattern with no
//!    slash matches a basename at *any* depth. So `nested` means one thing in
//!    one mode and another in the other, and the difference is visible in the
//!    worktree rather than only in the pattern file. That is the whole reason
//!    the `core.sparseCheckoutCone=false` cases below use `nested` and
//!    `drop.txt` rather than `outside` — see "What the state digest cannot see".
//!
//! The other shapes used here are not sparse and are not made sparse by any
//! prior step; a case that wants a sparse repository plus something else must
//! get that something else from the shape, because `sparse-checkout set` is
//! itself the one argv the case has. [`Shape::Dirty`] supplies a modified
//! `README.md` and a deleted `src/lib.rs`, which is how the "not up to date and
//! were left despite sparse patterns" warning is reached. [`Shape::AwkwardPaths`]
//! supplies `with space.txt` and `nested/deep/`, which is how a pattern and a
//! `--rules-file` operand carrying a space are reached. [`Shape::Linear`] and
//! [`Shape::Branched`] supply repositories with no sparse state at all, which is
//! where the `core.sparseCheckout=true`-but-no-pattern-file corner lives.
//!
//! # What the state digest cannot see, and how each group is built around it
//!
//! Two files that sparse mode's whole behaviour is written into are **not read
//! by any probe**, and this is the single most important constraint on what a
//! case in this file is allowed to claim:
//!
//! * **`.git/info/sparse-checkout`.** `probe_op_state` reads a fixed list of
//!   `.git` root files (`runner.rs:2586`) and walks four operation directories
//!   (`runner.rs:2629`); `info/` is in neither. `probe_storage` reads
//!   `objects/info`, which is a different directory. So the pattern file's
//!   text, its order, and its quoting are invisible to the comparison. The one
//!   place they surface is `sparse-checkout list` on **stdout**, and a case is
//!   one invocation, so a single case can *write* patterns or *read* them and
//!   never both.
//! * **`.git/config.worktree`.** `Shape::Sparse` sets `extensions.worktreeConfig`,
//!   so `core.sparseCheckout`, `core.sparseCheckoutCone` and `index.sparse` are
//!   written there and **not** to `.git/config`. `probe_state`'s config probe is
//!   `config --list --local` (`runner.rs:2054`), which does not read the
//!   worktree scope, and nothing else opens the file. So "did this command set
//!   the mode booleans" is unmeasured except through a later command's answer.
//!
//! The consequence is a rule every group below obeys: **a case must make its
//! answer land in the worktree, in the index, or on stdout.** Concretely —
//!
//! * The pattern-writing cases are chosen so that a wrong pattern moves a
//!   `SKIP_WORKTREE` bit or a file. `-c core.sparseCheckoutCone=false
//!   sparse-checkout add nested` is here and `… add outside` is not, because
//!   the first is measurable and the second is not: both write a different
//!   pattern than the port does, but only the first produces a different
//!   working tree. Verified with stock 2.55.0 — `add outside` writes `outside`
//!   where the port writes `/outside/`, and both spellings check out the same
//!   files, so the harness would score it a match while the file on disk
//!   differs.
//! * The mode-boolean cases are read questions, not write questions: `-c
//!   core.sparseCheckout=true sparse-checkout list` asks what a command *does*
//!   with the setting rather than whether it stored it.
//! * The `--sparse-index` cases lean on the one extension probe that does
//!   exist: `probe_index_meta` (`runner.rs:4040`) parses the index's extension
//!   chain and reports `sdir` by signature and length, so "a sparse index was
//!   written" is compared even though the config key that asked for it is not.
//!
//! # `check-rules`, and why it is the centre of this file
//!
//! `git sparse-checkout check-rules` reads **paths on stdin** and prints the
//! ones the pattern set keeps. It is the only way to interrogate the matcher
//! directly — every other route runs it through a checkout and reports the
//! result as a working tree. `worktree_index.rs` names it as unreachable
//! ("`sparse-checkout check-rules` … cannot be fed input") because stdin was
//! nailed shut when that module was written; `Case::with_stdin` has since
//! opened it, and `shape_reach.rs`'s single `check-rules` case still runs with
//! stdin closed. So the matcher has never been measured. The group below feeds
//! it four payloads, and pairs them with `--rules-file` operands that name
//! files the fixture already contains — `.git/info/sparse-checkout` (its own
//! rules, re-read as a file), `README.md` (whose only line is `# fixture`, a
//! comment, so the rule set is empty), `inside/keep.txt` and `root.txt` (whose
//! contents are English sentences, so the rule set is a handful of
//! space-bearing patterns), and `with space.txt` on `AwkwardPaths`.
//!
//! # The refusal and warning budget
//!
//! Nineteen cases here are `Case::strict`. They fall into four kinds, and the
//! reason each kind needs stderr compared is different:
//!
//! * **Refusals that name the offending operand** — `fatal: 'root.txt' is not a
//!   directory; to treat it as a directory anyway, rerun with --skip-checks`
//!   and `fatal: could not open '<file>' for reading`. The named operand is the
//!   only place the command says what it thought it was given.
//! * **Cone-mode grammar refusals** — `/*`, `inside/*`, `!inside` and
//!   `../escape` as cone operands. Each is rejected for a different reason and
//!   the four messages are the only thing that distinguishes them; the exit
//!   code and the untouched repository are identical across all four, so a port
//!   that refuses everything with one message scores four matches without
//!   `strict`.
//! * **The untracked-cone warning**, `warning: directory 'outside/' contains
//!   untracked files, but is not in the sparse-checkout cone`. This is the one
//!   sparse message with no other symptom at all: the checkout it accompanies
//!   is byte-identical either way, so a port that never prints it scores a
//!   match on every non-strict case. It is deliberately measured by **three**
//!   representatives — one `set`, one `reapply`, one `init` — and not by every
//!   invocation that emits it, so that one defect reads as one finding rather
//!   than as twenty. The remaining cases in those groups are `Case::new` and
//!   measure the checkout instead.
//! * **The stale-worktree warning** on `Shape::Dirty`, `warning: The following
//!   paths are not up to date and were left despite sparse patterns:`, which is
//!   likewise accompanied by an identical working tree — stock leaves the dirty
//!   path in place and so does a port that says nothing about it.
//!
//! # Not measurable, recorded rather than shipped
//!
//! * **`sparse-checkout list` after a `set` in the same case.** One argv per
//!   case; this is `sequences.rs`'s dimension and is left there.
//! * **The exact bytes of `.git/info/sparse-checkout` and
//!   `.git/config.worktree`.** No probe reads either — see above. Every case
//!   here is chosen so its verdict does not rest on them.
//! * **The cache tree without a sparse index.** The `index.sparse` group's
//!   first two cases find the port invalidating `TREE` entries stock keeps, and
//!   the same argv without the setting produces identical cache trees on both
//!   sides — so the defect is the sparse-index round trip, not the verb. The
//!   plain forms are therefore not duplicated here.
//! * **`advice.sparseIndexExpanded`'s hint text.** The hint is emitted when a
//!   sparse index is expanded to a full one, which needs a repository that
//!   already *has* a sparse index; no fixture shape does, and the one argv a
//!   case gets is spent writing the index rather than expanding it. The
//!   `-c advice.sparseIndexExpanded=false` case below therefore measures the
//!   key being accepted and the sparse index being written, not the hint being
//!   suppressed.
//! * **A branch switch, merge or rebase across the cone boundary.** Only
//!   `Shape::Sparse` is sparse and it has a single branch (`main`) and no tags,
//!   so there is nothing to check out, merge or rebase *into* a sparse tree
//!   from one argv. Making another shape sparse costs the case's only
//!   invocation. This is a fixture gap, not a coverage choice: it needs a shape
//!   that is both sparse and branched, and shapes are not this file's to add.
//! * **`sparse-checkout` in a linked worktree.** `worktree add wt2` is measured
//!   here for what it copies into `.git/worktrees/wt2/config.worktree`, but a
//!   `sparse-checkout` invocation run *inside* that new worktree needs the
//!   worktree to exist first, which is a second step.
//! * **`git clean` of an excluded directory with `-x`.** `shape_reach.rs`
//!   already owns the three `clean -n` spellings; the destructive `-f -d` form
//!   is here, and `-f -d -x` is not, because the fixture has no ignored file
//!   inside the excluded cone for `-x` to distinguish.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this module's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    check_rules_matcher(out);
    cone_directory_checks(out);
    non_cone_patterns(out);
    mode_and_index_flags(out);
    mode_booleans_read(out);
    patterns_from_stdin(out);
    untracked_cone_warning(out);
    subcommand_arguments(out);
    from_a_subdirectory(out);
    sparse_worktree_verbs(out);
}

/// `sparse-checkout`, stderr compared. Every message this module cares about is
/// a refusal or a warning, and both are on stderr.
fn sc_strict(args: &[&str], shape: Shape) -> Case {
    Case::strict("sparse-checkout", args, shape)
}

/// `sparse-checkout`, stdout/exit/state only.
fn sc(args: &[&str], shape: Shape) -> Case {
    Case::new("sparse-checkout", args, shape)
}

/// `sparse-checkout` with a payload on stdin, stderr compared as well.
fn sc_stdin_strict(args: &[&str], shape: Shape, stdin: &'static [u8]) -> Case {
    Case { compare_stderr: true, ..Case::with_stdin("sparse-checkout", args, shape, stdin) }
}

/// `sparse-checkout` with a payload on stdin.
fn sc_stdin(args: &[&str], shape: Shape, stdin: &'static [u8]) -> Case {
    Case::with_stdin("sparse-checkout", args, shape, stdin)
}

// ---------------------------------------------------------------------------
// check-rules: the matcher, asked directly
// ---------------------------------------------------------------------------

/// One of each entry in [`Shape::Sparse`]'s index, in index order. Three are
/// inside the cone and four are not, so the answer is a filter and not a
/// pass-through.
const TRACKED_PATHS: &[u8] = b"inside/keep.txt\noutside/drop.txt\nroot.txt\nsrc/lib.rs\ninside/nested/also.txt\noutside/nested/deep.txt\n";

/// The spellings of a path that a matcher has to normalize, or refuse to.
///
/// `inside` and `inside/` are the same directory written two ways; `nope.txt`
/// is a root-level path that does not exist, which the cone keeps anyway
/// because `/*` matches every root entry; `/root.txt` and `./root.txt` are the
/// two prefixed forms. Stock 2.55.0 answers `inside`, `inside/`, `root.txt`,
/// `nope.txt` and nothing else — it keeps neither prefixed form.
const PATH_SPELLINGS: &[u8] = b"inside\ninside/\noutside/drop.txt\nroot.txt\nnope.txt\n/root.txt\n./root.txt\n";

/// The same question with NUL separators, for `-z`.
const TRACKED_PATHS_NUL: &[u8] = b"inside/keep.txt\0root.txt\0outside/drop.txt\0";

/// Paths that are *words*, matched against a `--rules-file` whose contents are
/// English sentences. Nothing here is a path in any fixture; the point is
/// whether a pattern with spaces in it matches at all.
const WORD_PATHS: &[u8] = b"kept by the cone\nspace\n# fixture\nroot files stay in a cone checkout\n";

/// Paths from [`Shape::AwkwardPaths`], one of which carries a space.
const AWKWARD_PATHS: &[u8] = b"space\nwith space.txt\nnested/deep/path.txt\nsrc/lib.rs\n";

/// `check-rules`: the pattern matcher with its input on stdin.
///
/// The whole subcommand was unmeasured — see the module header — and it is the
/// only place git will answer "does this pattern set keep this path" without
/// first performing a checkout. Two of these are `strict`, and both are
/// because the answer is a refusal:
///
/// * **`--rules-file` naming a file that is not there.** Stock names the file
///   it could not open; the port reports a different failure entirely.
///   Verified against stock 2.55.0 and git 2.50.1, which agree:
///
///   ```text
///   stock: fatal: could not open 'no-such-file' for reading: No such file or directory
///   port:  fatal: unable to load existing sparse-checkout patterns
///   ```
///
/// * **`--skip-checks`.** The usage string `check-rules` prints advertises
///   `[--skip-checks]`, and the option is not actually registered for this
///   subcommand, so stock rejects it with `error: unknown option 'skip-checks'`
///   and exits **129** — a quirk of git's own, corroborated by 2.50.1. A port
///   that reads the usage string and implements what it says exits 0.
fn check_rules_matcher(out: &mut Vec<Case>) {
    out.push(sc_stdin(&["sparse-checkout", "check-rules"], Shape::Sparse, TRACKED_PATHS));
    out.push(sc_stdin(&["sparse-checkout", "check-rules"], Shape::Sparse, PATH_SPELLINGS));
    out.push(sc_stdin(&["sparse-checkout", "check-rules", "-z"], Shape::Sparse, TRACKED_PATHS_NUL));

    // `--rules-file` against the repository's own pattern file. The same rules
    // the repository is already using, but reached through the file reader
    // rather than through the in-memory set, and then through both cone
    // interpretations of them.
    for args in [
        &["sparse-checkout", "check-rules", "--rules-file", ".git/info/sparse-checkout"][..],
        &["sparse-checkout", "check-rules", "--cone", "--rules-file", ".git/info/sparse-checkout"],
        &["sparse-checkout", "check-rules", "--no-cone", "--rules-file", ".git/info/sparse-checkout"],
    ] {
        out.push(sc_stdin(args, Shape::Sparse, TRACKED_PATHS));
    }

    // A rules file whose only line is a comment: `README.md` is `# fixture\n`,
    // so the rule set is empty and nothing may match.
    out.push(sc_stdin(&["sparse-checkout", "check-rules", "--rules-file", "README.md"], Shape::Sparse, WORD_PATHS));
    // Rules files whose lines are sentences, so every pattern carries spaces.
    out.push(sc_stdin(&["sparse-checkout", "check-rules", "--rules-file", "inside/keep.txt"], Shape::Sparse, WORD_PATHS));
    out.push(sc_stdin(&["sparse-checkout", "check-rules", "--rules-file", "root.txt"], Shape::Sparse, TRACKED_PATHS));
    // `--no-rules-file` cancels a `--rules-file` that was never given, which
    // must leave the repository's own rules in force.
    out.push(sc_stdin(&["sparse-checkout", "check-rules", "--no-rules-file"], Shape::Sparse, TRACKED_PATHS));

    out.push(sc_stdin_strict(&["sparse-checkout", "check-rules", "--rules-file", "no-such-file"], Shape::Sparse, TRACKED_PATHS));
    out.push(sc_strict(&["sparse-checkout", "check-rules", "--skip-checks"], Shape::Sparse));

    // A repository that is not sparse at all: the matcher still has to answer,
    // and `--rules-file` still has to be honoured there.
    out.push(sc_stdin(&["sparse-checkout", "check-rules"], Shape::Linear, TRACKED_PATHS));
    out.push(sc_stdin(&["sparse-checkout", "check-rules", "--rules-file", "README.md"], Shape::Linear, TRACKED_PATHS));

    // A rules file whose *name* has a space in it, so the operand survives argv
    // and the open, and whose contents (`space\n`) are a pattern that matches a
    // path with no space and not the one with.
    out.push(sc_stdin(&["sparse-checkout", "check-rules", "--rules-file", "with space.txt"], Shape::AwkwardPaths, AWKWARD_PATHS));
    out.push(sc_stdin(&["sparse-checkout", "check-rules"], Shape::AwkwardPaths, AWKWARD_PATHS));
}

// ---------------------------------------------------------------------------
// Cone mode: what counts as a directory
// ---------------------------------------------------------------------------

/// Cone mode's operand check, and `--skip-checks` standing behind it.
///
/// In cone mode every operand must be a directory in the current `HEAD`, and
/// git verifies it before writing anything. The check is the whole difference
/// between cone mode and a pattern list, and skipping it is a documented escape
/// hatch, so both halves are here.
///
/// `set root.txt` is the sharp one and it is strict. `root.txt` is a tracked
/// **file**, so stock refuses, exits 128 and writes nothing. Verified against
/// stock 2.55.0 and corroborated by git 2.50.1:
///
/// ```text
/// stock: fatal: 'root.txt' is not a directory; to treat it as a directory
///        anyway, rerun with --skip-checks          (exit 128, tree unchanged)
/// port:  (silent, exit 0) — writes `/root.txt/` to the pattern file and
///        removes inside/ from the working tree
/// ```
///
/// The port's answer is not only a missing message: `inside/keep.txt` and
/// `inside/nested/also.txt` gain the `SKIP_WORKTREE` bit and disappear from the
/// worktree, so this case fails on state and exit code as well as stderr.
fn cone_directory_checks(out: &mut Vec<Case>) {
    out.push(sc_strict(&["sparse-checkout", "set", "root.txt"], Shape::Sparse));
    out.push(sc_strict(&["sparse-checkout", "add", "root.txt"], Shape::Sparse));

    // The escape hatch, and the same operands through it.
    out.push(sc(&["sparse-checkout", "set", "--skip-checks", "root.txt"], Shape::Sparse));
    out.push(sc(&["sparse-checkout", "set", "--skip-checks", "nonexistent"], Shape::Sparse));
    out.push(sc(&["sparse-checkout", "add", "--skip-checks", "nope"], Shape::Sparse));
    out.push(sc(&["sparse-checkout", "set", "--cone", "--skip-checks", "inside", "outside"], Shape::Sparse));

    // Directory operands in every spelling the check has to accept: nested,
    // trailing slash, leading slash, and the same one twice.
    out.push(sc(&["sparse-checkout", "set", "inside/nested"], Shape::Sparse));
    out.push(sc(&["sparse-checkout", "set", "inside/"], Shape::Sparse));
    out.push(sc(&["sparse-checkout", "set", "/inside"], Shape::Sparse));
    out.push(sc(&["sparse-checkout", "set", "inside", "inside"], Shape::Sparse));
    out.push(sc(&["sparse-checkout", "add", "outside/nested"], Shape::Sparse));

    // Operands that are patterns rather than directories. Cone mode has no
    // glob and no negation, so each of these is a name that happens to contain
    // a metacharacter and the refusal must name it as such rather than expand
    // it. Strict: the refusal text is the only place the operand appears.
    out.push(sc_strict(&["sparse-checkout", "set", "--cone", "/*"], Shape::Sparse));
    out.push(sc_strict(&["sparse-checkout", "set", "--cone", "inside/*"], Shape::Sparse));
    out.push(sc_strict(&["sparse-checkout", "set", "--cone", "!inside"], Shape::Sparse));
    // An operand that climbs out of the repository.
    out.push(sc_strict(&["sparse-checkout", "set", "../escape"], Shape::Sparse));

    // A directory two deep, a two-operand cone, and an operand with a space —
    // `AwkwardPaths` is the only shape that has one, and `--skip-checks` is
    // needed because `with space.txt` is a file.
    out.push(sc(&["sparse-checkout", "set", "nested/deep"], Shape::AwkwardPaths));
    out.push(sc(&["sparse-checkout", "set", "--cone", "src", "nested"], Shape::AwkwardPaths));
    out.push(sc(&["sparse-checkout", "set", "--skip-checks", "with space.txt"], Shape::AwkwardPaths));
}

// ---------------------------------------------------------------------------
// Non-cone mode: the gitignore pattern language
// ---------------------------------------------------------------------------

/// `--no-cone`: the same file, a different language.
///
/// Non-cone patterns are `.gitignore` syntax — anchoring by leading slash,
/// directory-only by trailing slash, negation by `!`, globs, character classes
/// — and the mode is written into the pattern file *and* into
/// `core.sparseCheckoutCone`. Every case below starts from the fixture's cone
/// configuration, so each is also a mode switch.
///
/// The `Shape::Dirty` case is strict and measures a warning with no other
/// symptom: `README.md` is modified there, so applying a pattern set that
/// excludes it leaves the file in place and stock says so —
///
/// ```text
/// warning: The following paths are not up to date and were left despite sparse patterns:
///     README.md
///
/// After fixing the above paths, you may want to run `git sparse-checkout reapply`.
/// ```
///
/// — while the port leaves the identical working tree and prints nothing.
fn non_cone_patterns(out: &mut Vec<Case>) {
    for args in [
        // A single anchored file, and a single anchored directory.
        &["sparse-checkout", "set", "--no-cone", "/root.txt"][..],
        &["sparse-checkout", "set", "--no-cone", "/outside/"],
        // Everything, then everything minus one subtree — the two-line idiom
        // the documentation gives for "the whole tree except".
        &["sparse-checkout", "set", "--no-cone", "/*"],
        &["sparse-checkout", "set", "--no-cone", "/*", "!/outside/"],
        // A leading negation with nothing positive in front of it: the result
        // is an empty checkout, not the complement.
        &["sparse-checkout", "set", "--no-cone", "!/outside/"],
        // Unanchored, so it matches a basename at any depth — the difference
        // between this and `/outside/` is the whole of the anchoring rule.
        &["sparse-checkout", "set", "--no-cone", "outside"],
        // Anchored but without the trailing slash, so it may match a file.
        &["sparse-checkout", "set", "--no-cone", "/outside"],
        // A glob in the middle of a path, a trailing `**`, and a character
        // class — three constructs cone mode cannot express at all.
        &["sparse-checkout", "set", "--no-cone", "/out*/"],
        &["sparse-checkout", "set", "--no-cone", "/inside/**"],
        &["sparse-checkout", "set", "--no-cone", "[a-z]nside/"],
        // `add` in non-cone mode appends a line rather than a cone entry.
        &["sparse-checkout", "add", "--no-cone", "/outside/"],
    ] {
        out.push(sc(args, Shape::Sparse));
    }

    // The two flags together, in both orders. Last one wins, and which one is
    // last decides which language the operand is read in.
    out.push(sc(&["sparse-checkout", "set", "--cone", "--no-cone", "inside"], Shape::Sparse));
    out.push(sc(&["sparse-checkout", "set", "--no-cone", "--cone", "inside"], Shape::Sparse));

    // Non-cone mode on repositories that were never sparse.
    out.push(sc(&["sparse-checkout", "set", "--no-cone", "/src/"], Shape::Branched));
    out.push(sc(&["sparse-checkout", "set", "--no-cone", "/with space.txt"], Shape::AwkwardPaths));
    out.push(sc(&["sparse-checkout", "set", "--no-cone", "/nested/deep/"], Shape::AwkwardPaths));
    out.push(sc(&["sparse-checkout", "reapply", "--no-cone"], Shape::Linear));

    // The stale-worktree warning; see the doc comment above.
    out.push(sc_strict(&["sparse-checkout", "set", "--no-cone", "/src/"], Shape::Dirty));
}

// ---------------------------------------------------------------------------
// --sparse-index, --cone/--no-cone on init and reapply
// ---------------------------------------------------------------------------

/// `--sparse-index`, and the flags that re-decide the mode without new patterns.
///
/// The sparse index is the one piece of sparse state the harness reads
/// directly: `probe_index_meta` (`runner.rs:4040`) parses the extension chain,
/// so an index carrying `sdir` is distinguishable from one that does not. That
/// makes `--sparse-index` measurable even though the `index.sparse` key it sets
/// lives in the unread worktree config. Verified with stock 2.55.0: after
/// `sparse-checkout init --sparse-index` the index carries `sdir` and
/// `.git/config.worktree` gains `index.sparse = true`.
fn mode_and_index_flags(out: &mut Vec<Case>) {
    for args in [
        &["sparse-checkout", "init", "--sparse-index"][..],
        &["sparse-checkout", "init", "--no-sparse-index"],
        &["sparse-checkout", "init", "--no-cone"],
        &["sparse-checkout", "set", "--sparse-index", "inside"],
        &["sparse-checkout", "set", "--no-sparse-index", "inside"],
        &["sparse-checkout", "reapply", "--no-cone"],
    ] {
        out.push(sc(args, Shape::Sparse));
    }

    // The same flags on repositories that are not sparse yet, where `init` has
    // to create the mode rather than change it.
    out.push(sc(&["sparse-checkout", "init", "--cone", "--sparse-index"], Shape::Branched));
    out.push(sc(&["sparse-checkout", "set", "--sparse-index", "src"], Shape::Linear));

    // `index.sparse` as a setting rather than as a flag: the same request
    // delivered through configuration, which is the path `git clone
    // --sparse` and `maintenance` take.
    out.push(
        Case::new("sparse-checkout", &["sparse-checkout", "reapply"], Shape::Sparse)
            .with_config(&[("index.sparse", "true")]),
    );
    out.push(
        Case::new("sparse-checkout", &["sparse-checkout", "set", "src"], Shape::Linear)
            .with_config(&[("index.sparse", "true")]),
    );
}

// ---------------------------------------------------------------------------
// The mode booleans, read rather than written
// ---------------------------------------------------------------------------

/// `core.sparseCheckout`, `core.sparseCheckoutCone` and `index.sparse` supplied
/// on the command line, so that what is measured is how a command *reads* them.
///
/// This is the only angle available: the keys are written to
/// `.git/config.worktree`, which no probe opens (see the module header), so
/// "did the command store the setting" is unmeasurable while "what did the
/// command do with the setting" is not. Three findings live here, all
/// corroborated by git 2.50.1:
///
/// * **`core.sparseCheckoutCone=false` changes what `list` prints.** Stock
///   honours the override and prints the pattern file verbatim; the port
///   ignores it and prints the cone rendering:
///
///   ```text
///   stock:  /*        port:  inside
///           !/*/
///           /inside/
///   ```
///
/// * **`core.sparseCheckoutCone=false` changes what `set`/`add` write**, and
///   the two operands chosen here make that visible in the working tree rather
///   than only in the unread pattern file. `nested` is a basename that exists
///   at two depths, so as a gitignore pattern it matches `inside/nested/` *and*
///   `outside/nested/`, while the cone translation `/nested/` matches neither:
///
///   ```text
///   stock: writes `nested`   → outside/nested/deep.txt materializes (H)
///   port:  writes `/nested/` → outside/nested/deep.txt stays skipped (S)
///   ```
///
///   `drop.txt` is the same shape of question one level down.
///
/// * **`core.sparseCheckout=true` on a repository with no pattern file.** Stock
///   warns and exits 0; the port dies. Strict, because the exit code and the
///   message are the whole answer:
///
///   ```text
///   stock: warning: this worktree is not sparse (sparse-checkout file may not exist)   exit 0
///   port:  fatal: this worktree is not sparse                                          exit 128
///   ```
fn mode_booleans_read(out: &mut Vec<Case>) {
    const CONE_OFF: &[(&str, &str)] = &[("core.sparseCheckoutCone", "false")];

    out.push(
        Case::strict("sparse-checkout", &["sparse-checkout", "list"], Shape::Sparse)
            .with_config(CONE_OFF),
    );
    for args in [
        &["sparse-checkout", "add", "nested"][..],
        &["sparse-checkout", "set", "nested"],
        &["sparse-checkout", "add", "drop.txt"],
        &["sparse-checkout", "set", "drop.txt"],
    ] {
        out.push(Case::new("sparse-checkout", args, Shape::Sparse).with_config(CONE_OFF));
    }

    // The mode switched off entirely, on a repository whose worktree is already
    // missing the excluded files: `status` must not start reporting them as
    // deleted, and `ls-files -t` must still show the bits.
    for (cmd, args) in [
        ("status", &["status", "--porcelain=v1", "--untracked-files=all"][..]),
        ("ls-files", &["ls-files", "-t"]),
    ] {
        out.push(
            Case::new(cmd, args, Shape::Sparse)
                .with_config(&[("core.sparseCheckout", "false")]),
        );
    }

    // A repository that has never been sparse, told that it is.
    out.push(
        Case::strict("sparse-checkout", &["sparse-checkout", "list"], Shape::Linear)
            .with_config(&[("core.sparseCheckout", "true")]),
    );
    out.push(
        Case::strict("sparse-checkout", &["sparse-checkout", "list"], Shape::Linear)
            .with_config(&[("core.sparseCheckout", "true"), ("core.sparseCheckoutCone", "true")]),
    );
    out.push(
        Case::new("status", &["status", "--porcelain"], Shape::Linear)
            .with_config(&[("core.sparseCheckout", "true")]),
    );
    out.push(
        Case::new("ls-files", &["ls-files", "-t"], Shape::Linear)
            .with_config(&[("core.sparseCheckout", "true")]),
    );

    // `advice.updateSparsePath` off, which is the hint `add_rm_mv_clean.rs`
    // measures on: the refusal stays, the hint goes.
    out.push(
        Case::strict("add", &["add", "outside/drop.txt"], Shape::Sparse)
            .with_config(&[("advice.updateSparsePath", "false")]),
    );
    // `advice.sparseIndexExpanded` off. The hint itself is unreachable from one
    // argv (see the module header); what this measures is the key being
    // accepted and the sparse index still being written.
    out.push(
        Case::new("sparse-checkout", &["sparse-checkout", "init", "--sparse-index"], Shape::Sparse)
            .with_config(&[("advice.sparseIndexExpanded", "false")]),
    );
}

// ---------------------------------------------------------------------------
// Patterns delivered on stdin
// ---------------------------------------------------------------------------

/// Two cone directories, newline separated.
const CONE_DIRS: &[u8] = b"inside\noutside\n";
/// The same two, NUL separated, for `-z`.
const CONE_DIRS_NUL: &[u8] = b"inside\0outside\0";
/// A non-cone rule set: keep `inside/` but not its `nested/` subtree.
const NON_CONE_RULES: &[u8] = b"/inside/\n!/inside/nested/\n";

/// `--stdin`: the operand list arrives on stdin instead of argv.
///
/// The parser is a different one from argv's — it has to split on the chosen
/// separator, and with `-z` it must not treat a newline as one — and it is the
/// mode `git clone --sparse` and scripted callers use. `set --stdin` with the
/// stream closed is the empty-operand case, and is a different invocation from
/// `set` with no operands at all, which is why both are here.
fn patterns_from_stdin(out: &mut Vec<Case>) {
    out.push(sc_stdin(&["sparse-checkout", "set", "--stdin"], Shape::Sparse, CONE_DIRS));
    out.push(sc_stdin(&["sparse-checkout", "set", "--stdin", "-z"], Shape::Sparse, CONE_DIRS_NUL));
    out.push(sc_stdin(&["sparse-checkout", "add", "--stdin"], Shape::Sparse, CONE_DIRS));
    out.push(sc_stdin(&["sparse-checkout", "set", "--stdin", "--no-cone"], Shape::Sparse, NON_CONE_RULES));
    out.push(sc_stdin(&["sparse-checkout", "set", "--stdin"], Shape::Linear, CONE_DIRS));
    // The stream closed: no operands at all, delivered through the stdin path.
    out.push(sc(&["sparse-checkout", "set", "--stdin"], Shape::Sparse));
}

// ---------------------------------------------------------------------------
// The one message with no other symptom
// ---------------------------------------------------------------------------

/// `warning: directory 'outside/' contains untracked files, but is not in the
/// sparse-checkout cone`.
///
/// Stock emits this whenever it re-applies patterns that leave `outside/` out
/// of the cone while `outside/stray.txt` is still on disk — which is nearly
/// every mutating invocation on [`Shape::Sparse`]. The checkout it accompanies
/// is byte-identical whether or not the warning is printed, so it is invisible
/// to stdout, to the exit code and to the state digest: a port that never
/// prints it matches on every non-strict case in this file. Verified against
/// stock 2.55.0 and corroborated by git 2.50.1; the port prints nothing.
///
/// Three representatives, one per subcommand that can reach it, deliberately
/// not one per invocation — see the module header's refusal budget. Every other
/// case that emits this warning is `Case::new` and measures its checkout.
fn untracked_cone_warning(out: &mut Vec<Case>) {
    out.push(sc_strict(&["sparse-checkout", "set", "nonexistent"], Shape::Sparse));
    out.push(sc_strict(&["sparse-checkout", "reapply", "--cone"], Shape::Sparse));
    out.push(sc_strict(&["sparse-checkout", "init", "extra-arg"], Shape::Sparse));
}

// ---------------------------------------------------------------------------
// Argument handling per subcommand
// ---------------------------------------------------------------------------

/// What each subcommand does with an argument it has no use for, and with none
/// at all.
///
/// `list`, `disable`, `reapply`, `init` and `clean` take no operands, and git
/// accepts a stray one on some of them and not others; `set` and `add` require
/// at least one and differ on what they do without. This is the parse table,
/// and it is exactly the kind of thing a port fills in by guessing.
///
/// `init extra-arg` is in [`untracked_cone_warning`] rather than here, because
/// on this shape its answer is that warning.
fn subcommand_arguments(out: &mut Vec<Case>) {
    for args in [
        &["sparse-checkout", "list", "extra-arg"][..],
        &["sparse-checkout", "disable", "extra-arg"],
        &["sparse-checkout", "clean", "extra-arg"],
    ] {
        out.push(sc_strict(args, Shape::Sparse));
    }
    // `set` and `add` with no operands, in both modes. `set --no-cone` with
    // nothing to set is the sharp one and its answer is a working tree: stock
    // leaves the pattern file holding `/*` and `!/*/`, so the root files stay
    // checked out, while the port writes an empty file, which matches nothing
    // and marks every entry `SKIP_WORKTREE`. Verified against stock 2.55.0 and
    // corroborated by git 2.50.1:
    //
    // ```text
    // stock  README.md and root.txt stay (H); pattern file: /* and !/*/
    // port   every entry S; worktree holds only the untracked outside/stray.txt
    // ```
    for args in [
        &["sparse-checkout", "reapply", "extra-arg"][..],
        &["sparse-checkout", "set"],
        &["sparse-checkout", "add"],
        &["sparse-checkout", "set", "--no-cone"],
    ] {
        out.push(sc(args, Shape::Sparse));
    }

    // `clean` removes the directories a previous narrowing left behind. It is
    // the newest subcommand in the set and `worktree_index.rs` only reaches its
    // "no sparse-checkout to clean" refusal on `Linear`; here it has something
    // to do.
    out.push(sc(&["sparse-checkout", "clean"], Shape::Sparse));
    out.push(sc(&["sparse-checkout", "clean", "--dry-run"], Shape::Sparse));
    out.push(sc(&["sparse-checkout", "reapply", "--cone"], Shape::Linear));
    out.push(sc(&["sparse-checkout", "set", "--skip-checks", "nope"], Shape::Linear));
}

// ---------------------------------------------------------------------------
// From inside the tree
// ---------------------------------------------------------------------------

/// The same subcommands run from a subdirectory.
///
/// Cone operands are paths from the repository root, not from the working
/// directory, so a port that joins the prefix produces a different cone from
/// the same argv. `inside/` is the only subdirectory of [`Shape::Sparse`] that
/// exists on disk — the excluded ones do not — so it is the one place a case
/// can stand.
fn from_a_subdirectory(out: &mut Vec<Case>) {
    for args in [
        &["sparse-checkout", "list"][..],
        &["sparse-checkout", "set", "nested"],
        &["sparse-checkout", "set", "."],
        &["sparse-checkout", "add", "nested"],
        &["sparse-checkout", "reapply"],
        &["sparse-checkout", "set", "--no-cone", "/nested/"],
    ] {
        out.push(sc(args, Shape::Sparse).in_dir("inside"));
    }
}

// ---------------------------------------------------------------------------
// Every other verb, against a tree that is missing half its files
// ---------------------------------------------------------------------------

/// The verbs that have to decide what a `SKIP_WORKTREE` entry means.
///
/// `shape_reach.rs` asks each of these its plain question; these are the forms
/// it does not spell — the `--sparse`/`--no-sparse` pathspec flags on the verbs
/// it does not cover, the destructive spellings it deliberately avoids, and the
/// index-writing verbs run with a sparse index requested.
///
/// `worktree add wt2` is the sharpest of them, and it is a state divergence
/// rather than a message. Verified against stock 2.55.0, corroborated by git
/// 2.50.1: stock copies the current worktree's sparse settings into the new
/// worktree's own config and checks it out sparsely; the port writes no
/// per-worktree config and checks out the whole tree.
///
/// ```text
/// stock: .git/worktrees/wt2/config.worktree = [core] sparseCheckout = true
///                                                    sparseCheckoutCone = true
///        wt2/ holds README.md, inside/, root.txt
/// port:  no .git/worktrees/wt2/config.worktree
///        wt2/ holds README.md, inside/, outside/, root.txt, src/
/// ```
fn sparse_worktree_verbs(out: &mut Vec<Case>) {
    // `mv` across the cone boundary, in both directions, with and without the
    // flags that lift the refusal.
    for args in [
        &["mv", "inside/keep.txt", "outside/moved.txt"][..],
        &["mv", "outside/drop.txt", "inside/moved.txt"],
        &["mv", "--no-sparse", "outside/drop.txt", "inside/moved.txt"],
        &["mv", "-k", "outside/drop.txt", "inside/moved.txt"],
        &["mv", "inside/keep.txt", "inside/renamed.txt"],
    ] {
        out.push(Case::new("mv", args, Shape::Sparse));
    }

    // `rm` with the flag spelled the other way, and recursively over a
    // directory that is not on disk.
    for args in [
        &["rm", "--no-sparse", "--cached", "outside/drop.txt"][..],
        &["rm", "-r", "--cached", "--sparse", "outside"],
        &["rm", "--cached", "--sparse", "outside/nested/deep.txt"],
    ] {
        out.push(Case::new("rm", args, Shape::Sparse));
    }
    out.push(Case::new("add", &["add", "--no-sparse", "outside/drop.txt"], Shape::Sparse));

    // `restore` is not asked anything about sparse anywhere in the corpus, and
    // it is the verb whose whole job is putting a file back — including one
    // that is deliberately absent.
    for args in [
        &["restore", "inside/keep.txt"][..],
        &["restore", "outside/drop.txt"],
        &["restore", "--staged", "outside/drop.txt"],
        &["restore", "--source=HEAD", "outside/drop.txt"],
    ] {
        out.push(Case::new("restore", args, Shape::Sparse));
    }
    out.push(Case::new("switch", &["switch", "-c", "topic"], Shape::Sparse));
    out.push(Case::new("commit", &["commit", "-am", "wip"], Shape::Sparse));
    out.push(Case::new("stash", &["stash", "push"], Shape::Sparse));
    out.push(Case::new("worktree", &["worktree", "add", "wt2"], Shape::Sparse));

    // `checkout-index` writes the working tree straight from the index and has
    // no sparse awareness of its own, so what it does with a `SKIP_WORKTREE`
    // entry is decided entirely by whether the caller filtered it out first.
    for args in [
        &["checkout-index", "--all", "--force"][..],
        &["checkout-index", "-f", "-a", "--stage=all"],
    ] {
        out.push(Case::new("checkout-index", args, Shape::Sparse));
    }

    // The bit itself, set and cleared by hand.
    for args in [
        &["update-index", "--skip-worktree", "nosuch.txt"][..],
        &["update-index", "--no-skip-worktree", "--stdin"],
    ] {
        out.push(Case::new("update-index", args, Shape::Sparse));
    }

    // `ls-files --sparse` combinations. The flag changes whether an excluded
    // directory prints as one entry or as its files, so it interacts with every
    // other selector rather than being independent of them.
    for args in [
        &["ls-files", "--sparse", "-t"][..],
        &["ls-files", "-t", "--sparse", "-v"],
        &["ls-files", "--sparse", "--deleted"],
        &["ls-files", "--sparse", "--others", "--exclude-standard"],
        &["ls-files", "--sparse", "--directory"],
        &["ls-files", "--format=%(path)%(objectname)"],
    ] {
        out.push(Case::new("ls-files", args, Shape::Sparse));
    }
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch"], Shape::Sparse));
    out.push(Case::new("status", &["status", "--ignored", "--porcelain"], Shape::Sparse));
    out.push(Case::new("grep", &["grep", "--cached", "-n", "kept"], Shape::Sparse));

    // The same verbs asked to keep the index sparse while they write it. The
    // sparse index has to be expanded to answer most of these and collapsed
    // again afterwards, and `probe_index_meta` sees the `TREE` extension that
    // survives the round trip.
    //
    // That is what the first two of these catch, and the finding belongs to the
    // sparse index rather than to `add` or `rm`: run without `index.sparse` the
    // two sides produce byte-identical cache trees, and with it the port
    // invalidates entries stock keeps. Verified by parsing both indexes'
    // `TREE` extensions by hand after `-c index.sparse=true add inside/keep.txt`
    // — a path that is already in the index unchanged, so nothing may be
    // invalidated at all:
    //
    // ```text
    // stock  <root> entries=7 subtrees=3 oid=288621d…   inside entries=2 oid=c465c8e…
    // port   <root> entries=-1 subtrees=3 oid=(none)    inside entries=-1 oid=(none)
    // ```
    //
    // `-c index.sparse=true rm --cached --sparse outside/nested/deep.txt` is the
    // other direction: stock prunes the emptied `outside/nested` subtree and
    // recomputes `outside` and `<root>`; the port keeps the dead subtree with
    // `entries=-1` and invalidates its two ancestors instead. Both make stock's
    // own `write-tree` repair the index the port left, which
    // `probe_interop` reports as `index-repaired: yes` against stock's `no`.
    const SPARSE_INDEX: &[(&str, &str)] = &[("index.sparse", "true")];
    for (cmd, args) in [
        ("add", &["add", "inside/keep.txt"][..]),
        ("rm", &["rm", "--cached", "--sparse", "outside/nested/deep.txt"]),
        ("commit", &["commit", "--allow-empty", "-m", "x"]),
        ("read-tree", &["read-tree", "-m", "-u", "HEAD"]),
        ("checkout-index", &["checkout-index", "-a", "-f"]),
        ("grep", &["grep", "-n", "kept"]),
        ("diff", &["diff", "--cached"]),
        ("update-index", &["update-index", "--refresh"]),
        ("status", &["status", "--porcelain"]),
        ("ls-files", &["ls-files", "--sparse"]),
    ] {
        out.push(Case::new(cmd, args, Shape::Sparse).with_config(SPARSE_INDEX));
    }
}
