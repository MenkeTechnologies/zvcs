//! The ref **store**: how refs are physically kept, declared, migrated, packed
//! and transacted — as opposed to what they are named or what they point at.
//!
//! A ref has two independent descriptions. One is logical: `refs/heads/main` is
//! a name that resolves to a commit, and every reader in git agrees on it
//! whatever is underneath. The other is physical: that name lives in a loose
//! file, or in a line of `packed-refs`, or in a reftable block; the repository
//! *declares* which of those it is through `extensions.refStorage`; a write to
//! it is a transaction with a lock, a precondition and a rollback; and a
//! successful write also appends to a log that is a separate store again. The
//! corpus measured the first description thoroughly and the second one barely,
//! and the second is where a port is asymmetrically dangerous: a reader that
//! answers "no refs" about a repository whose refs it merely cannot parse is
//! worse than one that refuses to open it, because the caller cannot tell the
//! two apart.
//!
//! # How this divides territory with the five adjacent modules
//!
//! * **`plumbing_refs.rs`** is the nearest neighbour and owns the *logical*
//!   half of `show-ref`, `for-each-ref`, `update-ref`, `symbolic-ref`,
//!   `pack-refs`, `reflog` and `refs`: one flag or one format atom per case,
//!   asked of the standard shapes. Everything it writes for `update-ref
//!   --stdin`, `for-each-ref --stdin` and `name-rev --stdin` is on the
//!   **empty-input** path — its header says so, and that note is now stale
//!   about the runner but still true about that file's cases. Its `refs` group
//!   reaches `migrate` only on the two error paths, and its `pack-refs` group
//!   is `Shape::Branched`/`Linear`/`Merged` with four loose refs to pack. This
//!   file takes: real `--stdin` transaction payloads, real `refs migrate` in
//!   both directions, the backend *declaration*, `pack-refs` on the shapes with
//!   remote-tracking refs and nested tags, and the reflog store's own verbs
//!   (`write`, `drop`, `delete --updateref`) which that file does not name.
//! * **`corpus.rs::stdin_driven`** took the first `update-ref --stdin` pass —
//!   six payloads, one line or two each, covering `create`/`delete`/`update`/
//!   `verify` and one `start`/`commit` pair. This file is the depth pass on the
//!   same protocol: the rest of the verb set (`prepare`, `abort`,
//!   `symref-create`/`-update`/`-delete`/`-verify`, `option`), `-z` framing,
//!   the line-termination rule, and the rollback of a multi-command batch. No
//!   payload here repeats one of those six bytes for bytes.
//! * **`stdin_plumbing.rs`** owns the stdin-fed *object* plumbing and, in its
//!   neighbours group, `pack-refs --auto`, `pack-refs --include refs/heads/*
//!   --exclude refs/heads/main` and `pack-refs --all --exclude refs/tags/*` on
//!   `Shape::Branched`. Those three exact invocations are therefore absent
//!   below; `pack-refs` here is on `BehindRemote`, `TagChain`, `AmbiguousRef`
//!   and `Octopus`, whose ref sets it cannot reach.
//! * **`revision_syntax.rs`** owns ref *naming*: `ref_rev_parse_rules`, the six
//!   rules that decide which `refs/…/<name>` a bare `<name>` means, and
//!   `Shape::AmbiguousRef`. A name is not a store, so this file uses that shape
//!   only as a repository that happens to hold nine refs in four namespaces —
//!   something to pack and to migrate — and never asks what a short name
//!   resolves to.
//! * **`branch_remote.rs`** owns `branch`/`remote`/`push` and the porcelain
//!   view of upstream configuration. `%(upstream:track)` and its siblings are
//!   here rather than there because the atom reads two stores against each
//!   other — the local ref and the remote-tracking ref — and `plumbing_refs.rs`
//!   stops at `%(upstream)`/`%(upstream:short)`, which read one.
//! * **`stateful_side_files.rs`** owns the files beside the refs
//!   (`ORIG_HEAD`, `MERGE_*`, rerere, notes). It never sets
//!   `extensions.*`, and no module in the corpus did before this one.
//! * **`fixture_gaps3.rs`** owns whatever a newly built shape made reachable;
//!   grep confirms it names no `refs`, `pack-refs` or `update-ref` case.
//!
//! # What the harness can and cannot see here, stated rather than assumed
//!
//! `runner::probe_state` reads the refs back with stock `for-each-ref`, reads
//! `.git/logs/**` **verbatim** (`probe_reflogs`), reads `.git/config --list
//! --local`, and records the *presence* of `packed-refs` in the accelerator
//! line. So four of the five things this file is about are measured directly:
//! which refs exist, what the reflog store holds byte for byte, what the
//! repository declares about its backend, and whether refs were packed at all.
//!
//! **The fifth is not: the bytes of `packed-refs` are never compared.** A case
//! is one argv, so nothing here can print that file, and its header line, its
//! sort order and its `^`-peeled lines are outside the digest. They were
//! checked by hand instead — stock and the port produce byte-identical
//! `packed-refs` for `pack-refs --all` on `Shape::Branched`, header
//! `# pack-refs with: peeled fully-peeled sorted ` included, with the peeled
//! line for `refs/tags/v0.2.0` present and every loose ref removed — and that
//! is a measurement of today's binaries, not a guarantee the corpus will keep
//! making. What the cases below *do* pin is everything a reader can still see
//! afterwards: the ref set, the reflogs, and stock's own `fsck` verdict on the
//! result.
//!
//! # Determinism
//!
//! * No case here uses `reflog expire --expire=now` or any other relative date.
//!   `now` is the wall clock; `all` and `never` are not, and are what the
//!   expiry cases below pass.
//! * `refs migrate --dry-run` writes into a temp directory whose name is
//!   re-rolled every run and prints it, so it is absent — the same call
//!   `plumbing_refs.rs` makes and for the same reason.
//! * A **reftable file name embeds the update index and a random suffix**
//!   (`0x000000000001-0x000000000009-27a82907.ref`), so a case that leaves a
//!   repository in reftable format can only be compared through probes that do
//!   not name files. `probe_state` is exactly that — `for-each-ref` and
//!   `config --list --local` — and `probe_storage` censuses `objects/`, not
//!   `reftable/`. Verified by running `refs migrate --ref-format=reftable`
//!   twice against a stock-built `Shape::Branched` copy: the two runs produce
//!   different reftable file names and identical `probe_state` digests.
//! * `pack-refs --auto` is a heuristic over the loose ref *count*, and both
//!   sides count the same fixture, so it is deterministic here. It is only
//!   drawn on shapes where the answer is not already fixed by `--all`.
//! * Object ids written into a stdin payload are constants of the fixture. Only
//!   `Shape::Branched`'s are used, and only the three that its build pins:
//!   `edfab1b7…` (`initial`, shared by every shape), `5915d79d…` (`main`) and
//!   `07e86d1f…` (`feature`). They are stated in [`BRANCHED_MAIN`] and friends
//!   so a fixture change breaks one constant rather than silently changing what
//!   a transaction means.
//!
//! # What could not be measured, and why
//!
//! **The port reading a real reftable repository.** This is the question the
//! module most wants to ask and cannot: it needs a fixture whose ref store *is*
//! a reftable, a `Shape` cannot be added from here, and the port refuses to
//! create one (`refs migrate`, `init --ref-format` and `clone --ref-format` all
//! exit non-zero with `no vendored reftable backend`), so no single argv leaves
//! one behind for the probes to find. Asked by hand instead, against a
//! reftable repository produced by stock `refs migrate` from a `Shape::Branched`
//! copy — the results are in the report accompanying this file and they are the
//! bad direction: `show-ref` exits 1 with no output, `status` and `log` print a
//! parse error on stderr and still **exit 0**, and `update-ref refs/heads/x
//! HEAD` says `fatal: HEAD: not a valid SHA1` and also exits 0. Closing this
//! properly needs a reftable `Shape`, which is a `fixture.rs` change.
//!
//! What *is* reachable, and is below, is the declaration without the store:
//! `extensions.refStorage` set from repository configuration over a files
//! repository. Stock believes the declaration and reads an empty reftable; the
//! port ignores it and serves the loose refs. That is the same defect measured
//! from the other side.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// `Shape::Branched`'s `refs/heads/main`, which is also `HEAD` and
/// `refs/tags/v0.1.0`.
const BRANCHED_MAIN: &str = "5915d79de18d919476d339c8b8efda1d9bb166e2";
/// `Shape::Branched`'s `refs/heads/feature`.
const BRANCHED_FEATURE: &str = "07e86d1fedb713fbc84a754c98ea4bfe53316416";
/// The `initial` commit every shape descends from; `Shape::Branched`'s `HEAD~1`.
const BRANCHED_ROOT: &str = "edfab1b71619a22120a8da1a3d85d68e0200290a";

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    debug_assert_eq!(BRANCHED_MAIN.len(), 40);
    debug_assert_eq!(BRANCHED_FEATURE.len(), 40);
    debug_assert_eq!(BRANCHED_ROOT.len(), 40);
    migrate(out);
    declared_backend(out);
    transaction_protocol(out);
    symref_protocol(out);
    reflog_store(out);
    packing(out);
    exclude_existing(out);
    two_store_atoms(out);
    created_format(out);
}

