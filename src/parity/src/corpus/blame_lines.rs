//! Line-level history: every question git answers about a **line** rather than
//! about a file or a commit.
//!
//! One engine — `blame.c`'s scoreboard — is reached through four front ends in
//! this tree (`blame`, `annotate`, `pickaxe`, and `log -L`'s line-log walk),
//! and the questions it answers are all of the form *which commit last touched
//! this line, and where did the line come from before that*. The corpus asked
//! that question in eleven places and nowhere owned it, so the parts that only
//! a fixture with real per-line history can reach were unmeasured: the `-L`
//! grammar past `n,m`, the `-C` copy score **threshold**, and the funcname
//! machinery's dependence on the `diff=<driver>` attribute.
//!
//! # How this divides territory with the eleven modules that already blame
//!
//! Every one of them was read before a case was written here; what each owns:
//!
//! * **`info_attrs.rs`** — the largest holder, and the one that reaches the
//!   flag *surface*: ~55 `blame` cases and the whole `annotate`/`pickaxe`
//!   front-end sweep, almost all of them on `Shape::Branched`'s two-line
//!   `src/lib.rs`. It covers `-s`/`-e`/`-w`/`-t`/`-n`/`-l`/`-c`/`-f`/`-b`/
//!   `--root`, the `--date=` modes, `--abbrev=`/`--no-abbrev`, `-M`/`-M3`/`-C`/
//!   `-C5`/`-CC`/`-CCC`, `--ignore-rev HEAD`, `--ignore-revs-file` at
//!   `/dev/null` and at a missing path, the six `blame.*` settings,
//!   `--color-lines`/`--color-by-age`, `--porcelain`/`--line-porcelain`/
//!   `--incremental`, `--first-parent`, `--reverse HEAD~1..HEAD`,
//!   `--contents README.md`, `--show-stats`, `--score-debug` and the error
//!   paths. Its `-L` set is `2,2`, `1,+1`, `/two/,+1`, `9,9`, `1,9`, `0,0` —
//!   six forms out of the grammar's dozen, all on a **two-line** file where
//!   nearly every range is the whole file and no two ranges can be
//!   distinguished. That is the gap this module is mostly filling.
//! * **`log_format.rs`** — `log -L 3,5:`, `-L 3,+2:` and `-L :main:` on
//!   `Shape::Whitespace`, three `log -L` error cases (`99,100:`, `nonsense`,
//!   `:nosuchfn:`), a `blame` group on `Whitespace` (porcelain/line-porcelain/
//!   incremental/`-w -M`/`--ignore-rev HEAD~1`/`--abbrev=16`/`-L 5,2`) and a
//!   `blame` group on `Renamed` (porcelain `-L1,3` and line-porcelain `-L3,3`
//!   of `moved/beta.txt`, `-C -C -C --porcelain -L1,2` of `copies/gamma.txt`).
//!   Those last three are the corpus's only cases that cross a rename or a copy
//!   *inside* the porcelain, and they are currently the failing ones — see
//!   [`copy_score_threshold`] for what this module adds around them rather than
//!   on top of them.
//! * **`shape_reach.rs`** — one small group per shape, to prove the shape is
//!   reached at all: `-s`/`--porcelain`/`--no-use-mailmap` on `Attributes`,
//!   `-s`/`-s -C`/`-s -C -C`/`-s -M` on `Renamed`'s four paths, `-s`/`-s -w` on
//!   `Whitespace`. Whole-file blames only — no `-L` anywhere in it, which is
//!   what leaves the interaction between `-L` and `-C` open.
//! * **`graft_partial.rs`** — `blame`, `-L 1,1`, `--line-porcelain` and
//!   `--incremental` of `deep.txt` in a shallow clone, plus
//!   `blame HEAD~2 -- deep.txt`: the boundary commit, not the line history.
//! * **`fixture_gaps2.rs`** — `blame`/`blame --porcelain` of `hist.txt` on
//!   `Shape::Promisor` (objects fetched on demand).
//! * **`fixture_gaps3.rs`** — `blame --porcelain dual` with and without `--` on
//!   `AmbiguousRef` (is `dual` a rev or a path), and `blame --abbrev=4` on
//!   `PrefixCollision` (how wide an id must print).
//! * **`hooks_identity.rs`** — `blame -e` and `blame --line-porcelain` on
//!   `Attributes` under the mailmap keys: whose *name* is printed.
//! * **`config_reads.rs`** — `blame README.md` under `blame.showEmail`,
//!   `blame.blankBoundary`, `blame.showRoot`, one key at a time.
//! * **`env_layer.rs`** — `blame --date=relative` with `GIT_TEST_DATE_NOW`
//!   pinned, the one place a relative date is comparable.
//! * **`attributes_filters.rs`** — `blame --textconv -s docs/manual.md` under a
//!   `diff.markdown.textconv` driver. It owns **textconv**; this module owns
//!   **funcname**, the other half of what a `diff=<lang>` attribute selects.
//! * **`misc_commands.rs`** — `blame --help-all`, argument parsing only.
//!
//! And for the pickaxe half: **`diff_family.rs`** owns `log -S`/`-G`/
//! `--pickaxe-regex`/`--pickaxe-all`/`--find-object` on `Branched` and `Merged`
//! plus `diff-index -S` and `diff-files -G` on `Dirty`; **`log_format.rs`** owns
//! `--find-object`/`--pickaxe-all` on `Renamed`. Neither ever combines a
//! pickaxe with a **pathspec**, which is where the search's scope is decided.
//!
//! # Which shapes can express a line-history question at all
//!
//! A blame is only worth running where one file has been touched by more than
//! one commit. Measured against every shape's builder in `fixture.rs`:
//!
//! * [`Shape::Whitespace`] — `ws/indent.c`, five commits deep, four of them
//!   rewriting overlapping line ranges of the same eight lines, one of them a
//!   real edit buried in whitespace churn. The only shape where `-w` changes a
//!   blame's *answer* and not just its rendering, and the only one with a C file
//!   whose `int main(void)` gives `:funcname` something to find under the
//!   built-in driver. Its worktree copy is edited and uncommitted, so every case
//!   here names `HEAD` explicitly or passes `-s`: an un-pinned blame of that
//!   path attributes lines to `00000000` and prints the **wall clock**.
//! * [`Shape::Renamed`] — 40-line files across a pure rename, a rename with
//!   eight edited lines, a copy whose source is modified in the same commit, and
//!   a rewrite. The only shape where `-M` and `-C` have anything to find, and
//!   the only one where a line's *previous name* differs from its current one.
//! * [`Shape::Attributes`] — `docs/manual.md` carries `diff=markdown` and is
//!   touched by two commits. The only shape where a funcname pattern comes from
//!   an attribute rather than from the built-in default.
//! * [`Shape::Merged`], [`Shape::Octopus`] — a file introduced on a side branch
//!   and reachable only through a merge's second (or fourth) parent, which is
//!   the only way `--first-parent` changes a blame.
//!
//! Every other shape is a floor case for this module and is deliberately not
//! used: `Linear`, `Detached`, `AwkwardPaths`, `Symlinks`, `DecomposedPaths`,
//! `Submodule`, `Sparse`, `Packed`, `NoIndexTrees`, `Hooked`, `Unrelated`,
//! `TagChain`, `Stashed`, `Worktree`, `SplitIndex` and the rest hold at most one
//! commit per path, so a `-L` range, a `-M`, a `-C` and a `--first-parent` all
//! collapse to the same one-commit answer and measure nothing about lines. The
//! corpus already blames several of them for *other* reasons (ambiguity,
//! abbreviation, mailmap, promisors); repeating that here would add cases and no
//! measurement.
//!
//! # What is not measurable, and why no case here pretends otherwise
//!
//! * **`--color-by-age`** colours each line by its age *against the wall clock*
//!   (`blame.coloring=highlightRecent`, whose default threshold is one year), so
//!   which colour a line gets is a function of the day the run happens. It is
//!   stable today only because the pinned author date (`1700000000`,
//!   2023-11-14) is permanently more than a year in the past — a fact about
//!   this decade, not a property of the flag. `info_attrs.rs` already ships one
//!   such case; this module adds none.
//! * **`--date=relative`** and `%ar` need `GIT_TEST_DATE_NOW` to be comparable
//!   at all; `env_layer.rs` owns that variable and the one blame case using it.
//! * **`--date=human`** is measurable here for exactly the same reason
//!   `--color-by-age` is not quite: its output collapses to `Mon DD YYYY` for
//!   anything over a year old and never changes again. One case uses it, and it
//!   is the *exit code* that carries the finding, not the rendering.
//! * **`--progress`** writes to stderr, which the runner deliberately does not
//!   byte-compare; a case on it measures only that both sides exit 0.
//! * **`--show-stats`** prints counters (`num read blob`, `num get patch`,
//!   `num commits`) that are a function of the traversal, not of the clock —
//!   verified identical across three stock runs on `Shape::Renamed` — so it is
//!   deterministic and *is* a legitimate parity surface. `info_attrs.rs`
//!   already has one; nothing is added here.
//! * **`--incremental`** is ordered by the traversal, not by wall time, and
//!   reproduces byte for byte across runs (verified three times on stock). It
//!   is already covered in four places and is not re-covered here.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append the line-history cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    line_ranges(out);
    funcname_ranges(out);
    attribute_driven_funcname(out);
    copy_score_threshold(out);
    rename_boundary(out);
    reverse_and_ranges(out);
    first_parent_through_merges(out);
    ignored_revs(out);
    refused_options(out);
    log_line_log(out);
    pickaxe_scope(out);
}

