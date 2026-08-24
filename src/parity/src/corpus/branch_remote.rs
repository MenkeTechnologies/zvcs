//! Differential corpus cases for `branch`, `remote` and `push`.
//!
//! These three verbs are one subsystem because they write the same two places
//! and nothing else: the ref store, and the `branch.*`/`remote.*` stanzas of
//! `.git/config`. Both are read back by `runner::probe_state` — `for-each-ref`
//! proves which ref a command created, moved or deleted, and
//! `config --list --local` proves an upstream or a refspec was actually
//! persisted. A port that prints stock's success line and writes neither is
//! caught only there, which is why almost every mutating case below is chosen
//! for what it leaves behind rather than for what it prints.
//!
//! # Which fixture, and why it has to be that one
//!
//! * [`Shape::BehindRemote`] is the only shape with a real remote. Its `origin`
//!   is the bare `./.remote.git` *inside* the fixture, reached by a relative
//!   URL, so each case gets its own private peer and no case can see another's
//!   writes. What that peer contains is load-bearing for every push case here:
//!   `refs/heads/main` at the tip of three commits the local `main` does not
//!   have, and `refs/heads/div` one commit sideways from the local `div`. So
//!   `main` is *behind* and `div` has *diverged* — pushing either without
//!   `--force` is a rejection, and that is the state the force/lease/atomic
//!   cases are measured against. It carries no tags, so `--tags` legitimately
//!   reports "Everything up-to-date" and `--follow-tags` has nothing to follow;
//!   both are still worth pinning, because reaching that answer means having
//!   walked the tag set rather than skipped it.
//! * The peer's own `HEAD` is unborn — `fixture.rs` builds it with
//!   `init --bare .remote.git` and pushes branches into it, never setting its
//!   `HEAD` — which is what makes `remote set-head -a` reach its
//!   "Cannot determine remote HEAD" path rather than succeeding.
//! * [`Shape::Branched`] has two branches, a lightweight and an annotated tag,
//!   and *no* remote. That is what makes it the shape for the unmerged-delete
//!   refusal (`feature` is not merged into `main`), for creating a branch off a
//!   tag, and for a `push` whose remote is spelled entirely on the command line.
//! * [`Shape::Worktree`] has `linked` checked out in `wt/`. `branch -d`,
//!   `branch -D` and `branch -M` all consult the worktree list before touching a
//!   ref (`builtin/branch.c` calls `branch_checked_out()`), so this is the only
//!   shape where those refusals are reachable at all.
//! * [`Shape::Octopus`] has five branches with one merge that reaches three of
//!   them and one branch (`oct-side`) it does not, so `--merged`/`--no-merged`/
//!   `--contains` partition into two non-empty sets. On [`Shape::Branched`]
//!   `--no-contains main` is empty and a port that answered "nothing" to every
//!   reachability question would score a match.
//!
//! # Determinism notes
//!
//! * `env::harden` pins `GIT_COMMITTER_DATE`, so every commit in every fixture
//!   shares one committer timestamp. `--sort=committerdate` is therefore a total
//!   tie, and what it measures is the *fallback*: `ref_sorting` appends refname
//!   as the last key (`ref-filter.c`), so the answer must equal `--sort=refname`.
//!   That is a real contract — a port that returns unsorted order on a tie
//!   diverges — but it is not a date comparison, and no case here pretends it is.
//! * Refusals that name a worktree print an absolute path. `runner::normalize`
//!   rewrites the fixture root to `<REPO>` on both sides, so those messages
//!   compare byte for byte and the cases can be `Case::strict`.
//! * Every URL is either `{repo}`-free and relative (`./.remote.git`, `.`,
//!   `./other.git`) or `https://example.invalid/…`. Nothing here resolves a
//!   hostname or leaves the fixture.
//!
//! # What is not measured here
//!
//! * **The peer's contents after a push.** `probe_state` walks the fixture, and
//!   `.remote.git` is inside it — but it is masked from `status` by
//!   `.git/info/exclude` and is not a worktree path, so what the probe compares
//!   is the *local* effect: the remote-tracking refs `push` writes on success,
//!   and the ones it deletes on `--prune`. A push that reported success while
//!   updating nothing on the peer, and skipped its own tracking-ref update, would
//!   score a match. Closing that needs a probe that recurses into nested
//!   repositories.
//! * **`push.autoSetupRemote` on a branch that has an upstream to lose.** Both
//!   branches of [`Shape::BehindRemote`] are already tracking, and a case cannot
//!   unset config before running. The one case below reaches it from
//!   [`Shape::Branched`] with the remote defined on the command line, which is
//!   the only fixture where a branch has no upstream *and* a remote exists.
//! * **`remote show` against a remote that is slow or unreachable.** Every URL
//!   here answers instantly or fails instantly; timeouts are out of scope.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    branch_listing(out);
    branch_sort_and_format(out);
    branch_reachability(out);
    branch_create(out);
    branch_copy_rename(out);
    branch_delete(out);
    branch_tracking(out);
    branch_config(out);
    remote_add(out);
    remote_rename_remove(out);
    remote_urls_and_branches(out);
    remote_head_and_query(out);
    remote_prune_update(out);
    push_refspecs(out);
    push_force(out);
    push_bulk_and_flags(out);
    push_defaults(out);
}

