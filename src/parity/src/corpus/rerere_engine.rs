//! `rerere`: the conflict-identity machinery, and the operations that feed it.
//!
//! # What this module owns, and what the neighbours already owned
//!
//! Six modules touch rerere. The split is by *question*, not by verb:
//!
//! * `merge_family.rs` — the verbs against a repository with **no** `rr-cache`
//!   ([`Shape::Linear`], [`Shape::Conflicted`]): every subcommand as a silent
//!   success, plus `-c rerere.enabled=true rerere` recording the one add/add
//!   conflict, plus `rerere bogus`.
//! * `fixture_gaps2.rs` — the verbs against a repository that **has** one
//!   ([`Shape::Rerere`]): bare `rerere`, `status`, `remaining`, `diff`, `gc`,
//!   `clear`, and `forget` over each of the three paths and `.`. All at the
//!   default configuration, all from the worktree root.
//! * `sequences.rs` — every **multi-invocation** workflow: record-then-replay on
//!   [`Shape::Conflicted`] and [`Shape::CrissCross`], forget-then-recreate,
//!   `merge --abort` keeping the cache, and `rerere gc` with both `gc.rerere*`
//!   keys at `0` followed by the merge that proves what it deleted. A single
//!   `Case` is one invocation, so the replay — git writing an old resolution
//!   back into a *new* conflict — is theirs by construction and is not here.
//! * `external_tools.rs` — `mergetool`'s use of `rerere remaining` to decide
//!   which paths it still has to open.
//! * `am_deep.rs` — `am --3way --rerere-autoupdate` / `--no-rerere-autoupdate`
//!   with rerere **off**, which measures the flag's parse and nothing else.
//! * `integrity_gc.rs` — `git gc` itself.
//!
//! None of them asks the question rerere actually turns on: **is the conflict
//! id the same on both sides?** The id is the name of a directory under
//! `.git/rr-cache`, `probe_rr_cache` (`runner.rs`) walks that tree and compares
//! every path and every byte, and `probe_op_state` compares `.git/MERGE_RR`,
//! which maps path to id. So one invocation that records a preimage pins the id
//! exactly — and an id that differs by one byte makes every resolution one side
//! recorded invisible to the other, with byte-identical stdout on both. That is
//! what this module is for, and it is why most of the cases here are `merge`,
//! `rebase`, `cherry-pick`, `revert` and `am` rather than `rerere`.
//!
//! # The normalisation contract, measured by hand on stock 2.55.0
//!
//! The id is the hash of the conflict *hunks* after normalisation. What that
//! does and does not fold together, verified in a scratch repository rather
//! than recalled:
//!
//! * **Conflict style is folded.** `merge`, `diff3` and `zdiff3` write three
//!   different files into the worktree and record the **same** preimage bytes
//!   and the same id (`0630df85…` for [`Shape::CrissCross`]'s `clash.txt` under
//!   all three). The `|||||||` base section and the `HEAD` / branch-name labels
//!   are dropped before hashing.
//! * **Side order is folded.** Merging `cc-right` from `cc-left` and `cc-left`
//!   from `cc-right` both record `<<<<<<<\na\n=======\nb\n>>>>>>>` — the two
//!   sides are sorted, so `a` is first either way, and the id is the same.
//! * **The operation is folded.** `merge cc-right`, `rebase cc-b` and
//!   `cherry-pick cc-b` all conflict `a` against `b` and all record
//!   `0630df85…`. `revert cc-b` conflicts `a` against the *base*, which is a
//!   different pair, and records `2b93f59d…`.
//! * **Surrounding context is not hashed, but is stored.** Two paths whose
//!   conflict blocks are identical and whose trailing context differs get the
//!   same id and different preimages; git tells them apart with the `.1` / `.2`
//!   *variant* suffix in `MERGE_RR`. No shape in the corpus has two paths with
//!   the same conflict block, so the variant mechanism is not reachable from a
//!   `Case` — see "not measurable" below.
//! * **Whitespace is not folded.** ` M ` against ` S ` hashes differently from
//!   `M` against `S`. Also unreachable: no shape offers the pair.
//!
//! # What is not measurable here, and why
//!
//! * **The replay.** Two invocations minimum. `sequences.rs` owns it.
//! * **`rerere.autoUpdate`'s effect.** It only acts when a resolution is
//!   *replayed*, so it needs the same two invocations. The cases below pin the
//!   flag and the key on the recording path, where both are inert on stock —
//!   which is itself the assertion, since a port that stages on record diverges.
//! * **The `.1` / `.2` variant suffix**, and **whitespace / marker-size
//!   normalisation**. All three need a fixture with two conflicting paths whose
//!   blocks collide, or with a `conflict-marker-size` attribute. A `Case`
//!   cannot write a file into the fixture and this module may not add a
//!   `Shape`, and no existing shape carries either. Verified by hand instead
//!   (both binaries assign `<id>`, `<id>.1`, `<id>.2` in path order and agree);
//!   recorded here rather than left unstated.
//! * **`stash apply` recording.** [`Shape::Stashed`] is dirty on exactly the
//!   paths its entries touch, so every `stash apply`/`pop`/`branch` there is
//!   refused by the "local changes would be overwritten" check *before* a merge
//!   happens — measured on stock 2.55.0 for `stash@{0}`, `stash@{1}` and
//!   `stash@{2}`. No conflict, so nothing to record. It needs a commit first,
//!   which is a second invocation.
//! * **`rerere gc` at any non-extreme age.** `gc.rerereResolved` and
//!   `gc.rerereUnresolved` are day counts measured against the wall clock, and
//!   nothing can age a file in the fixture. Only the two ends are reachable: at
//!   `0` every record is past its cutoff, at the default (60/15) none is. Both
//!   are used below. The `0` end carries the same sub-second race
//!   `sequences.rs` already accepts — a record written in the same second the
//!   `gc` runs has `st_mtime == now` and survives — and the fixture template is
//!   built before the run, so it does not fire in practice. Stated rather than
//!   hidden.
//! * **`contrib/rerere-train.sh`.** Shipped as a loose script under
//!   `share/git-core/contrib/`, not installed as `git-rerere-train`, so the
//!   binary under test cannot dispatch it and there is nothing differential to
//!   compare.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