/// One case on one shape.
fn one(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], shape: Shape) {
    out.push(Case::new(cmd, args, shape));
}

/// Every argv in `args` on `shape`, under `cmd`.
fn each(out: &mut Vec<Case>, cmd: &'static str, shape: Shape, args: &[&[&str]]) {
    for a in args {
        out.push(Case::new(cmd, a, shape));
    }
}

// ---------------------------------------------------------------------------
// -L, the whole grammar
// ---------------------------------------------------------------------------

/// The `-L` grammar past `<start>,<end>`.
///
/// `blame`'s range argument is a small language — `line-range.c:parse_range`
/// plus `blame.c:parse_loc` — and the corpus reached six of its forms, all on a
/// two-line file where `1,9`, `9,9` and `0,0` are the only outcomes that
/// differ. The forms below are the rest of it, on an eight-line file with five
/// commits behind it, so a range that is off by one lands on a different
/// commit and is visible rather than being absorbed by a file that has only one
/// answer.
///
/// Every case names `HEAD` explicitly: `Shape::Whitespace` leaves `ws/indent.c`
/// edited in the worktree, and a blame that reaches that edit prints the wall
/// clock for its lines.
fn line_ranges(out: &mut Vec<Case>) {
    each(
        out,
        "blame",
        Shape::Whitespace,
        &[
            // `<start>,-<n>`: the count runs *backwards* from the start. The
            // corpus had `+<n>` and never the mirror.
            &["blame", "-s", "-L", "3,-2", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "-L", "5,+2", "HEAD", "--", "ws/indent.c"],
            // The two open ends, neither of which was reachable before.
            &["blame", "-s", "-L", ",3", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "-L", "6,", "HEAD", "--", "ws/indent.c"],
            // A bare start with no comma: git reads it as `<n>,<n>`.
            &["blame", "-s", "-L", "3", "HEAD", "--", "ws/indent.c"],
            // Leading `-`: a start relative to the end of the file.
            &["blame", "-s", "-L", "-3", "HEAD", "--", "ws/indent.c"],
            // Zero-length counts in both directions.
            &["blame", "-s", "-L", "1,+0", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "-L", "3,-0", "HEAD", "--", "ws/indent.c"],
            // `^/regex/` anchors the search at the start of the file rather
            // than at the previous range's end.
            &["blame", "-s", "-L", "^/total/,+2", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "-L", "/nosuchregex/", "HEAD", "--", "ws/indent.c"],
            // Two ranges at once, and the two orderings that decide whether the
            // second is resolved relative to the first: disjoint ascending,
            // disjoint descending, and overlapping.
            &["blame", "-s", "-L", "1,2", "-L", "5,6", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "-L", "5,6", "-L", "1,2", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "-L", "1,2", "-L", "2,3", "HEAD", "--", "ws/indent.c"],
            // Both ends past the end of an eight-line file, and a reversed pair.
            &["blame", "-s", "-L", "100,200", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "-L", "2,1", "HEAD", "--", "ws/indent.c"],
        ],
    );

    // The same grammar through the other two front ends, on the two forms most
    // likely to be parsed by a separate code path: a backwards count and two
    // ranges. One engine behind three entry points must not answer differently
    // depending on which one was typed.
    one(
        out,
        "annotate",
        &["annotate", "-L", "3,-2", "HEAD", "--", "ws/indent.c"],
        Shape::Whitespace,
    );
    one(
        out,
        "annotate",
        &["annotate", "-L", "1,2", "-L", "5,6", "HEAD", "--", "ws/indent.c"],
        Shape::Whitespace,
    );
    one(
        out,
        "pickaxe",
        &["pickaxe", "-s", "-L", "3,-2", "HEAD", "--", "ws/indent.c"],
        Shape::Whitespace,
    );
}

