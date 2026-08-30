//! Differential corpus cases for the parts of `git stash` that are reached
//! through something other than a plain flag on a plain argv: the interactive
//! `--patch` loop, the `stash.show*` configuration, the revision *spellings* an
//! entry can be named by, `log`'s option surface underneath `stash list`, and
//! `stash`'s behaviour from a subdirectory.
//!
//! Every case is compared against stock git for stdout, exit code and the
//! post-command state digest; the ones whose whole content is a refusal are
//! [`Case::strict`] so the message is compared too.
//!
//! # Territory, and how it was divided
//!
//! `stash` is the most heavily covered verb in the corpus — 508 case ids
//! mention it — so this module is defined by what the neighbours already own.
//! Read in full before a line of this file was written:
//!
//! * **`corpus/stash_deep.rs`** — the nearest neighbour. Owns the entry **as an
//!   object graph** (`cat-file`/`rev-list --parents`/`ls-tree` over `^1`, `^2`
//!   and `^3`), the reflog as the stack, `create` and `store`'s normal forms,
//!   `--staged` over a stack, `--pathspec-from-file=-` from stdin, and the
//!   `apply`/`pop` refusal family over `stash@{2}`. Nothing here restates any of
//!   it: no case below reads a parent with plumbing, and the `create`/`store`
//!   cases here are exactly the argument shapes that file does not spell.
//! * **`corpus/sequences.rs`** — owns every multi-step stash workflow
//!   (`pop-conflict-resolve-drop`, `drop-by-index-then-pop`,
//!   `keep-index-commit-then-pop-conflict`, `clear-then-empty-stack-refusals`,
//!   `the-stack-is-the-reflog-of-the-stash-ref`,
//!   `expiring-every-reflog-empties-the-whole-stack`, `branch-from-a-named-entry`,
//!   `apply-twice-second-refuses`). A [`Case`] is one argv against a pristine
//!   copy, so anything needing a prior commit or a prior push belongs there and
//!   is not half-stated here.
//! * **`corpus/rerere_engine.rs`** — its header records that
//!   [`Shape::Stashed`]'s worktree is dirty on the paths its entries touch, so
//!   `apply`/`pop` is refused before any merge runs and there is nothing for
//!   rerere to record. See the correction under "What no shape can ask" below:
//!   that is true of `stash@{0}` and `stash@{2}` and **not** of `stash@{1}`.
//! * **`corpus/merge_dirty.rs`** — `--autostash` (which its header calls "`git
//!   stash create` in all but name") on the merge side; no `stash` argv at all.
//! * **`corpus/index_plumbing.rs`** — owns `status --porcelain=v2 --show-stash`,
//!   the only place the stash appears in another command's output.
//! * **`corpus/pathspec_stdin.rs`** — owns `stash push --pathspec-from-file=-`
//!   over [`Shape::Dirty`], both separators. This module takes the form it does
//!   not: `--pathspec-from-file=<path>`, reading a file that is *in* the
//!   fixture.
//! * **`corpus/stateful_side_files.rs`**, **`corpus/worktree_index.rs`** — no
//!   `stash` argv in either, contrary to what a plan might assume; checked
//!   rather than presumed.
//!
//! What is left, and is what this module is:
//!
//! 1. **`push --patch`, driven.** `env::harden` pins `GIT_EDITOR=true`, and the
//!    corpus reasonably read that as "interactive verbs are unreachable". For
//!    `stash push -p` that is wrong: the hunk loop is not an *editor*, it is a
//!    prompt on **stdin**, and [`Case::with_stdin`] delivers stdin. Every
//!    keystroke below is one of `y n q a d p P ?` — deliberately never `e`,
//!    which is the one answer that does open `GIT_EDITOR` and would measure
//!    `true` instead of git. With no payload at all the loop reads EOF, which is
//!    its own measured behaviour ("No changes selected", exit 1, nothing moved).
//!    Nothing in the corpus had ever selected a hunk.
//! 2. **`stash.showPatch` / `stash.showStat` / `stash.showIncludeUntracked` /
//!    `stash.showOnlyUntracked`.** Not one `-c stash.*` exists anywhere in the
//!    corpus, so `stash show`'s entire configuration surface was measured only
//!    at its defaults — including the value that makes it print **nothing**.
//! 3. **The spellings of an entry.** `stash@{n}` and `refs/stash` are covered;
//!    `stash@{/text}`, `refs/stash^{/text}`, `:/text`, `stash@{-1}`,
//!    `stash@{+1}` and bare `stash` are not, and they do not all fail — see the
//!    group's own note for which resolve.
//! 4. **`log`'s options under `stash list`.** `--date=raw`/`unix` and a handful
//!    of `--format`s are covered; the rendering formats, `--grep`, `--all`,
//!    `--name-status` and `--parents` are not.
//! 5. **`show`'s diff options and the argument-count contract**, `store`'s and
//!    `branch`'s argument errors, and `push`'s message and pathspec *spellings*
//!    (`--message=`, an empty `-m`, a bare pathspec with no `--`, pathspec
//!    magic, a pathspec file inside the repository).
//! 6. **`stash` from a subdirectory.** Checked against `--list-cases`: before
//!    this module the whole corpus had two `stash` cases with a `cwd`, `stash
//!    list` from `sub` in [`Shape::Hooked`] and a bare `stash` from
//!    `.remote.git` in [`Shape::BehindRemote`]. `push` from a subdirectory is
//!    where the pathspec prefix lives and `show` from one is where it must
//!    *not* be applied, and neither had ever been asked.
//!
//! # Determinism
//!
//! `push`, `create` and `store` mint commits at case run time, so their ids are
//! part of what is compared. Two separate checks back that up rather than one
//! claim. The harness itself re-runs any failing case a second time and reports
//! a side that did not reproduce itself as `zvcs-flaky`, having first confirmed
//! that stock reproduced its own stdout and post-state; over the whole `stash`
//! corpus that count is zero. On top of that, every case *added* here was run
//! twice by hand against stock in identical fresh copies of a
//! [`Shape::Stashed`] replica and the output plus a digest (`cat-file
//! --batch-all-objects`, `stash list`, `reflog show stash`) diffed clean. That
//! holds only because [`crate::env`] pins both dates and both identities.
//!
//! Two things are therefore **not** here, and are not measurable rather than
//! merely omitted:
//!
//! * **`stash list --date=relative` and `--date=human`.** Both render against
//!   the wall clock (`stash@{2 years, 10 months ago}` on stock today). The two
//!   sides run seconds apart so they would usually agree, but "usually" is a
//!   flake, not a measurement, and the corpus does not ship one.
//! * **The `e` answer in the `--patch` loop.** It hands the hunk to
//!   `GIT_EDITOR`, which `harden` pins to `true`; that pin may not be
//!   re-pointed from a case, so what `e` does with a *real* editor cannot be
//!   asked here at all. `p`/`P` are used instead: `P` pages through
//!   `GIT_PAGER=cat`, which is pinned to something that prints.
//!
//! # What no shape can ask
//!
//! [`Shape::Stashed`] is the only shape with entries, and its worktree is dirty
//! on `counter.txt` and `notes.txt`. Measured on stock 2.55.0, one entry at a
//! time:
//!
//! ```text
//!   stash apply stash@{0}  → refused, "notes.txt would be overwritten",   rc 1
//!   stash apply stash@{1}  → "Already up to date.", extra.txt restored,   rc 0
//!   stash apply stash@{2}  → refused, "counter.txt would be overwritten", rc 1
//! ```
//!
//! So a single-invocation `apply` **can** succeed here, which corrects the
//! blanket statement in `rerere_engine.rs`'s header: `stash@{1}` carries no
//! tracked change at all (it was pushed with `-u` over a clean worktree), so its
//! merge is a no-op and its untracked parent is unpacked into a worktree holding
//! nothing at that path. What is still unreachable in one invocation is a
//! *conflicting* apply — that needs a commit first, and `sequences.rs` owns it.
//! No case below tries to manufacture one.

