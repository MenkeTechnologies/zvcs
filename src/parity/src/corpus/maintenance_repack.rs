//! Differential corpus cases for the verbs that **rewrite the object store in
//! place**: `gc`, `repack`, `prune`, `prune-packed`, `fsck`, `maintenance`,
//! `pack-refs` and `reflog expire`.
//!
//! Every case here is compared against stock git for stdout, exit code and
//! post-command repository state.
//!
//! # How this divides from `corpus/maintenance.rs`
//!
//! `maintenance.rs` owns the same verbs and says so; it is the *breadth* pass —
//! one or two invocations per flag, almost entirely over `Linear`/`Branched`/
//! `Merged`/`Detached`, which are five-to-nine-object repositories with a single
//! loose object store and nothing unreachable in them. On those shapes `gc` and
//! `repack` have almost nothing to decide: there is no second pack to keep or
//! drop, no loose duplicate of a packed object to remove, no unreachable object
//! to expire, and no damage to refuse over. A port that packs everything
//! unconditionally and prunes nothing scores full marks there.
//!
//! This module is the *depth* pass over the shapes where those decisions are
//! real, and it does not repeat an argv/shape pair `maintenance.rs`,
//! `object_pack.rs`, `shape_reach.rs` or `fixture_gaps.rs` already carries:
//!
//!   * [`Shape::Packed`] — two packs, eight loose objects of which five are
//!     loose duplicates of packed ones and three are unreachable, and an
//!     expired reflog. Every `gc`/`repack`/`prune` flag that selects *which*
//!     objects move has something to select here, and the loose count alone
//!     separates the four outcomes (8 untouched, 3 dedup'd, 5 kept-unreachable,
//!     0 fully pruned).
//!   * [`Shape::Damaged`] — the refusal surface. Stock declines to `gc`,
//!     `repack -a -d` or `prune` over `refs/heads/dangling` and leaves the store
//!     exactly as it found it; a port that proceeds *deletes objects stock
//!     keeps*, which is the failure class this module exists to find.
//!   * [`Shape::Stashed`] and [`Shape::Worktree`] — extra reachability roots
//!     (`refs/stash` and a linked worktree's `HEAD`). An object that is only
//!     reachable through one of them is what a naive `for-each-ref` walk drops.
//!   * [`Shape::Unrelated`] and [`Shape::CommitGraph`] — three roots, so
//!     reachability is not one walk; and a graph file that a repack has to
//!     rewrite or invalidate.
//!
//! # What the probes can and cannot see here
//!
//! `runner::probe_state` runs `cat-file --batch-check --batch-all-objects` after
//! every case, so **object survival is fully pinned** — that listing is the
//! primary instrument below and it is what proves a case did or did not destroy
//! something. `runner::probe_reflogs` compares every file under `.git/logs`
//! byte for byte, which is what makes `reflog expire` measurable at all.
//!
//! `runner::probe_storage` enumerates `objects/pack` and `objects/info` from the
//! directory rather than from a whitelist and elides hash runs
//! (`runner::stable_entry_name`), so the listing does see:
//!
//!   * `pack-<hash>.pack` / `.idx` / `.rev` / `.mtimes`, one line each, duplicates
//!     kept — so "one pack or three" and "cruft pack written or not" are exact;
//!   * `multi-pack-index`, which has no extension, and `pack-<hash>.bitmap`;
//!   * `loose-<hash>.pack`, the distinct prefix `maintenance run
//!     --task=loose-objects` writes, and `expired-<hash>.pack` from
//!     `repack --expire-to=`;
//!   * a **half-written pack left behind by a failed run** —
//!     `.tmp-<pid>-pack-<hash>.pack` and `tmp_pack_<suffix>` both survive
//!     elision as stable strings.
//!
//! What it cannot see, stated rather than left to be discovered:
//!
//!   * **Pack bytes and pack names.** A pack's filename embeds its own
//!     checksum, so no case may name one in argv and no comparison may be made
//!     on the name. This is why `--keep-pack=` below is reachable only through a
//!     name that matches nothing.
//!   * **`.git/lost-found/`.** `fsck --lost-found` writes
//!     `.git/lost-found/commit/<oid>`, and no probe reads that directory. Those
//!     cases are pinned on stdout and exit code alone.
//!
//! A linked worktree's own reflog is *not* on that list, and the distinction is
//! worth stating because it is easy to get backwards: `probe_reflogs` walks
//! `.git/logs` and stops, so it never sees `.git/worktrees/wt/logs/HEAD` — but
//! `runner::probe_worktrees` reports every file under `.git/worktrees/<name>/`
//! by content, that log included. The `reflog expire` cases on
//! [`Shape::Worktree`] below are therefore fully pinned on both logs, and the
//! run confirmed it: the port's divergence there was reported as
//! `wt/logs/HEAD` holding two lines where stock left none.
//!
//! # Determinism constraints these cases are written around
//!
//!   * **`--schedule=` detaches.** `git maintenance run --schedule=daily`
//!     returns while a background child is still rewriting the store; measured
//!     three times over [`Shape::Packed`], the object listing came back mid-write
//!     once. Every `--schedule=` case below therefore carries `--no-detach`, and
//!     so does every `maintenance run` that reaches real work.
//!   * **`gc --auto` detaches** unless `gc.autoDetach=false`, for the same
//!     reason. `git help config`: "gc.autoDetach — Make git gc --auto return
//!     immediately and run in the background if the system supports it. Default
//!     is true." Every `--auto` case below that is configured *over* the
//!     threshold sets it to false.
//!   * **Dates are pinned by the fixture, not by the clock.** `env::harden` sets
//!     `GIT_COMMITTER_DATE` to `1700000000` and the fixtures' loose objects are
//!     written at build time, so `--expire=now`/`all` always expires and
//!     `--expire=never`, `2.weeks.ago` and an ISO date in 2005 never do. Both
//!     directions are stable for as long as wall-clock time moves forward.
//!
//! # Scheduler safety
//!
//! `maintenance start`, `stop`, `register` and `unregister` write **outside the
//! fixture**: a launchd plist, a crontab entry, or the user's global config.
//! `env::harden` points `GIT_CONFIG_GLOBAL` at `/dev/null`, which contains the
//! config half, but a scheduler entry escapes the fixture entirely and no
//! hardening in this crate can hold it.
//!
//! So `start` and `stop` are reached **only on their option-parse refusals**,
//! which happen in `parse_options()` before `maintenance_start()` runs anything
//! (`builtin/gc.c`, `builtin_maintenance_start_options`). Each was verified by
//! hand against stock 2.55.0 with `~/Library/LaunchAgents`, `crontab -l` and
//! `~/.gitconfig` fingerprinted before and after: all six refusals exit 129 and
//! all three fingerprints were unchanged. `maintenance stop` has no refusal that
//! is reachable without a value that could succeed, so it is absent entirely —
//! as is any `start` invocation with a *valid* `--scheduler=`.
//!
//! `register`/`unregister` on their success paths are already covered by
//! `maintenance.rs`; only their parse refusals are added here.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    gc_selection(out);
    gc_config(out);
    gc_roots(out);
    repack_selection(out);
    repack_config(out);
    prune_family(out);
    fsck_damage(out);
    maintenance_tasks(out);
    scheduler_refusals(out);
    pack_refs(out);
    reflog_expire(out);
}