// ---------------------------------------------------------------------------
// :funcname, under the built-in driver
// ---------------------------------------------------------------------------

/// `-L :<funcname>` resolved by git's **default** funcname pattern.
///
/// The default pattern (`userdiff.c`'s fallback: a line beginning with an
/// alphabetic character, `_` or `$`) is what applies to a path with no
/// `diff=<driver>` attribute, and `ws/indent.c`'s `int main(void)` is the only
/// line in the corpus's fixtures that it matches at a known place. The three
/// forms below are the whole of the syntax: the name alone, the name with the
/// file spelled into the argument, and a name that is not there.
///
/// `log_format.rs` reaches the same pattern through `log -L :main:ws/indent.c`;
/// having both is the point — one scoreboard, two front ends, and the answer
/// must not depend on which one was typed.
fn funcname_ranges(out: &mut Vec<Case>) {
    each(
        out,
        "blame",
        Shape::Whitespace,
        &[
            &["blame", "-s", "-L", ":main", "HEAD", "--", "ws/indent.c"],
            // The `:<funcname>:<file>` spelling `log -L` uses. `blame` accepts
            // it and then has no argument left for the file, so the rev that
            // follows is read as a path — a parse whose failure mode is a
            // *different* message than a missing function.
            &["blame", "-s", "-L", ":main:ws/indent.c"],
            &["blame", "-s", "-L", ":nosuch", "ws/indent.c"],
        ],
    );
    one(
        out,
        "annotate",
        &["annotate", "-L", ":main", "HEAD", "--", "ws/indent.c"],
        Shape::Whitespace,
    );
    one(
        out,
        "pickaxe",
        &["pickaxe", "-s", "-L", ":main", "HEAD", "--", "ws/indent.c"],
        Shape::Whitespace,
    );
}

// ---------------------------------------------------------------------------
// :funcname, driven by a gitattributes diff driver
// ---------------------------------------------------------------------------

