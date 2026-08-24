//! `submodule`, `submodule--helper`, and the ordinary verbs run from *inside* a
//! submodule.
//!
//! This is the one subsystem where a port can be wrong about **which repository
//! it is in**. Every other corpus module asks a command to do something in a
//! repository both sides already agree on; here the repository is chosen by an
//! indirection that a naive implementation does not know exists.
//!
//! # What [`Shape::Submodule`] actually contains
//!
//! Built by `fixture.rs:444-468`, and every case below depends on these facts:
//!
//!  * A *separate* upstream repository beside the fixture template, holding one
//!    commit (`submodule initial`) whose only file is `mod.txt`. Its commit id is
//!    `7c9f5d7e12e0b209b88dea5b678f0584186b8b28` under the hermetic identity and
//!    clock, so `submodule status` prints a fixed sha.
//!  * A parent worktree with the common prelude every shape gets — `README.md`,
//!    `src/lib.rs`, one `initial` commit — then `submodule add` of that upstream
//!    at path `sub`, then a second commit `add submodule`. So `HEAD~1` is the
//!    commit *before* the submodule existed, which is what makes
//!    `submodule summary HEAD~1` print a real summary instead of nothing.
//!  * The submodule is **already initialised and checked out**: `.git/config`
//!    carries `submodule.sub.url` and `submodule.sub.active=true`,
//!    `.git/modules/sub/config` carries `core.worktree = ../../../sub` and an
//!    `origin` remote, and `sub/.git` is a *file* reading
//!    `gitdir: ../.git/modules/sub`. `submodule init` therefore prints nothing
//!    (there is nothing left to register) and `submodule update` has nothing to
//!    clone.
//!  * The index holds the gitlink as `160000 7c9f5d7e… 0 sub`, which is what the
//!    runner's `ls-files --stage` state probe reports — the only evidence that an
//!    `update` moved, or failed to move, the recorded commit.
//!  * `sub` is clean. Nothing here can dirty it first, because a case is one argv
//!    against a pristine copy; see the fixture constraints below.
//!
//! # The absolute path in `.gitmodules`
//!
//! `.gitmodules` and `.git/config` record the upstream's **absolute** path, which
//! contains the harness pid (`main.rs:278`). It is identical on both sides within
//! one run — both copies of the fixture are instantiated from the same template
//! and point at the same upstream — so every case scores correctly. It is *not*
//! identical across runs, and `runner::normalize` does not mask it: the upstream
//! lives beside the template rather than under `<REPO>` or `<HOME>`.
//!
//! Invocations whose output echoes that path are therefore stable within a run
//! and not across runs. They are kept where the behaviour is worth the property,
//! and they are exactly these: `deinit` (`Submodule 'sub' (<url>) unregistered`)
//! and `config --list --local` inside the submodule (`remote.origin.url`).
//! Everywhere else a spelling that does not print it was preferred — `sync`
//! prints only `Synchronizing submodule url for 'sub'`, `status` and `summary`
//! print ids and paths, and `submodule init` on this fixture prints nothing at
//! all because the url is already registered.
//!
//! # `git submodule` is still a shell script in 2.55.0
//!
//! `libexec/git-core/git-submodule` is `git-submodule.sh`, and it does two things
//! before dispatching to `git submodule--helper <cmd>` that the helper never does
//! for itself:
//!
//!  * `git-submodule.sh:24-25` — `wt_prefix=$(git rev-parse --show-prefix)` then
//!    `cd_to_toplevel`, and every dispatch passes `-C "$wt_prefix"`. That is why
//!    running from `src/` makes `status`, `summary` and `foreach` name the
//!    submodule `../sub` rather than `sub`. A port that resolves paths against the
//!    worktree root regardless of where it was invoked prints `sub` and passes
//!    every root-directory case.
//!  * `git-submodule.sh:29-30` — `GIT_PROTOCOL_FROM_USER=0`, exported. The
//!    submodule's url is a local path, and `protocol.file` defaults to `user`, so
//!    the front-end is refused where the helper is allowed. Measured against stock
//!    2.55.0, same fixture, same argv:
//!
//!    | invocation | `protocol.file.allow` unset | `=user` | `=always` |
//!    |---|---|---|---|
//!    | `submodule update --remote` | 128 `transport 'file' not allowed` | 128 | 0 |
//!    | `submodule--helper update --remote` | 0 | 0 | 0 |
//!
//!    Both spellings are pinned below, in both directions, because a port that
//!    forwards `submodule` straight to its `submodule--helper` implementation
//!    scores 100% on the helper rows and silently allows a fetch stock refuses.
//!
//! # Fixture constraints worked around here
//!
//!  * **One argv per case.** No multi-step setup, so several documented refusals
//!    are unreachable and are *not* faked with a nearby command: `deinit` without
//!    `-f` over local modifications (nothing can dirty `sub` first), `update`
//!    against a submodule with no url configured (nothing can `deinit` first),
//!    and `absorbgitdirs` with something to absorb (nothing can un-absorb the
//!    already-absorbed `.git/modules/sub`). `absorbgitdirs` is pinned on its
//!    nothing-to-do path instead, which is the path that must stay a silent
//!    success.
//!  * **No nesting.** The submodule has no submodule of its own, so `--recursive`
//!    is measured as "descends and finds nothing" rather than as real recursion.
//!    It still separates a port that ignores the flag from one that rejects it.
//!  * **No network.** `submodule add` of an `https://` url reaches DNS, so the
//!    only remote-url `add` cases here are the ones that refuse *before* cloning
//!    (path already in the index). Cases that really clone use the fixture's own
//!    `./sub` as the source with `protocol.file.allow=always` delivered
//!    identically to both sides.
//!  * **Config scopes reach the parent only.** `ConfigScope::Repo` writes
//!    `.git/config`, never `.git/modules/sub/config`, so `core.worktree` *inside*
//!    the submodule cannot be set by a case. It is read instead — through
//!    `foreach 'git config --get core.worktree'` and `config --list --local` in
//!    `sub` — which is enough to catch a port that never wrote it.
//!
//! # What is already covered elsewhere, and is not repeated
//!
//! `corpus/misc_commands.rs:145-262` holds the first 62 `submodule` /
//! `submodule--helper` cases (the bare subcommands and their simplest flags);
//! `corpus/discovery.rs:329-347` runs the full twelve-column `rev-parse` grid plus
//! `status`, `log` and `symbolic-ref` from `sub` and from `.git/modules/sub`;
//! `corpus/config_cmd.rs:516-525` covers `config` reading `.gitmodules`;
//! `corpus/worktree_index.rs` covers `mv`/`rm`/`clean`/`worktree` over the
//! gitlink. This module extends those rather than restating them.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    status(out);
    init_deinit(out);
    update(out);
    summary(out);
    foreach(out);
    sync_set_url_branch(out);
    absorbgitdirs(out);
    add(out);
    helper_surface(out);
    helper_vs_front_end(out);
    inside_the_submodule(out);
    dash_c_spelling(out);
    explicit_git_dir(out);
}

