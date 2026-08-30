//! The refspec **matcher**: `[+]<src>[:<dst>]`, `^<src>`, wildcards, and the
//! algorithm that decides which refs move and where.
//!
//! A refspec is a small language with its own grammar, its own validity rules
//! and its own matching algorithm, and every transport verb is a *caller* of
//! that one algorithm (`refspec.c` parses, `remote.c:match_name_with_pattern`
//! and `get_fetch_map`/`match_push_refs` match). The corpus tested the callers
//! and never the language: `fetch_clone.rs` and `branch_remote.rs` ask what
//! `fetch` and `push` do, so a matcher bug can only be seen there through
//! whichever verb happens to surface it, and the same bug then reads as two
//! unrelated findings. This module asks the language directly.
//!
//! # How this divides territory with the four adjacent modules
//!
//! * **`fetch_clone.rs`** owns `fetch`/`clone`/`bundle`/`ls-remote` and the
//!   pack-protocol verbs against `Shape::BehindRemote`'s peer. Its
//!   `fetch_refspecs` group covers the *ordinary* forms — `<src>`,
//!   `<src>:<dst>` in four spellings, two specs at once, the non-fast-forward
//!   wall — and its `fetch_bulk_and_prune` group covers `--all`, `--tags` and
//!   the four `--prune` spellings. None of it leaves the grammar's happy path:
//!   no negative refspec, no glob anywhere but a whole trailing component, no
//!   `<src>:` , no `:<dst>`, no bare `+`, no `--refmap`, and nothing that asks
//!   what happens when two specs claim one destination. Those are this file.
//! * **`branch_remote.rs`** owns `push` on the same peer, including its
//!   `push_defaults` group — the six `push.default` values and
//!   `remote.<n>.push`, all measured on a branch that **has** an upstream,
//!   because both branches of `BehindRemote` do. The other half of that cross —
//!   the same keys on a branch with **no** upstream — is unreachable there and
//!   is [`push_defaults_without_upstream`] here, on `Shape::Branched` with the
//!   remote supplied by configuration.
//! * **`transport_local.rs`** owns a repository used as its own remote
//!   (`fetch .`, `push .`, `clone . copy`) on `Linear`/`Branched`/`Merged`/
//!   `Detached`/`Submodule`. It uses `.` to reach a second ref namespace at all;
//!   this file uses `.` only where the *ambiguity* of the fixture's ref names is
//!   the thing being measured (`Shape::AmbiguousRef`), which is a shape it never
//!   touches.
//! * **`plumbing_refs.rs`** owns `show-ref`/`for-each-ref`/`update-ref` — the
//!   local ref store, no remote and therefore no refspec.
//! * **`graft_partial.rs`** owns the two repositories with objects missing. A
//!   matcher decides ref names, so nothing here depends on whether an object is
//!   present.
//!
//! # The fixtures, and what each one is here for
//!
//! * [`Shape::BehindRemote`] — the bare peer at `./.remote.git`, reached by a
//!   relative URL. Verified against stock 2.55.0 in a hand-built copy of the
//!   shape:
//!
//!   ```text
//!   $ git --git-dir=.remote.git for-each-ref
//!   79f764e… commit  refs/heads/div
//!   91bfcd8… commit  refs/heads/main
//!   ```
//!
//!   Two branches, **no tags**, and a **dangling `HEAD`** (`refs/heads/master`,
//!   which does not exist). The dangling HEAD is load-bearing for a whole group
//!   here: every refspec whose source is empty or absent — `:<dst>`, a bare `+`,
//!   a bare `:`, `HEAD`, `@` — falls back to the remote's HEAD and dies
//!   `fatal: couldn't find remote ref HEAD`. That is five different *parses*
//!   arriving at one message, which is exactly the kind of thing a port gets
//!   right for one spelling and wrong for the other four.
//! * [`Shape::Branched`] — two branches, a lightweight tag `v0.1.0` and an
//!   annotated `v0.2.0`, and **no remote**. Supplying `remote.origin.url=.` by
//!   configuration gives a remote whose advertisement carries tags (which the
//!   peer of `BehindRemote` cannot) and a current branch with **no upstream**
//!   (which neither branch of `BehindRemote` can).
//! * [`Shape::AmbiguousRef`] — `ambi` is a branch *and* a tag, `top` is
//!   `refs/top` *and* a branch *and* a tag, `rem/ambi` is a branch *and* a
//!   remote-tracking ref. The refspec matcher's source lookup is **not**
//!   `rev-parse`: it runs over the advertisement with its own precedence list,
//!   and `push` runs a third algorithm again that refuses rather than choosing.
//!   Measured with stock, and the three answers are three different objects:
//!
//!   ```text
//!   $ git fetch . ambi:refs/heads/pick     #  * [new tag]  ambi     -> pick
//!   $ git fetch . top:refs/heads/pick      #  * [new ref]  refs/top -> pick
//!   $ git push  . ambi:refs/heads/pushpick # error: src refspec ambi matches more than one
//!   ```
//!
//! # `push --dry-run`, and the known defect it has to be read around
//!
//! `src/extensions/src/porcelain/push.rs:564` guards the `pre-push` hook with
//! `if !f.dry_run`, and `no_verify` appears nowhere in that file — so the port
//! has `--dry-run` doing `--no-verify`'s job and `--no-verify` doing nothing.
//! That defect belongs to `push`, not to the matcher, and mixing the two would
//! make a matcher case unreadable.
//!
//! It is kept out rather than worked around: **every `--dry-run` case here runs
//! on a shape with no `pre-push` hook.** `fixture.rs` installs hooks in exactly
//! two shapes (`HooksFail` and `AmHooks`, via `install_hooks`), and none of the
//! three shapes used here is one of them. With no hook on disk the guard has
//! nothing to skip, so a `--dry-run` case measures the ref mapping the port
//! *reported* and nothing else. The complementary question — whether
//! `--no-verify` is honoured — needs a hook-bearing shape and is not asked here.
//!
//! # The refusal budget
//!
//! Thirty-one of the hundred and forty cases here exit non-zero, and every one
//! of them is `Case::strict`. For a matcher that is not a stderr-fishing
//! expedition: `fatal: invalid refspec '<spec>'`, `fatal: Cannot fetch both
//! <a> and <b> to <dst>`, `error: dst ref <dst> receives from more than one
//! src` and `error: src refspec <src> matches more than one` each **name the
//! offending spec**, which is the only place the matcher says what it thought
//! the spec meant. Two of the thirty-one (`push origin :` and the unforced
//! wildcard push) are not grammar refusals at all — they are the fixture's
//! diverged branches rejecting a legitimate mapping, and their report is the
//! matcher's answer just as a successful one would be.
//!
//! # What the peer probe compares, and what is still invisible
//!
//! `runner::probe_peer` no longer gates on the name `.remote.git`: `other_peers`
//! walks the fixture for anything shaped like a repository and `peer_section`
//! asks each one for `HEAD`, `for-each-ref --format=%(refname) %(objecttype)
//! %(objectname)`, `cat-file --batch-check --batch-all-objects`, the `storage_of`
//! object census, `reflog_listing` and `fsck --strict`. So for a push the *far*
//! side's ref set, the oid each ref carries, the objects that arrived and the
//! peer's own reflogs are all compared — a push that reported
//! `main -> newbr` and wrote nothing on the peer is caught, which it was not
//! when the probe stopped at the fixture's own git directory.
//!
//! Three things about the peer remain invisible, and they bound what a push case
//! here can claim:
//!
//! 1. **The peer's configuration.** `peer_section` runs no `config --list`, so a
//!    push that (say) wrote a remote stanza into the peer is unmeasured. Nothing
//!    below depends on peer config; `remote.<n>.mirror` is read from the *local*
//!    side, and its effect on the peer is a ref set, which is compared.
//! 2. **Loose-versus-packed ref storage on the peer.** `for-each-ref` answers
//!    identically either way and `storage_of` counts objects, not refs. A port
//!    that packs the peer's refs where stock leaves them loose scores a match.
//! 3. **Which refspec produced a given peer ref.** The probe sees the resulting
//!    ref set, so two different mappings that happen to land the same names are
//!    indistinguishable in state alone. That is why the push cases below are
//!    `Case::strict`: the `<src> -> <dst>` line of the report *is* the matcher's
//!    answer, and it is the only place the mapping itself is visible.