/// `refs migrate`: the only command that rewrites the ref store into a second
/// physical format.
///
/// `plumbing_refs.rs` reaches it twice, both on error paths, with the note that
/// there is "no backend to migrate to". There is: stock 2.55.0 migrates
/// `Shape::Branched` into `.git/reftable/` and back, and the two directions are
/// different code — files→reftable enumerates a store and writes one table,
/// reftable→files explodes one table into loose refs — so both are drawn here.
///
/// The migration is the whole measurement and it is visible without reading a
/// single reftable byte. Afterwards `probe_state` asks stock `for-each-ref`
/// (which must answer identically whichever backend it went through),
/// `config --list --local` (which must now carry `core.repositoryformatversion
/// = 1` and `extensions.refstorage = reftable`), and reads `.git/logs/**`
/// (which `--no-reflog` empties and the default form carries across). A port
/// that refuses the migration outright differs on the exit code; a port that
/// claimed to migrate and dropped a ref, a reflog or the declaration differs on
/// the digest.
///
/// Eight shapes rather than one, because what the migration has to carry
/// differs: `Linear` has one branch and one reflog, `Branched` adds a peeled
/// annotated tag, `Merged` a second parent, `TagChain` a three-deep tag chain
/// plus tags on a blob and a tree, `AmbiguousRef` nine refs across four
/// namespaces including a bare `refs/top`, `BehindRemote` remote-tracking refs
/// and a `HEAD` whose branch has an upstream, and `Detached` a `HEAD` that is
/// not a symref at all.
///
/// `Worktree` is drawn for the opposite reason: stock 2.55.0 **refuses** there
/// (`error: migrating repositories with worktrees is not supported yet`, exit
/// 255), so it is the one shape where the right answer is a refusal, and a port
/// that migrated it anyway would be the interesting failure. It is also why the
/// determinism note above does not need to cover `.git/worktrees`: no reftable
/// is ever written under it.
fn migrate(out: &mut Vec<Case>) {
    let to_reftable = |shape: Shape, out: &mut Vec<Case>| {
        out.push(Case::new("refs", &["refs", "migrate", "--ref-format=reftable"], shape));
    };
    to_reftable(Shape::Linear, out);
    to_reftable(Shape::Branched, out);
    to_reftable(Shape::Merged, out);
    to_reftable(Shape::TagChain, out);
    to_reftable(Shape::AmbiguousRef, out);
    to_reftable(Shape::BehindRemote, out);
    to_reftable(Shape::Detached, out);
    to_reftable(Shape::Worktree, out);

    // `--no-reflog` is the flag that decides whether the *second* store travels
    // with the first. With it the migrated repository has no `.git/logs` at all,
    // which `probe_reflogs` reads directly.
    out.push(Case::new(
        "refs",
        &["refs", "migrate", "--ref-format=reftable", "--no-reflog"],
        Shape::Branched,
    ));

    // The refusals, with their diagnostics pinned: an unknown format name, the
    // flag with no value, and the flag omitted entirely. Each is a distinct exit
    // code on stock 2.55.0 (255, 129, 129) and each was measured by hand to
    // produce byte-identical stderr on both sides before being marked strict.
    out.push(Case::strict("refs", &["refs", "migrate", "--ref-format=bogus"], Shape::Branched));
    out.push(Case::strict("refs", &["refs", "migrate", "--ref-format="], Shape::Branched));
    out.push(Case::strict("refs", &["refs", "migrate", "--no-reflog"], Shape::Branched));
    // Migrating a files repository *to* files: not a no-op, a refusal.
    out.push(Case::strict(
        "refs",
        &["refs", "migrate", "--ref-format=files", "--no-reflog"],
        Shape::Branched,
    ));
    out.push(Case::new("refs", &["refs", "migrate", "--ref-format=files"], Shape::Linear));
}

