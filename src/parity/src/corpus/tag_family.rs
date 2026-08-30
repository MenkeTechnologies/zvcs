//! `git tag` as a *writer* and as a *listing engine*: the message that becomes
//! the tag object's bytes, the object the tag names, the name the ref is given,
//! the order and shape of the listing, and what deletion leaves behind.
//!
//! # How this file divides territory with its neighbours
//!
//! Every module below was read before a case was written here, and each of them
//! already owns a piece of `tag`. What this file adds is stated per neighbour so
//! the boundary is checkable rather than asserted:
//!
//! * **`tag_describe.rs`** — the nearest neighbour, read in full. It owns the
//!   `tag`/`describe`/`verify-tag`/`verify-commit`/`mktag` *sweep*: one case per
//!   creation form (`-a`, implied `-a`, `-F -`, `--edit`, `-f` with a rev,
//!   `--cleanup=verbatim`, one `--trailer`), one case per listing flag
//!   (`-l`, `-n`, `--sort=-refname`, `--sort=version:refname`, `--contains`,
//!   `--merged`, `--points-at`, `--column`, `--omit-empty`, `--ignore-case`),
//!   and the `describe`/`mktag` families outright. It measures *that each flag
//!   is reached*. This file measures the parts of the same flags that need two
//!   values to be separable: `-m` given **twice**, `-F` from a **file** rather
//!   than stdin (including a file whose whole content is a comment), the three
//!   `--cleanup` modes that disagree with each other and the invalid fourth,
//!   `--trailer` twice, `--sort` given **twice** (which key wins), the four
//!   filter flags **paired**, and the tag-object bytes each of those produces.
//!   Nothing here repeats an argv it already has.
//! * **`naming_ancestry.rs`** — `describe`, `name-rev`, `show-branch`,
//!   `merge-base`: naming a commit *after* a tag. It contains no `tag` case at
//!   all (`grep '"tag"'` over it is empty), so the creation side of the tag it
//!   names is entirely this file's.
//! * **`plumbing_refs.rs`** — `show-ref`, `for-each-ref`, `update-ref`,
//!   `symbolic-ref`, `pack-refs`, `reflog`, `check-ref-format`. Also no `tag`
//!   case. It owns `check-ref-format` as a *verb*; this file owns the same
//!   rule table as `tag` applies it, which is a different code path
//!   (`builtin/tag.c:check_tag_ref` → `check_refname_format` with
//!   `REFNAME_ALLOW_ONELEVEL`) and a different diagnostic
//!   (`'x' is not a valid tag name.`, not `check-ref-format`'s silent exit 1).
//! * **`ref_storage.rs`** — the physical store: `packed-refs`, reftable,
//!   `extensions.refStorage`, transactions. No `tag` case. See "not measurable"
//!   below for why loose-vs-packed `refs/tags/` after a delete stays its
//!   problem and not this one.
//! * **`revision_syntax.rs`** — the rev-spec grammar behind forty verbs. Its
//!   only `tag` case is `tag --points-at inner` on [`Shape::TagChain`], proving
//!   `--points-at` peels the refs it scans. This file does not touch
//!   `--points-at` on that shape.
//! * **`interchange.rs`** — `bundle`/`fast-export`/`fast-import`. No `tag` case.
//! * **`misc_commands.rs`** — the leftovers; its one `tag` case is
//!   `tag --bogus` (strict), the unknown-switch usage dump. This file therefore
//!   deliberately ships **no** case whose stderr is the multi-screen usage
//!   block: `tag -a -l` and `tag -l -m msg` were probed, produce exactly that
//!   block, and would be a second copy of a measurement already made.
//! * **`config_reads.rs`** — settings whose whole effect is on a read; its one
//!   `tag` case is `tag.sort=-refname` on [`Shape::TagChain`]. This file adds
//!   the *failing* value of the same key (`tag.sort=nosuchkey`, a `fatal:` at
//!   128) and the keys that change a **write**: `core.commentChar` and
//!   `tag.gpgSign` beside `--no-sign`.
//! * **`fixture_gaps2.rs` / `fixture_gaps3.rs`** — the shape-reach sweeps for
//!   [`Shape::TagChain`] and [`Shape::AmbiguousRef`]. They own `tag -l`,
//!   `-l -n1`, `-d outermost|outer|inner|blobtag`, `--points-at HEAD~2`,
//!   `--contains HEAD~2`, `-v outermost` and one nested-tag creation on the
//!   chain, plus `-d ambi` and `-f ambi main` on the ambiguous shape. This file
//!   uses `TagChain` only where six tags with *different peel targets* are the
//!   point — multi-key sorting and `--sort=objectsize` — and deletes only the
//!   two names those sweeps leave alone (`light-to-tag`, `treetag`).
//! * **`exit_codes.rs`** owns `tag -d` with no name, `tag -d -l` and
//!   `tag -d main`; **`hooks_identity.rs`** owns `tag.gpgSign=true` and `-s`
//!   against a missing gpg. Neither argv appears here.
//!
//! # Determinism, verified rather than assumed
//!
//! **An annotated tag object is byte-reproducible.** `env::harden` pins
//! `GIT_COMMITTER_NAME`/`_EMAIL`/`_DATE`, and `builtin/tag.c` writes the tagger
//! line from the committer identity, so the `tagger` header is a constant.
//! Checked directly rather than reasoned about: the same `tag -a rep -m
//! 'reproducible body'` run against two independently built copies of
//! [`Shape::Branched`] under `harden`'s environment gave
//! `842ca240892537ed48b9a92d1f7a719a107d2a2b` both times, with
//! `cat-file tag rep` ending `tagger zvcs parity <parity@example.invalid>
//! 1700000000 +0000`. That is what makes every creating case in this file worth
//! running: a successful `git tag` prints nothing, so the finding lives in
//! `probe_state`'s `cat-file --batch-check --batch-all-objects`, and a message
//! cleaned differently or a header emitted in a different order is a different
//! oid.
//!
//! **The default cleanup mode for a tag message is `strip`, not `whitespace`.**
//! Measured on stock 2.55.0 over [`Shape::Branched`], on the exact message the
//! cases below pass (`body\n# comment\n\n\n`): `--cleanup=strip` and the bare
//! `-m` form both store the body `\nbody\n` (tag objects
//! `24d4972b318ae1d5ef90f1bddf23b3cb4d7a0d4d` and
//! `1f2263d9805d313b1e0f057d5544313066c8a0c0` — different only because the tag
//! names differ), while `--cleanup=whitespace` stores `\nbody\n# comment\n`
//! (`ef5636ab92330e1ea2662699a4fe932184f9ddec`). Two rules separate the modes and
//! only one of them is about comments: **`whitespace` already drops the trailing
//! blank lines and the trailing spaces**, so a message with no comment line in it
//! cannot tell `strip` from `whitespace` at all — checked on `\n\n  body  \n\n`,
//! where both store `\n  body\n`. `verbatim` is the only mode that keeps that
//! input whole, and the `vleadstrip`/`vverb` pair below is built on exactly that
//! difference rather than on a comment.
//!
//! **`-F` runs the same cleanup as `-m`.** `tag -F README.md vempty` — a file
//! whose whole content is `# fixture\n` — writes a tag with an *empty* message
//! under the default mode (`0ad6b3bc8702145ecf37145ca6bdf2134833740e`, 133 bytes,
//! ending `+0000\n\n`), and the same file with `--cleanup=verbatim` keeps the
//! line. So the byte-level cleanup rules are reachable from both message sources
//! and the `-F` half is not a restatement of the `-m` half.
//!
//! **Signing is unreachable and nothing here pretends otherwise.** There is no
//! gpg key in the harness and `harden` gives the child a fresh `HOME`. The one
//! signing-adjacent case here is `--no-sign` *overriding* `tag.gpgSign=true`,
//! which is the path that never invokes gpg at all — the refusal path when it
//! does is already owned by `hooks_identity.rs` and `tag_describe.rs`.
//!
//! # What is not measurable here, stated rather than papered over
//!
//! * **`--sort=taggerdate` and `--sort=creatordate` are degenerate in this
//!   harness and no case here claims otherwise.** Every commit and every tag in
//!   every shape carries `env::FIXED_DATE`, so on [`Shape::TagChain`] all six
//!   tags report `taggerdate:unix` = `creatordate:unix` = `1700000000`, and
//!   `--sort=taggerdate`, `--sort=-taggerdate`, `--sort=creatordate` and
//!   `--sort=-creatordate` all print the identical six lines in refname order.
//!   A case asserting a date ordering would pass for the wrong reason. There is
//!   no way out from inside a case: separating the keys needs two different
//!   timestamps, `GIT_COMMITTER_DATE` is pinned by `harden` and
//!   `env::is_pinned` forbids a case from re-pointing it. One case is kept from
//!   that family and is labelled for what it actually measures —
//!   `--sort=-taggerdate` over an all-ties key, which is a statement about the
//!   **stability of the sort**, not about dates.
//! * **Loose versus packed `refs/tags/` after a delete is not reachable from a
//!   case.** No shape has a `packed-refs` file (`grep pack-refs fixture.rs` is
//!   empty), a case is one argv so it cannot `pack-refs` first, and this file
//!   may not add a shape. Deleting a packed tag — the case where the ref file
//!   never existed and the `packed-refs` line has to be rewritten — therefore
//!   stays unmeasured, and belongs to `ref_storage.rs`, which owns that store.
//! * **Two reachability filters cannot be told apart from one on any shape this
//!   file may use.** Measured on stock over [`Shape::Branched`]: `--points-at
//!   HEAD`, `--contains HEAD~1`, `--contains HEAD`, `--merged feature` and
//!   `--merged HEAD` each answer `v0.1.0 v0.2.0`, while `--contains feature`,
//!   `--no-contains HEAD` and `--no-merged feature` each answer nothing. Over
//!   [`Shape::TagChain`] the same all-or-nothing split holds: every
//!   commit-reachability filter answers either `inner light-to-tag outer
//!   outermost` or nothing, because `blobtag` and `treetag` peel to a blob and a
//!   tree and are excluded from all of them. So for any pair `A`, `B` of these
//!   flags, `A ∩ B` equals `A` or equals `B`, and a case can separate **AND from
//!   OR** (when one half is empty and the other is not) but never AND from
//!   "the last flag wins". The pairs below are labelled with which of the two
//!   questions each one actually answers; none claims the second.
//! * **`--create-reflog` is fully measured, contrary to an earlier note here.**
//!   `probe_state` calls `probe_reflogs`, which walks `.git/logs` and prints
//!   every file verbatim, so the log line the flag creates is compared as well
//!   as the ref and the object. Checked on stock: the case leaves
//!   `.git/logs/refs/tags/vreflog-ann` holding a single line ending
//!   `tag: tagging 5915d79 (add two, 2023-11-14)`, and the date in it is
//!   `env::FIXED_DATE`'s, so the line is reproducible.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    creation_messages(out);
    creation_targets(out);
    creation_force(out);
    naming_refusals(out);
    listing_sort(out);
    listing_filters(out);
    listing_format(out);
    deletion(out);
}