// ---------------------------------------------------------------------------
// branch
// ---------------------------------------------------------------------------

/// Listing, on the three shapes the base corpus's `read_only` set never reaches.
///
/// The base corpus runs `branch`, `branch --list`, `branch -a` and
/// `branch --show-current` across Linear/Branched/Merged/Dirty/Detached — none of
/// which has a remote-tracking ref, a linked worktree, or more than two branches.
/// So the three columns `branch -vv` is built from were all trivially empty: the
/// upstream column, the ahead/behind column, and the worktree marker. Each case
/// here makes exactly one of them non-empty.
fn branch_listing(out: &mut Vec<Case>) {
    // `-v` is subject + abbreviated oid; `-vv` adds `[upstream: ahead N, behind M]`.
    // On BehindRemote the two differ, which is the whole point: `main` is behind 3
    // and `div` is ahead 1 behind 1, so a port that prints the upstream name but
    // computes no counts diverges on `-vv` while matching `-v`.
    out.push(Case::new("branch", &["branch", "-v"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "-vv"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "-vv"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "-a"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "-r"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "-r", "-v"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "--all"], Shape::BehindRemote));
    // No remote at all: `-r` must print nothing and exit 0, not error.
    out.push(Case::new("branch", &["branch", "-r"], Shape::Branched));

    // The oid column's width is a flag, not a constant.
    out.push(Case::new("branch", &["branch", "-v", "--abbrev=12"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "-v", "--no-abbrev"], Shape::BehindRemote));

    // Pattern selection. `--list` with two patterns is a union, and `-i` folds
    // case — both are `ref-filter` behaviour reached only through `branch`.
    out.push(Case::new("branch", &["branch", "--list", "oct-*"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "--list", "main", "div"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "-a", "--list", "origin/*"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "-i", "--list", "OCT-*"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "--ignore-case", "--list", "MAIN"], Shape::BehindRemote));

    // Column layout. Not a tty on either side, so `term_columns()` falls back to
    // 80 and the layout is fixed; five branches is enough for `--column` to pack
    // them onto one row and for `--no-column` to be visibly different.
    out.push(Case::new("branch", &["branch", "--column"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "--no-column"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "--column=always,plain"], Shape::Octopus));

    // The linked worktree: `+` instead of `*` or a space, and the worktree's path
    // in the `-vv` line. `--show-current` is asked from both worktrees, because a
    // discovery path that reads the *common* HEAD answers `main` in both.
    out.push(Case::new("branch", &["branch", "-vv"], Shape::Worktree));
    out.push(Case::new("branch", &["branch", "--show-current"], Shape::Worktree));
    out.push(Case::new("branch", &["branch", "--show-current"], Shape::Worktree).in_dir("wt"));
    out.push(Case::new("branch", &["branch", "--list"], Shape::Worktree).in_dir("wt"));
}