/// `extensions.refStorage` and `core.repositoryFormatVersion`: the repository's
/// own declaration of which backend its refs are in, and what a reader does
/// when it meets one it cannot serve.
///
/// # Why a declaration without a store is the right premise
///
/// The question worth asking is "what happens when a git meets a ref backend it
/// does not support", and the honest way to ask it is a repository that really
/// is in that format. No `Shape` is, one cannot be added from here, and the port
/// refuses to create one, so that experiment is out of reach (see the module
/// header, which records what it answers when run by hand). What *is* in reach
/// is the declaration alone: `extensions.refStorage = reftable` written into
/// `.git/config` over a store that is still loose files.
///
/// That is not a contrived state — it is exactly the state a half-finished
/// migration leaves behind, and it is the state every reader has to be safe in.
/// Stock 2.55.0 believes the declaration: it opens a reftable store, finds
/// nothing, and answers "no refs" — `show-ref` exits 1 printing nothing. A
/// reader that ignores the declaration answers with the loose refs instead, and
/// the two answers are indistinguishable to a caller that only checks the exit
/// code. That is the defect this group exists to make visible, and it is
/// visible on stdout and on the exit code, so no probe is needed for it.
///
/// # The four declarations, and what each one is for
///
/// * **`reftable` at format version 1** — honoured. The reader must go to a
///   store that is not there.
/// * **`bogusbackend` at format version 1** — rejected outright by stock, with
///   `error: invalid value for 'extensions.refstorage'` and a `fatal: bad config
///   line` naming the file, before the verb runs at all. A reader that starts
///   the verb anyway has skipped the whole validation step.
/// * **`files` at format version 1** — honoured and true, so every verb must
///   behave exactly as it does with no declaration at all. This is the control:
///   without it, "ignores `extensions.refStorage` entirely" and "reads it and
///   agrees" would look the same.
/// * **`reftable` at format version 0** — *not* honoured, because extensions are
///   only consulted from version 1 upward. This is the other control, and it is
///   the one a port is most likely to get wrong in the safe-looking direction:
///   refusing here would be refusing a repository stock reads fine.
///
/// Delivered from [`ConfigScope::Repo`], which appends to `.git/config` — so
/// `core.repositoryformatversion` appears twice in the file and the later value
/// wins on read, which is the behaviour `install_config` is documented to
/// produce and which was verified against stock for exactly this key before the
/// group was written. Nothing here asks git to *write* that key.
fn declared_backend(out: &mut Vec<Case>) {
    let declare = |format: &str| {
        vec![
            ConfigEntry::set(ConfigScope::Repo, "core.repositoryFormatVersion", "1"),
            ConfigEntry::set(ConfigScope::Repo, "extensions.refStorage", format),
        ]
    };

    // The readers. Each one reaches the ref store by a different path — a
    // full-store enumeration, a formatted enumeration, a single-name lookup, a
    // symref read, the store's self-report, and its consistency check — so a
    // port that consults the declaration in one place and not another shows up
    // as a partial failure rather than a uniform one.
    let readers: &[(&str, &[&str])] = &[
        ("show-ref", &["show-ref"]),
        ("show-ref", &["show-ref", "--verify", "refs/heads/main"]),
        ("for-each-ref", &["for-each-ref", "--format=%(refname) %(objectname)"]),
        ("symbolic-ref", &["symbolic-ref", "HEAD"]),
        ("rev-parse", &["rev-parse", "--show-ref-format"]),
        ("rev-parse", &["rev-parse", "HEAD"]),
        ("repo", &["repo", "info", "references.format"]),
        ("refs", &["refs", "verify"]),
        ("refs", &["refs", "list", "--format=%(refname)"]),
        ("reflog", &["reflog", "show", "HEAD"]),
    ];
    for format in ["reftable", "bogusbackend", "files"] {
        for (cmd, args) in readers {
            out.push(
                Case::new(cmd, args, Shape::Branched).with_scoped_config(declare(format)),
            );
        }
    }

    // The writers, against the same declarations. A write to a store the reader
    // could not open is the case that turns a wrong answer into a wrong
    // repository, and `probe_state` reads the result back with stock.
    for format in ["reftable", "bogusbackend"] {
        out.push(
            Case::new("update-ref", &["update-ref", "refs/heads/declared", "HEAD"], Shape::Branched)
                .with_scoped_config(declare(format)),
        );
        out.push(
            Case::new("pack-refs", &["pack-refs", "--all"], Shape::Branched)
                .with_scoped_config(declare(format)),
        );
        out.push(
            Case::new("symbolic-ref", &["symbolic-ref", "HEAD", "refs/heads/feature"], Shape::Branched)
                .with_scoped_config(declare(format)),
        );
    }

    // Version 0: the extension is present and must be ignored by both sides.
    let ignored = || {
        vec![ConfigEntry::set(ConfigScope::Repo, "extensions.refStorage", "reftable")]
    };
    for (cmd, args) in [
        ("show-ref", &["show-ref"][..]),
        ("rev-parse", &["rev-parse", "--show-ref-format"][..]),
        ("refs", &["refs", "verify"][..]),
        ("update-ref", &["update-ref", "refs/heads/ignored", "HEAD"][..]),
    ] {
        out.push(Case::new(cmd, args, Shape::Branched).with_scoped_config(ignored()));
    }

    // The version bump on its own, with no extension: everything must behave as
    // it does at version 0, so a port that treats "version 1" as "unsupported"
    // is separated from one that reads the extension it names.
    for (cmd, args) in [
        ("show-ref", &["show-ref"][..]),
        ("rev-parse", &["rev-parse", "--show-ref-format"][..]),
    ] {
        out.push(Case::new(cmd, args, Shape::Branched).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Repo, "core.repositoryFormatVersion", "1"),
        ]));
    }

    // An unknown extension at version 1, which is the generic form of the same
    // rule: git refuses a repository declaring a capability it does not have.
    out.push(Case::new("show-ref", &["show-ref"], Shape::Branched).with_scoped_config(vec![
        ConfigEntry::set(ConfigScope::Repo, "core.repositoryFormatVersion", "1"),
        ConfigEntry::set(ConfigScope::Repo, "extensions.noSuchExtension", "true"),
    ]));

    // `--show-ref-format` with no repository configuration at all, on the shapes
    // whose layout differs: a linked worktree and a bare peer share one ref
    // store with their main repository, so both must report the same format.
    out.push(Case::new("rev-parse", &["rev-parse", "--show-ref-format"], Shape::Worktree).in_dir("wt"));
    out.push(Case::new("rev-parse", &["rev-parse", "--show-ref-format"], Shape::Submodule).in_dir("sub"));
}

// ---------------------------------------------------------------------------
// `update-ref --stdin`: the transaction protocol
// ---------------------------------------------------------------------------

/// A three-command batch driven through the explicit state machine.
const TX_EXPLICIT: &[u8] =
    b"start\nupdate refs/heads/newa HEAD\ncreate refs/heads/newb HEAD\nprepare\ncommit\n";
/// Two whole transactions in one stream: `commit` ends the first and the next
/// command implicitly opens the second.
const TX_TWO_TRANSACTIONS: &[u8] =
    b"create refs/heads/one HEAD\ncommit\ncreate refs/heads/two HEAD\ncommit\n";
/// `abort` after work has been queued: nothing may reach the store.
const TX_ABORT: &[u8] = b"start\ncreate refs/heads/aborted HEAD\nupdate refs/heads/main HEAD~1\nabort\n";
/// Three good commands and a fourth that cannot succeed. All four roll back, or
/// the store is left in a state no single command produced.
const TX_ROLLBACK_TAIL: &[u8] =
    b"create refs/heads/p1 HEAD\ncreate refs/heads/p2 HEAD\ncreate refs/heads/p3 HEAD\ndelete refs/heads/nope\n";
/// The same shape with the failure in the middle, and with a directory/file
/// conflict rather than a missing ref as the cause.
const TX_ROLLBACK_DF: &[u8] = b"create refs/heads/p1 HEAD\ncreate refs/heads/main/sub HEAD\n";
/// A batch whose second command names a revision that does not resolve.
const TX_ROLLBACK_BADREV: &[u8] = b"create refs/heads/p1 HEAD\nupdate refs/heads/p2 no-such-rev\n";
/// The same name created twice inside one transaction.
const TX_DUPLICATE_NAME: &[u8] =
    b"create refs/heads/a HEAD\ncreate refs/heads/b HEAD\ncreate refs/heads/a HEAD\n";
/// The same name deleted twice inside one transaction.
const TX_DUPLICATE_DELETE: &[u8] = b"delete refs/heads/feature\ndelete refs/heads/feature\n";
/// Four refs removed and a fifth moved, atomically — the shape a mirror push or
/// a prune runs.
const TX_BULK: &[u8] = b"delete refs/tags/v0.1.0\ndelete refs/tags/v0.2.0\ndelete refs/heads/feature\nupdate refs/heads/main HEAD~1\n";