// ---------------------------------------------------------------------------
// gc
// ---------------------------------------------------------------------------

/// Which objects `gc` moves, over the one shape where it has a choice.
///
/// [`Shape::Packed`] holds 34 objects: 26 packed across two packs, five loose
/// duplicates of packed objects, and three loose objects (a commit, its tree and
/// its blob) that no ref reaches and no reflog mentions. Measured against stock
/// 2.55.0, that makes each of these produce a *different* pair of (loose count,
/// pack listing):
///
/// | invocation               | objects | loose | packs afterwards          |
/// |--------------------------|---------|-------|---------------------------|
/// | `gc --quiet`             | 34      | 0     | two, one of them cruft    |
/// | `gc --prune=now`         | 31      | 0     | one                       |
/// | `gc --no-cruft --prune=now` | 31   | 0     | one, no `.mtimes`         |
/// | `gc --keep-largest-pack` | 34      | 0     | three                     |
/// | `gc --auto`              | 34      | 8     | two, unchanged            |
///
/// So a port that packs unconditionally, one that never prunes, and one that
/// prunes everything are three distinguishable answers here and are one answer
/// on `Linear`, which is where `maintenance.rs` measures them. The `--cruft`
/// axis is the `.mtimes` line in the storage listing: `builtin/gc.c` passes
/// `--cruft` down to `builtin/repack.c`, which writes the mtimes sidecar beside
/// the cruft pack, and a port that accepts the flag and writes no sidecar is
/// caught by that one line.
///
/// Every `--prune=` form git's approxidate parser accepts is here except the
/// aliases that measure the same: `now` and `all` both expire everything, and a
/// relative date two weeks back and an absolute date in 2005 are both older than
/// the fixture's own build time and so expire nothing. One of each pair is kept.
fn gc_selection(out: &mut Vec<Case>) {
    for args in [
        &["gc", "--quiet"][..],
        &["gc", "--aggressive", "--quiet"],
        &["gc", "--no-prune", "--quiet"],
        &["gc", "--keep-largest-pack", "--quiet"],
        &["gc", "--keep-largest-pack", "--prune=now", "--quiet"],
        &["gc", "--cruft", "--prune=now", "--quiet"],
        &["gc", "--no-cruft", "--prune=now", "--quiet"],
        &["gc", "--auto", "--quiet"],
        &["gc", "--prune=now", "--quiet"],
        &["gc", "--prune=never", "--quiet"],
        &["gc", "--prune=2.weeks.ago", "--quiet"],
    ] {
        out.push(Case::new("gc", args, Shape::Packed));
    }
    // Refusal: an unparsable expiry must abort before anything is rewritten.
    // Stock exits 128 with `fatal: failed to parse prune expiry value bogus` and
    // leaves all eight loose objects in place; a port that falls back to a
    // default and proceeds destroys three of them.
    out.push(Case::strict("gc", &["gc", "--prune=bogus", "--quiet"], Shape::Packed));
}