/// `--sort` and `--format`: the two knobs that turn `branch` into a ref query.
///
/// Every prompt and CI script in existence is built from these, and they are the
/// part of `branch` that is not `branch` at all — `builtin/branch.c` hands the
/// whole job to `ref-filter.c`. Splitting them one atom per case means a failure
/// names the atom instead of "the formatter".
fn branch_sort_and_format(out: &mut Vec<Case>) {
    out.push(Case::new("branch", &["branch", "--sort=refname"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "--sort=-refname"], Shape::Octopus));
    // A total tie under the pinned clock; see the module header. What is being
    // compared is the refname fallback, not a date order.
    out.push(Case::new("branch", &["branch", "--sort=committerdate"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "--sort=-committerdate"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "--sort=version:refname"], Shape::Octopus));
    // Two keys: the last one given is the primary, the earlier one breaks ties.
    out.push(Case::new("branch", &["branch", "--sort=-committerdate", "--sort=refname"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "-r", "--sort=-refname"], Shape::BehindRemote));

    // The atoms that mean something to a *branch* specifically. `%(upstream)`
    // and `%(upstream:track)` read `branch.<name>.remote`/`.merge` and then walk
    // history for the counts; `%(push)` resolves the push refspec, which is a
    // different lookup that happens to agree here.
    out.push(Case::new("branch", &["branch", "--format=%(refname:short)"], Shape::Octopus));
    out.push(Case::new(
        "branch",
        &["branch", "--format=%(refname:short) %(upstream) %(upstream:track)"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "branch",
        &["branch", "--format=%(HEAD)%(refname:short) %(upstream:short) %(upstream:trackshort)"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "branch",
        &["branch", "--format=%(upstream:remotename) %(upstream:remoteref) %(push) %(push:track)"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "branch",
        &["branch", "-a", "--format=%(if)%(HEAD)%(then)*%(else)-%(end)%(refname) %(objectname:short)"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "branch",
        &["branch", "--format=%(objectname:short=10) %(refname:lstrip=2)"],
        Shape::Octopus,
    ));
    // `%(worktreepath)` is empty for every branch that is not checked out
    // anywhere, and an absolute path for the two that are.
    out.push(Case::new(
        "branch",
        &["branch", "--format=%(refname:short) %(worktreepath)"],
        Shape::Worktree,
    ));
}

/// `--contains`/`--merged`/`--points-at`: reachability, asked through `branch`.
///
/// On Octopus the answers partition five branches into non-empty sets in both
/// directions, so "always print everything" and "always print nothing" are both
/// wrong. `--merged` with no argument means HEAD, which is a separate default to
/// get right.
fn branch_reachability(out: &mut Vec<Case>) {
    out.push(Case::new("branch", &["branch", "--merged", "HEAD"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "--no-merged", "HEAD"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "--merged"], Shape::Merged));
    out.push(Case::new("branch", &["branch", "--no-merged", "main"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "--contains", "oct-a"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "--no-contains", "oct-a"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "--contains", "main"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "-a", "--contains", "HEAD"], Shape::BehindRemote));
    // `--points-at` takes a *tag* here, so the argument has to be peeled to a
    // commit before the comparison; `v0.2.0` is annotated and `v0.1.0` is not.
    out.push(Case::new("branch", &["branch", "--points-at", "v0.1.0"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "--points-at", "v0.2.0"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "--points-at", "HEAD"], Shape::Octopus));
    // Two filters at once must intersect, not replace one another.
    out.push(Case::new("branch", &["branch", "--merged", "HEAD", "--list", "oct-*"], Shape::Octopus));
}

/// Creation. The start point is the half a port most often ignores: it is a
/// revision, not a branch name, so a tag has to be peeled and `HEAD~1` resolved.
fn branch_create(out: &mut Vec<Case>) {
    out.push(Case::new("branch", &["branch", "topic", "HEAD~1"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "topic", "feature"], Shape::Branched));
    // An annotated tag: the new branch must point at the commit, not at the tag
    // object. `for-each-ref` in the probe prints the objecttype, so a branch left
    // pointing at a tag object is visible rather than merely wrong-looking.
    out.push(Case::new("branch", &["branch", "topic", "v0.2.0"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "topic", "origin/div"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "--create-reflog", "logged"], Shape::Branched));
    // `--force` over an existing branch that is *not* checked out: allowed, and
    // the ref must actually move.
    out.push(Case::new("branch", &["branch", "-f", "feature", "main"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "--force", "oct-side", "main"], Shape::Octopus));

    // Refusals. The first two are `check_refname_format()` rules quoted through
    // `strbuf_check_branch_ref()`; the third is the worktree gate, which `-f`
    // does not open.
    out.push(Case::strict("branch", &["branch", "bad..name"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "HEAD"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "-f", "main", "HEAD~1"], Shape::Branched));
}

/// `-c`/`-C`/`-m`/`-M`. Copy keeps the source, rename does not; both carry the
/// branch's config stanza and its reflog across, which is why the local config
/// in the post-state probe is the interesting half rather than stdout.
fn branch_copy_rename(out: &mut Vec<Case>) {
    out.push(Case::new("branch", &["branch", "-c", "feature", "copy"], Shape::Branched));
    // One-argument copy: source is the current branch.
    out.push(Case::new("branch", &["branch", "-c", "copy"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "-C", "oct-a", "oct-side"], Shape::Octopus));
    out.push(Case::new("branch", &["branch", "-m", "feature", "renamed"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "-M", "oct-side", "moved"], Shape::Octopus));
    // Renaming a *tracking* branch has to rewrite `branch.div.*` under the new
    // name, not leave an orphan stanza behind.
    out.push(Case::new("branch", &["branch", "-m", "div", "diverged"], Shape::BehindRemote));
    // Renaming the branch checked out in the linked worktree: allowed, and the
    // worktree's HEAD has to follow.
    out.push(Case::new("branch", &["branch", "-m", "linked", "relinked"], Shape::Worktree));
    // Renaming the current branch, one-argument form.
    out.push(Case::new("branch", &["branch", "-m", "trunk"], Shape::BehindRemote));

    // Refusals: an existing destination needs `-M`, and `-M` still cannot land on
    // a branch some worktree has checked out.
    out.push(Case::strict("branch", &["branch", "-m", "feature", "main"], Shape::Branched));
    out.push(Case::strict("branch", &["branch", "-c", "oct-a", "oct-b"], Shape::Octopus));
    out.push(Case::strict("branch", &["branch", "-M", "feature", "main"], Shape::Branched));
}

/// Deletion, which is mostly a list of things git refuses to do.
///
/// Each refusal is a different gate in `builtin/branch.c`: merged-ness
/// (`branch_merged()`), the worktree list (`branch_checked_out()`), and
/// existence. A port that collapses them into one "cannot delete" answer passes
/// none of these, and a port that skips them all destroys work silently.
fn branch_delete(out: &mut Vec<Case>) {
    out.push(Case::new("branch", &["branch", "-D", "feature"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "--delete", "--force", "feature"], Shape::Branched));
    out.push(Case::new("branch", &["branch", "-d", "oct-a"], Shape::Octopus));
    // Deleting a tracking branch must take `branch.div.*` with it.
    out.push(Case::new("branch", &["branch", "-D", "div"], Shape::BehindRemote));
    // Remote-tracking refs are deleted through the same verb with `-r`, and it
    // touches `refs/remotes/`, never `refs/heads/`.
    out.push(Case::new("branch", &["branch", "-d", "-r", "origin/div"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "-r", "-d", "origin/div", "origin/main"], Shape::BehindRemote));
    // Two names at once: both go, and the report is one line each.
    out.push(Case::new("branch", &["branch", "-D", "oct-a", "oct-b"], Shape::Octopus));

    // Refusals.
    out.push(Case::strict("branch", &["branch", "-d", "feature"], Shape::Branched));
    out.push(Case::strict("branch", &["branch", "-d", "oct-side"], Shape::Octopus));
    out.push(Case::strict("branch", &["branch", "-d", "main"], Shape::Branched));
    out.push(Case::strict("branch", &["branch", "-d", "linked"], Shape::Worktree));
    // `-D` does not open the worktree gate either — force overrides merged-ness
    // and nothing else.
    out.push(Case::strict("branch", &["branch", "-D", "linked"], Shape::Worktree));
    out.push(Case::strict("branch", &["branch", "-d", "-r", "origin/nope"], Shape::BehindRemote));
}

/// Upstream configuration: the `branch.<name>.remote` / `.merge` pair.
///
/// Nothing here prints more than one line, and the line is not the point — the
/// pair either lands in `.git/config` or it does not, and only
/// `config --list --local` in the post-state probe can tell. Five different
/// spellings write it and two remove it.
fn branch_tracking(out: &mut Vec<Case>) {
    out.push(Case::new("branch", &["branch", "--track", "topic", "origin/div"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "--no-track", "topic", "origin/div"], Shape::BehindRemote));
    // A *local* start point: remote becomes `.`, which is a different code path
    // in `install_branch_config()` than a remote-tracking one.
    out.push(Case::new("branch", &["branch", "-t", "topic", "main"], Shape::BehindRemote));
    // `inherit` copies the start point's own upstream instead of pointing at it.
    out.push(Case::new("branch", &["branch", "--track=inherit", "topic", "main"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "--set-upstream-to=origin/div", "main"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "-u", "origin/main", "div"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "--unset-upstream"], Shape::BehindRemote));
    out.push(Case::new("branch", &["branch", "--unset-upstream", "div"], Shape::BehindRemote));
    // The editor is `true`, so it exits 0 having written nothing: git reads back
    // an empty description and must *not* write `branch.main.description`.
    out.push(Case::new("branch", &["branch", "--edit-description"], Shape::Branched));

    // Refusals: an upstream that does not exist, and unsetting one that is not set.
    out.push(Case::strict("branch", &["branch", "--set-upstream-to=origin/nope", "main"], Shape::BehindRemote));
    out.push(Case::strict("branch", &["branch", "--unset-upstream"], Shape::Branched));
}

/// The configuration keys that change what `branch` does without any flag.
///
/// `branch.autoSetupMerge` decides whether a new branch tracks at all, and its
/// four values disagree with each other on the *same* invocation — `always`
/// tracks a local start point, `simple` tracks only a remote branch of the same
/// name, `inherit` copies the start point's upstream, `false` tracks nothing. A
/// port that treats the key as a boolean matches two of the four.
fn branch_config(out: &mut Vec<Case>) {
    let cfg = |key: &str, value: &str, args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("branch", args, Shape::BehindRemote).with_config(&[(key, value)]));
    };
    cfg("branch.autoSetupMerge", "always", &["branch", "topic", "main"], out);
    cfg("branch.autoSetupMerge", "inherit", &["branch", "topic", "main"], out);
    cfg("branch.autoSetupMerge", "false", &["branch", "topic", "origin/div"], out);
    cfg("branch.autoSetupMerge", "simple", &["branch", "topic", "origin/div"], out);
    cfg("branch.autoSetupRebase", "always", &["branch", "topic", "origin/div"], out);
    cfg("branch.autoSetupRebase", "remote", &["branch", "topic", "origin/div"], out);
    cfg("branch.sort", "-refname", &["branch", "-a"], out);
    cfg("branch.sort", "committerdate", &["branch", "--list"], out);

    // Colour: `color.branch=always` makes the current-branch marker and the
    // remote-tracking names carry SGR sequences, and `--no-color` must strip them
    // back out. Both halves are byte comparisons of escape sequences.
    out.push(Case::new("branch", &["branch", "-a"], Shape::BehindRemote).with_config(&[("color.branch", "always")]));
    out.push(
        Case::new("branch", &["branch", "--no-color", "-a"], Shape::BehindRemote)
            .with_config(&[("color.branch", "always")]),
    );
    out.push(Case::new("branch", &["branch", "-vv"], Shape::BehindRemote).with_config(&[("color.branch", "always")]));

    // `--no-advice` is a *global* option, and it removes the hint block from a
    // refusal without changing the error line or the exit code. Both refusals
    // below print a multi-line hint without it.
    out.push(Case::strict("branch", &["branch", "-d", "feature"], Shape::Branched).with_globals(&[&["--no-advice"]]));
    out.push(
        Case::strict("branch", &["branch", "--set-upstream-to=origin/nope", "main"], Shape::BehindRemote)
            .with_globals(&[&["--no-advice"]]),
    );
}

// ---------------------------------------------------------------------------
// remote
// ---------------------------------------------------------------------------

/// `remote add`: one command, seven different config stanzas.
///
/// The default is a url plus one glob fetch refspec. Every flag below rewrites
/// that stanza differently, and the difference is invisible on stdout — `add`
/// prints nothing at all except under `-f`. `config --list --local` in the
/// post-state probe is the entire measurement, which is what makes this group
/// worth its size: a port that accepts all seven flags and writes the default
/// stanza seven times scores 100% on stdout and 0% here.
fn remote_add(out: &mut Vec<Case>) {
    let add = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("remote", args, Shape::BehindRemote));
    };
    add(&["remote", "add", "up", "./.remote.git"], out);
    // `-t` narrows the refspec to one branch; twice, it writes two refspecs
    // rather than replacing the first.
    add(&["remote", "add", "-t", "main", "up", "./.remote.git"], out);
    add(&["remote", "add", "-t", "main", "-t", "div", "up", "./.remote.git"], out);
    // `--mirror=fetch` maps the whole ref namespace onto itself; `--mirror=push`
    // writes no refspec at all and sets `remote.up.mirror` instead.
    add(&["remote", "add", "--mirror=fetch", "up", "./.remote.git"], out);
    add(&["remote", "add", "--mirror=push", "up", "./.remote.git"], out);
    add(&["remote", "add", "--tags", "up", "./.remote.git"], out);
    add(&["remote", "add", "--no-tags", "up", "./.remote.git"], out);
    // A url that is not a path: nothing contacts it, so this measures only that
    // the stanza is written verbatim.
    add(&["remote", "add", "up", "https://example.invalid/r.git"], out);

    // `-f` fetches immediately, so this one *does* print, and it writes
    // `refs/remotes/up/*` for the probe to compare.
    add(&["remote", "add", "-f", "up", "./.remote.git"], out);
    // `-m` writes `refs/remotes/up/HEAD` as a symref. Alone it is a broken symref
    // that `for-each-ref` skips, so it is paired with `-f` — with the branches
    // fetched the symref resolves and the probe sees it.
    add(&["remote", "add", "-f", "-m", "div", "up", "./.remote.git"], out);
    add(&["remote", "add", "-f", "-t", "main", "up", "./.remote.git"], out);
    // A repository with no remotes at all, so `add` writes the first stanza.
    out.push(Case::new("remote", &["remote", "add", "origin", "./.remote.git"], Shape::Branched));

    // Refusal: the name is taken. Exit code 3, not 1 — `builtin/remote.c` uses
    // its own code for "already exists" and scripts branch on it.
    out.push(Case::strict("remote", &["remote", "add", "origin", "./.remote.git"], Shape::BehindRemote));
}

