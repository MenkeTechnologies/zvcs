//! Differential corpus cases for object transfer over a **local** path:
//! `fetch`, `clone`, `bundle`, `ls-remote`, and the four pack-protocol verbs
//! `fetch-pack`, `upload-pack`, `send-pack` and `receive-pack`.
//!
//! # How this divides territory with `transport_local.rs`
//!
//! `transport_local.rs` came first and owns the *self-referential* spelling of
//! these verbs: a repository used as its own remote (`fetch .`, `push .`,
//! `clone . copy`), plus `pull`, `push` and `upload-archive` â none of which
//! appear here. Its shapes are `Linear`/`Branched`/`Merged`/`Detached`/
//! `AwkwardPaths`/`Submodule`, and its own header records the three things it
//! could not reach: a fixture with a genuinely separate peer, a delta-bearing
//! pack, and a negotiation with something to negotiate about.
//!
//! Two shapes that did not exist when it was written close two of those, and
//! this module is built on them:
//!
//! * [`Shape::BehindRemote`] carries a real bare peer at `./.remote.git`,
//!   reached by a *relative* URL so each per-case copy talks to its own private
//!   one. Almost every case here is on it.
//! * [`Shape::Packed`] carries `repack -ad`-produced packs with real deltas,
//!   which is what makes `bundle create`'s pack-objects run do work rather than
//!   copy seven whole blobs.
//!
//! `push` is deliberately absent: `branch_remote.rs` owns it, on this same
//! shape. Where a spelling would collide with an id already in either module it
//! is not repeated â the collisions are called out at each group.
//!
//! # Exactly what the peer contains, because every refspec below depends on it
//!
//! `fixture.rs`'s `BehindRemote` arm builds `.remote.git` with
//! `init --bare`, pushes two branches into it, and never sets its `HEAD`.
//! Verified with stock 2.55.0 against a hand-built copy of the shape:
//!
//! ```text
//! $ git --git-dir=.remote.git for-each-ref
//! cedd0b7â¦ commit  refs/heads/div
//! f6df84câ¦ commit  refs/heads/main
//! ```
//!
//! Three consequences run through this whole file:
//!
//! 1. **The peer has no tags.** `fetch origin tag v0.1.0` is therefore a
//!    refusal, `ls-remote --tags origin` is empty, and `--prune` against a
//!    refspec whose left side is `refs/tags/*` deletes the *entire*
//!    `refs/remotes/origin/*` namespace â which is the one spelling in the
//!    corpus where prune has something to delete without a setup step.
//! 2. **The peer's `HEAD` is dangling** (`refs/heads/master`, which does not
//!    exist). So `clone ./.remote.git copy` warns `remote HEAD refers to
//!    nonexistent ref, unable to checkout`, `clone --single-branch` reports an
//!    *empty* repository and fetches nothing at all, and `ls-remote --symref
//!    origin` prints no `ref:` line. A port that hard-codes "the remote's HEAD
//!    is the default branch" passes every other fixture and fails all three.
//! 3. **The local side already has every object the peer has**, because the
//!    shape ends with `fetch -q origin`. So a plain `fetch origin` is a no-op:
//!    what the cases below measure is refspec *mapping*, not bytes on the wire.
//!    Where a real transfer is wanted the destination is a fresh clone, whose
//!    object store starts empty.
//!
//! # The no-network rule, and how it is kept
//!
//! No case here touches the network. Every URL is one of: the remote name
//! `origin`, the relative path `./.remote.git`, `.`, `.git/modules/sub`, or a
//! path that is not a repository. Exactly **one** case names a URL at all:
//!
//! ```text
//! $ git clone --bare --separate-git-dir=x https://example.invalid/r.git d
//! fatal: options '--bare' and '--separate-git-dir' cannot be used together
//! ```
//!
//! and it survives the rule only because *both* binaries were checked by hand:
//! stock 2.55.0 and the port under test each refuse during option parsing, exit
//! 128 with that same line, and never construct a transport.
//!
//! That check is not ceremony. A first draft of this module also pinned
//! `fetch --depth=1 --unshallow https://example.invalid/r.git` on the reasoning
//! that stock returns in under a second without resolving. That is true of
//! stock and **false of the port**, whose `fetch` reached its HTTP client and
//! performed a real DNS lookup:
//!
//! ```text
//! zvcs: fetch: An IO error occurred when talking to the server: error sending
//! request for url (https://example.invalid/r.git/info/refs?service=git-upload-pack):
//! dns error: failed to lookup address information
//! ```
//!
//! "Stock refuses before the transport" is therefore not sufficient grounds for
//! a URL in this corpus. That case was respelled against `origin`, where no
//! transport of any kind can be opened, and it still reaches the same refusal.
//! `clone --local https://example.invalid/...` was rejected on the same grounds
//! earlier: it warns `--local is ignored` and then really does resolve the host.
//!
//! `file://` never appears either, and not for the usual reason: a `file://`
//! URL has to be absolute, and an absolute path in argv names one side's
//! fixture root and not the other's. Everything here is a plain relative path,
//! which git routes through the same local transport with
//! `protocol.file.allow` defaulting to `user` and `GIT_PROTOCOL_FROM_USER`
//! unset â so no case needs the setting except the submodule ones, where the
//! *submodule's* URL is what is being guarded.
//!
//! # What the state probe can and cannot see here
//!
//! `runner::probe_state` runs `status --porcelain=v1 -uall`, `for-each-ref`,
//! `rev-parse HEAD`, `ls-files --stage`, `stash list`,
//! `cat-file --batch-check --batch-all-objects` and `config --list --local`
//! **in the fixture root only**. Three limits follow, and the cases are chosen
//! around them rather than pretending they are not there.
//!
//! * **The peer is not probed.** `.remote.git` is inside the fixture but it is
//!   a nested repository and it is masked from `status` by `.git/info/exclude`.
//!   So a `send-pack ./.remote.git â¦` that reported success while writing
//!   nothing on the peer would still score a state match. Every case that
//!   writes only on the peer is therefore `Case::strict`, because its
//!   `To ./.remote.git` / `* [new branch]` report on **stderr** is the only
//!   surface that distinguishes "did it" from "said it did". Where a local side
//!   effect was available instead, the case uses `.` as the destination so the
//!   ref lands under `for-each-ref` â both spellings appear, on purpose.
//! * **A non-bare clone's contents are not probed.** `copy/.git` stops the
//!   walk, so `?? copy/` is the whole story. Every clone case that wants its
//!   *result* compared writes a **bare** destination (`copy.git`, `mirror.git`)
//!   or uses `--separate-git-dir gitdir`, where the git directory is a plain
//!   directory in the worktree and `status -uall` lists every file in it â
//!   including `objects/info/alternates`, which is how `--shared`, `--reference`
//!   and `--dissociate` are told apart.
//! * **`FETCH_HEAD` is not compared.** `runner.rs:2355` deliberately keeps it
//!   out of the probed set, and no single-argv case can read it back. So
//!   `--write-fetch-head` / `--no-write-fetch-head` are pinned on their
//!   *reports* (the `-> FETCH_HEAD` line appears for one and not the other) and
//!   not on the file. The file's contents are measured elsewhere: `fuzz.rs`'s
//!   `bundle-create-unbundle` family and `stdin_plumbing.rs`'s `fmt-merge-msg`
//!   cases feed `FETCH_HEAD`-shaped bytes in by hand.
//! * **`.git/shallow` is not compared either**, for the same reason â it is not
//!   in `OP_STATE_FILES`. `--depth`/`--deepen`/`--shallow-since` are therefore
//!   measured on their reports and on the objects that arrive, not on the
//!   graft file. Said here rather than implied at each case.
//!
//! # Determinism notes
//!
//! * A pack file's name embeds its own checksum, so no case can name one in
//!   argv. `Shape::Packed` copies its pack to the stable worktree path
//!   `packs/sample.pack`, and that is the only pack any case here mentions â
//!   used as a file that is *not* a bundle, which is a sharper rejection than
//!   `README.md` because it has a `PACK` magic of its own.
//! * `bundle create -` writes the bundle to stdout, where it is compared byte
//!   for byte. On `Shape::Packed` those bytes are a fresh `pack-objects` run,
//!   whose delta window is split by `pack.threads`; the Packed bundle cases pin
//!   `pack.threads=1` so the answer is a function of the repository and not of
//!   the builder's core count. Verified: two runs of
//!   `-c pack.threads=1 bundle create - --all` on the shape hash identically.
//! * `receive.certNonceSeed` was tried and **excluded**: it makes
//!   `receive-pack` advertise `push-cert=<unix timestamp>-<hmac>`, which is a
//!   clock reading in stdout. Nothing here sets it.
//! * `--shallow-since=2005-01-01` is a literal date and not a clock read:
//!   `env::harden` pins every commit in every fixture to 2005-04-07, so the
//!   cutoff sits before the whole history on every machine and forever.
//! * The agent string in an `upload-pack`/`receive-pack` advertisement
//!   (`agent=git/2.55.0-Darwin`) is each side's own. `transport_local.rs`
//!   already carries advertisement cases and that difference is already on the
//!   books; the ones here are added for what *surrounds* the agent token â the
//!   capability set, `symref=HEAD:`, and which refs are listed at all.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    fetch_refspecs(out);
    fetch_bulk_and_prune(out);
    fetch_shallow(out);
    fetch_reporting(out);
    fetch_config(out);
    fetch_submodules(out);
    clone_from_peer(out);
    clone_object_sharing(out);
    clone_config_and_layout(out);
    clone_errors(out);
    bundle_create(out);
    bundle_read(out);
    ls_remote(out);
    fetch_pack(out);
    upload_pack(out);
    send_pack(out);
    receive_pack(out);
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