use crate::fixture::Shape;
use crate::runner::Case;

pub fn cases(out: &mut Vec<Case>) {
    grammar_forms(out);
    wildcard_shapes(out);
    multiple_matches(out);
    negative_refspecs(out);
    ambiguous_sources(out);
    configured_fetch_specs(out);
    configured_push_specs(out);
    push_defaults_without_upstream(out);
    matcher_probes(out);
    configured_spec_writers(out);
}

/// `fetch` against the peer, strict. Every fetch report is on stderr, so a
/// non-strict fetch case compares an empty stdout against an empty stdout.
fn f(args: &[&str], out: &mut Vec<Case>) {
    out.push(Case::strict("fetch", args, Shape::BehindRemote));
}

/// `push` against the peer, strict. The `<src> -> <dst>` line is the mapping.
fn p(args: &[&str], out: &mut Vec<Case>) {
    out.push(Case::strict("push", args, Shape::BehindRemote));
}

/// `Shape::Branched` with `origin` pointing at the fixture itself.
///
/// Two settings, always together: a URL and the default fetch refspec `clone`
/// would have written. Without the second there is no configured refmap, and
/// the opportunistic tracking update that half these cases measure never
/// happens.
const SELF_ORIGIN: &[(&str, &str)] =
    &[("remote.origin.url", "."), ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*")];

/// `SELF_ORIGIN` plus one more setting, since `with_config` takes the whole set.
fn self_origin_plus(extra: &[(&str, &str)]) -> Vec<(String, String)> {
    SELF_ORIGIN
        .iter()
        .chain(extra.iter())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A case on `Shape::Branched` whose remote is the fixture itself.
fn branched(cmd: &'static str, args: &[&str], extra: &[(&str, &str)]) -> Case {
    let owned = self_origin_plus(extra);
    let pairs: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    Case::strict(cmd, args, Shape::Branched).with_config(&pairs)
}

/// The shapes of `[+]<src>[:<dst>]` that are not `<src>:<dst>`.
///
/// `fetch_clone.rs` covers `<src>`, `<src>:<dst>` and `+<src>:<dst>`. What is
/// left is the grammar's edges, and they divide into two answers:
///
/// * **`<src>:` — an empty destination.** Not an error and not a no-op: the ref
///   is fetched to `FETCH_HEAD` only, exactly as a bare `<src>` is, and the
///   report says `* branch main -> FETCH_HEAD`. A port that treats the trailing
///   colon as a parse error, or that writes `refs/heads/` with an empty tail,
///   diverges on the first line.
/// * **Everything with no source — `:<dst>`, `:`, `+`, `HEAD`, `@`.** For
///   `fetch` these all resolve the *remote's* HEAD, and this peer's HEAD dangles,
///   so all five die `fatal: couldn't find remote ref HEAD`. Five parses, one
///   message: the value is that a port has to get all five to the same place.
///
/// The partial-name group is the other half. `heads/main`, `main` and `partial`
/// are each expanded by a different rule (source against the advertisement,
/// destination against the local ref store), and stock resolves all three —
/// `refs/heads/partial` is what lands in every case.
fn grammar_forms(out: &mut Vec<Case>) {
    // Empty destination, fully-qualified and short source.
    f(&["fetch", "origin", "refs/heads/main:"], out);
    f(&["fetch", "origin", "main:"], out);

    // No source at all, in three spellings — an empty left side, a bare `+`, and
    // the name the other two fall back to. `:` and `@` reach the identical
    // message by the identical path and are left out rather than filed twice.
    f(&["fetch", "origin", ":refs/heads/deleteme"], out);
    f(&["fetch", "origin", "+"], out);
    f(&["fetch", "origin", "HEAD"], out);

    // Partial names on each side and on both. All three land refs/heads/partial.
    f(&["fetch", "origin", "heads/main:refs/heads/partial"], out);
    f(&["fetch", "origin", "refs/heads/main:heads/partial"], out);
    f(&["fetch", "origin", "main:partial"], out);

    // The same three questions asked of `push`, where the answers differ. A
    // trailing colon is a *parse error* for push (`fatal: invalid refspec
    // 'refs/heads/div:'`) rather than the FETCH_HEAD path it is for fetch, and a
    // bare `+` is rejected by the parser instead of resolving HEAD. `HEAD` and
    // `@` both push the *local* HEAD and succeed, which is the mirror image of
    // the fetch answer above.
    p(&["push", "origin", "refs/heads/div:"], out);
    p(&["push", "origin", "HEAD:refs/heads/fromhead"], out);
    p(&["push", "origin", "@:refs/heads/fromat"], out);
    p(&["push", "origin", "heads/div:heads/partialpush"], out);
    p(&["push", "origin", "div:pushedshort"], out);
    p(&["push", "origin", "refs/heads/div:pushedfull"], out);
    // A bare `:` is not the HEAD path for push at all — it is the *matching*
    // refspec, and on this fixture both branches are behind or diverged, so it
    // rejects twice and prints the fast-forward hint block.
    p(&["push", "origin", ":"], out);
}

/// Where a `*` may appear, what it captures, and what it does when it captures
/// nothing.
///
/// Git's rule (`refspec.c:parse_refspec`) is not "globs are allowed": a pattern
/// refspec must have **exactly one** `*` on *each* side, and either both sides
/// are patterns or neither is. The four rejections below are the four ways to
/// break that, and each names the offending spec verbatim — which is why they
/// are strict.
///
/// What the `*` matches is a *substring*, not a path component, and that is the
/// part ports get wrong by reaching for a path-aware glob. Verified with stock:
///
/// ```text
/// $ git fetch origin 'refs/heads/*ain:refs/remotes/mid/*'   #  main -> mid/m
/// $ git fetch origin 'refs/heads/m*:refs/remotes/pre/*'     #  main -> pre/ain
/// ```
///
/// The capture is spliced into the destination's `*`, so a mid-component glob
/// produces a destination that has nothing to do with the source's shape. A
/// matcher that treats `*` as "one whole component" gets `mid/main` and
/// `pre/main`, matches on nothing, and is caught here.
///
/// Zero-width is the sharpest of these. `refs/heads/main*` matches
/// `refs/heads/main` with an *empty* capture, so the destination becomes
/// `refs/remotes/zero/` — a name with a trailing slash, which git will not
/// write. It does not die: it prints `error: * Ignoring funny ref
/// 'refs/remotes/zero/' locally` and exits **0**. Push reaches the same place by
/// a different road — the peer's `receive-pack` refuses it and the push exits 1
/// with `! [remote rejected] main -> zero/ (funny refname)`.
fn wildcard_shapes(out: &mut Vec<Case>) {
    // Substring capture, suffix and prefix.
    f(&["fetch", "origin", "refs/heads/*ain:refs/remotes/mid/*"], out);
    f(&["fetch", "origin", "refs/heads/m*:refs/remotes/pre/*"], out);
    // Zero-width capture, both directions.
    f(&["fetch", "origin", "refs/heads/main*:refs/remotes/zero/*"], out);
    f(&["fetch", "origin", "refs/heads/*main:refs/remotes/zpre/*"], out);
    p(&["push", "origin", "refs/heads/main*:refs/heads/zero/*"], out);

    // A pattern that matches nothing is silent and exits 0 — three different
    // ways to match nothing, because a port that special-cases one of them
    // (unknown namespace, unmatched component, unmatched prefix) still has the
    // other two.
    f(&["fetch", "origin", "refs/heads/*/fix:refs/remotes/nomatch/*"], out);
    f(&["fetch", "origin", "refs/nope/*:refs/remotes/nomatch/*"], out);
    f(&["fetch", "origin", "refs/heads/nope*:refs/remotes/nomatch/*"], out);

    // The four invalid shapes: glob on the source only, on the destination only,
    // twice on the source, twice on the destination.
    f(&["fetch", "origin", "refs/heads/*:refs/remotes/one/x"], out);
    f(&["fetch", "origin", "refs/heads/x:refs/remotes/one/*"], out);
    f(&["fetch", "origin", "refs/heads/*/*:refs/remotes/two/*"], out);
    // Push parses with the same code and must produce the same two refusals.
    p(&["push", "origin", "refs/heads/*:refs/heads/one"], out);
    p(&["push", "origin", "refs/heads/one:refs/heads/*"], out);

    // The forced wildcard, and a wildcard whose destination namespace the
    // configured refmap already owns: the second is silent because every mapping
    // it makes is one the configured spec made too, which is the deduplication
    // half of the same algorithm.
    f(&["fetch", "origin", "+refs/heads/*:refs/remotes/plus/*"], out);
    f(&["fetch", "origin", "refs/heads/main:refs/remotes/origin/main"], out);
    f(&["fetch", "origin", "refs/heads/div:refs/remotes/origin/div"], out);

    // A glob over the whole ref namespace, on the shape that has tags to catch:
    // `refs/*:refs/copies/*` maps heads and tags alike, and the destination keeps
    // the captured `heads/`/`tags/` prefix. The configured refmap still runs
    // alongside it, so the report carries both mappings.
    out.push(branched("fetch", &["fetch", "origin", "refs/*:refs/copies/*"], &[]));
    out.push(branched("fetch", &["fetch", "origin", "refs/tags/*:refs/tags/copy/*"], &[]));
    out.push(branched("fetch", &["fetch", "origin", "refs/tags/v0.2.0:refs/tags/anncopy"], &[]));
}

/// Two specs that both match, two specs that collide, and the pattern that
/// matches every ref twice.
///
/// The interesting fact is that git tolerates *redundancy* and refuses
/// *ambiguity*, and it draws the line at the destination:
///
/// * The same wildcard twice is fine — the second mapping is identical to the
///   first, so both refs land once each.
/// * Two different sources onto one destination is `fatal: Cannot fetch both
///   refs/heads/main and refs/heads/div to refs/heads/x`, exit 128, before
///   anything is written. `push` refuses the same collision with a different
///   message from a different function (`error: dst ref refs/heads/x receives
///   from more than one src`, exit 1, and it is an `error` rather than a `die`).
///   Two verbs, two messages, one rule — a port that shares one refusal between
///   them diverges on whichever it did not copy.
/// * A wildcard whose destination namespace already holds a ref updates it
///   rather than colliding, which is what makes the `dup` case below different
///   from the collision above despite looking similar.
fn multiple_matches(out: &mut Vec<Case>) {
    f(&["fetch", "origin", "refs/heads/*:refs/remotes/twice/*", "refs/heads/*:refs/remotes/twice/*"], out);
    f(&["fetch", "origin", "refs/heads/main:refs/heads/x", "refs/heads/div:refs/heads/x"], out);
    f(&["fetch", "origin", "main:refs/heads/x", "refs/heads/main:refs/heads/x"], out);
    // A wildcard and an explicit spec that both claim one destination: the
    // explicit one wins and the wildcard's own mapping is dropped for that ref.
    f(&["fetch", "origin", "refs/heads/*:refs/remotes/both/*", "refs/heads/div:refs/remotes/both/main"], out);
    p(&["push", "origin", "refs/heads/main:refs/heads/x", "refs/heads/div:refs/heads/x"], out);
    // Pushing into the peer's *remote-tracking* namespace: nothing in the
    // matcher stops a destination from being `refs/remotes/…`, and the peer
    // accepts it as `[new reference]` rather than `[new branch]`.
    p(&["push", "origin", "refs/heads/main:refs/remotes/origin/pushed"], out);
    // A wildcard push, with and without the leading `+`. Both branches are
    // behind/diverged, so the unforced one is where the per-ref force flag
    // becomes visible: `+` on the spec is per-refspec, not per-invocation.
    p(&["push", "origin", "+refs/heads/*:refs/heads/copy/*"], out);
    p(&["push", "origin", "refs/heads/*:refs/heads/plain/*"], out);
}

/// `^<src>`, the negative refspec (git 2.29+), and the asymmetry that it is a
/// **fetch-only** construct.
///
/// A negative does not fetch anything and does not stand alone as an
/// instruction: it removes refs from whatever the positives matched. Four facts,
/// each measured with stock and each a place a port can be wrong while agreeing
/// everywhere else:
///
/// 1. `^main` with no positive is a silent exit 0 — nothing to subtract from.
/// 2. `^main` beside `refs/heads/*:…` subtracts nothing, because the negative's
///    source is matched against the *advertisement* by the same partial-name
///    rules as a positive's, and `main` there is `refs/heads/main` — which the
///    positive maps and the report shows arriving. Verified: both `neg/div` and
///    `neg/main` are created.
/// 3. `^refs/heads/*` beside the same positive subtracts *everything*, and the
///    result is silence and exit 0 rather than an error.
/// 4. A negative may not carry a destination: `fatal: invalid refspec
///    '^refs/heads/div:refs/heads/x'`.
///
/// And the asymmetry: **`push` ignores `^`**. Stock pushes both branches for
/// `push origin ^refs/heads/div +refs/heads/*:refs/heads/n/*`. A port that
/// implemented one refspec parser for both verbs and honoured the negative in
/// both is wrong in the direction that looks more correct.
fn negative_refspecs(out: &mut Vec<Case>) {
    f(&["fetch", "origin", "^main"], out);
    f(&["fetch", "origin", "^main", "refs/heads/*:refs/remotes/neg/*"], out);
    f(&["fetch", "origin", "^refs/heads/main", "refs/heads/*:refs/remotes/negfull/*"], out);
    f(&["fetch", "origin", "^refs/heads/*", "refs/heads/*:refs/remotes/negall/*"], out);
    f(&["fetch", "origin", "^refs/heads/nope", "refs/heads/*:refs/remotes/negmiss/*"], out);
    // A negative against an *explicit* positive, not a pattern: the subtraction
    // happens after matching, so the explicit spec is cancelled too.
    f(&["fetch", "origin", "^refs/heads/div", "refs/heads/div:refs/heads/copy"], out);
    f(&["fetch", "origin", "^refs/heads/div:refs/heads/x", "refs/heads/*:refs/remotes/n2/*"], out);
    // Negatives on a shape with tags, where the subtraction is visible as one of
    // two tags surviving.
    out.push(branched(
        "fetch",
        &["fetch", "origin", "^refs/tags/v0.1.0", "refs/tags/*:refs/tags/negt/*"],
        &[],
    ));
    // Push, which does not implement `^` at all.
    p(&["push", "origin", "^refs/heads/div", "+refs/heads/*:refs/heads/n/*"], out);
}

/// The source lookup, on the one shape where the same short name is three refs.
///
/// The refspec matcher does not call `rev-parse`. For `fetch` it walks the
/// advertisement in `refs/<name>`, `refs/heads/<name>`, `refs/tags/<name>`,
/// … order (`remote.c:count_refspec_match`, which prefers an exact hit and then
/// falls back through the same list `refs_ref_exists` uses); for `push` it
/// refuses outright when more than one local ref matches. Measured with stock
/// 2.55.0 on this shape, the answers are three different objects and two
/// different verbs' worth of behaviour:
///
/// ```text
/// $ git fetch . ambi:refs/heads/pick       #  * [new tag]     ambi     -> pick   (the TAG)
/// $ git fetch . top:refs/heads/pick        #  * [new ref]     refs/top -> pick   (refs/top)
/// $ git fetch . rem/ambi:refs/heads/pick   #  * [new branch]  rem/ambi -> pick   (the BRANCH)
/// $ git push  . ambi:refs/heads/pushpick   # error: src refspec ambi matches more than one
/// ```
///
/// `ambi-ann` is the case that reaches past the matcher: the source resolves to
/// the *annotated tag object*, and writing a non-commit into `refs/heads/`
/// fails at the ref-update layer with `error: trying to write non-commit object
/// … to branch 'refs/heads/pick'` and exit 1, after the fetch itself succeeded.
///
/// The remote here is `.` — the fixture reading its own ref store through the
/// transport. That is the only way to get an *advertisement* that carries these
/// names, and it stays inside the fixture copy.
fn ambiguous_sources(out: &mut Vec<Case>) {
    let amb = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::strict("fetch", args, Shape::AmbiguousRef));
    };
    amb(&["fetch", ".", "ambi:refs/heads/pick"], out);
    amb(&["fetch", ".", "top:refs/heads/pick"], out);
    amb(&["fetch", ".", "rem/ambi:refs/heads/pick"], out);
    amb(&["fetch", ".", "ambi-ann:refs/heads/pick"], out);
    // The disambiguated spellings of the same three names, which must reach past
    // the precedence list to the ref actually named.
    amb(&["fetch", ".", "heads/ambi:refs/heads/pick"], out);
    amb(&["fetch", ".", "tags/ambi:refs/heads/pick"], out);
    amb(&["fetch", ".", "refs/top:refs/heads/pick"], out);
    // A wildcard over an ambiguous namespace: every name matches once per
    // namespace it lives in, so the destination namespace ends up holding both.
    amb(&["fetch", ".", "refs/*:refs/mirrored/*"], out);
    // `push`'s answer to the same question is a refusal naming the spec.
    out.push(Case::strict("push", &["push", ".", "ambi:refs/heads/pushpick"], Shape::AmbiguousRef));
    out.push(Case::strict("push", &["push", ".", "top:refs/heads/pushpick"], Shape::AmbiguousRef));
    // …and the unambiguous spelling that goes through.
    out.push(Case::strict(
        "push",
        &["push", ".", "refs/heads/ambi:refs/heads/pushpick"],
        Shape::AmbiguousRef,
    ));
    // `ls-remote` answers with *every* match rather than choosing one, which is
    // the read-only way to see the whole candidate set the two verbs above pick
    // from.
    out.push(Case::new("ls-remote", &["ls-remote", ".", "ambi"], Shape::AmbiguousRef));
    out.push(Case::new("ls-remote", &["ls-remote", ".", "top"], Shape::AmbiguousRef));
    out.push(Case::new("ls-remote", &["ls-remote", ".", "rem/ambi"], Shape::AmbiguousRef));
}