/// `rename` and `remove`: the two commands that rewrite *other* stanzas.
///
/// Renaming `origin` has to move `remote.origin.*`, rewrite the fetch refspec's
/// destination, move every `refs/remotes/origin/*` ref, and repoint
/// `branch.main.remote`/`branch.div.remote`. Removing it has to delete all four.
/// Three of those four are invisible on stdout, which prints nothing.
fn remote_rename_remove(out: &mut Vec<Case>) {
    out.push(Case::new("remote", &["remote", "rename", "origin", "up"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "rename", "--progress", "origin", "up"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "rename", "--no-progress", "origin", "up"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "remove", "origin"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "rm", "origin"], Shape::BehindRemote));

    // Refusals: exit 2 for "no such remote", from both spellings.
    out.push(Case::strict("remote", &["remote", "rename", "nope", "up"], Shape::BehindRemote));
    out.push(Case::strict("remote", &["remote", "remove", "nope"], Shape::BehindRemote));
}

/// `set-url` and `set-branches`: editing one stanza in place.
///
/// `remote.<name>.url` is multi-valued and `remote.<name>.pushurl` shadows it for
/// push only, so "set", "add" and "delete" are three different edits to two
/// different keys — and `--delete` refuses to empty the fetch url list, which is
/// the one case where doing what you were told would break the remote.
fn remote_urls_and_branches(out: &mut Vec<Case>) {
    let br = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("remote", args, Shape::BehindRemote));
    };
    br(&["remote", "set-url", "origin", "./other.git"], out);
    br(&["remote", "set-url", "origin", "https://example.invalid/r.git"], out);
    br(&["remote", "set-url", "--push", "origin", "./push.git"], out);
    br(&["remote", "set-url", "--add", "origin", "./extra.git"], out);
    br(&["remote", "set-url", "--add", "--push", "origin", "./extra.git"], out);
    br(&["remote", "set-url", "--push", "--delete", "origin", "./.remote.git"], out);
    // `set-branches` rewrites the fetch refspec; `--add` appends a second one.
    br(&["remote", "set-branches", "origin", "main"], out);
    br(&["remote", "set-branches", "origin", "main", "div"], out);
    br(&["remote", "set-branches", "--add", "origin", "div"], out);

    // Refusals: emptying the fetch url list, and naming a remote that is absent.
    out.push(Case::strict("remote", &["remote", "set-url", "--delete", "origin", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::strict("remote", &["remote", "set-url", "nope", "./other.git"], Shape::BehindRemote));
    out.push(Case::strict("remote", &["remote", "set-branches", "nope", "main"], Shape::BehindRemote));
}

/// `set-head`, `get-url`, `show`, and the bare listing.
///
/// `set-head` is the only remote subcommand whose effect is a *symref*, and it
/// has three modes that share no code: `-a` asks the remote, an explicit name
/// trusts the caller, `-d` deletes. The `-a` path is a genuine refusal here —
/// the fixture's bare peer was built by `init --bare` and never had its `HEAD`
/// pointed anywhere real, so there is nothing to copy.
fn remote_head_and_query(out: &mut Vec<Case>) {
    out.push(Case::new("remote", &["remote", "set-head", "origin", "main"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "set-head", "origin", "div"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "set-head", "origin", "-d"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "get-url", "origin"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "get-url", "--push", "origin"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "get-url", "--all", "origin"], Shape::BehindRemote));
    // `show` contacts the remote and reports four sections built from four
    // different sources: the urls, the peer's advertisement, the `branch.*`
    // stanzas, and the push refspec resolution.
    out.push(Case::new("remote", &["remote", "show", "origin"], Shape::BehindRemote));
    // `-n` skips the network round trip, so three of those four sections change
    // wording rather than disappearing.
    out.push(Case::new("remote", &["remote", "show", "-n", "origin"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "show"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "-v"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "-v", "show"], Shape::BehindRemote));

    // Refusals. `set-head -a` fails because the peer's HEAD is unborn; the
    // explicit form fails on a branch the peer does not have; `show` on an
    // unknown name fails in the *transport* layer, not in `remote.c`, and prints
    // a different message from every other "no such remote" above.
    out.push(Case::strict("remote", &["remote", "set-head", "origin", "-a"], Shape::BehindRemote));
    out.push(Case::strict("remote", &["remote", "set-head", "origin", "nope"], Shape::BehindRemote));
    out.push(Case::strict("remote", &["remote", "get-url", "nope"], Shape::BehindRemote));
    out.push(Case::strict("remote", &["remote", "show", "nope"], Shape::BehindRemote));
}

/// `prune` and `update`.
///
/// Both are only meaningful when there is something stale to find, and the
/// fixture's remote-tracking refs all match the peer — so the first four cases
/// define a *second* remote entirely on the command line, pointing at the same
/// peer through a refspec whose source side (`refs/heads/nosuch/*`) matches
/// nothing. Every ref under the destination pattern is then unmatched, and prune
/// has real work: it deletes `origin/div` and `origin/main`, which the post-state
/// `for-each-ref` reports. `-n` must find the same two and delete neither.
///
/// The command-line remote is the only way to reach this in one argv, and it is
/// honest — `-c` config is a scope git reads, not a harness trick.
fn remote_prune_update(out: &mut Vec<Case>) {
    const STALE: &[(&str, &str)] = &[
        ("remote.up.url", "./.remote.git"),
        ("remote.up.fetch", "+refs/heads/nosuch/*:refs/remotes/origin/*"),
    ];
    out.push(Case::new("remote", &["remote", "prune", "up"], Shape::BehindRemote).with_config(STALE));
    out.push(Case::new("remote", &["remote", "prune", "-n", "up"], Shape::BehindRemote).with_config(STALE));
    out.push(Case::new("remote", &["remote", "prune", "--dry-run", "up"], Shape::BehindRemote).with_config(STALE));
    out.push(Case::new("remote", &["remote", "update", "--prune", "up"], Shape::BehindRemote).with_config(STALE));

    // Nothing stale: prune must be a no-op that still exits 0, and update must
    // re-fetch and change nothing.
    out.push(Case::new("remote", &["remote", "prune", "origin"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "update"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "update", "origin"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "update", "--prune"], Shape::BehindRemote));
    out.push(
        Case::new("remote", &["remote", "update"], Shape::BehindRemote).with_config(&[("fetch.prune", "true")]),
    );
    // `default` is a remote *group*, not a remote, and resolves to every remote
    // without `remote.<name>.skipDefaultUpdate`.
    out.push(Case::new("remote", &["remote", "update", "default"], Shape::BehindRemote));
}

// ---------------------------------------------------------------------------
// push
// ---------------------------------------------------------------------------

/// Refspec forms, all against the fixture's own `origin`.
///
/// Success here writes two things: the ref on the peer, and the matching
/// `refs/remotes/origin/*` tracking ref locally. Only the second is inside the
/// probe's reach (see the module header), and it is the one ports forget —
/// `transport.c` updates it after `receive-pack` reports success, and a port that
/// stops at "the peer said ok" leaves the tracking ref stale while printing
/// stock's exact output.
fn push_refspecs(out: &mut Vec<Case>) {
    let p = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("push", args, Shape::BehindRemote));
    };
    p(&["push", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "origin", "main:refs/heads/other"], out);
    p(&["push", "origin", "refs/heads/div:refs/heads/copied"], out);
    // Deletion, both spellings, against a ref that exists on the peer.
    p(&["push", "origin", ":refs/heads/div"], out);
    p(&["push", "--delete", "origin", "div"], out);
    p(&["push", "--delete", "origin", "refs/heads/div"], out);
    // `-u` writes `branch.main.merge` pointing at the ref just created, which is
    // a config write the probe compares even though the push itself succeeded.
    p(&["push", "--set-upstream", "origin", "HEAD:refs/heads/tracked"], out);
    p(&["push", "-u", "origin", "main:refs/heads/other"], out);
    // Reporting modes on the success path.
    p(&["push", "--porcelain", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "--dry-run", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "--dry-run", "--porcelain", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "-q", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "-v", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "--no-verify", "origin", "HEAD:refs/heads/newbr"], out);
    // Two refspecs in one invocation, both fast-forward.
    p(&["push", "origin", "HEAD:refs/heads/a", "refs/heads/div:refs/heads/b"], out);
    p(&["push", "--atomic", "origin", "HEAD:refs/heads/a", "refs/heads/div:refs/heads/b"], out);

    // Refusals: a source that resolves to nothing, and a delete of a ref the peer
    // does not have. Both are `push.c` refusals, before any transport happens.
    out.push(Case::strict("push", &["push", "origin", "nosuchref:refs/heads/x"], Shape::BehindRemote));
    out.push(Case::strict("push", &["push", "--delete", "origin", "nosuch"], Shape::BehindRemote));
}