/// The configuration `gc` reads instead of, or alongside, its flags.
///
/// `gc.cruftPacks` and `gc.pruneExpire` are the two that decide whether an
/// unreachable object survives, and both are observable in the object listing
/// rather than in stdout: with `gc.cruftPacks=false` stock writes one pack and
/// leaves three loose objects behind; with it true it writes a cruft pack and a
/// `.mtimes` sidecar and leaves none. `gc.bigPackThreshold=1` makes every
/// existing pack "big", so stock keeps both and adds a third.
///
/// The `--auto` group is the threshold arithmetic in `too_many_packs()`
/// (`builtin/gc.c`): with `gc.auto=1` alone the repository is still under the
/// pack limit and stock declines, and adding `gc.autoPackLimit=1` is what tips
/// it over. `gc.autoDetach=false` is mandatory on any `--auto` case expected to
/// *do* something — see the module doc. The last one is `--auto` over a
/// repository stock refuses to touch at all: the threshold check runs first, so
/// it must exit 0 having rewritten nothing.
fn gc_config(out: &mut Vec<Case>) {
    for cfg in [
        &[("gc.pruneExpire", "now")][..],
        &[("gc.pruneExpire", "never")],
        &[("gc.cruftPacks", "false")],
        &[("gc.cruftPacks", "true")],
        &[("gc.bigPackThreshold", "1")],
        &[("gc.maxCruftSize", "1k")],
    ] {
        out.push(Case::new("gc", &["gc", "--quiet"], Shape::Packed).with_config(cfg));
    }
    out.push(
        Case::new("gc", &["gc", "--cruft", "--quiet"], Shape::Packed)
            .with_config(&[("gc.cruftWindow", "1")]),
    );
    out.push(
        Case::new("gc", &["gc", "--auto", "--quiet"], Shape::Packed)
            .with_config(&[("gc.auto", "1"), ("gc.autoDetach", "false")]),
    );
    out.push(
        Case::new("gc", &["gc", "--auto", "--quiet"], Shape::Packed).with_config(&[
            ("gc.auto", "1"),
            ("gc.autoPackLimit", "1"),
            ("gc.autoDetach", "false"),
        ]),
    );
    out.push(
        Case::new("gc", &["gc", "--auto", "--prune=now", "--quiet"], Shape::Packed).with_config(&[
            ("gc.auto", "1"),
            ("gc.autoPackLimit", "1"),
            ("gc.autoDetach", "false"),
        ]),
    );
    out.push(
        Case::new("gc", &["gc", "--auto", "--quiet"], Shape::Damaged)
            .with_config(&[("gc.auto", "1"), ("gc.autoDetach", "false")]),
    );
}

/// `gc` where reachability is not one walk off `refs/heads/*`.
///
/// Four shapes, four different reasons an object survives: `refs/stash` and its
/// three entries' untracked-file commits ([`Shape::Stashed`]); a linked
/// worktree's `HEAD` ([`Shape::Worktree`]); three disjoint roots
/// ([`Shape::Unrelated`]); and a commit made after the commit-graph was written
/// ([`Shape::CommitGraph`]). A port whose `gc` walks only the main worktree's
/// refs loses objects on the first two, and the object listing names exactly
/// which.
///
/// The [`Shape::Damaged`] pair is the refusal that protects data. Stock reads
/// `refs/heads/dangling`, finds no object behind it, and exits 128 with
/// `fatal: bad object refs/heads/dangling` **before** `builtin/gc.c` reaches
/// `repack` or `prune` — nine loose objects in, nine out, no pack written. A
/// port that treats an unreadable ref as "not a root" and carries on prunes the
/// corrupt object and everything else it could not reach. `strict` because the
/// whole refusal is on stderr and there is nothing else to compare.
fn gc_roots(out: &mut Vec<Case>) {
    for &shape in &[Shape::Stashed, Shape::Worktree, Shape::Unrelated, Shape::CommitGraph] {
        out.push(Case::new("gc", &["gc", "--prune=now", "--quiet"], shape));
    }
    out.push(Case::new("gc", &["gc", "--cruft", "--prune=now", "--quiet"], Shape::Stashed));
    out.push(Case::new("gc", &["gc", "--aggressive", "--prune=now", "--quiet"], Shape::Worktree));
    out.push(
        Case::new("gc", &["gc", "--prune=now", "--quiet"], Shape::Worktree)
            .with_config(&[("gc.worktreePruneExpire", "now")]),
    );
    out.push(
        Case::new("gc", &["gc", "--quiet"], Shape::CommitGraph)
            .with_config(&[("core.commitGraph", "false")]),
    );
    out.push(Case::strict("gc", &["gc", "--quiet"], Shape::Damaged));
    out.push(Case::strict("gc", &["gc", "--prune=now", "--quiet"], Shape::Damaged));
}

// ---------------------------------------------------------------------------
// repack
// ---------------------------------------------------------------------------