/// `-L :<funcname>` when the funcname pattern comes from `diff=<lang>`.
///
/// This is the crossing the brief calls for and the one the corpus could not
/// reach: which regex `:funcname` searches with is not a property of `blame`,
/// it is a property of the **path**. `git_attr("diff")` names a driver,
/// `userdiff_find_by_name()` supplies that driver's `funcname` regex, and only
/// if neither answers does the built-in default apply
/// (`blame.c` → `userdiff_find_by_path()`).
///
/// `Shape::Attributes` carries `*.md diff=markdown` and a `docs/manual.md`
/// whose first line is `# manual`. Under the markdown driver that line **is** a
/// funcname and `prose` is not; under the default pattern `prose` is and
/// `# manual` is not. So the same file answers `-L :manual` and `-L :prose`
/// with exactly opposite outcomes depending on whether the attribute was read,
/// and the two cases together cannot both be satisfied by ignoring it:
///
/// ```text
/// $ git blame -s -L :manual docs/manual.md        # stock 2.55.0
/// f3f4c337 1) # manual
/// …
/// $ git blame -s -L :prose docs/manual.md
/// fatal: -L parameter 'prose' starting at line 1: no match
/// ```
///
/// The third case supplies the driver's regex from configuration
/// (`diff.markdown.xfuncname`) instead of taking the built-in markdown one, so
/// the attribute is still what *selects* the driver but the pattern is a value
/// the case controls. Its answer is a range, not an error, which is what makes
/// it the strictest of the three: a front end that silently used the wrong
/// pattern would still exit 0 and print a plausible line.
///
/// `attributes_filters.rs` owns the *other* thing a `diff=` driver supplies —
/// `textconv` — on the same path and the same shape. Nothing here re-covers
/// attribute lookup itself; `info_attrs.rs`'s `check-attr` cases own that.
fn attribute_driven_funcname(out: &mut Vec<Case>) {
    each(
        out,
        "blame",
        Shape::Attributes,
        &[
            // A markdown heading: a funcname only under the driver.
            &["blame", "-s", "-L", ":manual", "docs/manual.md"],
            // A plain word: a funcname only under the default pattern.
            &["blame", "-s", "-L", ":prose", "docs/manual.md"],
            // Neither: both patterns must fail, and fail the same way.
            &["blame", "-s", "-L", ":nosuch", "docs/manual.md"],
            // `src/tabs.rs` has no `diff=` attribute, so this is the control:
            // the default pattern, on the same shape, must still work.
            &["blame", "-s", "-L", ":indented", "src/tabs.rs"],
        ],
    );

    // The driver selected by the attribute, with its funcname regex supplied
    // from configuration. `^prose` matches line 3 and not line 4, so the range
    // is 3..4 — one line longer than the default pattern would give, because
    // the default matches `more prose` on line 4 and stops there.
    out.push(
        Case::new("blame", &["blame", "-s", "-L", ":prose", "docs/manual.md"], Shape::Attributes)
            .with_config(&[("diff.markdown.xfuncname", "^prose")]),
    );

    // The same question through the other three front ends.
    one(out, "annotate", &["annotate", "-L", ":manual", "docs/manual.md"], Shape::Attributes);
    one(out, "pickaxe", &["pickaxe", "-s", "-L", ":manual", "docs/manual.md"], Shape::Attributes);
    each(
        out,
        "log",
        Shape::Attributes,
        &[
            &["log", "-L", ":manual:docs/manual.md", "--oneline"],
            &["log", "-L", ":prose:docs/manual.md", "--oneline"],
            &["log", "-L", ":indented:src/tabs.rs", "--oneline"],
        ],
    );
}

// ---------------------------------------------------------------------------
// -C and its threshold, measured through -L
// ---------------------------------------------------------------------------

/// The `-C` **copy score**, which is only observable when the blamed chunk is
/// small enough to fall under it.
///
/// `-C<num>` is documented as a threshold and the corpus set it four times
/// (`-M3`, `-C5`, `-C`, `-CC`, `-CCC` in `info_attrs.rs`; `-s -C`/`-s -C -C` in
/// `shape_reach.rs`) without ever being able to see it, because every one of
/// those blames a **whole** file. Git measures the score in *characters* of the
/// candidate chunk (`blame.c`'s `blame_copy_score`, default
/// `BLAME_DEFAULT_COPY_SCORE` = 40) and a 40-line file is 500-odd characters —
/// hundreds of times the threshold, so the comparison is never close and any
/// value of `-C<num>` gives the same answer.
///
/// `-L` is what makes it close. `copies/gamma.txt` is `gamma line <n>\n`, 13
/// characters a line, so a three-line range is 39 characters and a four-line
/// range is 52 — one below the default threshold and one above it. Verified by
/// hand against stock 2.55.0 in a rebuilt copy of the shape:
///
/// ```text
/// $ git blame -s -C -L1,3 copies/gamma.txt
/// 06d06aa0 1) gamma line 1              # 39 chars: under 40, copy rejected
/// $ git blame -s -C -L1,4 copies/gamma.txt
/// 3fc09baf orig/gamma.txt 1) gamma line 1   # 52 chars: over 40, copy found
/// $ git blame -s -C20 -L1,3 copies/gamma.txt
/// 3fc09baf orig/gamma.txt 1) gamma line 1   # 39 chars: over 20, copy found
/// ```
///
/// The seven cases below straddle that boundary from both sides and at three
/// different thresholds, so an implementation that ignores the score and one
/// that applies it are separated by *which* of them agree rather than by all of
/// them failing together — which is the difference between a case that reports
/// a defect and a case that locates one.
///
/// `-M` is included at the same range as the control: the move score has a
/// different default (20) and a different candidate set, and a single answer for
/// both would be the mistake this group exists to catch.
fn copy_score_threshold(out: &mut Vec<Case>) {
    each(
        out,
        "blame",
        Shape::Renamed,
        &[
            // 39 characters against a default threshold of 40.
            &["blame", "-s", "-C", "-L", "1,3", "copies/gamma.txt"],
            // 52 characters against the same threshold.
            &["blame", "-s", "-C", "-L", "1,4", "copies/gamma.txt"],
            // The same 39 characters against a threshold they clear.
            &["blame", "-s", "-C20", "-L", "1,3", "copies/gamma.txt"],
            // One line, 13 characters, against a threshold of 10.
            &["blame", "-s", "-C10", "-L", "1,1", "copies/gamma.txt"],
            // 52 characters against a threshold of 90, which they do not clear.
            &["blame", "-s", "-C90", "-L", "1,4", "copies/gamma.txt"],
            // A second `-C` widens the candidate set to files the commit did
            // not touch; it does not lower the score.
            &["blame", "-s", "-C", "-C", "-L", "1,3", "copies/gamma.txt"],
            // `-M` at the same range: a different score, a different answer.
            &["blame", "-s", "-M", "-L", "1,3", "copies/gamma.txt"],
        ],
    );

    // The porcelain form of the same boundary. `--porcelain` is where a found
    // copy is reported as a `filename` header rather than as a column, so a
    // front end can get the score right and the header wrong.
    one(
        out,
        "blame",
        &["blame", "--porcelain", "-C", "-L", "1,1", "copies/gamma.txt"],
        Shape::Renamed,
    );
    one(
        out,
        "blame",
        &["blame", "--line-porcelain", "-C", "-L", "1,4", "copies/gamma.txt"],
        Shape::Renamed,
    );

    // The same threshold through the other two front ends.
    one(out, "annotate", &["annotate", "-C", "-L", "1,3", "copies/gamma.txt"], Shape::Renamed);
    one(out, "pickaxe", &["pickaxe", "-s", "-C", "-L", "1,3", "copies/gamma.txt"], Shape::Renamed);
}

