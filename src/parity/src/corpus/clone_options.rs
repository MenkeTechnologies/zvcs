//! `git clone`'s option surface, and the repository each option leaves behind.
//!
//! # How this divides territory with the four modules that already clone
//!
//! `clone` is the most-covered verb in the corpus, so the first job of this
//! module is to not repeat any of it. What the neighbours own, read out of
//! their source rather than from their headers:
//!
//! * **`transport_local.rs`** owns the *self-referential* clone: `.` as the
//!   source, `copy`/`copy.git`/`mirror.git` as the destination, on
//!   `Linear`/`Branched`/`Merged`/`AwkwardPaths`/`Submodule`. Its flag set is
//!   `--quiet`, `--no-checkout`, `--single-branch`, `--branch feature`,
//!   `--origin`/`-o upstream`, `--local`, `--no-hardlinks`, `--shared`,
//!   `--no-local`, `--depth 1 --no-local`, `--filter=blob:none --no-local`,
//!   `--bare`, `--mirror`, `--bare --single-branch`, `--bare --branch feature`,
//!   `--separate-git-dir gitdir`, `--recurse-submodules`, and four error paths
//!   (`clone .`, a missing source, an extra argument, `-h`).
//! * **`fetch_clone.rs`** owns the same option families asked of a *separate
//!   peer* — `Shape::BehindRemote`'s `./.remote.git`, whose `HEAD` dangles.
//!   Everything it carries is on that peer or on `.` **within that shape**:
//!   `--bare`/`--mirror`, the `--single-branch` trio, `--origin up` and
//!   `clone.defaultRemoteName`, the four object-sharing routes, `--reference .`
//!   and `--dissociate --reference .`, the `--depth=1`/`--filter=blob:none`
//!   local carve-outs with and without `--no-local`, `clone.rejectShallow` on a
//!   source that is *not* shallow, `--separate-git-dir gitdir`,
//!   `--template=src`, `-c core.abbrev=12`, the `-q`/`-v`/`--progress` ladder,
//!   `--recurse-submodules` gating, and six refusals.
//! * **`submodule_deep.rs`** owns `--recurse-submodules` as a *feature*:
//!   `-j2`, `--remote-submodules`, `--shallow-submodules` and
//!   `submodule.recurse` against a two-level submodule tree.
//! * **`graft_partial.rs`** owns what a shallow or partial repository *answers*
//!   once it exists; it contains no `clone` case at all. `fixture_gaps2.rs`
//!   carries a `clone --no-local . sh-clone` / `pc-clone` pair, and
//!   `sequences.rs` one `clone --no-hardlinks . sh-copy` sequence.
//! * **`ref_storage.rs`** owns `--ref-format=`; `misc_commands.rs` owns
//!   `--no-ipv4`; `wire_protocol.rs` owns the advertisement a clone negotiates
//!   over; `discovery.rs` owns where git decides it is; `init_family.rs` owns
//!   `init`'s own `--separate-git-dir` and template handling.
//!
//! What none of them has, and what this module is:
//!
//! 1. **`-b`/`--branch` naming a tag rather than a branch.** Every existing
//!    case names `feature`, `div` or `nosuch`. A tag sends `builtin/clone.c`
//!    down a different arm — the clone comes out on a detached `HEAD`, and with
//!    `--single-branch` the refspec it writes is
//!    `+refs/tags/v0.2.0:refs/tags/v0.2.0`, which fetches *no branch at all*.
//! 2. **Tag policy.** `--no-tags` appears nowhere in the corpus, in any verb's
//!    clone cases. It writes `remote.origin.tagOpt` and it changes the ref set.
//! 3. **A `--sparse` clone that really checks out sparsely.** `fetch_clone.rs`'s
//!    `--sparse` is on the dangling-`HEAD` peer, where nothing is checked out at
//!    all, so the cone has never been applied to a worktree.
//! 4. **The whole option-validation layer**: a depth that is not a positive
//!    number, a filter-spec that does not parse, `--also-filter-submodules`
//!    without its two prerequisites, `--shared=group` (`--shared` takes no
//!    value for `clone`, unlike for `init`), `-o` naming something that is not a
//!    valid remote name, `-c` with a section-less key, `--bare --sparse`,
//!    `--mirror -b`, and the three `--separate-git-dir` refusals.
//! 5. **`--reference` versus `--reference-if-able`,** which is the pair that
//!    decides whether a clone can lose its objects. See the section below.
//! 6. **`--reject-shallow`/`clone.rejectShallow` against a source that *is*
//!    shallow** — `fetch_clone.rs` only ever runs that check against a source
//!    where it must pass.
//! 7. **Destination handling**: no destination at all, a destination whose
//!    parent directories do not exist, and a destination that does.
//!
//! # What is measurable about a clone, and what is not
//!
//! This matters more here than anywhere else in the corpus, because a clone's
//! entire product is a *new repository* and stdout is usually two lines.
//! Established by reading `runner.rs`, not assumed:
//!
//! * **The clone's worktree bytes ARE compared.** `probe_worktree_content` →
//!    `collect_worktree` walks the fixture and recurses into any directory that
//!    is neither named `.git` nor itself a bare repository, comparing each
//!    file's content. So `clone . dst` leaves `dst/README.md` and every other
//!    checked-out path in the digest, and `dst/.git` alone is elided. This is
//!    what makes `--sparse` and `-b <tag>` measurable at all.
//!    (`fetch_clone.rs`'s header states the opposite — "a non-bare clone's
//!    contents are not probed" — which is true of the `status` probe it was
//!    describing and not of `collect_worktree`.)
//! * **The clone's `HEAD`, refs, object set, reflogs and `fsck` verdict ARE
//!    compared,** for a clone anywhere in the fixture at any depth.
//!    `probe_peer` → `other_peers` finds every repository under the fixture
//!    root — a bare `dst.git`, a working clone's `dst/.git`, a nested
//!    `a/b/c.git` — and `peer_section` reports its `HEAD` file verbatim, then
//!    runs stock `for-each-ref`, `cat-file --batch-check --batch-all-objects`,
//!    a storage census and `fsck --strict` inside it.
//! * **The clone's file *names* are compared,** by `probe_state`'s
//!    `status --porcelain=v1 --untracked-files=all` in the fixture, which lists
//!    every path under an untracked directory. That is how `--template=`
//!    is measured: no template means seventeen `hooks/*.sample` paths, plus
//!    `info/exclude` and `description`, simply do not appear.
//! * **The clone's configuration VALUES are NOT compared.** `probe_state` runs
//!    `config --list --local` in the *fixture* only, and `peer_section` has no
//!    config probe; `status` sees `dst.git/config` as a path and never opens it.
//!    So `remote.origin.fetch`, `branch.<n>.remote`/`merge`,
//!    `remote.origin.mirror`, `remote.origin.tagOpt`, `core.bare`,
//!    `core.worktree` and every `clone -c key=value` are **unmeasurable
//!    directly**, and a port that writes the wrong refspec into a clone scores a
//!    match here while breaking the next `fetch` in that repository.
//!    Every case below that cares about one of those keys is therefore spelled
//!    so the key has a *consequence* in the same command: `--single-branch -b
//!    <tag>` is compared on the ref set the refspec produced, `--no-tags` on the
//!    tags that are absent, `--mirror` on the refs under `refs/heads`, and
//!    `clone -c core.logAllRefUpdates=true --bare` on the `logs/HEAD` a bare
//!    clone otherwise does not have. The keys with no same-command consequence
//!    stay unmeasured and are named here rather than pretended away.
//! * **`.git/shallow` is not compared** (it is not in `OP_STATE_FILES`), and a
//!    clone's own `objects/info/alternates` is listed by name but not read
//!    (`storage_of` walks `objects/info` and never opens it) — although
//!    `is_alternates` *does* read it for a submodule git directory. See below.
//!
//! # `--shared` / `--reference`, and whether the alternates file is right
//!
//! A wrong alternates file produces a repository that looks complete until the
//! source is garbage-collected and then silently loses objects, so it is worth
//! stating exactly how much of it this harness can see.
//!
//! The **path inside** `objects/info/alternates` is not read for a clone. What
//! *is* read is the consequence of it: `peer_section` runs
//! `cat-file --batch-check --batch-all-objects` and `fsck --strict` inside the
//! clone, and both resolve the alternate. A clone with no objects of its own
//! and a working alternates file lists the source's whole object set and fscks
//! clean; one whose alternates file is missing, empty, or names a path that does
//! not resolve lists nothing and fscks with broken links. So "the borrow works"
//! is measured and "the borrow is spelled the way git spells it" is not.
//! Verified against stock 2.55.0: `clone --bare --shared . c.git` on
//! `Shape::Branched` writes `<repo>/./.git/objects` — with the `./` from the
//! source argument still embedded — and leaves zero loose objects and no pack.
//!
//! # Determinism
//!
//! Every case below was run twice against stock 2.55.0 in a scratch copy of its
//! own shape and compared on stdout, stderr, exit code and the full path listing
//! of the produced tree; all agree. No case reads a clock: `--shallow-since` is
//! spelled `1970-01-01`, which is an absolute date before every fixture commit
//! rather than an offset from now, and it is on the local-clone path where git
//! ignores it anyway. No case names an absolute path; the four whose *message*
//! carries one (`--reference=README.md`, `--separate-git-dir=a/b/gitdir`) are
//! covered by `normalize`'s `<REPO>` masking, which `fetch_clone.rs` already
//! relies on for the same messages. `--jobs` is present only in a spelling with
//! no submodules to clone, so it cannot reorder anything. No URL is named and no
//! transport but the local one is opened; the two `--no-local` cases spawn the
//! fixture's own `upload-pack` over a pipe.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    branch_names_a_tag(out);
    tag_policy(out);
    sparse_checkout(out);
    reference_repositories(out);
    shallow_source_gates(out);
    depth_argument_validation(out);
    filter_argument_validation(out);
    remote_naming_and_clone_time_config(out);
    separate_git_dir(out);
    templates(out);
    destinations(out);
    inert_options(out);
}

