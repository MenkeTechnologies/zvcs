//! How a diff is **computed**, as opposed to which paths it covers: the
//! algorithm that produces the edit script, the rename/copy detector's
//! thresholds and limits, and the summary/indicator renderings that are
//! arithmetic over the result rather than a transcription of it.
//!
//! Every case here is `git diff` inside a repository. That is the boundary this
//! module is drawn along, because the in-repository path is the one the corpus
//! had reached least on this axis — see the territory list below.
//!
//! # The finding that shapes this file
//!
//! **No fixture in the corpus contains a file pair on which git's four diff
//! algorithms disagree.** Measured, not assumed: every parent/child commit pair
//! reachable from every ref of every template, every `diff`/`diff --cached`/
//! `diff <a> <b>` over every pair of branches, and 566 `--no-index` file pairs
//! drawn from the nine templates with the most text (1698 algorithm
//! comparisons in that last sweep alone) were run through
//! `--diff-algorithm=myers` and then through `minimal`, `patience` and
//! `histogram`, and through
//! `--no-indent-heuristic`, against stock 2.55.0. Not one produced a different
//! byte. The reason is in the fixtures rather than in git: `fixture.rs` builds
//! its text with `numbered()`/`numbered_with_edits()`, so every line of every
//! generated file is unique, and an LCS over lines that are all distinct is
//! unambiguous — which is exactly the condition under which patience and myers
//! must agree. The three hand-written C-shaped payloads (`WS_*`, `MAIN_C_*`,
//! `ni/fn_?.c`) do repeat `}` and blank lines, but their edits are single
//! contiguous insertions that no algorithm mis-anchors, and git's default
//! indent heuristic removes the one classic disagreement (a function added
//! above another) before the algorithms can differ on it.
//!
//! Two consequences, stated plainly rather than papered over:
//!
//! * A port that parses all four algorithm names and runs one algorithm passes
//!   every case in this file and every case in the corpus. **The algorithm axis
//!   cannot be separated by output on the current fixture set.** Separating it
//!   needs a new `Shape` carrying a moved block over repeated lines, which a
//!   corpus module is not allowed to add.
//! * What the cases below *do* measure is therefore stated honestly: the option
//!   surface (every spelling, the config key, the precedence between them, the
//!   two error paths) and the guarantee that selecting an algorithm does not
//!   change the answer where stock says it must not. That is worth pinning —
//!   `--anchored=` and `--diff-algorithm <arg>` (detached argument) are parsed
//!   nowhere else in the corpus, and a port that rejects one of them, or
//!   swallows the anchor as a pathspec, fails here — but it is not the same as
//!   measuring four algorithms.
//!
//! For the record, because it is the first question a reader of this file will
//! ask: the port does **not** alias the four names to one implementation. Asked
//! outside the corpus, on 250 random line sequences over a six-letter alphabet
//! (211 of which split stock's four algorithms into more than one answer), the
//! port matched stock 2.55.0 on `myers`, `minimal` and `patience` 250/250 and
//! on `histogram` 248/250 — and on one of the two exceptions stock 2.50.1
//! disagrees with stock 2.55.0 as well, so even that is not a clean defect.
//! That measurement cannot become a case here: it needs file pairs no shape
//! contains, which is the same conclusion from the other direction.
//!
//! Everything outside the algorithm block *is* separable, and was chosen on
//! that basis: each group below contains at least one pair of cases whose stock
//! outputs differ from each other, so a port that implements one member and
//! aliases the rest is caught.
//!
//! # Territory
//!
//! * **`diff_family.rs`** owns the plumbing diff verbs (`diff-files`,
//!   `diff-index`, `diff-tree`, `diff-pairs`), `range-diff`, `whatchanged` and
//!   the `pickaxe` label. Nothing here uses a plumbing verb, and no case here
//!   carries the `pickaxe` label. Its rename cases are bare detector flags
//!   (`-M`, `-B`, `-C`, `-l1`, `--no-renames`) asked of `diff-files`/
//!   `diff-index` for *output shape*; the thresholds and limits below are asked
//!   of `diff` on the shape that has renames in history.
//! * **`no_index_diff.rs`** owns `git diff --no-index` — the queue built by
//!   `stat` with no repository, no index, no attributes and no `diff.*` config
//!   behind it. It is the only module that reached `--diff-algorithm=`,
//!   `--anchored=`, `--color-words`, `--stat-width=`, `--dirstat-by-file`,
//!   `-W` or `--stat=<w>` at all, and it reached all of them *outside* a
//!   repository under `GIT_CEILING_DIRECTORIES`. Every one of those options has
//!   a second life inside a repository, where `diff.*` config can reach it and
//!   where the queue comes from trees; that second life is this file. No case
//!   here passes `--no-index`.
//! * **`log_format.rs`** owns the *presentation* half of `diff.c` and states so:
//!   the stat family's `--stat=60,30`, `--stat-name-width=12`,
//!   `--stat-graph-width=8`, `diff.statGraphWidth`, `diff.statNameWidth`, the
//!   two `--dirstat` parameters it pins (`files,0` and `cumulative,0`) and
//!   `diff.dirstat=lines,0`, plus word diff, colour, moved-block detection,
//!   prefixes and hunk shaping (`-U0`, `-U6`, `--function-context`,
//!   `--inter-hunk-context`). This file adds only what that leaves out: the
//!   **third** `--stat` argument (the file-count cut-off and its `...` line),
//!   `--stat-width=`/`--stat-graph-width=3` at widths that bite, the two
//!   plausible-but-nonexistent config keys (`diff.statWidth`,
//!   `diff.statCount`), the **three remaining** `--dirstat` parameters (`changes`,
//!   `lines`, a bare percentage) plus the whole `diff.dirstat` value space, the
//!   `-W` short spelling, and the `--output-indicator-*` trio, which the corpus
//!   had only under `range-diff`.
//! * **`blame_lines.rs`** and **`info_attrs.rs`** own `blame`, including the
//!   only `--indent-heuristic`/`--no-indent-heuristic` cases in the corpus
//!   (`blame --indent-heuristic src/lib.rs`, `blame -s --no-indent-heuristic`).
//!   `diff`'s own two spellings, and `diff.indentHeuristic` in any command, are
//!   here and were previously unreachable.
//! * **`attributes_filters.rs`** owns `Shape::Attributes` as a *rule* fixture:
//!   `diff.<driver>.textconv`, `.funcname`, `.binary`, `.wordregex`,
//!   `.algorithm`, `core.autocrlf`. The cases here that use that shape set no
//!   attribute-related configuration; they use its root commit purely as the
//!   only queue in the corpus with eleven files across six directories of very
//!   different sizes, which is what makes `--stat`'s three widths and
//!   `--dirstat`'s five parameters produce five distinguishable answers.
//! * **`mail_patch.rs`** and **`apply_patch.rs`** own `format-patch`/`am` and
//!   `apply`. No case here produces or consumes a patch file.
//! * **`shape_reach.rs`** owns the rename/copy *threshold sweep* on
//!   `Shape::Renamed` (`-M50%`…`-M90%`, `-C50%`, `-B50%`, `--find-copies=90%`).
//!   The rename cases here are the parts of the detector it does not touch: the
//!   `-l<num>` **limit** (and the warning it prints when it fires), the
//!   two-number `-B<n>/<m>` form, `--rename-empty`/`--no-rename-empty`, and
//!   `--irreversible-delete` on a tree-to-tree deletion, crossed with the two
//!   summarisers.
//!
//! # Determinism
//!
//! Every case below was run twice against stock 2.55.0 in two independent
//! copies of its template and compared byte for byte before being written down.
//! Three environmental facts are load-bearing:
//!
//! * **`--stat` widths do not depend on the terminal.** `env::harden` calls
//!   `Command::env_clear`, so `COLUMNS` is absent, and the child's stdout is a
//!   pipe rather than a tty, so `term_columns()` falls back to 80 on both
//!   sides. `TERM=dumb` never reaches the width computation; it only keeps
//!   colour's `auto` mode off.
//! * **Colour is reachable, by two routes.** `--color=always` overrides
//!   `NO_COLOR`, and — the surprising one — `--color-words` implies
//!   `--word-diff=color`, which *forces* colour on regardless of `NO_COLOR` and
//!   the dumb terminal. `diff --color-words` therefore emits SGR sequences in
//!   this harness with no `--color` flag anywhere, which is why the group below
//!   pins it with and without an explicit `--color=always`.
//! * **`--dirstat` percentages are a function of blob sizes only**, and the
//!   blobs of `Shape::Attributes`' root commit are fixed bytes in `fixture.rs`
//!   (a 1024-byte `assets/logo.bin` dominating five small text files), so the
//!   six percentages are constants of the fixture rather than of the machine.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    algorithm_selection(out);
    anchored(out);
    indent_heuristic(out);
    rename_limits(out);
    break_and_delete(out);
    stat_arithmetic(out);
    dirstat_parameters(out);
    indicators_and_words(out);
    whitespace_errors(out);
}