/// The non-fast-forward wall, and the four ways through it.
///
/// `main` is three commits behind the peer and `div` has diverged, so an
/// unforced push of either is a rejection — the default state of this fixture is
/// the interesting one. `--force` overrides unconditionally, `+` in the refspec
/// means the same thing for one ref, and `--force-with-lease` overrides only
/// while the remote-tracking ref still matches the peer. All three land the same
/// ref update, and only the third can be made to *fail* on a stale lease, which
/// is what separates a real lease check from a spelling of `--force`.
fn push_force(out: &mut Vec<Case>) {
    let p = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("push", args, Shape::BehindRemote));
    };
    p(&["push", "--force", "origin", "main"], out);
    p(&["push", "origin", "+main"], out);
    p(&["push", "origin", "+refs/heads/div:refs/heads/div"], out);
    p(&["push", "--force", "origin", "div"], out);
    p(&["push", "--force-with-lease", "origin", "main"], out);
    p(&["push", "--force-with-lease=main", "origin", "main"], out);
    p(&["push", "--force", "--porcelain", "origin", "main"], out);
    p(&["push", "--force", "--dry-run", "origin", "main"], out);

    // Rejections. Each is a different refusal with a different message and a
    // different `--porcelain` status token.
    out.push(Case::strict("push", &["push", "origin", "main"], Shape::BehindRemote));
    out.push(Case::strict("push", &["push", "--dry-run", "origin", "main"], Shape::BehindRemote));
    out.push(Case::strict("push", &["push", "--porcelain", "origin", "main"], Shape::BehindRemote));
    // A lease naming an oid the peer does not have: "stale info", not
    // "non-fast-forward" — the lease is checked before the ancestry is.
    out.push(Case::strict(
        "push",
        &[
            "push",
            "--force-with-lease=main:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "origin",
            "main",
        ],
        Shape::BehindRemote,
    ));
    // `--force-if-includes` refuses even though the lease itself would pass,
    // because the local branch does not contain the remote-tracking tip.
    out.push(Case::strict(
        "push",
        &["push", "--force-if-includes", "--force-with-lease", "origin", "main"],
        Shape::BehindRemote,
    ));
    // Atomic: one bad ref takes the good one down with it, and the good ref must
    // *not* appear on the peer or in the tracking refs afterwards.
    out.push(Case::strict(
        "push",
        &["push", "--atomic", "origin", "main", "HEAD:refs/heads/newbr"],
        Shape::BehindRemote,
    ));
}