/// `repack`'s selection flags, over a repository that already has two packs and
/// eight loose objects.
///
/// The pack *bytes* legitimately differ between the two sides, so nothing below
/// asserts on them. What it asserts on is the pair (how many packs exist
/// afterwards, which objects still exist), and over [`Shape::Packed`] that pair
/// separates the decisions `builtin/repack.c` makes:
///
///   * `-a -d` collapses to one pack and runs `prune-packed`, leaving three
///     loose objects — the unreachable ones, which `-a` folded into the pack and
///     `prune-packed` therefore did not remove.
///   * `-d` without `-a` leaves both packs, because neither is redundant.
///   * `-n` skips `update-server-info` and touches nothing else, so all eight
///     loose objects stay: the case a port that ignores `-n` fails.
///   * `-l` is the alternates half of the same decision — `--local` reaches
///     `pack-objects` and changes which objects are candidates.
///   * `--cruft` writes the `.mtimes` sidecar the storage listing counts, and
///     `--cruft-expiration=now` is the same flag deciding the other way (loose
///     3 rather than 0).
///   * `--write-midx` writes `objects/pack/multi-pack-index` and
///     `--write-bitmap-index` writes `pack-<hash>.bitmap`; both are enumerated
///     by `probe_storage` and neither is inferable from the pack count.
///   * `--geometric=2` keeps a progression — three packs where `-a -d` ends with
///     one — while `--geometric=0` degenerates to two.
///
/// The pack-tuning flags (`-f`, `--window=`/`--depth=`, `--path-walk`) all land
/// on the same one-pack/three-loose outcome and differ only in bytes, so a
/// representative few are kept rather than the whole set: they measure argument
/// plumbing, and one case per plumbing path is enough for that.
fn repack_selection(out: &mut Vec<Case>) {
    for args in [
        &["repack", "-a", "-d", "-f", "-q"][..],
        &["repack", "-n", "-q"],
        &["repack", "-l", "-d", "-q"],
        &["repack", "-a", "-d", "-l", "-q"],
        &["repack", "-a", "-d", "--window=1", "--depth=1", "-q"],
        &["repack", "-a", "-d", "--path-walk", "-q"],
        &["repack", "-a", "-d", "--keep-unreachable", "-q"],
        &["repack", "-a", "-d", "--unpack-unreachable=now", "-q"],
        &["repack", "-a", "-d", "--write-bitmap-index", "-q"],
        &["repack", "-a", "-d", "--no-write-bitmap-index", "-q"],
        &["repack", "-a", "-d", "--write-midx", "-q"],
        &["repack", "--write-midx"],
        &["repack", "--geometric=2", "-d", "-q"],
        &["repack", "--geometric=0", "-d", "-q"],
        &["repack", "--geometric=2", "--write-midx", "-d", "-q"],
        &["repack", "--cruft", "-a", "-d", "-q"],
        &["repack", "--cruft", "--cruft-expiration=now", "-d", "-q"],
        // `--keep-pack=` names a pack file, and a pack's name embeds its own
        // checksum — see the module doc. A name that matches nothing is the only
        // form a case can spell, and it is still worth pinning: stock accepts it
        // silently and repacks everything, so a port that errors on an unknown
        // pack name diverges on the exit code.
        &["repack", "-a", "-d", "--keep-pack=nope", "-q"],
        // `--expire-to=` writes its pack under a *prefix*, and the resulting file
        // name embeds a checksum too. Directing it inside `.git/objects/pack` is
        // what keeps it out of `status --porcelain -uall`, which is compared byte
        // for byte and would otherwise carry a checksum-bearing untracked
        // filename. `probe_storage` elides the hash, so the line it produces is
        // the stable `pack/expired-<hash>.pack`.
        &["repack", "--cruft", "-d", "--expire-to=.git/objects/pack/expired", "-q"],
    ] {
        out.push(Case::new("repack", args, Shape::Packed));
    }

    // Other shapes: a graph file to invalidate, three roots to enumerate, and
    // extra reachability roots that `-A` has to honour when it decides which
    // objects to turn loose rather than drop.
    for &shape in &[Shape::CommitGraph, Shape::Unrelated, Shape::Stashed, Shape::Worktree] {
        out.push(Case::new("repack", &["repack", "-A", "-d", "-q"], shape));
    }
    out.push(Case::new("repack", &["repack", "--cruft", "-d", "-q"], Shape::Stashed));
    out.push(Case::new("repack", &["repack", "-a", "-d", "--write-midx", "-q"], Shape::CommitGraph));

    // Refusals. `--name-hash-version=9` is rejected by `builtin/repack.c` before
    // any pack is written; `repack -a -d -l` and `-A -d` over the damaged store
    // are rejected by the reachability walk for the same reason `gc` is, and
    // leave all nine loose objects intact. `--cruft -d` over the same store is
    // *not* a refusal — it exits 0 — which is why it is here beside them.
    out.push(Case::strict(
        "repack",
        &["repack", "-a", "-d", "--name-hash-version=9", "-q"],
        Shape::Packed,
    ));
    out.push(Case::strict("repack", &["repack", "-a", "-d", "-l", "-q"], Shape::Damaged));
    out.push(Case::strict("repack", &["repack", "-A", "-d", "-q"], Shape::Damaged));
    out.push(Case::new("repack", &["repack", "--cruft", "-d", "-q"], Shape::Damaged));
}

/// The configuration `repack` reads for the same decisions.
///
/// `repack.writeBitmaps` is directly observable — the `.bitmap` line appears or
/// does not — and `pack.writeReverseIndex=false` drops the `.rev` line, which is
/// the sidecar half of the same question over a repository where two packs
/// collapse into one. `repack.packKeptObjects` and `repack.cruftWindow` are
/// pinned because a port that reads only the flag and not the key scores full
/// marks on the flag cases above and fails here.
fn repack_config(out: &mut Vec<Case>) {
    for cfg in [
        &[("repack.useDeltaBaseOffset", "false")][..],
        &[("repack.packKeptObjects", "true")],
        &[("repack.writeBitmaps", "true")],
        &[("repack.writeBitmaps", "false")],
        &[("pack.writeReverseIndex", "false")],
    ] {
        out.push(Case::new("repack", &["repack", "-a", "-d", "-q"], Shape::Packed).with_config(cfg));
    }
    out.push(
        Case::new("repack", &["repack", "--cruft", "-d", "-q"], Shape::Packed)
            .with_config(&[("repack.cruftWindow", "1")]),
    );
}

