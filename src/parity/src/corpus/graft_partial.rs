//! The two repositories that are missing objects on purpose: a shallow clone
//! and a partial one.
//!
//! [`Shape::Shallow`] and [`Shape::Promisor`] are the only fixtures where an
//! object a command asks for is *not there*, and the two answer that question
//! differently: a shallow clone says the history stops (`.git/shallow` grafts
//! the boundary and the parent is simply absent), a partial clone says the
//! object can be had on demand (the promisor remote is consulted and the object
//! arrives). Both were built with a handful of cases each — eighteen verbs
//! apiece out of the hundred-plus the corpus knows — so most of the port's
//! behaviour at a graft boundary or a lazy fetch was never compared to
//! anything.
//!
//! The cases here are chosen by one rule: **the verb has to be able to tell.**
//! A command that never walks past the tip answers identically in a shallow
//! clone and a full one, and adding it here would measure the fixture rather
//! than the boundary. What is left is the set that walks (`blame`, `show`,
//! `log --graph`, `shortlog`, `fast-export`), the set that asks a reachability
//! question (`branch --contains`, `for-each-ref --contains`, `name-rev`,
//! `merge-base --is-ancestor`), and the set that must *refuse* because the
//! commit it was handed is on the other side of the graft — those are compared
//! on stderr, since the refusal is the whole answer.
//!
//! The partial-clone half is deliberately smaller. A case that reaches for a
//! filtered-out blob makes the side that fetches it write a pack, so the state
//! probe reports a difference that is about *when* an implementation fetches
//! rather than about what it answers; the corpus already carries one such case
//! and a dozen more would only restate it. The ones here either need no blob
//! (the reachability and naming verbs) or need one so plainly that both sides
//! must fetch it (`show`, `grep`).

use crate::fixture::Shape;
use crate::runner::Case;

pub fn cases(out: &mut Vec<Case>) {
    shallow_walks(out);
    shallow_reachability(out);
    shallow_past_the_graft(out);
    partial(out);
}

/// The verbs that walk history, run against a clone whose history stops after
/// two commits.
///
/// The fixture is `--depth=2` over five commits with a second branch forked
/// below the graft, so every one of these has a boundary to reach and a second
/// ref whose base is missing.
fn shallow_walks(out: &mut Vec<Case>) {
    for args in [
        // `blame` at a boundary attributes every surviving line to the grafted
        // commit rather than to whichever commit actually wrote it.
        &["blame", "deep.txt"][..],
        &["blame", "-L", "1,1", "deep.txt"],
        &["blame", "--line-porcelain", "deep.txt"],
        &["blame", "--incremental", "deep.txt"],
        // `show` renders the tip commit and its diff against a parent that is
        // present; `show` of the boundary commit has no parent to diff against.
        &["show", "HEAD"],
        &["show", "--stat", "HEAD"],
        &["show", "HEAD~1"],
        &["show", "--stat", "HEAD~1"],
        // The walk itself, in the shapes that print the graft.
        &["log", "--graph", "--oneline"],
        &["log", "--oneline", "--boundary", "HEAD"],
        &["log", "--format=%H %p", "HEAD"],
        &["shortlog", "-s", "-n", "HEAD"],
        &["shortlog", "--summary", "--numbered", "--all"],
        // `fast-export` has to decide what to do with a parent it does not
        // have: a shallow clone is exactly the input where its `--reference-
        // excluded-parents` question has an answer.
        &["fast-export", "--all"],
        &["fast-export", "HEAD"],
        // Object access at the tip is ordinary; naming the boundary is not.
        &["ls-tree", "-r", "HEAD"],
        &["cat-file", "-p", "HEAD^{tree}"],
    ] {
        out.push(Case::new(args[0], args, Shape::Shallow));
    }
}