// ---------------------------------------------------------------------------
// A line whose previous name is not its current one
// ---------------------------------------------------------------------------

/// Blame across a rename, from both directions.
///
/// `log_format.rs` already owns the two porcelain cases that cross
/// `renames: rename with edit` (`--porcelain -L1,3` and `--line-porcelain
/// -L3,3` of `moved/beta.txt`) and they are the corpus's existing failures
/// there. Hand comparison against stock 2.55.0 localises what they are
/// reporting, which is worth recording because the two cases alone do not say
/// it: the missing bytes are the `previous <sha> <path>` header, and it is
/// missing **only** where the parent held the file under a different name —
///
/// ```text
/// $ git blame --porcelain moved/beta.txt   | grep ^previous   # stock
/// previous 89b071fc838e0cb5147c619157d25b67beaf2a70 orig/beta.txt
/// $ git blame --porcelain src/lib.rs       | grep ^previous   # stock, Branched
/// previous edfab1b71619a22120a8da1a3d85d68e0200290a src/lib.rs
/// ```
///
/// — the second of which the port reproduces and the first of which it drops.
/// So this group does not pile more cases onto the same failure. It pins the
/// **surrounding** answers instead, which is what turns a failure into a
/// bisected one: the same rename asked without porcelain, the same rename asked
/// at the old path, a rename with no edit, a modification whose path did not
/// change, and the deleted path's error.
fn rename_boundary(out: &mut Vec<Case>) {
    each(
        out,
        "blame",
        Shape::Renamed,
        &[
            // The rename, reported as a column rather than as a header.
            &["blame", "-s", "-f", "-M", "-L", "1,4", "moved/beta.txt"],
            // The pure rename: content identical, path changed, so the blamed
            // commit is the seed and there is no `previous` to print.
            &["blame", "--porcelain", "-L", "1,1", "moved/alpha.txt"],
            &["blame", "-s", "--root", "-f", "-L", "1,2", "moved/alpha.txt"],
            // A modification whose path did *not* change, on the same shape:
            // the control for the header above.
            &["blame", "--porcelain", "-L", "5,5", "orig/gamma.txt"],
            // The whole-file rewrite: every line new, one commit deep.
            &["blame", "--porcelain", "-L", "1,1", "orig/delta.txt"],
            // The old path, at a revision where it still existed.
            &["blame", "-s", "-n", "-f", "HEAD~4", "--", "orig/beta.txt"],
            // The old path at `HEAD`, where it does not.
            &["blame", "-s", "orig/alpha.txt"],
        ],
    );
}

// ---------------------------------------------------------------------------
// --reverse, and a blame limited to a range of history
// ---------------------------------------------------------------------------

/// The two ways to blame something other than "all of history up to `HEAD`".
///
/// `--reverse <old>..<new>` inverts the question — *when did this line last
/// exist*, walking forward — and a commit range without `--reverse` bounds the
/// walk from below so the oldest surviving commit becomes a boundary. The
/// corpus had one of each (`--reverse HEAD~1..HEAD` and `HEAD~1 --` on
/// `Branched`, whose history is two commits deep, so neither could show a
/// boundary that was not also the root).
///
/// Five commits deep, they can. Every case names `HEAD` or an explicit revision
/// so the uncommitted worktree edit stays out of the answer.
fn reverse_and_ranges(out: &mut Vec<Case>) {
    each(
        out,
        "blame",
        Shape::Whitespace,
        &[
            &["blame", "-s", "--reverse", "HEAD~3..HEAD", "--", "ws/indent.c"],
            // A reverse walk that stops short of `HEAD`: the lines that die
            // inside the range are attributed to where they died.
            &["blame", "-s", "--reverse", "HEAD~4..HEAD~1", "--", "ws/indent.c"],
            // The same bounds without `--reverse`: a lower bound, not an
            // inversion.
            &["blame", "-s", "HEAD~4..HEAD", "--", "ws/indent.c"],
            // The `^<rev> <rev>` spelling of the same bound.
            &["blame", "-s", "^HEAD~3", "HEAD", "--", "ws/indent.c"],
            // `-b` blanks the boundary commit's id, `--root` refuses to treat
            // the initial commit as one.
            &["blame", "-s", "-b", "HEAD~2", "--", "ws/indent.c"],
            &["blame", "-s", "--root", "HEAD~4", "--", "ws/indent.c"],
        ],
    );

    each(
        out,
        "blame",
        Shape::Renamed,
        &[
            // A reverse walk that has to follow a rename forwards: the path is
            // `orig/alpha.txt` at the start of the range and `moved/alpha.txt`
            // at the end.
            &["blame", "-s", "--reverse", "HEAD~4..HEAD", "--", "orig/alpha.txt"],
            &["blame", "-s", "--reverse", "--first-parent", "HEAD~4..HEAD", "--", "orig/alpha.txt"],
            // A reverse walk that ends before the rename happens.
            &["blame", "-s", "--reverse", "HEAD~4..HEAD~2", "--", "orig/alpha.txt"],
        ],
    );
}