/// A [`Shape::Branched`] case, non-strict.
fn b(args: &[&str], out: &mut Vec<Case>) {
    out.push(Case::new("tag", args, Shape::Branched));
}

/// A [`Shape::Branched`] case with stderr compared too.
fn bs(args: &[&str], out: &mut Vec<Case>) {
    out.push(Case::strict("tag", args, Shape::Branched));
}

// ---------------------------------------------------------------------------
// creation: the message that becomes the object's bytes
// ---------------------------------------------------------------------------

/// Where a tag message comes from, and what is done to it before it is stored.
///
/// Every case here is silent on stdout and is scored by the object it left
/// behind. The pairs are chosen so the two members produce *different bytes*
/// from the same input: a port that concatenates `-m` without the blank line, or
/// that applies one cleanup rule to all four modes, or that ignores
/// `core.commentChar`, writes a different oid and is caught by
/// `cat-file --batch-check --batch-all-objects` even though nothing was printed.
fn creation_messages(out: &mut Vec<Case>) {
    // Two `-m`s are joined by a **blank line**, not by a newline — the same rule
    // `commit` uses. Measured on stock: the object body is
    // `first para\n\nsecond para\n`. `tag_describe.rs` only ever passes one.
    b(&["tag", "-a", "vmulti", "-m", "first para", "-m", "second para"], out);

    // `-F <file>` rather than `-F -`. Two files with opposite outcomes:
    //
    //  * `src/lib.rs` is two lines of real content and lands verbatim.
    //  * `README.md` is `# fixture\n` — one comment line, which the default
    //    `strip` cleanup removes entirely. Git writes the tag anyway, with an
    //    **empty** message, and does not raise the `no tag message?` refusal that
    //    `-a` with no message at all raises. That asymmetry (the check belongs to
    //    the editor path, not to the message) is the whole point of the pair, and
    //    a port that validates emptiness centrally refuses one of them.
    b(&["tag", "-F", "src/lib.rs", "vsrc"], out);
    b(&["tag", "-F", "README.md", "vempty"], out);
    // The same comment-only file with cleanup off: now the `# fixture` line
    // survives into the object, so the two cases differ by their oid.
    b(&["tag", "-F", "README.md", "--cleanup=verbatim", "vkeepcomment"], out);
    // `-e` beside `-F`: `GIT_EDITOR` is `true`, so the buffer comes back
    // unchanged and the cleaned-to-empty message is still accepted.
    b(&["tag", "-e", "-F", "README.md", "vedit-file"], out);

    // `-F` and `-m` are mutually exclusive. A one-line `fatal:`, so strict.
    bs(&["tag", "-a", "-F", "README.md", "-m", "also", "vboth"], out);
    // A path that is not there: the message is read before any ref is touched.
    bs(&["tag", "-F", "nosuchfile", "vmissingfile"], out);

    // The cleanup modes, on one message that every mode treats differently.
    // `strip` is the documented default (verified: the bare `-m` form below
    // produces the identical object to the explicit `--cleanup=strip` one), so
    // the pair also proves the default is not `whitespace`.
    b(&["tag", "-a", "vstrip", "-m", "body\n# comment\n\n\n", "--cleanup=strip"], out);
    b(&["tag", "-a", "vws", "-m", "body\n# comment\n\n\n", "--cleanup=whitespace"], out);
    b(&["tag", "-a", "vdefault", "-m", "body\n# comment\n\n\n"], out);
    // `scissors` is a *valid* mode for `commit` and an invalid one for `tag`;
    // a port that shares one cleanup-mode table between the two accepts it.
    bs(&["tag", "-a", "vsciss", "-m", "body", "--cleanup=scissors"], out);
    bs(&["tag", "-a", "vbogus", "-m", "body", "--cleanup=bogus"], out);

    // The comment character cleanup strips is configurable. With `;` chosen,
    // `; semi` goes and `# hash` stays — the exact inverse of the default — so
    // the setting is measured by the object's bytes rather than by being
    // accepted. Verified on stock: the body is `body\n# hash\n`.
    out.push(
        Case::new("tag", &["tag", "-a", "vcc", "-m", "body\n; semi\n# hash\n"], Shape::Branched)
            .with_config(&[("core.commentChar", ";")]),
    );

    // Two trailers, which is where the interpret-trailers machinery has to add
    // exactly one blank line before the block and none between its lines.
    // `tag_describe.rs` passes one trailer, which cannot show either rule.
    b(&["tag", "-a", "vtrailers", "-m", "body", "--trailer", "Acked-by=A", "--trailer", "Reviewed-by=B"], out);
    // A trailer added to a message that *already ends in a trailer block*. The
    // rule is different from the one above: no second blank line is inserted, the
    // new line joins the existing block. Measured on stock, the stored body is
    // `\nbody\n\nAcked-by: A\nReviewed-by: B\n`
    // (`071eabd16c9b50a116f3f54b1b61a1b27948a2d3`); a port that always prefixes a
    // blank line before the block writes a different object.
    b(&["tag", "-a", "vtrail2", "-m", "body\n\nAcked-by: A\n", "--trailer", "Reviewed-by=B"], out);

    // The blank-line half of cleanup, on a message with no comment in it at all.
    // `strip` and `whitespace` agree here (see the module header), so the pair
    // that separates is default-versus-`verbatim`: stock stores `\n  body\n` for
    // the first and `\n\n\n  body  \n\n` for the second
    // (`4d4503d3d6a94febfd1951e1ccfa97d1f1ca2fb7` and
    // `6852159f0bb17ec369a4dd05d27cdaec17da379d`). Leading *spaces* survive both;
    // only the leading and trailing blank lines and the trailing spaces go.
    b(&["tag", "-a", "vleadstrip", "-m", "\n\n  body  \n\n"], out);
    b(&["tag", "-a", "vverb", "-m", "\n\n  body  \n\n", "--cleanup=verbatim"], out);

    // `strip` removes a comment line, not a comment *character*. Measured: the
    // body is stored whole as `\nbody # not a comment\n`, so a port that strips
    // from the first comment character rather than from the start of a line
    // truncates it.
    b(&["tag", "-a", "vmid", "-m", "body # not a comment"], out);

    // An empty second `-m`. The blank-line join is not applied to it: stock
    // stores `\nfirst\n` (`dfe8d1cf9e18c4b80f96dcd906603cde5d638040`), the same
    // body a single `-m first` gives, rather than `first\n\n`. The pair with
    // `vmulti` above is what separates "joins with a blank line" from "joins with
    // a blank line per operand".
    b(&["tag", "-a", "vmempty", "-m", "first", "-m", ""], out);

    // `core.commentChar=auto` is accepted by the config parser and, measured on
    // stock, changes nothing about a *tag* message: with `body\n# hash\n` the
    // stored body is `\nbody\n` — `#` is still the comment character and the line
    // is still stripped. Two ports fail here in opposite directions: one that
    // rejects `auto` as not a single character, and one that implements the
    // auto-selection (which would keep `# hash`, because `#` occurs in the
    // message). `core.commentChar=auto` does appear elsewhere — `informational.rs`
    // aims it at `stripspace -c` — and `commit_family.rs`'s header leaves it out
    // of `commit` because 2.55 warns there; measured on `tag` it is silent, both
    // streams empty at rc 0, which is why this is strict.
    out.push(
        Case::strict("tag", &["tag", "-a", "vauto", "-m", "body\n# hash\n"], Shape::Branched)
            .with_config(&[("core.commentChar", "auto")]),
    );
}