/// Reachability questions, whose answers are a function of the history that is
/// *present* rather than of the history that exists upstream.
fn shallow_reachability(out: &mut Vec<Case>) {
    for args in [
        &["branch", "--contains", "HEAD"][..],
        &["branch", "-a", "--contains", "HEAD~1"],
        &["branch", "--no-contains", "HEAD"],
        &["for-each-ref", "--contains=HEAD", "--format=%(refname)"],
        &["for-each-ref", "--format=%(refname) %(objecttype)"],
        &["name-rev", "HEAD"],
        &["name-rev", "--all"],
        &["name-rev", "--annotate-stdin"],
        &["merge-base", "--all", "main", "origin/sh-side"],
        &["merge-base", "--octopus", "main", "origin/sh-side"],
        &["describe", "--always", "--all", "HEAD"],
    ] {
        out.push(Case::new(args[0], args, Shape::Shallow));
    }
    // `name-rev --annotate-stdin` reads the ids to name from stdin, so without
    // a payload it is only the empty-input path.
    out.push(Case::with_stdin(
        "name-rev",
        &["name-rev", "--annotate-stdin"],
        Shape::Shallow,
        b"HEAD\n",
    ));
}

/// The commits on the other side of the graft: every one of these must refuse,
/// and the refusal is the whole answer, so all are compared on stderr.
///
/// The fixture is two commits deep, so `HEAD~1` is the boundary and `HEAD~2` is
/// past it. A port that walks into the missing parent instead of stopping
/// answers with a traceback, an empty success, or the wrong commit — three
/// failures a stdout comparison alone cannot separate from each other.
fn shallow_past_the_graft(out: &mut Vec<Case>) {
    for args in [
        &["rev-parse", "HEAD~2"][..],
        &["rev-parse", "HEAD~5"],
        &["rev-parse", "--verify", "HEAD~4"],
        &["cat-file", "-p", "HEAD~2"],
        &["cat-file", "-t", "HEAD~3"],
        &["log", "HEAD~2"],
        &["diff", "HEAD~2", "HEAD"],
        &["show", "HEAD~2"],
        &["blame", "HEAD~2", "--", "deep.txt"],
    ] {
        out.push(Case::strict(args[0], args, Shape::Shallow));
    }
}

/// The partial clone: the verbs that answer without a blob, and the two that
/// plainly need one.
fn partial(out: &mut Vec<Case>) {
    for args in [
        // Reachability and naming need commits and trees, which a `blob:none`
        // filter kept — so these must answer exactly as in a full clone, with
        // no fetch at all.
        &["name-rev", "HEAD"][..],
        &["branch", "--contains", "HEAD~1"],
        &["for-each-ref", "--format=%(refname) %(objecttype)"],
        &["merge-base", "--is-ancestor", "HEAD~2", "HEAD"],
        &["describe", "--always", "HEAD"],
        &["shortlog", "-s", "-n", "HEAD"],
        &["log", "--format=%H %T", "HEAD"],
        &["ls-tree", "-r", "HEAD"],
        &["rev-list", "--count", "HEAD"],
        // The missing objects, named as such. `--missing=print` is the one
        // report that exists only for this repository kind.
        &["rev-list", "--objects", "--missing=print", "HEAD"],
        &["rev-list", "--objects", "--missing=allow-any", "HEAD"],
        &["rev-list", "--objects", "--filter=blob:none", "HEAD"],
        &["rev-list", "--objects", "--filter=blob:none", "--filter-print-omitted", "HEAD"],
        // What the object store holds, without asking for any of it.
        &["cat-file", "--batch-check", "--batch-all-objects"],
        &["cat-file", "--batch-check=%(objecttype) %(objectsize:disk)", "--batch-all-objects"],
    ] {
        out.push(Case::new(args[0], args, Shape::Promisor));
    }

    // The two that need a blob, so both sides must go and get it. They are here
    // because the *fetch* is the behaviour: a port that renders a missing blob
    // as empty rather than fetching it passes every case above and fails these.
    for args in [&["show", "HEAD"][..], &["grep", "-n", "hist", "HEAD"]] {
        out.push(Case::new(args[0], args, Shape::Promisor));
    }
}