// ---------------------------------------------------------------------------
// --first-parent, where a merge is the only way in
// ---------------------------------------------------------------------------

/// `--first-parent` on a file that arrived through a *second* parent.
///
/// The flag can only change an answer where the walk has a choice, and the
/// corpus's two cases are on `Branched` (no merge at all) and on `Merged`'s
/// `main.txt` (which is on the first-parent line anyway, so the flag is inert).
/// The interesting side is the other one: `side.txt` exists only through the
/// merge's second parent, and `oct-b.txt` only through an octopus's third, so
/// with `--first-parent` git has to attribute every one of their lines to the
/// merge commit itself rather than to the commit that wrote them.
fn first_parent_through_merges(out: &mut Vec<Case>) {
    each(
        out,
        "blame",
        Shape::Merged,
        &[
            &["blame", "-s", "--first-parent", "side.txt"],
            &["blame", "--porcelain", "--first-parent", "-L", "1,1", "side.txt"],
        ],
    );
    each(
        out,
        "blame",
        Shape::Octopus,
        &[
            // Second, third and fourth parents of one merge.
            &["blame", "-s", "--first-parent", "oct-a.txt"],
            &["blame", "--porcelain", "--first-parent", "-L", "1,1", "oct-b.txt"],
            &["blame", "-s", "--first-parent", "oct-c.txt"],
            // On the first-parent line, where the flag must change nothing.
            &["blame", "-s", "--first-parent", "trunk.txt"],
        ],
    );
    each(
        out,
        "log",
        Shape::Octopus,
        &[
            &["log", "-L", "1,1:oct-b.txt", "--oneline"],
            &["log", "-L", "1,1:oct-b.txt", "--first-parent", "--oneline"],
            &["log", "-L", "1,1:oct-b.txt", "--graph", "--oneline"],
        ],
    );
    one(out, "log", &["log", "-L", "1,1:side.txt", "--oneline"], Shape::Merged);
}

// ---------------------------------------------------------------------------
// --ignore-rev, on a revision that really moved the lines
// ---------------------------------------------------------------------------

/// The ignore-revs machinery, on a commit whose only content *is* the thing
/// being ignored.
///
/// `info_attrs.rs` covers the flags (`--ignore-rev HEAD`,
/// `--ignore-revs-file /dev/null`, a missing file, `blame.ignoreRevsFile`,
/// `blame.markIgnoredLines`, `blame.markUnblamableLines`) on `Branched`, where
/// `HEAD` is the commit that *added* the lines. Ignoring the commit that added
/// a line is the degenerate case: there is nowhere to pass the blame to, so
/// every one of those cases exercises the unblamable path and none of them
/// exercises the ordinary one.
///
/// `Shape::Whitespace` supplies the ordinary one. `HEAD~1` reflows
/// `ws/indent.c` without changing a token, so ignoring it must hand each line
/// to whichever commit last changed it *for real* — which is the entire reason
/// the flag exists — and `HEAD~2`+`HEAD~1` together ask the same of two
/// consecutive reflows.
fn ignored_revs(out: &mut Vec<Case>) {
    each(
        out,
        "blame",
        Shape::Whitespace,
        &[
            // `log_format.rs` owns the single `--ignore-rev HEAD~1` form; what
            // follows is the multi-revision half of the flag it cannot reach.
            &[
                "blame", "-s", "--ignore-rev", "HEAD~2", "--ignore-rev", "HEAD~1", "HEAD", "--",
                "ws/indent.c",
            ],
            &["blame", "-s", "--ignore-rev", "HEAD", "--ignore-rev", "HEAD~1", "HEAD", "--", "ws/indent.c"],
            // A revs file that exists and holds no revision: `README.md` is
            // `# fixture`, so the parse has something to reject rather than an
            // empty file or a missing one.
            &["blame", "-s", "--ignore-revs-file", "README.md", "HEAD", "--", "ws/indent.c"],
        ],
    );

    // The two marker settings, over an ignored revision that *can* be blamed
    // elsewhere — so `?` (ignored) and `*` (unblamable) are distinguishable,
    // which they are not when the ignored commit is the one that added the
    // line.
    for (key, rev) in [("blame.markIgnoredLines", "HEAD~1"), ("blame.markUnblamableLines", "HEAD~1")]
    {
        out.push(
            Case::new(
                "blame",
                &["blame", "-s", "--ignore-rev", rev, "HEAD", "--", "ws/indent.c"],
                Shape::Whitespace,
            )
            .with_config(&[(key, "true")]),
        );
    }

    // `blame.ignoreRevsFile` naming a tracked file that is not a revs file, and
    // the empty value that resets the list.
    out.push(
        Case::new("blame", &["blame", "-s", "HEAD", "--", "ws/indent.c"], Shape::Whitespace)
            .with_config(&[("blame.ignoreRevsFile", "README.md")]),
    );
    out.push(
        Case::new("blame", &["blame", "-s", "HEAD", "--", "ws/indent.c"], Shape::Whitespace)
            .with_config(&[("blame.ignoreRevsFile", "")]),
    );
}

// ---------------------------------------------------------------------------
// The flags the port does not take
// ---------------------------------------------------------------------------

