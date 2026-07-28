//! Differential corpus cases for the worktree_index subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! Covers the commands that write the index and the working tree:
//! `update-index`, `checkout-index`, `stage`, `sparse-checkout`, `clean`,
//! `worktree`, `column`, `checkout--worker`, plus the `add`/`mv`/`rm`/`checkout`
//! pathspec and force-flag corners the base corpus in `corpus.rs` leaves open.
//!
//! Two structural limits of the harness shape what can be written here, and are
//! recorded so nobody mistakes their absence for an untested-by-choice gap:
//!
//! * **No stdin.** `runner::run_side` wires the child to `Stdio::null()`
//!   (`src/parity/src/runner.rs:130`), so every stdin-driven mode is reachable
//!   only on its EOF path: `column`'s actual layout algorithm,
//!   `update-index --index-info`, `sparse-checkout check-rules`, and
//!   `checkout--worker`'s pkt-line protocol cannot be fed input. The EOF path is
//!   still worth pinning — it is where argument parsing and the empty-input
//!   contract live — but the layout/protocol bodies are not measured.
//! * **One argv per case.** A case is a single invocation against a pristine
//!   fixture, so multi-step setups are not expressible. That rules out
//!   `git rm` on a path a *previous* `sparse-checkout set` excluded, since no
//!   fixture shape ships sparse and nothing here can make one sparse first.
//!
//! `clean` is destructive, so read-only breadth uses `-n`/`--dry-run` and the
//! `-f` variants run against `Dirty`, where the post-command state comparison —
//! not stdout — is the assertion.

use crate::corpus::read_only;
use crate::fixture::Shape;
use crate::runner::Case;

/// Blob id of the fixture's `README.md` (`"# fixture\n"`, written by
/// `fixture::build` before the initial commit). Content-addressed, so it is the
/// same in every shape and independent of the hermetic identity/clock.
const README_BLOB: &str = "9741694d75caeb49d3b7c1f59451c0c56bf6216c";

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    update_index(out);
    checkout_index(out);
    stage_and_add(out);
    sparse_checkout(out);
    clean(out);
    worktree(out);
    column(out);
    checkout_worker(out);
    mv_gaps(out);
    rm_gaps(out);
    checkout_force_gaps(out);
}