// ---------------------------------------------------------------------------
// -b / --branch naming a tag
// ---------------------------------------------------------------------------

/// `-b`/`--branch` handed a tag instead of a branch.
///
/// `Shape::Branched` carries both kinds — `v0.1.0` is lightweight and points
/// straight at `main`'s tip, `v0.2.0` is an annotated tag object — and every
/// `-b` case in the corpus before this one named a branch. The three answers
/// stock 2.55.0 gives are all visible to the probes:
///
/// ```text
/// $ git clone --bare -b v0.1.0 . tagclone.git
/// $ cat tagclone.git/HEAD
/// 5915d79de18d919476d339c8b8efda1d9bb166e2      <- an id, not `ref: …`
///
/// $ git clone --bare --branch=v0.2.0 . tagclone.git
/// warning: refs/tags/v0.2.0 d7277ea9… is not a commit!
///
/// $ git clone --single-branch --branch=v0.2.0 . tagwork
/// $ git -C tagwork for-each-ref
/// refs/tags/v0.1.0 …
/// refs/tags/v0.2.0 …                            <- and no branch at all
/// ```
///
/// The last is the sharpest: `--single-branch` with a tag writes
/// `fetch = +refs/tags/v0.2.0:refs/tags/v0.2.0`, so the clone has **no
/// remote-tracking branch and no local branch**, only tags — a repository shape
/// no other case in the corpus produces. The refspec itself is unreadable by any
/// probe; the ref set it produced is what the case is compared on.
///
/// The non-bare spelling is `strict` because the detached-`HEAD` advice block
/// is where a port most obviously differs, and it is entirely on stderr.
fn branch_names_a_tag(out: &mut Vec<Case>) {
    out.push(Case::new("clone", &["clone", "--bare", "-b", "v0.1.0", ".", "tagclone.git"], Shape::Branched));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--branch=v0.2.0", ".", "tagclone.git"],
        Shape::Branched,
    ));
    out.push(Case::strict("clone", &["clone", "-b", "v0.1.0", ".", "tagwork"], Shape::Branched));
    out.push(Case::strict(
        "clone",
        &["clone", "--single-branch", "--branch=v0.2.0", ".", "tagwork"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--single-branch", "-b", "v0.1.0", ".", "tagclone.git"],
        Shape::Branched,
    ));
    // `--mirror` writes `+refs/*:refs/*` and `-b` adds a tag refspec on top, so
    // two refspecs claim `refs/tags/v0.1.0` and the ref transaction refuses:
    //
    //     Cloning into bare repository 'mirror.git'...
    //     done.
    //     fatal: multiple updates for ref 'refs/tags/v0.1.0' not allowed
    //
    // exit 128 — *after* the repository has been created and `done.` printed.
    // Stock then *unwinds*: `mirror.git` does not exist when the process ends,
    // verified by hand, so the case measures the cleanup as much as the refusal.
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--mirror", "-b", "feature", ".", "mirror.git"],
        Shape::Branched,
    ));
}

