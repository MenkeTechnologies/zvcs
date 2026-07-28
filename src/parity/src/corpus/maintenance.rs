//! Differential corpus cases for the maintenance subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! # What this subsystem can and cannot assert
//!
//! Every command here rewrites object *storage*, and storage is the one surface
//! the runner compares loosely. `runner::probe_storage` counts loose objects,
//! `.pack`/`.idx`/`.rev`/`.mtimes` files, and the presence of
//! `objects/info/commit-graph` and `objects/info/packs` — it never compares pack
//! *names* or *bytes*, because a pack's filename embeds its checksum and
//! `repack.rs` enumerates objects in its own order with no delta reuse, so the
//! bytes differ by design.
//!
//! Cases below are therefore written against properties that survive that
//! relaxation:
//!
//!   * **object survival** — `cat-file --batch-check --batch-all-objects` is a
//!     probe, so "did this command lose an object" is fully pinned. That is the
//!     assertion behind the `repack -a -d --filter=…` cases: the filtered
//!     objects were once written to a deliberately empty pack and then destroyed
//!     by `-d`.
//!   * **loose-vs-packed transitions** — the loose count and the pack/idx counts
//!     are exact, so a `repack` that silently does nothing cannot pass.
//!   * **artifact presence** — the `.rev` and `.mtimes` counts and the
//!     commit-graph bit catch "wrote the pack but skipped the sidecar".
//!   * **stdout and exit code** — untouched by the storage relaxation, so
//!     `count-objects -v`, `fsck`, and every error path are byte-exact.
//!
//! Two artifacts are invisible to the runner and no case here can assert on
//! them: `objects/pack/multi-pack-index` (extensionless, so `count_ext` never
//! sees it) and `*.bitmap` (not among the counted extensions). Cases that
//! produce them are still included for the stdout, exit code and loose/pack
//! counts they do pin, but a midx- or bitmap-only difference would score MATCH.

use crate::corpus::read_only;
use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    count_objects(out);
    gc(out);
    repack(out);
    prune(out);
    fsck(out);
    commit_graph(out);
    multi_pack_index(out);
    maintenance(out);
    misc(out);
}

/// `count-objects` is pure reporting, so it is byte-comparable on every shape —
/// the strictest command in this group and the baseline the rest lean on.
fn count_objects(out: &mut Vec<Case>) {
    read_only("count-objects", &["count-objects"], out);
    read_only("count-objects", &["count-objects", "-v"], out);
    read_only("count-objects", &["count-objects", "--verbose"], out);
    read_only("count-objects", &["count-objects", "-H"], out);
    read_only("count-objects", &["count-objects", "--human-readable"], out);
    read_only("count-objects", &["count-objects", "-v", "-H"], out);
    // Negated forms: a later `--no-` must cancel an earlier flag.
    read_only("count-objects", &["count-objects", "-v", "--no-verbose"], out);
    read_only("count-objects", &["count-objects", "-H", "--no-human-readable"], out);
    out.push(Case::new("count-objects", &["count-objects", "-v"], Shape::AwkwardPaths));
    out.push(Case::new("count-objects", &["count-objects", "-v"], Shape::Conflicted));
    out.push(Case::new("count-objects", &["count-objects", "-v"], Shape::Submodule));
    // Error path: an unknown option must be rejected the same way.
    read_only("count-objects", &["count-objects", "--bogus"], out);
}