pub fn cases(out: &mut Vec<Case>) {
    conflict_id_from_a_merge(out);
    conflict_id_without_a_base(out);
    record_from_other_operations(out);
    record_from_am_three_way(out);
    enabled_over_a_populated_cache(out);
    verb_argument_surface(out);
    forget_pathspecs(out);
    from_a_subdirectory(out);
    gc_by_age(out);
}

/// `-c rerere.enabled=true`, the setting every recording case needs.
const ON: &[(&str, &str)] = &[("rerere.enabled", "true")];

/// Push one case per argv against `shape`, under `config`.
fn each_on(shape: Shape, cmd: &'static str, argvs: &[&[&str]], config: &[(&str, &str)], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape).with_config(config));
    }
}

// ---------------------------------------------------------------------------
// The conflict id, recorded by a merge over a virtual base
// ---------------------------------------------------------------------------

/// [`Shape::CrissCross`] merged with rerere on: one conflicting path, and a
/// merge base that is itself a merge.
///
/// The preimage is the *outer* conflict — `a` against `b`, markers stripped of
/// their labels — and the directory holding it is named by its hash, so these
/// cases pin the id and the bytes at the same time. Measured on stock 2.55.0:
/// `.git/rr-cache/0630df854874fc5ffb92a197732cce0d8928e898/preimage` and a
/// `MERGE_RR` line naming the same id for `clash.txt`.
///
/// The three conflict styles are here because they are the obvious thing to get
/// wrong: they write three visibly different files into the worktree, and all
/// three must record one preimage and one id. A port that hashed what it wrote
/// instead of the normalised hunk passes the `merge` case and fails the other
/// two, and no stdout anywhere would say so. `merge_strategies.rs` already runs
/// the same three styles over this shape *without* rerere, which measures the
/// worktree bytes; these measure the cache, and an unparseable style is that
/// module's question (`merge.conflictStyle=nonsense`) and is deliberately not
/// repeated here.
///
/// `-X ours` is the negative control: it resolves the conflict, so there is
/// nothing to record and `rr-cache` must stay absent. Without it, a port that
/// records unconditionally would score full marks on every case above.
fn conflict_id_from_a_merge(out: &mut Vec<Case>) {
    each_on(
        Shape::CrissCross,
        "merge",
        &[
            &["-c", "merge.conflictStyle=merge", "merge", "cc-right"],
            &["-c", "merge.conflictStyle=diff3", "merge", "cc-right"],
            &["-c", "merge.conflictStyle=zdiff3", "merge", "cc-right"],
            &["merge", "--no-commit", "cc-right"],
            &["merge", "--squash", "cc-right"],
            &["merge", "--no-ff", "cc-right"],
            &["merge", "-s", "recursive", "cc-right"],
            // Resolves; nothing recorded.
            &["merge", "-X", "ours", "cc-right"],
            // `autoUpdate` is inert on the recording path — it acts on replay —
            // so all three of these must leave the identical repository. A port
            // that stages the conflicted path on record diverges on the index.
            &["merge", "--rerere-autoupdate", "cc-right"],
            &["merge", "--no-rerere-autoupdate", "cc-right"],
            &["-c", "rerere.autoUpdate=true", "merge", "cc-right"],
        ],
        ON,
        out,
    );

    // The same merge with `rerere.enabled` delivered from `.git/config` rather
    // than from `-c`. Every other recording case here uses the command line,
    // which is the one scope a port can honour by special-casing `-c`;
    // [`Shape::Rerere`] is the only fixture whose repository config carries the
    // key, and no case could put it there for a *different* shape until the
    // scoped-config field existed.
    out.push(
        Case::new("merge", &["merge", "cc-right"], Shape::CrissCross).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Repo, "rerere.enabled", "true"),
        ]),
    );
}