// ---------------------------------------------------------------------------
// --no-tags
// ---------------------------------------------------------------------------

/// `--no-tags`, which no clone case in the corpus carries.
///
/// It does two things and the probes see one of them: it writes
/// `remote.origin.tagOpt = --no-tags` into the clone (unreadable, see the
/// header) and it leaves `refs/tags/*` out of the clone (read by
/// `peer_section`'s `for-each-ref`). `Shape::Branched` has two tags, so the
/// difference is two lines of the peer's ref listing plus the tag object itself
/// missing from its `cat-file --batch-all-objects` census.
///
/// Three spellings because the interaction is where it goes wrong: with
/// `--single-branch` the refspec is narrowed twice over, and with `--mirror` the
/// mirror refspec `+refs/*:refs/*` copies the tags *anyway* — `--no-tags` only
/// suppresses the auto-following of tags, it does not subtract from an explicit
/// refspec. A port that implements `--no-tags` as "delete tags at the end"
/// passes the first two and fails the third.
fn tag_policy(out: &mut Vec<Case>) {
    out.push(Case::new("clone", &["clone", "--bare", "--no-tags", ".", "notags.git"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "--no-tags", "--single-branch", ".", "notags"], Shape::Branched));
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--no-tags", "--mirror", ".", "notags.git"],
        Shape::Branched,
    ));
}