/// Bulk refspec modes, `--prune`, and the flags that change the wire rather than
/// the ref set.
fn push_bulk_and_flags(out: &mut Vec<Case>) {
    let p = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("push", args, Shape::BehindRemote));
    };
    // `--all`/`--mirror` both resolve to every local branch, which here is two
    // diverged ones — so both are rejections that reject *twice*.
    p(&["push", "--all", "origin"], out);
    p(&["push", "--all", "--force", "origin"], out);
    p(&["push", "--mirror", "origin"], out);
    // No tags in this fixture: both must walk the tag set and report
    // "Everything up-to-date" rather than erroring or inventing a ref.
    p(&["push", "--tags", "origin"], out);
    p(&["push", "--follow-tags", "origin", "main:refs/heads/other"], out);
    // `--prune` with a source pattern that matches nothing locally: every peer
    // ref under the destination pattern is unmatched, so both are deleted — along
    // with their local tracking refs, which is what the probe sees.
    p(&["push", "--prune", "origin", "+refs/tags/*:refs/heads/*"], out);
    p(&["push", "--prune", "--dry-run", "origin", "+refs/tags/*:refs/heads/*"], out);
    // `--prune` with an explicit non-glob refspec prunes nothing: the destination
    // namespace it is allowed to touch is exactly the one ref it names.
    p(&["push", "--prune", "origin", "+main:refs/heads/main"], out);
    // Transport shaping. None of these changes the ref set; they change what is
    // sent, and a port that rejects the flag fails while a port that ignores it
    // passes — which is the correct outcome for both.
    p(&["push", "--thin", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "--no-thin", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "--receive-pack=git-receive-pack", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "--progress", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "--no-progress", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "--signed=false", "origin", "HEAD:refs/heads/newbr"], out);
    p(&["push", "--verify", "origin", "HEAD:refs/heads/newbr"], out);

    // Refusals. A push option needs a receiving end that advertised support, and
    // `receive-pack` over the file transport advertises none unless
    // `receive.advertisePushOptions` is set on the peer — which no case can do.
    // The failure is therefore the *contract*: `fatal:` and exit 128, not a
    // silently dropped option.
    out.push(Case::strict(
        "push",
        &["push", "--push-option=parity", "origin", "HEAD:refs/heads/newbr"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "push",
        &["push", "-o", "parity", "origin", "HEAD:refs/heads/newbr"],
        Shape::BehindRemote,
    ));
    // Deleting the branch a *non-bare* remote has checked out. The fixture's
    // `origin` is bare and allows it, so this uses `.` — the repository itself —
    // where `receive.denyDeleteCurrent` applies. `transport_local.rs` deletes
    // `feature` this way, which is not the current branch and is allowed; this is
    // the refusal it does not reach.
    out.push(Case::strict("push", &["push", "--delete", ".", "main"], Shape::Branched));
}