use crate::fixture::Shape;
use crate::runner::Case;

pub fn cases(out: &mut Vec<Case>) {
    patch_loop(out);
    show_configuration(out);
    entry_spellings(out);
    list_is_a_log(out);
    show_option_surface(out);
    argument_contracts(out);
    push_spellings(out);
    from_a_subdirectory(out);
    empty_stack_and_clean_tree(out);
}

/// **`push --patch`, answered.** The hunk loop reads stdin, not an editor, so
/// [`Case::with_stdin`] drives it and the *selection* becomes measurable.
///
/// [`Shape::Stashed`]'s worktree offers exactly two hunks in a fixed order —
/// `counter.txt`'s appended `worktree-unstaged` line, then `notes.txt`'s two
/// appended lines — which is what makes a two-byte payload a precise
/// instruction. Verified against stock 2.55.0:
///
/// ```text
///   (no stdin)  prompt printed, "No changes selected" on stderr, rc 1, nothing moved
///   y y         both hunks stashed, entry has two parents, worktree keeps `MM notes.txt`
///   n y         only `notes.txt` stashed; `counter.txt` stays ` M`
///   q           quits at the first prompt: same result as EOF, rc 1
///   d y         `d` skips the rest of *that file*, so the second hunk is still
///               offered — and because `counter.txt` has exactly one hunk, the
///               entry this builds is byte-identical to the `n y` one
///               (`320ed5a3494c73341f34c638563e623156b7af9f` both times). The
///               case is kept for the *answer*, not for a distinct entry: a port
///               that does not implement `d` reprompts instead of proceeding.
///   a           `a` takes the rest of that file, then EOF at `notes.txt`'s
///               prompt still commits what was selected (rc 0, entry
///               `de086f0936dffc4361ce65ca6f5fc3edfb7f2ae7`) — EOF is only
///               "No changes selected" when nothing was selected at all
/// ```
///
/// The refusals are [`Case::strict`] because "No changes selected" is on
/// **stderr** — stdout ends with the prompt — so a port that exits 1 silently
/// would otherwise score as agreement.
fn patch_loop(out: &mut Vec<Case>) {
    // EOF at the first prompt. The hunk and the prompt are still printed, so a
    // port that refuses `-p` outright diverges on stdout as well as on stderr.
    out.push(Case::strict("stash", &["stash", "push", "-p"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "push", "--patch"], Shape::Stashed));

    // The selections. Each of these mints a commit, so the case compares the
    // printed "Saved working directory…" line, the reflog it appended, and the
    // object ids of the entry and its index parent through the state digest.
    out.push(Case::with_stdin("stash", &["stash", "push", "-p"], Shape::Stashed, b"y\ny\n"));
    out.push(Case::with_stdin("stash", &["stash", "push", "-p"], Shape::Stashed, b"n\ny\n"));
    out.push(Case::with_stdin("stash", &["stash", "push", "-p"], Shape::Stashed, b"y\nn\n"));
    out.push(Case::with_stdin("stash", &["stash", "push", "-p"], Shape::Stashed, b"d\ny\n"));
    out.push(Case::with_stdin("stash", &["stash", "push", "-p"], Shape::Stashed, b"a\n"));
    // `q` at the first prompt abandons everything, including the hunk that was
    // never offered — the same outcome as EOF but reached by an answer.
    out.push(Case {
        compare_stderr: true,
        ..Case::with_stdin("stash", &["stash", "push", "-p"], Shape::Stashed, b"q\n")
    });
    // `?` prints the whole key legend, and `p` reprints the current hunk: two
    // blocks of prose emitted by the loop itself rather than by `diff`.
    out.push(Case::with_stdin("stash", &["stash", "push", "-p"], Shape::Stashed, b"?\nq\n"));
    out.push(Case::with_stdin("stash", &["stash", "push", "-p"], Shape::Stashed, b"p\nq\n"));
    // `P` routes the reprint through the pager, which `harden` pins to `cat`.
    out.push(Case::with_stdin("stash", &["stash", "push", "-p"], Shape::Stashed, b"P\nq\n"));

    // The message is taken from `-m` exactly as in a non-interactive push, so a
    // port that builds the interactive entry down a separate path prints the
    // default `WIP on main: <sha> <subject>` here instead.
    out.push(Case::with_stdin("stash", &["stash", "push", "-p", "-m", "patched"], Shape::Stashed, b"y\nn\n"));
    // A pathspec narrows what the loop offers — one hunk instead of two — so the
    // single `y` finishes the selection rather than leaving a prompt unanswered.
    out.push(Case::with_stdin("stash", &["stash", "push", "-p", "--", "counter.txt"], Shape::Stashed, b"y\n"));
    out.push(Case::with_stdin("stash", &["stash", "push", "-p", "--", "notes.txt"], Shape::Stashed, b"y\n"));

    // `-p` implies `--keep-index`, so these two spellings ask whether the
    // implication is honoured and whether it can be turned back off.
    out.push(Case::with_stdin("stash", &["stash", "push", "-p", "-k"], Shape::Stashed, b"y\ny\n"));
    out.push(Case::with_stdin("stash", &["stash", "push", "-p", "--no-keep-index"], Shape::Stashed, b"y\ny\n"));
    // Measured on stock 2.55.0: `-S` alongside `-p` is **not** rejected — the
    // patch loop runs and the entry is built as if `-S` were absent. A port that
    // reads the two as mutually exclusive fails here with an error.
    out.push(Case::with_stdin("stash", &["stash", "push", "-p", "-S"], Shape::Stashed, b"y\ny\n"));
    // These two *are* rejected, before a single hunk is printed, and the
    // sentence names both spellings at once.
    out.push(Case::strict("stash", &["stash", "push", "-p", "-u"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "push", "-p", "-a"], Shape::Stashed));

    // The two argv spellings that reach the same loop without saying `push`.
    out.push(Case::with_stdin("stash", &["stash", "-p"], Shape::Stashed, b"y\ny\n"));
    out.push(Case::with_stdin("stash", &["stash", "save", "-p", "wip"], Shape::Stashed, b"y\ny\n"));

    // Nothing to offer: the loop is never entered and the ordinary
    // "No local changes to save" is printed instead of a prompt.
    out.push(Case::strict("stash", &["stash", "push", "-p"], Shape::Linear));
}