// ---------------------------------------------------------------------------
// --sparse
// ---------------------------------------------------------------------------

/// `--sparse`, applied to a checkout that actually happens.
///
/// The corpus's only other `--sparse` clone is against the dangling-`HEAD` peer,
/// which checks nothing out, so the cone has never been applied. `Shape::Branched`
/// has one subdirectory (`src/`) and files at the root, and a `--sparse` clone
/// is defined to check out only the root level — which
/// `collect_worktree` compares file by file, so "which paths are in the
/// worktree" is the verdict rather than an inference from stdout.
///
/// `-n --sparse` is the pair: `--no-checkout` means there is no worktree to
/// narrow, so the sparse-checkout file is written and nothing is checked out,
/// and the two cases differ in exactly the paths the cone would have kept.
///
/// `--bare --sparse` is the refusal, and it is a *late* one — stock 2.55.0
/// clones the repository, prints `done.`, and only then fails:
///
/// ```text
/// Cloning into bare repository 'sparse.git'...
/// done.
/// fatal: this operation must be run in a work tree
/// error: failed to initialize sparse-checkout
/// ```
///
/// exit 1, not 128, and the bare repository is left behind complete. A port that
/// rejects the combination during option parsing exits 128 with no repository,
/// which is a difference in the exit code, in stderr and in the post-state at
/// once.
fn sparse_checkout(out: &mut Vec<Case>) {
    out.push(Case::new("clone", &["clone", "--sparse", ".", "sparsework"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "-n", "--sparse", ".", "sparsework"], Shape::Branched));
    out.push(Case::strict("clone", &["clone", "--bare", "--sparse", ".", "sparse.git"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// --reference / --reference-if-able
// ---------------------------------------------------------------------------

/// Borrowing another repository's objects, and the four ways it can fail.
///
/// `fetch_clone.rs` carries the *successful* borrow (`--reference .`,
/// `--dissociate --reference .`). What it does not carry is the difference
/// between the two spellings, which is the whole point of `--reference-if-able`
/// existing — and, measured against stock 2.55.0, that difference is **not** the
/// blanket "warn instead of fail" a reader would assume:
///
/// ```text
/// $ git clone --bare --reference=nosuchdir . refc.git
/// fatal: reference repository 'nosuchdir' is not a local repository.      exit 128
/// $ git clone --bare --reference-if-able=nosuchdir . refc.git
/// info: Could not add alternate for 'nosuchdir': reference repository
///       'nosuchdir' is not a local repository.
/// done.                                                                   exit 0
///
/// $ git clone --bare --reference=README.md . refc.git
/// fatal: invalid gitfile format: <REPO>/README.md                         exit 128
/// $ git clone --bare --reference-if-able=README.md . refc.git
/// fatal: invalid gitfile format: <REPO>/README.md                         exit 128
/// ```
///
/// A regular file as the reference is fatal under **both** spellings: `if-able`
/// tolerates `add_to_alternates_file` declining, and does not tolerate the
/// gitfile parser dying first. A port that implements `--reference-if-able` as
/// "wrap the whole thing and swallow errors" passes the second pair's first line
/// and fails its second.
///
/// `--dissociate` with no `--reference` at all is accepted silently by stock —
/// there is nothing to dissociate from and the clone proceeds as an ordinary
/// local one — which is worth pinning because "reject an option whose partner is
/// absent" is exactly what git does for `--also-filter-submodules` two groups
/// down, so the asymmetry is real and not guessable.
fn reference_repositories(out: &mut Vec<Case>) {
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--reference=nosuchdir", ".", "refc.git"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--reference-if-able=nosuchdir", ".", "refc.git"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--reference=README.md", ".", "refc.git"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--reference-if-able=README.md", ".", "refc.git"],
        Shape::Branched,
    ));
    out.push(Case::new("clone", &["clone", "--bare", "--dissociate", ".", "diss.git"], Shape::Branched));
    // `-s` is `--shared`'s short form and appears nowhere in the corpus. The
    // clone it makes has *no objects of its own*: `peer_section`'s census lists
    // the source's whole object set through the alternate and `fsck --strict`
    // passes, which is the pair of facts that tells a correct alternates file
    // from a missing one (see the header).
    out.push(Case::new("clone", &["clone", "--bare", "-s", ".", "sharedshort.git"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// A shallow source
// ---------------------------------------------------------------------------

/// The three gates that fire when the *source* repository is shallow.
///
/// `Shape::Shallow` is a `--depth=2` clone of its own peer, so the fixture root
/// is a shallow repository — and every one of these checks was previously only
/// ever run against a source where it passes (`fetch_clone.rs` sets
/// `clone.rejectShallow=true` on `Shape::BehindRemote`, which is not shallow).
///
/// ```text
/// $ git clone --bare --reject-shallow . rej.git
/// fatal: source repository is shallow, reject to clone.                    exit 128
/// $ git -c clone.rejectShallow=true clone --bare --no-reject-shallow . rej.git
/// done.                                                                    exit 0
/// ```
///
/// The second is the one that separates a real option from a hard-coded
/// refusal: the configuration says reject and the command line says do not, and
/// the command line wins.
///
/// The `--reference` pair repeats the group above against a *different* refusal
/// reason — a reference repository that exists and is a repository but is
/// shallow — because that is the reason with the most interesting split:
/// `--reference` is fatal, `--reference-if-able` prints `info:` and clones
/// without the alternate, which is a clone that must then contain the objects
/// itself. The census in `peer_section` is what tells the two outcomes apart.
fn shallow_source_gates(out: &mut Vec<Case>) {
    out.push(Case::strict("clone", &["clone", "--bare", "--reject-shallow", ".", "rej.git"], Shape::Shallow));
    out.push(
        Case::strict("clone", &["clone", "--bare", ".", "rej.git"], Shape::Shallow)
            .with_config(&[("clone.rejectShallow", "true")]),
    );
    out.push(
        Case::strict("clone", &["clone", "--bare", "--no-reject-shallow", ".", "rej.git"], Shape::Shallow)
            .with_config(&[("clone.rejectShallow", "true")]),
    );
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--reference", ".", "./.remote.git", "shref.git"],
        Shape::Shallow,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--reference-if-able", ".", "./.remote.git", "shref.git"],
        Shape::Shallow,
    ));
    // Borrowing from a shallow store. The clone gets no `.git/shallow` of its
    // own, so it is a repository whose alternate's history stops without it
    // knowing — `fsck --strict` inside it is stock 2.55.0's own verdict on that
    // arrangement (exit 0), and pinning it is what makes a port that copies the
    // graft, or that refuses, visible.
    out.push(Case::new("clone", &["clone", "--bare", "--shared", ".", "shshared.git"], Shape::Shallow));
}

// ---------------------------------------------------------------------------
// --depth / --shallow-since / --shallow-exclude argument handling
// ---------------------------------------------------------------------------

/// The shallow options' *arguments*, and the local-clone carve-out for the two
/// `fetch_clone.rs` does not carry.
///
/// `--depth` is parsed before anything else happens — before the URL is looked
/// at, before the local-copy decision — so a bad depth is a pure parser
/// refusal with no `Cloning into…` line at all:
///
/// ```text
/// $ git clone --bare --depth=0 . d.git
/// fatal: depth 0 is not a positive number                                 exit 128
/// $ git clone --bare --depth=abc . d.git
/// fatal: depth abc is not a positive number                               exit 128
/// ```
///
/// `--shallow-since` and `--shallow-exclude` take the same local-clone carve-out
/// `--depth` does, which `fetch_clone.rs` documents for `--depth` alone, and
/// they take it with their own message and a second consequence a reader would
/// not predict: any deepening option makes `clone` default to
/// `--single-branch`, so the clone follows the source's `HEAD` and reports on
/// that too.
///
/// The last case is the only one here that opens a transport, and it opens the
/// fixture's own: `--no-local` puts `--depth` and `--shallow-since` on the wire
/// together, where `upload-pack` refuses the combination rather than `clone`
/// doing it:
///
/// ```text
/// fatal: git upload-pack: deepen and deepen-since (or deepen-not) cannot be used together
/// fatal: the remote end hung up unexpectedly
/// ```
///
/// That refusal comes from the *server* half of the port, which no other clone
/// case reaches, and the `hung up unexpectedly` line is the client's reaction to
/// it — so the pair pins that a port fails the two halves in the right order.
fn depth_argument_validation(out: &mut Vec<Case>) {
    out.push(Case::strict("clone", &["clone", "--bare", "--depth=0", ".", "d.git"], Shape::Branched));
    out.push(Case::strict("clone", &["clone", "--bare", "--depth=abc", ".", "d.git"], Shape::Branched));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--shallow-since=1970-01-01", ".", "d.git"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--shallow-exclude=feature", ".", "d.git"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--no-local", "--depth=1", "--shallow-since=1970-01-01", ".", "d.git"],
        Shape::Branched,
    ));
}

// ---------------------------------------------------------------------------
// --filter / --also-filter-submodules
// ---------------------------------------------------------------------------

/// Filter-spec parsing, and the two prerequisites `--also-filter-submodules`
/// has.
///
/// `fetch_clone.rs` carries `--filter=blob:none` with and without `--no-local`
/// and nothing else of this family. What is added is the parser and the
/// dependency checks, which fire at three different points:
///
/// ```text
/// $ git clone --bare --filter=bogus:spec . f.git
/// fatal: invalid filter-spec 'bogus:spec'                        exit 128, before `Cloning into…`
/// $ git clone --bare --also-filter-submodules . f.git
/// Cloning into bare repository 'f.git'...
/// fatal: the option '--also-filter-submodules' requires '--filter'          exit 128
/// $ git clone --bare --also-filter-submodules --filter=blob:none . f.git
/// Cloning into bare repository 'f.git'...
/// fatal: the option '--also-filter-submodules' requires '--recurse-submodules'  exit 128
/// ```
///
/// The order is the fact: `--filter` is validated first and its dependants
/// second, so satisfying one prerequisite reveals the next rather than the two
/// being reported together. All three leave the destination directory created
/// but empty, which the state probe carries.
///
/// `--filter=blob:limit=1k` is the local carve-out with a *sized* filter rather
/// than `blob:none`, and `--no-local --filter=tree:0` puts a filter the local
/// path cannot fake onto the wire, where the fixture's own `upload-pack` does
/// not advertise `filter` and answers `warning: filtering not recognized by
/// server, ignoring` — a full object set arriving under a flag that asked for a
/// partial one, which is exactly the state a port is tempted to short-circuit.
///
/// One caveat on the last case, recorded so its verdict is not over-read: it is
/// the only case in this module whose destination holds a pack, and
/// `status --untracked-files=all` prints a pack file under its real name, which
/// embeds the pack's own checksum. Measured by hand, the two sides' object sets
/// are byte-identical (`cat-file --batch-check --batch-all-objects` diffs empty)
/// and both write the `.promisor` marker; only the pack name differs. So a
/// difference reported here can be the crate's standing pack-bytes relaxation
/// rather than anything about `--filter`, and the promisor marker is the part of
/// it that is about this option.
fn filter_argument_validation(out: &mut Vec<Case>) {
    out.push(Case::strict("clone", &["clone", "--bare", "--filter=bogus:spec", ".", "f.git"], Shape::Branched));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--filter=blob:limit=1k", ".", "f.git"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--also-filter-submodules", ".", "f.git"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--also-filter-submodules", "--filter=blob:none", ".", "f.git"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--no-local", "--filter=tree:0", ".", "f.git"],
        Shape::Branched,
    ));
}

