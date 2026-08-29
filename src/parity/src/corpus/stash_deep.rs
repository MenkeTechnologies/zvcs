//! Differential corpus cases for the stash **entry** — the object it is, the
//! stack it sits on, and the merge that takes it back out.
//!
//! Every case here is compared against stock git for stdout, exit code and
//! post-command repository state.
//!
//! # Why this module exists beside the ~163 `stash` cases already in the corpus
//!
//! The existing cases — `corpus.rs`'s flag matrix, `nested.rs`'s `STASH_PUSH`
//! product, the workflows in `sequences.rs`, and the shape-specific ones in
//! `worktree_index.rs`, `pathspec_stdin.rs` and `fixture_gaps2.rs` — sweep the
//! *verbs* and their flags. They ask "does `stash push -u -k -S -q` print what
//! git prints". None of them asks what a stash entry **is**, and that is where
//! every defect this subsystem has shipped actually lived: an entry written with
//! the wrong number of parents, an index parent whose tree does not match the
//! index it claims to record, a `show -u` that wrote an object while reading, a
//! `pop` that consumed the entry after refusing to apply it.
//!
//! A stash entry is a commit with **two or three parents**:
//!
//! ```text
//!   ^1  the HEAD the stash was taken from
//!   ^2  the index commit — the base of the three-way merge `apply` performs,
//!       whose tree is the index as it stood, and whose only parent is ^1
//!   ^3  the untracked commit — present only for `-u`/`-a`, a **parentless**
//!       commit whose tree holds nothing but the untracked files
//! ```
//!
//! and the entry's own tree is the worktree. Almost every hard question about
//! `stash` is a question about that shape: `--index` restores `^2`'s tree into
//! the index and must refuse when it cannot; `apply` three-way merges with `^2`
//! as the base, which is why it behaves differently from `checkout` when HEAD
//! has moved; `-u` is exactly "is there a third parent"; and `stash show -u`
//! reads `^3` and must write nothing at all.
//!
//! So the axes here are the graph, the reflog that stacks the graph, and the
//! merge that unstacks it — asked through plumbing that prints the structure
//! (`cat-file`, `rev-parse`, `rev-list --parents`, `ls-tree`) rather than
//! through `stash` itself, because `stash`'s own output can be right about a
//! commit that is wrong.
//!
//! # Determinism
//!
//! A stash commit's message embeds the branch name **and** an abbreviated HEAD
//! id with its subject (`index on main: 8fd6232 add counter and notes`), so it
//! is byte-stable only because [`crate::env`] pins identity and both dates and
//! because the fixture is built once by stock git and copied to both sides. The
//! ids themselves are therefore reproducible, which is what makes
//! `cat-file -p refs/stash` a legitimate comparison rather than a clock reading.
//!
//! `stash create` is the one verb here that mints a commit *at case run time*
//! rather than reading one the fixture built. It is still deterministic for the
//! same reason — every input to the commit object is pinned — and that is
//! precisely what makes it worth comparing: the printed id is a checksum over
//! the whole entry the implementation decided to build.
//!
//! # What is deliberately not here
//!
//! * **Multi-step workflows.** This module contributes [`Case`]s, and a case is
//!   one argv against a pristine copy. Push-then-pop, drop-then-renumber and
//!   conflict-resolve-then-drop are already in `sequences.rs` and are not
//!   restated as broken halves here.
//! * **A genuine content conflict from `apply`.** [`Shape::Stashed`]'s worktree
//!   is dirty on both paths its entries touch, so every single-invocation
//!   `apply`/`pop` against it stops at the earlier gate — unpack-trees' "would
//!   be overwritten" refusal — before any three-way merge runs. Reaching a real
//!   conflict needs a step that first cleans or commits the worktree, which is
//!   what `sequences.rs`'s `pop-conflict-resolve-drop` does. The
//!   `merge.conflictStyle` cases below therefore pin that the *refusal* is taken
//!   at the same point regardless of the style, which is the part a single case
//!   can prove.

use crate::fixture::Shape;
use crate::runner::Case;