/// One `git diff` case.
fn d(out: &mut Vec<Case>, shape: Shape, args: &[&str]) {
    out.push(Case::new("diff", args, shape));
}

/// The same, compared on stderr too: for the cases whose whole answer is a
/// diagnostic — a refusal, or a warning emitted beside a correct stdout.
fn d_strict(out: &mut Vec<Case>, shape: Shape, args: &[&str]) {
    out.push(Case::strict("diff", args, shape));
}

/// One `git diff` case with `-c <key>=<value>` in front of the subcommand.
fn d_cfg(out: &mut Vec<Case>, shape: Shape, config: &[(&str, &str)], args: &[&str]) {
    out.push(Case::new("diff", args, shape).with_config(config));
}

/// Selecting the edit-script algorithm: four names, five spellings, one config
/// key, and the precedence between them.
///
/// What this measures, given the finding in the module header: not which
/// algorithm ran — no fixture can show that — but that each spelling is
/// *accepted* and leaves the diff stock produces unchanged. Three of the
/// spellings are parsed nowhere else in the corpus at all: the detached-argument
/// form (`--diff-algorithm patience`, where the value is a separate argv
/// element and a naive parser eats the next token as a revision), the
/// last-one-wins stack (`--patience --histogram`, and a long form overriding a
/// short one), and the empty value.
///
/// The two error paths are the only cases in the group whose output differs
/// from every other, and they differ from *each other*: an unknown value on the
/// command line is a `parse-options` error at exit 129, while the same unknown
/// value in configuration is a config-parse `fatal` at exit 128. A port that
/// routes both through one validator gets one of the two exit codes wrong.
fn algorithm_selection(out: &mut Vec<Case>) {
    for algo in ["myers", "minimal", "patience", "histogram"] {
        let arg = format!("--diff-algorithm={algo}");
        d(out, Shape::Whitespace, &["diff", &arg, "HEAD~1", "HEAD"]);
    }
    // The detached-argument spelling, and the three short flags. The corpus
    // reaches the short flags only under `-w` (`shape_reach.rs`'s
    // `diff -w --patience`), where whatever they select is applied to a
    // whitespace-collapsed pair; these are the same flags with nothing else on.
    d(out, Shape::Whitespace, &["diff", "--diff-algorithm", "patience", "HEAD~1", "HEAD"]);
    d(out, Shape::Whitespace, &["diff", "--patience", "HEAD~1", "HEAD"]);
    d(out, Shape::Whitespace, &["diff", "--histogram", "HEAD~1", "HEAD"]);
    d(out, Shape::Whitespace, &["diff", "--minimal", "HEAD~1", "HEAD"]);
    // Last occurrence wins, in both directions between the short and long
    // spellings. `--minimal` is not an algorithm but a flag on top of one, so
    // the third case is a different question from the first two.
    d(out, Shape::Whitespace, &["diff", "--patience", "--histogram", "HEAD~1", "HEAD"]);
    d(out, Shape::Whitespace, &["diff", "--histogram", "--diff-algorithm=myers", "HEAD~1", "HEAD"]);
    d(out, Shape::Whitespace, &["diff", "--diff-algorithm=patience", "--minimal", "HEAD~1", "HEAD"]);

    // `diff.algorithm`: the one value the corpus never set (`myers`, the
    // default, which is the value that catches a port that treats "config
    // present" as "not default"), and the flag-beats-config precedence in both
    // spellings.
    d_cfg(out, Shape::Whitespace, &[("diff.algorithm", "myers")], &["diff", "HEAD~1", "HEAD"]);
    d_cfg(
        out,
        Shape::Whitespace,
        &[("diff.algorithm", "patience")],
        &["diff", "--diff-algorithm=myers", "HEAD~1", "HEAD"],
    );
    d_cfg(
        out,
        Shape::Whitespace,
        &[("diff.algorithm", "histogram")],
        &["diff", "--patience", "HEAD~1", "HEAD"],
    );

    // The same rejected value through the two parsers, and the empty value.
    out.push(
        Case::strict("diff", &["diff", "HEAD~1", "HEAD"], Shape::Whitespace)
            .with_config(&[("diff.algorithm", "nosuch")]),
    );
    d_strict(out, Shape::Whitespace, &["diff", "--diff-algorithm=nosuch", "HEAD~1", "HEAD"]);
    d_strict(out, Shape::Whitespace, &["diff", "--diff-algorithm=", "HEAD~1", "HEAD"]);

    // The algorithm choice must survive the two post-processing passes that run
    // over the same queue: rename detection, and the stat summariser.
    d(out, Shape::Renamed, &["diff", "--diff-algorithm=histogram", "-M", "HEAD~3", "HEAD~2"]);
    d(out, Shape::Renamed, &["diff", "--patience", "--stat", "HEAD~5", "HEAD"]);

    // `Shape::Patches`' `pending` branch is the corpus's only hand-written C
    // payload with a function *inserted above* another — the shape of change
    // the patience algorithm exists for. Stock renders it identically under all
    // four algorithms (the indent heuristic settles it first), which is the
    // measurement: these cases pin that agreement, and would be the first to
    // move if a port's patience implementation anchored differently.
    d(out, Shape::Patches, &["diff", "--diff-algorithm=patience", "main", "pending"]);
    d(out, Shape::Patches, &["diff", "--diff-algorithm=myers", "main", "pending"]);
    d(out, Shape::Patches, &["diff", "--histogram", "main", "pending"]);
}

