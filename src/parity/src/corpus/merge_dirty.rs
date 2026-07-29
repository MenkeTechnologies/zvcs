//! Differential cases for git's dirty-worktree gates on `merge` and `pull`.
//!
//! git does not ask whether the worktree is dirty. It asks two narrower
//! questions, in this order, and only a strategy asks the first:
//!
//! * **Index versus `HEAD`** — merge-ort's `merge_start()` (and `merge-ours`,
//!   and `git-merge-octopus`'s opening `diff-index --cached`) refuse the whole
//!   merge when the index differs from `HEAD` anywhere, whether or not the merge
//!   would go near that path. A fast-forward runs no strategy and so skips this
//!   entirely.
//! * **This path, on the way past** — `unpack_trees()`' `twoway_merge` plus
//!   `verify_uptodate()`/`verify_absent()` refuse per path, and only look at
//!   paths the two trees disagree on. Everything else is `keep_entry()`d.
//!
//! The distinction is invisible to a corpus whose dirty shape has nothing to
//! merge: refusing every dirty merge and refusing git's subset score the same.
//! [`Shape::MergeableDirty`] and [`Shape::MergeableStaged`] exist to separate
//! them, and the cases below walk one branch per outcome.
//!
//! # What these pin, and what they cannot
//!
//! The runner compares stdout, exit code, and the post-command state probed by
//! stock git. All three carry weight here:
//!
//! * The **exit code** is the gate's identity. git leaves 1 behind a refused
//!   fast-forward (`checkout_fast_forward()` failed, no strategy ran) and 2
//!   behind a refused strategy (`Merge with strategy <name> failed.`), so a port
//!   that refuses in the wrong layer fails the case even when it refuses.
//! * **stdout** carries `Updating <a>..<b>`, which git prints *before* it
//!   attempts the checkout — so a refused fast-forward still emits it — and the
//!   octopus strategy's own refusal block, which is on stdout rather than
//!   stderr because a shell strategy `echo`s it.
//! * The **state digest** is what proves the local work survived: `status
//!   --porcelain` still reporting ` M keep.txt` after a merge landed, or `M `
//!   for the staged shape, is the assertion that the merge wrote only its own
//!   footprint. A refusal case leans on the same digest for the opposite claim —
//!   `HEAD` unmoved, nothing checked out.
//!
//! stderr is deliberately not byte-compared by this harness (see `runner`), so
//! the wording of `error: Your local changes …`, its tab-indented path list and
//! the trailing `Aborting` are pinned by the unit tests in
//! `src/extensions/tests/merge_dirty_worktree.rs` instead. The two are
//! complementary: those tests assert the bytes against a transcript of stock
//! git, these assert the behaviour against stock git as it runs.
//!
//! # Cases that cannot pass yet, and why
//!
//! Every case below agrees with stock git on exit code, and the fast-forward
//! ones agree on the post-state byte for byte. Three groups still differ, none
//! of them about which paths a merge may write over:
//!
//! * **Any case that runs a strategy over a dirty tree** differs only by five
//!   unreferenced objects. `try_merge_strategy()` calls `save_state()` first,
//!   which is `git stash create` in all but name — an index commit, a worktree
//!   commit, its tree and the two dirty blobs — so it can `restore_state()` if
//!   the strategy fails. The objects are never referenced and are left behind
//!   either way, and `cat-file --batch-all-objects` counts them as state. This
//!   port needs no snapshot: nothing on disk moves until the checkout gate has
//!   already decided, so there is nothing to roll back and nothing to write.
//!   Matching git here would mean writing garbage on purpose.
//! * **`pull`** additionally disagrees on the reflog *message*: git's
//!   `cmd_merge` reads `GIT_REFLOG_ACTION`, which `pull` sets, so a pull leaves
//!   `pull . refs/heads/<branch>: Fast-forward` where this port leaves
//!   `merge FETCH_HEAD: Fast-forward`. That gap predates these cases — it is
//!   stated in the `pull` module's own docs and is what the pre-existing
//!   `branched::pull` cases already fail on.
//! * **A diverged `pull` with no reconcile preference** exits 128 under git
//!   (`hint: You have divergent branches …`) and merges here. The bare
//!   `pull . refs/heads/div-cold` cases pin that; their `--no-rebase` twins pin
//!   the gate itself, which is what they are for.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    fast_forward_gate(out);
    strategy_gate(out);
    through_pull(out);
    frontier_order(out);
}

