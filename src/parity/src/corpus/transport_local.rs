//! Differential corpus cases for the transport_local subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! # Where the second repository comes from
//!
//! Every command in this subsystem needs a remote, and a [`Case`] is one argv
//! run inside one fixture copy — the harness never materializes a peer repo.
//! Three sources of a remote are available without changing the harness, and
//! all three are used below:
//!
//! 1. **The fixture itself, as `.`** — a repository is a legal remote for
//!    itself. `fetch . <src>:<dst>` and `push . <src>:<dst>` move refs and
//!    objects for real, and land them where `for-each-ref` in the post-state
//!    probe can see them. This is the workhorse here.
//! 2. **`.git/modules/sub` and `sub` under [`Shape::Submodule`]** — a genuinely
//!    separate object store with its own history, reachable by a relative path
//!    that is identical in both runs. Fetching from it transfers objects the
//!    parent does not have, so `cat-file --batch-all-objects` and the storage
//!    counters in the post-state probe measure a real transfer.
//! 3. **A clone written into the worktree** — `clone . copy` leaves `copy/` as
//!    an untracked path, and `status --untracked-files=all` reports it. For a
//!    non-bare destination the probe stops at the nested `.git`, so only
//!    creation is observable; for a **bare** destination (`clone --bare .
//!    copy.git`) there is no nested `.git` and the probe walks the whole
//!    repository — every ref, `HEAD`, `packed-refs`, the hook set and the
//!    object layout are compared file by file. Bare clone is the one case in
//!    this subsystem where the clone's contents are fully measured.
//!
//! # What still cannot be expressed, and what it would take
//!
//! * **A round trip through a file the case itself wrote.** `bundle create`
//!   then `bundle verify`/`list-heads`/`unbundle` is two invocations; a case is
//!   one. The read side is therefore only exercised on its error paths (absent
//!   file, non-bundle file, `-` against the null stdin). A `Case` variant
//!   carrying a setup argv run with *stock* git before the compared argv would
//!   close this, and would close it for `git am`, `git apply` and every other
//!   consume-what-you-produced pair as well.
//! * **zvcs talking to a zvcs server.** `clone --no-local .`, `fetch-pack` and
//!   `send-pack` all resolve `git-upload-pack`/`git-receive-pack` through
//!   `PATH`, which the harness pins to the host's — so both runs talk to
//!   *stock* upload-pack, and zvcs's own server side is never on the far end of
//!   a transfer. Fixing that needs either `--upload-pack=<abs path to the
//!   binary under test>` (not expressible: the path is not known to a static
//!   argv) or a runner that prepends the binary's directory to `PATH` under a
//!   `git-upload-pack` alias.
//! * **A delta-bearing history.** No [`Shape`] packs its objects or writes a
//!   blob large and similar enough for git to delta it, so the clone path that
//!   recently broke on delta-bearing packs cannot be pinned here. It needs a
//!   shape that commits several revisions of one sizeable file and then runs
//!   `repack -ad`.
//! * **A true network remote.** Nothing here reaches ssh/https, so credential
//!   handling, redirects and protocol v2 over a real transport are out of
//!   scope by construction.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    clone(out);
    fetch(out);
    pull(out);
    push(out);
    ls_remote(out);
    send_pack(out);
    receive_pack(out);
    upload_pack(out);
    fetch_pack(out);
    upload_archive(out);
    bundle(out);
}