// ---------------------------------------------------------------------------
// prune and prune-packed
// ---------------------------------------------------------------------------

/// The two verbs that delete objects and nothing else.
///
/// `builtin/prune.c` does **two** things, and every fixture except
/// [`Shape::Packed`] can only show one of them: it removes loose objects that
/// are unreachable *and* older than `--expire`, and it calls
/// `prune_packed_objects()` unconditionally, which removes every loose object
/// that also exists inside a pack. Measured against stock over
/// [`Shape::Packed`], the eight loose objects split five/three along exactly
/// that line:
///
/// | invocation             | loose after | objects after |
/// |------------------------|-------------|---------------|
/// | `prune -n`             | 8           | 34            |
/// | `prune --expire=never` | 3           | 34            |
/// | `prune --expire=now`   | 0           | 31            |
///
/// So a port that implements only the expire half leaves eight, one that
/// implements only the packed-duplicate half leaves three and keeps 34 objects,
/// and only one that does both matches. `prune -v` prints the three unreachable
/// ids it removed and `-n -v` additionally prints `rm -f .git/objects/<xx>/<38>`
/// for the five duplicates — relative paths and fixture-constant ids, so both
/// listings are byte-comparable.
///
/// The refusals are what protect data: an unparsable or empty `--expire=` must
/// abort with nothing removed, and over [`Shape::Damaged`] stock refuses the
/// whole run with `fatal: unable to parse object: refs/heads/dangling` and
/// leaves all nine loose objects — including the corrupt one — in place. That
/// last pair is the case this module exists for.
fn prune_family(out: &mut Vec<Case>) {
    for args in [
        &["prune", "-v"][..],
        &["prune", "-n", "-v"],
        &["prune", "--expire=now", "-v"],
        &["prune", "--expire=never", "-v"],
        &["prune", "--expire=2.weeks.ago", "-v"],
        &["prune", "--progress", "--expire=now"],
        &["prune", "-n", "--progress"],
        &["prune", "--exclude-promisor-objects", "--expire=now", "-v"],
        // An explicit `<head>` operand: an extra reachability root on the
        // command line, which `builtin/prune.c` adds to the walk. `HEAD` is
        // already a root, so the correct answer is the same three removals — a
        // port that ignores the operand and one that mishandles it are still
        // separated by the `--expire=never` neighbour above.
        &["prune", "HEAD", "--expire=now", "-v"],
    ] {
        out.push(Case::new("prune", args, Shape::Packed));
    }
    for &shape in &[Shape::Stashed, Shape::Worktree, Shape::Unrelated] {
        out.push(Case::new("prune", &["prune", "--expire=now", "-v"], shape));
    }
    out.push(Case::strict("prune", &["prune", "--expire=not-a-date"], Shape::Packed));
    out.push(Case::strict("prune", &["prune", "--expire="], Shape::Packed));
    out.push(Case::strict("prune", &["prune", "-n", "-v"], Shape::Damaged));
    out.push(Case::strict("prune", &["prune", "--expire=now", "-v"], Shape::Damaged));

    // `prune-packed` is the second half on its own. It never consults
    // reachability, so it is the one verb in this module that is *safe* over the
    // damaged store — stock removes nothing there and exits 0, which is the
    // answer a port that conflates it with `prune` gets wrong.
    for args in [
        &["prune-packed", "-n", "-v"][..],
        &["prune-packed", "--dry-run", "--quiet"],
        &["prune-packed"],
    ] {
        out.push(Case::new("prune-packed", args, Shape::Packed));
    }
    for &shape in &[Shape::Stashed, Shape::Worktree, Shape::Damaged] {
        out.push(Case::new("prune-packed", &["prune-packed"], shape));
    }
    out.push(Case::strict("prune-packed", &["prune-packed", "--bogus"], Shape::Packed));
}

// ---------------------------------------------------------------------------
// fsck
// ---------------------------------------------------------------------------