/// `--anchored=<text>`: lines matching the text are forced into the common
/// subsequence, which switches the driver to the patience algorithm with
/// anchors whatever `--diff-algorithm` said.
///
/// Reachable in the corpus at exactly two places before this: one `--no-index`
/// case, and one `diff-pairs` refusal. The option is repeatable — every
/// occurrence adds an anchor — and its argument is a bare string that a parser
/// which mis-handles it will take for a revision or a pathspec. The anchor that
/// matches nothing is the control: it must leave the diff exactly as it was.
fn anchored(out: &mut Vec<Case>) {
    d(out, Shape::Whitespace, &["diff", "--anchored=main", "HEAD~1", "HEAD"]);
    d(out, Shape::Whitespace, &["diff", "--anchored=main", "--anchored=return", "HEAD~1", "HEAD"]);
    d(out, Shape::Whitespace, &["diff", "--anchored=nosuchtext", "HEAD~1", "HEAD"]);
    // An anchor and an explicit algorithm together: the anchor wins the driver
    // selection, so this must not differ from the case above it.
    d(out, Shape::Whitespace, &["diff", "--anchored=main", "--diff-algorithm=myers", "HEAD~1", "HEAD"]);
    d(out, Shape::Patches, &["diff", "--anchored=int", "main", "pending"]);
}