/// Options `git blame` accepts that nothing in the corpus had asked it for.
///
/// Each of these is a separate entry in `builtin/blame.c`'s option table and
/// each is refused by the port today, which is exactly the kind of gap a corpus
/// exists to name. They are grouped because they share a failure *shape*, not
/// because they share a cause:
///
/// ```text
/// $ git blame -s -S README.md HEAD -- ws/indent.c   # stock 2.55.0: exit 0
/// $ zvcs blame -s -S README.md HEAD -- ws/indent.c
/// error: switch `S' requires a value                # exit 129
/// $ git blame --no-first-parent HEAD -- ws/indent.c # stock: exit 129
/// error: unknown option `(null)'
/// $ zvcs blame --no-first-parent HEAD -- ws/indent.c # exit 0, full output
/// ```
///
/// `--no-first-parent` is the one that runs the other way: stock 2.55.0 has no
/// negation for `--first-parent` and dies, and the port accepts it. A superset
/// is still a difference, and this is the case that says so.
///
/// `-S <revs-file>` is measured for *acceptance*, not for its semantics: the
/// only file a case can name is one the fixture already tracks, none of them
/// holds an object id, and stock treats a revs file it cannot parse as no
/// restriction at all. That still separates a front end that takes the option
/// from one that does not, which is the whole distance between the two sides
/// here.
///
/// `--textconv` is `attributes_filters.rs`'s (it needs a driver to be worth
/// asking); its negation is not, and is here.
fn refused_options(out: &mut Vec<Case>) {
    each(
        out,
        "blame",
        Shape::Whitespace,
        &[
            &["blame", "-s", "-S", "README.md", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "-S", "/dev/null", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "--no-first-parent", "HEAD", "--", "ws/indent.c"],
            &["blame", "-s", "--no-textconv", "HEAD", "--", "ws/indent.c"],
            // `ws/eol.txt` lost its CRLF line endings in `HEAD~1`, so this is
            // the one path in the corpus where ignoring a carriage return at
            // end of line can change which commit a line belongs to.
            &["blame", "-s", "--ignore-cr-at-eol", "HEAD", "--", "ws/eol.txt"],
            // The rendering is `Mon DD YYYY` for anything over a year old, and
            // the fixture's pinned date is permanently over a year old — see
            // the module header. The exit code is what this case turns on.
            &["blame", "--date=human", "-L", "1,1", "HEAD", "--", "ws/indent.c"],
        ],
    );
    // `--indent-heuristic` is covered; its negation, which selects the other
    // diff slider, is not.
    one(
        out,
        "blame",
        &["blame", "-s", "--no-indent-heuristic", "-L", "1,3", "moved/beta.txt"],
        Shape::Renamed,
    );

    // The same three through `annotate`. One engine, two front ends: if the
    // option table is shared they must refuse identically, and if it is not,
    // that is the finding.
    each(
        out,
        "annotate",
        Shape::Whitespace,
        &[
            &["annotate", "-S", "README.md", "HEAD", "--", "ws/indent.c"],
            &["annotate", "--no-first-parent", "HEAD", "--", "ws/indent.c"],
            &["annotate", "--date=human", "-L", "1,1", "HEAD", "--", "ws/indent.c"],
        ],
    );
}

// ---------------------------------------------------------------------------
// log -L: the other front end onto the same walk
// ---------------------------------------------------------------------------

/// `git log -L`, the line-log walk.
///
/// `log_format.rs` owns three forms (`3,5:`, `3,+2:`, `:main:`) and three error
/// cases. What it does not ask is how the line-log interacts with the rest of
/// `log`'s output machinery, and that interaction is not free: a line-log
/// builds its own diff (`line-log.c:dump_diff_hacky`) rather than going through
/// `diff_flush`, so every output selector has to be re-honoured there or
/// silently ignored. Two of them are:
///
/// ```text
/// $ git log -L 3,5:ws/indent.c --raw --oneline | head -2   # stock 2.55.0
/// 38f9403 whitespace: one edit amid churn
/// :100644 100644 027ce28 4359683 M	ws/indent.c
/// $ git log -L 3,5:ws/indent.c -w --oneline | head -3
/// 38f9403 whitespace: one edit amid churn
/// diff --git a/ws/indent.c b/ws/indent.c
/// @@ -3,3 +3,3 @@ int main(void)
/// ```
///
/// — a raw entry instead of a patch in the first, and a narrower hunk carrying
/// a funcname context in the second. Both are things a line-log that ignores
/// the flag would print differently, and neither had a case.
fn log_line_log(out: &mut Vec<Case>) {
    each(
        out,
        "log",
        Shape::Whitespace,
        &[
            // Output selectors on top of a line-log.
            &["log", "-L", "3,5:ws/indent.c", "--raw", "--oneline"],
            &["log", "-L", "3,5:ws/indent.c", "-w", "--oneline"],
            &["log", "-L", "3,5:ws/indent.c", "-s", "--oneline"],
            &["log", "-L", "3,5:ws/indent.c", "--stat", "--oneline"],
            &["log", "-L", "3,5:ws/indent.c", "--numstat", "--oneline"],
            &["log", "-L", "3,5:ws/indent.c", "-U0", "--oneline"],
            &["log", "-L", "3,5:ws/indent.c", "--format=%h%x20%s"],
            // Walk selectors.
            &["log", "-L", "3,5:ws/indent.c", "--reverse", "--oneline"],
            &["log", "-L", "3,5:ws/indent.c", "--max-count=1", "--oneline"],
            &["log", "-L", "3,5:ws/indent.c", "HEAD~1", "--oneline"],
            // The range grammar `log -L` shares with `blame -L`, which
            // `log_format.rs` reaches only as `n,m` and `n,+k`.
            &["log", "-L", "/total/,+2:ws/indent.c", "--oneline"],
            &["log", "-L", "^/total/,+2:ws/indent.c", "--oneline"],
            // Two ranges in one file, and one range in each of two files.
            &["log", "-L", "3,3:ws/indent.c", "-L", "5,5:ws/indent.c", "--oneline"],
            &["log", "-L", "1,1:README.md", "-L", "1,1:src/lib.rs", "--oneline"],
            // The other file on the shape, whose only change is its line
            // endings.
            &["log", "-L", "1,2:ws/eol.txt", "--oneline"],
        ],
    );

    each(
        out,
        "log",
        Shape::Renamed,
        &[
            // A line-log that has to walk back through a rename, with and
            // without the flags that are supposed to make it do so.
            &["log", "-L", "1,3:moved/alpha.txt", "--oneline"],
            &["log", "-M", "-L", "1,3:moved/alpha.txt", "--oneline"],
            &["log", "-L", "1,3:moved/beta.txt", "--oneline"],
            &["log", "--follow", "-L", "1,3:moved/beta.txt", "--oneline"],
            &["log", "-L", "1,3:copies/gamma.txt", "-p", "--oneline"],
        ],
    );
}