/// The path the fixture's submodule is checked out at, and the tracked
/// non-submodule path used to prove a pathspec that matches something real but
/// not a submodule still selects nothing.
const SUB: &str = "sub";

// ---------------------------------------------------------------------------
// submodule status
// ---------------------------------------------------------------------------

/// `status`: the gitlink's recorded oid, its checked-out branch, and the leading
/// status character.
///
/// Three things are measured that the base cases in `misc_commands.rs` cannot:
/// the `--` pathspec restriction, the prefix rewrite when run from a
/// subdirectory, and the `-` prefix an inactive submodule gets.
///
/// `submodule.<name>.active=false` is the only spelling in the corpus that makes
/// stock print `-7c9f5d7e… sub` instead of ` 7c9f5d7e… sub (heads/main)` — the
/// leading character is the whole report, and a port that always prints a space
/// agrees with stock everywhere else.
///
/// From `src/` the displayed path becomes `../sub`, because `git-submodule.sh:24`
/// captures the prefix and re-enters at the toplevel with `-C`. `submodule status
/// sub` run from `src/` is then a pathspec relative to `src/`, matches nothing,
/// and exits 1 — pinned strictly, since a port that ignores the prefix answers 0
/// and prints the submodule.
fn status(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    out.push(Case::new("submodule", &["submodule", "status", "--"], s));
    out.push(Case::new("submodule", &["submodule", "status", "--", SUB], s));
    out.push(Case::new("submodule", &["submodule", "status", SUB], s));
    // A tracked path that is not a submodule: selects nothing, exits 0.
    out.push(Case::new("submodule", &["submodule", "status", "--", "src"], s));
    out.push(Case::new("submodule", &["submodule", "status", "--quiet", "--recursive"], s));
    out.push(Case::new("submodule", &["submodule", "status"], s).in_dir("src"));
    out.push(Case::new("submodule", &["submodule", "status", "--recursive"], s).in_dir("src"));
    out.push(Case::new("submodule", &["submodule", "status", "../sub"], s).in_dir("src"));
    out.push(Case::strict("submodule", &["submodule", "status", "sub"], s).in_dir("src"));

    // Activation. `submodule.active` is a pathspec, `submodule.<name>.active` a
    // boolean, and they are read by different code (`submodule.c:is_submodule_active`).
    out.push(
        Case::new("submodule", &["submodule", "status"], s)
            .with_config(&[("submodule.sub.active", "false")]),
    );
    out.push(
        Case::new("submodule", &["submodule", "status"], s)
            .with_config(&[("submodule.active", "sub")]),
    );

    // Inside the submodule there is no submodule: empty, exit 0. A port that
    // walks the *superproject's* index from here prints the parent's gitlink.
    out.push(Case::new("submodule", &["submodule", "status"], s).in_dir(SUB));
}

// ---------------------------------------------------------------------------
// submodule init / deinit
// ---------------------------------------------------------------------------