/// The indent heuristic: on by default since git 2.14, and switchable from
/// either the command line or configuration.
///
/// `diff.indentHeuristic` was set by no case in the corpus, in any command, and
/// `diff`'s own `--indent-heuristic`/`--no-indent-heuristic` pair was reachable
/// only through `blame` and `format-patch`. The heuristic slides a hunk
/// boundary to the least-indented plausible position, so it is a *computation*
/// over the edit script the algorithm produced rather than a rendering of it —
/// which is why it lives here beside the algorithms and not in the stat block.
///
/// `bogus` is included because the key is a boolean and the failure is a
/// boolean-parse `fatal`, not the `diff.algorithm` string-parse one.
fn indent_heuristic(out: &mut Vec<Case>) {
    d(out, Shape::Whitespace, &["diff", "--indent-heuristic", "HEAD~1", "HEAD"]);
    d(out, Shape::Whitespace, &["diff", "--no-indent-heuristic", "HEAD~1", "HEAD"]);
    d_cfg(out, Shape::Whitespace, &[("diff.indentHeuristic", "false")], &["diff", "HEAD~1", "HEAD"]);
    d_cfg(
        out,
        Shape::Whitespace,
        &[("diff.indentHeuristic", "true")],
        &["diff", "--no-indent-heuristic", "HEAD~1", "HEAD"],
    );
    out.push(
        Case::strict("diff", &["diff", "HEAD~1", "HEAD"], Shape::Whitespace)
            .with_config(&[("diff.indentHeuristic", "bogus")]),
    );
    d(out, Shape::Patches, &["diff", "--no-indent-heuristic", "main", "pending"]);
}