// ---------------------------------------------------------------------------
// The conflict id with no merge base at all
// ---------------------------------------------------------------------------

/// An add/add conflict between two roots ([`Shape::Unrelated`]).
///
/// Every other recording case in the corpus has a base — a real one on
/// [`Shape::Conflicted`], a virtual one on [`Shape::CrissCross`]. Here there is
/// none: `README.md` exists on both roots and shares no history, so the
/// three-way merge runs against the empty blob. Measured on stock 2.55.0 the
/// recorded preimage is
/// `<<<<<<<\n# alien fixture\n\nsame path, no common ancestor\n=======\n# fixture\n>>>>>>>`
/// under `f0c0d5401b007d5f9544df24c27b9efd08998dfa` — the alien side first,
/// which is the sort, not the merge order.
///
/// `merge alien-clash` without the flag is the control: git refuses unrelated
/// histories outright, so no merge and no record.
fn conflict_id_without_a_base(out: &mut Vec<Case>) {
    each_on(
        Shape::Unrelated,
        "merge",
        &[
            &["merge", "--allow-unrelated-histories", "alien-clash"],
            &["-c", "merge.conflictStyle=diff3", "merge", "--allow-unrelated-histories", "alien-clash"],
            &["merge", "alien-clash"],
        ],
        ON,
        out,
    );
}

// ---------------------------------------------------------------------------
// The operations other than `merge` that record
// ---------------------------------------------------------------------------