/// An old-value precondition that holds: `main` really is at this id.
const TX_OLD_MATCHES: &[u8] =
    b"update refs/heads/main edfab1b71619a22120a8da1a3d85d68e0200290a 5915d79de18d919476d339c8b8efda1d9bb166e2\n";
/// One that does not: `main` is not at the root commit.
const TX_OLD_MISMATCH: &[u8] =
    b"update refs/heads/main 5915d79de18d919476d339c8b8efda1d9bb166e2 edfab1b71619a22120a8da1a3d85d68e0200290a\n";
/// A delete guarded by the right id.
const TX_DELETE_GUARDED: &[u8] =
    b"delete refs/heads/feature 07e86d1fedb713fbc84a754c98ea4bfe53316416\n";
/// `verify` with the zero oid against a ref that exists: the "must not exist"
/// spelling, which must fail here.
const TX_VERIFY_ZERO: &[u8] =
    b"verify refs/heads/main 0000000000000000000000000000000000000000\n";
/// `verify` with no old value against a ref that does not exist: the "must not
/// exist" assertion in its other spelling, which must pass.
const TX_VERIFY_ABSENT: &[u8] = b"verify refs/heads/main\nverify refs/heads/nope\n";
/// A good update followed by a `verify` that cannot hold: the guard rolls back
/// work that had already been accepted.
const TX_VERIFY_ROLLS_BACK: &[u8] =
    b"update refs/heads/main HEAD~1\nverify refs/heads/feature deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n";

/// A quoted C-style argument, which the parser accepts in the newline form.
const TX_QUOTED: &[u8] = b"update refs/heads/quoted \"refs/heads/feature\"\n";
/// A quoted ref name containing a space — legal in a ref name, unreachable
/// unquoted.
const TX_QUOTED_SPACE: &[u8] = b"create \"refs/heads/with space\" HEAD\n";
/// A blank line between two commands.
const TX_BLANK_LINE: &[u8] = b"create refs/heads/e HEAD\n\ncreate refs/heads/f HEAD\n";
/// A final command with **no trailing newline**. Stock refuses the whole batch.
const TX_NO_TRAILING_NEWLINE: &[u8] = b"create refs/heads/g HEAD";
/// An unknown transaction verb.
const TX_UNKNOWN_COMMAND: &[u8] = b"bogus refs/heads/x HEAD\n";
/// An unknown `option`.
const TX_UNKNOWN_OPTION: &[u8] = b"option unknown-opt\nupdate refs/heads/x HEAD\n";
/// A command with no arguments at all.
const TX_NO_ARGUMENTS: &[u8] = b"update\n";
/// `update` with a ref and no new value.
const TX_MISSING_NEW_VALUE: &[u8] = b"update refs/heads/x\n";
/// `create` with a ref and no new value.
const TX_CREATE_NO_VALUE: &[u8] = b"create refs/heads/x\n";
/// More arguments than the verb takes.
const TX_TOO_MANY_ARGUMENTS: &[u8] = b"update refs/heads/x HEAD extra-extra HEAD\n";
/// `commit` with no transaction open.
const TX_BARE_COMMIT: &[u8] = b"commit\n";
/// `abort` with no transaction open.
const TX_BARE_ABORT: &[u8] = b"abort\n";
/// `commit` twice.
const TX_DOUBLE_COMMIT: &[u8] = b"prepare\ncommit\ncommit\n";
/// `start` twice.
const TX_DOUBLE_START: &[u8] = b"start\nstart\n";

/// `-z` framing: the command word is still separated from the ref by a space,
/// and every value is NUL-terminated including the omitted old value.
const TXZ_UPDATE_DELETE: &[u8] =
    b"update refs/heads/zed\0refs/heads/feature\0\0delete refs/tags/v0.1.0\0\0";
/// `-z` with the old value supplied.
const TXZ_GUARDED: &[u8] =
    b"update refs/heads/main\0edfab1b71619a22120a8da1a3d85d68e0200290a\05915d79de18d919476d339c8b8efda1d9bb166e2\0";
/// `-z` with a guard that does not hold.
const TXZ_GUARD_FAILS: &[u8] =
    b"update refs/heads/main\0edfab1b71619a22120a8da1a3d85d68e0200290a\007e86d1fedb713fbc84a754c98ea4bfe53316416\0";
/// `-z` with the *command word* NUL-terminated, which is the natural wrong
/// guess about the framing and is a hard error on stock.
const TXZ_WRONG_FRAMING: &[u8] = b"update\0refs/heads/zed\0refs/heads/feature\0\0";
/// A newline-framed payload fed to `-z`: the whole thing is one command name.
const TXZ_NEWLINE_PAYLOAD: &[u8] = b"create refs/heads/x HEAD\n";
/// `-z` deletion with an empty old value, which is not the same as an absent one.
const TXZ_DELETE_EMPTY_OLD: &[u8] = b"delete refs/heads/feature\0\0";

/// Two commands whose refs already have a reflog, run under a configuration
/// that says not to log. Git's rule is that an *existing* log keeps being
/// appended to regardless, and this is the payload that separates the two.
const TX_LOGGED_AND_NEW: &[u8] = b"create refs/heads/n1 HEAD\nupdate refs/heads/main HEAD~1\n";
/// Refs in namespaces git does not log by default (`refs/tags`, `refs/notes`),
/// so `core.logAllRefUpdates = always` is the only thing that would create one.
const TX_UNLOGGED_NAMESPACES: &[u8] = b"create refs/tags/tnew HEAD\nupdate refs/notes/x HEAD\n";