/// `push` with no refspec, under each `push.default` value and each config key
/// that supplies a refspec of its own.
///
/// The six `push.default` values choose between four different refspecs, and on
/// this fixture three of the six are distinguishable by output alone: `matching`
/// rejects *both* branches, `nothing` refuses before contacting the peer, and the
/// rest reject only the current one. A port that hard-codes `simple` matches four
/// of six and fails the two that matter.
fn push_defaults(out: &mut Vec<Case>) {
    for value in ["simple", "current", "upstream", "tracking", "matching", "nothing"] {
        out.push(
            Case::new("push", &["push"], Shape::BehindRemote).with_config(&[("push.default", value)]),
        );
    }
    out.push(
        Case::new("push", &["push", "origin"], Shape::BehindRemote)
            .with_config(&[("push.default", "matching")]),
    );
    // `remote.pushDefault` supplies the remote when the argv does not.
    out.push(
        Case::new("push", &["push"], Shape::BehindRemote)
            .with_config(&[("remote.pushDefault", "origin"), ("push.default", "current")]),
    );
    // `remote.<name>.push` supplies the refspec, overriding `push.default`
    // entirely — and it is allowed to name a branch that is not the current one.
    out.push(
        Case::new("push", &["push", "origin"], Shape::BehindRemote)
            .with_config(&[("remote.origin.push", "refs/heads/div:refs/heads/pushed")]),
    );
    out.push(
        Case::new("push", &["push"], Shape::BehindRemote).with_config(&[
            ("remote.pushDefault", "origin"),
            ("remote.origin.push", "+refs/heads/main:refs/heads/main"),
        ]),
    );
    out.push(
        Case::new("push", &["push", "origin", "main:refs/heads/other"], Shape::BehindRemote)
            .with_config(&[("push.followTags", "true")]),
    );
    // `push.autoSetupRemote` only fires for a branch with no upstream, and both
    // branches of BehindRemote have one. Branched has no remote at all, so the
    // remote is defined on the command line: `main` then has a remote to push to
    // and no upstream, which is the one configuration where the key does anything.
    // The push itself is a no-op ("Everything up-to-date" — the destination is
    // the same repository); the measured effect is `branch.main.remote`/`.merge`
    // appearing in the local config.
    out.push(
        Case::new("push", &["push"], Shape::Branched).with_config(&[
            ("remote.origin.url", "."),
            ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
            ("push.autoSetupRemote", "true"),
        ]),
    );
    out.push(
        Case::new("push", &["push"], Shape::Branched).with_config(&[
            ("remote.origin.url", "."),
            ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
            ("push.autoSetupRemote", "false"),
        ]),
    );
}
