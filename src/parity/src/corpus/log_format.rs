//! Differential corpus cases for the *formatting* layer: the bytes `log`,
//! `show`, `diff` and `blame` hand to a human or to a script.
//!
//! Scope: the pretty-format atom table (`pretty.c`), the built-in `--pretty=`
//! names and the `format:`/`tformat:` split, the `--date=` renderers
//! (`date.c`), ref decoration (`log-tree.c`), and the *presentation* half of
//! the diff machinery (`diff.c`) — stat family, hunk shaping, word diff, moved
//! blocks, prefixes, colour, and the combined-diff selectors. `blame`'s output
//! formats (`builtin/blame.c`) ride along because they are the same question
//! asked of a different emitter: which fields, in which columns, at which
//! abbreviation.
//!
//! # Territory
//!
//! * `diff_family.rs` owns the *plumbing* diff commands (`diff-files`,
//!   `diff-index`, `diff-tree`, `diff-pairs`), `range-diff`, `whatchanged`,
//!   `shortlog`, `show-branch` and the `pickaxe` label. Nothing here is
//!   labelled `pickaxe`, and the plumbing verbs appear only where a porcelain
//!   flag has no porcelain spelling (`diff-tree -c --combined-all-paths`).
//! * `history_query.rs` owns traversal *selection* — which commits come back,
//!   in what order. This module never asks that question; every case here is
//!   about how the commits it already has are rendered.
//! * The `--graph` renderer cases in `corpus.rs` are left alone: they pin
//!   `graph.c`'s rows, and none of the formatting axes below change a row.
//! * `shape_reach.rs` pins rename/whitespace *detection* (`-M`/`-C`/`-B`/`-w`
//!   deciding what a hunk is). This module takes detection as given and pins
//!   how the result is printed — `--compact-summary`'s `(new)` column,
//!   `--stat`'s `{orig => moved}` brace form, `-D`'s `@@ -?,? @@` header.
//! * `info_attrs.rs` pins `blame` on `Branched`, `Merged`, `Conflicted` and
//!   `AwkwardPaths`. The blame block here is on `Whitespace` and `Renamed`,
//!   which it does not reach.
//!
//! # What is excluded for nondeterminism, and why
//!
//! `env::harden` pins identity, `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` to
//! `1700000000 +0000`, `TZ=UTC` and `LC_ALL=C`. Three things still move and are
//! therefore absent from this file:
//!
//! * **`%ar`, `%cr`, `%ah`-adjacent relative rendering, and `--date=relative`.**
//!   `show_date_relative()` subtracts from `time(NULL)`; stock prints
//!   `2 years, 9 months ago` today and something else next month. No case here
//!   uses them.
//! * **`blame` fields for lines that are not committed.** A worktree line is
//!   attributed to the all-zero id with the *wall clock* as its author time
//!   (`blame --porcelain ws/indent.c` emits `author-time 1787581740`, a value
//!   that is simply "now"). `Shape::Whitespace` carries an unstaged edit to
//!   `ws/indent.c`, so every case below that blames that path either names a
//!   revision (`blame HEAD -- ws/indent.c`, which blames the commit's content)
//!   or uses `-s` (which prints no date at all). `ws/eol.txt` is clean and is
//!   blamed freely.
//! * **`--date=local` is *not* excluded**: `TZ=UTC` is pinned, so it renders
//!   identically to `default` minus the zone suffix. `--date=human` is not
//!   excluded either: `show_date_human()` drops the year only for dates inside
//!   the current year and the time only for dates within the last few days, and
//!   the pinned stamp is already years in the past, so it has settled on its
//!   `Nov 14 2023` branch and cannot move again.
//!
//! # Fixture constraints
//!
//! * **No commit in any shape carries a trailer or a note**, and none is
//!   signed. `%(trailers)` therefore has only its empty answer and is absent;
//!   `%N`, `%GG`, `%G?` and `%GS` are kept in one bracketed case because their
//!   empty answer still has a shape — a port fails it by printing a stray
//!   newline or the literal atom, not by printing the wrong value.
//! * **No shape has an intent-to-add index entry**, so `--ita-visible-in-index`
//!   and `--ita-invisible-in-index` cannot be distinguished; a case is one argv
//!   against a pristine copy and cannot run `add -N` first. Absent here.
//! * **`Shape::Attributes` is the only shape with more than one author
//!   identity** and the only one with a `.mailmap`. It is therefore the only
//!   place `%aN`/`%aE` differ from `%an`/`%ae`, `--use-mailmap` changes a
//!   built-in format, and `--author=` has two possible answers.
//! * **`Shape::Branched` is the only shape with tags**, so `%(describe)`,
//!   `tag:` decorations and `--decorate-refs=refs/tags/*` live there.
//! * **`Shape::Renamed`'s pure rename is the corpus's only moved block.**
//!   `HEAD~4..HEAD~3` under `--no-renames` is 40 deleted lines and the same 40
//!   added under a different name, which is exactly what `--color-moved` was
//!   built to colour.
//! * **Colour is reachable only through `--color=always` / `color.ui=always`.**
//!   `env::harden` sets `NO_COLOR=1` and `TERM=dumb`; both are overridden by an
//!   explicit `always`, and verified so — `log --color=always --oneline`
//!   emits `\e[33m` under the hardened environment.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    pretty_atoms(out);
    pretty_named(out);
    padding(out);
    dates(out);
    decorations(out);
    stat_family(out);
    hunk_shape(out);
    word_diff(out);
    colour(out);
    naming(out);
    merge_diffs(out);
    blame_format(out);
    message_search(out);
    errors(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

/// The blob of `orig/alpha.txt` in [`Shape::Renamed`], which the pure rename
/// carries unchanged to `moved/alpha.txt`. Written out rather than derived
/// because a case is a literal argv; the id is a function of the fixture's
/// pinned content and identity, the same way `history_query.rs` pins
/// `README_BLOB`.
const ALPHA_BLOB: &str = "7843c7f2b37d7476ff34ace934dca1bf1b430170";

/// The pretty-format atom table, walked deliberately.
///
/// What a port gets wrong without these: `pretty.c` has roughly sixty
/// placeholders and a port typically implements the dozen a smoke test uses.
/// The ones that fall out are the *pairs that differ only in case* — `%an` is
/// the raw author name and `%aN` the mailmap-resolved one, `%ae`/`%aE`
/// likewise, `%al`/`%aL` the local part — and the ones whose value is empty in
/// a simple repository (`%b`, `%N`, `%e`, `%GG`), where the failure is printing
/// the literal atom or a stray newline rather than nothing.
///
/// `%an` under `--use-mailmap` is pinned on its own: the flag rewrites the
/// identity a *built-in* format prints but leaves `%an` raw, so a port that
/// wires `--use-mailmap` straight into the `%an` lookup diverges on exactly one
/// of the two cases.
fn pretty_atoms(out: &mut Vec<Case>) {
    // Object ids. `Merged` has a two-parent row and `Octopus` a four-parent
    // one, so `%P`/`%p` print a list rather than a single id.
    each(
        Shape::Merged,
        "log",
        &[
            &["log", "--format=%H|%h|%T|%t|%P|%p"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "log",
        &[&["log", "-1", "--format=%h|%p"]],
        out,
    );

    // Identity atoms, and the mailmap pair. `Attributes` is the only shape
    // where the two halves of each pair differ.
    each(
        Shape::Branched,
        "log",
        &[&["log", "-1", "--format=%an|%aN|%ae|%aE|%al|%aL"]],
        out,
    );
    each(
        Shape::Attributes,
        "log",
        &[
            &["log", "--format=%an|%aN|%ae|%aE|%al|%aL"],
            // The flag moves a built-in format's identity but not `%an`'s.
            &["log", "--use-mailmap", "--format=%an <%ae>"],
            &["log", "-1", "--use-mailmap", "--pretty=full", "HEAD~1"],
        ],
        out,
    );
    out.push(
        Case::new("log", &["log", "--format=%aN <%aE>"], Shape::Attributes)
            .with_config(&[("log.mailmap", "false")]),
    );
    out.push(
        Case::new("log", &["log", "--format=%aN"], Shape::Attributes)
            .with_config(&[("mailmap.file", ".mailmap")]),
    );

    // Date atoms in their own right, as opposed to the `--date=` renderer the
    // `dates` block covers: these are the placeholders that ignore `--date=`.
    each(
        Shape::Branched,
        "log",
        &[
            &["log", "-1", "--format=%ad|%aD|%at|%ai|%aI|%as|%ah"],
        ],
        out,
    );

    // Message atoms, the empty ones included, plus the escapes.
    each(
        Shape::Branched,
        "log",
        &[
            &["log", "-1", "--format=[%e][%s][%f][%b][%N][%GG][%G?][%GS]"],
            &["log", "-1", "--format=a%nb%%c%x41d%x7ce"],
            // Unknown atoms are not an error: git copies them through verbatim.
            &["log", "-1", "--format=%zz|%(nope)|%"],
        ],
        out,
    );
    // `%f` sanitises the subject into a filename, which only bites on a subject
    // that needs sanitising.
    each(Shape::AwkwardPaths, "log", &[&["log", "-1", "--format=%s|%f"]], out);

    // Decoration and describe atoms. `Branched` is the only shape with tags, so
    // it is the only one where `%(describe)` answers anything and `%d`/`%D`
    // carry a `tag:` entry.
    each(
        Shape::Branched,
        "log",
        &[
            &["log", "--format=[%d][%D]", "--all"],
            &["log", "-1", "--format=%(decorate:prefix=[,suffix=],separator=; ,tag=T:)", "--all"],
            &["log", "-1", "--format=%(describe)|%(describe:tags)|%(describe:abbrev=12)|%(describe:match=v0.1*)", "feature"],
        ],
        out,
    );

    // `%S` needs `--source`; `%m` needs a symmetric range. Both are atoms whose
    // value comes from the *traversal*, not from the commit.
    each(
        Shape::Branched,
        "log",
        &[
            &["log", "--source", "--format=%S %h", "--all"],
            &["log", "--left-right", "--format=%m %h %s", "main...feature"],
        ],
        out,
    );

    // Reflog atoms. The reflog is written at fixture build time under the
    // pinned committer date, so `%gd` under `--date=` is stable too.
    each(
        Shape::Branched,
        "log",
        &[&["log", "-g", "-3", "--format=%gD|%gd|%gn|%ge|%gs"]],
        out,
    );
}

/// The built-in `--pretty=` names, and the one thing that separates `format:`
/// from `tformat:`.
///
/// What a port gets wrong without these: `format:` is a *separator* and
/// `tformat:` a *terminator*, so two commits under `format:%h` end without a
/// trailing newline and under `tformat:%h` with one. `--format=` is `tformat:`,
/// which is why the difference is invisible until someone writes `--pretty=`
/// explicitly. The named formats are each a distinct emitter in `pretty.c`:
/// `email` prints an mbox `From ` line and a `Subject: [PATCH]`,
/// `reference` prints `%h (%s, %as)`, `raw` prints the object's own header
/// rather than a rendered one.
fn pretty_named(out: &mut Vec<Case>) {
    for name in ["oneline", "short", "full", "fuller", "reference", "email", "raw"] {
        let arg = format!("--pretty={name}");
        out.push(Case::new("log", &["log", "-1", &arg], Shape::Branched));
    }
    each(
        Shape::Branched,
        "log",
        &[
            &["log", "-2", "--pretty=format:%h"],
            &["log", "-2", "--pretty=tformat:%h"],
        ],
        out,
    );
    // An annotated tag renders a tag header before the commit; `show` is the
    // only verb that reaches it.
    each(
        Shape::Branched,
        "show",
        &[
            &["show", "--no-patch", "--pretty=raw", "v0.2.0"],
            &["show", "--no-patch", "--pretty=reference", "v0.1.0"],
        ],
        out,
    );
    out.push(
        Case::new("log", &["log", "-1"], Shape::Branched).with_config(&[("format.pretty", "%h %s")]),
    );
}

/// Column padding and wrapping.
///
/// What a port gets wrong without these: `%<`, `%>`, `%><` and `%>>` are not
/// four spellings of one operation — left-pad, right-pad, centre and
/// right-pad-with-steal each have their own truncation rule, and `%<|(N)`
/// measures from the *start of the line* rather than from the placeholder. A
/// port that treats them as `format!("{:width$}")` passes `%<(20)` and fails
/// every other one, and `%>|(N)` measures from the *start of the line* rather
/// than from the placeholder. `misc_commands.rs` already pins `%<(20)`,
/// `%<(10,mtrunc)`, `%<|(20)`, `%>>(20)` and `%w(20,2,4)`; these are the modes
/// it does not reach.
fn padding(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "log",
        &[
            &["log", "-1", "--format=%<(30,trunc)%s|"],
            &["log", "-1", "--format=%<(30,ltrunc)%s|"],
            &["log", "-1", "--format=%><(24)%s|"],
            &["log", "-1", "--format=%>|(40)%s|"],
            &["log", "-1", "--format=%<(12)%h%>(20)%s|"],
        ],
        out,
    );
    // A long subject, so wrapping has a second line to produce.
    each(
        Shape::Attributes,
        "log",
        &[
            &["log", "--format=%w(24,0,4)%s %s %s"],
            &["log", "--format=%<(18,mtrunc)%s|%>(12)%an|"],
        ],
        out,
    );
}

/// The `--date=` renderers.
///
/// What a port gets wrong without these: every mode is a separate function in
/// `date.c` with its own field order, zone suffix and punctuation — `iso` uses
/// a space and `iso-strict` a `T` with `Z` for UTC, `rfc` puts the weekday
/// first with a comma, `raw` prints the epoch *and* the zone while `unix`
/// prints the epoch alone, `local` drops the zone. `format:` hands the string
/// to `strftime` and `format-local:` does the same after converting to the
/// local zone, so the two differ only under a non-UTC `TZ` — and `TZ` is pinned
/// to UTC here, which makes agreement between them part of the contract rather
/// than an accident.
///
/// `relative` is deliberately absent: see the module doc.
fn dates(out: &mut Vec<Case>) {
    for mode in ["local", "iso", "iso-strict", "rfc", "short", "raw", "human", "unix"] {
        let arg = format!("--date={mode}");
        out.push(Case::new("log", &["log", "-1", &arg, "--format=%ad|%cd"], Shape::Branched));
    }
    each(
        Shape::Branched,
        "log",
        &[
            &["log", "-1", "--date=format:%Y-%m-%dT%H:%M:%S", "--format=%ad"],
            &["log", "-1", "--date=format-local:%Y-%m-%dT%H:%M:%S %Z", "--format=%ad"],
            &["log", "-g", "-3", "--date=iso", "--format=%gd %gs"],
        ],
        out,
    );
    out.push(
        Case::new("log", &["log", "-1"], Shape::Branched)
            .with_config(&[("log.date", "format:%Y/%m/%d")]),
    );
    // `blame` has its own date default and its own config key.
    out.push(Case::new("blame", &["blame", "--date=iso-strict", "ws/eol.txt"], Shape::Whitespace));
}

/// Ref decoration.
///
/// What a port gets wrong without these: `--decorate=short` strips the
/// `refs/heads/` and `refs/tags/` prefixes while `full` keeps them, `auto`
/// decorates only when the output is a terminal — which under the hardened
/// environment means *not at all*, so `auto` and `no` must agree and neither
/// may agree with `short`. `--decorate-refs` and
/// `--decorate-refs-exclude` filter the eligible ref set independently and
/// compose, and `HEAD -> main` is a single decoration rather than two.
fn decorations(out: &mut Vec<Case>) {
    for mode in ["short", "full", "no", "auto"] {
        let arg = format!("--decorate={mode}");
        out.push(Case::new("log", &["log", &arg, "--oneline", "--all"], Shape::Branched));
    }
    each(
        Shape::Branched,
        "log",
        &[
            &["log", "--decorate=full", "--decorate-refs=refs/tags/*", "--oneline", "--all"],
            &["log", "--decorate", "--decorate-refs-exclude=refs/tags/*", "--oneline", "--all"],
            &[
                "log",
                "--decorate",
                "--decorate-refs=refs/heads/*",
                "--decorate-refs-exclude=refs/heads/feature",
                "--oneline",
                "--all",
            ],
        ],
        out,
    );
    out.push(
        Case::new("log", &["log", "--oneline", "--all"], Shape::Branched)
            .with_config(&[("log.decorate", "auto")]),
    );
    // Remote-tracking refs decorate differently from local ones, and only one
    // shape has any.
    each(
        Shape::BehindRemote,
        "log",
        &[&["log", "--decorate", "--oneline", "--all"]],
        out,
    );
}

/// The stat family: `--stat`, `--numstat`, `--shortstat`, `--dirstat`,
/// `--summary`, `--compact-summary` and the width controls.
///
/// What a port gets wrong without these: `--stat`'s layout is three widths that
/// are *computed*, not fixed — a name column bounded by the terminal width and
/// truncated with a leading `...`, a count column, and a graph column that is
/// scaled so the largest file fills it. `--stat=<width>,<name-width>`,
/// `--stat-name-width` and `--stat-graph-width` each override one of the three,
/// and `diff.statGraphWidth` overrides the same one from config. A rename
/// collapses to the `{orig => moved}/alpha.txt` brace form in the name column,
/// which is a different string from either path. `--dirstat`'s three parameter
/// families (`files`/`lines`/`changes`, `cumulative`, a bare percentage cut-off)
/// produce three different sets of percentages over the same diff.
fn stat_family(out: &mut Vec<Case>) {
    each(
        Shape::Renamed,
        "diff",
        &[
            &["diff", "--stat=60,30", "HEAD~5", "HEAD"],
            &["diff", "--stat", "--stat-name-width=12", "HEAD~5", "HEAD"],
            &["diff", "--stat", "--stat-graph-width=8", "HEAD~5", "HEAD"],
            &["diff", "--dirstat=files,0", "HEAD~5", "HEAD"],
            &["diff", "--dirstat=cumulative,0", "HEAD~5", "HEAD"],
            // The brace form, which only a detected rename produces.
            &["diff", "--stat", "-M", "HEAD~3", "HEAD~2"],
            &["diff", "--compact-summary", "-M", "-C", "-B", "HEAD~5", "HEAD"],
            &["diff", "--summary", "-C", "HEAD~2", "HEAD~1"],
            &["diff", "--patch-with-stat", "-M", "HEAD~4", "HEAD~3"],
        ],
        out,
    );
    out.push(
        Case::new("diff", &["diff", "--stat", "HEAD~5", "HEAD"], Shape::Renamed)
            .with_config(&[("diff.statGraphWidth", "10")]),
    );
    out.push(
        Case::new("diff", &["diff", "--dirstat", "HEAD~5", "HEAD"], Shape::Renamed)
            .with_config(&[("diff.dirstat", "lines,0")]),
    );
    // A stat whose name column holds quoted paths: the width is computed from
    // the *quoted* form, so a port that measures the raw bytes mis-pads.
    each(
        Shape::AwkwardPaths,
        "diff",
        &[
            &["diff", "--stat", "HEAD~1", "HEAD"],
            &["diff", "--numstat", "-z", "HEAD~1", "HEAD"],
        ],
        out,
    );
    // A binary path reports `Bin <a> -> <b> bytes` in the stat and `-\t-` in
    // numstat rather than line counts.
    each(
        Shape::Attributes,
        "diff",
        &[
            &["diff", "--stat", "HEAD~4", "HEAD~3"],
        ],
        out,
    );
}

/// Hunk shaping: context width, hunk merging, function context, and `-L`.
///
/// What a port gets wrong without these: `--inter-hunk-context=<n>` merges two
/// hunks whose gap is at most `n` lines into one, which changes the `@@` header
/// *and* the line counts, not just the spacing. `--function-context` extends
/// each hunk backwards to the enclosing function line and forwards to the end
/// of it, using the same `xdiff` funcname matcher that produces the `@@ … @@`
/// suffix. `-L<start>,<end>:<file>` is a different traversal entirely — it
/// rewrites the pathspec into a line range and follows that range backwards
/// through history, printing a diff per commit that touched it.
fn hunk_shape(out: &mut Vec<Case>) {
    each(
        Shape::Whitespace,
        "diff",
        &[
            &["diff", "-U0", "HEAD~1", "HEAD"],
            &["diff", "-U6", "HEAD~1", "HEAD"],
            &["diff", "--function-context", "HEAD~1", "HEAD"],
        ],
        out,
    );
    out.push(
        Case::new("diff", &["diff", "HEAD~1", "HEAD"], Shape::Whitespace)
            .with_config(&[("diff.context", "6")]),
    );
    // Two edits four lines apart: `-U0` keeps them separate and
    // `--inter-hunk-context=3` merges them.
    each(
        Shape::Renamed,
        "diff",
        &[
            &["diff", "-U0", "HEAD~3", "HEAD~2"],
            &["diff", "-U0", "--inter-hunk-context=3", "HEAD~3", "HEAD~2"],
        ],
        out,
    );
    out.push(
        Case::new("diff", &["diff", "-U0", "HEAD~3", "HEAD~2"], Shape::Renamed)
            .with_config(&[("diff.interHunkContext", "4")]),
    );
    each(
        Shape::Whitespace,
        "log",
        &[
            &["log", "-L", "3,5:ws/indent.c", "--oneline"],
            &["log", "-L", "3,+2:ws/indent.c", "--oneline"],
            &["log", "-L", ":main:ws/indent.c", "--oneline"],
        ],
        out,
    );
}

/// Word diff.
///
/// What a port gets wrong without these: `plain` brackets removals `[-…-]` and
/// additions `{+…+}` inline, `porcelain` emits a machine-readable stream with
/// `~` line terminators and one `-`/`+` line per changed word run, and `color`
/// (pinned in the `colour` block) emits no brackets at all and relies purely on
/// escape sequences. The word boundary itself is
/// `--word-diff-regex`, which reshapes what counts as one word — and with a
/// regex that admits whitespace as a word, an indentation change becomes a
/// diff where the default regex hid it.
fn word_diff(out: &mut Vec<Case>) {
    each(
        Shape::Whitespace,
        "diff",
        &[
            &["diff", "--word-diff=plain", "HEAD~1", "HEAD"],
            &["diff", "--word-diff=porcelain", "HEAD~1", "HEAD"],
            &["diff", "--word-diff=plain", "--word-diff-regex=[a-z]+|.", "HEAD~1", "HEAD"],
        ],
        out,
    );
    out.push(
        Case::new("diff", &["diff", "--word-diff", "HEAD~1", "HEAD"], Shape::Whitespace)
            .with_config(&[("diff.wordRegex", "[a-z]+|[0-9]+|.")]),
    );
    each(
        Shape::Renamed,
        "diff",
        &[&["diff", "--word-diff=plain", "-M", "HEAD~3", "HEAD~2"]],
        out,
    );
}

/// Colour, which `env::harden` otherwise switches off.
///
/// What a port gets wrong without these: the colour slots are looked up by
/// name (`color.diff.meta`, `color.decorate.tag`, …) and each has a built-in
/// default; a port that hard-codes the defaults passes every case that does not
/// set one. `%C(auto)` resolves to the slot the surrounding context implies and
/// emits *nothing* when colour is off, which is a different code path from
/// `%C(red)`. `--no-color` after `--color=always` must win, because the last
/// occurrence does.
///
/// Moved-block detection is here rather than in the whitespace block because it
/// is a *colouring* decision: the diff text is identical across every
/// `--color-moved` mode and only the escape sequences differ. `zebra`, `blocks`
/// and `plain` all paint this fixture's single moved block the same colour, so
/// the pair that separates a real implementation from a stub is `zebra` against
/// `dimmed-zebra`, which dims it.
fn colour(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "log",
        &[
            &["log", "--color=always", "--decorate", "--oneline", "-1"],
            &["log", "--color=always", "-1", "--format=%C(auto)%h%Creset %C(red)%s%Creset %C(bold blue)%an%C(reset)"],
        ],
        out,
    );
    out.push(
        Case::new("log", &["log", "--color=always", "--decorate", "--oneline", "-1"], Shape::Branched)
            .with_config(&[
                ("color.decorate.branch", "green"),
                ("color.decorate.tag", "magenta"),
                ("color.decorate.HEAD", "blue bold"),
            ]),
    );
    out.push(
        Case::new("log", &["log", "--oneline", "-1"], Shape::Branched)
            .with_config(&[("color.ui", "always")]),
    );
    out.push(
        Case::new("diff", &["diff", "--color=always", "HEAD~1", "HEAD"], Shape::Whitespace)
            .with_config(&[
                ("color.diff.meta", "magenta"),
                ("color.diff.frag", "yellow bold"),
                ("color.diff.new", "blue"),
            ]),
    );
    each(
        Shape::Whitespace,
        "diff",
        &[
            &["diff", "--color=always", "--no-color", "HEAD~1", "HEAD"],
            &["diff", "--color=always", "--word-diff=color", "HEAD~1", "HEAD"],
        ],
        out,
    );
    for mode in ["zebra", "dimmed-zebra"] {
        let arg = format!("--color-moved={mode}");
        out.push(Case::new(
            "diff",
            &["diff", "--color=always", "--no-renames", &arg, "HEAD~4", "HEAD~3"],
            Shape::Renamed,
        ));
    }
    each(
        Shape::Renamed,
        "diff",
        &[&[
            "diff",
            "--color=always",
            "--no-renames",
            "--color-moved=zebra",
            "--color-moved-ws=allow-indentation-change",
            "HEAD~4",
            "HEAD~3",
        ]],
        out,
    );
}

/// How a path and an object id are *named* in the output: prefixes,
/// abbreviation, relativity, binary rendering and textconv.
///
/// What a port gets wrong without these: the `a/` and `b/` prefixes appear in
/// four places per file pair (`diff --git`, `---`, `+++`, and the rename
/// headers) and `--no-prefix`, `--src-prefix`, `--dst-prefix` and
/// `--default-prefix` each rewrite a different subset. `--relative` strips the
/// current directory's prefix from every path *and* drops the pairs outside it,
/// which is two behaviours a port tends to implement as one. `--binary` emits a
/// base-85 `GIT binary patch` literal where the default emits one `Binary files
/// … differ` line, and `--text` forces the file through the text path instead.
/// `--textconv` replaces both sides with a filter's output before diffing, so a
/// filter that collapses the file makes the diff vanish — that vanishing is the
/// measurement.
fn naming(out: &mut Vec<Case>) {
    each(
        Shape::Renamed,
        "diff",
        &[
            &["diff", "--no-prefix", "-M", "HEAD~3", "HEAD~2"],
            &["diff", "--src-prefix=OLD/", "--dst-prefix=NEW/", "-M", "HEAD~3", "HEAD~2"],
            &["diff", "--abbrev=12", "-M", "HEAD~3", "HEAD~2"],
            // `-B` plus `-D`: the rewrite's pre-image is suppressed and the hunk
            // header becomes `@@ -?,? +1,40 @@`.
            &["diff", "-B", "-D", "HEAD~1", "HEAD"],
        ],
        out,
    );
    // Two selectors whose *output shape* is the measurement: `--find-object`
    // narrows the walk to the commits that add or remove one blob, and
    // `--pickaxe-all` widens the printed diff from the matching paths back to
    // the whole commit. `diff_family.rs` files its pickaxe cases under the
    // `pickaxe` label on `Branched`; these are on the shape where a blob
    // survives a rename.
    each(
        Shape::Renamed,
        "log",
        &[
            &["log", "--oneline", "--find-object", ALPHA_BLOB],
            &["log", "--raw", "--pickaxe-all", "-Sedited", "--oneline"],
        ],
        out,
    );
    out.push(
        Case::new("diff", &["diff", "--default-prefix", "-M", "HEAD~3", "HEAD~2"], Shape::Renamed)
            .with_config(&[("diff.noprefix", "true")]),
    );
    // `--relative` resolves against the working directory, so the case has to
    // run from one.
    out.push(
        Case::new("diff", &["diff", "--relative", "--name-only", "HEAD~3", "HEAD~2"], Shape::Renamed)
            .in_dir("moved"),
    );
    each(
        Shape::Attributes,
        "diff",
        &[
            &["diff", "--binary", "HEAD~4", "HEAD~3", "--", "assets/logo.bin"],
            &["diff", "--text", "HEAD~4", "HEAD~3", "--", "assets/logo.bin"],
            // `logs/keep.log` carries `-diff`, so it renders as binary despite
            // being text.
            &["diff", "HEAD~4", "HEAD~3", "--", "logs/keep.log"],
        ],
        out,
    );
    // `*.md` carries `diff=markdown`; the driver's textconv is supplied here so
    // the filter is part of the case rather than of the fixture.
    out.push(
        Case::new("diff", &["diff", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes)
            .with_config(&[("diff.markdown.textconv", "head -1")]),
    );
    out.push(
        Case::new(
            "diff",
            &["diff", "--no-textconv", "HEAD~2", "HEAD~1", "--", "docs/manual.md"],
            Shape::Attributes,
        )
        .with_config(&[("diff.markdown.textconv", "head -1")]),
    );
    // `-O<file>` and `diff.orderFile` need a file in the worktree to read, and
    // `Shape::Attributes` is the only shape that ships one whose patterns match
    // tracked paths (`*.log` pulls `logs/keep.log` to the front).
    out.push(Case::new(
        "diff",
        &["diff", "-O.gitignore", "--name-only", "HEAD~4", "HEAD~3"],
        Shape::Attributes,
    ));
    out.push(
        Case::new("diff", &["diff", "--stat", "HEAD~4", "HEAD~3"], Shape::Attributes)
            .with_config(&[("diff.orderFile", ".gitignore")]),
    );
}

/// Diffs of merges.
///
/// What a port gets wrong without these: a merge has *no* diff by default, one
/// diff per parent under `-m`/`--diff-merges=separate` with a `(from <id>)`
/// suffix on each record, a condensed multi-column diff under
/// `-c`/`--diff-merges=dense-combined`, and the first-parent diff alone under
/// `--diff-merges=first-parent`. On a clean
/// merge whose parents touch disjoint paths the combined forms print *nothing*
/// but still print the commit header — a port that emits the first-parent diff
/// there is wrong in a way only a case can catch. `Octopus` is where `-m`
/// produces four blocks and each `(from …)` names a different parent.
fn merge_diffs(out: &mut Vec<Case>) {
    for mode in ["off", "first-parent", "separate", "dense-combined", "remerge"] {
        let arg = format!("--diff-merges={mode}");
        out.push(Case::new("show", &["show", "--oneline", &arg], Shape::Merged));
    }
    each(
        Shape::Merged,
        "show",
        &[
            &["show", "-m", "--oneline", "--name-status"],
            &["show", "-c", "--combined-all-paths", "--raw"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "show",
        &[
            &["show", "-m", "--raw", "--oneline"],
            &["show", "--diff-merges=first-parent", "--stat", "--oneline"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "diff-tree",
        &[&["diff-tree", "-c", "--combined-all-paths", "-r", "HEAD"]],
        out,
    );
    out.push(
        Case::new("log", &["log", "-1", "--oneline", "--stat"], Shape::Merged)
            .with_config(&[("log.diffMerges", "first-parent")]),
    );
}

/// `blame`'s output formats, on the two shapes `info_attrs.rs` does not reach.
///
/// What a port gets wrong without these: `--porcelain` emits a header block per
/// *commit* and a bare id line for subsequent lines of the same commit, while
/// `--line-porcelain` repeats the whole block on every line; `--incremental`
/// emits the same blocks in completion order with no line content at all. The
/// short flags each remove or add one column (`-s` drops author and date, `-e`
/// swaps the name for the address, `-f` adds the original filename, `-n` the
/// original line number, `-l` the full id), and they compose, so the column
/// order is a contract of its own.
///
/// Every case that touches `ws/indent.c` names a revision or uses `-s`: the
/// worktree copy is unstaged-dirty and its uncommitted lines are stamped with
/// the wall clock. See the module doc.
fn blame_format(out: &mut Vec<Case>) {
    each(
        Shape::Whitespace,
        "blame",
        &[
            &["blame", "--porcelain", "HEAD", "--", "ws/indent.c"],
            &["blame", "--line-porcelain", "-L1,2", "HEAD", "--", "ws/indent.c"],
            &["blame", "--incremental", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "-w", "-M", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "--ignore-rev", "HEAD~1", "HEAD", "--", "ws/indent.c"],
            &["blame", "--abbrev=16", "ws/eol.txt"],
            &["blame", "-e", "-n", "-l", "-L1,2", "ws/eol.txt"],
            // A reversed range: git clamps rather than refusing.
            &["blame", "-L", "5,2", "ws/eol.txt"],
        ],
        out,
    );
    each(
        Shape::Renamed,
        "blame",
        &[
            &["blame", "--porcelain", "-L1,3", "moved/beta.txt"],
            &["blame", "--line-porcelain", "-L3,3", "moved/beta.txt"],
            &["blame", "-f", "-n", "-L1,2", "moved/alpha.txt"],
            &["blame", "-C", "-C", "-C", "--porcelain", "-L1,2", "copies/gamma.txt"],
            &["blame", "--abbrev=12", "-L1,1", "moved/alpha.txt"],
        ],
        out,
    );
}

/// Message and identity search.
///
/// What a port gets wrong without these: multiple `--grep=` are OR'd, and
/// `--all-match` turns them into an AND; `--invert-grep` negates the whole set
/// rather than each pattern; `-E`, `-F` and `--perl-regexp` select three
/// different matchers. `--author=` is matched against the
/// *mailmap-resolved* identity, because `log.mailmap` defaults to true — so on
/// a shape with a mailmap, `--author=Alias` finds nothing and
/// `--author=Proper` finds the commit Alias authored.
fn message_search(out: &mut Vec<Case>) {
    each(
        Shape::Renamed,
        "log",
        &[
            &["log", "--oneline", "--grep=rename", "--grep=copy"],
            &["log", "--oneline", "--all-match", "--grep=rename", "--grep=edit"],
            &["log", "--oneline", "--invert-grep", "--grep=renames:"],
            &["log", "--oneline", "-F", "--grep=renames:"],
            &["log", "--oneline", "-E", "--grep=ren(ame|ames) with"],
            &["log", "--oneline", "--perl-regexp", "--grep=rename\\s+with"],
        ],
        out,
    );
    each(
        Shape::Attributes,
        "log",
        &[
            &["log", "--oneline", "--author=Proper"],
            &["log", "--oneline", "--author=Alias"],
            &["log", "--oneline", "--no-use-mailmap", "--author=Alias"],
            &["log", "--oneline", "--all-match", "--committer=zvcs", "--grep=alias"],
        ],
        out,
    );
}

/// Error paths: what each renderer does with an argument it cannot use.
///
/// Every case here compares stderr byte for byte, because the diagnostic *is*
/// the behaviour. Two of them are not errors and are marked as such: an unknown
/// pretty *atom* is copied through verbatim (`%zz` above), and a malformed
/// padding argument is likewise literal — only an unknown *colour name* inside
/// `%C(...)` aborts the whole format.
fn errors(out: &mut Vec<Case>) {
    for (cmd, args, shape) in [
        ("log", &["log", "-1", "--pretty=nope"][..], Shape::Branched),
        ("log", &["log", "-1", "--date=bogus"], Shape::Branched),
        ("log", &["log", "--decorate=bogus", "--oneline", "-1"], Shape::Branched),
        (
            "log",
            &["log", "-1", "--color=always", "--format=%C(nosuchcolor)%h"],
            Shape::Branched,
        ),
        ("log", &["log", "-E", "--grep=[unclosed"], Shape::Branched),
        ("log", &["log", "-L", "99,100:ws/indent.c"], Shape::Whitespace),
        ("log", &["log", "-L", "nonsense"], Shape::Whitespace),
        ("log", &["log", "-L", ":nosuchfn:ws/indent.c"], Shape::Whitespace),
        ("diff", &["diff", "--color-moved=bogus", "HEAD~1", "HEAD"], Shape::Whitespace),
        ("diff", &["diff", "--color-moved-ws=bogus", "HEAD~1", "HEAD"], Shape::Whitespace),
        ("diff", &["diff", "--word-diff=bogus", "HEAD~1", "HEAD"], Shape::Whitespace),
        ("diff", &["diff", "--dirstat=bogus", "HEAD~1", "HEAD"], Shape::Whitespace),
        ("diff", &["diff", "--stat=abc", "HEAD~1", "HEAD"], Shape::Whitespace),
        ("diff", &["diff", "-Onosuchfile", "--name-only", "HEAD~1", "HEAD"], Shape::Whitespace),
        ("show", &["show", "--diff-merges=bogus"], Shape::Merged),
    ] {
        out.push(Case::strict(cmd, args, shape));
    }
    // Not errors: a malformed padding or wrapping argument is copied through as
    // literal text, which is the opposite of what a parser-first port does.
    each(
        Shape::Branched,
        "log",
        &[&["log", "-1", "--format=%<(x)%s"]],
        out,
    );
}