pub fn cases(out: &mut Vec<Case>) {
    entry_object_graph(out);
    the_stack_is_a_reflog(out);
    apply_is_a_merge(out);
    create_and_store(out);
    push_flags_over_a_stack(out);
    other_shapes(out);
}

/// **The entry as an object graph.** Parent count, parent trees, and the
/// relationship between them, read with plumbing rather than with `stash`.
///
/// `Shape::Stashed` gives three entries with deliberately different insides —
/// `@{2}` unstaged-only (two parents), `@{1}` carrying an untracked file (three
/// parents), `@{0}` staged *and* unstaged (two parents, but an index parent
/// whose tree differs from `^1`'s). That spread is the whole point: a port that
/// always writes two parents passes on `@{0}` and `@{2}` and fails only on
/// `@{1}`, and a port that always writes three fails only on the other two.
fn entry_object_graph(out: &mut Vec<Case>) {
    // The entries themselves, whole. Header order, parent count and message are
    // all in one comparison, and `probe_state`'s object listing proves that
    // reading them wrote nothing.
    for rev in ["refs/stash", "stash@{1}", "stash@{2}"] {
        out.push(Case::new("cat-file", &["cat-file", "-p", rev], Shape::Stashed));
    }
    // The index parent of each entry. Its message (`index on <branch>: <abbrev>
    // <subject>`) and its single parent are as much of the contract as its tree.
    for rev in ["refs/stash^2", "stash@{1}^2", "stash@{2}^2"] {
        out.push(Case::new("cat-file", &["cat-file", "-p", rev], Shape::Stashed));
    }
    // The untracked parent — and the fact that it has **no** parent line at all,
    // which is the one structural difference between it and the index commit.
    out.push(Case::new("cat-file", &["cat-file", "-p", "stash@{1}^3"], Shape::Stashed));
    out.push(Case::new("cat-file", &["cat-file", "-t", "refs/stash"], Shape::Stashed));
    out.push(Case::new("cat-file", &["cat-file", "-s", "refs/stash"], Shape::Stashed));

    // The three trees, side by side. `^{tree}` of the entry is the worktree,
    // `^2^{tree}` is the index, `^3^{tree}` is untracked-only — and a port that
    // conflates any two of them prints the same listing twice here.
    for rev in ["refs/stash^{tree}", "refs/stash^2^{tree}", "stash@{1}^3^{tree}"] {
        out.push(Case::new("cat-file", &["cat-file", "-p", rev], Shape::Stashed));
    }

    // Parent resolution by index, including the one that must not resolve: the
    // top entry has no untracked parent, so `^3` is an unknown revision.
    out.push(Case::new("rev-parse", &["rev-parse", "refs/stash^1", "refs/stash^2"], Shape::Stashed));
    out.push(Case::new(
        "rev-parse",
        &["rev-parse", "stash@{1}^1", "stash@{1}^2", "stash@{1}^3"],
        Shape::Stashed,
    ));
    out.push(Case::new(
        "rev-parse",
        &["rev-parse", "refs/stash^{tree}", "refs/stash^2^{tree}"],
        Shape::Stashed,
    ));
    out.push(Case::new("rev-parse", &["rev-parse", "--short", "refs/stash^2"], Shape::Stashed));
    // The absence of a third parent is a refusal, and the message names the rev.
    out.push(Case::strict("rev-parse", &["rev-parse", "refs/stash^3"], Shape::Stashed));

    // Parent *count* on one line, which is the single fact `-u` decides.
    for rev in ["refs/stash", "stash@{1}", "stash@{2}"] {
        out.push(Case::new("rev-list", &["rev-list", "--parents", "-1", rev], Shape::Stashed));
    }
    // The untracked commit is parentless, so its history is exactly one commit
    // while the entry's reaches back through `^1`.
    out.push(Case::new("rev-list", &["rev-list", "--count", "refs/stash"], Shape::Stashed));
    out.push(Case::new("rev-list", &["rev-list", "--count", "stash@{1}^3"], Shape::Stashed));

    // Tree id, parent ids and subject together: one line that is wrong if any
    // part of the entry is.
    out.push(Case::new("log", &["log", "-1", "--format=%T %P %s", "refs/stash"], Shape::Stashed));
    out.push(Case::new("log", &["log", "-1", "--format=%T %P %s", "stash@{1}"], Shape::Stashed));
    out.push(Case::new("log", &["log", "--format=%h %p %s", "-4", "refs/stash"], Shape::Stashed));

    // The trees, listed. `stash@{1}^3` must contain the untracked file and
    // *nothing else* — a port that builds the untracked commit from the whole
    // worktree rather than from the untracked set fails here and nowhere else.
    out.push(Case::new("ls-tree", &["ls-tree", "refs/stash^2"], Shape::Stashed));
    out.push(Case::new("ls-tree", &["ls-tree", "-r", "--name-only", "stash@{1}^3"], Shape::Stashed));
    out.push(Case::new("ls-tree", &["ls-tree", "-r", "-t", "refs/stash"], Shape::Stashed));
    out.push(Case::new("ls-tree", &["ls-tree", "-r", "--name-only", "refs/stash^2"], Shape::Stashed));
    // One id and nothing after it: the untracked commit is a root commit, and a
    // port that parents it on HEAD prints two ids here.
    out.push(Case::new("rev-list", &["rev-list", "--parents", "-1", "stash@{1}^3"], Shape::Stashed));

    out.push(Case::new("show", &["show", "--stat", "stash@{1}^3"], Shape::Stashed));
    out.push(Case::new("show", &["show", "--stat", "refs/stash^2"], Shape::Stashed));

    // The two diffs that define what an entry means: index-vs-worktree is the
    // unstaged half, HEAD-vs-index is the staged half.
    out.push(Case::new("diff", &["diff", "--stat", "refs/stash^2", "refs/stash"], Shape::Stashed));
    // …and the two halves added together, which is what `stash show` reports.
    out.push(Case::new("diff", &["diff", "--stat", "refs/stash^1", "refs/stash"], Shape::Stashed));
    out.push(Case::new(
        "diff",
        &["diff", "--name-status", "refs/stash^1", "refs/stash^2"],
        Shape::Stashed,
    ));
    // And against the untracked commit, whose tree shares no path with HEAD's,
    // so every tracked file reads as a deletion.
    out.push(Case::new(
        "diff",
        &["diff", "--name-status", "stash@{1}^1", "stash@{1}^3"],
        Shape::Stashed,
    ));

    // `^2` descends from `^1`, which is what makes it a usable merge base.
    out.push(Case::new("merge-base", &["merge-base", "refs/stash^1", "refs/stash^2"], Shape::Stashed));
    out.push(Case::new(
        "merge-base",
        &["merge-base", "--is-ancestor", "refs/stash^2", "refs/stash"],
        Shape::Stashed,
    ));

    out.push(Case::new(
        "for-each-ref",
        &["for-each-ref", "--format=%(refname) %(objecttype) %(subject)", "refs/stash"],
        Shape::Stashed,
    ));
    out.push(Case::new("show-ref", &["show-ref", "--verify", "refs/stash"], Shape::Stashed));

    // Batch mode over the whole family in one invocation: four names in, four
    // `<oid> commit <size>` lines out. A port that resolves `stash@{1}^3`
    // through a different path than `refs/stash^2` shows it here.
    out.push(Case::with_stdin(
        "cat-file",
        &["cat-file", "--batch-check"],
        Shape::Stashed,
        b"refs/stash\nrefs/stash^1\nrefs/stash^2\nstash@{1}^3\n",
    ));
    out.push(Case::with_stdin(
        "cat-file",
        &["cat-file", "--batch-check=%(objecttype) %(objectname) %(objectsize)"],
        Shape::Stashed,
        b"refs/stash^{tree}\nrefs/stash^2^{tree}\n",
    ));
    out.push(Case::with_stdin(
        "cat-file",
        &["cat-file", "--batch"],
        Shape::Stashed,
        b"refs/stash\n",
    ));
}