// ---------------------------------------------------------------------------
// creation: what the tag names
// ---------------------------------------------------------------------------

/// The object operand, and the ref name that ends up beside it.
///
/// `tag_describe.rs` covers `HEAD^{}`, `HEAD^{tree}`, an abbreviated id and a
/// *lightweight* ref at a tag object. What is added here is the annotated tag
/// **on** a non-commit and **on** another tag, where git has a hint to print,
/// plus the two name shapes that collide with something else in the repository.
fn creation_targets(out: &mut Vec<Case>) {
    // An annotated tag whose target is a blob named by a path-in-revision.
    // The object records `type blob`; a port that peels to a commit before
    // writing the header stores a different object under the same name.
    b(&["tag", "-a", "von-blob", "-m", "blob tag", "HEAD:README.md"], out);

    // Annotating a tag object: git creates it and prints the four-line
    // `nested tag` hint on stderr, naming the `-f <name> <target>^{}` fix.
    // Strict, because the hint is the entire observable difference from the
    // silent success beside it. `fixture_gaps2.rs` has the non-strict form of
    // this on [`Shape::TagChain`]; here it is measured with its text.
    bs(&["tag", "-a", "vnested", "-m", "over an annotated tag", "v0.2.0"], out);
    // The same creation with the advice silenced: same object, no stderr. The
    // pair is what separates "does not print the hint" from "does not know the
    // setting".
    out.push(
        Case::strict("tag", &["tag", "-a", "vquiet", "-m", "over an annotated tag", "v0.2.0"], Shape::Branched)
            .with_config(&[("advice.nestedTag", "false")]),
    );

    // A tag whose name is also a branch name. Git accepts it without a word, and
    // the repository is left with `refs/heads/feature` and `refs/tags/feature`
    // both live — which is the state `AmbiguousRef` is built to *have* and that
    // no case could previously *create*.
    bs(&["tag", "feature"], out);
    // A hierarchical tag name: `refs/tags/a/b/c`, three levels under the prefix.
    b(&["tag", "a/b/c"], out);
    // A name that already spells a full ref. It is not stripped: the ref written
    // is `refs/tags/refs/tags/plain`, which is exactly why `tag -d
    // refs/tags/<name>` below cannot find an ordinary tag.
    b(&["tag", "refs/tags/plain"], out);
}