/// Refspec forms against the peer, one per shape of the `<src>[:<dst>]` grammar.
///
/// Every one of these is measured by `for-each-ref` in the post-state: the
/// question is which local ref a given refspec creates, moves, or refuses to
/// move. `transport_local.rs` asks the same grammar of a repository fetching
/// from *itself*, where source and destination share one object store and one
/// ref namespace; here they are two repositories, so a port that resolves the
/// left-hand side locally instead of against the advertisement gets a different
/// answer for every line below.
///
/// Strict throughout, because `fetch` writes its entire report to stderr:
/// without it a case that updated the wrong ref and a case that updated nothing
/// both compare equal on stdout (empty) and exit code (0).
fn fetch_refspecs(out: &mut Vec<Case>) {
    // No refspec at all: the configured `+refs/heads/*:refs/remotes/origin/*`
    // applies, both tracking refs are already at the peer's tips, and the whole
    // invocation is silent. Worth pinning precisely because it is silent â the
    // "up to date" path is where a port that re-reports every ref shows up.
    out.push(Case::strict("fetch", &["fetch", "origin"], Shape::BehindRemote));
    out.push(Case::strict("fetch", &["fetch", "./.remote.git"], Shape::BehindRemote));

    // `<src>` with no `<dst>`: nothing is written to the ref store at all, only
    // to FETCH_HEAD, and the report says so (`-> FETCH_HEAD`).
    out.push(Case::strict("fetch", &["fetch", "origin", "main"], Shape::BehindRemote));
    out.push(Case::strict("fetch", &["fetch", "origin", "div"], Shape::BehindRemote));

    // `<src>:<dst>` in its four spellings: short/short, full/full, forced, glob.
    out.push(Case::strict(
        "fetch",
        &["fetch", "origin", "+main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "origin", "refs/heads/div:refs/heads/imported"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "origin", "refs/heads/*:refs/remotes/mirror/*"],
        Shape::BehindRemote,
    ));
    // The URL spelled out instead of the remote name: same transport, but the
    // refspec defaulting comes from nowhere rather than from `remote.origin.fetch`.
    out.push(Case::strict(
        "fetch",
        &["fetch", "./.remote.git", "refs/heads/main:refs/heads/direct"],
        Shape::BehindRemote,
    ));

    // Two refspecs in one invocation, non-atomic and atomic. Both must land both
    // refs; the difference is only visible when one of them fails, which is the
    // third case.
    out.push(Case::strict(
        "fetch",
        &["fetch", "origin", "main:refs/heads/a", "div:refs/heads/b"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--atomic", "origin", "main:refs/heads/a", "div:refs/heads/b"],
        Shape::BehindRemote,
    ));

    // Rewinding `refs/heads/div` to the peer's `main`: `div` is not an ancestor,
    // so the update is a non-fast-forward. Refused, then forced. The forced case
    // is the one that proves the ref actually moved.
    out.push(Case::strict(
        "fetch",
        &["fetch", "origin", "main:refs/heads/div"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--force", "origin", "main:refs/heads/div"],
        Shape::BehindRemote,
    ));

    // Writing the branch that is checked out. Refused by default with a message
    // naming the worktree by absolute path â `runner::normalize` rewrites the
    // fixture root to `<REPO>` on both sides, which is what makes it strict-able.
    // With `--update-head-ok` it goes through, and the post-state is the
    // interesting part: `refs/heads/main` moves three commits forward while the
    // index and worktree stay put, so `status` reports the whole delta.
    out.push(Case::strict("fetch", &["fetch", "origin", "main:main"], Shape::BehindRemote));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--update-head-ok", "origin", "main:refs/heads/main"],
        Shape::BehindRemote,
    ));

    // Error paths: a source that does not exist on the peer, and `tag <name>`
    // for a tag the peer does not carry â two different messages from
    // `builtin/fetch.c`, and a port that maps both onto one diverges.
    out.push(Case::strict("fetch", &["fetch", "origin", "nosuchref"], Shape::BehindRemote));
    out.push(Case::strict("fetch", &["fetch", "origin", "tag", "v0.1.0"], Shape::BehindRemote));
}

/// Bulk selectors and pruning: `--all`, `--multiple`, `--tags`, `--prune`.
///
/// The prune cases are the payoff of this group and they exist because of one
/// fact about the fixture: **the peer carries no tags.** A refspec whose left
/// side is `refs/tags/*` and whose right side is `refs/remotes/origin/*`
/// therefore matches nothing on the wire while claiming the whole
/// `origin/*` namespace as its destination, so `--prune` deletes both tracking
/// refs. Verified with stock:
///
/// ```text
/// $ git fetch --prune origin 'refs/tags/*:refs/remotes/origin/*'
/// From ./.remote
///  - [deleted]         (none)     -> origin/div
///  - [deleted]         (none)     -> origin/main
/// ```
///
/// That is the only spelling in the corpus where prune has real work to do in a
/// single argv â a stale tracking ref otherwise has to be manufactured by a
/// setup step no `Case` can run. A port that treats `--prune` as advisory, or
/// that computes the prune set from the *configured* refspec instead of the one
/// on the command line, keeps both refs and is caught here and nowhere else.
fn fetch_bulk_and_prune(out: &mut Vec<Case>) {
    // `--all` and `--multiple` walk the configured remote list rather than
    // taking a URL. `--multiple` announces each remote on **stdout**
    // (`Fetching origin`), which is the one fetch report that is not on stderr.
    out.push(Case::strict("fetch", &["fetch", "--all"], Shape::BehindRemote));
    out.push(Case::strict("fetch", &["fetch", "--all", "--jobs=1"], Shape::BehindRemote));
    out.push(Case::strict("fetch", &["fetch", "--multiple", "origin"], Shape::BehindRemote));
    // The same remote named twice: git fetches it twice and says so twice.
    out.push(Case::strict(
        "fetch",
        &["fetch", "--multiple", "origin", "origin"],
        Shape::BehindRemote,
    ));
    // `--multiple` with nothing to multiply: exits 0 in silence rather than
    // falling back to the default remote.
    out.push(Case::strict("fetch", &["fetch", "--multiple"], Shape::BehindRemote));

    // Tag following. The peer has no tags, so both of these reach the tag walk
    // and come back empty â which is the point: arriving at "nothing" means
    // having asked, and a port that never asks scores the same on stdout but
    // differs the moment `--tags` is combined with a refspec.
    out.push(Case::strict("fetch", &["fetch", "--tags", "origin"], Shape::BehindRemote));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--no-tags", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--tags", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));

    // Pruning, in the four spellings that differ in what they are allowed to
    // delete. The first deletes nothing (every tracking ref has a counterpart);
    // the second and third delete both; the fourth proves `--dry-run` composes
    // with prune and leaves the refs alone.
    out.push(Case::strict("fetch", &["fetch", "--prune", "origin"], Shape::BehindRemote));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--prune", "origin", "refs/tags/*:refs/remotes/origin/*"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--prune", "./.remote.git", "refs/tags/*:refs/remotes/origin/*"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--prune", "--dry-run", "origin", "refs/tags/*:refs/remotes/origin/*"],
        Shape::BehindRemote,
    ));
    // `--prune-tags` implies `--prune` for `refs/tags/*` as well. Nothing local
    // to prune on this shape, so what is pinned is that it stays quiet.
    out.push(Case::strict(
        "fetch",
        &["fetch", "--prune", "--prune-tags", "origin"],
        Shape::BehindRemote,
    ));

    // `--all` with a repository argument is rejected during option parsing, before
    // any transport: `builtin/fetch.c` checks `all && argc` and dies.
    out.push(Case::strict("fetch", &["fetch", "--all", "origin"], Shape::BehindRemote));
}

/// Shallow and deepening options.
///
/// A local path is a *shallow-capable* source â `upload-pack` advertises
/// `shallow deepen-since deepen-not deepen-relative` over a pipe exactly as it
/// would over a socket, which the advertisement cases in `upload_pack` below
/// print verbatim. So `--depth`, `--deepen` and `--shallow-since` all take
/// effect here, and only `clone` has a local-transport carve-out (see
/// `clone_object_sharing`).
///
/// What is *not* measured, stated once rather than at every case: `.git/shallow`
/// is not in `runner.rs`'s `OP_STATE_FILES`, so the graft file these write is
/// invisible to the probe. What is compared is the report and the ref that
/// lands. The local store already holds every object the peer has, so a depth
/// limit cannot be caught by object counting on this shape either.
fn fetch_shallow(out: &mut Vec<Case>) {
    out.push(Case::strict(
        "fetch",
        &["fetch", "--depth=1", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--depth=1", "--no-tags", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--deepen=1", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    // `--refetch` tells the negotiator to ignore what the local side already
    // has. Everything is already here, so what is pinned is that the answer is
    // the same and the report does not grow.
    out.push(Case::strict("fetch", &["fetch", "--refetch", "origin"], Shape::BehindRemote));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--refetch", "--depth=1", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));

    // `--shallow-since` with a literal date. Not a clock read: `env::harden`
    // pins every fixture commit to 2005-04-07, so this cutoff is before the
    // entire history on every machine and the answer never changes.
    out.push(Case::strict(
        "fetch",
        &["fetch", "--shallow-since=2005-01-01", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));

    // Two refusals that are worth their slots because they come from different
    // places. `--unshallow` on a complete repository is a `builtin/fetch.c`
    // precondition; `--shallow-exclude=main` reaches the *server* and comes back
    // as `no commits selected for shallow requests` followed by a hangup, which
    // is a two-line stderr a port that fabricates one line cannot match.
    out.push(Case::strict("fetch", &["fetch", "--unshallow", "origin"], Shape::BehindRemote));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--shallow-exclude=main", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    // Option incompatibility, resolved during parsing before the remote is even
    // resolved. Deliberately spelled against `origin` — a local path — rather than
    // against a URL: a first draft used `https://example.invalid/r.git` on the
    // reasoning that stock returns in 0s without resolving, and the harness run
    // proved that reasoning wrong for the *port*, which reached its HTTP client
    // and performed a real DNS lookup. A case is only network-free if BOTH sides
    // are, so the URL is gone and the refusal is pinned where no transport of any
    // kind can be opened.
    out.push(Case::strict(
        "fetch",
        &["fetch", "--depth=1", "--unshallow", "origin"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--depth=1", "--deepen=1", "origin", "main"],
        Shape::BehindRemote,
    ));
}

/// Reporting surfaces: `--dry-run`, `--porcelain`, the verbosity ladder, and
/// the two `FETCH_HEAD` switches.
///
/// `--porcelain` is the only one of these that writes to **stdout**, and it is
/// the machine-readable contract: `<flag> <old> <new> <ref>` per update, with
/// the all-zero id for a ref that did not exist. Verified:
///
/// ```text
/// $ git fetch --porcelain origin '+main:refs/heads/x'
/// * 0000000000000000000000000000000000000000 f6df84câ¦ refs/heads/x
/// ```
///
/// Pairing it with `--dry-run` is where a port most often slips: the porcelain
/// line must still be printed for an update that is not performed, so a port
/// that generates the report from the ref store after the fact prints nothing.
fn fetch_reporting(out: &mut Vec<Case>) {
    out.push(Case::strict(
        "fetch",
        &["fetch", "--dry-run", "origin", "+main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--dry-run", "--atomic", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--porcelain", "origin", "+main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--porcelain", "--dry-run", "origin", "+main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    // Two updates through one glob, so the porcelain stream has to be ordered.
    out.push(Case::strict(
        "fetch",
        &["fetch", "--porcelain", "origin", "refs/heads/*:refs/remotes/mirror/*"],
        Shape::BehindRemote,
    ));

    // The verbosity ladder over one identical update. `-q` prints nothing, the
    // default prints only the ref that changed, and `-v` additionally prints the
    // `= [up to date]` line for the tracking ref that did *not* change â which is
    // the only place in the fetch report where an unchanged ref is mentioned at
    // all, and the line a port that reports only its own writes never produces.
    out.push(Case::strict(
        "fetch",
        &["fetch", "-q", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "-v", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    // On this fixture the local side already holds every object the peer has, so
    // there is no transfer to count and stock's `--progress` output is identical
    // to its `--no-progress` output -- verified, both are just the two-line
    // `From ./.remote` report. The pair is therefore a test that a port does not
    // invent progress where git produces none; it is not a claim about the
    // `isatty` gate, which `bundle create --progress` below shows git overriding.
    out.push(Case::strict(
        "fetch",
        &["fetch", "--progress", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--no-progress", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));

    // The FETCH_HEAD pair. The file itself is outside the probe (see the module
    // header), so what separates these two is the report: `--write-fetch-head`
    // produces the `* branch main -> FETCH_HEAD` line and `--no-write-fetch-head`
    // is silent, because with no `<dst>` in the refspec FETCH_HEAD was the only
    // thing that was going to be written.
    out.push(Case::strict(
        "fetch",
        &["fetch", "--write-fetch-head", "origin", "main"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--no-write-fetch-head", "origin", "main"],
        Shape::BehindRemote,
    ));
}

/// Configuration that changes what a fetch does, delivered through `-c` and
/// through `.git/config`.
///
/// `config --list --local` is part of the state probe, so the settings
/// themselves appear identically on both sides and it is their *effect* that is
/// being compared. Two of these are effects the probe can see directly:
/// `remote.origin.fetch` rewritten to a non-default destination puts the
/// tracking ref under `refs/remotes/pinned/`, and `--set-upstream` writes
/// `branch.main.remote`/`branch.main.merge` â verified to record the *URL*
/// (`branch.main.remote=./.remote.git`) when the remote is spelled as a path
/// rather than as a name, which is a distinction a port that always writes
/// `origin` gets wrong.
fn fetch_config(out: &mut Vec<Case>) {
    // Protocol version. All three complete the same fetch over a pipe, so a
    // divergence here is a port that implements one wire format and silently
    // falls back for the others.
    for version in ["0", "1", "2"] {
        out.push(
            Case::strict("fetch", &["fetch", "origin", "main:refs/heads/x"], Shape::BehindRemote)
                .with_config(&[("protocol.version", version)]),
        );
    }

    // Prune driven by configuration instead of by the flag, in both scopes that
    // can drive it. Same refspec as the flag cases above, so the expected result
    // is known: both tracking refs deleted.
    out.push(
        Case::strict(
            "fetch",
            &["fetch", "origin", "refs/tags/*:refs/remotes/origin/*"],
            Shape::BehindRemote,
        )
        .with_config(&[("fetch.prune", "true")]),
    );
    out.push(
        Case::strict(
            "fetch",
            &["fetch", "origin", "refs/tags/*:refs/remotes/origin/*"],
            Shape::BehindRemote,
        )
        .with_config(&[("remote.origin.prune", "true")]),
    );
    out.push(
        Case::strict("fetch", &["fetch", "origin"], Shape::BehindRemote)
            .with_config(&[("fetch.prune", "true"), ("fetch.pruneTags", "true")]),
    );

    // The configured refspec, rewritten. With no refspec on the command line
    // this is the only thing that decides where the tracking ref lands, and
    // `for-each-ref` shows it arriving at `refs/remotes/pinned/main` while
    // `refs/remotes/origin/*` is left exactly as it was.
    out.push(
        Case::strict("fetch", &["fetch", "origin"], Shape::BehindRemote).with_config(&[(
            "remote.origin.fetch",
            "+refs/heads/main:refs/remotes/pinned/main",
        )]),
    );
    // `tagOpt` is the configured form of `--no-tags`.
    out.push(
        Case::strict("fetch", &["fetch", "origin", "main:refs/heads/x"], Shape::BehindRemote)
            .with_config(&[("remote.origin.tagOpt", "--no-tags")]),
    );

    // `showForcedUpdates=false` replaces the `(forced update)` annotation with a
    // three-line warning on stderr telling the user the check was disabled. That
    // whole warning is the payload â an implementation that just drops the
    // annotation matches the ref state and diverges on every line of the text.
    out.push(
        Case::strict("fetch", &["fetch", "--force", "origin", "main:refs/heads/div"], Shape::BehindRemote)
            .with_config(&[("fetch.showForcedUpdates", "false")]),
    );

    // Negotiation and integrity knobs. Each one changes which code path inside
    // `fetch-pack.c` runs while the observable answer must stay identical, which
    // is the only kind of case that can catch "implemented the flag by ignoring
    // it" in a repository where everything is already present.
    out.push(
        Case::strict("fetch", &["fetch", "origin", "main:refs/heads/x"], Shape::BehindRemote)
            .with_config(&[("fetch.negotiationAlgorithm", "skipping")]),
    );
    out.push(
        Case::strict("fetch", &["fetch", "origin", "main:refs/heads/x"], Shape::BehindRemote)
            .with_config(&[("transfer.fsckObjects", "true")]),
    );
    out.push(
        Case::strict("fetch", &["fetch", "origin", "main:refs/heads/x"], Shape::BehindRemote)
            .with_config(&[("transfer.unpackLimit", "1")]),
    );
    out.push(
        Case::strict("fetch", &["fetch", "origin", "main:refs/heads/x"], Shape::BehindRemote)
            .with_config(&[("fetch.writeCommitGraph", "true"), ("gc.auto", "0")]),
    );
    out.push(
        Case::strict("fetch", &["fetch", "--multiple", "origin", "origin"], Shape::BehindRemote)
            .with_config(&[("fetch.parallel", "1")]),
    );
    out.push(
        Case::strict("fetch", &["fetch", "--negotiation-tip=main", "origin", "main:refs/heads/x"], Shape::BehindRemote)
            .with_config(&[("fetch.negotiationAlgorithm", "noop")]),
    );

    // Partial clone. The filter is negotiated with the server, and the promisor
    // pair is the configuration a partial clone leaves behind â set here without
    // a partial clone having happened, which is the state a port has to tolerate
    // rather than trust.
    out.push(Case::strict(
        "fetch",
        &["fetch", "--filter=blob:none", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    out.push(
        Case::strict(
            "fetch",
            &["fetch", "--filter=blob:none", "origin", "main:refs/heads/x"],
            Shape::BehindRemote,
        )
        .with_config(&[("uploadpack.allowFilter", "true")]),
    );
    out.push(
        Case::strict("fetch", &["fetch", "origin", "main:refs/heads/x"], Shape::BehindRemote)
            .with_config(&[
                ("remote.origin.promisor", "true"),
                ("remote.origin.partialclonefilter", "blob:none"),
            ]),
    );

    // `--set-upstream`, in the two spellings that write different values.
    // Both are read back by the `config --list --local` probe.
    out.push(Case::strict(
        "fetch",
        &["fetch", "--set-upstream", "origin", "div"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--set-upstream", "./.remote.git", "main"],
        Shape::BehindRemote,
    ));
}

/// Submodule recursion.
///
/// `transport_local.rs` already carries the bare `fetch --recurse-submodules .`
/// on this shape; these differ from it by the setting that decides whether the
/// submodule's own fetch is *allowed* to run. The submodule's recorded URL is a
/// plain path, and `protocol.file.allow` defaults to `user` â but the recursion
/// runs with `GIT_PROTOCOL_FROM_USER=0`, so the child fetch is refused unless
/// the setting is `always`. The pair below is therefore a gate test: with the
/// setting the report gains a `Fetching submodule sub` line, without it the
/// parent still succeeds and the submodule is skipped.
fn fetch_submodules(out: &mut Vec<Case>) {
    out.push(
        Case::strict("fetch", &["fetch", "--recurse-submodules", "."], Shape::Submodule)
            .with_config(&[("protocol.file.allow", "always")]),
    );
    out.push(
        Case::strict("fetch", &["fetch", "--recurse-submodules", "."], Shape::Submodule)
            .with_config(&[("protocol.file.allow", "user")]),
    );
    // The submodule's own object store as the source. Its objects are genuinely
    // absent from the parent, so this is one of the few cases in the file where
    // `cat-file --batch-all-objects` actually moves.
    out.push(Case::strict(
        "fetch",
        &[
            "fetch",
            "--recurse-submodules=on-demand",
            ".git/modules/sub",
            "refs/heads/main:refs/heads/subimport",
        ],
        Shape::Submodule,
    ));
    // The two spellings of "do not recurse", which must not differ from each
    // other and must not differ from a plain fetch.
    out.push(Case::strict(
        "fetch",
        &["fetch", "--no-recurse-submodules", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch",
        &["fetch", "--recurse-submodules=no", "origin", "main:refs/heads/x"],
        Shape::BehindRemote,
    ));
}

// ---------------------------------------------------------------------------
// clone
// ---------------------------------------------------------------------------

/// Cloning the peer, whose `HEAD` is dangling.
///
/// This group exists because of one property no other fixture has: the source
/// repository's `HEAD` names `refs/heads/master`, which does not exist. That is
/// not an exotic state â it is what `git init --bare` plus `git push` leaves
/// behind, which is exactly how `fixture.rs` builds `.remote.git` â and it puts
/// `builtin/clone.c` on three different paths depending on the flags:
///
/// ```text
/// $ git clone ./.remote.git copy
/// warning: remote HEAD refers to nonexistent ref, unable to checkout
/// $ git clone --bare --single-branch ./.remote.git copy.git
/// warning: You appear to have cloned an empty repository.
/// $ git clone --no-checkout ./.remote.git copy
/// (no warning at all)
/// ```
///
/// The middle one is the sharpest: `--single-branch` follows the remote's
/// `HEAD` to pick the branch, so a broken `HEAD` means **no refs are fetched at
/// all** even though the peer advertises two. A port that falls back to "the
/// first advertised branch" or to a hard-coded `main` clones two branches, exits
/// 0, prints nothing unusual â and is caught only here.
///
/// Bare destinations throughout wherever the *result* matters: `copy.git` has no
/// nested `.git`, so `status --untracked-files=all` in the state probe walks the
/// whole cloned repository and compares its refs, `HEAD`, `packed-refs`, hook set
/// and object layout file by file.
fn clone_from_peer(out: &mut Vec<Case>) {
    // Non-bare, so only `?? copy/` is probed â these are here for the stderr,
    // which is where the three warnings above live.
    out.push(Case::strict("clone", &["clone", "./.remote.git", "copy"], Shape::BehindRemote));
    out.push(Case::strict(
        "clone",
        &["clone", "--no-checkout", "./.remote.git", "copy"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict("clone", &["clone", "--sparse", "./.remote.git", "copy"], Shape::BehindRemote));

    // Bare and mirror: fully probed. `--mirror` differs from `--bare` in the
    // configured refspec and in `remote.origin.mirror`, both inside the clone.
    out.push(Case::new("clone", &["clone", "--bare", "./.remote.git", "copy.git"], Shape::BehindRemote));
    out.push(Case::new("clone", &["clone", "--mirror", "./.remote.git", "mirror.git"], Shape::BehindRemote));

    // The `--single-branch` trio. The first fetches nothing (dangling HEAD), the
    // second is the default, the third names a branch explicitly and so does not
    // consult HEAD at all â three different answers from one peer.
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--single-branch", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--no-single-branch", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--branch", "div", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));

    // The same three questions asked of a source whose HEAD is *sound* â the
    // fixture itself. `--single-branch` here really does pick `main`, so the pair
    // with the case above isolates the dangling-HEAD behaviour from the flag.
    out.push(Case::new("clone", &["clone", "--bare", ".", "copy.git"], Shape::BehindRemote));
    out.push(Case::new("clone", &["clone", "--mirror", ".", "mirror.git"], Shape::BehindRemote));
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--single-branch", ".", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::new("clone", &["clone", "--bare", "--branch", "div", ".", "copy.git"], Shape::BehindRemote));

    // The remote's name in the clone, set two ways. Both are read back by the
    // clone's own config, which the bare destination puts under the probe.
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--origin", "up", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(
        Case::new("clone", &["clone", "--bare", "./.remote.git", "copy.git"], Shape::BehindRemote)
            .with_config(&[("clone.defaultRemoteName", "up")]),
    );
}

/// How the clone's object store is populated: hardlink, copy, alternate, or a
/// real pack transfer â and the two carve-outs the local transport has.
///
/// These four flags are indistinguishable on stdout and exit code and differ
/// entirely in what lands under `copy.git/objects/`, which a **bare**
/// destination puts inside `status --untracked-files=all`. `--shared` and
/// `--reference` leave `objects/info/alternates` and no loose objects;
/// `--dissociate` leaves the objects and no alternates file; `--no-hardlinks`
/// and `--no-local` leave the objects with no alternates either but by two
/// different routes. The probe lists the paths, so all four are separated.
///
/// The two carve-outs are the reason `--depth` and `--filter` appear twice each.
/// `builtin/clone.c` disables both when it takes the local-copy path and says so:
///
/// ```text
/// $ git clone --bare --depth=1 ./.remote.git copy.git
/// warning: --depth is ignored in local clones; use file:// instead.
/// $ git clone --bare --filter=blob:none ./.remote.git copy.git
/// warning: --filter is ignored in local clones; use file:// instead.
/// ```
///
/// while with `--no-local` the same options reach the server and produce a
/// different answer â a real shallow clone in the first case, and
/// `warning: filtering not recognized by server, ignoring` in the second,
/// because this peer's `upload-pack` does not advertise `filter`. A port that
/// implements the flags and skips the carve-out gets four cases wrong.
fn clone_object_sharing(out: &mut Vec<Case>) {
    out.push(Case::new("clone", &["clone", "--bare", "--local", "./.remote.git", "copy.git"], Shape::BehindRemote));
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--no-hardlinks", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::new("clone", &["clone", "--bare", "--shared", "./.remote.git", "copy.git"], Shape::BehindRemote));
    out.push(Case::new("clone", &["clone", "--bare", "--no-local", "./.remote.git", "copy.git"], Shape::BehindRemote));

    // `--reference` borrows a *third* repository's objects â the fixture itself,
    // which holds everything the peer does. The alternates file it writes names
    // an absolute path, which `runner::normalize` masks; the probe compares the
    // file's presence and the object list beside it, which is the part that
    // differs between borrowing and copying.
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--reference", ".", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--dissociate", "--reference", ".", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    // The reverse direction: the peer as the reference, the fixture as the source.
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--reference", "./.remote.git", ".", "copy.git"],
        Shape::BehindRemote,
    ));

    // The two carve-outs, each with its `--no-local` counterpart.
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--depth=1", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--no-local", "--depth=1", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--no-local", "--depth=1", ".", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--filter=blob:none", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--no-local", "--filter=blob:none", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    // `clone.rejectShallow` on a source that is not shallow: the check has to run
    // and pass, rather than be skipped.
    out.push(
        Case::new("clone", &["clone", "--bare", "./.remote.git", "copy.git"], Shape::BehindRemote)
            .with_config(&[("clone.rejectShallow", "true")]),
    );
}

/// Layout and configuration chosen at clone time.
///
/// `--separate-git-dir gitdir` is the one non-bare spelling whose result is
/// fully probed: the worktree's `.git` becomes a *file*, and `gitdir/` is a
/// plain directory that `status --untracked-files=all` walks like any other. So
/// this is where the non-bare clone's refs, config and object layout can be
/// compared at all â and it is combined with the dangling-HEAD peer so the
/// warning is on trial at the same time.
///
/// `--template=src` points at a directory that exists and contains no `hooks/`,
/// `info/` or `description`, so the clone's `.git` is built with the sample hook
/// set *absent* â a difference of seventeen paths in the probe. (`--template=.`
/// was tried and rejected: it makes git copy the destination into itself until
/// the path length blows up.)
fn clone_config_and_layout(out: &mut Vec<Case>) {
    out.push(Case::strict(
        "clone",
        &["clone", "--separate-git-dir", "gitdir", "./.remote.git", "copy"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "clone",
        &["clone", "--separate-git-dir", "gitdir", ".", "copy"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "clone",
        &["clone", "--template=src", "--bare", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    // `-c` at clone time is written into the *clone's* config, not consulted from
    // the caller's. The bare destination is what makes that observable.
    out.push(Case::new(
        "clone",
        &["clone", "-c", "core.abbrev=12", "--bare", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "clone",
        &["clone", "-c", "fetch.prune=true", "-c", "gc.auto=0", "--bare", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));

    // The verbosity ladder. A local clone copies object files rather than
    // negotiating a pack, so stock has nothing to count and its `--progress`
    // output equals its `--no-progress` output -- verified, both are the same
    // two lines. What the pair measures is that a port does not emit counters,
    // cursor-control escapes or a spinner where git emits none.
    out.push(Case::strict("clone", &["clone", "-q", "--bare", "./.remote.git", "copy.git"], Shape::BehindRemote));
    out.push(Case::strict("clone", &["clone", "-v", "--bare", "./.remote.git", "copy.git"], Shape::BehindRemote));
    out.push(Case::strict(
        "clone",
        &["clone", "--progress", "--bare", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--no-progress", "--bare", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));

    // Submodule recursion, as a gate test rather than a feature test.
    // `--no-checkout` suppresses the submodule work entirely, so these two must
    // be *identical to each other* despite one of them lifting the file-protocol
    // guard â a port that runs the submodule clone before checking `--no-checkout`
    // diverges on one of the pair and not the other.
    out.push(
        Case::strict("clone", &["clone", "--recurse-submodules", "--no-checkout", ".", "copy"], Shape::Submodule)
            .with_config(&[("protocol.file.allow", "always")]),
    );
    out.push(
        Case::strict("clone", &["clone", "--recurse-submodules", "--no-checkout", ".", "copy"], Shape::Submodule)
            .with_config(&[("protocol.file.allow", "user")]),
    );
    // With a checkout and the guard lifted, the submodule really is cloned and
    // the `Submodule path 'sub': checked out '<oid>'` line lands on **stdout**.
    // Not strict: the accompanying stderr names the submodule's own upstream by
    // absolute path, and that path is a fixture-template location outside either
    // side's repository root, so `normalize` has no token for it.
    out.push(
        Case::new(
            "clone",
            &["clone", "--recurse-submodules", "--separate-git-dir", "gitdir", ".", "copy"],
            Shape::Submodule,
        )
        .with_config(&[("protocol.file.allow", "always")]),
    );
}

/// Refusals that fire before or instead of a transfer.
///
/// Four different rejection sites, which is why they are worth four of the
/// budget: a destination that exists, a source that is a file rather than a
/// repository, a source that is a *pack* (magic `PACK`, so the bundle reader
/// gets further before giving up than it does on `README.md`), and an
/// option-compatibility check inside `builtin/clone.c` that returns before the
/// URL is ever opened.
fn clone_errors(out: &mut Vec<Case>) {
    // `src/` is a tracked directory in every shape, so the destination exists and
    // is not empty. The message names the path relatively, so it is strict-able.
    out.push(Case::strict("clone", &["clone", "./.remote.git", "src"], Shape::BehindRemote));
    out.push(Case::strict("clone", &["clone", "./.remote.git", "."], Shape::BehindRemote));
    // A regular file as the source: git tries it as a bundle, then as a gitfile,
    // and reports both failures. Two `fatal:` lines plus an `error:` line, all
    // naming absolute paths that `normalize` masks to `<REPO>`.
    out.push(Case::strict("clone", &["clone", "README.md", "copy"], Shape::BehindRemote));
    // The same path with a real pack file, which gets past the stat and fails on
    // the bundle signature instead.
    out.push(Case::strict("clone", &["clone", "--bare", "packs/sample.pack", "copy.git"], Shape::Packed));
    // Option incompatibility, decided during parsing â verified to return in
    // under a second with no name resolution attempted.
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--separate-git-dir=x", "https://example.invalid/r.git", "d"],
        Shape::BehindRemote,
    ));
    // A branch the peer does not have. `builtin/clone.c` reports it against the
    // remote's *name*, so the message carries `origin` even though the source was
    // spelled as a path.
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--branch", "nosuch", "./.remote.git", "copy.git"],
        Shape::BehindRemote,
    ));
}

// ---------------------------------------------------------------------------
// bundle
// ---------------------------------------------------------------------------

/// `bundle create`, written to **stdout** wherever the bytes matter.
///
/// A bundle is a ref list followed by a packfile, and `-` is the only
/// destination whose contents this harness compares: written to a file, the
/// state probe reports `?? out.bundle` and never opens it. So `-` is used for
/// everything that is about the *format*, and a file destination only where the
/// question is whether the file is created at all.
///
/// `Shape::Packed` is the shape this group needed and `transport_local.rs` did
/// not have. Its seven revisions of one 400-line file give `pack-objects`
/// genuine deltas to find, so the pack inside the bundle is delta-encoded rather
/// than seven whole blobs â the case that `transport_local.rs`'s header names as
/// unreachable. Delta selection splits its window by `pack.threads`, so those
/// cases pin `pack.threads=1`: verified that two runs of
/// `-c pack.threads=1 bundle create - --all` on this shape hash identically.
///
/// The selector cases on `Shape::Branched` are the ones `transport_local.rs`
/// does not spell (`--branches`, `--tags`, `--remotes`), and they matter because
/// each is a different call into `builtin/bundle.c`'s rev-list argument handling
/// and only one of them can be answered by walking `refs/heads`.
fn bundle_create(out: &mut Vec<Case>) {
    let threads = &[("pack.threads", "1")];

    // Delta-bearing bundles, byte for byte on stdout.
    out.push(
        Case::new("bundle", &["bundle", "create", "-", "--all"], Shape::Packed)
            .with_config(threads),
    );
    out.push(
        Case::new("bundle", &["bundle", "create", "-", "HEAD"], Shape::Packed)
            .with_config(threads),
    );
    // A range, so the bundle carries a *prerequisite* line (`-<oid> <subject>`)
    // and a thin pack whose deltas point at objects the bundle does not contain.
    out.push(
        Case::new("bundle", &["bundle", "create", "-", "HEAD~1..HEAD"], Shape::Packed)
            .with_config(threads),
    );
    // The two on-disk formats. v3 adds the `# v3 git bundle` signature and an
    // `@object-format=sha1` capability line ahead of the refs.
    out.push(
        Case::new("bundle", &["bundle", "create", "--version=2", "-", "--all"], Shape::Packed)
            .with_config(threads),
    );
    out.push(
        Case::new("bundle", &["bundle", "create", "--version=3", "-", "--all"], Shape::Packed)
            .with_config(threads),
    );
    // `-q` and `--progress` are NOT symmetric here, and that is the case.
    // `bundle create` runs `pack-objects`, and `--progress` forces the counters
    // on regardless of whether stderr is a terminal -- verified: stock emits
    // `Enumerating objects: 31, done.` followed by 31 `Counting objects: N%`
    // lines into a pipe. `-q` emits nothing. So the pair pins that the flag
    // overrides the isatty check in one direction and the quiet flag wins in the
    // other, while the bundle on stdout stays byte-identical across both.
    out.push(
        Case::strict("bundle", &["bundle", "create", "-q", "-", "--all"], Shape::Packed)
            .with_config(threads),
    );
    out.push(
        Case::strict("bundle", &["bundle", "create", "--progress", "-", "--all"], Shape::Packed)
            .with_config(threads),
    );
    // File destinations: what is compared is that the file appears and that
    // nothing else in the repository moved.
    out.push(
        Case::new("bundle", &["bundle", "create", "out.bundle", "--all"], Shape::Packed)
            .with_config(threads),
    );
    out.push(
        Case::new("bundle", &["bundle", "create", "-q", "out.bundle", "--all"], Shape::Packed)
            .with_config(threads),
    );

    // Selectors, on the shape that has two branches and two tags.
    out.push(Case::new("bundle", &["bundle", "create", "-", "--branches"], Shape::Branched));
    out.push(Case::new("bundle", &["bundle", "create", "-", "--tags"], Shape::Branched));
    out.push(Case::new(
        "bundle",
        &["bundle", "create", "-", "--branches", "--tags"],
        Shape::Branched,
    ));

    // Remote-tracking refs are only non-empty on the shape that has a remote, so
    // `--remotes` is a real selector here and an empty one on `Branched` â the
    // pair is below, in `bundle_read`'s error group for the empty half.
    out.push(Case::new("bundle", &["bundle", "create", "-", "--branches", "--remotes"], Shape::BehindRemote));
    out.push(Case::new("bundle", &["bundle", "create", "-", "--all"], Shape::BehindRemote));
    out.push(Case::new("bundle", &["bundle", "create", "-", "main..div"], Shape::BehindRemote));
    out.push(Case::new("bundle", &["bundle", "create", "out.bundle", "--all"], Shape::BehindRemote));
}

/// The read side, plus the two `create` refusals.
///
/// A `Case` is one argv against a pristine copy, so nothing here can read a
/// bundle it produced â `transport_local.rs`'s header states the constraint and
/// `fuzz.rs`'s `bundle-create-unbundle` family is where the round trip is
/// actually measured, as a multi-step sequence. What is left for a single case
/// is the rejection path, and this group picks the input that gets *furthest*
/// before being rejected: `packs/sample.pack`, a real packfile carried at a
/// stable worktree path by `Shape::Packed`.
///
/// That matters because a bundle is a header followed by a pack. A reader that
/// checks only for the `PACK` magic accepts this file; git checks for
/// `# v2 git bundle` / `# v3 git bundle` first and rejects it before reading a
/// byte of pack. `transport_local.rs` already asks the same three subcommands
/// about `README.md`, which fails on the first byte and cannot tell the two
/// implementations apart.
fn bundle_read(out: &mut Vec<Case>) {
    out.push(Case::strict("bundle", &["bundle", "verify", "packs/sample.pack"], Shape::Packed));
    out.push(Case::strict("bundle", &["bundle", "list-heads", "packs/sample.pack"], Shape::Packed));
    out.push(Case::strict("bundle", &["bundle", "unbundle", "packs/sample.pack"], Shape::Packed));
    out.push(Case::strict(
        "bundle",
        &["bundle", "unbundle", "--progress", "packs/sample.pack"],
        Shape::Packed,
    ));

    // An empty selection. The interesting part is that git has *already written
    // the header to stdout* when it discovers there is nothing to pack, so the
    // case compares a partial stdout against exit 128 â a port that validates
    // before emitting produces empty stdout and diverges while agreeing on the
    // exit code.
    out.push(Case::strict("bundle", &["bundle", "create", "-", "--remotes"], Shape::Branched));

    // `--version=` after the destination is not a bundle option any more, it is a
    // rev-list argument, and rev-list rejects it through `BUG()` â exit 134, not
    // 128, with the message on stderr. Argument *order* is the whole content of
    // this case.
    out.push(Case::strict(
        "bundle",
        &["bundle", "create", "-", "--all", "--version=3"],
        Shape::Branched,
    ));
}

// ---------------------------------------------------------------------------
// ls-remote
// ---------------------------------------------------------------------------

/// `ls-remote`: the ref advertisement, read back on stdout.
///
/// Everything this prints is stdout, so the whole group is measured without
/// `strict` and without leaning on the state probe at all. Two things make it
/// worth a group of its own beyond what `transport_local.rs` already asks of a
/// repository querying itself:
///
/// * **The peer's `HEAD` is dangling**, so `--symref origin` prints *no*
///   `ref: â¦ HEAD` line at all while `--symref .` prints one. A port that
///   synthesises the symref from its own idea of a default branch passes the
///   second and fails the first.
/// * **The peer carries no tags**, so `--tags` is legitimately empty and
///   `--exit-code --tags` is the documented exit 2 â the one non-zero exit in
///   this command that is not an error.
///
/// The `--sort=` cases are a real ordering test rather than a tie: `env::harden`
/// pins every committer date so `--sort=committerdate` would be a total tie, but
/// `refname` and `objectname` both separate `div` from `main` and separate them
/// in *different* orders (`div` first by refname ascending, `main` first by
/// objectname descending), which is why both keys appear.
fn ls_remote(out: &mut Vec<Case>) {
    // No argument at all: the configured upstream of the current branch is used,
    // and the remote is announced on stderr (`From ./.remote.git`) while the refs
    // go to stdout. That split is the case.
    out.push(Case::strict("ls-remote", &["ls-remote"], Shape::BehindRemote));

    out.push(Case::new("ls-remote", &["ls-remote", "origin"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "."], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "--refs", "."], Shape::BehindRemote));

    // The dangling-HEAD pair.
    out.push(Case::new("ls-remote", &["ls-remote", "--symref", "origin"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "--symref", "."], Shape::BehindRemote));

    // Ref-class filters. `--heads` and `--branches` are the old and new spelling
    // of one filter and must agree; `--tags` is empty; combining them restores
    // the unfiltered answer.
    out.push(Case::new("ls-remote", &["ls-remote", "--heads", "origin"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "--branches", "origin"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "--tags", "origin"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "--heads", "--tags", "origin"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "--refs", "origin"], Shape::BehindRemote));

    // Sorting, on the two keys the fixture can separate.
    out.push(Case::new("ls-remote", &["ls-remote", "--sort=refname", "origin"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "--sort=-refname", "origin"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "--sort=objectname", "origin"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "--sort=-objectname", "origin"], Shape::BehindRemote));
    out.push(Case::new(
        "ls-remote",
        &["ls-remote", "--sort=version:refname", "origin"],
        Shape::BehindRemote,
    ));

    // Patterns: a glob that selects one of the two, and a short name that git
    // expands to `refs/heads/main` on its own.
    out.push(Case::new("ls-remote", &["ls-remote", "origin", "refs/heads/d*"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "origin", "main"], Shape::BehindRemote));

    // `--exit-code`: 0 when something matched, 2 when nothing did â twice, once
    // through an empty ref class and once through a pattern that misses.
    out.push(Case::new(
        "ls-remote",
        &["ls-remote", "--exit-code", "origin", "refs/heads/main"],
        Shape::BehindRemote,
    ));
    out.push(Case::new("ls-remote", &["ls-remote", "--exit-code", "--tags", "origin"], Shape::BehindRemote));
    out.push(Case::new(
        "ls-remote",
        &["ls-remote", "--exit-code", "origin", "refs/heads/nope"],
        Shape::BehindRemote,
    ));

    // `--get-url` never opens a transport: it resolves `remote.<name>.url` and
    // prints it, and for a name that is not configured it echoes the argument
    // back and exits 0 rather than failing. That second half is the case.
    out.push(Case::new("ls-remote", &["ls-remote", "--get-url", "origin"], Shape::BehindRemote));
    out.push(Case::new("ls-remote", &["ls-remote", "--get-url", "no-such-remote"], Shape::BehindRemote));

    // `-q` suppresses the `From â¦` announcement on stderr and leaves stdout
    // untouched, so it is only visible under strict.
    out.push(Case::strict("ls-remote", &["ls-remote", "-q", "origin"], Shape::BehindRemote));
    // The helper path spelled out. Both sides resolve `git-upload-pack` through
    // the harness's `PATH`, which is the host's â so both talk to *stock*
    // upload-pack and what is measured is the client half only.
    out.push(Case::new(
        "ls-remote",
        &["ls-remote", "--upload-pack=git-upload-pack", "origin"],
        Shape::BehindRemote,
    ));
}

// ---------------------------------------------------------------------------
// fetch-pack / upload-pack / send-pack / receive-pack
// ---------------------------------------------------------------------------

/// `fetch-pack`: the client half of the fetch protocol, driven directly.
///
/// These four verbs are where a port most often implements the porcelain and
/// skips the protocol, which is why they get a group each even though
/// `transport_local.rs` already touches them. The difference here is the *peer*:
/// `transport_local.rs` runs them against the repository itself or against
/// `.git/modules/sub`, and every case below runs against `./.remote.git`, a bare
/// repository with a dangling `HEAD` and no tags â the advertisement it produces
/// is a different shape from anything that module sees.
///
/// `fetch-pack` prints the refs it obtained on stdout, one `<oid> <ref>` per
/// line, so this whole group is measured on stdout and exit code. Note what is
/// *not* being measured: both sides resolve `git-upload-pack` through the
/// harness's `PATH`, which is the host's, so the far end is always **stock**
/// upload-pack and only the client half is on trial. `transport_local.rs`'s
/// header records that limitation and it applies unchanged here.
fn fetch_pack(out: &mut Vec<Case>) {
    out.push(Case::new("fetch-pack", &["fetch-pack", "--all", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::new(
        "fetch-pack",
        &["fetch-pack", "--all", "--thin", "./.remote.git"],
        Shape::BehindRemote,
    ));
    // `--include-tag` asks the server to append tags that point into the pack.
    // The peer has none, so the correct answer is the same two lines â reached by
    // having asked, which is the only way a port can produce it.
    out.push(Case::new(
        "fetch-pack",
        &["fetch-pack", "--all", "--include-tag", "./.remote.git"],
        Shape::BehindRemote,
    ));
    // `--keep` changes where the received pack lands rather than what is printed.
    out.push(Case::new(
        "fetch-pack",
        &["fetch-pack", "--all", "--keep", "./.remote.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::new("fetch-pack", &["fetch-pack", "-q", "--all", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::new(
        "fetch-pack",
        &["fetch-pack", "--all", "--no-progress", "./.remote.git"],
        Shape::BehindRemote,
    ));
    // Shallow request through the plumbing. The server reports its pack totals on
    // the side band, prefixed `remote:`; not strict, because that line is padded
    // to the side-band width and stdout already carries the answer.
    out.push(Case::new(
        "fetch-pack",
        &["fetch-pack", "--all", "--depth=1", "./.remote.git"],
        Shape::BehindRemote,
    ));

    // Named refs instead of `--all`: one, and then two, so the output ordering is
    // on trial as well as the selection. Git prints them in advertisement order,
    // not argv order â `div` comes back first for `main div`.
    out.push(Case::new(
        "fetch-pack",
        &["fetch-pack", "./.remote.git", "refs/heads/div"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "fetch-pack",
        &["fetch-pack", "./.remote.git", "refs/heads/main", "refs/heads/div"],
        Shape::BehindRemote,
    ));

    // `--diag-url` parses the URL and prints its decomposition without opening
    // anything: `protocol=file` is the line that proves a relative path takes the
    // local transport, which is the premise the whole module rests on.
    out.push(Case::new("fetch-pack", &["fetch-pack", "--diag-url", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::new(
        "fetch-pack",
        &["fetch-pack", "--upload-pack=git-upload-pack", "--all", "./.remote.git"],
        Shape::BehindRemote,
    ));
    out.push(
        Case::new("fetch-pack", &["fetch-pack", "--all", "./.remote.git"], Shape::BehindRemote)
            .with_config(&[("protocol.version", "2")]),
    );

    // Two refusals with different exit codes. A ref the peer does not advertise
    // is `error: no such remote ref â¦` and exit 1; `--stateless-rpc` is not a
    // `fetch-pack` option at all and exits 129 with the usage block, which is the
    // case that catches a port sharing one option table across the four verbs.
    out.push(Case::strict(
        "fetch-pack",
        &["fetch-pack", "./.remote.git", "refs/heads/nope"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "fetch-pack",
        &["fetch-pack", "--stateless-rpc", "--advertise-refs", "./.remote.git"],
        Shape::BehindRemote,
    ));
    // `--stdin` with the harness's null stdin: no refs are requested, and git
    // exits 1 in silence rather than falling back to `--all`.
    out.push(Case::strict("fetch-pack", &["fetch-pack", "--stdin", "./.remote.git"], Shape::BehindRemote));
}

/// `upload-pack`: the server half, printing its v0 advertisement and stopping.
///
/// `--advertise-refs` emits the pkt-line advertisement and exits, so the
/// capability list, the `symref=HEAD:` hint and the ref set are all on stdout
/// with no negotiation. Three of these are configuration cases, and they are the
/// point of the group: each one changes the advertisement in a way that is
/// visible byte for byte.
///
/// * `uploadpack.allowAnySHA1InWant` adds `allow-tip-sha1-in-want` and
///   `allow-reachable-sha1-in-want` to the capability line.
/// * `uploadpack.hiderefs=refs/remotes` removes both `refs/remotes/origin/*`
///   entries from the advertisement while leaving `HEAD` and the branches â
///   verified, and the only case in the corpus where the advertised ref set is a
///   strict subset of the ref store.
/// * `protocol.version=2` does **not** change this output: version selection
///   comes from the `GIT_PROTOCOL` environment variable the client sets, not from
///   the server's own config, so the v0 advertisement is still correct. A port
///   that switches on the config key emits a v2 `version 2` header and diverges.
///
/// The peer's dangling `HEAD` is load-bearing again: `--advertise-refs
/// ./.remote.git` has **no** `symref=HEAD:` capability and no `HEAD` line, while
/// the same command against `.` has both.
fn upload_pack(out: &mut Vec<Case>) {
    out.push(Case::new(
        "upload-pack",
        &["upload-pack", "--advertise-refs", "./.remote.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::BehindRemote));
    out.push(Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::Packed));
    out.push(Case::new(
        "upload-pack",
        &["upload-pack", "--stateless-rpc", "--advertise-refs", "./.remote.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "upload-pack",
        &["upload-pack", "--timeout=1", "--advertise-refs", "./.remote.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "upload-pack",
        &["upload-pack", "--strict", "--advertise-refs", "./.remote.git"],
        Shape::BehindRemote,
    ));

    out.push(
        Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::BehindRemote)
            .with_config(&[("uploadpack.allowAnySHA1InWant", "true")]),
    );
    out.push(
        Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::BehindRemote)
            .with_config(&[("uploadpack.hiderefs", "refs/remotes")]),
    );
    out.push(
        Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::BehindRemote)
            .with_config(&[("uploadpack.allowFilter", "true")]),
    );
    out.push(
        Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::BehindRemote)
            .with_config(&[("protocol.version", "2")]),
    );

    // No `--advertise-refs`: it advertises, reads the null stdin, and must report
    // the hangup rather than block. The capability list differs from the
    // `--advertise-refs` one (`no-done` is absent), which is the detail a port
    // that prints one canned advertisement for both modes gets wrong.
    out.push(Case::strict("upload-pack", &["upload-pack", "./.remote.git"], Shape::BehindRemote));
    // A regular file as the repository.
    out.push(Case::strict(
        "upload-pack",
        &["upload-pack", "--advertise-refs", "README.md"],
        Shape::BehindRemote,
    ));
}

/// `send-pack`: the client half of the push protocol.
///
/// Two destinations, deliberately: `.` and `./.remote.git`.
///
/// * Into `.`, the ref update lands in the fixture's own ref store and
///   `for-each-ref` in the state probe sees it. These cases are measured on
///   state.
/// * Into `./.remote.git`, the ref update lands on the **peer, which the probe
///   does not walk** â it is a nested repository and `.git/info/exclude` masks it
///   from `status`. A `send-pack` that reported success and wrote nothing would
///   score a state match. So every peer-destination case here is
///   `Case::strict`, and its `To ./.remote.git` / `* [new branch] main -> sent`
///   report on stderr is the only evidence the harness can collect. This is the
///   peer-probe limitation named in the module header, and it is where it bites
///   hardest.
///
/// The `receive.*` cases are delivered at **`Repo` scope** rather than through
/// `-c`, and that is not a stylistic choice. `receive.denyCurrentBranch` and
/// `receive.denyNonFastForwards` are read by the *receiving* repository, and a
/// `-c` on the client does not reach it: verified that
/// `-c receive.denyCurrentBranch=false send-pack . â¦` is still refused with the
/// default message, while the same key written into `.git/config` takes effect.
fn send_pack(out: &mut Vec<Case>) {
    // Local destination: probe-visible.
    out.push(Case::strict(
        "send-pack",
        &["send-pack", ".", "refs/heads/div:refs/heads/sent"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "send-pack",
        &["send-pack", "--dry-run", ".", "refs/heads/div:refs/heads/sent"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "send-pack",
        &["send-pack", "--atomic", ".", "refs/heads/div:refs/heads/a", "refs/heads/main:refs/heads/b"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "send-pack",
        &["send-pack", "--receive-pack=git-receive-pack", ".", "refs/heads/div:refs/heads/sent"],
        Shape::BehindRemote,
    ));
    out.push(
        Case::strict("send-pack", &["send-pack", ".", "refs/heads/div:refs/heads/sent"], Shape::BehindRemote)
            .with_config(&[("protocol.version", "2")]),
    );
    // `--stdin` over the null stdin: no ref updates are read, so the correct
    // answer is `Everything up-to-date` and exit 0 â not an error.
    out.push(Case::strict("send-pack", &["send-pack", "--stdin", "."], Shape::BehindRemote));

    // Peer destination: stderr is the only evidence. `main` is not an ancestor of
    // the peer's `div`, so the first is a rejection and the second forces it.
    out.push(Case::strict(
        "send-pack",
        &["send-pack", "./.remote.git", "refs/heads/main:refs/heads/sent"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "send-pack",
        &["send-pack", "./.remote.git", "refs/heads/main:refs/heads/div"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "send-pack",
        &["send-pack", "--force", "./.remote.git", "refs/heads/main:refs/heads/div"],
        Shape::BehindRemote,
    ));

    // The receiving repository's own policy, three settings, one refspec.
    // Default: refused because `main` is checked out. `ignore`: taken, and
    // `for-each-ref` shows `main` moved while the worktree did not.
    // `updateInstead`: refused again, but by a *different* check and with a
    // different message â the worktree has unstaged changes, which this fixture
    // is built to have.
    out.push(Case::strict(
        "send-pack",
        &["send-pack", ".", "refs/heads/div:refs/heads/main"],
        Shape::BehindRemote,
    ));
    out.push(
        Case::strict("send-pack", &["send-pack", ".", "refs/heads/div:refs/heads/main"], Shape::BehindRemote)
            .with_scoped_config(vec![ConfigEntry::set(
                ConfigScope::Repo,
                "receive.denyCurrentBranch",
                "ignore",
            )]),
    );
    out.push(
        Case::strict("send-pack", &["send-pack", ".", "refs/heads/div:refs/heads/main"], Shape::BehindRemote)
            .with_scoped_config(vec![ConfigEntry::set(
                ConfigScope::Repo,
                "receive.denyCurrentBranch",
                "updateInstead",
            )]),
    );
    // `denyNonFastForwards` overrides the client's `--force`: the rejection comes
    // back from the far end prefixed `remote:`, which is a different shape from
    // the client-side refusal above.
    out.push(
        Case::strict(
            "send-pack",
            &["send-pack", "--force", ".", "refs/heads/main:refs/heads/div"],
            Shape::BehindRemote,
        )
        .with_scoped_config(vec![ConfigEntry::set(
            ConfigScope::Repo,
            "receive.denyNonFastForwards",
            "true",
        )]),
    );
}

/// `receive-pack`: the server half. stdin is `/dev/null`, so it advertises its
/// refs on stdout and then reports the hangup â the advertisement is what is
/// compared, byte for byte.
///
/// `transport_local.rs` already runs this on Linear/Branched/Merged/Detached/
/// AwkwardPaths; the cases here add the two ref-store shapes it could not reach
/// and the one configuration key that changes the advertisement.
///
/// `receive.certNonceSeed` is deliberately **absent**. It makes `receive-pack`
/// advertise `push-cert=<unix timestamp>-<hmac>`, which puts a clock reading in
/// stdout and would make the case nondeterministic â verified, and excluded on
/// that basis rather than on principle.
fn receive_pack(out: &mut Vec<Case>) {
    // A ref store with remote-tracking refs in it: the advertisement carries
    // `refs/remotes/origin/*` beside the branches, which no shape in
    // `transport_local.rs` produces.
    out.push(Case::strict("receive-pack", &["receive-pack", "."], Shape::BehindRemote));
    // The bare peer: no `HEAD` line at all, because its `HEAD` is dangling.
    out.push(Case::strict("receive-pack", &["receive-pack", "./.remote.git"], Shape::BehindRemote));
    // A packed ref store with a single branch â the advertisement has to come out
    // of `packed-refs` rather than out of loose files.
    out.push(Case::strict("receive-pack", &["receive-pack", "."], Shape::Packed));
    // Five branches, so the advertisement is long enough that its ordering is a
    // claim rather than an accident.
    out.push(Case::strict("receive-pack", &["receive-pack", "."], Shape::Octopus));
    // A branch checked out in a *linked* worktree. `refs/heads/linked` leads the
    // advertisement and carries the capability list, verified â a port that reads
    // the ref store through the common directory only still finds it, but one
    // that filters out refs checked out elsewhere does not.
    out.push(Case::strict("receive-pack", &["receive-pack", "."], Shape::Worktree));
    // `refs/stash` is advertised like any other ref, verified. It is the one ref
    // class outside `refs/heads` and `refs/tags` any shape carries.
    out.push(Case::strict("receive-pack", &["receive-pack", "."], Shape::Stashed));

    // `transfer.hiderefs` is the receive-side twin of `uploadpack.hiderefs`, and
    // it removes the remote-tracking refs from the advertisement while leaving
    // the branches â verified.
    out.push(
        Case::strict("receive-pack", &["receive-pack", "."], Shape::BehindRemote)
            .with_config(&[("transfer.hiderefs", "refs/remotes")]),
    );
    out.push(
        Case::strict("receive-pack", &["receive-pack", "."], Shape::BehindRemote)
            .with_config(&[("receive.denyCurrentBranch", "updateInstead")]),
    );

    // Two refusals from two different places: a path that does not exist is
    // `does not appear to be a git repository`, while a regular file gets past
    // that check and fails as `invalid gitfile format` â the same split
    // `clone README.md` shows, reached through a different entry point.
    out.push(Case::strict("receive-pack", &["receive-pack", "README.md"], Shape::BehindRemote));
    out.push(Case::strict("receive-pack", &["receive-pack", "no-such-path"], Shape::BehindRemote));
}