/// **The stack is the reflog of one ref.** `refs/stash` names only the newest
/// entry; `stash@{1}` and `stash@{2}` exist solely because `.git/logs/refs/stash`
/// records what the ref used to point at.
///
/// That is why `drop` renumbers, why `clear` has to delete the log and not just
/// the ref, and why an out-of-range index is a *reflog* error rather than an
/// unknown revision. The formats below print the reflog selector beside the
/// commit it names, so a port that keeps its own side list instead of a reflog
/// diverges on the first case that asks either half about the other.
fn the_stack_is_a_reflog(out: &mut Vec<Case>) {
    // `%P` in a `stash list` format is the parent list of every entry at once:
    // three lines, two of which have two ids and one of which has three. The
    // single densest statement of the whole contract in this module.
    out.push(Case::new("stash", &["stash", "list", "--format=%gd %H %P"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "--format=%gD|%gs"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "--format=%h %gd %T"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "--oneline"], Shape::Stashed));
    // NUL-separated: the entries run together with no newline, so a port that
    // emits its own separator shows it here and nowhere in the default format.
    out.push(Case::new("stash", &["stash", "list", "-z", "--format=%gd"], Shape::Stashed));
    // The selector is normally `stash@{<n>}`; with a date format it becomes
    // `stash@{<date>}`, which is a different code path in `log`'s reflog walker.
    out.push(Case::new("stash", &["stash", "list", "--date=raw"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "--date=unix"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "-n", "2"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "--skip=1"], Shape::Stashed));
    // A reflog walk cannot be reversed, and `stash list` inherits that refusal
    // from `log` rather than implementing one of its own.
    out.push(Case::strict("stash", &["stash", "list", "--reverse"], Shape::Stashed));

    // The same walk asked of `log` and `reflog` directly. Identical content
    // through three front doors: any one of them disagreeing is the finding.
    out.push(Case::new("log", &["log", "-g", "--format=%gd %H %P", "refs/stash"], Shape::Stashed));
    out.push(Case::new("log", &["log", "-g", "--format=%gd %ct %H", "refs/stash"], Shape::Stashed));
    out.push(Case::new("log", &["log", "-g", "--oneline", "refs/stash"], Shape::Stashed));
    // `--stat` on a reflog walk of merges prints the `Merge:` line — the parent
    // count again, this time in porcelain.
    out.push(Case::new("log", &["log", "-g", "--stat", "refs/stash"], Shape::Stashed));
    out.push(Case::new("reflog", &["reflog", "show", "stash", "--format=%gd %h %gs"], Shape::Stashed));
    out.push(Case::new("reflog", &["reflog", "show", "refs/stash"], Shape::Stashed));

    out.push(Case::new("rev-parse", &["rev-parse", "stash@{0}", "stash@{1}", "stash@{2}"], Shape::Stashed));
    // Past the end of the stack. The message states the reflog's length, so it
    // is a claim about the log rather than about the ref.
    out.push(Case::strict("rev-parse", &["rev-parse", "--verify", "refs/stash@{3}"], Shape::Stashed));

    // Dropping the *bottom* entry: `@{0}` must not move, `@{1}` must become what
    // `@{2}` was, and the dropped commit must survive as unreachable. Existing
    // cases drop `@{0}` and `@{1}`; neither can tell a renumber from a rewrite.
    out.push(Case::new("stash", &["stash", "drop", "stash@{2}"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "drop", "refs/stash@{1}"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "drop", "stash@{3}"], Shape::Stashed));

    // The whole stack at once. The ref and its log both have to go, and all three
    // entries plus their index and untracked parents have to stay in the object
    // store as unreachable objects — which `probe_state`'s `--batch-all-objects`
    // listing is what proves. A port that prunes here is losing stash content.
    out.push(Case::new("stash", &["stash", "clear"], Shape::Stashed));
}