// ---------------------------------------------------------------------------
// creation: force, and the settings that gate a write
// ---------------------------------------------------------------------------

/// `-f`, and the two config keys that change what a *write* produces.
fn creation_force(out: &mut Vec<Case>) {
    // `-f` with no rev, over a tag that is already at `HEAD`. The ref does not
    // move, so git prints **nothing** — `tag_describe.rs`'s `tag -f v0.1.0
    // HEAD~1` prints `Updated tag 'v0.1.0' (was 5915d79)`. The line is emitted
    // on the change, not on the flag, and a port that announces every forced
    // write disagrees only here.
    b(&["tag", "-f", "v0.1.0"], out);
    // Forcing over the *annotated* tag: the `(was …)` id is the tag object's,
    // not the commit's, and the superseded tag object stays in the store for
    // `--batch-all-objects` to find beside the new one.
    b(&["tag", "-a", "--force", "v0.2.0", "-m", "again"], out);
    // Without `-f`, an annotated re-creation is `fatal:` at 128 — a different
    // exit code and a different word from `tag -d`'s `error:` at 1.
    bs(&["tag", "-a", "v0.2.0", "-m", "again"], out);

    // `--no-sign` has to beat `tag.gpgSign=true`. gpg is pointed at nothing, so
    // a port that consults the config without honouring the flag cannot fail
    // quietly: it either writes the ordinary unsigned object both sides agree on
    // or it dies trying to exec. This is the one signing case in this file and
    // it is the one that never reaches gpg.
    out.push(
        Case::new("tag", &["tag", "--no-sign", "-a", "vnosign", "-m", "msg"], Shape::Branched)
            .with_config(&[("tag.gpgSign", "true"), ("gpg.program", "/nonexistent-gpg")]),
    );

    // `--create-reflog` on an *annotated* tag. `tag_describe.rs` has the
    // lightweight form; the annotated one writes an object as well as a log, so
    // the half the probe can see is bigger. See the module header for the half
    // it cannot.
    b(&["tag", "--create-reflog", "-a", "vreflog-ann", "-m", "msg"], out);
}