/// The rename/copy **limit**, `-l<num>`: how many candidate pairs the inexact
/// pass is allowed to consider before it gives up.
///
/// The distinguishing case is `-C --find-copies-harder -l1` across five files,
/// where stock actually exceeds the limit and says so on stderr:
///
/// ```text
/// warning: only found copies from modified paths due to too many files.
/// warning: you may want to set your diff.renameLimit variable to at least 5
///   and retry the command.
/// ```
///
/// Compared strictly, because the warning *is* the behaviour: stdout is the
/// same five `A` records whether the limit fired or not, so a port that
/// silently ignores `-l` is invisible on stdout alone. `-l0` (no limit) and
/// `-l5` (exactly enough) are the two controls that must stay silent, and they
/// bracket the threshold from both sides.
///
/// `diff.renameLimit` is left to `config_reads.rs` and `diff_family.rs`, which
/// already set it; the flag spelling was reached only once in the corpus, by
/// `diff-files -l1`, on a queue where it cannot fire.
fn rename_limits(out: &mut Vec<Case>) {
    d_strict(
        out,
        Shape::Renamed,
        &["diff", "-C", "--find-copies-harder", "-l1", "--name-status", "HEAD~5", "HEAD"],
    );
    d_strict(
        out,
        Shape::Renamed,
        &["diff", "-C", "--find-copies-harder", "-l0", "--name-status", "HEAD~5", "HEAD"],
    );
    d_strict(
        out,
        Shape::Renamed,
        &["diff", "-C", "--find-copies-harder", "-l5", "--name-status", "HEAD~5", "HEAD"],
    );
    // A limit of one over a queue with one candidate pair: under the threshold,
    // so detection still runs and the `R072` record survives.
    d(out, Shape::Renamed, &["diff", "-M", "-l1", "--name-status", "HEAD~3", "HEAD~2"]);
    d(out, Shape::Renamed, &["diff", "-M", "-C", "-l1", "--name-status", "HEAD~2", "HEAD~1"]);
}

/// The rewrite splitter's two-number form, and the deletion renderings that
/// depend on it.
///
/// `-B<n>/<m>` is two thresholds, not one: `n` is how dissimilar a modification
/// must be before it is *split* into a delete and an add, and `m` is how
/// similar the halves must then be for the split to be *merged back*. Every
/// existing case in the corpus passes one number (`-B50%`) or none, so the
/// second threshold has never been parsed — and `--break-rewrites=/60`, which
/// omits the first, is the spelling that catches a parser splitting on `/`
/// without handling an empty left side.
///
/// `--rename-empty` and `--no-rename-empty` are set by no case in the corpus.
/// They decide whether an empty blob may be a rename source, which on this
/// fixture changes nothing — that invariance is the measurement, since the pair
/// is otherwise indistinguishable from an unparsed flag.
///
/// `--irreversible-delete` is the one flag in this group the corpus already
/// reaches: `diff_family.rs` gives it to `diff-files -p` on `Shape::Dirty`,
/// whose queue does hold a deletion (`D src/lib.rs`), and `log_format.rs`
/// reaches the same suppression through `-B -D` on a rewrite. What neither has
/// is the porcelain spelling on a **tree-to-tree** deletion, or the flag
/// crossed with the two summarisers: `--stat` and `--numstat` must report the
/// removed lines they are no longer printing, and a port that suppresses the
/// pre-image before the counters run reports zeroes on both. The three cases
/// are one queue (`orig/alpha.txt` disappearing with `--no-renames` in force)
/// through the three emitters.
fn break_and_delete(out: &mut Vec<Case>) {
    d(out, Shape::Renamed, &["diff", "-B10%/20%", "--name-status", "HEAD~1", "HEAD"]);
    d(out, Shape::Renamed, &["diff", "-B90%/95%", "--name-status", "HEAD~1", "HEAD"]);
    d(out, Shape::Renamed, &["diff", "--break-rewrites=50/60", "--name-status", "HEAD~1", "HEAD"]);
    d(out, Shape::Renamed, &["diff", "--break-rewrites=/60", "--summary", "HEAD~1", "HEAD"]);
    d(out, Shape::Renamed, &["diff", "--break-rewrites", "--stat", "HEAD~1", "HEAD"]);
    d(out, Shape::Renamed, &["diff", "--rename-empty", "--name-status", "HEAD~3", "HEAD~2"]);
    d(out, Shape::Renamed, &["diff", "--no-rename-empty", "--name-status", "HEAD~3", "HEAD~2"]);
    // The deletion, in the three renderings that report it differently: the
    // patch (pre-image suppressed), the stat (line counts unaffected by the
    // suppression) and numstat (likewise).
    d(out, Shape::Renamed, &["diff", "--no-renames", "--irreversible-delete", "HEAD~4", "HEAD~3"]);
    d(out, Shape::Renamed, &["diff", "--no-renames", "--irreversible-delete", "--stat", "HEAD~4", "HEAD~3"]);
    d(out, Shape::Renamed, &["diff", "--no-renames", "--irreversible-delete", "--numstat", "HEAD~4", "HEAD~3"]);
}