/// `update-ref --stdin`: the transaction, in full.
///
/// `corpus.rs::stdin_driven` established that the port reads the protocol at
/// all. What it cannot show is whether the *transaction* is real, and that is
/// the only property of this command that matters to a caller: `git push`,
/// `git fetch --prune` and every mirroring script hand git a batch of dozens of
/// updates and rely on all-or-nothing. A port that applies commands as it parses
/// them passes every single-command case and destroys a repository on the first
/// batch whose last line is bad — and the resulting half-applied state is worse
/// than the rejection, because the caller was told the whole thing failed.
///
/// Every rollback case below is therefore built the same way: work that would
/// succeed on its own, followed (or preceded) by one command that cannot, with
/// the failure caused a different way each time — a name that already exists, a
/// name that does not, a directory/file collision, an unresolvable revision, a
/// duplicate inside one batch, and an old-value guard. `probe_state` reads the
/// refs back with stock, so a partial apply is a state difference on a case
/// whose stdout and exit code may well agree.
fn transaction_protocol(out: &mut Vec<Case>) {
    let tx = |shape: Shape, payload: &'static [u8], out: &mut Vec<Case>| {
        out.push(Case::with_stdin("update-ref", &["update-ref", "--stdin"], shape, payload));
    };
    let txz = |shape: Shape, payload: &'static [u8], out: &mut Vec<Case>| {
        out.push(Case::with_stdin("update-ref", &["update-ref", "--stdin", "-z"], shape, payload));
    };

    // The state machine.
    tx(Shape::Branched, TX_EXPLICIT, out);
    tx(Shape::Branched, TX_TWO_TRANSACTIONS, out);
    tx(Shape::Branched, TX_ABORT, out);
    tx(Shape::Branched, TX_DOUBLE_COMMIT, out);
    tx(Shape::Branched, TX_DOUBLE_START, out);
    tx(Shape::Branched, TX_BARE_COMMIT, out);
    tx(Shape::Branched, TX_BARE_ABORT, out);

    // Atomicity, six ways of failing.
    tx(Shape::Branched, TX_ROLLBACK_TAIL, out);
    tx(Shape::Branched, TX_ROLLBACK_DF, out);
    tx(Shape::Branched, TX_ROLLBACK_BADREV, out);
    tx(Shape::Branched, TX_DUPLICATE_NAME, out);
    tx(Shape::Branched, TX_DUPLICATE_DELETE, out);
    tx(Shape::Branched, TX_VERIFY_ROLLS_BACK, out);

    // The bulk batch, on the two shapes with enough refs to make it a batch.
    tx(Shape::Branched, TX_BULK, out);
    out.push(Case::with_stdin("update-ref", &["update-ref", "--stdin"], Shape::TagChain, TX_ROLLBACK_TAIL));

    // Old-value preconditions.
    tx(Shape::Branched, TX_OLD_MATCHES, out);
    tx(Shape::Branched, TX_OLD_MISMATCH, out);
    tx(Shape::Branched, TX_DELETE_GUARDED, out);
    tx(Shape::Branched, TX_VERIFY_ZERO, out);
    tx(Shape::Branched, TX_VERIFY_ABSENT, out);

    // Parsing: quoting, blank lines, and the line-termination rule. The last one
    // is `strict` because the whole difference is the diagnostic and the exit
    // code — stock refuses an unterminated final command with `fatal: create
    // refs/heads/g: extra input:` and applies nothing.
    tx(Shape::Branched, TX_QUOTED, out);
    tx(Shape::Branched, TX_QUOTED_SPACE, out);
    tx(Shape::Branched, TX_BLANK_LINE, out);
    out.push(strict_stdin("update-ref", &["update-ref", "--stdin"], Shape::Branched, TX_NO_TRAILING_NEWLINE));

    // Argument-count and vocabulary errors.
    tx(Shape::Branched, TX_UNKNOWN_COMMAND, out);
    tx(Shape::Branched, TX_UNKNOWN_OPTION, out);
    tx(Shape::Branched, TX_NO_ARGUMENTS, out);
    tx(Shape::Branched, TX_MISSING_NEW_VALUE, out);
    tx(Shape::Branched, TX_CREATE_NO_VALUE, out);
    tx(Shape::Branched, TX_TOO_MANY_ARGUMENTS, out);

    // `-z`: a second framing of the same grammar, with its own parser.
    txz(Shape::Branched, TXZ_UPDATE_DELETE, out);
    txz(Shape::Branched, TXZ_GUARDED, out);
    txz(Shape::Branched, TXZ_GUARD_FAILS, out);
    txz(Shape::Branched, TXZ_WRONG_FRAMING, out);
    txz(Shape::Branched, TXZ_NEWLINE_PAYLOAD, out);
    txz(Shape::Branched, TXZ_DELETE_EMPTY_OLD, out);

    // The same transaction against shapes whose HEAD is not a branch tip: a
    // detached HEAD and a linked worktree each own a `HEAD` the transaction may
    // or may not be allowed to move.
    tx(Shape::Detached, TX_EXPLICIT, out);
    out.push(
        Case::with_stdin("update-ref", &["update-ref", "--stdin"], Shape::Worktree, TX_EXPLICIT)
            .in_dir("wt"),
    );
}

/// [`Case::with_stdin`] with stderr compared byte for byte, which no
/// constructor spells: `Case::strict` takes no payload and `with_stdin` is not
/// strict, and this file needs both wherever the difference between the two
/// sides *is* the diagnostic.
fn strict_stdin(
    cmd: &'static str,
    args: &[&str],
    shape: Shape,
    stdin: &'static [u8],
) -> Case {
    let mut case = Case::with_stdin(cmd, args, shape, stdin);
    case.compare_stderr = true;
    case
}

// ---------------------------------------------------------------------------
// The symref half of the transaction protocol
// ---------------------------------------------------------------------------

/// `symref-create` for a name that does not exist yet.
const SYM_CREATE: &[u8] = b"symref-create refs/heads/symlinkref refs/heads/feature\n";
/// `symref-create` for a name that exists as an ordinary ref.
const SYM_CREATE_EXISTS: &[u8] = b"symref-create refs/heads/main refs/heads/feature\n";
/// `symref-create` pointing at a ref that does not exist — a dangling symref,
/// which is legal.
const SYM_CREATE_DANGLING: &[u8] = b"symref-create refs/sym/one refs/heads/nonexistent\n";
/// `symref-create HEAD`, which is asking to replace the one symref every
/// repository already has.
const SYM_CREATE_HEAD: &[u8] = b"symref-create HEAD refs/heads/feature\n";
/// `symref-update HEAD` in the default (deref) mode. Stock dereferences `HEAD`
/// first and rewrites *the branch it names* into a symref.
const SYM_UPDATE_HEAD_DEREF: &[u8] = b"symref-update HEAD refs/heads/feature\n";
/// The same with `option no-deref`, which retargets `HEAD` itself.
const SYM_UPDATE_HEAD_NODEREF: &[u8] = b"option no-deref\nsymref-update HEAD refs/heads/feature\n";
/// `symref-update` on a name that is not a symref at all.
const SYM_UPDATE_PLAIN: &[u8] = b"option no-deref\nsymref-update refs/heads/main refs/heads/feature\n";
/// `symref-delete` in deref mode, which stock refuses outright.
const SYM_DELETE_DEREF: &[u8] = b"start\nsymref-delete HEAD refs/heads/main\nabort\n";
/// `symref-delete` with `no-deref` and the right old target.
const SYM_DELETE_NODEREF: &[u8] = b"option no-deref\nsymref-delete HEAD refs/heads/main\n";
/// `symref-delete` with `no-deref` and no old target at all.
const SYM_DELETE_NODEREF_UNGUARDED: &[u8] = b"option no-deref\nsymref-delete HEAD\n";
/// `symref-verify` in deref mode, which stock also refuses.
const SYM_VERIFY_DEREF: &[u8] = b"symref-verify HEAD refs/heads/main\n";
/// The same with a target that does not match, so the refusal and the failed
/// precondition are two different answers to tell apart.
const SYM_VERIFY_DEREF_MISMATCH: &[u8] = b"symref-verify HEAD refs/heads/feature\n";
/// `symref-verify` with `no-deref`, holding.
const SYM_VERIFY_NODEREF: &[u8] = b"option no-deref\nsymref-verify HEAD refs/heads/main\n";
/// `symref-verify` with `no-deref`, not holding.
const SYM_VERIFY_NODEREF_MISMATCH: &[u8] = b"option no-deref\nsymref-verify HEAD refs/heads/feature\n";
/// An ordinary `delete` of `HEAD`, with and without deref: one removes the
/// branch `HEAD` names, the other removes `HEAD`.
const SYM_DELETE_PLAIN_DEREF: &[u8] = b"delete HEAD\n";
const SYM_DELETE_PLAIN_NODEREF: &[u8] = b"option no-deref\ndelete HEAD\n";