// ---------------------------------------------------------------------------
// creation: names git refuses
// ---------------------------------------------------------------------------

/// `check_refname_format` with `REFNAME_ALLOW_ONELEVEL`, as `tag` applies it.
///
/// One name per rule rather than one representative, because the rules are
/// separate tests in `refs.c:check_refname_component` and a port that implements
/// three of the five passes a single-case check. `tag_describe.rs` covers
/// `bad..name` (the doubled-dot rule) and says one representative is enough; it
/// is enough for *that* rule. The refusal text is identical in shape for all of
/// them, so every case is strict and the finding is which inputs reach it.
fn naming_refusals(out: &mut Vec<Case>) {
    // A space: forbidden anywhere in a component.
    bs(&["tag", "bad name"], out);
    // `~` is one of the six characters a revision grammar claims.
    bs(&["tag", "bad~name"], out);
    // A component may not end in `.lock`.
    bs(&["tag", "bad.lock"], out);
    // `@{` is the reflog-selector sequence.
    bs(&["tag", "bad@{x}"], out);
    // The empty name: rejected by the same function, before the ref store.
    bs(&["tag", ""], out);
    // Not a name rule at all — a third positional. `fatal: too many arguments`
    // at 128, which a port that ignores extra operands never reaches.
    bs(&["tag", "-a", "vextra", "-m", "m", "HEAD", "extra"], out);
}