/// `update-index`: the lowest-level index writer, so a divergence here is
/// invisible in stdout and only shows up in the post-command index probe.
fn update_index(out: &mut Vec<Case>) {
    // Refresh: exit code carries the answer (1 when entries are out of date).
    read_only("update-index", &["update-index", "--refresh"], out);
    read_only("update-index", &["update-index", "-q", "--refresh"], out);
    out.push(Case::new("update-index", &["update-index", "--really-refresh"], Shape::Dirty));
    out.push(Case::new("update-index", &["update-index", "--unmerged", "--refresh"], Shape::Conflicted));
    out.push(Case::new("update-index", &["update-index", "--ignore-submodules", "--refresh"], Shape::Submodule));

    // Adding and removing entries.
    out.push(Case::new("update-index", &["update-index", "--add", "untracked.txt"], Shape::Dirty));
    out.push(Case::new("update-index", &["update-index", "--verbose", "--add", "untracked.txt"], Shape::Dirty));
    out.push(Case::new("update-index", &["update-index", "--remove", "src/lib.rs"], Shape::Dirty));
    out.push(Case::new("update-index", &["update-index", "--force-remove", "README.md"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--info-only", "--add", "README.md"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--again"], Shape::Dirty));

    // Per-entry bits. These are only observable downstream: `assume-unchanged`
    // and `skip-worktree` make `status` stop reporting a real modification, so
    // the Dirty shape (README.md modified) is the one that can see them.
    out.push(Case::new("update-index", &["update-index", "--assume-unchanged", "README.md"], Shape::Dirty));
    out.push(Case::new("update-index", &["update-index", "--no-assume-unchanged", "README.md"], Shape::Dirty));
    out.push(Case::new("update-index", &["update-index", "--skip-worktree", "README.md"], Shape::Dirty));
    out.push(Case::new("update-index", &["update-index", "--no-skip-worktree", "README.md"], Shape::Dirty));
    out.push(Case::new("update-index", &["update-index", "--chmod=+x", "README.md"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--chmod=-x", "README.md"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--chmod=+x", "üñïçødé.txt"], Shape::AwkwardPaths));

    // Index-format switches. The probes read the index back with stock git, so
    // writing a format stock cannot parse surfaces as a state difference.
    out.push(Case::new("update-index", &["update-index", "--show-index-version"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--index-version", "4"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--index-version", "2"], Shape::Branched));
    out.push(Case::new("update-index", &["update-index", "--split-index"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--no-split-index"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--untracked-cache"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--no-untracked-cache"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--test-untracked-cache"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--fsmonitor"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--no-fsmonitor"], Shape::Linear));

    // `--stdin` with no stdin: EOF immediately, so this pins the empty-input
    // contract only (see the module note on `Stdio::null()`).
    out.push(Case::new("update-index", &["update-index", "--stdin"], Shape::Linear));

    // `--cacheinfo`, both the packed and the legacy three-argument spelling.
    let packed = format!("100644,{README_BLOB},copy.md");
    out.push(Case::new("update-index", &["update-index", "--add", "--cacheinfo", &packed], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--add", "--cacheinfo", "100644", README_BLOB, "legacy.md"], Shape::Linear));
    let link = format!("120000,{README_BLOB},link");
    out.push(Case::new("update-index", &["update-index", "--add", "--cacheinfo", &link], Shape::Linear));
    let gitlink = format!("160000,{README_BLOB},gitlink");
    out.push(Case::new("update-index", &["update-index", "--add", "--cacheinfo", &gitlink], Shape::Linear));
    let exe = format!("100755,{README_BLOB},exe.md");
    out.push(Case::new("update-index", &["update-index", "--add", "--cacheinfo", &exe], Shape::Linear));

    // A null object id in an index entry is a corrupt index. Git refuses to
    // write one at all; anything that accepts it produces a tree stock git will
    // later reject, which the post-state probe sees as an `<err>` digest.
    out.push(Case::new(
        "update-index",
        &["update-index", "--add", "--cacheinfo", "100644,0000000000000000000000000000000000000000,bogus.txt"],
        Shape::Linear,
    ));

    // Error paths.
    let no_add = format!("100644,{README_BLOB},copy.md");
    out.push(Case::new("update-index", &["update-index", "--cacheinfo", &no_add], Shape::Linear));
    read_only("update-index", &["update-index", "nosuchfile.txt"], out);
    out.push(Case::new("update-index", &["update-index", "untracked.txt"], Shape::Dirty));
    out.push(Case::new("update-index", &["update-index", "--chmod=+x", "nosuchfile.txt"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--assume-unchanged", "nosuchfile.txt"], Shape::Linear));
}

/// `checkout-index`: index -> working tree, with the create/overwrite rules
/// that make it distinct from `checkout`.
fn checkout_index(out: &mut Vec<Case>) {
    // Without -f an existing file blocks the checkout and the command exits 1.
    out.push(Case::new("checkout-index", &["checkout-index", "-a"], Shape::Dirty));
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "-f"], Shape::Dirty));
    out.push(Case::new("checkout-index", &["checkout-index", "--all", "--force", "--quiet"], Shape::Dirty));
    out.push(Case::new("checkout-index", &["checkout-index", "README.md"], Shape::Dirty));
    out.push(Case::new("checkout-index", &["checkout-index", "-f", "README.md"], Shape::Dirty));
    out.push(Case::new("checkout-index", &["checkout-index", "-f", "src/lib.rs"], Shape::Dirty));

    // `--prefix` writes a second copy of the tree; with and without the
    // trailing slash, which git treats as directory vs filename prefix.
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "--prefix=out/"], Shape::Linear));
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "--prefix=out"], Shape::Linear));
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "--prefix=out/"], Shape::AwkwardPaths));

    out.push(Case::new("checkout-index", &["checkout-index", "-n", "-a"], Shape::Dirty));
    out.push(Case::new("checkout-index", &["checkout-index", "-u", "-a"], Shape::Dirty));
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "-f"], Shape::AwkwardPaths));
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "-f"], Shape::Detached));
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "-f"], Shape::Conflicted));
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "--stage=2", "--prefix=st/"], Shape::Conflicted));

    // Error paths: no `-a` and no pathspec is a silent no-op, an unknown path
    // is a hard error. Both are contracts scripts depend on.
    out.push(Case::new("checkout-index", &["checkout-index"], Shape::Linear));
    out.push(Case::new("checkout-index", &["checkout-index", "-f"], Shape::Dirty));
    read_only("checkout-index", &["checkout-index", "nosuch.txt"], out);
}