/// **`apply` is a three-way merge whose base is the index commit**, and the
/// refusals around it are the data-safety contract.
///
/// The classic data-loss shape is `pop` refusing to apply and dropping the entry
/// anyway; git prints "The stash entry is kept in case you need it again" and
/// leaves `refs/stash` exactly where it was. Every `pop` case here is
/// [`Case::strict`] for that reason — the message *is* the behaviour — and the
/// post-command state probe is what proves the entry actually survived rather
/// than the sentence merely being printed.
///
/// `--index` adds a second promise: restore the index to `^2`'s tree, or restore
/// nothing at all and say so ("Index was not unstashed."). A port that applies
/// the worktree half and then gives up has already destroyed staged state.
fn apply_is_a_merge(out: &mut Vec<Case>) {
    // `@{2}` rewrites `counter.txt`, which the worktree is holding dirty, so the
    // gate fires. No existing case reaches `@{2}` with any of these four verbs.
    out.push(Case::strict("stash", &["stash", "apply", "stash@{2}"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "pop", "stash@{2}"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "apply", "--index", "stash@{2}"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "pop", "--index", "stash@{2}"], Shape::Stashed));
    // Quiet suppresses the trailing `status`, not the refusal or the "kept"
    // sentence — a port that gates the whole tail on `-q` loses the one line
    // that tells a user their work is still there.
    out.push(Case::strict("stash", &["stash", "pop", "--quiet", "stash@{2}"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "apply", "--index", "--quiet", "stash@{2}"], Shape::Stashed));

    // The refusal is taken by unpack-trees before any merge driver runs, so the
    // conflict style must make no difference to it. A port that reaches its
    // merge machinery first would produce style-dependent output here.
    out.push(
        Case::new("stash", &["stash", "apply", "stash@{2}"], Shape::Stashed)
            .with_config(&[("merge.conflictStyle", "diff3")]),
    );
    out.push(
        Case::new("stash", &["stash", "pop", "stash@{2}"], Shape::Stashed)
            .with_config(&[("merge.conflictStyle", "zdiff3")]),
    );

    // "Stash-like" is a structural test — two or three parents, second parent a
    // commit whose tree is the index — and each of these fails it for its own
    // reason: HEAD has one parent, the index commit has one. (`stash@{0}^2`
    // resolves to the same commit as `refs/stash^2` and is deliberately not
    // spelled twice.)
    out.push(Case::strict("stash", &["stash", "apply", "HEAD"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "apply", "refs/stash^2"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "show", "refs/stash^2"], Shape::Stashed));

    // `stash branch` is `apply` with a checkout in front: it creates the branch
    // at the entry's *base* commit and then applies with `--index`. Both halves
    // are observable, and they fail differently — over `@{2}` the checkout
    // succeeds and unpack-trees refuses; over `@{0}` the checkout succeeds and
    // the index restore fails with "conflicts in index. Try without --index."
    // In both, the branch exists afterwards and the entry survives.
    out.push(Case::new("stash", &["stash", "branch", "off2", "stash@{2}"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "branch", "off2", "refs/stash"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "branch", "main"], Shape::Stashed));

    // `show -u` on an entry that has **no** untracked parent. Read-only by
    // contract; the object listing in the state digest is what enforces it.
    out.push(Case::new("stash", &["stash", "show", "-u"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "-u", "stash@{2}"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "--only-untracked"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "--numstat", "stash@{1}"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "--raw", "refs/stash"], Shape::Stashed));
    out.push(Case::new(
        "stash",
        &["stash", "show", "--include-untracked", "--stat", "stash@{1}"],
        Shape::Stashed,
    ));
    out.push(Case::new(
        "stash",
        &["stash", "show", "--only-untracked", "--name-only", "stash@{1}"],
        Shape::Stashed,
    ));
}