/// `--stat`'s three widths and its file-count cut-off.
///
/// `Shape::Attributes`' root commit is the queue: eleven files across six
/// directories, three of them binary, with name lengths from `.mailmap` to
/// `vendor/generated.js`. That spread is what makes the arithmetic visible —
/// on a two-file queue every width choice prints the same thing.
///
/// The third `--stat` argument is the gap: `--stat=<width>,<name-width>,<count>`
/// stops after `count` files and prints a bare ` ...` line in place of the
/// rest, a line no case in the corpus has ever produced. Verified on stock:
///
/// ```text
/// $ git diff --stat=60,20,4 HEAD~4 HEAD~3
///  .gitattributes  |   8 ++++++++
///  .gitignore      |   6 ++++++
///  .mailmap        |   4 ++++
///  assets/logo.bin | Bin 0 -> 1024 bytes
///  ...
///  11 files changed, 28 insertions(+)
/// ```
///
/// `--stat-width=` is the same budget spelled as its own option, and it has to
/// be small enough to bite: at 40 the graph already fits and the output is the
/// default byte for byte (measured), so the case uses 24, where the name column
/// starts truncating and the graph shrinks.
///
/// `diff.statCount` and `diff.statWidth` are **not** configuration keys git
/// has — the documented pair is `diff.statNameWidth`/`diff.statGraphWidth`, and
/// the width/count budgets have command-line spellings only. Both are pinned
/// here anyway, and deliberately: an unknown `diff.*` key must be *ignored*
/// silently, not rejected and not guessed at, and stock's output under each is
/// byte-identical to plain `--stat` (measured). A port that grew a plausible
/// key its own way would diverge on exactly these two cases and nowhere else.
/// `diff.statNameWidth` and `diff.statGraphWidth` are each set individually by
/// `log_format.rs`, so they appear here only together, where the two real
/// overrides have to compose.
fn stat_arithmetic(out: &mut Vec<Case>) {
    for args in [
        &["diff", "--stat=60,20,4", "HEAD~4", "HEAD~3"][..],
        // A cut-off of one: the `...` line stands for ten files.
        &["diff", "--stat=60,20,1", "HEAD~4", "HEAD~3"],
        // A name column narrower than the longest path, which git truncates
        // with a leading `...` inside the name itself.
        &["diff", "--stat=30,10", "HEAD~4", "HEAD~3"],
        &["diff", "--stat-width=24", "HEAD~4", "HEAD~3"],
        &["diff", "--stat-width=24", "--stat-name-width=10", "--stat-graph-width=5", "HEAD~4", "HEAD~3"],
        &["diff", "--stat-graph-width=3", "HEAD~4", "HEAD~3"],
    ] {
        d(out, Shape::Attributes, args);
    }
    d_cfg(out, Shape::Attributes, &[("diff.statCount", "3")], &["diff", "--stat", "HEAD~4", "HEAD~3"]);
    d_cfg(out, Shape::Attributes, &[("diff.statWidth", "44")], &["diff", "--stat", "HEAD~4", "HEAD~3"]);
    d_cfg(
        out,
        Shape::Attributes,
        &[("diff.statNameWidth", "9"), ("diff.statGraphWidth", "6")],
        &["diff", "--stat", "HEAD~4", "HEAD~3"],
    );
}