/// The `symref-*` verbs, and the `no-deref` option that changes what every one
/// of them means.
///
/// This is the newest part of the `update-ref` grammar and the part with the
/// most implementable-looking wrong answer. Four of the six verbs behave
/// differently depending on whether the transaction is in deref mode, two of
/// them **refuse to run at all** in the default mode, and the difference is
/// invisible on the happy path: `symref-update HEAD <target>` succeeds either
/// way and leaves two entirely different repositories behind. Stock rewrites
/// the branch `HEAD` points at into a symref; a reader that skips the deref
/// rewrites `HEAD`. Both exit 0, both print nothing, and only the ref set says
/// which one happened — which is why every case here is paired with its
/// `no-deref` twin rather than drawn on its own.
///
/// The two refusals (`symref-delete` and `symref-verify` in deref mode) are
/// `strict`: the exit code is the whole observable difference for one of them
/// and the message is the whole difference for the other, so pinning only one
/// surface would miss half of it.
fn symref_protocol(out: &mut Vec<Case>) {
    let sym = |payload: &'static [u8], out: &mut Vec<Case>| {
        out.push(Case::with_stdin("update-ref", &["update-ref", "--stdin"], Shape::Branched, payload));
    };
    sym(SYM_CREATE, out);
    sym(SYM_CREATE_EXISTS, out);
    sym(SYM_CREATE_DANGLING, out);
    sym(SYM_CREATE_HEAD, out);
    sym(SYM_UPDATE_HEAD_DEREF, out);
    sym(SYM_UPDATE_HEAD_NODEREF, out);
    sym(SYM_UPDATE_PLAIN, out);
    sym(SYM_DELETE_NODEREF, out);
    sym(SYM_DELETE_NODEREF_UNGUARDED, out);
    sym(SYM_VERIFY_NODEREF, out);
    sym(SYM_VERIFY_NODEREF_MISMATCH, out);
    sym(SYM_DELETE_PLAIN_DEREF, out);
    sym(SYM_DELETE_PLAIN_NODEREF, out);

    for payload in [SYM_DELETE_DEREF, SYM_VERIFY_DEREF, SYM_VERIFY_DEREF_MISMATCH] {
        out.push(strict_stdin("update-ref", &["update-ref", "--stdin"], Shape::Branched, payload));
    }

    // The same protocol against a detached HEAD, where the deref has nothing to
    // dereference to.
    out.push(Case::with_stdin("update-ref", &["update-ref", "--stdin"], Shape::Detached, SYM_UPDATE_HEAD_DEREF));
    out.push(Case::with_stdin("update-ref", &["update-ref", "--stdin"], Shape::Detached, SYM_CREATE_HEAD));

    // `symbolic-ref`, the porcelain spelling of the same operations, on the
    // edges `plumbing_refs.rs` does not reach: a symref that is not `HEAD`, a
    // symref pointing at itself, a target that is not a ref at all, and the
    // deletion of a name that was never symbolic.
    let sr = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("symbolic-ref", args, Shape::Branched));
    };
    sr(&["symbolic-ref", "refs/heads/alias", "refs/heads/feature"], out);
    sr(&["symbolic-ref", "HEAD", "HEAD"], out);
    sr(&["symbolic-ref", "HEAD", "refs/tags/v0.1.0"], out);
    sr(&["symbolic-ref", "HEAD", "refs/heads/bad..name"], out);
    sr(&["symbolic-ref", "--short", "refs/heads/main"], out);
    sr(&["symbolic-ref", "-d", "refs/heads/main"], out);
    sr(&["symbolic-ref", "-q", "-d", "refs/heads/main"], out);
    sr(&["symbolic-ref", "-d", "no-such-ref"], out);
    sr(&["symbolic-ref", "-m", "parity retarget", "HEAD", "refs/heads/main"], out);
    sr(&["symbolic-ref", "ALLCAPS", "refs/heads/main"], out);
    sr(&["symbolic-ref", "lowercase", "refs/heads/main"], out);
    sr(&["symbolic-ref", "--short", "HEAD", "extra"], out);
    sr(&["symbolic-ref"], out);
    // `plumbing_refs.rs` already reads `symbolic-ref HEAD` and `-q HEAD` across
    // `READ_SHAPES`, which includes `Detached`; nothing is repeated here.
}

/// The **reflog store**: a second, append-only store that shadows the first,
/// with its own verbs and its own policy about when a write reaches it.
///
/// `probe_reflogs` reads `.git/logs/**` verbatim, so this is one of the few
/// stores the harness compares byte for byte — including the message after the
/// tab, which is the field a port is most likely to get almost right. Nothing
/// here reads a relative date: the entries the fixture carries were written
/// under `env::harden`'s pinned committer clock, `--expire=all` and
/// `--expire=never` are absolute, and `--expire=now` is the wall clock and is
/// deliberately absent (`plumbing_refs.rs` draws it; this file does not repeat
/// it).
///
/// The policy half is the part with no coverage anywhere. `core.logAllRefUpdates`
/// is not a boolean with two answers, it has three, and the third is the rule
/// that catches implementations out: **an existing log keeps being appended to
/// even when logging is off**, because git checks for the file before it checks
/// the setting. So `false` does not mean "no reflog writes", it means "no *new*
/// reflog files" — and a port that reads the setting first stops recording
/// `HEAD`'s own history the moment a repository sets it.
fn reflog_store(out: &mut Vec<Case>) {
    // `reflog write`, `drop` and `delete --updateref` are named in git's own
    // synopsis and appear in no case in the corpus.
    out.push(Case::new(
        "reflog",
        &[
            "reflog",
            "write",
            "refs/heads/main",
            BRANCHED_MAIN,
            BRANCHED_ROOT,
            "parity: written by hand",
        ],
        Shape::Branched,
    ));
    out.push(Case::new(
        "reflog",
        &["reflog", "write", "refs/heads/unlogged", BRANCHED_ROOT, BRANCHED_MAIN, "parity: new log"],
        Shape::Branched,
    ));
    out.push(Case::new("reflog", &["reflog", "drop", "refs/heads/main"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "drop", "--all"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "drop", "--all", "--single-worktree"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "drop", "refs/heads/nope"], Shape::Branched));

    // `--updateref` moves the ref to whatever the surviving top entry names, and
    // for `HEAD` that must go *through* the symref rather than overwrite it.
    out.push(Case::new("reflog", &["reflog", "delete", "--updateref", "--rewrite", "HEAD@{0}"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "delete", "--dry-run", "HEAD@{0}"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "delete", "--rewrite", "refs/heads/main@{1}"], Shape::Branched));

    // Absolute expiry bounds only.
    out.push(Case::new("reflog", &["reflog", "expire", "--all", "--expire=all"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "expire", "--stale-fix", "--all", "--expire=all"], Shape::Branched));
    out.push(Case::new(
        "reflog",
        &["reflog", "expire", "--expire=all", "--updateref", "--rewrite", "refs/heads/main"],
        Shape::Branched,
    ));
    out.push(Case::new("reflog", &["reflog", "expire", "--all", "--expire=all"], Shape::BehindRemote));

    // The three-valued policy, delivered from the command line so `.git/config`
    // keeps exactly one value for the key.
    let logging = |value: &'static str| vec![("core.logAllRefUpdates", value)];
    out.push(
        Case::with_stdin("update-ref", &["update-ref", "--stdin"], Shape::Branched, TX_LOGGED_AND_NEW)
            .with_config(&logging("false")),
    );
    out.push(
        Case::with_stdin("update-ref", &["update-ref", "--stdin"], Shape::Branched, TX_LOGGED_AND_NEW)
            .with_config(&logging("true")),
    );
    out.push(
        Case::with_stdin("update-ref", &["update-ref", "--stdin"], Shape::Branched, TX_UNLOGGED_NAMESPACES)
            .with_config(&logging("always")),
    );
    out.push(
        Case::with_stdin("update-ref", &["update-ref", "--stdin"], Shape::Branched, TX_UNLOGGED_NAMESPACES),
    );
    out.push(
        Case::new("update-ref", &["update-ref", "refs/heads/main", "HEAD~1"], Shape::Branched)
            .with_config(&logging("false")),
    );
    // `--create-reflog` is the per-invocation override of the same policy.
    out.push(
        Case::new("update-ref", &["update-ref", "--create-reflog", "refs/heads/n2", "HEAD"], Shape::Branched)
            .with_config(&logging("false")),
    );
    out.push(
        Case::new("update-ref", &["update-ref", "--create-reflog", "refs/tags/logged", "HEAD"], Shape::Branched),
    );

    // Which store a reflog lands in: `logs/HEAD` versus `logs/refs/...`. A
    // symbolic update touches both, a `no-deref` update touches one.
    out.push(Case::new("update-ref", &["update-ref", "HEAD", "HEAD~1"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "exists", "refs/tags/v0.1.0"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "exists", "refs/heads/feature"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "show", "--all", "--format=%gd"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "show", "refs/heads/feature"], Shape::Branched));
    out.push(Case::new("reflog", &["reflog", "list"], Shape::BehindRemote));
}