/// **`create` and `store` — the two halves of `push`, separated.**
///
/// `stash create` builds the entry commit, prints its id and writes **no ref**;
/// `stash store` takes an id and moves the stack onto it. `push` is the two run
/// back to back. Nothing else in the corpus reaches either verb, and they are
/// the only spelling that can tell "made the commit" apart from "moved the
/// stack": a port whose `push` looks right can still be building the commit
/// wrong, and `create`'s printed id is a checksum over the entire entry —
/// message, parent list, and all three trees — in forty hex digits.
///
/// The measured parsing contract, verified against stock 2.55.0: **`create`
/// parses no options at all.** Every argument, `-u` and `--include-untracked`
/// included, is concatenated into the message, and the commit it writes has two
/// parents in all nine spellings below. So `git stash create -u` produces an
/// entry whose message is `On main: -u` and which carries *no* untracked parent.
/// A port that runs the arguments through `push`'s option parser writes a
/// three-parent commit here and a different id in every one of these cases.
fn create_and_store(out: &mut Vec<Case>) {
    for args in [
        &["stash", "create"][..],
        &["stash", "create", "-u"],
        &["stash", "create", "--include-untracked"],
        &["stash", "create", "-a"],
        &["stash", "create", "hello"],
        &["stash", "create", "-m", "msg"],
        &["stash", "create", "--keep-index"],
        &["stash", "create", "--staged"],
        &["stash", "create", "--", "counter.txt"],
    ] {
        out.push(Case::new("stash", args, Shape::Stashed));
    }
    // A clean worktree has nothing to make a commit out of: `create` prints
    // nothing and exits 0, which is not the same as an error and not the same as
    // `push`'s "No local changes to save".
    out.push(Case::new("stash", &["stash", "create"], Shape::Linear));
    // Dirty in four ways `Shape::Stashed` is not — a deletion, a staged add, an
    // untracked file — so the four `create` spellings produce four different ids
    // over content the stash fixture cannot express.
    for args in [
        &["stash", "create"][..],
        &["stash", "create", "-u"],
        &["stash", "create", "-a"],
        &["stash", "create", "--staged"],
    ] {
        out.push(Case::new("stash", args, Shape::Dirty));
    }

    // `store` moves the ref and appends a reflog entry, and takes its message
    // from `-m` rather than from the commit. Storing an entry that is already on
    // the stack is legitimate and pushes the stack down by one.
    out.push(Case::new("stash", &["stash", "store", "refs/stash"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "store", "-m", "stored", "refs/stash"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "store", "-q", "refs/stash"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "store", "-m", "x", "stash@{1}"], Shape::Stashed));
    // The same structural "stash-like" test `apply` applies, on the write side:
    // the index commit has one parent, so it cannot be stored.
    out.push(Case::strict("stash", &["stash", "store", "refs/stash^2"], Shape::Stashed));
    // Unresolvable argument. Note the exit code: git reports this one as a
    // failure to *update the ref* (1), not as a bad revision (128).
    out.push(Case::strict("stash", &["stash", "store", "refs/stash^3"], Shape::Stashed));
    // No stack, so `refs/stash` does not resolve at all.
    out.push(Case::strict("stash", &["stash", "store", "refs/stash"], Shape::Linear));
}