/// `--dirstat`'s five parameters, and the config key that carries the same
/// value space.
///
/// The five are not spellings of one thing: over the same queue they produce
/// five different sets of percentages, because each counts something else.
/// Measured on stock 2.55.0 against `Shape::Attributes`' root commit:
///
/// ```text
/// --dirstat=changes,0     --dirstat=lines,0      --dirstat=files,0
///   60.1% assets/           34.0% assets/           9.0% assets/
///    0.9% docs/              6.3% docs/             9.0% docs/
///    2.1% logs/              2.1% logs/             9.0% logs/
///    1.7% src/               6.3% src/              9.0% src/
///    3.4% sub/              10.6% sub/            27.2% sub/
///    1.5% vendor/            2.1% vendor/           9.0% vendor/
/// ```
///
/// and the bare-percentage form is a cut-off applied to the default counter:
/// `--dirstat=20` prints `60.1% assets/` and nothing else. `log_format.rs`
/// pins `files,0` and `cumulative,0` on `Shape::Renamed`; the other three
/// parameters, the combination of a counter with `cumulative`, and every value
/// of `diff.dirstat` except `lines,0` were unmeasured.
///
/// The config half of this group is where the corpus's one existing
/// `diff.dirstat` case was hiding a defect: the port parses the *flag* value
/// correctly and ignores the *config* value entirely, falling back to the
/// built-in `changes` counter with the built-in 3% cut-off. Both stock 2.55.0
/// and the 2.50.1 oracle agree against it, so the five failing cases below are
/// a corroborated defect and not version skew. The last case in the group is
/// the control that still passes: a flag on top of the config, where the flag
/// is honoured.
fn dirstat_parameters(out: &mut Vec<Case>) {
    for args in [
        &["diff", "--dirstat=changes,0", "HEAD~4", "HEAD~3"][..],
        &["diff", "--dirstat=lines,0", "HEAD~4", "HEAD~3"],
        &["diff", "--dirstat=files,0", "HEAD~4", "HEAD~3"],
        &["diff", "--dirstat=cumulative,0", "HEAD~4", "HEAD~3"],
        &["diff", "--dirstat=20", "HEAD~4", "HEAD~3"],
        &["diff", "--dirstat=files,cumulative,0", "HEAD~4", "HEAD~3"],
        &["diff", "--dirstat-by-file", "HEAD~4", "HEAD~3"],
    ] {
        d(out, Shape::Attributes, args);
    }
    for value in ["changes,0", "files,0", "cumulative,0", "0", "lines"] {
        d_cfg(out, Shape::Attributes, &[("diff.dirstat", value)], &["diff", "--dirstat", "HEAD~4", "HEAD~3"]);
    }
    d_cfg(
        out,
        Shape::Attributes,
        &[("diff.dirstat", "files,0")],
        &["diff", "--dirstat=lines,0", "HEAD~4", "HEAD~3"],
    );
}