/// `init` and `deinit`: the two verbs whose whole effect is in `.git/config`.
///
/// The runner's `config --list --local` probe is the assertion for both —
/// `init` must leave `submodule.sub.url`/`.active` in place and `deinit` must
/// remove them *and* clear the worktree, which the `status --porcelain` probe
/// then reports.
///
/// `deinit` with neither `--all` nor a pathspec is a refusal that matters: it is
/// the guard against a script deinitialising a repository it only meant to
/// inspect, and it is spelled as a `fatal:` rather than a usage error. `--all`
/// together with a pathspec is the opposite refusal, spelled as a usage error
/// with exit 129. Both are strict, because the message is the contract.
///
/// `-f` is not reachable on its interesting path here: the fixture's submodule is
/// clean, so `deinit sub` succeeds without it and `-f` is measured only as "does
/// not change a clean removal". Stated rather than skipped, so the gap is not
/// mistaken for coverage.
fn init_deinit(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    out.push(Case::new("submodule", &["submodule", "init", "--"], s));
    out.push(Case::new("submodule", &["submodule", "init", "--", SUB], s));
    out.push(Case::new("submodule", &["submodule", "init"], s).in_dir("src"));
    out.push(Case::new("submodule", &["submodule", "init"], s).in_dir(SUB));
    out.push(Case::strict("submodule", &["submodule", "init", "no-such-path"], s));

    out.push(Case::new("submodule", &["submodule", "deinit", "-f", SUB], s));
    out.push(Case::new("submodule", &["submodule", "deinit", "--force", "--all"], s));
    out.push(Case::new("submodule", &["submodule", "deinit", "--quiet", "--all"], s));
    out.push(Case::strict("submodule", &["submodule", "deinit"], s));
    out.push(Case::strict("submodule", &["submodule", "deinit", "--all", SUB], s));
    out.push(Case::strict("submodule", &["submodule", "deinit", "no-such-path"], s));
    // No submodules to deinitialise from in here: `--all` over an empty set.
    out.push(Case::new("submodule", &["submodule", "deinit", "--all"], s).in_dir(SUB));
}

// ---------------------------------------------------------------------------
// submodule update
// ---------------------------------------------------------------------------

/// `update`: the verb with the most flags and the most ways to do nothing.
///
/// On this fixture the submodule is already at the recorded commit, so the
/// checkout half of `update` is a no-op and the `ls-files --stage` probe must
/// still show `160000 7c9f5d7e… sub` afterwards. That makes the *flag parsing*
/// the measured surface for most rows, with three exceptions that reach real
/// behaviour:
///
///  * `--force` re-checks out and prints
///    `Submodule path 'sub': checked out '7c9f5d7e…'` where the plain form prints
///    nothing. A port that treats `--force` as a synonym of the default prints
///    nothing and fails on stdout.
///  * `--remote` fetches, and is refused by the front-end for the protocol reason
///    in the module header. Both the refusal and the `protocol.file.allow=always`
///    success are pinned.
///  * `submodule.<name>.update=none` makes stock skip the submodule entirely
///    (`Skipping submodule 'sub'` on stderr, exit 0); `=rebase` selects a
///    different update strategy for the same no-op.
///
/// `--merge --rebase` together is accepted by stock 2.55.0 rather than refused —
/// pinned as observed, not as expected.
fn update(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    // Strategy and force.
    out.push(Case::new("submodule", &["submodule", "update", "--force"], s));
    out.push(Case::new("submodule", &["submodule", "update", "--rebase"], s));
    out.push(Case::new("submodule", &["submodule", "update", "--merge"], s));
    out.push(Case::new("submodule", &["submodule", "update", "--merge", "--rebase"], s));
    out.push(Case::new("submodule", &["submodule", "update", "--no-fetch"], s));
    out.push(Case::new("submodule", &["submodule", "update", "--depth", "1"], s));
    out.push(Case::new("submodule", &["submodule", "update", "--single-branch"], s));
    out.push(Case::new("submodule", &["submodule", "update", "--", SUB], s));
    out.push(Case::strict("submodule", &["submodule", "update", "no-such-path"], s));

    // `--remote`: the fetch the front-end is not allowed to make. `misc_commands`
    // already pins the bare refusal; these pin the two config values that decide
    // it, and the successful fetch on the far side of it.
    out.push(Case::strict("submodule", &["submodule", "update", "--remote"], s)
        .with_config(&[("protocol.file.allow", "user")]));
    out.push(Case::new("submodule", &["submodule", "update", "--remote"], s)
        .with_config(&[("protocol.file.allow", "always")]));
    out.push(Case::new("submodule", &["submodule", "update", "--remote", "--recursive"], s)
        .with_config(&[("protocol.file.allow", "always")]));
    out.push(Case::new("submodule", &["submodule", "update", "--remote", "--no-fetch"], s));
    out.push(Case::new("submodule", &["submodule", "update", "--remote"], s).with_config(&[
        ("submodule.fetchJobs", "1"),
        ("protocol.file.allow", "always"),
    ]));
    out.push(Case::new("submodule", &["submodule", "update", "--remote"], s).with_config(&[
        ("submodule.sub.branch", "main"),
        ("protocol.file.allow", "always"),
    ]));

    // Per-submodule policy.
    out.push(Case::new("submodule", &["submodule", "update"], s)
        .with_config(&[("submodule.sub.update", "none")]));
    out.push(Case::new("submodule", &["submodule", "update"], s)
        .with_config(&[("submodule.sub.update", "rebase")]));
    out.push(Case::new("submodule", &["submodule", "update"], s)
        .with_config(&[("submodule.sub.active", "false")]));
    out.push(Case::new("submodule", &["submodule", "update"], s)
        .with_config(&[("fetch.recurseSubmodules", "yes")]));

    // From a subdirectory, and from inside the submodule (which has none).
    out.push(Case::new("submodule", &["submodule", "update"], s).in_dir("src"));
    out.push(Case::new("submodule", &["submodule", "update"], s).in_dir(SUB));
}