/// **Push flags that only a repository with an existing stack can sort.**
///
/// The existing `push` matrix (`corpus.rs`, `nested.rs`) covers the `-u`/`-a`/
/// `-k`/`-S`/`-q` product. What it does not cover is `--staged` reaching a
/// worktree where the staged and unstaged halves touch the *same* path, the
/// pathspec forms over a fixture that has untracked and ignored files to sort,
/// or the deprecated `save` spelling over a non-empty stack.
///
/// `--staged` over `Shape::Stashed` is the interesting one and was verified
/// against stock 2.55.0: `notes.txt` is staged *and* then edited again in the
/// worktree, so git creates the entry, prints "Saved working directory…", then
/// fails to reverse-apply the staged patch and exits 1 with "Cannot remove
/// worktree changes". The stack has grown by one and the worktree is untouched.
/// A partial success like that is exactly the state a port gets wrong quietly.
fn push_flags_over_a_stack(out: &mut Vec<Case>) {
    out.push(Case::strict("stash", &["stash", "push", "--staged", "-m", "staged-only"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "push", "--staged", "--", "notes.txt"], Shape::Stashed));
    // Mutually exclusive by construction: `--staged` records the index and
    // untracked files are by definition not in it.
    out.push(Case::strict("stash", &["stash", "push", "--staged", "-u"], Shape::Stashed));

    // Pathspec forms. `notes.txt` is the path with both halves; `fresh.txt` is
    // untracked, so it is a pathspec error without `-u` and a normal save with.
    out.push(Case::new("stash", &["stash", "push", "--", "notes.txt"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "push", "--", "fresh.txt"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "push", "-u", "--", "fresh.txt"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "push", "--no-include-untracked"], Shape::Stashed));
    // Bare `stash` with a pathspec and no `push`: a different argv parse that
    // has to arrive at the same place.
    out.push(Case::new("stash", &["stash", "--", "counter.txt"], Shape::Stashed));

    // `save` is the deprecated spelling and takes its message positionally,
    // which is the whole difference from `push`.
    out.push(Case::new("stash", &["stash", "save", "wip"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "save", "-u", "wip"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "save", "-k", "wip"], Shape::Stashed));

    // `--pathspec-from-file` over a stack. The existing cases run it on
    // `Shape::Dirty`, which has no ignored file and no second stash entry, so
    // neither the NUL form nor the `-u` interaction was measured with anything
    // to sort.
    out.push(Case::with_stdin(
        "stash",
        &["stash", "push", "--pathspec-from-file=-"],
        Shape::Stashed,
        b"counter.txt\nnotes.txt\n",
    ));
    out.push(Case::with_stdin(
        "stash",
        &["stash", "push", "--pathspec-from-file=-", "--pathspec-file-nul"],
        Shape::Stashed,
        b"counter.txt\0notes.txt\0",
    ));
    out.push(Case::with_stdin(
        "stash",
        &["stash", "push", "-u", "--pathspec-from-file=-"],
        Shape::Stashed,
        b"fresh.txt\n",
    ));
    out.push(Case::with_stdin(
        "stash",
        &["stash", "push", "-k", "--pathspec-from-file=-"],
        Shape::Stashed,
        b"counter.txt\n",
    ));
    // A pathspec that matches nothing. Verified on stock 2.55.0: this does *not*
    // fail — an entry is created and the stack grows.
    out.push(Case::with_stdin(
        "stash",
        &["stash", "push", "--pathspec-from-file=-"],
        Shape::Stashed,
        b"nosuch.txt\n",
    ));
    // The file and the argv cannot both supply the pathspec.
    out.push(Case {
        compare_stderr: true,
        ..Case::with_stdin(
            "stash",
            &["stash", "push", "--pathspec-from-file=-", "--", "counter.txt"],
            Shape::Stashed,
            b"counter.txt\n",
        )
    });
}