/// `gc` is the whole subsystem in one verb: pack, prune, write the graph. The
/// assertions that matter are that loose objects became packed, that the
/// commit-graph appeared, and that nothing was lost doing it.
fn gc(out: &mut Vec<Case>) {
    for &shape in &[Shape::Linear, Shape::Branched, Shape::Merged, Shape::Detached] {
        out.push(Case::new("gc", &["gc"], shape));
    }
    out.push(Case::new("gc", &["gc"], Shape::Dirty));
    out.push(Case::new("gc", &["gc"], Shape::Conflicted));
    out.push(Case::new("gc", &["gc"], Shape::AwkwardPaths));
    out.push(Case::new("gc", &["gc"], Shape::Submodule));
    out.push(Case::new("gc", &["gc", "--quiet"], Shape::Branched));
    out.push(Case::new("gc", &["gc", "--no-quiet"], Shape::Branched));
    out.push(Case::new("gc", &["gc", "--aggressive"], Shape::Branched));
    out.push(Case::new("gc", &["gc", "--aggressive", "--prune=now"], Shape::Merged));
    out.push(Case::new("gc", &["gc", "--prune=now"], Shape::Detached));
    out.push(Case::new("gc", &["gc", "--no-prune"], Shape::Branched));
    out.push(Case::new("gc", &["gc", "--force"], Shape::Linear));
    out.push(Case::new("gc", &["gc", "--keep-largest-pack"], Shape::Branched));
    // `--cruft` is the flag whose `.mtimes` sidecar the storage probe counts.
    out.push(Case::new("gc", &["gc", "--cruft", "--prune=now"], Shape::Merged));
    out.push(Case::new("gc", &["gc", "--no-cruft", "--prune=now"], Shape::Merged));
    // `--auto` on a repo far under every threshold must decline to do work; with
    // `gc.auto=1` it must not. Both sides get the same config.
    out.push(Case::new("gc", &["gc", "--auto"], Shape::Branched));
    out.push(Case::new("gc", &["-c", "gc.auto=1", "gc", "--auto"], Shape::Branched));
    out.push(Case::new(
        "gc",
        &["-c", "gc.autoDetach=false", "-c", "gc.auto=1", "gc", "--auto"],
        Shape::Merged,
    ));
    out.push(Case::new("gc", &["-c", "gc.writeCommitGraph=false", "gc"], Shape::Branched));
    out.push(Case::new("gc", &["gc", "--bogus"], Shape::Linear));
}