/// `clone`, always into a relative destination inside the fixture.
///
/// The destination name is spelled out in every case: with `.` as the source
/// and no destination, git derives the directory from the *source path*, which
/// differs between the two run directories and would compare noise.
fn clone(out: &mut Vec<Case>) {
    // Non-bare: the post-state sees `?? copy/` and nothing inside it, so these
    // measure "did a clone happen at all" plus stdout and exit code.
    out.push(Case::new("clone", &["clone", ".", "copy"], Shape::Linear));
    out.push(Case::new("clone", &["clone", ".", "copy"], Shape::Branched));
    out.push(Case::new("clone", &["clone", ".", "copy"], Shape::AwkwardPaths));
    out.push(Case::new("clone", &["clone", ".", "copy"], Shape::Submodule));
    out.push(Case::new("clone", &["clone", "--quiet", ".", "copy"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "--no-checkout", ".", "copy"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "--single-branch", ".", "copy"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "--branch", "feature", ".", "copy"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "--origin", "upstream", ".", "copy"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "-o", "upstream", ".", "copy"], Shape::Branched));

    // Local-clone optimizations: hardlink, copy, and alternates respectively.
    out.push(Case::new("clone", &["clone", "--local", ".", "copy"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "--no-hardlinks", ".", "copy"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "--shared", ".", "copy"], Shape::Branched));

    // `--no-local` forces the pack-transfer path instead of copying objects.
    out.push(Case::new("clone", &["clone", "--no-local", ".", "copy"], Shape::Branched));
    out.push(Case::new(
        "clone",
        &["clone", "--depth", "1", "--no-local", ".", "copy"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "clone",
        &["clone", "--filter=blob:none", "--no-local", ".", "copy"],
        Shape::Branched,
    ));

    // Bare destinations: no nested `.git`, so the state probe walks the whole
    // cloned repository. These are the only clone cases whose *contents* are
    // compared — refs, HEAD, packed-refs, hooks and object layout included.
    out.push(Case::new("clone", &["clone", "--bare", ".", "copy.git"], Shape::Linear));
    out.push(Case::new("clone", &["clone", "--bare", ".", "copy.git"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "--bare", ".", "copy.git"], Shape::Merged));
    out.push(Case::new("clone", &["clone", "--mirror", ".", "mirror.git"], Shape::Branched));
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--single-branch", ".", "copy.git"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--branch", "feature", ".", "copy.git"],
        Shape::Branched,
    ));
    // `--separate-git-dir` puts the git dir in the worktree, where the probe
    // walks it just like a bare clone.
    out.push(Case::new(
        "clone",
        &["clone", "--separate-git-dir", "gitdir", ".", "copy"],
        Shape::Branched,
    ));

    // Submodule recursion. The first must be *refused*: the recorded submodule
    // URL is a plain path, and `protocol.file.allow` defaults to `user`, which
    // forbids the file transport for submodule clones. The second opts in and
    // must report the checkout on stdout — the "exit 0 but nothing populated"
    // failure shows up exactly there.
    out.push(Case::new(
        "clone",
        &["clone", "--recurse-submodules", ".", "copy"],
        Shape::Submodule,
    ));
    out.push(Case::new(
        "clone",
        &["-c", "protocol.file.allow=always", "clone", "--recurse-submodules", ".", "copy"],
        Shape::Submodule,
    ));

    // Error paths.
    out.push(Case::new("clone", &["clone", "."], Shape::Linear));
    out.push(Case::new("clone", &["clone", "no-such-path", "copy"], Shape::Linear));
    out.push(Case::new("clone", &["clone", ".", "copy", "extra"], Shape::Linear));
    out.push(Case::new("clone", &["clone", "-h"], Shape::Linear));
}