/// `rebase`, `cherry-pick` and `revert` over the same criss-cross conflict.
///
/// rerere is not a merge feature: `sequencer.c` calls it on every pick that
/// stops, so a rebase, a cherry-pick and a revert each record a preimage. None
/// of the three had ever been run with rerere on anywhere in the corpus, and
/// their stdout is *identical* with rerere on and off — the entire difference
/// is `rr-cache` and `MERGE_RR`, which is exactly what `probe_rr_cache` and
/// `probe_op_state` read.
///
/// Measured on stock 2.55.0 over [`Shape::CrissCross`]:
///
/// * `rebase cc-b` stops on `criss-cross: a` and records
///   `0630df854874fc5ffb92a197732cce0d8928e898` — the same id `merge cc-right`
///   records, because the normalised conflict is the same `a`/`b` pair.
/// * `cherry-pick cc-b` records the same id again.
/// * `revert cc-b` records `2b93f59dd46c35259ae20e9ed2692d23d1a16215`: reverting
///   `b` conflicts `a` against `base`, which is a different pair.
///
/// Three ids across four operations, two of them equal and one deliberately
/// not, is the sharpest statement of the identity contract a single invocation
/// can make.
fn record_from_other_operations(out: &mut Vec<Case>) {
    each_on(
        Shape::CrissCross,
        "rebase",
        &[
            &["rebase", "cc-b"],
            &["rebase", "--rerere-autoupdate", "cc-b"],
            &["rebase", "--merge", "cc-b"],
        ],
        ON,
        out,
    );

    each_on(
        Shape::CrissCross,
        "cherry-pick",
        &[
            &["cherry-pick", "cc-b"],
            &["cherry-pick", "--rerere-autoupdate", "cc-b"],
            &["cherry-pick", "-n", "cc-b"],
        ],
        ON,
        out,
    );

    each_on(
        Shape::CrissCross,
        "revert",
        &[&["revert", "cc-b"], &["revert", "--no-rerere-autoupdate", "cc-b"]],
        ON,
        out,
    );
}

/// A patch whose three-way fallback conflicts, applied with rerere on.
///
/// `am_deep.rs` owns `am` and already sends `--rerere-autoupdate` /
/// `--no-rerere-autoupdate` through [`Shape::Branched`] — but with rerere
/// *disabled*, which its own header says, so those cases measure the option
/// parser and stop there. Turning rerere on is what makes `am`'s call into the
/// recording path reachable, and `am` reaches it through `builtin/am.c` rather
/// than through the sequencer, so it is a third code path and not a repeat of
/// the two above.
///
/// The payload is this module's own rather than a second reference to
/// `am_deep`'s: it names blob `89cc62e` where that one names `b8c1e94`, so the
/// two conflicts — and therefore the two ids — are distinguishable in a report.
/// Measured on stock 2.55.0: exit 128, a preimage of
/// `<<<<<<<\npub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\n=======\npub fn one() -> u32 { 7 }\npub fn seven() -> u32 { 7 }\n>>>>>>>`
/// under `c86b934e38810404489728340b8927a8960f8993`, and a `MERGE_RR` naming it
/// for `src/lib.rs`.
const AM_3WAY: &[u8] = b"From bbbb0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] rerere-engine: rewrite the only line and add another

Body.
---
diff --git a/src/lib.rs b/src/lib.rs
index 46e89a2..89cc62e 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1,2 @@
-pub fn one() -> u32 { 1 }
+pub fn one() -> u32 { 7 }
+pub fn seven() -> u32 { 7 }
";

fn record_from_am_three_way(out: &mut Vec<Case>) {
    for args in [
        &["am", "--3way"][..],
        &["am", "--3way", "--rerere-autoupdate"][..],
        &["am", "--3way", "--no-rerere-autoupdate"][..],
    ] {
        out.push(
            Case::with_stdin("am", args, Shape::Branched, AM_3WAY).with_config(ON),
        );
    }
}

// ---------------------------------------------------------------------------
// `rerere.enabled` against a repository that already has a cache
// ---------------------------------------------------------------------------