/// `repack` is where pack bytes legitimately diverge, so these cases assert only
/// on counts, sidecar presence, and object survival.
fn repack(out: &mut Vec<Case>) {
    out.push(Case::new("repack", &["repack"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "-a"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "-a", "-d"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "-a", "-d"], Shape::Merged));
    out.push(Case::new("repack", &["repack", "-a", "-d"], Shape::Dirty));
    out.push(Case::new("repack", &["repack", "-a", "-d"], Shape::Detached));
    out.push(Case::new("repack", &["repack", "-a", "-d"], Shape::AwkwardPaths));
    out.push(Case::new("repack", &["repack", "-a", "-d"], Shape::Submodule));
    out.push(Case::new("repack", &["repack", "-A", "-d"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "-d", "-l"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "-a", "-d", "-f"], Shape::Merged));
    out.push(Case::new("repack", &["repack", "-a", "-d", "--quiet"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "-n"], Shape::Branched));
    out.push(Case::new(
        "repack",
        &["repack", "-a", "-d", "--window=10", "--depth=10"],
        Shape::Branched,
    ));
    out.push(Case::new("repack", &["repack", "-a", "-d", "--max-pack-size=1m"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "--geometric=2", "-d"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "--geometric=2", "-d"], Shape::Merged));
    out.push(Case::new("repack", &["repack", "-a", "-d", "--keep-unreachable"], Shape::Branched));
    out.push(Case::new(
        "repack",
        &["repack", "-a", "-d", "--unpack-unreachable=now"],
        Shape::Branched,
    ));
    // Cruft: the `.mtimes` count is the assertion. A `--cruft` run that writes
    // no `.mtimes` shows up here as a sidecar count of 0 against git's.
    out.push(Case::new("repack", &["repack", "--cruft"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "-a", "-d", "--cruft"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "-a", "-d", "--cruft"], Shape::Merged));
    // Sidecars whose presence the probe counts exactly.
    out.push(Case::new(
        "repack",
        &["-c", "pack.writeReverseIndex=false", "repack", "-a", "-d"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "repack",
        &["-c", "pack.writeReverseIndex=true", "repack", "-a", "-d"],
        Shape::Branched,
    ));
    // Bitmap and midx artifacts are *not* observed by the storage probe; these
    // are kept for the loose/pack counts and exit codes they still pin.
    out.push(Case::new("repack", &["repack", "-a", "-d", "--write-bitmap-index"], Shape::Merged));
    out.push(Case::new(
        "repack",
        &["repack", "-a", "-d", "--no-write-bitmap-index"],
        Shape::Merged,
    ));
    out.push(Case::new("repack", &["repack", "-a", "-d", "--write-midx"], Shape::Branched));
    // Object survival under `--filter`: the filtered objects were once written
    // to a deliberately empty pack and then destroyed by `-d`. The all-objects
    // probe is what makes this case load-bearing.
    out.push(Case::new("repack", &["repack", "-a", "-d", "--filter=blob:none"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "-a", "-d", "--filter=blob:none"], Shape::Merged));
    out.push(Case::new(
        "repack",
        &["repack", "-a", "-d", "--filter=blob:limit=1"],
        Shape::Branched,
    ));
    out.push(Case::new("repack", &["repack", "-a", "--filter=blob:none"], Shape::Branched));
    out.push(Case::new("repack", &["repack", "--bogus"], Shape::Linear));
}

/// `prune` and `prune-packed` remove objects, so a false positive here is data
/// loss. Every fixture is fully reachable, which makes "removed nothing" the
/// correct answer and any deletion a visible failure.
fn prune(out: &mut Vec<Case>) {
    read_only("prune", &["prune"], out);
    read_only("prune", &["prune", "-n"], out);
    read_only("prune", &["prune", "--dry-run", "-v"], out);
    read_only("prune", &["prune", "--expire=now"], out);
    out.push(Case::new("prune", &["prune", "-v", "--expire=now"], Shape::Branched));
    out.push(Case::new("prune", &["prune", "--expire=never"], Shape::Branched));
    out.push(Case::new("prune", &["prune", "--expire=2005-04-07"], Shape::Merged));
    out.push(Case::new("prune", &["prune", "--progress"], Shape::Branched));
    out.push(Case::new("prune", &["prune", "--no-progress"], Shape::Branched));
    out.push(Case::new("prune", &["prune", "--expire=now"], Shape::Conflicted));
    out.push(Case::new("prune", &["prune", "--expire=now"], Shape::Submodule));
    // Error paths: an unparsable date, an empty date, and a nonexistent object.
    read_only("prune", &["prune", "--expire=not-a-date"], out);
    read_only("prune", &["prune", "--expire="], out);
    read_only("prune", &["prune", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"], out);

    read_only("prune-packed", &["prune-packed"], out);
    read_only("prune-packed", &["prune-packed", "-n"], out);
    read_only("prune-packed", &["prune-packed", "-q"], out);
    out.push(Case::new("prune-packed", &["prune-packed", "--dry-run", "--quiet"], Shape::Merged));
    out.push(Case::new("prune-packed", &["prune-packed"], Shape::AwkwardPaths));
    out.push(Case::new("prune-packed", &["prune-packed", "--bogus"], Shape::Linear));
}

/// `fsck` is read-only, so its stdout is byte-comparable — the strongest
/// assertion available in this group. `fsck-objects` is the same command under
/// its historical name and must agree with it.
fn fsck(out: &mut Vec<Case>) {
    read_only("fsck", &["fsck"], out);
    read_only("fsck", &["fsck", "--no-progress"], out);
    read_only("fsck", &["fsck", "--strict"], out);
    read_only("fsck", &["fsck", "--connectivity-only"], out);
    read_only("fsck", &["fsck", "--connectivity-only", "--strict"], out);
    read_only("fsck", &["fsck", "--dangling"], out);
    read_only("fsck", &["fsck", "--no-dangling"], out);
    read_only("fsck", &["fsck", "--unreachable"], out);
    read_only("fsck", &["fsck", "--root"], out);
    read_only("fsck", &["fsck", "--tags"], out);
    read_only("fsck", &["fsck", "--cache"], out);
    read_only("fsck", &["fsck", "--full"], out);
    read_only("fsck", &["fsck", "--no-full"], out);
    read_only("fsck", &["fsck", "--name-objects"], out);
    // `--references` and the `refs verify` machinery behind it landed recently.
    read_only("fsck", &["fsck", "--references"], out);
    read_only("fsck", &["fsck", "--no-references"], out);
    out.push(Case::new("fsck", &["fsck", "--strict", "--no-progress"], Shape::Conflicted));
    out.push(Case::new("fsck", &["fsck"], Shape::AwkwardPaths));
    out.push(Case::new("fsck", &["fsck"], Shape::Submodule));
    out.push(Case::new("fsck", &["fsck", "--cache", "--unreachable"], Shape::Dirty));
    out.push(Case::new("fsck", &["fsck", "--lost-found"], Shape::Branched));
    out.push(Case::new("fsck", &["fsck", "HEAD"], Shape::Branched));
    // Error paths: a well-formed oid that names nothing, and an unknown flag.
    read_only("fsck", &["fsck", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"], out);
    read_only("fsck", &["fsck", "--bogus-flag"], out);

    read_only("fsck-objects", &["fsck-objects"], out);
    out.push(Case::new("fsck-objects", &["fsck-objects", "--strict"], Shape::Branched));
    out.push(Case::new("fsck-objects", &["fsck-objects", "--connectivity-only"], Shape::Merged));
    out.push(Case::new(
        "fsck-objects",
        &["fsck-objects", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"],
        Shape::Linear,
    ));
}

/// `commit-graph` is the one artifact in this group claimed byte-identical to
/// stock; the probe still only sees presence, so these lean on exit codes and on
/// the config that must *suppress* the write.
fn commit_graph(out: &mut Vec<Case>) {
    for &shape in &[Shape::Linear, Shape::Branched, Shape::Merged, Shape::Detached] {
        out.push(Case::new("commit-graph", &["commit-graph", "write", "--reachable"], shape));
    }
    out.push(Case::new("commit-graph", &["commit-graph", "write", "--reachable"], Shape::Dirty));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable"],
        Shape::Conflicted,
    ));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable"],
        Shape::Submodule,
    ));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable", "--no-progress"],
        Shape::Branched,
    ));
    // Bloom filters over changed paths.
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable", "--changed-paths"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable", "--changed-paths"],
        Shape::Merged,
    ));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable", "--changed-paths"],
        Shape::AwkwardPaths,
    ));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable", "--no-changed-paths"],
        Shape::Merged,
    ));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable", "--changed-paths", "--max-new-filters=0"],
        Shape::Merged,
    ));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable", "--changed-paths", "--max-new-filters=1"],
        Shape::Merged,
    ));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable", "--append"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable", "--split"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable", "--split=no-merge"],
        Shape::Merged,
    ));
    // `core.commitGraph=false` must suppress the write entirely; the probe reads
    // the resulting presence bit exactly.
    out.push(Case::new(
        "commit-graph",
        &["-c", "core.commitGraph=false", "commit-graph", "write", "--reachable"],
        Shape::Branched,
    ));
    // Error paths: verify with no graph on disk, a missing object dir, a missing
    // subcommand, and an unknown one.
    read_only("commit-graph", &["commit-graph", "verify"], out);
    read_only("commit-graph", &["commit-graph", "verify", "--no-progress"], out);
    read_only("commit-graph", &["commit-graph", "verify", "--object-dir=/nonexistent"], out);
    read_only("commit-graph", &["commit-graph"], out);
    read_only("commit-graph", &["commit-graph", "bogus"], out);
}