/// `remote.<name>.fetch` as a *list*, `--refmap` as its command-line override,
/// and how both interact with `--prune`.
///
/// `fetch_clone.rs` sets `remote.origin.fetch` once, to one non-default value.
/// The matcher's behaviour with a *multi-valued* key is a different thing
/// entirely: the entries are matched in order and their results unioned, a
/// second entry may name the same source as the first, a `^` entry may appear
/// among them, and an entry that is not a valid pattern kills the whole fetch
/// before the transport opens.
///
/// `--refmap` is the same list supplied on the command line, and it is only
/// legal *beside* a command-line refspec — `fatal: --refmap option is only
/// meaningful with command-line refspec(s)` otherwise. That refusal is the one
/// place the distinction between "the refspec you asked for" and "the map that
/// files it locally" is stated out loud.
///
/// `--prune`'s reach is decided by the same list: it may delete only within the
/// destination namespaces the *active* refspecs cover. So a configured
/// `refs/heads/main:refs/remotes/origin/keep` prunes nothing under
/// `refs/remotes/origin/` except `keep`, while the default wildcard prunes the
/// lot — which is the same rule `fetch_clone.rs` exercises from the command line
/// and this exercises from configuration.
fn configured_fetch_specs(out: &mut Vec<Case>) {
    let cfg = |args: &[&str], config: &[(&str, &str)], out: &mut Vec<Case>| {
        out.push(Case::strict("fetch", args, Shape::BehindRemote).with_config(config));
    };

    // Two entries, disjoint destinations. Both must run.
    cfg(
        &["fetch", "origin"],
        &[
            ("remote.origin.fetch", "+refs/heads/main:refs/remotes/origin/main"),
            ("remote.origin.fetch", "+refs/heads/div:refs/remotes/second/div"),
        ],
        out,
    );
    // A wildcard entry plus an explicit one naming a ref the wildcard already
    // covers: the ref lands in both destinations.
    cfg(
        &["fetch", "origin"],
        &[
            ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
            ("remote.origin.fetch", "+refs/heads/main:refs/remotes/dup/main"),
        ],
        out,
    );
    // A negative among the configured entries.
    cfg(
        &["fetch", "origin"],
        &[
            ("remote.origin.fetch", "^refs/heads/div"),
            ("remote.origin.fetch", "+refs/heads/*:refs/remotes/cfgneg/*"),
        ],
        out,
    );
    // An entry that is not a valid pattern: half a glob, delivered from config
    // rather than argv. The refusal must still name the spec.
    cfg(
        &["fetch", "origin"],
        &[("remote.origin.fetch", "+refs/heads/main:refs/remotes/origin/*")],
        out,
    );
    // Prune, bounded by a configured non-wildcard destination.
    cfg(
        &["fetch", "--prune", "origin"],
        &[("remote.origin.fetch", "+refs/heads/main:refs/remotes/origin/keep")],
        out,
    );
    // Prune with a command-line refspec that narrows the covered namespace to
    // one ref: `origin/div` is outside it and survives.
    cfg(
        &["fetch", "--prune", "origin", "refs/heads/main:refs/remotes/origin/main"],
        &[("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*")],
        out,
    );
    // `tagOpt=--tags` is the configured form of `--tags`; on a peer with no tags
    // it has to reach the tag walk and come back with nothing.
    cfg(&["fetch", "origin", "main:refs/heads/tagopt"], &[("remote.origin.tagOpt", "--tags")], out);
    // `--prune-tags` without `--prune`, and with it, against a configured
    // wildcard. Neither has a stale tag to delete here; what is pinned is that
    // both stay quiet rather than deleting a branch namespace.
    f(&["fetch", "--prune-tags", "origin"], out);
    f(&["fetch", "--prune-tags", "--prune", "origin", "refs/heads/*:refs/remotes/pt/*"], out);

    // `--refmap`: empty, wildcard, explicit, unparseable, and illegal.
    f(&["fetch", "--refmap=", "origin", "refs/heads/main"], out);
    f(&["fetch", "--refmap=refs/heads/*:refs/remotes/rm/*", "origin", "refs/heads/main"], out);
    f(&["fetch", "--refmap=refs/heads/main:refs/remotes/rm/one", "origin", "main"], out);
    f(&["fetch", "--refmap=bogus", "origin", "main"], out);
    f(&["fetch", "--refmap=refs/heads/*:refs/remotes/rm/*", "origin"], out);
    // `--refmap` beside `--prune`: the map, not the configured spec, decides
    // what prune is allowed to touch.
    f(&["fetch", "--refmap=refs/heads/*:refs/remotes/rm/*", "--prune", "origin", "refs/heads/main"], out);
    // Two `--refmap` entries at once: the list is matched in order and both
    // entries fire, so one invocation writes two tracking refs and reports four
    // lines — two to FETCH_HEAD and two to the maps.
    f(
        &[
            "fetch",
            "--refmap=refs/heads/main:refs/remotes/rm/two",
            "--refmap=refs/heads/div:refs/remotes/rm/three",
            "origin",
            "main",
            "div",
        ],
        out,
    );
    // Tags fetched through a refmap on the shape that has them.
    out.push(branched(
        "fetch",
        &["fetch", "--refmap=refs/tags/*:refs/remotes/tagmap/*", "origin", "refs/tags/v0.1.0"],
        &[],
    ));
}