// ---------------------------------------------------------------------------
// submodule summary
// ---------------------------------------------------------------------------

/// `summary`: a diff of the *gitlink*, rendered as the submodule's own log.
///
/// The commit argument is what makes this reachable. Against `HEAD` there is no
/// change to the gitlink and stock prints nothing, which is what
/// `misc_commands.rs` already measures; against `HEAD~1` — the commit before the
/// submodule existed — stock prints
///
/// ```text
/// * sub 0000000...7c9f5d7 (1):
///   > submodule initial
/// ```
///
/// which requires reading a commit out of `.git/modules/sub` while standing in
/// the parent. A port that never opens the submodule's object store prints the
/// header and no `>` line.
///
/// `-n` (the short spelling of `--summary-limit`) caps the number of `>` lines;
/// `-n 0` suppresses the commit list and `-n 1` admits exactly the one row that
/// exists, so the two rows separate "reads the cap" from "ignores it".
/// `--files --cached` is the documented incompatibility and is strict.
fn summary(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    out.push(Case::new("submodule", &["submodule", "summary", "HEAD~1"], s));
    out.push(Case::new("submodule", &["submodule", "summary", "--cached", "HEAD~1"], s));
    out.push(Case::new("submodule", &["submodule", "summary", "--files", "HEAD~1"], s));
    out.push(Case::new("submodule", &["submodule", "summary", "-n", "1", "HEAD~1"], s));
    out.push(Case::new("submodule", &["submodule", "summary", "-n", "0", "HEAD~1"], s));
    out.push(Case::new("submodule", &["submodule", "summary", "--", SUB], s));
    out.push(Case::strict("submodule", &["submodule", "summary", "--files", "--cached"], s));
    // The prefix rewrite reaches the summary header too: `* ../sub 0000000...`.
    out.push(Case::new("submodule", &["submodule", "summary", "HEAD~1"], s).in_dir("src"));
}

// ---------------------------------------------------------------------------
// submodule foreach
// ---------------------------------------------------------------------------

/// `foreach`: a shell run once per submodule, with five variables exported and
/// the child's exit status escalated to a `fatal:`.
///
/// `misc_commands.rs` already prints all five variables in one line; this group
/// separates the ones a port is likely to get individually wrong:
///
///  * `$displaypath` moves the moment the command is run from a subdirectory —
///    `../sub` where `$sm_path` stays `sub` — so it is pinned from `src/`, where
///    the two disagree, as well as from the root where they do not.
///  * `$toplevel` is the superproject's absolute worktree, printed while the
///    child's own cwd is the submodule.
///  * `$sha1` is the *recorded* gitlink oid, not the submodule's `HEAD`.
///
/// The child's environment is the other half. `git rev-parse --git-dir` inside
/// the loop must answer `<REPO>/.git/modules/sub`, and
/// `git config --get core.worktree` must answer `../../../sub`: both are read out
/// of the submodule's own config, and a port that runs the child with the
/// parent's `GIT_DIR` still exported answers the parent's.
///
/// A command that exits non-zero is the refusal that matters —
/// `fatal: run_command returned non-zero status for sub` with exit 128, not the
/// child's own 3 — and is strict.
fn foreach(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    // No command at all: stock still prints the `Entering` line and exits 0.
    out.push(Case::new("submodule", &["submodule", "foreach"], s));
    out.push(Case::strict("submodule", &["submodule", "foreach", "exit 3"], s));

    out.push(Case::new("submodule", &["submodule", "foreach", "echo $displaypath"], s));
    out.push(Case::new("submodule", &["submodule", "foreach", "echo $toplevel"], s));
    out.push(Case::new("submodule", &["submodule", "foreach", "--quiet", "echo $sha1"], s));
    out.push(Case::new("submodule", &["submodule", "foreach", "git", "rev-parse", "--git-dir"], s));
    out.push(Case::new("submodule", &["submodule", "foreach", "git", "rev-parse", "--show-toplevel"], s));
    out.push(Case::new("submodule", &["submodule", "foreach", "git config --get core.worktree"], s));
    out.push(Case::new("submodule", &["submodule", "foreach", "--recursive", "git rev-parse HEAD"], s));

    // Prefix: `Entering '../sub'` and `$displaypath` of `../sub`.
    out.push(Case::new("submodule", &["submodule", "foreach", "echo $displaypath"], s).in_dir("src"));
    // No submodules in here: the loop body never runs.
    out.push(Case::new("submodule", &["submodule", "foreach", "true"], s).in_dir(SUB));
}

// ---------------------------------------------------------------------------
// submodule sync / set-url / set-branch
// ---------------------------------------------------------------------------