/// **`stash show`'s configuration.** Four keys decide what `show` prints when
/// no flag says otherwise, and no case in the corpus sets any of them.
///
/// Measured on stock 2.55.0 over [`Shape::Stashed`]: the default is `--stat`;
/// `stash.showPatch=true` prints the stat **and** the patch; `stash.showStat=false`
/// alone prints **nothing at all** and still exits 0 — which is the value a port
/// that hardcodes the stat cannot produce. An explicit flag wins over the key in
/// both directions, and a non-boolean value is `fatal: bad boolean config value
/// 'bogus' for 'stash.showstat'` at 128.
///
/// The untracked keys do **not** mirror their flags, which was measured rather
/// than assumed and is what the last three cases here are for:
///
/// ```text
///   -c stash.showOnlyUntracked=true  show stash@{1}   prints nothing
///   --only-untracked                 show stash@{1}   prints extra.txt
///   -c stash.showOnlyUntracked=true  show stash@{2}   prints the tracked stat
///   --only-untracked                 show stash@{2}   prints nothing
/// ```
///
/// `stash@{1}` is the entry with an untracked parent and `stash@{2}` the one
/// without, so on both entries the key and the flag answer *differently*, and in
/// opposite directions. A port that implements the key by setting the flag gets
/// both of the config rows wrong.
fn show_configuration(out: &mut Vec<Case>) {
    for (key, value) in [
        ("stash.showPatch", "true"),
        ("stash.showPatch", "false"),
        ("stash.showStat", "false"),
        ("stash.showStat", "true"),
    ] {
        out.push(Case::new("stash", &["stash", "show"], Shape::Stashed).with_config(&[(key, value)]));
    }
    // Both off: the one combination that produces an empty successful run.
    out.push(
        Case::new("stash", &["stash", "show"], Shape::Stashed)
            .with_config(&[("stash.showStat", "false"), ("stash.showPatch", "false")]),
    );
    out.push(
        Case::new("stash", &["stash", "show"], Shape::Stashed)
            .with_config(&[("stash.showStat", "true"), ("stash.showPatch", "true")]),
    );

    // Flag versus key, in both directions.
    out.push(
        Case::new("stash", &["stash", "show", "--stat"], Shape::Stashed)
            .with_config(&[("stash.showPatch", "true")]),
    );
    out.push(
        Case::new("stash", &["stash", "show", "--no-patch"], Shape::Stashed)
            .with_config(&[("stash.showPatch", "true")]),
    );
    out.push(
        Case::new("stash", &["stash", "show", "-p"], Shape::Stashed)
            .with_config(&[("stash.showStat", "false")]),
    );

    // The untracked keys, asked of `stash@{1}` — the only entry with a third
    // parent. `showIncludeUntracked` adds the untracked half to the tracked one;
    // over `@{1}` the tracked half is empty, so what prints is `extra.txt` alone.
    out.push(
        Case::new("stash", &["stash", "show", "stash@{1}"], Shape::Stashed)
            .with_config(&[("stash.showIncludeUntracked", "true")]),
    );
    out.push(
        Case::new("stash", &["stash", "show", "--no-include-untracked", "stash@{1}"], Shape::Stashed)
            .with_config(&[("stash.showIncludeUntracked", "true")]),
    );
    out.push(
        Case::new("stash", &["stash", "show", "stash@{1}"], Shape::Stashed)
            .with_config(&[("stash.showIncludeUntracked", "true"), ("stash.showPatch", "true")]),
    );
    out.push(
        Case::new("stash", &["stash", "show", "--only-untracked", "stash@{1}"], Shape::Stashed)
            .with_config(&[("stash.showPatch", "true")]),
    );
    // `showIncludeUntracked` on the default entry, which has no untracked parent:
    // measured, it adds nothing and the ordinary `notes.txt` stat is printed, so
    // the key must not invent a parent that is not there.
    out.push(
        Case::new("stash", &["stash", "show"], Shape::Stashed)
            .with_config(&[("stash.showIncludeUntracked", "true")]),
    );
    // The two `showOnlyUntracked` rows of the table above. On `stash@{2}` — no
    // untracked parent — the key is measurably *ignored* and the tracked stat is
    // printed, while the `--only-untracked` flag on the same entry prints
    // nothing (that flag case lives in `show_option_surface` below). On
    // `stash@{1}` — which does have one — the key prints nothing while the flag
    // prints `extra.txt`. Both rows are needed: either one alone is consistent
    // with "the key is the flag".
    out.push(
        Case::new("stash", &["stash", "show", "stash@{2}"], Shape::Stashed)
            .with_config(&[("stash.showOnlyUntracked", "true")]),
    );
    out.push(
        Case::new("stash", &["stash", "show", "stash@{1}"], Shape::Stashed)
            .with_config(&[("stash.showOnlyUntracked", "true")]),
    );

    // A value that is not a boolean is fatal, and the message lower-cases the
    // key the way git's config parser always reports it.
    out.push(
        Case::strict("stash", &["stash", "show"], Shape::Stashed)
            .with_config(&[("stash.showStat", "bogus")]),
    );
}