/// `remote.<name>.push` as a list, and `remote.<name>.mirror`.
///
/// `branch_remote.rs` sets `remote.origin.push` to a single explicit spec. What
/// is untested is the same key holding a *pattern*, holding *two* entries, and
/// the mirror flag — which is not a refspec at all but a mode that synthesizes
/// `+refs/*:refs/*` and turns deletion on. Verified with stock, mirror on this
/// fixture force-updates both branches *and* copies the local
/// `refs/remotes/origin/*` onto the peer:
///
/// ```text
/// $ git -c remote.origin.mirror=true push origin
///  + 79f764e...b83e0a6 div -> div (forced update)
///  + 91bfcd8...54f11d5 main -> main (forced update)
///  * [new reference]   origin/div -> origin/div
///  * [new reference]   origin/main -> origin/main
/// ```
///
/// That is four ref updates on the peer, all of them inside `probe_peer`'s
/// `for-each-ref`, so the state half of this case is real rather than a report
/// comparison.
///
/// The two flag interactions are here because both were reported as dying
/// `no refspec to push`, and both are matcher questions rather than flag
/// questions: with `remote.<n>.push` set, `--tags` must still walk the tag set
/// (`Everything up-to-date` on a fixture with none) and `--prune` must still use
/// the configured spec as its coverage set.
fn configured_push_specs(out: &mut Vec<Case>) {
    let cfg = |args: &[&str], config: &[(&str, &str)], out: &mut Vec<Case>| {
        out.push(Case::strict("push", args, Shape::BehindRemote).with_config(config));
    };
    cfg(&["push", "origin"], &[("remote.origin.push", "refs/heads/*:refs/heads/mirror/*")], out);
    cfg(
        &["push", "origin"],
        &[
            ("remote.origin.push", "+refs/heads/div:refs/heads/one"),
            ("remote.origin.push", "+refs/heads/main:refs/heads/two"),
        ],
        out,
    );
    // A configured spec whose source matches nothing: `error: src refspec … does
    // not match any`, exit 1, and no transport.
    cfg(&["push", "origin"], &[("remote.origin.push", "refs/heads/nope:refs/heads/x")], out);
    // The two flags reported as dying on a configured spec.
    cfg(&["push", "--tags", "origin"], &[("remote.origin.push", "+refs/heads/*:refs/heads/t/*")], out);
    cfg(&["push", "--prune", "origin"], &[("remote.origin.push", "+refs/heads/*:refs/heads/t/*")], out);
    // Mirror: the mode, and the mode under --dry-run (no pre-push hook on this
    // shape, so the port's inverted guard cannot show up here).
    cfg(&["push", "origin"], &[("remote.origin.mirror", "true")], out);
    cfg(&["push", "--dry-run", "origin"], &[("remote.origin.mirror", "true")], out);
    // A configured spec under --dry-run: the report must name the mapping the
    // spec produces while the peer stays untouched.
    cfg(&["push", "--dry-run", "origin"], &[("remote.origin.push", "refs/heads/div:refs/heads/pushed")], out);
    // `--mirror` and `--all` are refspec *modes*, and a refspec beside either is
    // rejected during option parsing, before the matcher runs.
    p(&["push", "--mirror", "origin", "refs/heads/main:refs/heads/x"], out);
    // `--delete <pattern>` is `:<pattern>` after rewriting, and a destination-only
    // glob is not a legal spec — the refusal names the *rewritten* form,
    // `':refs/heads/*'`, which is the one place the rewrite is visible.
    p(&["push", "--delete", "origin", "refs/heads/*"], out);
    // Deletion under --dry-run, which is the only way to see a delete mapping
    // without losing the ref it names.
    p(&["push", "--dry-run", "origin", ":refs/heads/div"], out);
}