/// The three verbs that write configuration rather than the worktree.
///
/// `sync` copies `.gitmodules`'s url into `.git/config` *and* into the
/// submodule's own `remote.origin.url`; the runner's `config --list --local`
/// probe sees the first and nothing sees the second, so the assertion here is the
/// stdout line plus the parent's config staying intact.
///
/// `set-url` and `set-branch` write `.gitmodules` in the **worktree**, so their
/// effect shows up in the `status --porcelain` probe as a modified `.gitmodules`
/// — a port that writes to `.git/config` instead leaves the worktree clean and
/// fails on state while agreeing on stdout. Both take `--` before the path.
///
/// Four refusals are the contract and are strict: an unknown path is
/// `fatal: no submodule mapping found in .gitmodules for path '…'` from
/// `builtin/submodule--helper.c` for `set-url` and `set-branch` alike, `sync` of
/// an unknown path is `error: pathspec '…' did not match any file(s) known to
/// git` with exit 1, and `set-branch` with neither `--branch` nor `--default` is
/// `fatal: --branch or --default required`.
///
/// `set-branch --default sub` exits 1 with no output at all on this fixture,
/// because there is no `submodule.sub.branch` to unset and the underlying
/// `config --unset` returns "key not found". `misc_commands.rs` already pins it;
/// the short spelling `-d` is pinned here to prove the two are the same option.
///
/// New urls are relative on purpose. An absolute one would be this side's own
/// path, and the two sides live at different roots.
fn sync_set_url_branch(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    out.push(Case::new("submodule", &["submodule", "sync", "--quiet"], s));
    out.push(Case::new("submodule", &["submodule", "sync", "--", SUB], s));
    out.push(Case::strict("submodule", &["submodule", "sync", "no-such-path"], s));
    out.push(Case::new("submodule", &["submodule", "sync"], s).in_dir("src"));

    out.push(Case::new("submodule", &["submodule", "set-url", "--", SUB, "./relative-upstream"], s));
    out.push(Case::strict(
        "submodule",
        &["submodule", "set-url", "no-such-path", "./relative-upstream"],
        s,
    ));

    out.push(Case::new("submodule", &["submodule", "set-branch", "-b", "topic", SUB], s));
    out.push(Case::new("submodule", &["submodule", "set-branch", "--branch", "main", "--", SUB], s));
    out.push(Case::new("submodule", &["submodule", "set-branch", "-d", SUB], s));
    out.push(Case::strict("submodule", &["submodule", "set-branch", SUB], s));
    out.push(Case::strict("submodule", &["submodule", "set-branch", "--branch", "topic", "no-such-path"], s));
}

// ---------------------------------------------------------------------------
// submodule absorbgitdirs
// ---------------------------------------------------------------------------

/// `absorbgitdirs`: move a submodule's `.git` directory into
/// `.git/modules/<name>` and leave a `.git` file behind.
///
/// The fixture is already absorbed — `submodule add` has produced that layout
/// since git 1.7.8 — so there is nothing to move and the contract is that it is a
/// *silent success*, not a warning and not an error. That is worth pinning on its
/// own: a port that reports "nothing to do" on stdout diverges, and one that
/// errors because `sub/.git` is not a directory diverges harder.
///
/// The un-absorbed state cannot be built from a case (one argv, pristine copy),
/// so the moving half of this verb is unmeasured here and is stated rather than
/// approximated.
fn absorbgitdirs(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    out.push(Case::new("submodule", &["submodule", "absorbgitdirs", SUB], s));
    out.push(Case::new("submodule", &["submodule", "absorbgitdirs", "--", SUB], s));
    out.push(Case::strict("submodule", &["submodule", "absorbgitdirs", "no-such-path"], s));
    out.push(Case::new("submodule", &["submodule", "absorbgitdirs"], s).in_dir("src"));
}

// ---------------------------------------------------------------------------
// submodule add
// ---------------------------------------------------------------------------