/// `fetch`, mostly self-referential so the refs it writes land under the probe.
fn fetch(out: &mut Vec<Case>) {
    out.push(Case::new("fetch", &["fetch", "."], Shape::Branched));
    out.push(Case::new("fetch", &["fetch", ".", "HEAD"], Shape::Branched));
    out.push(Case::new("fetch", &["fetch", "--all"], Shape::Branched));
    out.push(Case::new("fetch", &["fetch", "--prune", "."], Shape::Branched));
    out.push(Case::new("fetch", &["fetch", "--multiple", "."], Shape::Branched));

    // Refspec forms: the destination ref is what `for-each-ref` compares.
    out.push(Case::new(
        "fetch",
        &["fetch", ".", "refs/heads/feature:refs/heads/copied"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "fetch",
        &["fetch", "--dry-run", ".", "refs/heads/feature:refs/heads/copied"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "fetch",
        &["fetch", "--quiet", ".", "refs/heads/feature:refs/heads/copied"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "fetch",
        &["fetch", "--depth", "1", ".", "refs/heads/feature:refs/heads/copied"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "fetch",
        &["fetch", "--refmap=", ".", "refs/heads/feature:refs/heads/copied"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "fetch",
        &["fetch", "--no-tags", ".", "refs/heads/main:refs/remotes/self/main"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "fetch",
        &["fetch", "--tags", ".", "refs/heads/main:refs/remotes/self/main"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "fetch",
        &["fetch", ".", "refs/tags/v0.2.0:refs/tags/mirrored"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "fetch",
        &[
            "fetch",
            "--atomic",
            ".",
            "refs/heads/feature:refs/heads/a",
            "refs/heads/main:refs/heads/b",
        ],
        Shape::Branched,
    ));
    out.push(Case::new(
        "fetch",
        &["fetch", ".", "refs/heads/side:refs/heads/imported"],
        Shape::Merged,
    ));

    // Rewinding a ref: refused without `--force`, taken with it.
    out.push(Case::new(
        "fetch",
        &["fetch", ".", "refs/heads/main:refs/heads/feature"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "fetch",
        &["fetch", "--force", ".", "refs/heads/main:refs/heads/feature"],
        Shape::Branched,
    ));
    // Writing the checked-out branch, which needs the explicit opt-in.
    out.push(Case::new(
        "fetch",
        &["fetch", "--update-head-ok", ".", "refs/heads/feature:refs/heads/main"],
        Shape::Branched,
    ));

    // A genuinely foreign object store: these objects are not in the parent, so
    // the object list and the loose/pack counters both move.
    out.push(Case::new(
        "fetch",
        &["fetch", ".git/modules/sub", "refs/heads/main:refs/heads/subimport"],
        Shape::Submodule,
    ));
    out.push(Case::new("fetch", &["fetch", "--recurse-submodules", "."], Shape::Submodule));

    // Error paths.
    out.push(Case::new("fetch", &["fetch", "no-such-remote"], Shape::Linear));
    out.push(Case::new(
        "fetch",
        &["fetch", ".", "refs/heads/nope:refs/heads/copied"],
        Shape::Branched,
    ));
    out.push(Case::new("fetch", &["fetch", "-h"], Shape::Linear));
}

/// `pull`: fetch plus an integration strategy, so both halves are on trial.
fn pull(out: &mut Vec<Case>) {
    // main is an ancestor of feature — the fast-forward path.
    out.push(Case::new("pull", &["pull", ".", "refs/heads/feature"], Shape::Branched));
    out.push(Case::new("pull", &["pull", "--no-rebase", ".", "refs/heads/feature"], Shape::Branched));
    out.push(Case::new("pull", &["pull", "--ff-only", ".", "refs/heads/feature"], Shape::Branched));
    out.push(Case::new("pull", &["pull", "--rebase", ".", "refs/heads/feature"], Shape::Branched));
    out.push(Case::new("pull", &["pull", "--autostash", ".", "refs/heads/feature"], Shape::Branched));
    out.push(Case::new("pull", &["pull", "--quiet", ".", "refs/heads/feature"], Shape::Branched));
    // Forces a merge commit even though a fast-forward was possible: the
    // resulting commit object is compared through `cat-file --batch-all-objects`.
    out.push(Case::new("pull", &["pull", "--no-ff", ".", "refs/heads/feature"], Shape::Branched));
    out.push(Case::new("pull", &["pull", "--squash", ".", "refs/heads/feature"], Shape::Branched));
    out.push(Case::new(
        "pull",
        &["pull", "--allow-unrelated-histories", ".", "refs/heads/feature"],
        Shape::Branched,
    ));

    // Already-integrated and up-to-date paths.
    out.push(Case::new("pull", &["pull", "--ff-only", ".", "refs/heads/main"], Shape::Branched));
    out.push(Case::new("pull", &["pull", ".", "refs/heads/side"], Shape::Merged));
    out.push(Case::new("pull", &["pull", ".", "refs/heads/main"], Shape::Dirty));
    out.push(Case::new("pull", &["pull", ".", "refs/heads/feature"], Shape::Detached));
    out.push(Case::new(
        "pull",
        &["pull", "--recurse-submodules", ".", "refs/heads/main"],
        Shape::Submodule,
    ));
    // Mid-merge: pulling on top of unmerged index entries must be refused.
    out.push(Case::new("pull", &["pull", ".", "refs/heads/theirs"], Shape::Conflicted));

    // Error paths.
    out.push(Case::new("pull", &["pull"], Shape::Linear));
    out.push(Case::new("pull", &["pull", ".", "refs/heads/nope"], Shape::Branched));
    out.push(Case::new("pull", &["pull", "no-such-remote", "main"], Shape::Linear));
    out.push(Case::new("pull", &["pull", "-h"], Shape::Linear));
}

/// `push` into the fixture itself: every ref update is visible to the probe.
fn push(out: &mut Vec<Case>) {
    out.push(Case::new("push", &["push", ".", "HEAD:refs/heads/pushed"], Shape::Branched));
    out.push(Case::new("push", &["push", "--dry-run", ".", "HEAD:refs/heads/pushed"], Shape::Branched));
    out.push(Case::new("push", &["push", "--quiet", ".", "HEAD:refs/heads/pushed"], Shape::Branched));
    out.push(Case::new("push", &["push", "--verbose", ".", "HEAD:refs/heads/pushed"], Shape::Branched));
    out.push(Case::new("push", &["push", "--porcelain", ".", "HEAD:refs/heads/pushed"], Shape::Branched));
    out.push(Case::new(
        "push",
        &["push", "--receive-pack=git-receive-pack", ".", "HEAD:refs/heads/pushed"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "push",
        &["push", "--follow-tags", ".", "HEAD:refs/heads/pushed"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "push",
        &["push", "--signed=false", ".", "HEAD:refs/heads/pushed"],
        Shape::Branched,
    ));
    out.push(Case::new("push", &["push", ".", "HEAD:refs/heads/fromdetached"], Shape::Detached));
    out.push(Case::new(
        "push",
        &["push", ".", "refs/tags/v0.1.0:refs/tags/copiedtag"],
        Shape::Branched,
    ));
    // A bare `HEAD` refspec means "the branch HEAD points at", not a ref
    // literally named HEAD.
    out.push(Case::new("push", &["push", ".", "HEAD"], Shape::Branched));
    out.push(Case::new("push", &["push", ".", "HEAD:refs/heads/main"], Shape::Branched));
    out.push(Case::new(
        "push",
        &["push", "--set-upstream", ".", "HEAD:refs/heads/tracked"],
        Shape::Branched,
    ));

    // Deletion, both spellings.
    out.push(Case::new("push", &["push", ".", ":refs/heads/feature"], Shape::Branched));
    out.push(Case::new("push", &["push", "--delete", ".", "feature"], Shape::Branched));

    // Non-fast-forward: rejected, forced, and lease-checked.
    out.push(Case::new("push", &["push", ".", "refs/heads/main:refs/heads/feature"], Shape::Branched));
    out.push(Case::new(
        "push",
        &["push", "--force", ".", "refs/heads/main:refs/heads/feature"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "push",
        &["push", "--force-with-lease", ".", "refs/heads/main:refs/heads/feature"],
        Shape::Branched,
    ));
    // Updating the branch the remote has checked out: denied by default.
    out.push(Case::new("push", &["push", ".", "refs/heads/feature:refs/heads/main"], Shape::Branched));

    // Bulk refspec modes.
    out.push(Case::new("push", &["push", "--all", "."], Shape::Branched));
    out.push(Case::new("push", &["push", "--tags", "."], Shape::Branched));
    out.push(Case::new("push", &["push", "--mirror", "."], Shape::Branched));
    out.push(Case::new(
        "push",
        &["push", "--prune", ".", "refs/heads/main:refs/heads/main"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "push",
        &[
            "push",
            "--atomic",
            ".",
            "refs/heads/feature:refs/heads/a",
            "refs/heads/main:refs/heads/b",
        ],
        Shape::Branched,
    ));

    // Error paths.
    out.push(Case::new("push", &["push"], Shape::Linear));
    out.push(Case::new("push", &["push", ".", "nonexistent:refs/heads/x"], Shape::Branched));
    out.push(Case::new("push", &["push", "-h"], Shape::Linear));
}

/// `ls-remote`: pure ref advertisement, read back on stdout.
fn ls_remote(out: &mut Vec<Case>) {
    for &shape in &[Shape::Linear, Shape::Branched, Shape::Merged, Shape::Detached] {
        out.push(Case::new("ls-remote", &["ls-remote", "."], shape));
    }
    out.push(Case::new("ls-remote", &["ls-remote", "--heads", "."], Shape::Branched));
    out.push(Case::new("ls-remote", &["ls-remote", "--tags", "."], Shape::Branched));
    out.push(Case::new("ls-remote", &["ls-remote", "--refs", "."], Shape::Branched));
    out.push(Case::new("ls-remote", &["ls-remote", "--symref", "."], Shape::Branched));
    out.push(Case::new("ls-remote", &["ls-remote", "--symref", "."], Shape::Detached));
    out.push(Case::new("ls-remote", &["ls-remote", "--get-url", "."], Shape::Branched));
    out.push(Case::new("ls-remote", &["ls-remote", ".", "refs/heads/main"], Shape::Branched));
    out.push(Case::new("ls-remote", &["ls-remote", ".", "v0.*"], Shape::Branched));
    out.push(Case::new(
        "ls-remote",
        &["ls-remote", "--exit-code", ".", "refs/heads/main"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "ls-remote",
        &["ls-remote", "--exit-code", ".", "refs/heads/nope"],
        Shape::Branched,
    ));
    // A second, unrelated repository reachable by a stable relative path.
    out.push(Case::new("ls-remote", &["ls-remote", "sub"], Shape::Submodule));
    out.push(Case::new("ls-remote", &["ls-remote", ".git/modules/sub"], Shape::Submodule));

    // Error paths.
    out.push(Case::new("ls-remote", &["ls-remote"], Shape::Linear));
    out.push(Case::new("ls-remote", &["ls-remote", "no-such-path"], Shape::Linear));
    out.push(Case::new("ls-remote", &["ls-remote", "-h"], Shape::Linear));
}

/// `send-pack`: the client half of the push protocol, driven directly.
fn send_pack(out: &mut Vec<Case>) {
    out.push(Case::new(
        "send-pack",
        &["send-pack", ".", "refs/heads/feature:refs/heads/sent"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "send-pack",
        &["send-pack", "--dry-run", ".", "refs/heads/feature:refs/heads/sent"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "send-pack",
        &["send-pack", "--atomic", ".", "refs/heads/feature:refs/heads/sent"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "send-pack",
        &["send-pack", "--force", ".", "refs/heads/main:refs/heads/feature"],
        Shape::Branched,
    ));
    out.push(Case::new("send-pack", &["send-pack", "--all", "."], Shape::Branched));
    out.push(Case::new("send-pack", &["send-pack", "--mirror", "."], Shape::Branched));

    // Error paths.
    out.push(Case::new("send-pack", &["send-pack"], Shape::Linear));
    out.push(Case::new(
        "send-pack",
        &["send-pack", "no-such-path", "refs/heads/main"],
        Shape::Linear,
    ));
    out.push(Case::new("send-pack", &["send-pack", "-h"], Shape::Linear));
}

/// `receive-pack`: the server half. stdin is `/dev/null`, so it advertises its
/// refs on stdout and then reports the hangup — the advertisement itself is
/// what gets compared, byte for byte.
fn receive_pack(out: &mut Vec<Case>) {
    for &shape in &[
        Shape::Linear,
        Shape::Branched,
        Shape::Merged,
        Shape::Detached,
        Shape::AwkwardPaths,
    ] {
        out.push(Case::new("receive-pack", &["receive-pack", "."], shape));
    }
    out.push(Case::new("receive-pack", &["receive-pack", ".git/modules/sub"], Shape::Submodule));

    // Error paths.
    out.push(Case::new("receive-pack", &["receive-pack", "no-such-path"], Shape::Linear));
    out.push(Case::new("receive-pack", &["receive-pack"], Shape::Linear));
    out.push(Case::new("receive-pack", &["receive-pack", "-h"], Shape::Linear));
}

/// `upload-pack`: `--advertise-refs` prints the v0 advertisement and exits, so
/// the capability list, the `symref=HEAD:` hint and the ref set are all on
/// stdout without any negotiation.
fn upload_pack(out: &mut Vec<Case>) {
    for &shape in &[
        Shape::Linear,
        Shape::Branched,
        Shape::Merged,
        Shape::Detached,
        Shape::AwkwardPaths,
    ] {
        out.push(Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], shape));
    }
    out.push(Case::new(
        "upload-pack",
        &["upload-pack", "--stateless-rpc", "--advertise-refs", "."],
        Shape::Branched,
    ));
    out.push(Case::new(
        "upload-pack",
        &["upload-pack", "--timeout=1", "--advertise-refs", "."],
        Shape::Branched,
    ));
    out.push(Case::new(
        "upload-pack",
        &["upload-pack", "--advertise-refs", ".git/modules/sub"],
        Shape::Submodule,
    ));
    // No `--advertise-refs`: it advertises, then reads the null stdin and must
    // report the hangup rather than blocking.
    out.push(Case::new("upload-pack", &["upload-pack", "."], Shape::Branched));

    // Error paths.
    out.push(Case::new(
        "upload-pack",
        &["upload-pack", "--strict", "--advertise-refs", "."],
        Shape::Branched,
    ));
    out.push(Case::new(
        "upload-pack",
        &["upload-pack", "--advertise-refs", "no-such-path"],
        Shape::Linear,
    ));
    out.push(Case::new("upload-pack", &["upload-pack", "-h"], Shape::Linear));
}

/// `fetch-pack`: the client half of the fetch protocol. It prints the refs it
/// obtained on stdout and writes the received objects into the local store, so
/// both surfaces are measured.
fn fetch_pack(out: &mut Vec<Case>) {
    out.push(Case::new("fetch-pack", &["fetch-pack", "--all", "."], Shape::Branched));
    out.push(Case::new("fetch-pack", &["fetch-pack", "--quiet", "--all", "."], Shape::Branched));
    out.push(Case::new("fetch-pack", &["fetch-pack", "--all", "--thin", "."], Shape::Branched));
    out.push(Case::new("fetch-pack", &["fetch-pack", "--include-tag", "--all", "."], Shape::Branched));
    out.push(Case::new("fetch-pack", &["fetch-pack", "--diag-url", "."], Shape::Branched));
    out.push(Case::new("fetch-pack", &["fetch-pack", ".", "refs/heads/feature"], Shape::Branched));

    // Objects the local store genuinely lacks, so the object list moves.
    out.push(Case::new(
        "fetch-pack",
        &["fetch-pack", "--all", ".git/modules/sub"],
        Shape::Submodule,
    ));
    out.push(Case::new(
        "fetch-pack",
        &["fetch-pack", "--keep", "--all", ".git/modules/sub"],
        Shape::Submodule,
    ));
    out.push(Case::new(
        "fetch-pack",
        &["fetch-pack", ".git/modules/sub", "refs/heads/main"],
        Shape::Submodule,
    ));

    // Error paths.
    out.push(Case::new("fetch-pack", &["fetch-pack", ".", "refs/heads/nope"], Shape::Branched));
    out.push(Case::new("fetch-pack", &["fetch-pack", "--all", "no-such-path"], Shape::Linear));
    out.push(Case::new("fetch-pack", &["fetch-pack"], Shape::Linear));
    out.push(Case::new("fetch-pack", &["fetch-pack", "-h"], Shape::Linear));
}

/// `upload-archive`: server side of `archive --remote`. With stdin closed it
/// still has to emit the `ACK` packet and then a well-formed error packet, so
/// the pkt-line framing is what is being compared.
fn upload_archive(out: &mut Vec<Case>) {
    out.push(Case::new("upload-archive", &["upload-archive", "."], Shape::Branched));
    out.push(Case::new(
        "upload-archive",
        &["upload-archive", ".git/modules/sub"],
        Shape::Submodule,
    ));
    out.push(Case::new("upload-archive", &["upload-archive", "no-such-path"], Shape::Linear));
    out.push(Case::new("upload-archive", &["upload-archive"], Shape::Linear));
    out.push(Case::new("upload-archive", &["upload-archive", "-h"], Shape::Linear));
}

/// `bundle`.
///
/// `create` is fully measurable: written to a file the state probe reports the
/// new untracked path, and written to `-` the bundle lands on stdout and is
/// compared byte for byte. The read subcommands can only be reached on their
/// error paths, because nothing in a one-argv case can produce a bundle for a
/// later argv to read — see the module header.
fn bundle(out: &mut Vec<Case>) {
    out.push(Case::new("bundle", &["bundle", "create", "out.bundle", "--all"], Shape::Linear));
    out.push(Case::new("bundle", &["bundle", "create", "out.bundle", "--all"], Shape::Branched));
    out.push(Case::new(
        "bundle",
        &["bundle", "create", "out.bundle", "--all"],
        Shape::AwkwardPaths,
    ));
    out.push(Case::new("bundle", &["bundle", "create", "out.bundle", "HEAD"], Shape::Branched));
    out.push(Case::new("bundle", &["bundle", "create", "out.bundle", "main"], Shape::Branched));
    // A prerequisite-bearing bundle: the thin-pack path.
    out.push(Case::new(
        "bundle",
        &["bundle", "create", "out.bundle", "main..feature"],
        Shape::Branched,
    ));
    out.push(Case::new("bundle", &["bundle", "create", "-q", "out.bundle", "--all"], Shape::Branched));
    out.push(Case::new(
        "bundle",
        &["bundle", "create", "--version=3", "out.bundle", "--all"],
        Shape::Branched,
    ));
    // To stdout: the only place a bundle's bytes are directly comparable.
    out.push(Case::new("bundle", &["bundle", "create", "-", "--all"], Shape::Branched));

    // Read side, error paths only.
    out.push(Case::new("bundle", &["bundle", "verify", "out.bundle"], Shape::Branched));
    out.push(Case::new("bundle", &["bundle", "list-heads", "out.bundle"], Shape::Branched));
    out.push(Case::new("bundle", &["bundle", "unbundle", "out.bundle"], Shape::Branched));
    // An existing file that is not a bundle: header rejection, not a stat error.
    out.push(Case::new("bundle", &["bundle", "verify", "README.md"], Shape::Branched));
    out.push(Case::new("bundle", &["bundle", "list-heads", "README.md"], Shape::Branched));
    out.push(Case::new("bundle", &["bundle", "unbundle", "README.md"], Shape::Branched));
    // `-` is the null stdin: an empty stream, not a missing file.
    out.push(Case::new("bundle", &["bundle", "verify", "-"], Shape::Branched));
    out.push(Case::new("bundle", &["bundle", "list-heads", "-"], Shape::Branched));
    out.push(Case::new("bundle", &["bundle", "unbundle", "-"], Shape::Branched));

    out.push(Case::new("bundle", &["bundle"], Shape::Linear));
    out.push(Case::new("bundle", &["bundle", "nosuchsub", "out.bundle"], Shape::Linear));
    out.push(Case::new("bundle", &["bundle", "-h"], Shape::Linear));
}