/// Turning rerere **off** where the fixture turned it on.
///
/// [`Shape::Rerere`] carries `rerere.enabled=true` in `.git/config`, so every
/// existing case over that shape measures the enabled path. `-c` outranks the
/// repository file, which makes the disabled path reachable over a populated
/// cache for the first time — and that is the interesting direction: with a
/// cache present but the key false, stock must not record, must not replay, and
/// must leave `rr-cache` and `MERGE_RR` exactly as it found them, while
/// `status`, `remaining` and `diff` fall silent.
///
/// `bogus` is the parse question. Stock reads the key with the boolean parser
/// and dies — `fatal: bad boolean config value 'bogus' for 'rerere.enabled'`,
/// exit 128, nothing written — where a port that treats "not false" as true
/// records a preimage at exit 0. `0` is the other spelling of false, which git
/// accepts and which a hand-rolled `== "false"` comparison does not.
fn enabled_over_a_populated_cache(out: &mut Vec<Case>) {
    let off: &[(&str, &str)] = &[("rerere.enabled", "false")];
    each_on(
        Shape::Rerere,
        "rerere",
        &[
            &["rerere"],
            &["rerere", "status"],
            &["rerere", "remaining"],
            &["rerere", "diff"],
            &["rerere", "forget", "fresh.txt"],
        ],
        off,
        out,
    );

    each_on(Shape::Rerere, "rerere", &[&["rerere"]], &[("rerere.enabled", "bogus")], out);
    each_on(Shape::Rerere, "rerere", &[&["rerere"]], &[("rerere.enabled", "0")], out);
}

// ---------------------------------------------------------------------------
// The verbs' own argument surface
// ---------------------------------------------------------------------------

/// What each subcommand does with arguments it was not given a use for.
///
/// `merge_family.rs` asks one question here (`rerere bogus` — an unknown verb)
/// and `fixture_gaps2.rs` asks none: every one of its cases is a well-formed
/// invocation. The shapes of failure below are all different from an unknown
/// verb — a verb with a trailing operand it ignores, a verb with an operand it
/// rejects, `forget` with the pathspec it requires missing, and `--` in front
/// of a word that is otherwise a verb name — and each is a place a
/// hand-written parser and git's `parse_options` diverge without either one
/// crashing.
fn verb_argument_surface(out: &mut Vec<Case>) {
    each_on(
        Shape::Rerere,
        "rerere",
        &[
            // `forget` is the one verb with a required operand.
            &["rerere", "forget"],
            // Trailing operands the reporting verbs have no use for.
            &["rerere", "status", "extra"],
            &["rerere", "remaining", "extra"],
            &["rerere", "remaining", "fresh.txt"],
            &["rerere", "clear", "extra"],
            &["rerere", "gc", "extra"],
            &["rerere", "diff", "fresh.txt"],
            &["rerere", "diff", "nosuch.txt"],
            // An option no verb defines, and `--` where a verb is expected.
            &["rerere", "--dry-run"],
            &["rerere", "--", "status"],
        ],
        &[],
        out,
    );
}

// ---------------------------------------------------------------------------
// `rerere forget`'s pathspec
// ---------------------------------------------------------------------------

/// `forget` takes a **pathspec**, not a path, and that is the whole group.
///
/// `fixture_gaps2.rs` names literal paths and `.`; `sequences.rs` names literal
/// paths. Nothing anywhere asks whether the argument goes through git's
/// pathspec machinery at all — wildcards, the `:(magic)` prefixes, the `:!`
/// exclusion shorthand, the `:/` root anchor, more than one spec at once, and a
/// spec that escapes the worktree.
///
/// Each case is scored on the cache it leaves behind: a `forget` that matched
/// removes the named entries' `postimage` and re-registers their paths in
/// `MERGE_RR`, and one that matched nothing removes nothing. Both exit 0 and
/// print nothing to stdout, so `probe_rr_cache` and `probe_op_state` are the
/// only witnesses.
///
/// Measured on stock 2.55.0 over [`Shape::Rerere`]: `*.txt`, `:(glob)*.txt`,
/// `:!fresh.txt` and `:(exclude)fresh.txt` each drop the `postimage` under
/// `3e35882836…` (`rr.txt`) and `b0831ed48d…` (`other.txt`) and leave
/// `a5be8d0ebc…` (`fresh.txt`, preimage only) alone; `../outside` is refused
/// with exit 128.
fn forget_pathspecs(out: &mut Vec<Case>) {
    each_on(
        Shape::Rerere,
        "rerere",
        &[
            &["rerere", "forget", "*.txt"],
            &["rerere", "forget", ":(glob)*.txt"],
            &["rerere", "forget", ":!fresh.txt"],
            &["rerere", "forget", ":(exclude)fresh.txt"],
            &["rerere", "forget", ":(icase)FRESH.TXT"],
            &["rerere", "forget", ":(literal)fresh.txt"],
            &["rerere", "forget", ":/rr.txt"],
            &["rerere", "forget", "rr.txt", "other.txt"],
            &["rerere", "forget", "nosuch.txt"],
            &["rerere", "forget", "../outside"],
        ],
        &[],
        out,
    );
}