/// `add`: the only verb that creates a submodule, and the only one that can reach
/// the network.
///
/// Split deliberately into two halves.
///
/// **Refusals that fire before any transport.** The index check comes first, so
/// an `https://` url paired with a path already in the index never resolves a
/// host — verified against stock, which exits 128 with
/// `fatal: 'sub' already exists in the index` and no DNS traffic. These are
/// strict: the message names the path, and a port that clones first and checks
/// afterwards both hangs on DNS and leaves a directory behind. `.gitmodules`
/// stands in for a tracked non-submodule path, and `./no-such-repo` for a local
/// source that is not a repository (`fatal: repository '<REPO>/no-such-repo' does
/// not exist`, path masked by `runner::normalize`).
///
/// **A real add.** `./sub` — the fixture's own submodule checkout — is a working
/// local repository, so `add ./sub other` clones it and produces a second gitlink.
/// It needs `protocol.file.allow=always` because of `GIT_PROTOCOL_FROM_USER=0`
/// (module header); the setting is delivered on the command line to both sides
/// identically, so it is part of the invocation rather than a thumb on the scale.
/// The alternative — pinning only the refusal — would leave `add`'s entire
/// success path unmeasured, and that path is where `.gitmodules` gets written, the
/// gitlink gets staged, and `submodule.<name>.url` gets resolved to an absolute
/// path in `.git/config`. All three are in the runner's state probe.
///
/// `--depth 1` over a local source additionally emits
/// `warning: --depth is ignored in local clones`, so it is not strict.
fn add(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    const REMOTE: &str = "https://example.invalid/x.git";

    out.push(Case::strict("submodule", &["submodule", "add", REMOTE, SUB], s));
    out.push(Case::strict("submodule", &["submodule", "add", REMOTE, ".gitmodules"], s));
    out.push(Case::strict("submodule", &["submodule", "add", "./no-such-repo", "other"], s));
    // The protocol refusal, with and without the policy that would lift it.
    out.push(Case::strict("submodule", &["submodule", "add", "./sub", "other"], s));
    out.push(Case::strict("submodule", &["submodule", "add", "./sub", "other"], s)
        .with_config(&[("protocol.file.allow", "user")]));

    let allow = &[("protocol.file.allow", "always")];
    out.push(Case::new("submodule", &["submodule", "add", "./sub", "other"], s).with_config(allow));
    out.push(Case::new("submodule", &["submodule", "add", "-b", "main", "./sub", "other"], s).with_config(allow));
    out.push(
        Case::new("submodule", &["submodule", "add", "--name", "renamed", "./sub", "other"], s)
            .with_config(allow),
    );
    out.push(Case::new("submodule", &["submodule", "add", "--depth", "1", "./sub", "other"], s).with_config(allow));
    out.push(Case::strict("submodule", &["submodule", "add", "./sub", SUB], s).with_config(allow));
}

// ---------------------------------------------------------------------------
// submodule--helper: the surface it still exposes
// ---------------------------------------------------------------------------

/// `submodule--helper`'s subcommand table in 2.55.0, established by probing every
/// name the command has ever carried rather than by assuming.
///
/// Still dispatched: `clone`, `add`, `update`, `foreach`, `init`, `status`,
/// `sync`, `deinit`, `summary`, `push-check`, `absorbgitdirs`, `set-url`,
/// `set-branch`, `create-branch`.
///
/// **Gone**, and answering `error: unknown subcommand: '<name>'` with exit 129:
/// `config`, `list`, `name`, `is-active`, `check-name`, `resolve-relative-url`,
/// `resolve-relative-url-test`, `relative-path`, `ensure-core-worktree`,
/// `print-default-remote`, `update-clone`, `run-update-procedure`. This surface
/// has shrunk every few releases as `git-submodule.sh` shed shell code, so a port
/// written against an older `builtin/submodule--helper.c` answers *something* for
/// names stock now rejects. Two of the removed names are pinned strictly — one
/// bare, one with an argument, since a port that dispatches on argument count
/// would answer differently for the two — and the rest share the identical
/// answer and would be duplicates.
///
/// `clone`, `create-branch` and `set-url` with no arguments print their own usage
/// blocks — three different `parse_options` tables, none of them the front-end's —
/// so they are the cheapest proof that the helper parses for itself.
/// `push-check` has no front-end spelling at all and is pinned on both its
/// arity refusal and its "remote not configured" refusal.
fn helper_surface(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    out.push(Case::strict("submodule--helper", &["submodule--helper", "clone"], s));
    out.push(Case::strict("submodule--helper", &["submodule--helper", "create-branch"], s));
    out.push(Case::strict("submodule--helper", &["submodule--helper", "set-url"], s));
    out.push(Case::strict("submodule--helper", &["submodule--helper", "set-branch"], s));
    out.push(Case::strict("submodule--helper", &["submodule--helper", "push-check"], s));
    out.push(Case::strict(
        "submodule--helper",
        &["submodule--helper", "push-check", "HEAD", "refs/heads/main"],
        s,
    ));
    out.push(Case::strict("submodule--helper", &["submodule--helper", "deinit"], s));
    out.push(Case::strict("submodule--helper", &["submodule--helper", "init", "no-such-path"], s));
    out.push(Case::strict("submodule--helper", &["submodule--helper", "absorbgitdirs", "no-such-path"], s));
    out.push(Case::strict("submodule--helper", &["submodule--helper", "summary", "--files", "--cached"], s));
    out.push(Case::strict("submodule--helper", &["submodule--helper", "foreach", "exit 3"], s));

    // Names the helper no longer answers for.
    out.push(Case::strict("submodule--helper", &["submodule--helper", "list"], s));
    out.push(Case::strict("submodule--helper", &["submodule--helper", "resolve-relative-url", "./x"], s));
    out.push(Case::new("submodule--helper", &["submodule--helper", "status", "--quiet"], s));
    out.push(Case::new("submodule--helper", &["submodule--helper", "summary", "--cached", "HEAD~1"], s));
    out.push(Case::new("submodule--helper", &["submodule--helper", "foreach"], s));
    out.push(Case::new(
        "submodule--helper",
        &["submodule--helper", "foreach", "--recursive", "echo $displaypath"],
        s,
    ));
    out.push(Case::new("submodule--helper", &["submodule--helper", "init", SUB], s));
    out.push(Case::new("submodule--helper", &["submodule--helper", "deinit", SUB], s));
    out.push(Case::new("submodule--helper", &["submodule--helper", "set-branch", "--branch", "main", SUB], s));
    out.push(Case::new("submodule--helper", &["submodule--helper", "set-url", SUB, "./relative-upstream"], s));
}