/// `fsck` over a store that is damaged, and over one that is merely packed.
///
/// Everything `fsck` reports about [`Shape::Damaged`] goes to **stderr** — the
/// three `error:` lines for the broken symref, the dangling ref and the corrupt
/// loose object — and its stdout is empty. `fixture_gaps.rs` already pins eight
/// flags on this shape on exit code alone; the cases here are `strict`, which is
/// the only way the diagnostic text itself is compared. A port that exits 3 with
/// a different message, or with two of the three errors, is indistinguishable
/// without it.
///
/// The exit codes are not uniform, and that is the point: `builtin/fsck.c`
/// returns 3 for "errors found" on the default path, and the
/// `--connectivity-only --strict` combination instead *dies* with exit 128 on
/// `fatal: loose object … is corrupt`, because the strict connectivity walk
/// opens the object rather than merely noting it missing.
///
/// `--lost-found` is included for its stdout (`dangling commit <oid>`, an id the
/// fixture pins) and its exit code only: it writes `.git/lost-found/commit/<oid>`
/// and no probe in the runner reads that directory — see the module doc.
fn fsck_damage(out: &mut Vec<Case>) {
    for args in [
        &["fsck", "--no-progress", "--full"][..],
        &["fsck", "--no-progress", "--cache"],
        &["fsck", "--no-progress", "--unreachable", "--dangling"],
        &["fsck", "--no-progress", "--connectivity-only", "--strict"],
    ] {
        out.push(Case::strict("fsck", args, Shape::Damaged));
    }
    out.push(Case::new("fsck", &["fsck", "--no-progress", "--lost-found"], Shape::Damaged));

    // The same command over a healthy but packed store, where the only finding
    // is the one unreachable commit. Here stdout carries the answer, so these
    // are byte-comparable without `strict`. `--root` prepends the root commit
    // line and `--unreachable` reclassifies the one dangling commit into three
    // unreachable objects, so the three listings are all different.
    for args in [
        &["fsck", "--no-progress", "--cache"][..],
        &["fsck", "--no-progress", "--no-reflogs"],
        &["fsck", "--no-progress", "--root"],
        &["fsck", "--no-progress", "--unreachable", "--name-objects"],
        &["fsck", "--no-progress", "--lost-found"],
    ] {
        out.push(Case::new("fsck", args, Shape::Packed));
    }
    for &shape in &[Shape::Stashed, Shape::Worktree, Shape::Unrelated] {
        out.push(Case::new("fsck", &["fsck", "--no-progress", "--unreachable"], shape));
    }

    // Configuration. `fsck.<msg-id>` reclassifies one diagnostic;
    // `transfer.fsckObjects` must *not* change a local `fsck` at all, because it
    // gates the receive path and nothing else.
    out.push(
        Case::strict("fsck", &["fsck", "--no-progress"], Shape::Damaged)
            .with_config(&[("fsck.badObjectSha1", "ignore")]),
    );
    out.push(
        Case::strict("fsck", &["fsck", "--no-progress"], Shape::Damaged)
            .with_config(&[("transfer.fsckObjects", "true")]),
    );
    out.push(
        Case::new("fsck", &["fsck", "--no-progress", "--strict"], Shape::Packed)
            .with_config(&[("fsck.missingTaggerEntry", "error")]),
    );
    // Refusal: a skip list that is not there. Stock exits 128 with
    // `fatal: could not open object name list: <name>` before checking anything,
    // so a port that treats an unreadable skip list as an empty one reports a
    // clean repository over a damaged store.
    out.push(
        Case::strict("fsck", &["fsck", "--no-progress"], Shape::Damaged)
            .with_config(&[("fsck.skipList", "nonexistent-skiplist")]),
    );
}

// ---------------------------------------------------------------------------
// maintenance
// ---------------------------------------------------------------------------

/// `maintenance run`, one task at a time, over a store the tasks have work to do
/// in.
///
/// `maintenance.rs` runs each `--task=` over [`Shape::Branched`], where four of
/// the tasks are no-ops — there is one loose object store, no pack to index, and
/// nothing unreachable. Over [`Shape::Packed`] each task leaves a different,
/// exactly identified mark, measured against stock 2.55.0:
///
/// | task                  | mark on `objects/pack`                     |
/// |-----------------------|--------------------------------------------|
/// | `loose-objects`       | a `loose-<hash>.pack`, loose 8 → 3          |
/// | `incremental-repack`  | a `multi-pack-index`, loose unchanged       |
/// | `geometric-repack`    | three packs plus a midx, loose 8 → 0        |
/// | `gc`                  | a cruft pack with its `.mtimes`, loose → 0  |
/// | `pack-refs`           | nothing under `objects/`                    |
/// | `prefetch`            | nothing: no remote to prefetch from         |
///
/// Every case carries `--no-detach`. Without it `maintenance run` can return
/// while a child is still writing, and the object listing is then read
/// mid-rewrite — reproduced against stock with `--schedule=daily`, which came
/// back with an unreadable object store on one run in three.
///
/// The `--task=loose-objects` case over [`Shape::Damaged`] is the half-written
/// artifact this module was asked to look for: stock's `pack-objects` dies on
/// the corrupt loose object and **leaves `objects/pack/tmp_pack_<suffix>`
/// behind**, exit 1. `runner::stable_entry_name` masks the mkstemp suffix, so
/// the leftover is a stable line in the storage listing and a port that cleans
/// up — or that never wrote one — diverges on it.
fn maintenance_tasks(out: &mut Vec<Case>) {
    for task in [
        "gc",
        "loose-objects",
        "incremental-repack",
        "geometric-repack",
        "pack-refs",
        "commit-graph",
        "prefetch",
        "reflog-expire",
        "rerere-gc",
    ] {
        let flag = format!("--task={task}");
        out.push(Case::new(
            "maintenance",
            &["maintenance", "run", flag.as_str(), "--no-detach", "--quiet"],
            Shape::Packed,
        ));
    }
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--task=gc", "--task=pack-refs", "--no-detach", "--quiet"],
        Shape::Packed,
    ));
    out.push(Case::new("maintenance", &["maintenance", "run", "--no-detach"], Shape::Packed));
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--auto", "--no-detach", "--quiet"],
        Shape::Packed,
    ));

    // `--schedule=` selects tasks by their configured frequency rather than by
    // name. With nothing configured every frequency is a no-op, which is itself
    // the assertion: a port that runs the default task set under `--schedule=`
    // rewrites a store stock leaves alone. The second case configures one task
    // at `hourly` so the same `--schedule=daily` does reach it.
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--schedule=daily", "--no-detach", "--quiet"],
        Shape::Packed,
    ));
    out.push(
        Case::new(
            "maintenance",
            &["maintenance", "run", "--schedule=daily", "--no-detach", "--quiet"],
            Shape::Packed,
        )
        .with_config(&[("maintenance.gc.enabled", "true"), ("maintenance.gc.schedule", "hourly")]),
    );
    out.push(
        Case::new("maintenance", &["maintenance", "run", "--no-detach", "--quiet"], Shape::Packed)
            .with_config(&[("maintenance.loose-objects.enabled", "true")]),
    );
    out.push(
        Case::new("maintenance", &["maintenance", "run", "--no-detach", "--quiet"], Shape::Packed)
            .with_config(&[("maintenance.gc.enabled", "false")]),
    );
    out.push(
        Case::new(
            "maintenance",
            &["maintenance", "run", "--auto", "--no-detach", "--quiet"],
            Shape::Packed,
        )
        .with_config(&[("maintenance.auto", "false")]),
    );

    // Other shapes, and the two failures. `incremental-repack` over a store with
    // no pack at all exits 1 with `error: no pack files to index.`; the
    // `loose-objects` failure is the tmp-pack case described above.
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--task=loose-objects", "--no-detach", "--quiet"],
        Shape::Stashed,
    ));
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--task=pack-refs", "--no-detach", "--quiet"],
        Shape::Worktree,
    ));
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--task=incremental-repack", "--no-detach", "--quiet"],
        Shape::Worktree,
    ));
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--task=commit-graph", "--no-detach", "--quiet"],
        Shape::CommitGraph,
    ));
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--task=loose-objects", "--no-detach", "--quiet"],
        Shape::Damaged,
    ));
    out.push(Case::strict(
        "maintenance",
        &["maintenance", "run", "--task=gc", "--no-detach", "--quiet"],
        Shape::Damaged,
    ));
}