/// `multi-pack-index` has no packs to index in any fixture — a case cannot pack
/// first and then index, because a case is one argv — so these are almost
/// entirely error paths, which is the surface most likely to drift.
fn multi_pack_index(out: &mut Vec<Case>) {
    read_only("multi-pack-index", &["multi-pack-index", "write"], out);
    read_only("multi-pack-index", &["multi-pack-index", "verify"], out);
    read_only("multi-pack-index", &["multi-pack-index", "expire"], out);
    read_only("multi-pack-index", &["multi-pack-index", "repack"], out);
    out.push(Case::new(
        "multi-pack-index",
        &["multi-pack-index", "write", "--no-progress"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "multi-pack-index",
        &["multi-pack-index", "write", "--bitmap"],
        Shape::Merged,
    ));
    out.push(Case::new(
        "multi-pack-index",
        &["multi-pack-index", "write", "--preferred-pack=nope"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "multi-pack-index",
        &["multi-pack-index", "repack", "--batch-size=0"],
        Shape::Branched,
    ));
    read_only("multi-pack-index", &["multi-pack-index"], out);
    read_only("multi-pack-index", &["multi-pack-index", "bogus"], out);
}

/// `maintenance` composes the rest of this group behind a task scheduler; the
/// per-task cases isolate which task diverges.
fn maintenance(out: &mut Vec<Case>) {
    out.push(Case::new("maintenance", &["maintenance", "run"], Shape::Branched));
    out.push(Case::new("maintenance", &["maintenance", "run"], Shape::Merged));
    out.push(Case::new("maintenance", &["maintenance", "run", "--quiet"], Shape::Branched));
    out.push(Case::new("maintenance", &["maintenance", "run", "--auto"], Shape::Branched));
    out.push(Case::new("maintenance", &["maintenance", "run", "--auto"], Shape::Merged));
    out.push(Case::new(
        "maintenance",
        &["-c", "maintenance.auto=false", "maintenance", "run", "--auto"],
        Shape::Branched,
    ));
    for task in [
        "gc",
        "commit-graph",
        "pack-refs",
        "loose-objects",
        "incremental-repack",
        "reflog-expire",
        "rerere-gc",
        "geometric-repack",
    ] {
        let flag = format!("--task={task}");
        out.push(Case::new("maintenance", &["maintenance", "run", flag.as_str()], Shape::Branched));
    }
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--task=pack-refs", "--task=commit-graph"],
        Shape::Branched,
    ));
    out.push(Case::new("maintenance", &["maintenance", "run", "--task=gc"], Shape::Merged));
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--schedule=hourly"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--schedule=daily"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "maintenance",
        &["maintenance", "run", "--schedule=weekly"],
        Shape::Branched,
    ));
    // `register`/`unregister` only touch global config, which the harness points
    // at /dev/null — so what is measured is the exit code and that the repo
    // itself is untouched. `start`/`stop` are deliberately absent: on this
    // platform they hand a launchd job to the real session manager, a side
    // effect outside the fixture that a parity case must not cause.
    out.push(Case::new("maintenance", &["maintenance", "register"], Shape::Branched));
    out.push(Case::new("maintenance", &["maintenance", "unregister"], Shape::Branched));
    out.push(Case::new("maintenance", &["maintenance", "unregister", "--force"], Shape::Branched));
    // Error paths.
    read_only("maintenance", &["maintenance"], out);
    read_only("maintenance", &["maintenance", "run", "--task=bogus"], out);
    read_only("maintenance", &["maintenance", "run", "--schedule=bogus"], out);
    read_only("maintenance", &["maintenance", "bogus"], out);
}