// ---------------------------------------------------------------------------
// The front-end is not the helper
// ---------------------------------------------------------------------------

/// The rows that separate `git submodule <cmd>` from `git submodule--helper <cmd>`
/// on identical arguments.
///
/// Both differences come from `git-submodule.sh` and neither is visible from
/// either spelling alone:
///
///  * **Protocol policy.** `git-submodule.sh:29-30` exports
///    `GIT_PROTOCOL_FROM_USER=0`, so the front-end's fetch and clone of the
///    fixture's local url are refused while the helper's succeed. The matrix in
///    the module header is what these rows pin; the `=never` row is included so
///    the helper is shown to be *subject* to the policy rather than exempt from
///    it, which is what distinguishes "runs as the user" from "ignores
///    `protocol.file`".
///  * **Prefix handling.** `git-submodule.sh:24-25` computes the prefix and
///    re-enters at the toplevel with `-C`; the helper is handed the same `-C` by
///    the script but computes nothing when called directly. Both spellings land
///    on `../sub` from `src/` here, which is the answer a port must reproduce
///    through two different routes.
///
/// A port that implements `submodule` by forwarding to its own
/// `submodule--helper` passes every helper row and fails the front-end's refusals
/// — which is the intended detection, since that forwarding is precisely the
/// shortcut the shell script does not take.
fn helper_vs_front_end(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    // The helper runs as the user: allowed with no policy, and with `=user`.
    out.push(Case::new("submodule--helper", &["submodule--helper", "update", "--remote"], s));
    out.push(Case::new("submodule--helper", &["submodule--helper", "update", "--remote"], s)
        .with_config(&[("protocol.file.allow", "user")]));
    // ...and still subject to the policy when it forbids the transport outright.
    out.push(Case::strict("submodule--helper", &["submodule--helper", "update", "--remote"], s)
        .with_config(&[("protocol.file.allow", "never")]));
    out.push(Case::strict("submodule--helper", &["submodule--helper", "add", "./sub", "other"], s)
        .with_config(&[("protocol.file.allow", "never")]));
    // The clone the front-end refuses.
    out.push(Case::new("submodule--helper", &["submodule--helper", "add", "./sub", "other"], s));

    // The prefix, through the helper.
    out.push(Case::new("submodule--helper", &["submodule--helper", "status"], s).in_dir("src"));
    out.push(Case::new(
        "submodule--helper",
        &["submodule--helper", "foreach", "echo $displaypath"],
        s,
    )
    .in_dir("src"));
    out.push(Case::new("submodule--helper", &["submodule--helper", "summary", "HEAD~1"], s).in_dir("src"));
}

// ---------------------------------------------------------------------------
// Ordinary verbs from inside the submodule
// ---------------------------------------------------------------------------

/// The `.git`-file indirection, asked by commands that are not `submodule`.
///
/// `sub/.git` is a *file* containing `gitdir: ../.git/modules/sub`
/// (`setup.c:read_gitfile_gently`). A port that only knows `.git`-as-a-directory
/// walks past it and finds the superproject, and then answers every question
/// below about the wrong repository — with plausible-looking output, which is why
/// this needs pinning rather than trusting.
///
/// `corpus/discovery.rs:330` already runs the twelve `rev-parse` discovery columns
/// plus `status`, `log` and `symbolic-ref` from `sub`. What is added here is the
/// set of answers that come from the submodule's *contents and configuration*
/// rather than from discovery alone, and one column discovery does not carry:
///
///  * `--show-superproject-working-tree` — the reverse link, and the only query
///    whose answer is a *different* repository. It prints `<REPO>` from `sub` and
///    nothing at all from the parent, so both directions are pinned; the path is
///    masked by `runner::normalize`.
///  * `--git-path config` and `--resolve-git-dir .git` — the two spellings that
///    make the indirection explicit rather than incidental. `--resolve-git-dir`
///    is handed the gitfile *by name*, so it cannot fall back on discovery.
///  * `config --list --local` — proves `core.worktree = ../../../sub` was written
///    and is being read from `.git/modules/sub/config`, not from the parent's.
///    This row prints `remote.origin.url`, so it carries the run-stable upstream
///    path noted in the module header.
///  * `ls-files --stage` and `for-each-ref` — the index and the refs are the
///    submodule's own (`mod.txt`, `refs/remotes/origin/*`), and share not one
///    entry with the parent's.
fn inside_the_submodule(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    out.push(Case::new("rev-parse", &["rev-parse", "--show-superproject-working-tree"], s).in_dir(SUB));
    // From the parent: no superproject, zero bytes, exit 0.
    out.push(Case::new("rev-parse", &["rev-parse", "--show-superproject-working-tree"], s));
    out.push(Case::new("rev-parse", &["rev-parse", "--git-path", "config"], s).in_dir(SUB));
    out.push(Case::new("rev-parse", &["rev-parse", "--resolve-git-dir", ".git"], s).in_dir(SUB));
    out.push(Case::new("config", &["config", "--list", "--local"], s).in_dir(SUB));
    out.push(Case::new("ls-files", &["ls-files", "--stage"], s).in_dir(SUB));
    out.push(Case::new("for-each-ref", &["for-each-ref"], s).in_dir(SUB));

    // The submodule's *git directory*, where `git submodule` itself refuses:
    // `git-submodule.sh:22` calls `require_work_tree`, and the cwd is inside the
    // git dir. Not strict — the message names the exec-path of the shell script.
    out.push(Case::new("submodule", &["submodule", "status"], s).in_dir(".git/modules/sub"));
    out.push(
        Case::new("rev-parse", &["rev-parse", "--show-superproject-working-tree"], s)
            .in_dir(".git/modules/sub"),
    );
}