/// `pack-refs` and its alias `refs optimize`: moving refs from one physical
/// place to another without changing a single answer.
///
/// The measurement is indirect by construction — the command prints nothing and
/// the ref set is supposed to be identical afterwards — so what it actually
/// pins is that the *round trip* is lossless: `probe_state` re-reads every ref
/// with stock, `probe_reflogs` re-reads the logs, `probe_interop` runs stock
/// `fsck --strict` over the result, and the accelerator line records that
/// `packed-refs` now exists. A pack that drops a ref, loses an annotated tag's
/// peel, or leaves a stale loose file shadowing a packed line is a difference in
/// one of those four.
///
/// `plumbing_refs.rs` packs `Linear`, `Branched` and `Merged`, and
/// `stdin_plumbing.rs` takes `--auto` and two `--include`/`--exclude` pairs on
/// `Branched`. What neither can reach is a ref set with anything *interesting*
/// in it, and that is the whole of what is added here: `BehindRemote` has
/// remote-tracking refs under `refs/remotes/` and two upstream-configured
/// branches, `TagChain` has a three-deep chain of annotated tags plus tags on a
/// blob and on a tree — five peels that a `fully-peeled` header claims to have
/// resolved — `AmbiguousRef` has nine refs across four namespaces including a
/// bare `refs/top` that no porcelain writes, and `Octopus` has a four-parent
/// merge with a branch left beside it.
fn packing(out: &mut Vec<Case>) {
    for shape in [Shape::BehindRemote, Shape::TagChain, Shape::AmbiguousRef, Shape::Octopus] {
        out.push(Case::new("pack-refs", &["pack-refs", "--all"], shape));
        out.push(Case::new("pack-refs", &["pack-refs"], shape));
    }
    out.push(Case::new("pack-refs", &["pack-refs", "--all", "--no-prune"], Shape::TagChain));
    out.push(Case::new("pack-refs", &["pack-refs", "--all", "--auto"], Shape::BehindRemote));
    out.push(Case::new(
        "pack-refs",
        &["pack-refs", "--include", "refs/remotes/*"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "pack-refs",
        &["pack-refs", "--all", "--exclude", "refs/remotes/*"],
        Shape::BehindRemote,
    ));
    out.push(Case::new(
        "pack-refs",
        &["pack-refs", "--include", "refs/tags/*", "--exclude", "refs/tags/blobtag"],
        Shape::TagChain,
    ));
    // A selection that matches nothing, and one that contradicts itself: both
    // must leave the store exactly as it was.
    out.push(Case::new("pack-refs", &["pack-refs", "--all", "--include", "refs/nothing/*"], Shape::Branched));
    out.push(Case::new(
        "pack-refs",
        &["pack-refs", "--include", "refs/heads/main", "--exclude", "refs/heads/main"],
        Shape::Branched,
    ));
    // A linked worktree shares the common ref store; packing from inside it must
    // pack the same refs.
    out.push(Case::new("pack-refs", &["pack-refs", "--all"], Shape::Worktree).in_dir("wt"));

    // `refs optimize` is documented as an alias for `pack-refs`, which means the
    // aliasing itself is a claim worth checking: the same flags through the other
    // name must reach the same code.
    out.push(Case::new("refs", &["refs", "optimize", "--all"], Shape::Branched));
    out.push(Case::new("refs", &["refs", "optimize", "--all", "--no-prune"], Shape::Branched));
    out.push(Case::new("refs", &["refs", "optimize", "--auto"], Shape::Branched));
    out.push(Case::new("refs", &["refs", "optimize"], Shape::TagChain));
    out.push(Case::new("refs", &["refs", "optimize", "--all"], Shape::BehindRemote));
    out.push(Case::new(
        "refs",
        &["refs", "optimize", "--include", "refs/tags/*", "--exclude", "refs/tags/v0.1.0"],
        Shape::Branched,
    ));
    out.push(Case::new("refs", &["refs", "optimize", "--bogus-flag"], Shape::Linear));

    // `refs verify` over the ref-rich shapes, which is the store's own
    // consistency check rather than a re-read of it.
    for shape in [Shape::TagChain, Shape::AmbiguousRef, Shape::BehindRemote, Shape::Worktree] {
        out.push(Case::new("refs", &["refs", "verify", "--strict", "--verbose"], shape));
    }
}

/// `show-ref --exclude-existing`: the one `show-ref` mode that reads stdin, and
/// therefore the one that could not be measured at all before `Case::stdin`.
///
/// It is a filter, not a query — it reads candidate ref names on stdin and
/// prints back the ones the repository does **not** have — which makes it the
/// exact inverse of every other case in this file, and the mode `git fetch`'s
/// tag-following path is built on. `plumbing_refs.rs` does not name it.
///
/// The payloads are chosen so each one exercises a different branch: names that
/// all exist, names that all do not, a mixture, a name that is not a well-formed
/// ref at all, and the `=<pattern>` form that restricts which prefix counts as
/// "existing".
fn exclude_existing(out: &mut Vec<Case>) {
    const ALL_KINDS: &[u8] =
        b"refs/heads/main\nrefs/heads/nope\nrefs/tags/v0.1.0\nrefs/tags/absent\n";
    const ONLY_EXISTING: &[u8] = b"refs/heads/main\nrefs/tags/v0.2.0\n";
    const ONLY_MISSING: &[u8] = b"refs/heads/nope\nrefs/tags/absent\n";
    const MALFORMED: &[u8] = b"not-a-ref-name\nrefs/heads/feature\n";
    const WITH_TRAILING_FIELD: &[u8] = b"refs/heads/main extra field\nrefs/heads/nope trailer\n";
    const EMPTY_LINE: &[u8] = b"\nrefs/heads/main\n";

    let ee = |args: &[&str], payload: &'static [u8], out: &mut Vec<Case>| {
        out.push(Case::with_stdin("show-ref", args, Shape::Branched, payload));
    };
    ee(&["show-ref", "--exclude-existing"], ALL_KINDS, out);
    ee(&["show-ref", "--exclude-existing"], ONLY_EXISTING, out);
    ee(&["show-ref", "--exclude-existing"], ONLY_MISSING, out);
    ee(&["show-ref", "--exclude-existing"], MALFORMED, out);
    ee(&["show-ref", "--exclude-existing"], WITH_TRAILING_FIELD, out);
    ee(&["show-ref", "--exclude-existing"], EMPTY_LINE, out);
    ee(&["show-ref", "--exclude-existing=refs/heads/"], ALL_KINDS, out);
    ee(&["show-ref", "--exclude-existing=refs/tags/"], ALL_KINDS, out);
    ee(&["show-ref", "--exclude-existing=refs/nothing/"], ALL_KINDS, out);
    // The flag combined with a mode it contradicts, and with a positional
    // pattern it has no use for.
    ee(&["show-ref", "--exclude-existing", "--heads"], ALL_KINDS, out);
    ee(&["show-ref", "--exclude-existing", "refs/heads/main"], ALL_KINDS, out);

    // The rest of `show-ref`'s lookup surface that `plumbing_refs.rs` leaves
    // open: `--verify` and `--exists` asked about `HEAD` (a root ref, not under
    // `refs/`), several names in one invocation, and the abbreviation width.
    let sr = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("show-ref", args, Shape::Branched));
    };
    sr(&["show-ref", "--verify", "HEAD"], out);
    sr(&["show-ref", "--verify", "--quiet", "HEAD"], out);
    sr(&["show-ref", "--exists", "HEAD"], out);
    sr(&["show-ref", "--exists", "refs/tags/v0.2.0"], out);
    sr(&["show-ref", "--verify", "refs/heads/main", "refs/heads/feature"], out);
    sr(&["show-ref", "--verify", "refs/heads/main", "refs/heads/nope"], out);
    sr(&["show-ref", "--head", "--dereference"], out);
    sr(&["show-ref", "--branches", "--tags", "--dereference"], out);
    sr(&["show-ref", "--hash=8", "--heads"], out);
    sr(&["show-ref", "v0.1.0", "v0.2.0"], out);
}