// ---------------------------------------------------------------------------
// The pickaxe, and the scope it searches
// ---------------------------------------------------------------------------

/// `-S`/`-G` narrowed by a **pathspec**.
///
/// `diff_family.rs` and `log_format.rs` between them cover `-S`, `-G`,
/// `--pickaxe-regex`, `--pickaxe-all` and `--find-object` on four shapes, and
/// every one of those cases searches the *whole* commit. A pathspec changes
/// what the search is run against, not just what is printed afterwards: git
/// applies it to the diff first and then counts occurrences in what is left
/// (`diffcore_pickaxe` runs after `diffcore_std`'s pathspec filter), so a
/// commit can match without a pathspec and not match with one.
///
/// `Shape::Renamed`'s `renames: rename with edit` is the commit where that is
/// visible. Restricted to `orig/beta.txt` its diff is a pure deletion of a blob
/// containing no `edited`, so the count goes 0 → 0 and the commit does not
/// match, even though the same commit *adds* eight `edited` lines at
/// `moved/beta.txt`:
///
/// ```text
/// $ git log -Sedited --oneline                        # stock 2.55.0
/// 06d06aa renames: copy with modified source
/// 1982909 renames: rename with edit
/// $ git log -Sedited --oneline -- orig/beta.txt       # stock: no output
/// $ git show --raw --oneline 1982909 -- orig/beta.txt
/// :100644 000000 a3c7529 0000000 D	orig/beta.txt
/// ```
///
/// Both sides were run three times by hand to confirm the answers are stable
/// before this was written down.
fn pickaxe_scope(out: &mut Vec<Case>) {
    each(
        out,
        "pickaxe",
        Shape::Renamed,
        &[
            // The same needle, unrestricted and restricted three ways: to the
            // path the commit deleted, to the path it created, and to the
            // directory holding both.
            &["log", "-Sedited", "--oneline"],
            &["log", "-Sedited", "--oneline", "--", "orig/beta.txt"],
            &["log", "-Sedited", "--oneline", "--", "moved/beta.txt"],
            &["log", "-Sedited", "--oneline", "--", "orig"],
            // `-G` counts differently — it matches the *diff text*, so a
            // deletion of a line that never held the needle still cannot match.
            &["log", "-Gedited", "--oneline", "--", "orig/beta.txt"],
            &["log", "-G", "line 5 edited", "--pickaxe-regex", "--oneline"],
            // Rename detection is what decides whether the restricted diff is a
            // deletion or a rename in the first place.
            &["log", "-Sedited", "-M", "--oneline", "--", "orig/beta.txt"],
            &["log", "-Sedited", "--find-copies-harder", "--oneline"],
            // The two output selectors on a pickaxe-limited walk.
            &["log", "-Sedited", "--name-status", "--oneline"],
            &["log", "-S", "gamma line 5", "--pickaxe-all", "--raw", "--oneline"],
            // An empty needle: every commit whose diff changes the count of the
            // empty string, which is none of them.
            &["log", "-S", "", "--oneline"],
            &["log", "-G", "", "--oneline"],
            // The same search through the plumbing that has no history walk.
            &["diff-tree", "-S", "edited", "-r", "HEAD~2", "HEAD"],
            &["rev-list", "-S", "edited", "HEAD"],
            &["rev-list", "--count", "-G", "edited", "HEAD"],
            // `--find-object` given a blob named by a revision the fixture
            // resolves, rather than by a hash constant that would go stale with
            // the shape.
            &["log", "--find-object=HEAD:moved/beta.txt", "--oneline"],
            &["diff-tree", "--find-object=HEAD:moved/beta.txt", "-r", "HEAD~3", "HEAD"],
        ],
    );

    each(
        out,
        "pickaxe",
        Shape::Whitespace,
        &[
            // A needle that only whitespace changes moved: `-S` counts
            // occurrences, so a reflow that does not add or remove one cannot
            // match however many lines it rewrote.
            &["log", "-S", "total += i", "--oneline"],
            &["log", "-G", "total", "--oneline"],
            &["diff", "-S", "total += i * 2", "HEAD~2", "HEAD"],
            &["diff", "-G", "total", "HEAD~2", "HEAD"],
            &["log", "--find-object=HEAD:ws/eol.txt", "--oneline"],
        ],
    );
}