/// **How an entry may be named.** `stash@{n}` and `refs/stash` are settled
/// elsewhere; these are the other spellings a user reaches for, and stock's
/// answers are not uniform.
///
/// Measured on stock 2.55.0:
///
/// ```text
///   stash@{/untracked}        rev-parse: fatal, rc 128 · stash show: "is not a
///                             valid reference", rc 1 — the `@{/text}` form is
///                             **not** a stash selector at all
///   refs/stash^{/untracked}   same pair of refusals
///   :/untracked               "is not a valid reference", rc 1
///   stash@{-1}                "is not a valid reference", rc 1
///   stash@{+1}                accepted, rc 0
///   stash                     accepted — the bare ref name works
///   stash@{0}^{/staged}       accepted — `^{/text}` searches *from* the entry
///   stash@{0}^2               "is not a stash-like commit", rc 128
/// ```
///
/// All three accepted spellings resolve to the **same** entry — `rev-parse` on
/// `stash@{0}`, `stash@{+1}`, `stash` and `stash@{0}^{/staged}` all print
/// `25920b7814e06917634853e7fc52ea4ebf6075c9` — so the three `show` cases below
/// print identical bytes on stock. That is the point rather than a defect in the
/// group: they are three different resolution paths to one answer, and a port
/// that implements two of them prints two matching lines and one empty one.
///
/// Every one is [`Case::strict`] where it fails, because the whole content of
/// the case is which of those three refusals git chose and at which exit code.
fn entry_spellings(out: &mut Vec<Case>) {
    // The form that looks like a stash selector and is not one. Two front doors,
    // two different refusals: `rev-parse`'s revision parser dies at 128,
    // `stash`'s own resolver reports 1.
    out.push(Case::strict("rev-parse", &["rev-parse", "stash@{/untracked}"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "show", "stash@{/untracked}"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "apply", "stash@{/untracked}"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "drop", "stash@{/untracked}"], Shape::Stashed));
    out.push(Case::strict("rev-parse", &["rev-parse", "stash@{/}"], Shape::Stashed));

    // The two spellings that *are* real revision syntax. `^{/text}` anchored at
    // an entry resolves (the entry's own message matches); anchored at the ref
    // with a `refs/` prefix it does not.
    out.push(Case::strict("rev-parse", &["rev-parse", "refs/stash^{/untracked}"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "stash@{0}^{/staged}"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "show", ":/untracked"], Shape::Stashed));

    // Signed reflog indices. `-1` is refused; `+1` is accepted, which is the
    // asymmetry a port is unlikely to reproduce by accident.
    out.push(Case::strict("stash", &["stash", "show", "stash@{-1}"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "stash@{+1}"], Shape::Stashed));

    // The bare ref name, and the fully qualified reflog selector. `drop
    // refs/stash@{1}` is `stash_deep`'s; `apply` through the same spelling is
    // not, and it is the one that has to reach the overwrite refusal.
    out.push(Case::new("stash", &["stash", "show", "stash"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "apply", "refs/stash@{2}"], Shape::Stashed));
    // A parent of an entry is a commit but not a stash, and `show` says so with
    // a different sentence than `apply` does.
    out.push(Case::strict("stash", &["stash", "show", "stash@{0}^2"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "drop", "refs/stash^2"], Shape::Stashed));
}