/// `push.default` on a branch with **no upstream**, which `branch_remote.rs`
/// cannot reach.
///
/// Both branches of `Shape::BehindRemote` carry `branch.<n>.merge`, so the six
/// `push.default` values there are all measured on the has-an-upstream side of
/// the cross. `Shape::Branched` has no remote at all; giving it one by
/// configuration leaves `main` with a remote to push to and no upstream, and the
/// six values then split three ways rather than the two they split into over
/// there:
///
/// * `simple` and `upstream` **die** — `fatal: The current branch main has no
///   upstream branch.` plus the five-line hint naming `--set-upstream` and
///   `push.autoSetupRemote`, exit 128.
/// * `current` and `matching` succeed with `Everything up-to-date` (the remote
///   is the same repository), and they differ in the *state* they leave:
///   `current` writes one opportunistic tracking ref, `matching` writes one per
///   local branch. That difference is invisible on stdout and is exactly what
///   `probe_state`'s `for-each-ref` is for.
/// * `nothing` refuses with its own message and never contacts the remote.
///
/// `@{push}` is the read-only half of the same question. It is the *only* way to
/// ask what the push matcher would answer without pushing, and each of its five
/// refusals comes from a different point in the algorithm: no upstream at all
/// (`fatal: no upstream configured for branch 'main'`), a push refspec list that
/// does not cover this branch (`fatal: push refspecs for 'origin' do not include
/// 'main'`), a destination with no local tracking ref (`fatal: push destination
/// 'refs/remotes/pushed/main' on remote 'origin' has no local tracking branch`),
/// and twice the generic `fatal: ambiguous argument '@{push}'` where the
/// resolution stops before it has a destination to name at all. Five inputs,
/// four distinct messages — a port that answers one message to all five agrees
/// on two cases and diverges on three.
fn push_defaults_without_upstream(out: &mut Vec<Case>) {
    for value in ["simple", "current", "upstream", "matching", "nothing"] {
        out.push(branched("push", &["push"], &[("push.default", value)]));
    }
    // `push.default=current` with the destination named explicitly is the
    // control: same value, a refspec on the command line, so the default never
    // fires and the tracking write must not happen.
    out.push(branched(
        "push",
        &["push", "origin", "refs/heads/feature:refs/heads/copied"],
        &[("push.default", "current")],
    ));

    // `@{push}`, four refusals and the paths that produce them.
    out.push(branched("rev-parse", &["rev-parse", "--symbolic-full-name", "@{push}"], &[]));
    out.push(branched(
        "rev-parse",
        &["rev-parse", "--symbolic-full-name", "@{push}"],
        &[("push.default", "current")],
    ));
    out.push(branched(
        "rev-parse",
        &["rev-parse", "--symbolic-full-name", "@{push}"],
        &[("remote.origin.push", "refs/heads/*:refs/remotes/pushed/*")],
    ));
    out.push(branched(
        "rev-parse",
        &["rev-parse", "--symbolic-full-name", "@{push}"],
        &[("remote.origin.push", "refs/heads/feature:refs/heads/other")],
    ));
    // The identity push spec `refs/heads/*:refs/heads/*`, which maps `main` onto
    // the remote's own `main`. Still a refusal, and deliberately so — measured
    // with stock, `@{push}` resolves through the *tracking* ref for that
    // destination, and no fetch has run on this shape, so
    // `refs/remotes/origin/main` does not exist and the answer is the
    // `ambiguous argument '@{push}'` form rather than the three named refusals
    // above. Four different messages for four different missing pieces is the
    // whole point of this group.
    out.push(branched(
        "rev-parse",
        &["rev-parse", "--symbolic-full-name", "@{push}"],
        &[("remote.origin.push", "refs/heads/*:refs/heads/*")],
    ));
}