/// **The same questions over worktrees `Shape::Stashed` cannot express.**
///
/// `stash create` is the probe of choice here: it is the whole of `push`'s
/// commit-building half with none of its ref movement, so it reaches each
/// shape's awkward index state and reports the result as one id — or as the
/// refusal that state forces.
fn other_shapes(out: &mut Vec<Case>) {
    // An intent-to-add entry has no blob in the index, so the index commit
    // cannot be written and git refuses before touching anything. This is the
    // shape a previous round found the port exiting 0 over.
    out.push(Case::strict("stash", &["stash", "create"], Shape::IntentToAdd));
    // A staged rename: same refusal, reached through a different index state.
    out.push(Case::strict("stash", &["stash", "create"], Shape::PendingRename));
    // …and the `--staged` half of it, which is the spelling that has to
    // reverse-apply a rename out of the worktree.
    out.push(Case::strict("stash", &["stash", "push", "--staged"], Shape::PendingRename));

    // An unmerged index cannot be committed at all, and the two verbs report it
    // differently: `create` lists every unmerged stage, `push` fails earlier
    // with "could not write index".
    out.push(Case::strict("stash", &["stash", "create"], Shape::Rerere));
    out.push(Case::strict("stash", &["stash", "push", "-m", "during-rerere"], Shape::Rerere));

    // Symlinks: the untracked commit has to carry a symlink as mode 120000
    // rather than as its target's bytes.
    out.push(Case::new("stash", &["stash", "create", "-u"], Shape::Symlinks));
    // A cone-mode sparse checkout whose only untracked file is *outside* the
    // cone. Verified on stock 2.55.0: `create -u` prints nothing and exits 0 —
    // the excluded file is not stashable — while `push` says "No local changes
    // to save". Two different answers to "is this worktree dirty".
    out.push(Case::new("stash", &["stash", "create", "-u"], Shape::Sparse));
    out.push(Case::strict("stash", &["stash", "push"], Shape::Sparse));
}