/// **`stash list` is `log --walk-reflogs` with a ref baked in**, so every `log`
/// option is part of its surface. The corpus covers `--date=raw`/`unix` and four
/// `--format`s; these are the rest of the rendering and selection surface.
///
/// `--date=relative` and `--date=human` are deliberately absent — see the module
/// header. Every format below renders [`crate::env::FIXED_DATE`] through a fixed
/// `TZ=UTC`, so none of them reads a clock: `--date=format:%Y-%m-%d` prints
/// `2023-11-14` and rebuilds the selector as `stash@{2023-11-14}`, which is the
/// clearest demonstration that `%gd` is *derived* from the date rather than
/// stored. All were checked byte-identical across two stock runs.
fn list_is_a_log(out: &mut Vec<Case>) {
    // The selector `stash@{…}` is rebuilt from the date in every one of these,
    // so the format is measured twice per line: once in `%gd` and once in `%ad`.
    for date in ["iso", "iso-strict", "short", "rfc", "default", "local", "format:%Y-%m-%d"] {
        let flag = format!("--date={date}");
        out.push(Case::new(
            "stash",
            &["stash", "list", flag.as_str(), "--format=%gd %ad %gs"],
            Shape::Stashed,
        ));
    }

    // Reflog identity, which is a different pair of fields from the commit's own
    // author and is what `drop` has to preserve when it rewrites the log.
    out.push(Case::new("stash", &["stash", "list", "--format=%gn %ge"], Shape::Stashed));
    // `--pretty=raw` prints the `Reflog:`/`Reflog message:` headers *and* the
    // full parent list of every entry, so one invocation states the whole stack's
    // shape in porcelain.
    out.push(Case::new("stash", &["stash", "list", "--pretty=raw"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "--parents", "--format=%h %p"], Shape::Stashed));

    // Selection, not rendering. `--grep` filters a reflog walk by the *commit*
    // message, which for a stash entry is the same text as the reflog message.
    out.push(Case::new("stash", &["stash", "list", "--grep=untracked"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "--grep=nosuchentry"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "-i", "--grep=UNSTAGED"], Shape::Stashed));
    // `--all` widens the walk to every reflog, so `main`, `HEAD` and `stash` all
    // print — and `stash` prints twice, because `stash list` already named it.
    out.push(Case::new("stash", &["stash", "list", "--all"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "--no-walk"], Shape::Stashed));

    // Diff output attached to a reflog walk. `--name-status` and `--numstat` are
    // the two forms of "what did each entry touch" that `stash list` has never
    // been asked for.
    out.push(Case::new("stash", &["stash", "list", "--name-status"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "--numstat"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "list", "--shortstat"], Shape::Stashed));

    // A pathspec on a reflog walk. Verified on stock: this is **not** a path
    // filter — `stash list` passes it to the revision parser, which reads it as a
    // second revision and dies.
    out.push(Case::strict("stash", &["stash", "list", "--", "counter.txt"], Shape::Stashed));
}