// ---------------------------------------------------------------------------
// The same verbs, run from a subdirectory
// ---------------------------------------------------------------------------

/// `rerere` from `src/`, which every existing case runs from the worktree root.
///
/// Two different questions live here and both are invisible from the root.
/// `status`, `remaining` and `diff` name paths, and the answer is
/// **root-relative** — stock 2.55.0 prints `fresh.txt` and diffs `a/fresh.txt`
/// from inside `src/`, not `../fresh.txt` — so an implementation that renders
/// against the current directory diverges. `forget` takes a pathspec, and that
/// one *is* resolved against the current directory, so `../rr.txt` is the
/// spelling that reaches the same record `rr.txt` reaches from the root, while
/// `:/rr.txt` reaches it regardless of where it is run.
fn from_a_subdirectory(out: &mut Vec<Case>) {
    for args in [
        &["rerere"][..],
        &["rerere", "status"][..],
        &["rerere", "remaining"][..],
        &["rerere", "diff"][..],
        &["rerere", "forget", "../rr.txt"][..],
        &["rerere", "forget", ":/rr.txt"][..],
    ] {
        out.push(Case::new("rerere", args, Shape::Rerere).in_dir("src"));
    }
}

// ---------------------------------------------------------------------------
// `rerere gc`, at the only two ages a fixture can reach
// ---------------------------------------------------------------------------

/// The two expiry keys, separately.
///
/// `sequences.rs` sets both to `0` at once and then re-creates the merge to
/// prove what was deleted. Setting them *separately* is what tells the two
/// halves of the cache apart, and it is a single invocation because
/// `probe_rr_cache` reads the tree directly:
///
/// * `gc.rerereResolved=0` expires the entries that have a `postimage` —
///   measured on stock 2.55.0, `3e35882836…` (`rr.txt`) and `b0831ed48d…`
///   (`other.txt`) go entirely and `a5be8d0ebc…` (`fresh.txt`) stays.
/// * `gc.rerereUnresolved=0` expires the other half — `a5be8d0ebc…` goes and
///   the two resolved entries stay.
///
/// A port that honours one key for both classes passes `sequences.rs`'s case
/// (where both are `0`, so the two behaviours coincide) and fails here.
///
/// `bogus` and the negative pair are the parse edge: stock accepts both at exit
/// 0, `bogus` falling back to the built-in defaults and expiring nothing, and a
/// negative day count expiring everything. Neither reads a clock in a way the
/// harness can see, because both ends are past every record's age.
fn gc_by_age(out: &mut Vec<Case>) {
    for cfg in [
        &[("gc.rerereResolved", "0")][..],
        &[("gc.rerereUnresolved", "0")][..],
        &[("gc.rerereResolved", "bogus")][..],
        &[("gc.rerereResolved", "-1"), ("gc.rerereUnresolved", "-1")][..],
    ] {
        out.push(Case::new("rerere", &["rerere", "gc"], Shape::Rerere).with_config(cfg));
    }
}