/// The three ways to ask the matcher a question without moving anything.
///
/// `ls-remote <pattern>` runs the *tail-match* rule — a pattern with no slash
/// matches any ref whose last components equal it, so `main` finds
/// `refs/heads/main` and `*/main` finds it too, while `refs/heads/` (a bare
/// prefix, no glob) finds nothing. It is the cheapest probe in the corpus: no
/// ref moves, so a divergence is unambiguously the matcher.
///
/// `fetch --dry-run` reports the mapping it would make and writes nothing, so a
/// port whose matcher is right and whose ref writer is wrong is separable from
/// one whose matcher is wrong. Pairing it with `--prune` is the sharpest of
/// these: the deletions are computed, printed and not performed.
///
/// `remote show -n origin` renders the configured specs back as prose — `Local
/// ref configured for 'git push'` naming source and destination, or `Local refs
/// will be mirrored by 'git push'` for the mirror mode. `-n` is what keeps it
/// local: without it the command contacts the remote for its ref list.
///
/// `ls-remote` is not strict: it writes its answer to stdout, and stdout is
/// compared for every case.
fn matcher_probes(out: &mut Vec<Case>) {
    let ls = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("ls-remote", args, Shape::BehindRemote));
    };
    ls(&["ls-remote", "origin", "refs/heads/*"], out);
    ls(&["ls-remote", "origin", "heads/*"], out);
    ls(&["ls-remote", "origin", "*/main"], out);
    ls(&["ls-remote", "origin", "ma*"], out);
    ls(&["ls-remote", "origin", "main", "div"], out);
    ls(&["ls-remote", "origin", "refs/heads/main", "refs/heads/nope"], out);
    ls(&["ls-remote", "origin", "refs/heads/"], out);
    ls(&["ls-remote", "origin", "refs/*"], out);
    // `^main` is not a negative here — `ls-remote` has no negatives, so it is an
    // ordinary pattern that matches no ref, and the answer is silence and 0.
    ls(&["ls-remote", "origin", "^main"], out);
    ls(&["ls-remote", "--refs", "origin", "refs/heads/*"], out);
    ls(&["ls-remote", "--heads", "origin", "div"], out);
    ls(&["ls-remote", "--sort=refname", "origin", "refs/heads/*"], out);
    out.push(Case::strict("ls-remote", &["ls-remote", "--exit-code", "origin", "refs/heads/nope"], Shape::BehindRemote));
    // Tag patterns, on the shape that has tags. `refs/tags/*` also brings the
    // peeled `^{}` line for the annotated tag, which is part of the
    // advertisement rather than of the ref store.
    out.push(branched("ls-remote", &["ls-remote", "origin", "refs/tags/*"], &[]));
    out.push(branched("ls-remote", &["ls-remote", "origin", "*0.1.0*"], &[]));
    out.push(branched("ls-remote", &["ls-remote", "origin", "v0.2.0^{}"], &[]));

    // `fetch --dry-run` as a mapping probe.
    f(&["fetch", "--dry-run", "origin", "refs/heads/*:refs/remotes/dry/*"], out);
    f(&["fetch", "--dry-run", "origin", "refs/heads/main:refs/heads/dryx"], out);
    f(&["fetch", "--dry-run", "--refmap=refs/heads/*:refs/remotes/dryrm/*", "origin", "main"], out);
    f(&["fetch", "--dry-run", "origin", "^main", "refs/heads/*:refs/remotes/dryneg/*"], out);
    // Prune under --dry-run where prune has *nothing* to delete: every tracking
    // ref is covered and current, so the answer is silence rather than the two
    // deletions the `refs/tags/*` spelling produces.
    f(&["fetch", "--dry-run", "--prune", "origin", "refs/heads/*:refs/remotes/origin/*"], out);
    out.push(branched("fetch", &["fetch", "--dry-run", "origin", "refs/tags/*:refs/tags/dryt/*"], &[]));

    // `push --dry-run` as a mapping probe. No pre-push hook exists on this
    // shape, so `push.rs:564`'s inverted guard has nothing to skip and the case
    // measures the mapping alone.
    p(&["push", "--dry-run", "origin", "+refs/heads/*:refs/heads/dry/*"], out);
    p(&["push", "--dry-run", "origin", "refs/heads/div:refs/heads/dryone"], out);

    // The configured specs, read back as prose.
    let show = |config: &[(&str, &str)], out: &mut Vec<Case>| {
        out.push(Case::new("remote", &["remote", "show", "-n", "origin"], Shape::BehindRemote).with_config(config));
    };
    show(&[("remote.origin.push", "refs/heads/div:refs/heads/pushed")], out);
    show(&[("remote.origin.push", "refs/heads/*:refs/heads/*")], out);
    show(
        &[
            ("remote.origin.push", "+refs/heads/div:refs/heads/one"),
            ("remote.origin.push", "+refs/heads/main:refs/heads/two"),
        ],
        out,
    );
    show(&[("remote.origin.mirror", "true")], out);
    show(&[("remote.origin.fetch", "+refs/heads/main:refs/remotes/origin/main")], out);
}