/// **`stash show` hands its unrecognized options to `diff`**, and takes exactly
/// one revision.
///
/// The corpus covers `-p`, `--stat`, `--name-only`, `--numstat`, `--raw` and
/// `--cached`. These are the options that change *whether* a patch appears at
/// all — `-U0` turns one on with no `-p` in sight, `--summary` prints nothing
/// for a content-only change — plus the argument-count contract, which is the
/// only place `stash show` refuses something that resolves.
fn show_option_surface(out: &mut Vec<Case>) {
    // Verified on stock 2.55.0: `-U0` alone produces a patch, so "did the user
    // ask for a patch" is not simply "was `-p` given".
    out.push(Case::new("stash", &["stash", "show", "-U0"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "-U1", "stash@{2}"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "--shortstat"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "--patch-with-stat", "stash@{2}"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "--summary", "stash@{1}"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "--stat=200", "stash@{2}"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "-p", "--word-diff=porcelain"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "show", "--no-patch", "--stat"], Shape::Stashed));
    // `--only-untracked` on the entry that has no untracked parent. Measured on
    // stock: the flag is valid, the parent is absent, and git prints **nothing**
    // and exits 0 — it neither errors nor falls back to the tracked diff. Strict
    // because "prints nothing" is only a measurement if stderr is compared too,
    // and it is the flag half of the table in `show_configuration`'s doc: the
    // *config* key on this same entry prints the tracked stat instead.
    out.push(Case::strict("stash", &["stash", "show", "--only-untracked", "stash@{2}"], Shape::Stashed));

    // Two revisions. `show` takes one, and the refusal quotes both back.
    out.push(Case::strict("stash", &["stash", "show", "stash@{0}", "stash@{1}"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "show", "refs/stash", "refs/stash"], Shape::Stashed));
}

/// **The argument contracts of `store` and `branch`**, which are refusals
/// reached before anything is written.
///
/// `stash_deep` covers `store`'s normal forms and its two "not stash-like"
/// refusals. Left over: the arity errors (none, two), an argument that is not a
/// commit *at all*, an ordinary commit, and the `--message=` spelling. `branch`
/// is covered with a name and an entry; left over is the missing name and a name
/// `check-ref-format` rejects.
fn argument_contracts(out: &mut Vec<Case>) {
    // Arity. Both spell the same sentence and exit 1 — not 128, which is what
    // the *content* errors below use.
    out.push(Case::strict("stash", &["stash", "store"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "store", "refs/stash", "refs/stash"], Shape::Stashed));
    // A tree is not a commit: two lines, an `error:` naming the object type and
    // then the `fatal:` about stash-likeness, at 128.
    out.push(Case::strict("stash", &["stash", "store", "refs/stash^{tree}"], Shape::Stashed));
    // An ordinary commit resolves and is refused for its shape alone.
    out.push(Case::strict("stash", &["stash", "store", "HEAD"], Shape::Stashed));
    // The long spelling of the message. Measured on stock: storing the commit the
    // ref already names is accepted, prints nothing, and appends no reflog entry
    // — the ref did not move, so there is nothing to log.
    out.push(Case::new("stash", &["stash", "store", "--message=stored", "refs/stash"], Shape::Stashed));
    // Storing a *different* entry does move the ref, and this is the spelling
    // that proves the message came from `-m` and not from the commit.
    out.push(Case::new("stash", &["stash", "store", "--message=lifted", "stash@{2}"], Shape::Stashed));

    // A message with a newline in it, which is the one place a stash entry's
    // *commit message* and its *reflog line* have to disagree. Measured on
    // stock: the commit keeps the newline, the reflog collapses it to a space,
    // so `stash list` prints `stash@{0}: two lines` on one line while
    // `cat-file commit` holds two. A port that writes one representation into
    // both places is caught by exactly one of the two probes.
    out.push(Case::new("stash", &["stash", "store", "--message=two\nlines", "stash@{2}"], Shape::Stashed));

    out.push(Case::strict("stash", &["stash", "branch"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "branch", "bad..name"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "branch", "off", "stash@{9}"], Shape::Stashed));
}