// ---------------------------------------------------------------------------
// listing: order
// ---------------------------------------------------------------------------

/// `--sort`, in the forms that need more than two tags or more than one key.
///
/// [`Shape::TagChain`] is used for the multi-key cases because it is the only
/// shape whose tags have **different peel targets** — four commits, one blob,
/// one tree — so `*objecttype` is a real key with three values and can be the
/// primary key of a two-key sort with a visible effect. [`Shape::Branched`] has
/// two tags and every ordering of them is one of two lines, which is why
/// `tag_describe.rs`'s single-key cases live there and these do not.
fn listing_sort(out: &mut Vec<Case>) {
    let tc = |args: &[&str], out: &mut Vec<Case>| out.push(Case::new("tag", args, Shape::TagChain));

    // Two keys, both orders. The **last** `--sort` is the most significant, and
    // the two orderings of the same pair are different listings:
    //
    //   --sort=*objecttype --sort=-refname
    //     treetag outermost outer light-to-tag inner blobtag   (refname wins)
    //   --sort=-refname --sort=*objecttype
    //     blobtag outermost outer light-to-tag inner treetag   (type wins)
    //
    // A port that treats the first key as primary — the reading most people
    // start from — swaps these two and nothing else in the corpus notices.
    tc(&["tag", "--sort=*objecttype", "--sort=-refname", "--format=%(refname:short) %(*objecttype)"], out);
    tc(&["tag", "--sort=-refname", "--sort=*objecttype", "--format=%(refname:short) %(*objecttype)"], out);

    // A numeric key with real ties. Measured: 146, 146, 152, 152, 156, 164 — so
    // it orders by size *and* keeps refname order inside each tie, which a port
    // sorting with an unstable comparator loses.
    tc(&["tag", "--sort=objectsize", "--format=%(refname:short) %(objectsize)"], out);

    // An all-ties key. Every tag carries `env::FIXED_DATE`, so this measures the
    // **stability** of the sort and not a date ordering: git prints refname
    // order, and prints the *same* refname order for `-taggerdate` as for
    // `taggerdate` because reversing a comparator that always returns equal
    // changes nothing. A port that reverses the array rather than the comparator
    // prints these six lines backwards. Labelled here so nobody later reads it
    // as evidence that date sorting works — see the module header.
    tc(&["tag", "--sort=-taggerdate", "--format=%(taggerdate:unix) %(refname:short)"], out);

    // The documented abbreviation of `version:refname`, which is a separate
    // entry in the atom table rather than a prefix match.
    tc(&["tag", "--sort=v:refname"], out);

    // `tag.sort` with a key that does not exist: `fatal: unknown field name:
    // nosuchkey` at 128. `config_reads.rs` sets the same key to a value that
    // works; the failing value is a different code path (the parse happens while
    // reading the config, not while sorting).
    out.push(
        Case::strict("tag", &["tag"], Shape::Branched)
            .with_config(&[("tag.sort", "nosuchkey")]),
    );
}