/// The commands that *write* a refspec into `.git/config`.
///
/// These are matcher cases because the spec they synthesize is the input every
/// later fetch runs on, and because git synthesizes it by string concatenation
/// rather than by parsing. Measured with stock 2.55.0:
///
/// ```text
/// remote add -t main second        -> +refs/heads/main:refs/remotes/second/main
/// remote add -t 'refs/heads/*' s2  -> +refs/heads/refs/heads/*:refs/remotes/s2/refs/heads/*
/// remote add --mirror=fetch m1     -> +refs/*:refs/*
/// remote add --mirror=push m2      -> mirror = true          (no fetch spec at all)
/// remote add -m div second3        -> +refs/heads/*:refs/remotes/second3/*
/// remote set-branches origin main  -> +refs/heads/main:refs/remotes/origin/main
/// ```
///
/// The second line is the one worth having: `-t` prefixes `refs/heads/`
/// unconditionally, so a fully-qualified argument produces a doubled path that
/// matches nothing. A port that "helpfully" notices the argument is already
/// qualified writes a *working* refspec and diverges from stock by being right.
///
/// All six are measured by `config --list --local` in `probe_state`; none of
/// them contacts the remote, so nothing here needs the peer.
fn configured_spec_writers(out: &mut Vec<Case>) {
    let r = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("remote", args, Shape::BehindRemote));
    };
    r(&["remote", "add", "-t", "main", "second", "./.remote.git"], out);
    r(&["remote", "add", "-t", "refs/heads/*", "second", "./.remote.git"], out);
    r(&["remote", "add", "-t", "main", "-t", "div", "second", "./.remote.git"], out);
    r(&["remote", "add", "--mirror=fetch", "second", "./.remote.git"], out);
    r(&["remote", "add", "--mirror=push", "second", "./.remote.git"], out);
    r(&["remote", "add", "-m", "div", "second", "./.remote.git"], out);
    // Only the glob spelling: `branch_remote.rs:466-468` already owns
    // `set-branches origin main`, `origin main div` and `--add origin div`, and
    // re-filing them here would be the same question asked twice. This one it
    // does not have, and it is the one that matters — the same unconditional
    // `refs/heads/` prefixing as `-t`, so a fully-qualified argument produces
    // `+refs/heads/refs/heads/*:refs/remotes/origin/refs/heads/*`.
    r(&["remote", "set-branches", "origin", "refs/heads/*"], out);
}
