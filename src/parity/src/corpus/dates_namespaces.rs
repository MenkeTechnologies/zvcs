//! Two axes the corpus could not measure at all, on the two fixture shapes that
//! finally make them measurable: **which of a commit's two dates a command
//! reads** ([`Shape::SkewedDates`]), and **what a namespace does when it is not
//! empty** ([`Shape::Namespaced`]).
//!
//! Both were structural blind spots rather than gaps in coverage, and both were
//! written down as such by the modules that hit them:
//!
//! * `traversal_order.rs`'s header measured every shape with
//!   `log --all --format='%ad|%cd' --date=raw` and found one timestamp in both
//!   fields everywhere, because `env::harden` pins `GIT_AUTHOR_DATE` and
//!   `GIT_COMMITTER_DATE` to `1700000000 +0000` and nothing in `fixture.rs`
//!   overrode either. Its verdict: "`--date-order` and `--author-date-order` are
//!   indistinguishable from each other, in every shape, and no case can separate
//!   them … that is a shape change, not a case". [`Shape::SkewedDates`] is that
//!   shape change, and this file is the cases it unlocks.
//! * `discovery_env.rs` listed "a namespace that actually holds refs" as
//!   unreachable and *deleted* two `GIT_NAMESPACE` cases that were byte-identical
//!   to the baseline. [`Shape::Namespaced`] holds two refs under
//!   `refs/namespaces/ns/`, one under `refs/namespaces/other/`, and one ordinary
//!   branch outside, so the same variable now has something to find *and*
//!   something to hide.
//!
//! # How this divides territory with the six adjacent modules
//!
//! Every one was read in full before a case was written here. **No argv below
//! runs on any shape those modules name**: everything is on `SkewedDates` or
//! `Namespaced`, and neither shape appears anywhere else under `corpus/`
//! (checked by grep for both names). The only other consumer is `fuzz.rs`, which
//! samples both shapes; a sampled argv is not a curated one and cannot collide
//! with a `Case::id` here.
//!
//! * **`traversal_order.rs`** owns the walk on [`Shape::CrissCross`],
//!   [`Shape::Octopus`], [`Shape::Cherry`], [`Shape::CommitGraph`] and
//!   [`Shape::Packed`], where the *other* three orders separate from each other
//!   with every date equal. This file takes the one leg it could not: the
//!   author-date leg. The two files are complementary halves of one table — see
//!   "Are the five orderings distinct" below, which states exactly which pair
//!   each file separates and which pair neither can. Its age-window group pins
//!   `--min-age`/`--max-age` against the *pinned* stamp
//!   (`1699999999` / `1700000001`); [`age_window`] here uses `1650000000` and
//!   `1600000325`, values chosen to sit **between** the author and committer
//!   dates, which is a different question with a different answer.
//! * **`history_query.rs`** owns `rev-list` selection on `Octopus`,
//!   `BehindRemote` and `MergeableDirty`, and `show-branch`'s column matrix
//!   including its `--topo-order`/`--date-order` pair on
//!   `Octopus`/`BehindRemote`/`Branched`. [`front_ends`] asks `show-branch` the
//!   same two flags on `SkewedDates` — and records that they are *still* a tie
//!   there, which is a fact about `show-branch` and not about the shape.
//! * **`log_format.rs`** owns the `--date=` renderers, as
//!   `log -1 --date=<mode> --format=%ad|%cd` on [`Shape::Branched`]. On
//!   `Branched` `%ad` and `%cd` hold the same bytes, so **that case cannot tell
//!   an author-date renderer from a committer-date one**: a port that rendered
//!   `%cd` for `%ad` passes all eight of its modes. [`date_rendering`] runs the
//!   same mode list on `SkewedDates` over `--all`, where the two fields differ in
//!   every commit but the root, and that is the whole point of repeating the
//!   axis. Its exclusion list is adopted verbatim (see "Not measurable" below).
//! * **`naming_ancestry.rs`** states that every one of its `show-branch` cases
//!   runs on [`Shape::CrissCross`]. Nothing here does.
//! * **`plumbing_refs.rs`** owns the `show-ref`/`for-each-ref` floor and two
//!   date-limited walks — `rev-list --since=2000-01-01` and
//!   `--until=2000-01-01` on `Branched`, both in the ISO spelling and both
//!   selecting all-or-nothing. This file uses only `@<epoch>` spellings and only
//!   at boundaries that split the shape.
//! * **`discovery_env.rs`** owns `GIT_NAMESPACE` on the two serving verbs with an
//!   **empty** namespace, on [`Shape::Branched`] and [`Shape::BehindRemote`]: the
//!   `..` and `x.lock` refusals, the empty value, and the `--namespace=ns` option
//!   spelling. None of those is repeated. `env_layer.rs` and `globals_layer.rs`
//!   each set the namespace on `ls-remote .`, on `Branched` and `TagChain` — both
//!   empty, so both are byte-identical to their own baseline. The same argv on
//!   `Namespaced` is a different case (the shape is part of `Case::id`) and is the
//!   first one in the corpus that can fail.
//!
//! # Are the five orderings now provably distinct?
//!
//! Yes, across the corpus; **no, on any single shape**, and the reason is
//! structural rather than fixable by another case.
//!
//! Stock 2.55.0, `log --oneline --all` on [`Shape::SkewedDates`], measured:
//!
//! | flag | order |
//! |------|-------|
//! | *(default)*           | `sd-2` `sd-3` `sd-1` `initial` |
//! | `--topo-order`        | `sd-2` `sd-3` `sd-1` `initial` |
//! | `--date-order`        | `sd-2` `sd-3` `sd-1` `initial` |
//! | `--author-date-order` | `sd-3` `sd-2` `sd-1` `initial` |
//! | `--reverse`           | `initial` `sd-1` `sd-3` `sd-2` |
//!
//! So this shape separates `--author-date-order` from the other four, which no
//! shape could before. It does **not** separate the default from `--date-order`
//! or from `--topo-order`, and it cannot: every committer date here is still the
//! pinned `1700000000`, so `--date-order`'s sort key is constant and its queue
//! degenerates to the default's, and the DAG is a fork with no merge, which is
//! the case `--topo-order`'s in-degree pass agrees with the default on.
//! `traversal_order.rs`'s table separates exactly that triple, on `CrissCross`
//! and `Octopus`. Union of the two files: all five orderings are pairwise
//! distinct, each pair by a named case. Neither file alone suffices, and a
//! shape that separated all five at once would have to carry both a skewed
//! author date *and* a merge whose parents were committed out of order — that is
//! a third fixture, recorded here rather than assumed.
//!
//! One further separation this shape makes that no other could:
//! **the two ordering flags are last-one-wins**, and until now that was
//! unobservable because the two flags produced the same list. Measured on stock:
//!
//! ```text
//! log --oneline --all --date-order --author-date-order   sd-3 sd-2 sd-1 initial
//! log --oneline --all --author-date-order --date-order   sd-2 sd-3 sd-1 initial
//! ```
//!
//! # The tag caveat, which this file does not contradict
//!
//! `fixture.rs`'s `SkewedDates` block states it and it is re-measured here: an
//! annotated tag takes its tagger date from the **committer** ident, which stays
//! pinned. `for-each-ref --format='%(refname) %(taggerdate:unix) %(creatordate:unix)'`
//! on stock gives `refs/tags/sd-tag 1700000000 1700000000`, and both branches
//! report `creatordate` `1700000000` too (a branch's creatordate is its
//! committerdate). So **`--sort=taggerdate` and `--sort=creatordate` are still
//! ties on this shape**, exactly as `tag_family.rs` established independently.
//! [`ref_sorting`] carries one case of each anyway — as the *negative* control
//! that keeps `--sort=-authordate` honest, since a port that sorted every date
//! atom by the author date would pass the authordate rows and fail these.
//!
//! What *is* newly separating there: `--sort=-authordate` gives
//! `refs/heads/sd-side refs/heads/main refs/tags/sd-tag` while
//! `--sort=-committerdate` gives `refs/heads/main refs/heads/sd-side
//! refs/tags/sd-tag`. First time in the corpus those two sort keys disagree.
//!
//! # What `GIT_NAMESPACE` does now that the namespace is non-empty
//!
//! Measured on stock 2.55.0 and on the port, both binaries, in a hand-built copy
//! of [`Shape::Namespaced`], across `for-each-ref`, `show-ref`, `branch --list`,
//! `tag --list`, `rev-parse --all`, `log --all`, `rev-list --all`,
//! `symbolic-ref HEAD`, `update-ref`, `fsck`, `gc`, `pack-refs` and
//! `count-objects`, under `GIT_NAMESPACE` unset / `ns` / `other` and under the
//! `--namespace=` option spelling:
//!
//! **Every local verb still ignores it, on both binaries, and so does the
//! write.** `discovery_env.rs` reported that from an empty namespace and it was
//! reasonable to suspect the emptiness was doing the work. It was not: with two
//! refs sitting under `refs/namespaces/ns/`, `for-each-ref` still lists all five
//! refs under their full `refs/namespaces/...` names, `branch --list` still shows
//! `main` and `ns-outside`, `tag --list` is still empty, and
//! `GIT_NAMESPACE=ns update-ref refs/heads/x HEAD` still writes
//! `.git/refs/heads/x`. That is git's documented design — the namespace is a
//! *serving* concept — and the rows below are kept as the pinning of a negative
//! that a port could easily get wrong in the other direction.
//!
//! What changes, and is measurable for the first time:
//!
//! ```text
//! ls-remote .                    5 lines: HEAD, 2 branches, 3 refs/namespaces/… names
//! GIT_NAMESPACE=ns  ls-remote .  2 lines: refs/heads/inside, refs/tags/inside-tag  (no HEAD)
//! GIT_NAMESPACE=other ls-remote. 1 line:  refs/heads/elsewhere
//! GIT_NAMESPACE=nope ls-remote.  0 lines, exit 0
//! ```
//!
//! The prefix is stripped on the wire and `HEAD` disappears, because the
//! namespace has no `HEAD` of its own. `upload-pack --advertise-refs` shows the
//! same thing one layer down, and shows one more: the `symref=HEAD:refs/heads/main`
//! capability is **absent** from the namespaced advertisement, which the empty
//! namespace could not demonstrate (an empty advertisement has no capability
//! line at all beyond `capabilities^{}`).
//!
//! # What the port gets wrong
//!
//! Four, all reproduced by hand against stock 2.55.0 and 2.50.1.
//!
//! 1. **`shortlog --topo-order` sorts by the *author* date.** `log` and
//!    `rev-list` get `--topo-order` right on the same shape; `shortlog` maps it
//!    onto the author-date sort, and its output under `--topo-order` is
//!    byte-identical to its own output under `--author-date-order`. Both gits
//!    agree with each other and disagree with the port:
//!
//!    ```text
//!    shortlog --all --topo-order          stock 2.55.0 / 2.50.1   initial sd-1 sd-3 sd-2
//!                                         port                    initial sd-1 sd-2 sd-3
//!    shortlog --all --author-date-order   all three               initial sd-1 sd-2 sd-3
//!    log --oneline --all --topo-order     all three               sd-2 sd-3 sd-1 initial
//!    ```
//!
//!    This is the defect shape `traversal_order.rs` was built for — one front
//!    end's option table disagreeing with another's over the same walk — and it
//!    is invisible on every shape that file runs on, because there the two sort
//!    keys hold one value and the wrong mapping produces the right answer. Its
//!    own `shortlog --first-parent --topo-order --all` row on
//!    [`Shape::CommitGraph`] passes for exactly that reason.
//!
//! 2. **`rev-list --date=<mode>` exits 129; `log --date=<mode>` works.** The
//!    renderer flag is missing from `rev-list`'s option table only. Same family
//!    as `traversal_order.rs`'s finding 2 (`rev-list --skip=`) and a different
//!    flag; `rev-list --date=` appears nowhere else in the corpus.
//!
//!    ```text
//!    stock  rev-list --all --date=raw            4 ids, exit 0
//!    port                                        usage: git rev-list …, exit 129
//!    stock  log --all --date=raw --format=%ad    4 dates, exit 0
//!    port                                        the same 4 dates, exit 0
//!    ```
//!
//! 3. **`format-patch --date=<mode>` exits 128.** Stock accepts it (and, as it
//!    happens, still renders the mail header in RFC form, which is itself worth
//!    pinning); the port refuses the argument outright.
//!
//!    ```text
//!    port   fatal: unrecognized argument: --date=iso     exit 128
//!    ```
//!
//! 4. **`log --since-as-filter=<epoch>` exits 1** — `zvcs: log: unsupported flag
//!    "--since-as-filter=@1650000000"`. The flag appears nowhere else in the
//!    corpus.
//!
//! Not re-filed, but carried by one row below so the corpus keeps measuring it:
//! `discovery_env.rs`'s finding that **`receive-pack --advertise-refs` drops the
//! `.have` advertisement under a namespace**. On a *non-empty* namespace that
//! finding sharpens — the port now gets the ref lines right and still loses the
//! three `.have` lines, which the empty-namespace row could not distinguish from
//! "advertises nothing":
//!
//! ```text
//! stock  <tip> .have <caps>  /  <root> .have  /  refs/heads/inside  /  refs/tags/inside-tag
//! port   <tip> refs/heads/inside <caps>       /  refs/tags/inside-tag
//! ```
//!
//! Un-namespaced `receive-pack --advertise-refs` on the same shape matches
//! byte-for-byte, which is why the row is paired with its baseline below.
//!
//! # Not measurable, and why
//!
//! * **`--since` / `--until` / `--after` / `--before` / `--max-age` / `--min-age`
//!   do not become non-degenerate on this shape.** `revision.c` compares against
//!   `commit->date`, which `parse_commit_buffer` takes from the **committer**
//!   line, and every committer date here is still `1700000000`. Verified:
//!   `--since=@1600000325` selects all four commits and `--until=@1600000325`
//!   selects none, which is the same all-or-nothing answer the pinned shapes
//!   give. What the shape *does* buy is a discriminator rather than a range: an
//!   implementation that filtered on the **author** date would answer
//!   `--since=@1650000000` with one commit where stock answers four, and
//!   `--until=@1650000000` with three where stock answers none. That is what
//!   [`age_window`]'s epochs are chosen for, and it is the only reason those rows
//!   exist.
//! * **`--date=relative`, `%ar` and `%cr`** read `time(NULL)`. Absent, as in
//!   `log_format.rs`. `--date=human` is present for that module's stated reason:
//!   `show_date_human()` drops the year only inside the current year, and both
//!   2020 and 2023 are already past, so it has settled.
//! * **A relative `--since` spelling** (`2.weeks.ago`, `yesterday`) is a clock
//!   read and appears nowhere here. Every date token below is an epoch or an
//!   absolute `strftime` template.
//! * **`--sort=taggerdate` and `--sort=creatordate` remain ties** — see the tag
//!   caveat above.
//! * **A namespace with its own `HEAD`.** `refs/namespaces/ns/HEAD` would restore
//!   the `symref=` capability and the `HEAD` line under a namespace, and it is
//!   the one namespaced ref [`Shape::Namespaced`] does not build. Creating it is
//!   a second step and so belongs to `sequences.rs`, not to a one-argv case.
//! * **A namespaced *fetch* or *push*.** `GIT_NAMESPACE` is honoured by the
//!   serving side, so measuring it end-to-end needs a client and a server in one
//!   invocation; `fetch_clone.rs` and `transport_local.rs` own that territory and
//!   neither runs on this shape.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    ordering_axis(out);
    ordering_crosses(out);
    front_ends(out);
    date_rendering(out);
    date_front_end_split(out);
    age_window(out);
    ref_sorting(out);
    namespace_local_reads(out);
    namespace_served_reads(out);
    namespace_ref_paths(out);
    namespace_integrity(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

// ---------------------------------------------------------------------------
// 1. The ordering axis, on the shape where the author date moves
// ---------------------------------------------------------------------------

/// The four orders, `--reverse`, and the last-one-wins rule between two orders.
///
/// What a port gets wrong without these: aliasing `--author-date-order` to
/// `--date-order`, or swapping the two. Both were free marks in every other
/// shape because the two sort keys held one value; here they hold different
/// values in three of the four commits, and the two flags produce different
/// lists (see the table in the module header).
///
/// `--reverse` is crossed with each order rather than measured alone, because it
/// is applied *after* the sort: a port that reverses the emission instead of the
/// sorted list gets the default right and `--author-date-order` wrong.
///
/// The two-order rows at the end are the last-wins rule. `revision.c` stores one
/// `sort_order` and each flag overwrites it, so the trailing flag decides; a port
/// that OR-ed the two into a flag set, or that let the first win, is caught by
/// the pair and by nothing else in the corpus.
fn ordering_axis(out: &mut Vec<Case>) {
    each(
        Shape::SkewedDates,
        "log",
        &[
            &["log", "--oneline", "--all"],
            &["log", "--oneline", "--all", "--topo-order"],
            &["log", "--oneline", "--all", "--date-order"],
            &["log", "--oneline", "--all", "--author-date-order"],
            &["log", "--oneline", "--all", "--reverse"],
            &["log", "--oneline", "--all", "--author-date-order", "--reverse"],
            &["log", "--oneline", "--all", "--date-order", "--reverse"],
            &["log", "--oneline", "--all", "--topo-order", "--reverse"],
            // Last-one-wins, in both directions.
            &["log", "--oneline", "--all", "--date-order", "--author-date-order"],
            &["log", "--oneline", "--all", "--author-date-order", "--date-order"],
            &["log", "--oneline", "--all", "--author-date-order", "--topo-order"],
        ],
        out,
    );
    each(
        Shape::SkewedDates,
        "rev-list",
        &[
            &["rev-list", "--all"],
            &["rev-list", "--all", "--topo-order"],
            &["rev-list", "--all", "--date-order"],
            &["rev-list", "--all", "--author-date-order"],
            &["rev-list", "--all", "--reverse"],
            &["rev-list", "--all", "--author-date-order", "--reverse"],
            &["rev-list", "--all", "--date-order", "--author-date-order"],
            &["rev-list", "--all", "--author-date-order", "--date-order"],
        ],
        out,
    );
    // The same orders on a walk that has no second tip, where all four collapse
    // onto one list. Without this row a port could special-case `--all` and
    // still pass every row above.
    each(
        Shape::SkewedDates,
        "log",
        &[
            &["log", "--oneline", "--author-date-order"],
            &["log", "--oneline", "--date-order"],
            &["log", "--oneline", "sd-side"],
            &["log", "--oneline", "--author-date-order", "sd-side"],
        ],
        out,
    );
}

/// The orders crossed with the flags that change what the walk emits around
/// them: `--graph`, `--first-parent`, `--boundary`, `--max-count`, `--skip`.
///
/// What a port gets wrong without these: applying the sort to the wrong list.
/// `--max-count` and `--skip` are applied to the *emitted* sequence, so they
/// interact with the order; `--boundary` appends its `-` rows after the walk, so
/// a port that sorted boundary commits in with the rest reorders them; `--graph`
/// consumes the ordered list to lay out lanes, so a wrong order becomes a wrong
/// picture. Every row pairs `--author-date-order` with `--date-order` or the
/// default, because a single row cannot say which of the two the port used.
///
/// `rev-list --skip=` is deliberately absent: `traversal_order.rs` finding 2
/// records that it exits 129 on the port, and re-spelling it here would file the
/// same defect twice. `log --skip=` works on the port and is used instead.
fn ordering_crosses(out: &mut Vec<Case>) {
    each(
        Shape::SkewedDates,
        "log",
        &[
            &["log", "--graph", "--oneline", "--all", "--author-date-order"],
            &["log", "--graph", "--oneline", "--all", "--date-order"],
            &["log", "--graph", "--oneline", "--all", "--topo-order"],
            &["log", "--graph", "--oneline", "--all", "--reverse"],
            &["log", "--graph", "--oneline", "--all", "--author-date-order", "--first-parent"],
            &["log", "--oneline", "--all", "--author-date-order", "--first-parent"],
            &["log", "--oneline", "--all", "--date-order", "--first-parent"],
            &["log", "--oneline", "--author-date-order", "--boundary", "sd-side..main"],
            &["log", "--oneline", "--date-order", "--boundary", "sd-side..main"],
            &["log", "--oneline", "--author-date-order", "--boundary", "sd-side...main"],
            &["log", "--oneline", "--all", "--author-date-order", "--max-count=2"],
            &["log", "--oneline", "--all", "--date-order", "--max-count=2"],
            &["log", "--oneline", "--all", "--author-date-order", "--skip=1"],
            &["log", "--oneline", "--all", "--date-order", "--skip=1"],
            &["log", "--oneline", "--all", "--author-date-order", "--max-count=1", "--skip=1"],
            &["log", "--oneline", "--all", "--author-date-order", "-n", "2"],
        ],
        out,
    );
    each(
        Shape::SkewedDates,
        "rev-list",
        &[
            &["rev-list", "--all", "--author-date-order", "--first-parent"],
            &["rev-list", "--all", "--author-date-order", "--boundary"],
            &["rev-list", "--all", "--date-order", "--boundary"],
            &["rev-list", "--all", "--author-date-order", "--reverse", "--boundary"],
            &["rev-list", "--all", "--author-date-order", "--max-count=3"],
            &["rev-list", "--all", "--author-date-order", "--count"],
        ],
        out,
    );
}

/// The other three front ends over the same walk: `shortlog`, `format-patch`,
/// `show-branch`.
///
/// What a port gets wrong without these: an option table that knows
/// `--author-date-order` under one binary name and not under another. That is
/// the defect shape `traversal_order.rs` found seven times, and its whole
/// ordering group predates the skewed shape — so under the flag that finally
/// distinguishes the two date orders, each front end is unmeasured.
///
/// **This group holds the sharpest finding in the file.** `shortlog --topo-order`
/// on the port emits its own `--author-date-order` list, while `log` and
/// `rev-list` order the same walk correctly — one front end's option table
/// disagreeing with two others over one walk. No shape `traversal_order.rs`
/// runs on can see it, because there the two sort keys hold one value and the
/// wrong mapping still produces the right list. Transcript in the module header.
///
/// **`show-branch --topo-order` and `--date-order` are a tie here**, and stay one:
/// `builtin/show-branch.c` sorts its commit list by `commit->date`, the committer
/// date, and offers no author-date spelling at all. The three rows are kept
/// because agreeing on a tie is still a contract — a port that reordered
/// `show-branch` under an ordering flag fails them — but they are not a
/// separation and are not counted as one.
fn front_ends(out: &mut Vec<Case>) {
    each(
        Shape::SkewedDates,
        "shortlog",
        &[
            &["shortlog", "--all", "--author-date-order"],
            &["shortlog", "--all", "--date-order"],
            &["shortlog", "--all", "--topo-order"],
            &["shortlog", "--all", "--author-date-order", "--reverse"],
            &["shortlog", "--all", "--format=%h|%ad", "--date=unix"],
        ],
        out,
    );
    each(
        Shape::SkewedDates,
        "format-patch",
        &[
            &["format-patch", "--stdout", "--author-date-order", "--root", "sd-side"],
            &["format-patch", "--stdout", "--date-order", "--root", "sd-side"],
            &["format-patch", "--stdout", "--reverse", "--root", "sd-side"],
            &["format-patch", "--stdout", "--author-date-order", "main~2..main"],
            &["format-patch", "--stdout", "--reverse", "main~2..main"],
        ],
        out,
    );
    each(
        Shape::SkewedDates,
        "show-branch",
        &[
            &["show-branch", "--all"],
            &["show-branch", "--all", "--date-order"],
            &["show-branch", "--all", "--topo-order"],
            &["show-branch", "--all", "--sha1-name"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// 2. Date rendering, where the two fields finally hold different bytes
// ---------------------------------------------------------------------------

/// The `--date=` renderers and the date atoms, asked where `%ad != %cd`.
///
/// What a port gets wrong without these: rendering the committer date for `%ad`,
/// or the author date for `%cd`. `log_format.rs` owns this axis on
/// [`Shape::Branched`], where both fields hold `1700000000` — so all eight of its
/// mode rows pass against a port that reads the wrong field, and the whole
/// author/committer distinction is a free mark. On [`Shape::SkewedDates`] the
/// three non-root commits carry `1600000300`, `1600000350` and `1600000400` as
/// author dates against a pinned `1700000000` committer date, so every row here
/// fails such a port on its first line.
///
/// The argv is deliberately not `log_format.rs`'s: it walks `--all` rather than
/// `-1`, so the mode is exercised over four different values instead of one.
///
/// `relative` is absent (clock). `human` is present: `show_date_human()` drops the
/// year only for dates inside the current year, and both 2020 and 2023 are past.
fn date_rendering(out: &mut Vec<Case>) {
    for mode in ["default", "local", "iso", "iso-strict", "rfc", "short", "raw", "human", "unix"] {
        let arg = format!("--date={mode}");
        out.push(Case::new(
            "log",
            &["log", "--all", &arg, "--format=%h|%ad|%cd"],
            Shape::SkewedDates,
        ));
    }
    each(
        Shape::SkewedDates,
        "log",
        &[
            &["log", "--all", "--date=format:%Y-%m-%dT%H:%M:%S", "--format=%h|%ad|%cd"],
            &["log", "--all", "--date=format-local:%s", "--format=%h|%ad|%cd"],
            // The atoms that ignore `--date=` and carry their own renderer. Each
            // pair is author-then-committer, so one row shows both fields.
            &["log", "--all", "--format=%h|%at|%ct"],
            &["log", "--all", "--format=%h|%aI|%cI"],
            &["log", "--all", "--format=%h|%aD|%cD"],
            &["log", "--all", "--format=%h|%as|%cs"],
            &["log", "--all", "--format=%h|%ai|%ci"],
            // The built-in formats that print both dates in prose. `fuller` is the
            // only one that shows `AuthorDate:` and `CommitDate:` as separate
            // lines, and until this shape the two lines were identical.
            &["log", "--all", "--pretty=fuller"],
            &["log", "--all", "--pretty=medium"],
            &["log", "--all", "--date=iso", "--pretty=fuller"],
            &["log", "--all", "--date=unix", "--pretty=fuller"],
        ],
        out,
    );
    // `log.date` reaching the same renderer from configuration rather than argv.
    out.push(
        Case::new("log", &["log", "--all", "--format=%h|%ad|%cd"], Shape::SkewedDates)
            .with_config(&[("log.date", "unix")]),
    );
    // `show`'s own emitter over the same commit, from both tips.
    each(
        Shape::SkewedDates,
        "show",
        &[
            &["show", "-s", "--date=unix", "--format=%h|%ad|%cd", "main"],
            &["show", "-s", "--date=unix", "--format=%h|%ad|%cd", "sd-side"],
            &["show", "-s", "--pretty=fuller", "sd-side"],
        ],
        out,
    );
}

/// The renderer flag asked of every front end, which is where the port splits.
///
/// One walk, four binaries, one `--date=`. `log` and `shortlog` accept it;
/// `rev-list` exits 129 and `format-patch` exits 128 (transcripts in the module
/// header). The rows are paired with their no-`--date=` baselines so the failure
/// is attributable to the flag rather than to the invocation.
///
/// This is the same defect *shape* as `traversal_order.rs` finding 2 — a walk
/// option missing from one front end's table — on a flag that file never passes
/// and that appears nowhere else in the corpus (`rev-list` and `--date=` never
/// co-occur in `corpus/`, checked by grep).
fn date_front_end_split(out: &mut Vec<Case>) {
    each(
        Shape::SkewedDates,
        "rev-list",
        &[
            &["rev-list", "--all", "--date=raw", "--pretty=format:%h|%ad|%cd"],
            &["rev-list", "--all", "--date=unix", "--format=%ad"],
            // The baselines: the same two argvs with the renderer removed.
            &["rev-list", "--all", "--pretty=format:%h|%ad|%cd"],
            &["rev-list", "--all", "--format=%ad"],
        ],
        out,
    );
    each(
        Shape::SkewedDates,
        "log",
        &[
            &["log", "--all", "--date=unix", "--format=%ad"],
            &["log", "--all", "--date=raw", "--pretty=format:%h|%ad|%cd"],
        ],
        out,
    );
    each(
        Shape::SkewedDates,
        "format-patch",
        &[
            &["format-patch", "--stdout", "--date=iso", "--root", "-1", "main"],
            &["format-patch", "--stdout", "--date=unix", "--root", "-1", "main"],
            &["format-patch", "--stdout", "--root", "-1", "main"],
        ],
        out,
    );
    each(
        Shape::SkewedDates,
        "shortlog",
        &[
            &["shortlog", "--all", "--date=unix", "--format=%ad"],
            &["shortlog", "--all", "--format=%ad"],
        ],
        out,
    );
}

/// The age window, as a test of **which field the filter reads**.
///
/// `revision.c` compares `commit->date`, which `parse_commit_buffer` fills from
/// the committer line, so on this shape every one of these selects all four
/// commits or none — the same all-or-nothing answer the pinned shapes give, and
/// therefore *not* a newly non-degenerate range. What is new is the
/// discrimination: the epochs below sit **between** the author dates
/// (`1600000300`–`1600000400`) and the committer date (`1700000000`), so an
/// implementation that filtered on the author date answers differently on every
/// row. Stock, measured:
///
/// ```text
/// --since=@1650000000    4 commits     author-date filter would give 1
/// --until=@1650000000    0 commits     author-date filter would give 3
/// --after=@1600000325    4 commits     author-date filter would give 3
/// --before=@1600000325   0 commits     author-date filter would give 1
/// ```
///
/// `traversal_order.rs` owns `--min-age`/`--max-age` at `1699999999` and
/// `1700000001`, which straddle the *pinned* stamp and cannot ask this question;
/// `plumbing_refs.rs` owns the two ISO-spelled `rev-list --since=2000-01-01` /
/// `--until=2000-01-01` rows on `Branched`. Every value here is an epoch.
///
/// `--since-as-filter` is the fourth spelling of the same predicate and the only
/// one that keeps walking past a commit it rejects. It exits 1 on the port and
/// appears nowhere else in the corpus.
fn age_window(out: &mut Vec<Case>) {
    each(
        Shape::SkewedDates,
        "log",
        &[
            &["log", "--oneline", "--all", "--since=@1650000000"],
            &["log", "--oneline", "--all", "--until=@1650000000"],
            &["log", "--oneline", "--all", "--after=@1600000325"],
            &["log", "--oneline", "--all", "--before=@1600000325"],
            &["log", "--oneline", "--all", "--since=@1650000000", "--author-date-order"],
            &["log", "--oneline", "--all", "--since-as-filter=@1650000000"],
            &["log", "--oneline", "--all", "--since-as-filter=@1600000325"],
        ],
        out,
    );
    each(
        Shape::SkewedDates,
        "rev-list",
        &[
            &["rev-list", "--all", "--max-age=1650000000"],
            &["rev-list", "--all", "--min-age=1650000000"],
            &["rev-list", "--all", "--max-age=1600000325"],
            &["rev-list", "--all", "--min-age=1600000325"],
            &["rev-list", "--all", "--since=@1650000000", "--count"],
            &["rev-list", "--all", "--until=@1650000000", "--count"],
        ],
        out,
    );
}

/// `for-each-ref`'s date sort keys, where two of them finally disagree.
///
/// What a port gets wrong without these: sorting `authordate` by the committer
/// date. Every shape but this one gives all four keys — `authordate`,
/// `committerdate`, `taggerdate`, `creatordate` — the same value on every ref, so
/// the sort was a refname tiebreak dressed as a date sort and a port that ignored
/// the key entirely scored full marks. Stock here:
///
/// ```text
/// --sort=-authordate      refs/heads/sd-side  refs/heads/main     refs/tags/sd-tag
/// --sort=-committerdate   refs/heads/main     refs/heads/sd-side  refs/tags/sd-tag
/// ```
///
/// The `taggerdate` and `creatordate` rows are the negative control and are
/// **expected to stay ties**: the fixture's annotated tag takes its tagger date
/// from the pinned committer ident, and a branch's `creatordate` is its
/// committerdate. A port that answered every date atom with the author date
/// passes the two rows above and fails these. See the module header's tag caveat;
/// `tag_family.rs` reached the same conclusion from the other direction.
fn ref_sorting(out: &mut Vec<Case>) {
    each(
        Shape::SkewedDates,
        "for-each-ref",
        &[
            &["for-each-ref", "--sort=authordate", "--format=%(refname)"],
            &["for-each-ref", "--sort=-authordate", "--format=%(refname)"],
            &["for-each-ref", "--sort=committerdate", "--format=%(refname)"],
            &["for-each-ref", "--sort=-committerdate", "--format=%(refname)"],
            &["for-each-ref", "--sort=creatordate", "--format=%(refname)"],
            &["for-each-ref", "--sort=-creatordate", "--format=%(refname)"],
            &["for-each-ref", "--sort=taggerdate", "--format=%(refname)"],
            // Two keys, so the second is the tiebreak for the first. With the
            // author dates distinct the primary key decides outright, which is
            // what separates this from a refname sort wearing a date's name.
            &["for-each-ref", "--sort=authordate", "--sort=refname", "--format=%(refname)"],
            // The values themselves, side by side.
            &[
                "for-each-ref",
                "--format=%(refname)|%(authordate:unix)|%(committerdate:unix)|%(creatordate:unix)",
            ],
            &[
                "for-each-ref",
                "--format=%(authordate:iso)|%(authordate:short)|%(authordate:raw)",
                "refs/heads",
            ],
            &[
                "for-each-ref",
                "--format=%(refname:short)|%(taggerdate:unix)|%(creatordate:unix)|%(authordate:unix)",
                "refs/tags",
            ],
        ],
        out,
    );
    // The two porcelain front ends over the same sort.
    each(
        Shape::SkewedDates,
        "branch",
        &[
            &["branch", "--list", "--sort=-authordate"],
            &["branch", "--list", "--sort=-committerdate"],
            &[
                "branch",
                "--list",
                "--format=%(refname:short)|%(authordate:unix)|%(committerdate:unix)",
            ],
        ],
        out,
    );
    each(
        Shape::SkewedDates,
        "tag",
        &[
            &["tag", "--list", "--sort=-authordate"],
            &["tag", "--list", "--sort=-taggerdate"],
            &["tag", "--list", "--format=%(refname:short)|%(taggerdate:unix)|%(creatordate:unix)"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// 3. Namespaces, on the shape whose namespace is not empty
// ---------------------------------------------------------------------------

/// `GIT_NAMESPACE=ns`, as an environment pair. Additive: `env::is_pinned` does
/// not claim the variable (`env.rs`'s `repository_selection_vars_are_not_pinned`
/// lists it explicitly), so setting it adds a fact both sides see rather than
/// replacing a determinism guarantee.
const NS: &[(&str, &str)] = &[("GIT_NAMESPACE", "ns")];
/// The second namespace, which holds one ref and a different one.
const NS_OTHER: &[(&str, &str)] = &[("GIT_NAMESPACE", "other")];
/// A namespace with nothing under it, in a repository that has two that do. The
/// negative control for every row that finds something.
const NS_EMPTY: &[(&str, &str)] = &[("GIT_NAMESPACE", "no-such-ns")];

/// Push one case per argv against [`Shape::Namespaced`], carrying `env`.
fn with_env(cmd: &'static str, argvs: &[&[&str]], env: &[(&str, &str)], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, Shape::Namespaced).with_env(env));
    }
}

/// The local ref readers under a namespace that is not empty.
///
/// **These pin a negative, and the negative is the finding.** `discovery_env.rs`
/// measured eight local verbs under `GIT_NAMESPACE` on [`Shape::Branched`], found
/// that none of them changed a byte, and correctly declined to make cases of them
/// — "a variable that changes no byte is a case that can never fail" — while
/// recording that the namespace being *empty* left the result ambiguous. It is no
/// longer ambiguous. With two refs under `refs/namespaces/ns/` and one under
/// `refs/namespaces/other/`, re-measured on stock 2.55.0 and on the port:
///
/// ```text
/// GIT_NAMESPACE=ns for-each-ref    all five refs, under their full refs/namespaces/… names
/// GIT_NAMESPACE=ns branch --list   main, ns-outside          (not `inside`)
/// GIT_NAMESPACE=ns tag --list      empty                     (not `inside-tag`)
/// GIT_NAMESPACE=ns rev-parse --all all five ids
/// GIT_NAMESPACE=ns log --all       two commits, undecorated by any namespaced ref
/// GIT_NAMESPACE=ns update-ref refs/heads/created HEAD
///                                  writes .git/refs/heads/created, not
///                                  refs/namespaces/ns/refs/heads/created
/// ```
///
/// That is git's design — a namespace is a *serving* concept, applied in
/// `refs.c:expand_namespace` on the paths the transport verbs build — and every
/// row now has content to be wrong about. A port that "helpfully" scoped
/// `for-each-ref` or `branch --list` to the namespace, or that namespaced the
/// write, fails here and passed everywhere before.
///
/// The `--namespace=` option spelling is asked of the same verbs because it
/// reaches the setting through `git.c:handle_options` rather than through the
/// environment, and a port could honour one and not the other.
fn namespace_local_reads(out: &mut Vec<Case>) {
    let reads: &[(&'static str, &[&str])] = &[
        ("for-each-ref", &["for-each-ref"]),
        ("show-ref", &["show-ref"]),
        ("branch", &["branch", "--list"]),
        ("tag", &["tag", "--list"]),
        ("rev-parse", &["rev-parse", "--all"]),
        ("log", &["log", "--oneline", "--all", "--decorate"]),
        ("rev-list", &["rev-list", "--all"]),
        ("symbolic-ref", &["symbolic-ref", "HEAD"]),
    ];
    for (cmd, args) in reads {
        // Unset, so every namespaced row has its own baseline in the same report.
        out.push(Case::new(cmd, args, Shape::Namespaced));
        out.push(Case::new(cmd, args, Shape::Namespaced).with_env(NS));
        out.push(
            Case::new(cmd, args, Shape::Namespaced).with_globals(&[&["--namespace=ns"]]),
        );
    }
    // A second namespace on the two rawest listings, so a port that hard-coded
    // one name is caught.
    with_env("for-each-ref", &[&["for-each-ref"]], NS_OTHER, out);
    with_env("show-ref", &[&["show-ref"]], NS_OTHER, out);

    // The write. Whether a namespace redirects a local ref update is the one
    // question here whose answer lands in the state digest rather than on stdout.
    out.push(
        Case::new("update-ref", &["update-ref", "refs/heads/created", "HEAD"], Shape::Namespaced)
            .with_env(NS),
    );
    out.push(
        Case::new("update-ref", &["update-ref", "refs/heads/created", "HEAD"], Shape::Namespaced)
            .with_globals(&[&["--namespace=ns"]]),
    );
    // Deleting a namespaced ref by its un-namespaced name. Exits 0 on both
    // sides — `update-ref -d` with no old value is a no-op on a ref that is not
    // there — so the whole content of the case is the state digest, which must
    // still hold `refs/namespaces/ns/refs/heads/inside` afterwards.
    out.push(
        Case::strict("update-ref", &["update-ref", "-d", "refs/heads/inside"], Shape::Namespaced)
            .with_env(NS),
    );
    // `symbolic-ref` writing under a namespace, which would be the way a
    // namespace acquired its own HEAD if the variable applied locally.
    out.push(
        Case::new(
            "symbolic-ref",
            &["symbolic-ref", "refs/heads/sym", "refs/heads/main"],
            Shape::Namespaced,
        )
        .with_env(NS),
    );
    // A namespace that exists nowhere, against a repository that has two: still
    // no change to any local read.
    with_env("for-each-ref", &[&["for-each-ref"]], NS_EMPTY, out);
    with_env("branch", &[&["branch", "--list"]], NS_EMPTY, out);
}

/// The verbs that *do* honour a namespace: `ls-remote` and the two advertisers.
///
/// This is the group the empty namespace could not reach. Stock 2.55.0:
///
/// ```text
/// ls-remote .                     HEAD, refs/heads/main, refs/heads/ns-outside,
///                                 refs/namespaces/ns/refs/heads/inside,
///                                 refs/namespaces/ns/refs/tags/inside-tag,
///                                 refs/namespaces/other/refs/heads/elsewhere
/// GIT_NAMESPACE=ns    ls-remote . refs/heads/inside, refs/tags/inside-tag   (no HEAD)
/// GIT_NAMESPACE=other ls-remote . refs/heads/elsewhere
/// GIT_NAMESPACE=no-such-ns        (nothing, exit 0)
/// ```
///
/// Two facts only a non-empty namespace can show, and both are what the rows are
/// for: the `refs/namespaces/<n>/` prefix is **stripped** on the wire, so the
/// client sees ordinary ref names; and `HEAD` **disappears**, because
/// [`Shape::Namespaced`] builds no `refs/namespaces/ns/HEAD`. The same two show up
/// one layer down in `upload-pack --advertise-refs`, where the
/// `symref=HEAD:refs/heads/main` capability is absent from the namespaced
/// advertisement and present in the baseline — a difference an empty
/// advertisement (which carries no capability line beyond `capabilities^{}`)
/// cannot express.
///
/// `env_layer.rs` and `globals_layer.rs` each set `ns` on `ls-remote .`, on
/// [`Shape::Branched`] and [`Shape::TagChain`]. Both namespaces are empty there,
/// so both cases are byte-identical to their baselines and can never fail; the
/// shape is part of `Case::id`, so these are separate cases and are the first
/// ones that can.
///
/// `discovery_env.rs` owns the *refusals* (`GIT_NAMESPACE=..`, `x.lock`) and the
/// empty value on [`Shape::Branched`]. Neither is repeated: a bad namespace dies
/// in `expand_namespace` before any ref is read, so having refs to read changes
/// nothing about it.
///
/// **The `receive-pack` row carries a defect that is already filed.**
/// `discovery_env.rs` found that the port drops the `.have` advertisement under a
/// namespace. It still does, and on a non-empty namespace the finding sharpens
/// rather than repeats — the port now emits the two namespaced ref lines
/// correctly and still loses the three `.have` lines, which the empty-namespace
/// row could not distinguish from "advertises nothing at all". The un-namespaced
/// baseline beside it matches byte-for-byte, which is what localises the defect to
/// the namespaced path.
fn namespace_served_reads(out: &mut Vec<Case>) {
    let ls: &[&[&str]] = &[
        &["ls-remote", "."],
        &["ls-remote", "--heads", "."],
        &["ls-remote", "--tags", "."],
        &["ls-remote", "--symref", "."],
        &["ls-remote", ".", "refs/heads/*"],
    ];
    for args in ls {
        out.push(Case::new("ls-remote", args, Shape::Namespaced));
        out.push(Case::new("ls-remote", args, Shape::Namespaced).with_env(NS));
    }
    with_env("ls-remote", &[&["ls-remote", "."]], NS_OTHER, out);
    with_env("ls-remote", &[&["ls-remote", "."]], NS_EMPTY, out);
    out.push(
        Case::new("ls-remote", &["ls-remote", "."], Shape::Namespaced)
            .with_globals(&[&["--namespace=ns"]]),
    );
    out.push(
        Case::new("ls-remote", &["ls-remote", "."], Shape::Namespaced)
            .with_globals(&[&["--namespace=other"]]),
    );

    let advertise: &[&str] = &["--advertise-refs", "."];
    for cmd in ["upload-pack", "receive-pack"] {
        let args: Vec<&str> = std::iter::once(cmd).chain(advertise.iter().copied()).collect();
        out.push(Case::new(cmd, &args, Shape::Namespaced));
        out.push(Case::new(cmd, &args, Shape::Namespaced).with_env(NS));
    }
    with_env(
        "upload-pack",
        &[&["upload-pack", "--advertise-refs", "."]],
        NS_OTHER,
        out,
    );
    with_env(
        "upload-pack",
        &[&["upload-pack", "--advertise-refs", "."]],
        NS_EMPTY,
        out,
    );
    out.push(
        Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::Namespaced)
            .with_globals(&[&["--namespace=ns"]]),
    );
}

/// The namespaced refs read as **ordinary refs**, with no namespace set at all.
///
/// No shape before this one had anything under `refs/namespaces/`, so a whole
/// region of the ref namespace was unreachable by name. These rows need no
/// environment: they ask whether a five-component ref path is listed, matched by a
/// pattern, verified, resolved and walked like any other.
///
/// `naming_ancestry.rs`'s `name_rev_namespaces` group covers refs outside
/// `heads`/`tags`/`remotes` — `refs/notes/commits`, a bare `refs/top` — and
/// `ref_storage.rs` covers nine refs across four namespaces. Neither reaches a
/// path this deep, and the `refname` modifiers have no other fixture where they
/// disagree this widely. Stock, on `refs/namespaces/ns/refs/heads/inside`:
/// `:short` gives `namespaces/ns/refs/heads/inside`, `:strip=2` gives
/// `ns/refs/heads/inside`, and `:lstrip=-1` gives `inside` — four different
/// answers from one ref, where a two-component ref makes several of them agree.
fn namespace_ref_paths(out: &mut Vec<Case>) {
    each(
        Shape::Namespaced,
        "for-each-ref",
        &[
            &["for-each-ref", "refs/namespaces/"],
            &["for-each-ref", "refs/namespaces/ns/*"],
            &[
                "for-each-ref",
                "--format=%(refname)|%(refname:short)|%(refname:strip=2)|%(refname:lstrip=-1)",
                "refs/namespaces/",
            ],
            &["for-each-ref", "--count=2", "--format=%(refname)", "refs/namespaces/"],
        ],
        out,
    );
    each(
        Shape::Namespaced,
        "show-ref",
        &[
            &["show-ref", "--verify", "refs/namespaces/ns/refs/heads/inside"],
            &["show-ref", "--verify", "refs/namespaces/other/refs/heads/elsewhere"],
            &["show-ref", "inside"],
            &["show-ref", "--heads"],
            &["show-ref", "--tags"],
        ],
        out,
    );
    each(
        Shape::Namespaced,
        "rev-parse",
        &[
            &["rev-parse", "refs/namespaces/ns/refs/heads/inside"],
            &["rev-parse", "--symbolic-full-name", "refs/namespaces/other/refs/heads/elsewhere"],
            &["rev-parse", "--abbrev-ref", "refs/namespaces/ns/refs/tags/inside-tag"],
        ],
        out,
    );
    each(
        Shape::Namespaced,
        "log",
        &[
            &["log", "--oneline", "refs/namespaces/ns/refs/heads/inside"],
            &["log", "--oneline", "--all", "--decorate=full"],
        ],
        out,
    );
    each(
        Shape::Namespaced,
        "rev-list",
        &[
            &["rev-list", "--all", "--count"],
            &["rev-list", "--count", "refs/namespaces/ns/refs/heads/inside"],
        ],
        out,
    );
}

/// The whole-repository verbs, run under a namespace and without one.
///
/// `fsck` walks every ref to seed reachability, `pack-refs` rewrites all of them
/// into one file, and `count-objects` reports what that leaves. None of the three
/// is namespace-aware, and the point of the pairs is that they stay that way: a
/// port that let `expand_namespace` reach the reachability roots would report
/// dangling objects under `GIT_NAMESPACE=ns`, and a port that namespaced
/// `pack-refs` would write the wrong `packed-refs`. The state digest is what
/// carries the `pack-refs` answer, since its stdout is empty.
fn namespace_integrity(out: &mut Vec<Case>) {
    for args in [
        &["fsck"][..],
        &["fsck", "--unreachable"][..],
        &["fsck", "--no-progress"][..],
    ] {
        out.push(Case::new("fsck", args, Shape::Namespaced));
        out.push(Case::new("fsck", args, Shape::Namespaced).with_env(NS));
    }
    out.push(Case::new("pack-refs", &["pack-refs", "--all"], Shape::Namespaced));
    out.push(Case::new("pack-refs", &["pack-refs", "--all"], Shape::Namespaced).with_env(NS));
    out.push(Case::new("gc", &["gc", "--quiet"], Shape::Namespaced));
    out.push(Case::new("gc", &["gc", "--quiet"], Shape::Namespaced).with_env(NS));
    out.push(Case::new("count-objects", &["count-objects", "-v"], Shape::Namespaced).with_env(NS));
}