// ---------------------------------------------------------------------------
// listing: which tags
// ---------------------------------------------------------------------------

/// Filters, and — the part no single-flag case can reach — two of them at once.
///
/// The four reachability flags are ANDed. What a pair of them can *show* on the
/// shapes available here is bounded, and the bound was measured rather than
/// assumed — see the module header: on [`Shape::Branched`] every one of these
/// flags answers either both tags or neither, so a pair can separate an AND from
/// an OR and cannot separate an AND from "the last flag wins". Each case below
/// says which of those it is.
fn listing_filters(out: &mut Vec<Case>) {
    // Both halves admit both tags (measured: `--contains HEAD~1` → `v0.1.0
    // v0.2.0`, `--merged feature` → `v0.1.0 v0.2.0`). Every combining rule gives
    // the same answer, so this separates nothing about the combination; what it
    // does measure is that a second reachability flag is *accepted* beside the
    // first rather than rejected or ignored — a port that ignored either one
    // would still print both tags, but a port that rejected the pair, or that
    // dropped to the unfiltered list, disagrees.
    b(&["tag", "--contains", "HEAD~1", "--merged", "feature"], out);
    // The AND-versus-OR case. `--points-at HEAD` admits both tags, `--contains
    // feature` admits neither, so stock prints nothing; a port that ORs the two
    // prints both.
    b(&["tag", "--points-at", "HEAD", "--contains", "feature"], out);
    // Two *negative* filters. Both halves are empty on this shape (both tags
    // contain `HEAD` and neither is unmerged into `feature`), so stock prints
    // nothing and the combining rule is again invisible. It measures that
    // neither negation is dropped: a port that ignores both prints two tags.
    // `tag_describe.rs` has `--no-contains HEAD~1` and `--no-merged feature`
    // singly; this is the only place the two appear together.
    b(&["tag", "--no-contains", "HEAD", "--no-merged", "feature"], out);
    // A filter beside a pattern, which *is* a strict narrowing: the filter
    // admits `v0.1.0 v0.2.0` and the pattern admits `v0.2.0`, so stock prints
    // one line and a port that ORs the two prints two.
    b(&["tag", "--points-at", "HEAD", "-l", "v0.2*"], out);
    // A bracket class in the pattern, which `wildmatch` handles and a port that
    // reaches for `fnmatch`'s defaults or for a plain prefix test does not.
    b(&["tag", "-l", "v0.[12].0"], out);
    // An empty pattern matches nothing at all — not everything, which is the
    // reading a port that treats "" as "no pattern given" produces.
    b(&["tag", "-l", ""], out);

    // `--merged`/`--no-merged` with the argument omitted default to `HEAD`.
    // Both are printed here because they are complementary over one tag set, so
    // a port that defaults one of them to something else has to disagree on one.
    b(&["tag", "--merged"], out);
    b(&["tag", "--no-merged"], out);

    // A filter flag outside list mode. `fatal: the '--contains' option is only
    // allowed in list mode` at 128 — and *not* the usage dump that `tag -a -l`
    // produces, so the two refusals are different code paths.
    bs(&["tag", "--contains", "HEAD", "-d", "v0.1.0"], out);
}

// ---------------------------------------------------------------------------
// listing: how each row is rendered
// ---------------------------------------------------------------------------