/// The three remaining verbs: `update-server-info`, `pack-redundant`, and
/// `backfill`.
fn misc(out: &mut Vec<Case>) {
    // `objects/info/packs` presence is a probed bit.
    read_only("update-server-info", &["update-server-info"], out);
    read_only("update-server-info", &["update-server-info", "-f"], out);
    out.push(Case::new("update-server-info", &["update-server-info", "--force"], Shape::Merged));
    out.push(Case::new("update-server-info", &["update-server-info"], Shape::AwkwardPaths));
    out.push(Case::new("update-server-info", &["update-server-info"], Shape::Submodule));
    out.push(Case::new("update-server-info", &["update-server-info", "--bogus"], Shape::Linear));

    // `pack-redundant` is nominated for removal upstream and refuses to run
    // without the opt-in flag; both the refusal and the opted-in run are pinned.
    read_only("pack-redundant", &["pack-redundant"], out);
    read_only("pack-redundant", &["pack-redundant", "--all"], out);
    read_only("pack-redundant", &["pack-redundant", "--i-still-use-this", "--all"], out);
    out.push(Case::new(
        "pack-redundant",
        &["pack-redundant", "--i-still-use-this", "--all", "--verbose"],
        Shape::Branched,
    ));
    out.push(Case::new("pack-redundant", &["pack-redundant", "--bogus"], Shape::Linear));

    read_only("backfill", &["backfill"], out);
    out.push(Case::new("backfill", &["backfill", "--min-batch-size=1"], Shape::Branched));
    out.push(Case::new("backfill", &["backfill", "--min-batch-size=0"], Shape::Merged));
    out.push(Case::new("backfill", &["backfill", "--sparse"], Shape::Branched));
    out.push(Case::new("backfill", &["backfill", "--no-sparse"], Shape::Branched));
    out.push(Case::new("backfill", &["backfill"], Shape::Submodule));
    out.push(Case::new("backfill", &["backfill", "--min-batch-size=bogus"], Shape::Linear));
}
