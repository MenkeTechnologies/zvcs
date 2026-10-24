//! The meters `git fsck --progress` draws after the object scan:
//!
//!   * `verify_reverse_indices()` (odb/source-packed.c:925-957), a delayed meter
//!     over the source's packs that runs with or without `--full`;
//!   * `traverse_reachable()`'s `Checking connectivity` (builtin/fsck.c:185-198),
//!     a delayed meter with no total, one step per object popped off `pending`;
//!   * the `git commit-graph verify --object-dir <source> --[no-]progress` child
//!     `cmd_fsck()` runs for every object source while `core.commitGraph` is on
//!     (builtin/fsck.c:985-999), whose `Verifying commits in commit graph` meter
//!     is a plain `start_progress()` (commit-graph.c:2924-2935) and so shows
//!     under the default `GIT_PROGRESS_DELAY` as well.
//!
//! The port drew none of the three, and its `commit-graph verify` ignored
//! `--progress`. `GIT_PROGRESS_DELAY=0` makes the delayed meters visible on
//! both sides. Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

const NOW: [(&str, &str); 1] = [("GIT_PROGRESS_DELAY", "0")];

/// Three more commits on the clone's one, so the meters have steps to draw.
fn grow(t: &Twin) {
    for m in ["c", "d", "e"] {
        t.prepare(&["commit", "-q", "--allow-empty", "-m", m]);
    }
}

#[track_caller]
fn same_env(t: &Twin, args: &[&str], env: &[(&str, &str)]) -> String {
    let (stock, zvcs) = t.run_env_in("work", args, env);
    assert_eq!(zvcs, stock, "git {args:?} with {env:?}: left is zvcs, right is stock");
    stock.stderr
}

#[test]
fn a_single_commit_graph_is_verified_with_its_meter() {
    let Some(t) = Twin::new("fsck-progress-graph") else { return };
    grow(&t);
    // One pack, so `verify_reverse_indices()` has something to count.
    t.prepare(&["repack", "-a", "-d", "-q"]);
    t.prepare(&["commit-graph", "write", "--reachable"]);
    let err = same_env(&t, &["fsck", "--progress"], &NOW);
    assert!(err.contains("Verifying reverse pack-indexes: 100% (1/1), done.\n"), "{err}");
    assert!(err.contains("Checking connectivity: "), "{err}");
    assert!(err.ends_with("Verifying commits in commit graph: 100% (4/4), done.\n"), "{err}");
    // `start_progress()`: no delay to wait out.
    let err = same_env(&t, &["fsck", "--progress"], &[]);
    assert!(err.ends_with("Verifying commits in commit graph: 100% (4/4), done.\n"), "{err}");
    for args in [
        &["fsck", "--progress", "--connectivity-only"][..],
        &["fsck", "--progress", "--no-full"],
        &["fsck", "--progress", "--unreachable"],
        &["fsck", "--no-progress"],
        &["fsck", "--progress", "--verbose"],
        &["commit-graph", "verify", "--progress"],
        &["commit-graph", "verify", "--no-progress"],
    ] {
        same_env(&t, args, &NOW);
    }
    let err = same_env(&t, &["-c", "core.commitGraph=false", "fsck", "--progress"], &NOW);
    assert!(!err.contains("commit graph"), "{err}");
}

#[test]
fn a_split_chain_counts_every_layer_unless_shallow() {
    let Some(t) = Twin::new("fsck-progress-split") else { return };
    grow(&t);
    t.prepare(&["commit-graph", "write", "--reachable", "--split"]);
    for m in ["f", "g"] {
        t.prepare(&["commit", "-q", "--allow-empty", "-m", m]);
    }
    t.prepare(&["commit-graph", "write", "--reachable", "--split=no-merge"]);
    let err = same_env(&t, &["fsck", "--progress"], &NOW);
    assert!(err.ends_with("Verifying commits in commit graph: 100% (6/6), done.\n"), "{err}");
    let err = same_env(&t, &["commit-graph", "verify", "--progress", "--shallow"], &NOW);
    assert_eq!(
        err,
        "Verifying commits in commit graph:  50% (1/2)\
         \rVerifying commits in commit graph: 100% (2/2)\
         \rVerifying commits in commit graph: 100% (2/2), done.\n"
    );
}