/// The subcommands that write outside the fixture, reached only where they
/// refuse.
///
/// See the module doc for why nothing here may succeed. Each of these six is an
/// option-parse failure in `parse_options()`, before `maintenance_start()` or
/// `maintenance_register()` runs a single step, and each was checked by hand
/// against stock 2.55.0 with `~/Library/LaunchAgents`, `crontab -l` and
/// `~/.gitconfig` fingerprinted on both sides of the run: all six exit 129 and
/// all three fingerprints were identical afterwards.
///
/// `strict` throughout, because the refusal *is* the case: the usage text and
/// the `error:` line are the entire output, and an exit code alone would not
/// distinguish "rejected the argument" from "rejected the subcommand".
fn scheduler_refusals(out: &mut Vec<Case>) {
    for args in [
        &["maintenance", "start", "--scheduler=bogus"][..],
        &["maintenance", "start", "--scheduler="],
        &["maintenance", "start", "--scheduler"],
        &["maintenance", "start", "extra-operand"],
        &["maintenance", "register", "--config-file"],
        &["maintenance", "unregister", "--force", "extra-operand"],
    ] {
        out.push(Case::strict("maintenance", args, Shape::Branched));
    }
}

// ---------------------------------------------------------------------------
// pack-refs
// ---------------------------------------------------------------------------