/// `stage` (a documented synonym for `add`), plus the `add` pathspec forms that
/// regressed once already: a trailing-slash directory and a `./`-relative path
/// matched nothing and were reported as ignored.
fn stage_and_add(out: &mut Vec<Case>) {
    out.push(Case::new("stage", &["stage", "untracked.txt"], Shape::Dirty));
    out.push(Case::new("stage", &["stage", "-A"], Shape::Dirty));
    out.push(Case::new("stage", &["stage", "-u"], Shape::Dirty));
    out.push(Case::new("stage", &["stage", "."], Shape::Dirty));
    out.push(Case::new("stage", &["stage", "-n", "untracked.txt"], Shape::Dirty));
    out.push(Case::new("stage", &["stage", "--renormalize", "."], Shape::Dirty));
    out.push(Case::new("stage", &["stage", "."], Shape::AwkwardPaths));
    out.push(Case::new("stage", &["stage", "src/"], Shape::Dirty));
    read_only("stage", &["stage", "nosuch.txt"], out);

    // Directory pathspecs. `src/` and `./src` must stage the deletion of
    // `src/lib.rs` in the Dirty shape, not report the path as ignored.
    out.push(Case::new("add", &["add", "src/"], Shape::Dirty));
    out.push(Case::new("add", &["add", "./src"], Shape::Dirty));
    out.push(Case::new("add", &["add", "./src/"], Shape::Dirty));
    out.push(Case::new("add", &["add", "./untracked.txt"], Shape::Dirty));
    out.push(Case::new("add", &["add", "./"], Shape::Dirty));
    out.push(Case::new("add", &["add", "./."], Shape::Dirty));
    out.push(Case::new("add", &["add", "--", "src/"], Shape::Dirty));
    out.push(Case::new("add", &["add", "nested/"], Shape::AwkwardPaths));
    out.push(Case::new("add", &["add", "./nested"], Shape::AwkwardPaths));
    out.push(Case::new("add", &["add", "nested/deep/"], Shape::AwkwardPaths));
}

/// `sparse-checkout`: each case is one subcommand against a non-sparse repo,
/// which is the only starting state a single invocation can assume.
fn sparse_checkout(out: &mut Vec<Case>) {
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "list"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "list"], Shape::Dirty));

    out.push(Case::new("sparse-checkout", &["sparse-checkout", "init"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "init", "--cone"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "init", "--no-cone"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "init", "--sparse-index"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "init"], Shape::Dirty));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "init"], Shape::Detached));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "init", "--cone"], Shape::Branched));

    out.push(Case::new("sparse-checkout", &["sparse-checkout", "set", "src"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "set", "src", "nested"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "set", "--no-cone", "/src/"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "set", "--no-cone", "/*", "!/src/"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "set", "nested"], Shape::AwkwardPaths));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "set", "src"], Shape::Dirty));

    out.push(Case::new("sparse-checkout", &["sparse-checkout", "disable"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "disable"], Shape::Dirty));

    // Error paths: add/reapply/clean all require an existing sparse-checkout.
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "add", "src"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "reapply"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "clean"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout", "bogus"], Shape::Linear));
    out.push(Case::new("sparse-checkout", &["sparse-checkout"], Shape::Linear));
}