/// `-n<num>`, `--format` atoms, and the column layout.
///
/// The atoms here are the ones that are *empty for a lightweight tag and not for
/// an annotated one*, so every case has one row of each on
/// [`Shape::Branched`] and a port that renders an absent atom as an error, or
/// that peels before reading it, disagrees on exactly one of the two lines.
fn listing_format(out: &mut Vec<Case>) {
    // `-n0` suppresses the annotation column that `-n` (== `-n1`) prints; `-n3`
    // asks for more lines than either message has, so both messages are printed
    // whole and the column is padded, not truncated. `tag_describe.rs` has the
    // bare `-n`, which is the middle of the three.
    b(&["tag", "-n0"], out);
    b(&["tag", "-n3"], out);
    // `-n` with a bare operand: the operand becomes a *pattern*, so the presence
    // of `-n` is what puts git in list mode without `-l`.
    b(&["tag", "-n1", "v0.1.0"], out);
    // `-n` beside a filter, which is the combination that makes the annotation
    // column appear for a subset.
    b(&["tag", "-n1", "--contains", "HEAD~1"], out);

    // `%(contents)` is the whole message including its trailing newline, so the
    // listing gains a blank line per tag — the atom `tag_describe.rs` reaches
    // only through `:subject`. For the lightweight tag it is the tagged
    // *commit's* message, which is the fallback that makes the two rows differ.
    b(&["tag", "--format=%(contents)"], out);
    // `:body` is empty for both (neither message has a second paragraph) and
    // `:lines=1` is not: the pair separates "the atom is unimplemented" from
    // "the message legitimately has no body".
    b(&["tag", "--format=[%(contents:body)]"], out);
    b(&["tag", "--format=[%(contents:lines=1)]"], out);
    // `:signature` is empty on an unsigned tag rather than an error. A port that
    // rejects the modifier because it never produces one fails here.
    b(&["tag", "--format=[%(contents:signature)]"], out);
    // The four raw tag-object header atoms. Every one of them is empty for the
    // lightweight row, so stock prints a line of three spaces above the
    // annotated row's full one — the shape a port that skips unannotated refs
    // never emits.
    b(&["tag", "--format=%(object) %(type) %(tag) %(tagger)"], out);
    // `%(align)` pads to a fixed width, which is how a difference in the *value*
    // of an atom shows up as a difference in column position rather than only in
    // text.
    b(&["tag", "--format=%(align:20,left)%(refname:short)%(end)|"], out);

    // Columns with an explicit style list. `tag_describe.rs` covers bare
    // `--column`/`--no-column` and `column.tag`; `column.ui` is the *general*
    // key, and the `--no-column` case beside it proves the flag beats the
    // config rather than merely being accepted.
    b(&["tag", "--column=always,dense"], out);
    out.push(
        Case::new("tag", &["tag"], Shape::Branched).with_config(&[("column.ui", "always")]),
    );
    out.push(
        Case::new("tag", &["tag", "--no-column"], Shape::Branched)
            .with_config(&[("column.ui", "always")]),
    );

    // `-v` with a format over a lightweight *and* an annotated name: the run
    // does not stop at the first refusal, the two refusals have different
    // wording (`cannot verify a non-tag object of type commit.` versus
    // `no signature found`), and the format renders for neither because it is
    // applied only after verification succeeds.
    bs(&["tag", "-v", "--format=%(tag) %(objecttype)", "v0.1.0", "v0.2.0"], out);
}

// ---------------------------------------------------------------------------
// deletion
// ---------------------------------------------------------------------------

/// `-d`: what is reported, in what order, and what survives.
///
/// The reporting is split across both streams — `Deleted tag …` on stdout, one
/// `error:` per missing name on stderr — so the strict cases here are the only
/// way to see that git *keeps going* rather than stopping at the first miss.
fn deletion(out: &mut Vec<Case>) {
    // Three names, the middle one absent. Stock deletes both real tags, reports
    // the miss, and exits 1. A port that aborts on the first error leaves
    // `v0.2.0` behind and is caught by the state probe as well as by stdout.
    bs(&["tag", "-d", "v0.1.0", "nosuchtag", "v0.2.0"], out);
    // The long spelling.
    b(&["tag", "--delete", "v0.2.0"], out);
    // A full ref name is *not* accepted: `-d` takes a short name and prefixes
    // it, so this looks for `refs/tags/refs/tags/v0.1.0` and reports the name
    // the user typed. The pair with `tag refs/tags/plain` above is what shows
    // the two ends of the same rule.
    bs(&["tag", "-d", "refs/tags/v0.1.0"], out);
    // The same name twice in one call. Both resolve, and the ref transaction
    // refuses the batch as a whole: `error: could not delete references:
    // multiple updates for ref 'refs/tags/v0.1.0' not allowed`, exit 1, and
    // **nothing is deleted** — a port that loops over the names one at a time
    // deletes the tag and reports success on the first pass.
    bs(&["tag", "-d", "v0.1.0", "v0.1.0"], out);
    // A format beside `-d` is accepted and ignored: the deletion report is not
    // a formatted listing, so `--format` must not reach it.
    b(&["tag", "--format=%(refname)", "-d", "v0.1.0"], out);

    // On the chain, the two names the `fixture_gaps2.rs` sweep does not delete:
    //
    //  * `light-to-tag` is a *lightweight* ref whose target is a tag object, so
    //    the `(was …)` id is the tag object's and the tag object survives,
    //    reachable through `inner`.
    //  * `treetag` is the only tag on a tree; deleting it makes that tag object
    //    unreachable while leaving the tree, which is a different post-state
    //    from deleting the blob tag the sweep already covers.
    out.push(Case::strict("tag", &["tag", "-d", "light-to-tag"], Shape::TagChain));
    out.push(Case::strict("tag", &["tag", "-d", "treetag"], Shape::TagChain));
}