/// **The spellings of `push`'s message and pathspec**, as opposed to its flags.
///
/// The flag product is exhaustively covered (`corpus.rs`, `nested.rs`,
/// `stash_deep.rs`). What is not covered is how the *operands* are written: an
/// attached `--message=`, an empty message, a pathspec with no `--` in front of
/// it, pathspec magic, and `--pathspec-from-file` pointed at a file in the
/// repository rather than at stdin.
///
/// Verified on stock 2.55.0, and each is a different answer:
///
/// ```text
///   -m ''                        falls back to `WIP on main: <sha> <subject>`
///   notes.txt (no `--`)          accepted as a pathspec
///   :(glob)*.txt                 matches both dirty paths
///   :!notes.txt                  excludes it; only `counter.txt` is stashed
///   --pathspec-from-file=.gitignore    reads `ignored.txt`, which is ignored:
///                                pathspec error, rc 1, nothing saved
///   -u --pathspec-from-file=.gitignore same file, `-u` on: no error at all,
///                                "No local changes to save", rc 0
/// ```
///
/// That last pair is the interesting one: the same pathspec file is a hard error
/// without `-u` and a quiet no-op with it.
fn push_spellings(out: &mut Vec<Case>) {
    out.push(Case::new("stash", &["stash", "push", "--message=inline"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "push", "-m", ""], Shape::Stashed));
    // The same newline split as `store --message=` above, reached through the
    // verb that also has to print the message back. Measured on stock: stdout is
    // `Saved working directory and index state On main: two\nlines` — two lines
    // — while `stash list` shows the single line `stash@{0}: On main: two lines`.
    out.push(Case::new("stash", &["stash", "push", "-m", "two\nlines"], Shape::Stashed));
    // Trailing whitespace: kept in the commit message, trimmed in the reflog
    // line. The narrowest form of the same rule, and the one a port that trims
    // once and reuses the result cannot reproduce.
    out.push(Case::new("stash", &["stash", "push", "-m", "trailing space "], Shape::Stashed));
    // `create` is the only spelling that prints the entry's **object id** on
    // stdout, so a message that differs by one byte is a *stdout* difference and
    // not only a state one. Two facts about it were measured here rather than
    // assumed, and both are surprising enough to be worth pinning:
    //
    //  * **`create` has no options at all — every argument is message text.**
    //    `stash create -m two<newline>lines` stores the message
    //    `On main: -m two\nlines`, flag spelling and all, and still builds a
    //    two-parent entry. (So do the corpus's existing `create -u` and
    //    `create --include-untracked` cases, which store `On main: -u` and
    //    `On main: --include-untracked`; a three-parent entry is not reachable
    //    through `create` at any spelling, and the one this module does mint is
    //    `push -u --message=inline-u` below, whose entry has parents
    //    HEAD + index + untracked.)
    //  * **An empty message argument falls back to the default.**
    //    `stash create ''` prints exactly the id bare `stash create` prints
    //    (`73a881c0587e19b407e6ca8fa611d639aef0e679`, message
    //    `WIP on main: d810229 ignore ignored.txt`) rather than storing
    //    `On main: `. It is the `create`-side mirror of `push -m ''` above, and
    //    the only place that fallback is visible in *stdout*.
    out.push(Case::new("stash", &["stash", "create", "-m", "two\nlines"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "create", ""], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "push", "-u", "--message=inline-u"], Shape::Stashed));

    // A bare pathspec, with and without the flag that changes what it may match.
    out.push(Case::new("stash", &["stash", "push", "notes.txt"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "push", "fresh.txt"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "push", "-u", "fresh.txt"], Shape::Stashed));

    // Two pathspecs at once, both matching. Checked against `--list-cases`:
    // every other `stash` case in the corpus passes one path or one magic word
    // after `--`, so this is the only place the pathspec list has more than one
    // entry, and it is what a port that reads the first operand after `--` and
    // stops gets wrong.
    out.push(Case::new("stash", &["stash", "push", "--", "counter.txt", "notes.txt"], Shape::Stashed));

    // Pathspec magic, which is parsed by the pathspec machinery rather than by
    // `stash`'s option parser and so is a different code path from `-- <path>`.
    out.push(Case::new("stash", &["stash", "push", ":(glob)*.txt"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "push", ":!notes.txt"], Shape::Stashed));
    out.push(Case::new("stash", &["stash", "push", "--", ":(icase)NOTES.TXT"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "push", "--", ":(literal)*.txt"], Shape::Stashed));

    // `--pathspec-from-file=<path>`, the form `pathspec_stdin.rs` does not
    // reach: the file is read from the worktree, and its `:(prefix:0)` rendering
    // shows up in the error message.
    out.push(Case::strict("stash", &["stash", "push", "--pathspec-from-file=.gitignore"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "push", "-u", "--pathspec-from-file=.gitignore"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "push", "--pathspec-from-file=notes.txt"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "push", "--pathspec-from-file=nosuchfile"], Shape::Stashed));
}