/// The characters a patch line begins with, the word-diff mode that turns the
/// word machinery back off, and `-W`'s short spelling.
///
/// `--output-indicator-{new,old,context}` replaces `+`, `-` and the leading
/// space respectively. The corpus reached the trio only through `range-diff`,
/// which has its own emitter; on `diff` they were unmeasured, and the
/// context indicator is the one a port forgets, because the context "character"
/// is a space that looks like padding.
///
/// `--word-diff=none` is the mode that must undo an earlier `--word-diff` and
/// produce an ordinary patch; no case set it. `--color-words` and
/// `--color-words=<regex>` were reachable only outside a repository, and they
/// carry the surprise recorded in the module header: they force colour on
/// through `NO_COLOR` and a dumb terminal, so the first of the two cases below
/// emits SGR sequences with no `--color` flag anywhere. The explicit
/// `--color=always` case beside it is what shows that the forcing is the
/// option's own doing rather than the environment's.
///
/// `-W` inside a repository was likewise unreached: `log_format.rs` uses the
/// long `--function-context`, and only the `--no-index` block has `-W`. Paired
/// with `-U0` — where every context line printed comes from the function
/// extension rather than from the context width — and with `--stat`, where the
/// extension must *not* change the line counts.
fn indicators_and_words(out: &mut Vec<Case>) {
    d(
        out,
        Shape::Renamed,
        &[
            "diff",
            "--output-indicator-new=N",
            "--output-indicator-old=O",
            "--output-indicator-context=.",
            "HEAD~1",
            "HEAD",
        ],
    );
    d(out, Shape::Renamed, &["diff", "--output-indicator-new=N", "HEAD~3", "HEAD~2"]);
    d(out, Shape::Renamed, &["diff", "--output-indicator-context=~", "-U2", "HEAD~3", "HEAD~2"]);
    // `--check` prints no patch lines at all, so the indicator must not reach
    // its output.
    d(out, Shape::Whitespace, &["diff", "--output-indicator-old=O", "--check", "HEAD~1", "HEAD"]);

    d(out, Shape::Renamed, &["diff", "--word-diff=none", "HEAD~3", "HEAD~2"]);
    d(out, Shape::Renamed, &["diff", "--color-words", "HEAD~3", "HEAD~2"]);
    d(out, Shape::Renamed, &["diff", "--color-words=[a-z]+", "HEAD~3", "HEAD~2"]);
    d(out, Shape::Whitespace, &["diff", "--color=always", "--color-words", "HEAD~1", "HEAD"]);

    d(out, Shape::Attributes, &["diff", "-W", "-U0", "HEAD~1", "HEAD", "--", "src/tabs.rs"]);
    d(out, Shape::Attributes, &["diff", "-W", "--stat", "HEAD~1", "HEAD", "--", "src/tabs.rs"]);
}

/// Whitespace errors as a computation over the hunks: which class of line is
/// examined, and which byte patterns count as an error.
///
/// `--ws-error-highlight=` selects the *lines* the highlighter runs over
/// (`old`, `new`, `context`, and the `all`/`none`/`default` aliases), and the
/// four values below produce four different byte streams under
/// `--color=always` — `old` paints the removed line's trailing run, `new,context`
/// paints the added and context lines and leaves the removed one plain, and
/// `none`/`default` differ from both. The corpus had `all` and
/// `diff.wsErrorHighlight=all` and nothing else, so a port implementing the
/// alias table and not the three primitives passed.
///
/// `core.whitespace` selects the *rules*, and was set by no case in the corpus
/// at all — which left `--check`'s entire rule table unmeasured, since every
/// existing `--check` case runs on the built-in default. `-trailing-space`
/// turns off the rule that produces every error `Shape::Whitespace` has;
/// `tab-in-indent` turns on one that is off by default and that the shape's
/// tab-indented pre-image trips; `tabwidth=2,tab-in-indent` adds the numeric
/// parameter to the same list; `blank-at-eof` is a third rule with a different
/// scope. Each is a different `--check` answer over the same hunks.
fn whitespace_errors(out: &mut Vec<Case>) {
    for value in ["old", "new,context", "none", "default"] {
        let arg = format!("--ws-error-highlight={value}");
        d(out, Shape::Whitespace, &["diff", "--color=always", &arg, "HEAD~1", "HEAD"]);
    }
    d_cfg(
        out,
        Shape::Whitespace,
        &[("diff.wsErrorHighlight", "old")],
        &["diff", "--color=always", "HEAD~1", "HEAD"],
    );
    d_cfg(
        out,
        Shape::Whitespace,
        &[("core.whitespace", "-trailing-space")],
        &["diff", "--check", "HEAD~2", "HEAD~1"],
    );
    d_cfg(
        out,
        Shape::Whitespace,
        &[("core.whitespace", "tab-in-indent")],
        &["diff", "--check", "HEAD~1", "HEAD"],
    );
    d_cfg(
        out,
        Shape::Whitespace,
        &[("core.whitespace", "tabwidth=2,tab-in-indent")],
        &["diff", "--check", "HEAD~1", "HEAD"],
    );
    d_cfg(
        out,
        Shape::Whitespace,
        &[("core.whitespace", "blank-at-eof")],
        &["diff", "--check", "HEAD~2", "HEAD~1"],
    );
    d_cfg(
        out,
        Shape::Whitespace,
        &[("core.whitespace", "tab-in-indent")],
        &["diff", "--color=always", "--ws-error-highlight=all", "HEAD~1", "HEAD"],
    );
}