/// `clean`. Read-only breadth uses `--dry-run`; the destructive variants run on
/// `Dirty`, where the surviving working tree is the assertion.
fn clean(out: &mut Vec<Case>) {
    read_only("clean", &["clean", "-n"], out);
    read_only("clean", &["clean", "-nd"], out);
    out.push(Case::new("clean", &["clean", "--dry-run", "-d"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-n", "-x"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-ndx"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-n", "-X"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-n", "-q"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-ndx"], Shape::AwkwardPaths));
    out.push(Case::new("clean", &["clean", "-nd"], Shape::Submodule));
    out.push(Case::new("clean", &["clean", "-nd"], Shape::Conflicted));

    // Destructive: the post-command state probe is what is being compared.
    out.push(Case::new("clean", &["clean", "-f"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-fd"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-fdx"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-ff"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-f", "-e", "untracked.txt"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-f", "untracked.txt"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-f", "nomatch.txt"], Shape::Dirty));
    out.push(Case::new("clean", &["clean", "-fq"], Shape::Dirty));

    // Refusal without `-f` is a safety contract, not cosmetics.
    read_only("clean", &["clean"], out);
    // `-i` with no stdin: EOF on the first prompt. Whether that aborts cleanly
    // or blocks is exactly what the harness's Hang verdict exists to catch.
    out.push(Case::new("clean", &["clean", "-i"], Shape::Dirty));
}

/// `worktree`. `add`, `remove` and `move` are documented as unported; they are
/// exercised anyway so the gap is counted, since `Unsupported` scores as a
/// failure by design.
fn worktree(out: &mut Vec<Case>) {
    read_only("worktree", &["worktree", "list"], out);
    read_only("worktree", &["worktree", "list", "--porcelain"], out);
    out.push(Case::new("worktree", &["worktree", "list", "-v"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "list", "--porcelain", "-z"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "list"], Shape::Submodule));
    out.push(Case::new("worktree", &["worktree", "list"], Shape::Conflicted));

    // Linked worktrees are created *inside* the fixture so each side stays
    // self-contained and the runner's path normalization still applies.
    out.push(Case::new("worktree", &["worktree", "add", "wt1"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "add", "--detach", "wt2", "HEAD"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "add", "-b", "newwt", "wt4"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "add", "wt3", "feature"], Shape::Branched));
    out.push(Case::new("worktree", &["worktree", "add", "wt5", "--detach", "v0.1.0"], Shape::Branched));

    out.push(Case::new("worktree", &["worktree", "prune"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "prune", "-n", "-v"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "repair"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "repair"], Shape::Submodule));

    // Error paths.
    read_only("worktree", &["worktree", "remove", "nosuch"], out);
    out.push(Case::new("worktree", &["worktree", "remove", "--force", "nosuch"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "move", "nosuch", "elsewhere"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "lock", "nosuch"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "unlock", "nosuch"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "lock"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "unlock"], Shape::Linear));
    out.push(Case::new("worktree", &["worktree", "bogus"], Shape::Linear));
}

/// `column`. Its input is stdin, which the runner nulls
/// (`src/parity/src/runner.rs:130`), so these measure option parsing and the
/// empty-input contract — **not** the column layout algorithm, which is
/// unreachable from this harness.
fn column(out: &mut Vec<Case>) {
    out.push(Case::new("column", &["column"], Shape::Linear));
    out.push(Case::new("column", &["column", "--mode=column"], Shape::Linear));
    out.push(Case::new("column", &["column", "--mode=plain"], Shape::Linear));
    out.push(Case::new("column", &["column", "--mode=row", "--padding=2"], Shape::Linear));
    out.push(Case::new("column", &["column", "--mode=dense", "--width=40", "--indent=.."], Shape::Linear));
    out.push(Case::new("column", &["column", "--mode=always", "--width=20"], Shape::Linear));
    out.push(Case::new("column", &["column", "--raw-mode=1"], Shape::Linear));
    out.push(Case::new("column", &["column", "--mode=bogus"], Shape::Linear));
    out.push(Case::new("column", &["column", "--nth=1"], Shape::Linear));
}

/// `checkout--worker`, git's internal parallel-checkout helper. It speaks
/// pkt-line on stdin, so with stdin nulled the only reachable behaviour is the
/// immediate-EOF error and option parsing.
fn checkout_worker(out: &mut Vec<Case>) {
    out.push(Case::new("checkout--worker", &["checkout--worker"], Shape::Linear));
    out.push(Case::new("checkout--worker", &["checkout--worker", "--prefix=out/"], Shape::Linear));
    out.push(Case::new("checkout--worker", &["checkout--worker", "--bogus"], Shape::Linear));
}

/// `mv` corners the base corpus does not reach: directories, overwrite rules,
/// dry-run, and non-ASCII sources.
fn mv_gaps(out: &mut Vec<Case>) {
    out.push(Case::new("mv", &["mv", "src/lib.rs", "src/renamed.rs"], Shape::Linear));
    out.push(Case::new("mv", &["mv", "README.md", "src/"], Shape::Linear));
    out.push(Case::new("mv", &["mv", "src", "newsrc"], Shape::Linear));
    out.push(Case::new("mv", &["mv", "-f", "README.md", "src/lib.rs"], Shape::Linear));
    out.push(Case::new("mv", &["mv", "-n", "README.md", "DOCS.md"], Shape::Linear));
    out.push(Case::new("mv", &["mv", "-v", "README.md", "DOCS.md"], Shape::Linear));
    out.push(Case::new("mv", &["mv", "-k", "nosuch.txt", "other.txt"], Shape::Linear));
    out.push(Case::new("mv", &["mv", "README.md", "src/lib.rs", "newdir"], Shape::Linear));
    out.push(Case::new("mv", &["mv", "README.md", "DOCS.md"], Shape::Dirty));
    out.push(Case::new("mv", &["mv", "src/lib.rs", "moved.rs"], Shape::Dirty));
    out.push(Case::new("mv", &["mv", "with space.txt", "spaced.txt"], Shape::AwkwardPaths));
    out.push(Case::new("mv", &["mv", "üñïçødé.txt", "uni.txt"], Shape::AwkwardPaths));
    out.push(Case::new("mv", &["mv", "quote\"name.txt", "quoted.txt"], Shape::AwkwardPaths));
    out.push(Case::new("mv", &["mv", "README.md", "nested/deep/"], Shape::AwkwardPaths));
    out.push(Case::new("mv", &["mv", "nested/deep", "nested/shallow"], Shape::AwkwardPaths));
    out.push(Case::new("mv", &["mv", "sub", "sub2"], Shape::Submodule));

    // Error paths.
    out.push(Case::new("mv", &["mv", "README.md", "src/lib.rs"], Shape::Linear));
    read_only("mv", &["mv", "nosuch.txt", "other.txt"], out);
    out.push(Case::new("mv", &["mv", "untracked.txt", "moved.txt"], Shape::Dirty));
    out.push(Case::new("mv", &["mv", "README.md", "src/lib.rs", "notadir"], Shape::Linear));
}

/// `rm` corners: recursion, dry-run, and the two refusals (local modifications,
/// staged changes) that must stay non-zero.
fn rm_gaps(out: &mut Vec<Case>) {
    out.push(Case::new("rm", &["rm", "-r", "src"], Shape::Linear));
    out.push(Case::new("rm", &["rm", "-f", "README.md", "src/lib.rs"], Shape::Linear));
    out.push(Case::new("rm", &["rm", "--cached", "-r", "src"], Shape::Linear));
    out.push(Case::new("rm", &["rm", "-n", "README.md"], Shape::Linear));
    out.push(Case::new("rm", &["rm", "-q", "-f", "README.md"], Shape::Linear));
    out.push(Case::new("rm", &["rm", "-r", "."], Shape::Linear));
    out.push(Case::new("rm", &["rm", "--ignore-unmatch", "nosuch.txt"], Shape::Linear));
    out.push(Case::new("rm", &["rm", "-f", "README.md"], Shape::Dirty));
    out.push(Case::new("rm", &["rm", "--cached", "README.md"], Shape::Dirty));
    out.push(Case::new("rm", &["rm", "-f", "src/lib.rs"], Shape::Dirty));
    out.push(Case::new("rm", &["rm", "quote\"name.txt"], Shape::AwkwardPaths));
    out.push(Case::new("rm", &["rm", "üñïçødé.txt"], Shape::AwkwardPaths));
    out.push(Case::new("rm", &["rm", "-r", "nested"], Shape::AwkwardPaths));
    out.push(Case::new("rm", &["rm", "-r", "-f", "sub"], Shape::Submodule));
    out.push(Case::new("rm", &["rm", "-f", "conflict.txt"], Shape::Conflicted));

    // Refusals. Each of these must stay non-zero *and* leave the index alone.
    out.push(Case::new("rm", &["rm", "src"], Shape::Linear));
    out.push(Case::new("rm", &["rm", "README.md"], Shape::Dirty));
    out.push(Case::new("rm", &["rm", "staged.txt"], Shape::Dirty));
    out.push(Case::new("rm", &["rm", "--cached", "staged.txt"], Shape::Dirty));
    out.push(Case::new("rm", &["rm", "conflict.txt"], Shape::Conflicted));
    read_only("rm", &["rm", "nosuch.txt"], out);
    out.push(Case::new("rm", &["rm", "untracked.txt"], Shape::Dirty));
}

/// `checkout -f`: restoring a working tree that is already at the requested
/// commit. Kept here rather than with the branch-switching cases because what
/// it exercises is the working-tree writer, not ref movement.
fn checkout_force_gaps(out: &mut Vec<Case>) {
    // `src/lib.rs` is deleted and `README.md` modified in the Dirty shape.
    // Every form below must restore both and leave the index at HEAD.
    out.push(Case::new("checkout", &["checkout", "-f", "HEAD"], Shape::Dirty));
    out.push(Case::new("checkout", &["checkout", "-f", "main"], Shape::Dirty));
    out.push(Case::new("checkout", &["checkout", "-f"], Shape::Dirty));
    out.push(Case::new("checkout", &["checkout", "--force", "HEAD"], Shape::Dirty));
    out.push(Case::new("checkout", &["checkout", "-f", "--", "."], Shape::Dirty));
    out.push(Case::new("checkout", &["checkout", "-f", "HEAD", "--", "src/lib.rs"], Shape::Dirty));
    out.push(Case::new("checkout", &["checkout", "-f", "HEAD"], Shape::Detached));
    out.push(Case::new("checkout", &["checkout", "-f", "HEAD"], Shape::Conflicted));
}