// ---------------------------------------------------------------------------
// -o / --origin, and `clone -c`
// ---------------------------------------------------------------------------

/// The remote's name, and configuration handed to `clone` rather than to `git`.
///
/// `-o bad~name` is the refusal, and its position is the point: stock validates
/// the name *after* creating the repository, so `Cloning into bare repository
/// 'o.git'...` is printed and `o.git` exists when the `fatal:` lands. A port
/// that validates during option parsing produces the same exit code with a
/// different stderr and a different post-state.
///
/// `--origin=up` is the equals spelling of an option the corpus has only ever
/// passed as two words (`transport_local.rs` and `fetch_clone.rs` both use
/// `-o up` / `--origin up`).
///
/// `clone -c` is git's own two-place ambiguity and the reason the last two cases
/// exist. `git -c k=v clone` sets the key for the cloning *process*;
/// `git clone -c k=v` sets it for the process **and writes it into the new
/// repository's config**. The written half is not readable by any probe (see the
/// header), so the case is chosen to have a same-command consequence instead:
/// `core.logAllRefUpdates=true` makes a **bare** clone write `logs/HEAD`, which
/// a bare clone otherwise never has, and `peer_section`'s reflog listing reports
/// it. Measured, not assumed — and measured in both spellings, which is how the
/// half that *is* unmeasurable got established: both produce `logs/HEAD`, and
/// only the `clone -c` spelling leaves `core.logAllRefUpdates` in the clone's
/// config, so the persistence itself stays outside this harness's reach.
///
/// `-c bogus` is the key with no section, refused by the config writer after the
/// repository is created:
///
/// ```text
/// error: key does not contain a section: bogus
/// fatal: unable to write parameters to config file
/// ```
fn remote_naming_and_clone_time_config(out: &mut Vec<Case>) {
    out.push(Case::strict("clone", &["clone", "--bare", "-o", "bad~name", ".", "o.git"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "--bare", "--origin=up", ".", "o.git"], Shape::Branched));
    out.push(Case::strict("clone", &["clone", "--bare", "-c", "bogus", ".", "o.git"], Shape::Branched));
    out.push(Case::new(
        "clone",
        &["clone", "-c", "core.logAllRefUpdates=true", "--bare", ".", "o.git"],
        Shape::Branched,
    ));
    // `--shared` is `OPT_BOOL` for `clone` and takes a value for `init`, so the
    // spelling a reader carries over from `init --shared=group` is a parse
    // error — exit **129**, which is `parse-options`'s usage exit and the only
    // case in this module that produces it.
    out.push(Case::strict("clone", &["clone", "--shared=group", "--bare", ".", "o.git"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// --separate-git-dir
// ---------------------------------------------------------------------------

/// `--separate-git-dir`, in the equals spelling and in its three refusals.
///
/// The successful space-separated spelling is `transport_local.rs`'s and
/// `fetch_clone.rs`'s. What is added is the `=` form, the `--no-checkout`
/// combination — which leaves a worktree containing *only* the `.git` file, so
/// `collect_worktree` compares that file's `gitdir: …` line and nothing else —
/// and three different rejection sites:
///
/// ```text
/// $ git clone --separate-git-dir=src . sgd
/// fatal: repository path 'src' already exists and is not an empty directory.  exit 128
/// $ git clone --separate-git-dir=a/b/gitdir . sgd
/// Cloning into 'sgd'...
/// fatal: Invalid path '<REPO>/a': No such file or directory                   exit 128
/// $ git clone --bare --separate-git-dir=gitdir . sgd.git
/// fatal: options '--bare' and '--separate-git-dir' cannot be used together    exit 128
/// ```
///
/// Note which of the three prints `Cloning into` first: the destination is
/// created before the git-directory path is validated (and then removed again —
/// `sgd` does not exist once stock has failed, verified by hand), while the
/// *bare* incompatibility is caught during option parsing before anything is
/// created at all. The last is `fetch_clone.rs`'s one URL-bearing case respelled against `.`, so the
/// same check is reachable without an `https://` argument in the corpus at all.
fn separate_git_dir(out: &mut Vec<Case>) {
    out.push(Case::new("clone", &["clone", "--separate-git-dir=gitdir", ".", "sgd"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "-n", "--separate-git-dir=gitdir", ".", "sgd"], Shape::Branched));
    out.push(Case::strict("clone", &["clone", "--separate-git-dir=src", ".", "sgd"], Shape::Branched));
    out.push(Case::strict("clone", &["clone", "--separate-git-dir=a/b/gitdir", ".", "sgd"], Shape::Branched));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--separate-git-dir=gitdir", ".", "sgd.git"],
        Shape::Branched,
    ));
}

// ---------------------------------------------------------------------------
// --template
// ---------------------------------------------------------------------------

/// `--template=` with an empty argument and with a directory that is not there.
///
/// `fetch_clone.rs` points `--template=src` at a directory that exists and
/// happens to contain nothing git recognises. Neither of the two spellings here
/// is that:
///
/// * **`--template=`** — the empty string — suppresses template copying
///   entirely and silently. The clone comes out with `HEAD`, `config`,
///   `objects`, `packed-refs`, `refs` and *nothing else*: no `hooks/`, no
///   `info/`, no `description`. That is around twenty paths absent from the
///   `status --untracked-files=all` listing, which is where this case is
///   decided.
/// * **`--template=nosuchtemplate`** — a path that does not exist — warns
///   (`warning: templates not found in nosuchtemplate`) and then produces the
///   same stripped repository, so the two cases separate the warning from the
///   effect.
fn templates(out: &mut Vec<Case>) {
    out.push(Case::strict("clone", &["clone", "--template=", "--bare", ".", "t.git"], Shape::Branched));
    out.push(Case::strict(
        "clone",
        &["clone", "--template=nosuchtemplate", "--bare", ".", "t.git"],
        Shape::Branched,
    ));
}

// ---------------------------------------------------------------------------
// Destinations
// ---------------------------------------------------------------------------

/// Where the clone lands when the destination is implied, nested, or occupied.
///
/// The corpus has never omitted the destination argument, and for a good
/// reason that is worth writing down: with `.` as the source git derives the
/// directory name from the *absolute* source path, which is `stock` on one side
/// and `zvcs` on the other, so `clone .` would compare noise. `./.remote.git`
/// does not have that problem — the name comes from the argument, `.git` is
/// stripped, and both sides create `.remote` — so `Shape::BehindRemote` is the
/// one shape where the implied destination is measurable at all.
///
/// ```text
/// $ git clone ./.remote.git
/// Cloning into '.remote'...
/// warning: remote HEAD refers to nonexistent ref, unable to checkout       exit 0
/// $ git clone --bare ./.remote.git
/// fatal: destination path '.remote.git' already exists and is not an empty directory.
/// ```
///
/// The bare form is the same derivation with `.git` *kept*, which lands on the
/// source itself — a clone that would have to overwrite what it is reading.
///
/// The nested destination is the other half: `a/b/c.git` has two levels of
/// parent that do not exist, git creates both, and `probe_peer`'s walk finds the
/// repository two directories down rather than as a direct child — so this is
/// also the case that exercises the recursive half of that probe.
fn destinations(out: &mut Vec<Case>) {
    out.push(Case::strict("clone", &["clone", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::strict("clone", &["clone", "--bare", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::new("clone", &["clone", "--bare", ".", "a/b/c.git"], Shape::Branched));
    out.push(Case::strict("clone", &["clone", "--bare", ".", "src"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// Options with nothing to act on
// ---------------------------------------------------------------------------

/// Options that are accepted and have no effect in this repository.
///
/// Every one of these is a flag whose *work* lives in a code path the fixture
/// does not reach — there are no submodules on `Shape::Branched`, and `--jobs`
/// only ever governs submodule clones. They are here because "accepted and
/// inert" and "rejected" are two different answers and a port can give the
/// wrong one for free: `--recurse-submodules=<pathspec>` with a pathspec that
/// matches nothing must still exit 0 and clone normally, `-j2` must not be
/// mistaken for a fetch-parallelism knob, and `--bundle-uri` pointing at a file
/// that is not there must warn twice and carry on:
///
/// ```text
/// warning: failed to download bundle from URI 'nosuch.bundle'
/// warning: failed to fetch objects from bundle URI 'nosuch.bundle'
/// done.                                                                    exit 0
/// ```
///
/// `--bundle-uri` is the only one of these that produces output, and it is the
/// only place in the corpus that option appears at all. The argument is a
/// relative path that does not exist, so nothing is opened and no transport but
/// the local one is involved.
fn inert_options(out: &mut Vec<Case>) {
    out.push(Case::new("clone", &["clone", "--bare", "-j2", ".", "j.git"], Shape::Branched));
    out.push(Case::new(
        "clone",
        &["clone", "--bare", "--recurse-submodules=nosuch", ".", "j.git"],
        Shape::Branched,
    ));
    out.push(Case::new("clone", &["clone", "--bare", "--shallow-submodules", ".", "j.git"], Shape::Branched));
    out.push(Case::strict(
        "clone",
        &["clone", "--bare", "--bundle-uri=nosuch.bundle", ".", "b.git"],
        Shape::Branched,
    ));
}