/// **`stash` run from a subdirectory.** [`Shape::Stashed`] carries `src/` from
/// the base fixture, and every dirty path is at the root — so `src` is a
/// directory with nothing of its own to stash, which is exactly the prefix
/// question.
///
/// Verified on stock 2.55.0: `push` with no pathspec from `src` stashes the
/// whole worktree (the prefix is not applied to an absent pathspec), `push -- .`
/// from `src` matches nothing and prints "No local changes to save" at rc 0, and
/// `show` from `src` prints paths **repo-relative** — a port that renders diff
/// paths relative to the cwd diverges on that one and nowhere else.
fn from_a_subdirectory(out: &mut Vec<Case>) {
    out.push(Case::new("stash", &["stash", "push"], Shape::Stashed).in_dir("src"));
    out.push(Case::strict("stash", &["stash", "push", "--", "."], Shape::Stashed).in_dir("src"));
    out.push(Case::new("stash", &["stash", "push", "--", ":/counter.txt"], Shape::Stashed).in_dir("src"));
    out.push(Case::new("stash", &["stash", "push", "--", ":/"], Shape::Stashed).in_dir("src"));
    out.push(Case::new("stash", &["stash", "create"], Shape::Stashed).in_dir("src"));
    out.push(Case::new("stash", &["stash", "show", "--stat"], Shape::Stashed).in_dir("src"));
    out.push(Case::new("stash", &["stash", "show", "--name-only", "stash@{2}"], Shape::Stashed).in_dir("src"));
    out.push(Case::new("stash", &["stash", "list", "--format=%gd %gs"], Shape::Stashed).in_dir("src"));
    // `push -u` from a subdirectory has to decide whether the untracked file at
    // the *root* is in scope. It is: the prefix limits pathspecs, not the sweep.
    out.push(Case::new("stash", &["stash", "push", "-u"], Shape::Stashed).in_dir("src"));
}

/// **Nothing to stash, and nothing to show.** [`Shape::Linear`] is a clean
/// single-commit repository with no `refs/stash` at all.
///
/// The corpus reaches it with `create`, `pop`, `drop` and `store`. Left over are
/// the verbs that have to distinguish "the stack is empty" (an error) from "the
/// worktree is clean" (not one). Measured on stock: `push` prints "No local
/// changes to save" and exits **0** for every flag spelling below, while `show`,
/// `apply` and `branch` all print "No stash entries found." and exit 1 —
/// `branch` was already the only one of the three the corpus asked, so `show`
/// and `apply` are added here to pin that the sentence and the code come from
/// the empty *stack* and not from the branch-name argument.
fn empty_stack_and_clean_tree(out: &mut Vec<Case>) {
    out.push(Case::strict("stash", &["stash", "push"], Shape::Linear));
    out.push(Case::strict("stash", &["stash", "push", "-u"], Shape::Linear));
    out.push(Case::strict("stash", &["stash", "push", "-a"], Shape::Linear));
    out.push(Case::strict("stash", &["stash", "push", "-m", "nothing"], Shape::Linear));
    out.push(Case::strict("stash", &["stash", "push", "--", "README.md"], Shape::Linear));
    out.push(Case::strict("stash", &["stash", "branch", "recovered"], Shape::Linear));
    out.push(Case::strict("stash", &["stash", "show"], Shape::Linear));
    out.push(Case::strict("stash", &["stash", "apply"], Shape::Linear));
    // An empty stack is not an error to *list*, at any format.
    out.push(Case::new("stash", &["stash", "list", "--format=%gd %gs"], Shape::Linear));
    out.push(Case::new("stash", &["stash", "list", "--all"], Shape::Linear));

    // A worktree that is dirty in ways [`Shape::Stashed`] is not — an edit on a
    // path several branches rewrite, an edit on a path none of them do, and an
    // untracked file sitting where two of them want to write — with no stack
    // underneath. `create` reports the whole of it as one id.
    out.push(Case::new("stash", &["stash", "create"], Shape::MergeableDirty));
    out.push(Case::new("stash", &["stash", "create", "-u"], Shape::MergeableDirty));
    out.push(Case::new("stash", &["stash", "push", "-u", "-m", "mergeable"], Shape::MergeableDirty));
}