// ---------------------------------------------------------------------------
// The `-C sub` spelling
// ---------------------------------------------------------------------------

/// The same questions asked with `git -C sub …` instead of by running in `sub`.
///
/// Stock reaches an identical answer by an entirely different route: `-C` is
/// handled in `git.c:handle_options` and chdirs *before* `setup_git_directory`
/// runs, so the two spellings converge only if the port's global-option handling
/// and its process working directory feed the same discovery. A port that
/// resolves `-C` after deciding on a repository — or that special-cases the
/// startup cwd and not the post-`-C` one — answers the superproject here and the
/// submodule in `inside_the_submodule`, or the reverse.
///
/// Only the columns that are distinctive are spelled out: the git dir (the
/// indirection's result), the toplevel and superproject (the two ends of the
/// link), the local config (the submodule's own, carrying `core.worktree`), the
/// index (`mod.txt`, not the parent's four entries), `status --porcelain` (which
/// must read the submodule's index against the submodule's `HEAD`), and
/// `submodule status` (which must find *no* submodules, not the parent's one).
fn dash_c_spelling(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    let c: &[&[&str]] = &[&["-C", "sub"]];
    out.push(Case::new("rev-parse", &["rev-parse", "--git-dir"], s).with_globals(c));
    out.push(Case::new("rev-parse", &["rev-parse", "--show-toplevel"], s).with_globals(c));
    out.push(Case::new("rev-parse", &["rev-parse", "--show-superproject-working-tree"], s).with_globals(c));
    out.push(Case::new("status", &["status", "--porcelain"], s).with_globals(c));
    out.push(Case::new("config", &["config", "--list", "--local"], s).with_globals(c));
    out.push(Case::new("ls-files", &["ls-files", "--stage"], s).with_globals(c));
    out.push(Case::new("submodule", &["submodule", "status"], s).with_globals(c));
}

// ---------------------------------------------------------------------------
// GIT_DIR against a submodule
// ---------------------------------------------------------------------------

/// `GIT_DIR` pointed at the submodule, by both of its spellings.
///
/// `{repo}` is `runner::REPO_PLACEHOLDER`, replaced with the running side's own
/// fixture root; a literal path would name the other side's copy.
///
///  * `GIT_DIR={repo}/.git/modules/sub` from the parent's worktree root. Discovery
///    is skipped entirely, and the worktree comes from that directory's
///    `core.worktree` — so `--show-toplevel` answers `<REPO>/sub` while the process
///    is standing in `<REPO>`. `git submodule` then refuses, because
///    `git-submodule.sh:22`'s `require_work_tree` asks whether the *cwd* is inside
///    the worktree and it is not. The refusal exits 1, not 128, and prints
///    nothing on stdout.
///  * `GIT_DIR=.git` from inside `sub`. The value names the gitfile, not a
///    directory, and stock resolves it through
///    `setup.c:read_gitfile_gently` to `<REPO>/.git/modules/sub` — the one case
///    where the indirection is reached through the environment rather than
///    through discovery, and the one a port is most likely to `stat` and reject.
///  * `GIT_WORK_TREE` alongside `GIT_DIR`, which is the spelling that makes the
///    pair self-consistent and must therefore *not* refuse.
fn explicit_git_dir(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    let sub_dir: &[(&str, &str)] = &[("GIT_DIR", "{repo}/.git/modules/sub")];
    out.push(Case::new("rev-parse", &["rev-parse", "--show-toplevel"], s).with_env(sub_dir));
    out.push(Case::new("rev-parse", &["rev-parse", "--absolute-git-dir"], s).with_env(sub_dir));
    out.push(Case::new("status", &["status", "--porcelain"], s).with_env(sub_dir));
    out.push(Case::new("log", &["log", "--oneline", "-1"], s).with_env(sub_dir));
    out.push(Case::new("submodule", &["submodule", "status"], s).with_env(sub_dir));

    out.push(Case::new("status", &["status", "--porcelain"], s).with_env(&[
        ("GIT_DIR", "{repo}/.git/modules/sub"),
        ("GIT_WORK_TREE", "{repo}/sub"),
    ]));

    // The gitfile, named explicitly.
    out.push(
        Case::new("rev-parse", &["rev-parse", "--git-dir"], s)
            .in_dir(SUB)
            .with_env(&[("GIT_DIR", ".git")]),
    );
}