/// `pack-refs` moves refs between two storage forms without changing what they
/// name, so the assertion is `for-each-ref`: the same refs, the same targets,
/// afterwards.
///
/// `plumbing_refs.rs` covers the flag surface over [`Shape::Branched`], where
/// there are three refs and no reason for any of them to be treated
/// differently. The value added here is the shapes where selection matters — a
/// linked worktree's per-worktree refs ([`Shape::Worktree`]), three independent
/// root branches ([`Shape::Unrelated`]), and `refs/stash` ([`Shape::Stashed`]) —
/// plus the one interesting refusal.
///
/// Over [`Shape::Damaged`], stock prints
/// `error: refs/heads/dangling does not point to a valid object!` on stderr and
/// still **exits 0**, packing `refs/heads/main` and leaving both broken refs
/// loose. That pairing — a diagnostic with a success exit, and two refs
/// deliberately not deleted — is what `strict` is for: a port that exits
/// non-zero, that is silent, or that prunes the refs it could not read is caught
/// by one of the three halves.
fn pack_refs(out: &mut Vec<Case>) {
    for &shape in &[Shape::Worktree, Shape::Unrelated, Shape::Stashed] {
        out.push(Case::new("pack-refs", &["pack-refs", "--all"], shape));
        out.push(Case::new("pack-refs", &["pack-refs", "--auto"], shape));
    }
    out.push(Case::new("pack-refs", &["pack-refs", "--all", "--no-prune"], Shape::Packed));
    out.push(Case::new(
        "pack-refs",
        &["pack-refs", "--include", "refs/heads/*", "--exclude", "refs/heads/main"],
        Shape::Unrelated,
    ));
    out.push(Case::new(
        "pack-refs",
        &["pack-refs", "--all", "--exclude", "refs/heads/linked"],
        Shape::Worktree,
    ));
    out.push(Case::strict("pack-refs", &["pack-refs", "--all"], Shape::Damaged));
    out.push(Case::strict("pack-refs", &["pack-refs", "--auto"], Shape::Damaged));
    // Refusal: `--include` with no value. Rejected by `parse_options()`, so
    // nothing is packed and no ref is touched.
    out.push(Case::strict("pack-refs", &["pack-refs", "--include"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// reflog expire / delete
// ---------------------------------------------------------------------------

/// `reflog expire` is the one verb in this module whose whole effect is compared
/// byte for byte: `runner::probe_reflogs` reads every file under `.git/logs` and
/// folds the contents into the state digest.
///
/// The fixtures' reflog entries all carry `env::harden`'s pinned committer time
/// (`1700000000`), which is years in the past, so the default 90-day
/// `gc.reflogExpire` already expires every entry. That makes the *negative*
/// cases the load-bearing ones: `--expire=never` and `--dry-run` must leave all
/// nine lines of [`Shape::Branched`]'s three logs exactly as they were, and a
/// port whose expiry is unconditional is caught there rather than on the cases
/// that empty the logs.
///
/// The selection axes each have a distinguishing outcome measured against stock:
/// `reflog expire refs/stash --expire=all` over [`Shape::Stashed`] empties
/// `logs/refs/stash` and leaves `logs/HEAD` at six lines and
/// `logs/refs/heads/main` at three; `--updateref --rewrite --expire=all
/// refs/heads/main` empties that one log and leaves `HEAD`'s five lines alone;
/// `--dry-run --verbose` prints twelve `would prune …` lines and changes nothing.
///
/// `--single-worktree` against `--all` is measurable on [`Shape::Worktree`]
/// because the linked worktree's own log at `.git/worktrees/wt/logs/HEAD` is
/// compared by content — by `runner::probe_worktrees`, not by `probe_reflogs`,
/// which stops at `.git/logs`. Stock empties both logs under `--all`; a port
/// that expires only the common ones leaves `wt/logs/HEAD` behind, and that is
/// the exact line the comparison prints.
fn reflog_expire(out: &mut Vec<Case>) {
    for args in [
        &["reflog", "expire", "--all", "--expire=never"][..],
        &["reflog", "expire", "--all", "--dry-run", "--expire=now"],
        &["reflog", "expire", "--all", "--verbose", "--expire=now"],
        &["reflog", "expire", "--all", "--expire=now", "--expire-unreachable=never"],
        &["reflog", "expire", "--all", "--expire-unreachable=all"],
        &["reflog", "expire", "--all", "--rewrite", "--expire=now"],
        &["reflog", "expire", "--all", "--updateref", "--expire=all"],
        &["reflog", "expire", "--stale-fix", "--all"],
        &["reflog", "expire", "--updateref", "--rewrite", "--expire=all", "refs/heads/main"],
        &["reflog", "expire", "--expire=all", "refs/heads/feature"],
    ] {
        out.push(Case::new("reflog", args, Shape::Branched));
    }
    // The two `gc.*` keys `builtin/reflog.c` falls back to when no `--expire=`
    // is given. Both directions, because a port that ignores the keys entirely
    // matches on one of them by accident.
    out.push(
        Case::new("reflog", &["reflog", "expire", "--all"], Shape::Branched)
            .with_config(&[("gc.reflogExpire", "never"), ("gc.reflogExpireUnreachable", "never")]),
    );
    out.push(
        Case::new("reflog", &["reflog", "expire", "--all"], Shape::Branched)
            .with_config(&[("gc.reflogExpire", "now"), ("gc.reflogExpireUnreachable", "now")]),
    );

    // Extra reflogs to select among: `refs/stash`, and a linked worktree.
    out.push(Case::new("reflog", &["reflog", "expire", "--all", "--expire=now"], Shape::Stashed));
    out.push(Case::new(
        "reflog",
        &["reflog", "expire", "--expire=all", "refs/stash"],
        Shape::Stashed,
    ));
    out.push(Case::new(
        "reflog",
        &["reflog", "expire", "--all", "--dry-run", "--verbose", "--expire=all"],
        Shape::Stashed,
    ));
    out.push(Case::new("reflog", &["reflog", "expire", "--all", "--expire=now"], Shape::Worktree));
    out.push(Case::new(
        "reflog",
        &["reflog", "expire", "--single-worktree", "--all", "--expire=now"],
        Shape::Worktree,
    ));
    out.push(Case::new(
        "reflog",
        &["reflog", "expire", "--all", "--expire=now"],
        Shape::CommitGraph,
    ));

    // `delete` on one entry: the surrounding lines must survive, which the
    // byte-for-byte reflog comparison is what proves.
    out.push(Case::new("reflog", &["reflog", "delete", "HEAD@{0}"], Shape::Branched));
    out.push(Case::new(
        "reflog",
        &["reflog", "delete", "--updateref", "refs/heads/main@{0}"],
        Shape::Branched,
    ));
    out.push(Case::new("reflog", &["reflog", "delete", "stash@{1}"], Shape::Stashed));

    // Refusals: an unparsable timestamp on either expiry knob must abort with
    // every reflog intact.
    out.push(Case::strict(
        "reflog",
        &["reflog", "expire", "--all", "--expire=bogus"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "reflog",
        &["reflog", "expire", "--all", "--expire-unreachable=bogus"],
        Shape::Branched,
    ));
}
