//! Differential corpus cases for `gitrevisions(7)` — the rev-spec grammar
//! itself, reached through every verb that resolves one.
//!
//! This is one parser behind forty doors. `revision.c:handle_revision_arg` and
//! `object-name.c:get_oid_with_context` (the file `gitrevisions(7)` describes
//! and that older trees called `sha1-name.c`) are entered by `rev-parse`,
//! `rev-list`, `log`, `show`, `diff`, `cat-file`, `ls-tree`, `describe`,
//! `name-rev`, `merge-base`, `archive`, `checkout`, `reset`, `cherry-pick`,
//! `format-patch` and every other verb that takes a revision. Before this
//! module the corpus measured that parser mostly by accident: `fuzz.rs`'s
//! `REVS` pool samples it at random against random verbs, and each
//! per-verb module writes the two or three spellings its author needed. Nothing
//! systematically asked whether one spelling resolves the *same way* through
//! ten different callers.
//!
//! # Territory
//!
//! * **`fuzz.rs`'s `REVS`** is a random pool crossed with random flags. It
//!   establishes that a spelling does not crash; it cannot establish that
//!   `HEAD^2` names the same commit to `log` as to `merge-base`, because it
//!   never draws the pair. This module deliberately does not import it — a
//!   corpus that shares a pool with the generator moves whenever the generator
//!   is retuned — and it does not contradict it either: every fact `REVS`'
//!   comments record (`feature` resolving to the tag under the round trip,
//!   `refs/heads/dangling` succeeding on `Damaged`, no four-character
//!   abbreviation being ambiguous in any fixture) is taken as given here.
//! * **`history_query.rs`** owns `merge-base`, `name-rev`, `cherry`,
//!   `range-diff` and `rev-list`'s *selection* flags. Where a case below names
//!   one of those verbs, the flag set is the plainest one that will print an
//!   answer and the rev-spec is the subject.
//! * **`plumbing_refs.rs`** owns `show-ref`, `for-each-ref` and `reflog` as
//!   verbs, including `reflog delete HEAD@{1}`. What is added here is `@{n}`
//!   as an *operand of something else*.
//! * **`shape_reach.rs`** and **`fixture_gaps2.rs`** own the new shapes'
//!   first-contact cases; `fixture_gaps2.rs`'s `tag_chain` block already pins
//!   eleven `rev-parse` peels and ten `cat-file` reads on [`Shape::TagChain`].
//!   None of those is repeated. What is added is the same peels through
//!   `ls-tree`, `archive`, `merge-base`, `describe`, `log`, `diff`,
//!   `tag --points-at`, `for-each-ref --points-at`, `reset` and `checkout`,
//!   plus the peel spellings that block leaves out — `^{object}`, `^{tag}` on
//!   two links of the chain, and the peels that cannot succeed.
//! * **`tag_describe.rs`** owns `describe`'s own flag matrix; the `describe`
//!   cases here take a *peeled* or an *abbreviated* operand instead.
//! * **`corpus.rs`'s own `rev-parse` block** holds nine one-liners (`HEAD`,
//!   `HEAD^`, `HEAD~1`, `main`, `v0.1.0`, `v0.2.0^{commit}`, the `--git-dir`
//!   family). Nothing below repeats an argv from it.
//!
//! # Which shape supplies which topology
//!
//! * [`Shape::Octopus`] — the only commit with more than two parents, so it is
//!   the only place `^2`/`^3`/`^4` are three different answers rather than one
//!   answer and two refusals, and the only place `^@` expands to more than two
//!   tips and `^-2` differs from `^-`.
//! * [`Shape::Packed`] — nine commits deep, the only history where `~3`,
//!   `^^^` and `~2^` are reachable and equal. Its reflog is **expired**
//!   (`fixture.rs` runs `reflog expire --expire=all --all`), which makes it the
//!   only shape where `HEAD@{1}` fails with `log for HEAD is empty` rather than
//!   with an out-of-range count — a second refusal text for the same syntax.
//! * [`Shape::CrissCross`] — `HEAD~1` is a merge, so `~n^2` is reachable
//!   (on [`Shape::Merged`] and [`Shape::Octopus`] the merge is `HEAD` itself
//!   and `HEAD~1^2` is a refusal). Two merge bases, so `A...B` is the only
//!   symmetric difference in the corpus whose answer depends on enumerating
//!   both.
//! * [`Shape::Unrelated`] — three roots. `main...alien` has **no** merge base,
//!   so the symmetric difference degenerates to the union of both histories and
//!   `merge-base` exits 1 with no output. Nothing else in the corpus can ask
//!   that question.
//! * [`Shape::Cherry`] — one patch id on both sides of a fork, which is what
//!   makes `cherry`'s `-` marker and `--cherry-mark`'s `=` reachable through a
//!   `A...B` written by hand.
//! * [`Shape::TagChain`] — `outermost` -> `outer` -> `inner` -> commit, plus
//!   `blobtag` on a blob and `treetag` on a tree. Three-deep peeling and
//!   peeling to a type the chain does not end in.
//! * [`Shape::Conflicted`] — the only index with stages. Its conflict is
//!   **add/add**: `conflict.txt` has stages 2 and 3 and *no stage 1*, verified
//!   with `ls-files -s` on stock 2.55.0. So `:2:` and `:3:` resolve while
//!   `:1:` on the same path is a refusal carrying the
//!   `hint: Did you mean ':2:conflict.txt'?` line that
//!   `object-name.c:diagnose_invalid_index_path` writes.
//! * [`Shape::BehindRemote`] — the only shape with a configured upstream, so
//!   `@{u}`, `@{upstream}` and `@{push}` resolve there and refuse everywhere
//!   else.
//! * [`Shape::Branched`] — five HEAD reflog entries and two on `main`, which is
//!   the only fixture deep enough for `HEAD@{1}` and `@{1}` to be *different*
//!   commits. Verified on stock 2.55.0: `HEAD@{1}` is `07e86d1` (the feature
//!   commit) while `@{1}` is `edfab1b` (the root), because `@{n}` indexes the
//!   **current branch's** reflog and not `HEAD`'s. `@{-1}` reaches `07e86d1`
//!   again by a third route — the branch checked out before this one — so the
//!   id alone cannot separate it from `HEAD@{1}` and `--symbolic-full-name`
//!   is asked beside it.
//! * [`Shape::Shallow`] — history that stops. `HEAD~1` resolves and `HEAD~2` is
//!   a refusal, which is a wrong answer no depth-unaware implementation can
//!   avoid.
//! * [`Shape::Promisor`] — blobs genuinely absent. `HEAD~2:hist.txt` resolves
//!   to a blob id *without* reading the blob, which separates name resolution
//!   from object access; reading it would make the case fetch from the peer.
//! * [`Shape::NotesReplace`] — two `refs/replace/*` entries. Every rev that
//!   resolves *through* one is a live question: the port is recorded as writing
//!   the ref and never consulting it.
//! * [`Shape::Damaged`] — `refs/heads/dangling` names an object nothing has, so
//!   the ref resolves and every suffix on it does not.
//! * [`Shape::Symlinks`] — the only shape that tracks a zero-byte file, so the
//!   empty blob is the one object in the corpus whose id is a constant of the
//!   hash function *and* present in a store.
//!
//! # Ids and abbreviations
//!
//! Both sides of a case are copies of one prebuilt template, so every id the
//! fixture contains is identical between them; the risk a literal id carries is
//! staleness against a future `fixture.rs`, not asymmetry. Four classes appear
//! below and each is justified:
//!
//! * **Hash-function constants** — the empty blob
//!   (`e69de29bb2d1d6434b8b29ae775ad8c2e48c5391`), the empty tree
//!   (`4b825dc642cb6eb9a060e54bf8d69288fbee4904`), the all-zero id, and
//!   `deadbeef…` as a well-formed id no object has. None is a fixture value.
//! * **The shared root commit** `edfab1b71619a22120a8da1a3d85d68e0200290a`,
//!   identical in every shape but [`Shape::Unrelated`]'s two orphans because
//!   the seed content, message and pinned identity are identical
//!   (`fixture.rs:build`, `env::harden`) — the same constant
//!   `history_query.rs` already spells out.
//! * **Abbreviations of that root.** `edfab1b`, `edfab1b7` and
//!   `edfab1b71619` are used only where an abbreviation is the subject.
//!   Established by running stock 2.55.0 against a `ditto` copy of the built
//!   `branched` template under `env::harden`'s environment:
//!   `git rev-parse edfab1b edfab1b7 edfab1b71619` printed the full root id
//!   three times, rc 0. Since the template is copied to both sides by
//!   `Templates::instantiate`, that resolution is a property of the template
//!   rather than of either copy.
//! * **`e69de29`**, the empty blob abbreviated. Verified the same way against
//!   the `symlinks` template, where it resolves, and recorded by `fuzz.rs` as
//!   failing on `Branched`, where no empty file is tracked.
//!
//! **An ambiguous abbreviation is unreachable and is not faked.** Measured over
//! every object in `Packed` (34), `CrissCross` (33), `Octopus` (24) and
//! `Branched` (13) with `cat-file --batch-all-objects --batch-check`, no two
//! objects in any shape share a four-character prefix — and four is git's
//! floor. So `core.disambiguate` cannot change an answer in this corpus, which
//! is itself worth pinning: the case below asks for `edfab1b7` under
//! `core.disambiguate=blob` and stock answers with the **commit**, because
//! `object-name.c` consults the setting only to break a tie that does not
//! exist here. A port that applies the type filter unconditionally fails that
//! one case and passes every other abbreviation case in the file.
//!
//! # Fixture constraints
//!
//! * **No shape has a name that is both a branch and a tag.** Compared
//!   `for-each-ref refs/heads` against `for-each-ref refs/tags` on all 39
//!   built templates: the intersection is empty everywhere. So the
//!   `refs/tags/%.*s`-before-`refs/heads/%.*s` ordering of
//!   `refs.c:ref_rev_parse_rules` — and the `refname 'x' is ambiguous.`
//!   warning that goes with it — is **not measured here**. `fuzz.rs` reaches it
//!   only inside the `tag-shadows-branch` sequence, which creates the tag
//!   first; a case is one argv against a pristine copy and cannot. What is
//!   measured instead is the rest of that table: the same ref spelled fully
//!   qualified, namespace-relative and short.
//! * **`Damaged` cannot be walked.** A broken ref makes every `--all`
//!   traversal fatal, so this module asks it only about single names.
//! * **`Promisor` must not be made to fetch.** `rev-parse` on `<rev>:<path>`
//!   reads a tree and stops; `cat-file -p` on the blob that names would lazily
//!   fetch from the peer and turn a rev-spec case into a transport case.
//! * **One timestamp for every commit** (`env::FIXED_DATE`), so `@{now}`,
//!   `@{yesterday}` and every other date-shaped reflog spec resolve by falling
//!   off the end of the log rather than by comparing times. They are left to
//!   `fuzz.rs`, which already samples `HEAD@{now}`.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    suffixes(out);
    suffixes_past_the_end(out);
    peels(out);
    peel_refusals(out);
    reflog_and_dwim(out);
    index_and_path(out);
    search(out);
    ranges(out);
    parent_sets(out);
    object_names(out);
    ref_spellings(out);
    replaced_and_missing(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

/// The commit every shape but [`Shape::Unrelated`]'s orphans descends from.
const ROOT: &str = "edfab1b71619a22120a8da1a3d85d68e0200290a";
/// A well-formed id no object in any fixture has.
const ABSENT: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
/// The all-zero id, which `rev-parse` treats as a name rather than as a lookup.
const ZERO: &str = "0000000000000000000000000000000000000000";
/// Constants of the hash function, not of any fixture.
const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

// ---------------------------------------------------------------------------
// Suffixes: ^, ^n, ^0, ~, ~n, and their compositions
// ---------------------------------------------------------------------------

/// `<rev>^`, `<rev>^<n>`, `<rev>~<n>` and the compositions of them, each asked
/// of a shape where the answers are distinguishable, and each asked of more
/// than one verb.
///
/// What a port gets wrong without these: `^<n>` selects the *n*-th parent and
/// `~<n>` walks *n* first-parents, and on a two-parent history the two are
/// confusable — `HEAD~1` and `HEAD^1` are the same commit there. Only a
/// four-parent merge separates `^2`, `^3` and `^4`, and only a history nine
/// deep separates `~3` from `^^^` by anything but luck. `^0` is a third
/// operation again: it is not a walk at all but a peel to a commit
/// (`object-name.c` treats `^0` as `^{commit}`), so on a tag it moves and on a
/// commit it does not.
///
/// The same spelling is then handed to a walker (`rev-list`, `log`), to a
/// differ (`diff`), to an object reader (`cat-file`, `ls-tree`), to a set query
/// (`merge-base`), to an exporter (`archive`) and to a writer (`reset`). An
/// implementation with one rev parser answers all of them or none; an
/// implementation that re-implements the suffix walk per verb — which is the
/// shape a port grows into — disagrees on the ones it forgot.
fn suffixes(out: &mut Vec<Case>) {
    // Four parents. `HEAD^`…`HEAD^4` are four different commits, `HEAD^1` is
    // `HEAD^`, and `HEAD~1` is `HEAD^1` but not `HEAD^2`.
    each(
        Shape::Octopus,
        "rev-parse",
        &[
            &["rev-parse", "HEAD^", "HEAD^2", "HEAD^3", "HEAD^4"],
            // `^0` on the merge is the merge; `~` with no number is `~1`.
            &["rev-parse", "HEAD^1", "HEAD~1", "HEAD~0", "HEAD^0", "HEAD~", "HEAD^^"],
        ],
        out,
    );
    each(Shape::Octopus, "rev-list", &[&["rev-list", "--oneline", "HEAD^2"]], out);
    each(Shape::Octopus, "log", &[&["log", "--oneline", "HEAD^3"]], out);
    each(Shape::Octopus, "diff", &[&["diff", "--stat", "HEAD^2", "HEAD^3"]], out);
    each(Shape::Octopus, "merge-base", &[&["merge-base", "HEAD^2", "HEAD^4"]], out);
    each(Shape::Octopus, "ls-tree", &[&["ls-tree", "--name-only", "HEAD^2"]], out);

    // Nine commits of one file: the only place `~3`, `^^^` and `~2^` are three
    // spellings of one commit rather than three refusals.
    each(
        Shape::Packed,
        "rev-parse",
        &[
            &["rev-parse", "HEAD^^^", "HEAD~3", "HEAD~2^", "HEAD^~2"],
        ],
        out,
    );
    each(Shape::Packed, "log", &[&["log", "--oneline", "HEAD~3..HEAD~1"]], out);
    each(Shape::Packed, "diff", &[&["diff", "--stat", "HEAD~3", "HEAD^^"]], out);
    each(Shape::Packed, "cat-file", &[&["cat-file", "-t", "HEAD~3"]], out);
    each(Shape::Packed, "rev-list", &[&["rev-list", "--count", "HEAD~4"]], out);
    // A tar of a tree reached by suffix. `archive` peels the argument itself
    // (`builtin/archive.c` → `get_oid`), so a wrong suffix produces a
    // well-formed archive of the wrong tree, which only a byte comparison
    // catches.
    each(Shape::Packed, "archive", &[&["archive", "--format=tar", "HEAD~4"]], out);
    // `reset --soft` moves `HEAD` and nothing else, so what the suffix resolved
    // to lands in the state probe rather than only on stdout.
    out.push(Case::new("reset", &["reset", "--soft", "HEAD~3"], Shape::Packed));

    // `HEAD~1` is a merge here and `HEAD` is not, so `~n^2` — a walk followed
    // by a parent selection — is reachable. On every other shape carrying a
    // merge the merge is `HEAD` itself.
    each(
        Shape::CrissCross,
        "rev-parse",
        &[
            &["rev-parse", "HEAD~1^2", "HEAD~1^1", "HEAD~1^2^"],
        ],
        out,
    );
    each(Shape::CrissCross, "log", &[&["log", "--oneline", "HEAD~1^2"]], out);
}

/// The same suffixes past the end of what the repository holds.
///
/// Four different refusals hide behind one syntax and a port that produces one
/// message for all four passes nothing.
///
/// * `HEAD^5` on the octopus — a parent index the commit does not have.
/// * `HEAD~99` — a walk that runs out of history.
/// * `HEAD~1^2` on [`Shape::Merged`] — the *right* number of steps to a commit
///   that has one parent, which is the case an implementation that clamps
///   instead of failing gets wrong in the quietest way.
/// * `HEAD~2` on [`Shape::Shallow`] — history that is present in the graph and
///   deliberately absent from the store. Verified on stock 2.55.0 over that
///   shape: `HEAD~1` resolves to `bd1c76c` and `HEAD~2` is the ordinary
///   `ambiguous argument` refusal, so the shallow boundary is reported as a
///   missing revision rather than as a special condition.
///
/// `rev-parse` prints the offending spec to **stdout** before dying on stderr,
/// which is why these are `strict` — the two streams carry different halves of
/// the answer. The last two ask a walker and an object reader about a
/// nonexistent parent instead, where the refusal is the verb's own
/// (`fatal: bad revision` / `fatal: Not a valid object name`) and no spec is
/// echoed at all.
fn suffixes_past_the_end(out: &mut Vec<Case>) {
    out.push(Case::strict("rev-parse", &["rev-parse", "HEAD^5"], Shape::Octopus));
    out.push(Case::strict("rev-parse", &["rev-parse", "HEAD~99"], Shape::Packed));
    out.push(Case::strict("rev-parse", &["rev-parse", "HEAD~1^2"], Shape::Merged));
    out.push(Case::strict("rev-parse", &["rev-parse", "HEAD~2"], Shape::Shallow));
    out.push(Case::new("rev-parse", &["rev-parse", "HEAD~1"], Shape::Shallow));
    out.push(Case::strict("log", &["log", "--oneline", "HEAD^5"], Shape::Octopus));
    out.push(Case::strict("cat-file", &["cat-file", "-t", "HEAD^9"], Shape::Merged));
}

// ---------------------------------------------------------------------------
// Peels: ^{}, ^{commit}, ^{tree}, ^{blob}, ^{tag}, ^{object}
// ---------------------------------------------------------------------------

/// Peeling, over the only shape where peeling is more than one step.
///
/// `fixture_gaps2.rs` already pins eleven `rev-parse` peels and ten `cat-file`
/// reads on this shape. This block adds the two spellings that block omits and
/// then hands the peel to ten other verbs.
///
/// The two spellings: `^{object}` is the one peel operator that peels
/// **nothing** — `gitrevisions(7)` defines it as "the object can be of any
/// type", so `outermost^{object}` is `outermost` and `cat-file -t` on it
/// answers `tag`, not `commit`. And `^{tag}` peels a chain *partway*: on
/// `outer` it stops at `outer` itself, on `light-to-tag` (a lightweight ref
/// pointing at a tag object) it yields the tag object that ref names. Verified
/// on stock 2.55.0 over this shape: `rev-parse outer^{tag}` is `24b224a…`
/// (`outer`), `light-to-tag^{tag}` and `inner^{tag}` are both `7f25e3f…`
/// (`inner`), and `light-to-tag^0` — the `^{commit}` of a ref that names a tag
/// object — is `7e36e3a…`, three peels further on.
///
/// What a port gets wrong without the verb crossing: peeling is done by the
/// caller as often as by the parser. `merge-base`, `describe` and `log` each
/// ask for a *committish* and get a tag object back; `ls-tree` and `archive`
/// each ask for a *treeish* and get a tag object back;
/// `tag --points-at` and `for-each-ref --points-at` peel the **refs they are
/// scanning** rather than the argument, which is why all four tags in the chain
/// answer to `inner^{}` — the argument peels to a commit and every ref in the
/// chain peels to the same commit. A port that peels in `rev-parse` and nowhere
/// else passes `fixture_gaps2.rs` completely and fails here.
fn peels(out: &mut Vec<Case>) {
    each(
        Shape::TagChain,
        "rev-parse",
        &[
            // The non-peel, and the partial peels the existing block omits.
            &["rev-parse", "outermost^{object}", "blobtag^{object}", "treetag^{object}"],
            // `^0` on a three-deep chain is `^{commit}` spelled the other way.
            &["rev-parse", "outer^{tag}", "light-to-tag^{tag}", "outer^0", "light-to-tag^0"],
        ],
        out,
    );
    each(
        Shape::TagChain,
        "cat-file",
        &[
            &["cat-file", "-t", "outermost^{object}"],
            &["cat-file", "-t", "blobtag^{object}"],
        ],
        out,
    );
    // A treeish operand: a tag handed straight to `ls-tree` has to be peeled
    // three times before there is a tree to read, and `light-to-tag` reaches
    // the same tree through a ref that is not itself a tag object.
    each(
        Shape::TagChain,
        "ls-tree",
        &[
            &["ls-tree", "outermost"],
            &["ls-tree", "-r", "--name-only", "light-to-tag"],
        ],
        out,
    );
    each(Shape::TagChain, "archive", &[&["archive", "--format=tar", "outermost"]], out);
    // A committish operand, three ways.
    each(
        Shape::TagChain,
        "merge-base",
        &[&["merge-base", "outermost", "HEAD"]],
        out,
    );
    each(
        Shape::TagChain,
        "describe",
        &[&["describe", "--tags", "outermost^{}"]],
        out,
    );
    each(
        Shape::TagChain,
        "log",
        &[&["log", "--oneline", "outermost"]],
        out,
    );
    each(
        Shape::TagChain,
        "diff",
        &[
            &["diff", "--stat", "outermost^{tree}", "HEAD^{tree}"],
        ],
        out,
    );
    // `--points-at` peels the refs it scans, not only the argument: all four
    // tags in the chain answer to the commit `inner` peels to.
    each(
        Shape::TagChain,
        "tag",
        &[&["tag", "--points-at", "inner"]],
        out,
    );
    each(
        Shape::TagChain,
        "for-each-ref",
        &[
            &["for-each-ref", "--points-at", "outermost^{}", "--format=%(refname) %(objecttype)"],
        ],
        out,
    );
    // Writers. `reset --soft` records the peeled commit in `HEAD` and the
    // reflog; `checkout` of a peeled tag detaches at it. Both land in the state
    // probe rather than only on stdout.
    out.push(Case::new("reset", &["reset", "--soft", "outermost^{commit}"], Shape::TagChain));
    out.push(Case::new("checkout", &["checkout", "outermost^{}"], Shape::TagChain));

    // The one-deep chain the rest of the corpus has, asked for the spellings
    // `corpus.rs` does not: `v0.2.0` is annotated so `^{}` moves, `v0.1.0` is
    // lightweight so it does not.
    each(
        Shape::Branched,
        "rev-parse",
        &[
            &["rev-parse", "v0.2.0", "v0.2.0^{}", "v0.2.0^{tag}"],
            &["rev-parse", "v0.1.0", "v0.1.0^{}", "v0.2.0^{tree}"],
        ],
        out,
    );
}

/// Peels that cannot succeed.
///
/// `object-name.c:peel_to_type` names both the type asked for and the type
/// found, so a port that prints a generic "not a valid object" fails all three
/// of the `rev-parse` cases below. Verified on stock 2.55.0:
///
/// ```text
/// error: treetag^{commit}: expected commit type, but the object dereferences to tree type
/// error: blobtag^{tree}:   expected tree type, but the object dereferences to blob type
/// error: v0.1.0^{tag}:     expected tag type, but the object dereferences to tree type
/// ```
///
/// The third is the subtle one: `v0.1.0` is a *lightweight* tag on a commit, so
/// asking for `^{tag}` finds no tag object and the type git reports is the one
/// the peel loop ended on, not the one the ref points at. Each of these prints
/// the `error:` line **twice** and then the `ambiguous argument` block, with
/// the spec echoed on stdout — three streams' worth of contract for one
/// spelling.
///
/// `merge-base blobtag HEAD` and `ls-tree blobtag` are the caller-side half:
/// the argument peels fine and the *verb* rejects the type, with a different
/// message again (`error: object 220697fd… is a blob, not a commit` /
/// `fatal: Not a valid commit name blobtag`).
fn peel_refusals(out: &mut Vec<Case>) {
    out.push(Case::strict("rev-parse", &["rev-parse", "treetag^{commit}"], Shape::TagChain));
    out.push(Case::strict("rev-parse", &["rev-parse", "blobtag^{tree}"], Shape::TagChain));
    out.push(Case::strict("rev-parse", &["rev-parse", "v0.1.0^{tag}"], Shape::Branched));
    out.push(Case::strict("merge-base", &["merge-base", "blobtag", "HEAD"], Shape::TagChain));
    out.push(Case::strict("ls-tree", &["ls-tree", "blobtag"], Shape::TagChain));
}

// ---------------------------------------------------------------------------
// @{n}, @{-n}, @{upstream}, @{push}
// ---------------------------------------------------------------------------

/// The `@{…}` family: reflog indexing, branch-switch history, and the two
/// tracking relations.
///
/// Four different lookups share one syntax and none of them is a graph walk:
///
/// * `<ref>@{<n>}` reads the *n*-th prior value out of `logs/<ref>`.
/// * `@{<n>}` with no ref reads the reflog of the **current branch**, not of
///   `HEAD`. Verified on stock 2.55.0 over [`Shape::Branched`]: `HEAD@{1}` is
///   `07e86d1` (the previous value of `HEAD`, set by a checkout) while `@{1}`
///   is `edfab1b` (the previous value of `refs/heads/main`, set by a commit).
///   They are two different commits, and a port that treats bare `@` as a
///   synonym for `HEAD` throughout — which is what `@` alone means — answers
///   `HEAD@{1}` for both.
/// * `@{-<n>}` is not a reflog index at all: it is the *n*-th branch checked
///   out before the current one, recovered by scanning `HEAD`'s reflog for
///   `checkout: moving from X to Y` messages
///   (`refs.c:interpret_branch_mark` / `wt-status.c`). On `Branched` it is
///   `07e86d1`, which is `HEAD@{1}` by coincidence of this fixture and
///   `refs/heads/feature` by name — so only `--symbolic-full-name` separates
///   the two, which is why that spelling is asked beside the id.
/// * `@{upstream}` / `@{u}` / `@{push}` consult the *configuration*
///   (`branch.<name>.remote` + `.merge`, and `remote.<name>.push` /
///   `push.default` for the second), not the reflog. On
///   [`Shape::BehindRemote`] all three resolve; verified there that
///   `--symbolic-full-name @{u}` and `@{push}` both answer
///   `refs/remotes/origin/main`, so the two spellings agree in this fixture and
///   only their *plumbing* differs — which is the point of asking both.
///
/// Which shapes can answer: `Branched` (five `HEAD` entries, two on `main`),
/// `Merged` and `Unrelated` (six each, with a different branch behind
/// `@{-1}`), `BehindRemote` (the only configured upstream). `Packed`'s reflog
/// was expired at build time, which is what makes its refusal a *different*
/// one.
fn reflog_and_dwim(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "rev-parse",
        &[
            &["rev-parse", "HEAD@{0}", "HEAD@{1}", "@{0}", "@{1}", "main@{0}", "main@{1}", "@{-1}", "feature@{1}"],
            &["rev-parse", "--symbolic-full-name", "@{-1}", "HEAD@{1}"],
        ],
        out,
    );
    each(Shape::Branched, "log", &[&["log", "--oneline", "HEAD@{1}"]], out);
    each(Shape::Branched, "show", &[&["show", "--oneline", "--stat", "@{-1}"]], out);
    each(Shape::Branched, "rev-list", &[&["rev-list", "--oneline", "main@{1}..main"]], out);
    each(Shape::Branched, "merge-base", &[&["merge-base", "@{-1}", "HEAD"]], out);
    // Writers, so the resolved id lands in the state probe. `reset --soft @{1}`
    // rewinds `main` to its previous value; `checkout @{-1}` is the long form of
    // `checkout -` and switches to `feature`.
    out.push(Case::new("reset", &["reset", "--soft", "@{1}"], Shape::Branched));
    out.push(Case::new("checkout", &["checkout", "@{-1}"], Shape::Branched));

    // A different branch behind `@{-1}` on two other shapes, so the answer is
    // not the fixture-specific coincidence `Branched` has.
    each(
        Shape::Merged,
        "rev-parse",
        &[&["rev-parse", "@{-1}", "HEAD@{2}"]],
        out,
    );
    each(Shape::Unrelated, "rev-parse", &[&["rev-parse", "@{-1}"]], out);
    // Only one branch was ever switched away from here, so `@{-2}` is the
    // `ambiguous argument` refusal — the same syntax one step past its data.
    out.push(Case::strict("rev-parse", &["rev-parse", "@{-2}"], Shape::Unrelated));

    // The tracking relations. `main` has an upstream and is behind it; `div`
    // has one and has diverged from it, so the two answer differently.
    each(
        Shape::BehindRemote,
        "rev-parse",
        &[
            &["rev-parse", "@{u}", "@{upstream}", "main@{upstream}", "@{push}", "main@{push}", "div@{upstream}"],
            &["rev-parse", "--symbolic-full-name", "@{u}", "@{push}", "div@{u}"],
        ],
        out,
    );
    each(
        Shape::BehindRemote,
        "rev-list",
        &[
            &["rev-list", "--count", "--left-right", "@{u}...HEAD"],
        ],
        out,
    );
    each(Shape::BehindRemote, "log", &[&["log", "--oneline", "@{u}"]], out);
    each(Shape::BehindRemote, "branch", &[&["branch", "--contains", "@{u}"]], out);

    // Three refusals, three different reasons, one syntax.
    //
    // * `@{u}` on a branch with no upstream:
    //   `fatal: no upstream configured for branch 'main'`.
    // * `main@{2}` where the log has two entries:
    //   `fatal: log for 'main' only has 2 entries`.
    // * `HEAD@{1}` where the log was expired:
    //   `fatal: log for HEAD is empty`.
    //
    // None of the three is the generic `ambiguous argument` block, and a port
    // that answers all `@{…}` misses with one message fails all three.
    out.push(Case::strict("rev-parse", &["rev-parse", "@{u}"], Shape::Branched));
    out.push(Case::strict("rev-parse", &["rev-parse", "main@{2}"], Shape::Branched));
    out.push(Case::strict("rev-parse", &["rev-parse", "HEAD@{1}"], Shape::Packed));
    out.push(Case::strict("log", &["log", "--oneline", "@{push}"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// :path, :<stage>:path, <rev>:path
// ---------------------------------------------------------------------------

/// The object-in-a-tree and object-in-the-index spellings.
///
/// These are the only rev-specs that do not name a commit, which is why a port
/// tends to grow them as a special case bolted onto the side of the parser
/// rather than as an arm of it. Three sub-grammars share the leading colon
/// (`gitrevisions(7)`, "Revision Range Summary"; `object-name.c` handles them
/// in `get_oid_with_context_1`):
///
/// * `:<path>` and `:0:<path>` — the index at stage 0.
/// * `:<1|2|3>:<path>` — a conflicted index's stages. [`Shape::Conflicted`] is
///   the only fixture with any, and its conflict is **add/add**, so
///   `conflict.txt` has stages 2 and 3 and no stage 1. That makes `:1:` on
///   that path a refusal *while* `:2:` and `:3:` succeed in the same argv,
///   which is a sharper measurement than a repository where the whole family
///   fails.
/// * `<rev>:<path>` — a path inside a commit's tree, including a directory
///   (`HEAD:src` is a tree, verified with `cat-file -t`) and the two
///   cwd-relative forms `<rev>:./<path>` and `:./<path>`, which
///   `object-name.c` resolves against the prefix `setup.c` computed. Those two
///   are the reason [`Case::in_dir`] exists in this block: from the fixture
///   root `HEAD:./lib.rs` names nothing, and from `src/` it names the same blob
///   as `HEAD:src/lib.rs`. A port that ignores the prefix answers the first
///   with a refusal and looks correct everywhere a case runs at the root.
///
/// The refusals are the contract twice over, because `object-name.c` writes a
/// *hint* rather than the generic block: asking for a stage a path does not
/// have prints `fatal: path 'conflict.txt' is in the index, but not at stage 1`
/// followed by `hint: Did you mean ':2:conflict.txt'?`, and asking for a path a
/// tree does not have prints `fatal: path 'nope.txt' does not exist in 'HEAD'`.
/// Both echo the spec on stdout before dying.
fn index_and_path(out: &mut Vec<Case>) {
    each(
        Shape::Conflicted,
        "rev-parse",
        &[
            &["rev-parse", ":README.md", ":0:README.md", ":2:conflict.txt", ":3:conflict.txt"],
            &["rev-parse", "HEAD:conflict.txt", "theirs:conflict.txt"],
        ],
        out,
    );
    each(
        Shape::Conflicted,
        "cat-file",
        &[
            &["cat-file", "-p", ":2:conflict.txt"],
        ],
        out,
    );
    // Two blobs named by stage, handed to a differ: `diff <blob> <blob>` is the
    // only way to see the two sides of a conflict without a merge driver.
    each(
        Shape::Conflicted,
        "diff",
        &[&["diff", ":2:conflict.txt", ":3:conflict.txt"]],
        out,
    );

    each(
        Shape::Branched,
        "rev-parse",
        &[
            &["rev-parse", "HEAD:src/lib.rs", ":src/lib.rs", ":0:src/lib.rs", "HEAD:src", "feature:feature.txt"],
        ],
        out,
    );
    each(Shape::Branched, "ls-tree", &[&["ls-tree", "HEAD:src"]], out);
    // Cwd-relative forms, which only resolve from inside the subdirectory.
    out.push(
        Case::new("rev-parse", &["rev-parse", "HEAD:./lib.rs", ":./lib.rs", "HEAD:../README.md"], Shape::Branched)
            .in_dir("src"),
    );
    out.push(Case::new("cat-file", &["cat-file", "-p", "HEAD:./lib.rs"], Shape::Branched).in_dir("src"));
    // A path in a tree the fixture keeps only in history: `Promisor` has the
    // blob's *id* in a tree it holds and the blob itself absent, so resolving
    // the name must succeed without reading the object. Asking `cat-file` for
    // its contents instead would make the case fetch from the peer.
    each(
        Shape::Promisor,
        "rev-parse",
        &[&["rev-parse", "HEAD~2:hist.txt", "HEAD:hist.txt", "HEAD~3^{tree}"]],
        out,
    );

    // Four refusals, four distinct diagnostics.
    out.push(Case::strict("rev-parse", &["rev-parse", ":1:conflict.txt"], Shape::Conflicted));
    out.push(Case::strict("rev-parse", &["rev-parse", "HEAD:nope.txt"], Shape::Branched));
    out.push(Case::strict("cat-file", &["cat-file", "-p", ":1:conflict.txt"], Shape::Conflicted));
    // From the root, the cwd-relative form names nothing.
    out.push(Case::strict("rev-parse", &["rev-parse", "HEAD:./lib.rs"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// :/text and <rev>^{/text}
// ---------------------------------------------------------------------------

/// Message search, in its two scopes.
///
/// `:/text` starts from **every** ref and walks back to the youngest commit
/// whose message matches; `<rev>^{/text}` starts from one revision and walks
/// only that history. Same regex, two different traversals, and
/// `object-name.c:get_oid_oneline` implements both from one entry point — so a
/// port that wires only one of them up gets half of this block.
///
/// Verified on stock 2.55.0:
///
/// * [`Shape::Branched`] — `:/two` and `:/add` are both `5915d79` ("add two"),
///   `:/!-add` is `07e86d1` ("feature commit"), the youngest commit that does
///   *not* match. `!-` is the negation prefix; a bare `!` is reserved and `!!`
///   escapes a literal one, so a port that forwards the pattern to a regex
///   engine unmodified answers `:/!-add` with a refusal.
/// * [`Shape::Cherry`] — `:/shared` is `7a4b88a`, the **topic** branch's copy
///   of the duplicated patch, not `main`'s. The all-refs walk picks the
///   youngest match across every ref, so a port that searches from `HEAD` alone
///   answers `6fca700` and is wrong only on the one shape that has two commits
///   with one message.
/// * [`Shape::Unrelated`] — `:/alien root` reaches a commit on an orphan
///   branch, which no walk from `HEAD` can see at all.
/// * A pattern with a space (`Packed`'s `:/revision 3`) travels as one argv
///   token, which is the case a port that splits its own arguments loses.
fn search(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "rev-parse",
        &[
            &["rev-parse", ":/two", ":/add", ":/!-add", "HEAD^{/two}", "feature^{/feature}"],
        ],
        out,
    );
    each(Shape::Branched, "log", &[&["log", "--oneline", ":/two"]], out);
    each(Shape::Branched, "rev-list", &[&["rev-list", "--oneline", "feature^{/feature}"]], out);
    each(
        Shape::Cherry,
        "rev-parse",
        &[&["rev-parse", ":/shared", ":/!-cherry", "main^{/upstream}"]],
        out,
    );
    each(Shape::Unrelated, "rev-parse", &[&["rev-parse", ":/alien root"]], out);
    each(
        Shape::Packed,
        "rev-parse",
        &[&["rev-parse", ":/revision 3", "HEAD^{/revision 5}"]],
        out,
    );

    // A miss in each scope. Both are the generic `ambiguous argument` block
    // rather than a search-specific message, which is itself the thing to pin:
    // the failure is reported by the caller, not by the search.
    out.push(Case::strict("rev-parse", &["rev-parse", ":/nomatchhere"], Shape::Branched));
    out.push(Case::strict("rev-parse", &["rev-parse", "HEAD^{/nomatchhere}"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// Ranges: A..B, A...B, ^A B, --not A B
// ---------------------------------------------------------------------------

/// The four ways to write a set of commits, against the verbs that take one.
///
/// `revision.c:handle_revision_arg` turns `A..B` into `B ^A` and `A...B` into
/// `A B --not $(merge-base --all A B)`, so the second is the only range whose
/// meaning depends on a *computation* rather than on a rewrite — and the
/// corpus had no shape where that computation could go more than one way. Two
/// now do:
///
/// * [`Shape::Unrelated`] — `main...alien` has **no** merge base, so the
///   symmetric difference is the union of two whole histories. `diff` refuses
///   it outright (`fatal: main...alien: no merge base`) while `rev-list` and
///   `format-patch` answer happily and `merge-base` exits 1 with no output —
///   four verbs disagreeing about one spelling on purpose.
/// * [`Shape::CrissCross`] — `cc-left...cc-right` has **two** merge bases.
///   `rev-list` negates both; `diff` cannot and picks one, printing
///   `warning: cc-left...cc-right: multiple merge bases, using 0a24ba3…` on
///   stderr before the diff. A port that stops at the first base produces the
///   same diff and a different warning, or the same warning and a different
///   diff, depending on which base it found first — which is why that case is
///   `strict`.
///
/// `^A B` and `--not A B` are the same negation spelled two ways, and they are
/// not interchangeable in position: `--not` flips the sense of *everything
/// after it* until the next `--not`, while `^` attaches to one argument. On
/// [`Shape::Octopus`], `rev-list HEAD --not HEAD^2` and `rev-list ^HEAD^2 HEAD`
/// agree, and `rev-list --not HEAD^2 HEAD^3` — where the positive argument is
/// also negated — is empty. That last one is the case an implementation that
/// treats `--not` as a prefix on the next token alone gets wrong.
fn ranges(out: &mut Vec<Case>) {
    each(
        Shape::Unrelated,
        "rev-list",
        &[
            &["rev-list", "--left-right", "--oneline", "main...alien"],
            &["rev-list", "--count", "--left-right", "main...alien-clash"],
            &["rev-list", "--oneline", "alien", "--not", "main"],
        ],
        out,
    );
    each(Shape::Unrelated, "diff", &[&["diff", "--stat", "main..alien"]], out);
    each(
        Shape::Unrelated,
        "format-patch",
        &[&["format-patch", "--stdout", "--no-signature", "main...alien"]],
        out,
    );
    each(Shape::Unrelated, "merge-base", &[&["merge-base", "main", "alien"]], out);

    each(
        Shape::CrissCross,
        "rev-list",
        &[
            &["rev-list", "--left-right", "--oneline", "cc-left...cc-right"],
            &["rev-list", "--count", "--left-right", "cc-left...cc-right"],
        ],
        out,
    );
    each(Shape::CrissCross, "range-diff", &[&["range-diff", "cc-left...cc-right"]], out);
    each(Shape::CrissCross, "merge-base", &[&["merge-base", "--all", "cc-left", "cc-right"]], out);
    // The one range whose stderr is part of the answer.
    out.push(Case::strict("diff", &["diff", "--stat", "cc-left...cc-right"], Shape::CrissCross));

    // Duplicated patch id on both sides of a hand-written symmetric
    // difference: the `=` class of `--cherry-mark` and the `-` of `cherry`.
    each(
        Shape::Cherry,
        "rev-list",
        &[
            &["rev-list", "--count", "--left-right", "--cherry-mark", "main...topic"],
            &["rev-list", "--left-right", "--cherry-pick", "--oneline", "main...topic"],
        ],
        out,
    );
    each(Shape::Cherry, "cherry", &[&["cherry", "main", "topic"]], out);
    each(Shape::Cherry, "range-diff", &[&["range-diff", "main...topic"]], out);
    each(
        Shape::Cherry,
        "format-patch",
        &[&["format-patch", "--stdout", "--no-signature", "main..topic"]],
        out,
    );

    // `^` versus `--not`, and the position `--not` is sensitive to.
    each(
        Shape::Octopus,
        "rev-list",
        &[
            &["rev-list", "--oneline", "^HEAD^2", "HEAD"],
            &["rev-list", "--oneline", "HEAD", "--not", "HEAD^2"],
            &["rev-list", "--oneline", "--not", "HEAD^2", "HEAD^3"],
        ],
        out,
    );

    // A range with an omitted endpoint defaults the missing side to `HEAD`; a
    // parser that splits on `..` and resolves both halves rejects it outright.
    each(
        Shape::Packed,
        "rev-list",
        &[
            &["rev-list", "--count", "HEAD~3.."],
        ],
        out,
    );
}

/// The parent-set notations: `^@`, `^!`, `^-` and `^-<n>`.
///
/// Each expands to a *set* rather than to one commit, and the expansion is done
/// by `revision.c:handle_revision_arg` before any verb sees it — `^@` becomes
/// every parent, `^!` becomes the commit with every parent negated, and `^-<n>`
/// becomes `<rev> ^<rev>^<n>`. So `rev-parse` prints them as several lines,
/// some with a leading `^`, and every walking verb turns them into a different
/// walk. A port that treats them as suffixes on a single resolution prints one
/// id where stock prints five.
///
/// [`Shape::Octopus`] is where they separate. Verified on stock 2.55.0:
/// `HEAD^@` lists five commits (the four merged tips' histories),
/// `HEAD^!` lists the merge alone, `HEAD^-` lists the merge plus the three
/// non-first-parent lanes, and `HEAD^-2` lists a *different* four because it
/// negates parent 2 instead of parent 1. On a two-parent history `^-` and
/// `^-2` are the only pair that could differ and they differ by one lane;
/// here they differ by which of four.
///
/// `rev-parse` is asked as well as the walkers because it is the only verb that
/// shows the expansion itself — the `^`-prefixed negative lines — rather than
/// its result.
fn parent_sets(out: &mut Vec<Case>) {
    each(
        Shape::Octopus,
        "rev-parse",
        &[&["rev-parse", "HEAD^!"], &["rev-parse", "HEAD^@"], &["rev-parse", "HEAD^-", "HEAD^-2"]],
        out,
    );
    each(
        Shape::Octopus,
        "rev-list",
        &[
            &["rev-list", "--oneline", "HEAD^@"],
            &["rev-list", "--oneline", "HEAD^-"],
            &["rev-list", "--oneline", "HEAD^-2"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "log",
        &[&["log", "--oneline", "HEAD^!"]],
        out,
    );
    each(
        Shape::Octopus,
        "format-patch",
        &[&["format-patch", "--stdout", "--no-signature", "HEAD^-2"]],
        out,
    );
    // The same expansion on a non-merge commit, where `^!` negates one parent
    // rather than four — the degenerate case a port can pass by accident, and
    // the reason the octopus cases above exist.
    each(
        Shape::Cherry,
        "format-patch",
        &[&["format-patch", "--stdout", "--no-signature", "main^!"]],
        out,
    );
    // `^!` through a writer: `cherry-pick <rev>^!` is the documented spelling
    // for replaying exactly one commit, and it is the only place the expansion
    // reaches the sequencer.
    out.push(Case::new("cherry-pick", &["cherry-pick", "feature^!"], Shape::Branched));
    // The root commit has no parents, so `^@` is the empty set and `^!` is the
    // commit with nothing negated — an expansion with nothing to expand.
    each(
        Shape::Branched,
        "rev-parse",
        &[&["rev-parse", "main~1^@"], &["rev-parse", "main~1^!"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// Object names: full ids, abbreviations, and the constants
// ---------------------------------------------------------------------------

/// Naming an object by its id, at every width the parser accepts.
///
/// A full 40-hex string is not a lookup at all: `object-name.c` accepts it as a
/// name and hands it back, so `rev-parse deadbeef…` (40 hex) **succeeds** with
/// rc 0 over a repository that has no such object, and only a verb that then
/// *reads* the object fails (`cat-file -t` →
/// `fatal: git cat-file: could not get object info`; `log` →
/// `fatal: bad object deadbeef…`). That split — resolution succeeding where
/// access fails — is the single most confusable thing in this file, and it is
/// why the same absent id is asked of `rev-parse` and of two readers below.
///
/// An abbreviation *is* a lookup (`get_short_oid`), so it fails where the
/// object is absent. `e69de29` is the sharpest pair available: it resolves on
/// [`Shape::Symlinks`], the only shape that tracks a zero-byte file, and is
/// recorded by `fuzz.rs` as failing on `Branched`, where nothing is empty.
///
/// `core.abbrev` sets the width `--short` prints and the width `--oneline`
/// decorates with; it does **not** set the width the parser accepts. And
/// `core.disambiguate` is a tie-breaker: with no four-character collision
/// anywhere in the corpus (see the module header) it can never fire, so
/// `edfab1b7` resolves to the commit even under `core.disambiguate=blob` and
/// `=committish`. Both are pinned below because a port that applies either
/// setting unconditionally passes every other abbreviation case here.
fn object_names(out: &mut Vec<Case>) {
    // The shared root, spelled at four widths, through four verbs.
    each(
        Shape::Branched,
        "rev-parse",
        &[&["rev-parse", ROOT, "edfab1b", "edfab1b7", "edfab1b71619"]],
        out,
    );
    each(Shape::Branched, "cat-file", &[&["cat-file", "-t", ROOT]], out);
    each(Shape::Branched, "log", &[&["log", "--oneline", "edfab1b7"]], out);
    each(Shape::Branched, "name-rev", &[&["name-rev", ROOT]], out);
    each(Shape::Branched, "describe", &[&["describe", "--always", "edfab1b71619"]], out);
    // The width `--short` prints, from the flag and from the configuration, and
    // the width the *parser* accepts, which the configuration does not change.
    each(
        Shape::Branched,
        "rev-parse",
        &[&["rev-parse", "--short=4", "HEAD"]],
        out,
    );
    for width in ["4", "40"] {
        out.push(
            Case::new("rev-parse", &["rev-parse", "--short", "edfab1b7"], Shape::Branched)
                .with_config(&[("core.abbrev", width)]),
        );
    }
    out.push(
        Case::new("log", &["log", "--oneline", "-1", "edfab1b7"], Shape::Branched)
            .with_config(&[("core.abbrev", "12")]),
    );
    // The tie-breaker that has no tie to break.
    for kind in ["blob", "committish"] {
        out.push(
            Case::new("rev-parse", &["rev-parse", "--verify", "edfab1b7"], Shape::Branched)
                .with_config(&[("core.disambiguate", kind)]),
        );
    }
    // `--disambiguate=` is a different thing entirely: it *lists* every object
    // with the prefix rather than resolving one.
    each(Shape::Branched, "rev-parse", &[&["rev-parse", "--disambiguate=edfab"]], out);

    // Constants of the hash function. The empty tree resolves everywhere
    // because git knows it without a store; the empty blob resolves only where
    // a zero-byte file is tracked.
    each(
        Shape::Branched,
        "rev-parse",
        &[&["rev-parse", EMPTY_TREE, "--verify", ZERO]],
        out,
    );
    each(
        Shape::Symlinks,
        "rev-parse",
        &[&["rev-parse", "e69de29", EMPTY_BLOB]],
        out,
    );
    each(Shape::Symlinks, "cat-file", &[&["cat-file", "-s", "e69de29"]], out);
    // Well-formed, absent, and the two sides of the resolution/access split.
    each(Shape::Branched, "rev-parse", &[&["rev-parse", "--verify", ABSENT]], out);
    out.push(Case::strict("cat-file", &["cat-file", "-t", ABSENT], Shape::Branched));
    out.push(Case::strict("log", &["log", "--oneline", ABSENT], Shape::Branched));
    out.push(Case::strict("rev-parse", &["rev-parse", "e69de29"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// Ref spellings and refname validity
// ---------------------------------------------------------------------------

/// The DWIM table (`refs.c:ref_rev_parse_rules`), and names it must refuse.
///
/// A short name is tried against six patterns in order — `%.*s`,
/// `refs/%.*s`, `refs/tags/%.*s`, `refs/heads/%.*s`, `refs/remotes/%.*s`,
/// `refs/remotes/%.*s/HEAD` — so one ref is reachable by three spellings and a
/// port with a two-entry table resolves the fully-qualified and the bare form
/// and misses the middle one. `--symbolic-full-name` is the flag that shows
/// *which* rule fired, which is why the three spellings are asked through it
/// rather than only through the id.
///
/// **The ambiguous case is not measured here and is not invented.** No shape
/// carries a name that is both a branch and a tag (verified across all built
/// templates), so the `refs/tags/` -before- `refs/heads/` ordering and the
/// `warning: refname 'x' is ambiguous.` that goes with it are only reachable
/// from `fuzz.rs`'s `tag-shadows-branch` sequence, which creates the tag first.
///
/// The refusals are the other half of the table. `bad..name` is rejected by
/// `check_refname_format` before any ref store is consulted (`..` is
/// forbidden) and `main.lock` by the `.lock` suffix rule, and stock reports
/// both as the ordinary `ambiguous argument` block rather than as a format
/// error — a port that surfaces its validator's own message diverges on the
/// text while agreeing on the exit code.
fn ref_spellings(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "rev-parse",
        &[
            &["rev-parse", "feature", "heads/feature", "refs/heads/feature", "v0.1.0", "tags/v0.1.0"],
            &["rev-parse", "--symbolic-full-name", "feature", "heads/feature", "v0.1.0", "tags/v0.1.0"],
            &["rev-parse", "--abbrev-ref", "refs/heads/feature", "refs/tags/v0.2.0"],
        ],
        out,
    );
    // Remote-tracking refs add the fifth and sixth rules: `origin/main` is
    // reached through `refs/remotes/%.*s` and `origin` through
    // `refs/remotes/%.*s/HEAD` where one exists.
    each(
        Shape::BehindRemote,
        "rev-parse",
        &[
            &["rev-parse", "origin/main", "remotes/origin/main", "refs/remotes/origin/main"],
        ],
        out,
    );
    each(
        Shape::Shallow,
        "rev-parse",
        &[&["rev-parse", "--symbolic-full-name", "origin", "origin/main"]],
        out,
    );
    // Notes and replace refs are ordinary refs under `refs/`, so the second
    // rule reaches them and nothing else does — `commits` alone names nothing.
    each(
        Shape::NotesReplace,
        "rev-parse",
        &[
            &["rev-parse", "refs/notes/commits", "notes/commits", "refs/notes/commits^{tree}"],
        ],
        out,
    );

    // Names the format rejects, and one the store simply does not have.
    out.push(Case::strict("rev-parse", &["rev-parse", "bad..name"], Shape::Branched));
    out.push(Case::strict("rev-parse", &["rev-parse", "main.lock"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// Revs that resolve through a replacement, a graft, or a missing object
// ---------------------------------------------------------------------------

/// Rev-specs over the three shapes where resolving a name and reading the
/// object it names are different questions.
///
/// * [`Shape::NotesReplace`] carries two `refs/replace/*` entries and the port
///   is recorded as writing them and never consulting them. The substitution
///   happens on **read**, not on resolution, and stock 2.55.0 draws that line
///   exactly: `rev-parse HEAD~2` prints the *original* id `0dc1e64…` with and
///   without `--no-replace-objects`, and `rev-parse HEAD:README.md` prints the
///   *original* blob `9741694d…` both ways — while `log --oneline -1 HEAD~2`
///   prints `notes: replacement for commit 1` with replacement and
///   `notes: commit 1` without, and `cat-file -p HEAD:README.md` prints
///   `# replaced readme` against `# fixture`. So a port that never reads
///   `refs/replace/*` agrees on every `rev-parse` case in this block and
///   disagrees on every reader. `GIT_NO_REPLACE_OBJECTS` is the environment
///   spelling of the same switch and goes through a different code path from
///   the option (`replace-object.c` reads it in `prepare_replace_object`).
/// * [`Shape::Damaged`] has a ref whose object is absent and a symref whose
///   target does not exist. `rev-parse --verify refs/heads/dangling` succeeds
///   and prints `deadbeef…` because a 40-hex ref value is taken at its word;
///   every suffix on it fails, because a suffix needs the object. The symref
///   warns twice and then dies.
/// * [`Shape::Shallow`] has a graft: `HEAD~1` is the last commit in the store
///   and its parent is recorded in `.git/shallow`. Verified there that
///   `rev-list --count HEAD` is bounded by the graft rather than by history.
fn replaced_and_missing(out: &mut Vec<Case>) {
    each(
        Shape::NotesReplace,
        "rev-parse",
        &[&["rev-parse", "HEAD~2", "HEAD~2^{tree}", "HEAD:README.md"]],
        out,
    );
    each(
        Shape::NotesReplace,
        "log",
        &[&["log", "--oneline", "-1", "HEAD~2"]],
        out,
    );
    each(Shape::NotesReplace, "cat-file", &[&["cat-file", "-p", "HEAD:README.md"]], out);
    each(Shape::NotesReplace, "ls-tree", &[&["ls-tree", "HEAD", "README.md"]], out);
    // The same three reads with the substitution switched off, by option and by
    // environment — two entry points to one decision.
    out.push(
        Case::new("log", &["log", "--oneline", "-1", "HEAD~2"], Shape::NotesReplace)
            .with_globals(&[&["--no-replace-objects"]]),
    );
    out.push(
        Case::new("cat-file", &["cat-file", "-p", "HEAD:README.md"], Shape::NotesReplace)
            .with_globals(&[&["--no-replace-objects"]]),
    );
    out.push(
        Case::new("cat-file", &["cat-file", "-p", "HEAD:README.md"], Shape::NotesReplace)
            .with_env(&[("GIT_NO_REPLACE_OBJECTS", "1")]),
    );

    // A ref that resolves to an id nothing has.
    each(
        Shape::Damaged,
        "rev-parse",
        &[
            &["rev-parse", "--verify", "refs/heads/dangling"],
        ],
        out,
    );
    out.push(Case::strict("rev-parse", &["rev-parse", "refs/heads/dangling^0"], Shape::Damaged));
    out.push(Case::strict("rev-parse", &["rev-parse", "refs/heads/broken-symref"], Shape::Damaged));
    out.push(Case::strict("cat-file", &["cat-file", "-t", "refs/heads/dangling"], Shape::Damaged));

    // History that stops.
    each(
        Shape::Shallow,
        "rev-list",
        &[&["rev-list", "--count", "HEAD"]],
        out,
    );
    each(Shape::Shallow, "merge-base", &[&["merge-base", "HEAD", "sh-side"]], out);
}