/// `for-each-ref` atoms that read **two** stores against each other.
///
/// `plumbing_refs.rs` owns the format engine and covers `%(upstream)` and
/// `%(upstream:short)` — both of which are a single lookup of a name recorded in
/// `.git/config`. The `:track` family is a different question entirely: it
/// resolves the local ref *and* the remote-tracking ref, walks between them, and
/// renders a count. It has no correct answer on a shape with no remote, which is
/// why the atom is here on `Shape::BehindRemote` — three commits behind on one
/// branch, diverged on the other — and why it was never measured before.
///
/// The rest of this group is the selection and ordering surface that decides
/// *which* store entries come back and in what order: several `--sort` keys at
/// once (a stable multi-key sort, not one comparison), `--no-sort` (the store's
/// own order), `--omit-empty` (a row that produces nothing must not produce a
/// blank line), and `%(if:equals=)`. Each is an invocation `plumbing_refs.rs`
/// does not make.
fn two_store_atoms(out: &mut Vec<Case>) {
    let br = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("for-each-ref", args, Shape::BehindRemote));
    };
    br(&["for-each-ref", "--format=%(refname:short)|%(upstream:track)"], out);
    br(&["for-each-ref", "--format=%(refname:short)|%(upstream:trackshort)"], out);
    br(&["for-each-ref", "--format=%(upstream:track,nobracket)"], out);
    br(&["for-each-ref", "--format=%(upstream:remotename)|%(upstream:remoteref)"], out);
    br(&["for-each-ref", "--format=%(push:track)|%(push:remotename)"], out);
    br(&["for-each-ref", "--omit-empty", "--format=%(upstream:track)"], out);
    br(&["for-each-ref", "--format=%(ahead-behind:refs/remotes/origin/main)"], out);
    br(&["for-each-ref", "--sort=refname", "--sort=-objectname", "--format=%(refname)"], out);
    br(&["for-each-ref", "--merged", "refs/remotes/origin/main", "--format=%(refname)"], out);
    br(&["for-each-ref", "--no-merged", "refs/remotes/origin/main", "--format=%(refname)"], out);

    let bn = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("for-each-ref", args, Shape::Branched));
    };
    bn(&["for-each-ref", "--sort=-committerdate", "--sort=refname", "--format=%(refname)"], out);
    bn(&["for-each-ref", "--no-sort", "--format=%(refname)"], out);
    bn(&["for-each-ref", "--omit-empty", "--format=%(symref)"], out);
    bn(&["for-each-ref", "--format=%(refname:lstrip=-1)"], out);
    bn(&["for-each-ref", "--format=%(refname:rstrip=-1)"], out);
    bn(&["for-each-ref", "--format=%(if:equals=refs/heads/main)%(refname)%(then)HIT%(else)miss%(end)"], out);
    bn(&["for-each-ref", "--format=%(if:notequals=refs/heads/main)%(refname)%(then)other%(else)MAIN%(end)"], out);
    bn(&["for-each-ref", "--format=%(is-base:HEAD)"], out);
    bn(&["for-each-ref", "--format=%(objectname:short=4)"], out);
    bn(&["for-each-ref", "--include-root-refs", "--format=%(refname)|%(symref)|%(objecttype)"], out);
    bn(&["for-each-ref", "--count=0", "--format=%(refname)"], out);
    out.push(Case::new(
        "for-each-ref",
        &["for-each-ref", "--format=%(refname)|%(objecttype)|%(*objecttype)|%(*objectname)", "refs/tags/"],
        Shape::TagChain,
    ));
    out.push(Case::new(
        "for-each-ref",
        &["for-each-ref", "--format=%(refname)|%(worktreepath)", "--include-root-refs"],
        Shape::Worktree,
    ));
}

/// `--ref-format` on the two commands that *create* a repository.
///
/// This is the one place `init` and `clone` are named in this file, and the
/// reason is the same as for `refs migrate`: the flag chooses a ref backend, and
/// nothing in `init_family.rs` or `fetch_clone.rs` passes it. The verbs stay
/// theirs; the flag is a storage question.
///
/// What these can and cannot show is worth stating, because it bounds the whole
/// reftable story in this corpus. `probe_state` reads the *fixture's* repository,
/// not a repository a case created inside it, so a reftable repository produced
/// here is never read back by the probes — the measurement is the exit code and
/// the message, which is exactly enough to separate "creates one" from "refuses
/// to". Stock creates it; the port exits non-zero saying it has no reftable
/// backend. Making the port *read* one needs a reftable `Shape`.
fn created_format(out: &mut Vec<Case>) {
    out.push(Case::new("init", &["init", "--ref-format=reftable", "rtsub"], Shape::Linear));
    out.push(Case::new("init", &["init", "--ref-format=files", "filesub"], Shape::Linear));
    out.push(Case::strict("init", &["init", "--ref-format=bogus", "bogussub"], Shape::Linear));
    out.push(Case::new("clone", &["clone", "--ref-format=reftable", ".", "rtclone"], Shape::Branched));
    out.push(Case::new("clone", &["clone", "--ref-format=files", ".", "filesclone"], Shape::Branched));
    out.push(Case::strict("clone", &["clone", "--ref-format=bogus", ".", "bogusclone"], Shape::Branched));
}