/// The per-path gate, which is the only one a fast-forward reaches.
fn fast_forward_gate(out: &mut Vec<Case>) {
    fn dirty(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("merge", args, Shape::MergeableDirty));
    }

    // Lands: `cold.txt` is the whole footprint and nothing local is on it. The
    // two local edits and the untracked file have to survive, which the digest
    // is what checks.
    dirty(out, &["merge", "ff-cold"]);
    dirty(out, &["merge", "--ff-only", "ff-cold"]);
    dirty(out, &["merge", "--quiet", "ff-cold"]);
    // `--squash` fast-forwards the content without moving the ref, and reaches
    // the same gate on the way.
    dirty(out, &["merge", "--squash", "ff-cold"]);

    // Refused: `hot.txt` is edited in the worktree and rewritten by the branch.
    // Exit 1, `Updating <a>..<b>` already printed, `main` left where it was.
    dirty(out, &["merge", "ff-hot"]);
    dirty(out, &["merge", "--ff-only", "ff-hot"]);
    dirty(out, &["merge", "--squash", "ff-hot"]);

    // Refused by `verify_absent()` instead: the branch adds a path an untracked
    // file already occupies. Different message class, same exit code.
    dirty(out, &["merge", "ff-squat"]);

    // `--no-ff` over a fast-forwardable history is a strategy after all, so it
    // refuses in the other layer — the pair is here to keep them distinguished.
    dirty(out, &["merge", "--no-ff", "ff-cold"]);
    dirty(out, &["merge", "--no-ff", "ff-hot"]);

    // The staged shape's whole point: a fast-forward does not consult the index
    // gate, so it lands and the staged change is still staged afterwards.
    out.push(Case::new("merge", &["merge", "ff-cold"], Shape::MergeableStaged));
    out.push(Case::new("merge", &["merge", "--squash", "ff-cold"], Shape::MergeableStaged));
    // …while the per-path gate still applies on the staged shape's clean tree,
    // where it has nothing to refuse.
    out.push(Case::new("merge", &["merge", "ff-hot"], Shape::MergeableStaged));
}

/// The index-versus-`HEAD` gate, which every strategy runs and each reports
/// differently: `ort` and `ours` on stderr behind their own exit 2, the octopus
/// on stdout in its own shape.
fn strategy_gate(out: &mut Vec<Case>) {
    fn dirty(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("merge", args, Shape::MergeableDirty));
    }
    fn staged(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("merge", args, Shape::MergeableStaged));
    }

    // Three-way merges over unstaged local work: the index matches HEAD, so the
    // strategy gate passes and only the footprint decides.
    dirty(out, &["merge", "div-other"]); // footprint is one added path
    dirty(out, &["merge", "div-cold"]); // footprint is a path nothing local touches
    dirty(out, &["merge", "div-hot"]); // footprint is the locally edited path
    dirty(out, &["merge", "div-squat"]); // footprint is the squatted path

    // `-s ours` keeps our tree verbatim, so a dirty worktree is none of its
    // business — but a staged change still stops it, with no message of its own.
    dirty(out, &["merge", "-s", "ours", "div-cold"]);
    staged(out, &["merge", "-s", "ours", "div-cold"]);

    // The octopus, both ways: two heads that merge cleanly over a dirty
    // worktree, and the same pair stopped by the index gate.
    dirty(out, &["merge", "div-cold", "div-other"]);
    dirty(out, &["merge", "div-hot", "div-other"]);
    staged(out, &["merge", "div-cold", "div-other"]);

    // `ort` against the staged shape: refused wherever the merge would land,
    // because this gate does not look at the footprint at all.
    staged(out, &["merge", "div-other"]);
    staged(out, &["merge", "div-cold"]);
}

/// Not a gate: the shapes' seven-branch, all-tied-dates history is the frontier
/// ordering case no other shape has.
///
/// `env::harden` pins one commit date for the whole corpus, so every commit ties
/// — and git breaks a tie by insertion order (`commit_list_insert_by_date()`
/// splices a commit in after every entry that is not older). A walk that breaks
/// it any other way reorders `git log` here while the existing one- and
/// two-branch shapes stay silent about it.
pub fn frontier_order(out: &mut Vec<Case>) {
    out.push(Case::new("log", &["log", "--oneline", "--all"], Shape::MergeableDirty));
    out.push(Case::new("log", &["log", "--graph", "--oneline", "--all"], Shape::MergeableDirty));
    out.push(Case::new("rev-list", &["rev-list", "--all"], Shape::MergeableDirty));
}

/// The same gates through `pull`, which is where a human meets them.
///
/// `pull . refs/heads/<branch>` is the corpus's established way to reach the
/// merge path without a network: `transport_local` uses the same form.
fn through_pull(out: &mut Vec<Case>) {
    out.push(Case::new("pull", &["pull", ".", "refs/heads/ff-cold"], Shape::MergeableDirty));
    out.push(Case::new("pull", &["pull", ".", "refs/heads/ff-hot"], Shape::MergeableDirty));
    out.push(Case::new("pull", &["pull", ".", "refs/heads/ff-squat"], Shape::MergeableDirty));
    out.push(Case::new(
        "pull",
        &["pull", "--ff-only", ".", "refs/heads/ff-cold"],
        Shape::MergeableDirty,
    ));
    // Bare: pins git's refusal to reconcile divergent branches without being
    // told how. `--no-rebase`: answers that question, so what is left to
    // disagree about is the gate.
    out.push(Case::new("pull", &["pull", ".", "refs/heads/div-cold"], Shape::MergeableDirty));
    out.push(Case::new(
        "pull",
        &["pull", "--no-rebase", ".", "refs/heads/div-cold"],
        Shape::MergeableDirty,
    ));
    out.push(Case::new(
        "pull",
        &["pull", "--no-rebase", ".", "refs/heads/div-hot"],
        Shape::MergeableDirty,
    ));
    out.push(Case::new("pull", &["pull", ".", "refs/heads/ff-cold"], Shape::MergeableStaged));
    out.push(Case::new("pull", &["pull", ".", "refs/heads/div-cold"], Shape::MergeableStaged));
    out.push(Case::new(
        "pull",
        &["pull", "--no-rebase", ".", "refs/heads/div-cold"],
        Shape::MergeableStaged,
    ));
}
