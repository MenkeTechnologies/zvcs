//! The **hash function** a repository is made of — SHA-1 versus SHA-256 — and
//! every place an implementation has to know which one it is looking at.
//!
//! # The support question, answered first
//!
//! **The port supports SHA-256 completely, and byte-identically.** That was
//! established before a case was written, because the whole shape of this file
//! depends on it. Measured by hand against stock 2.55.0 and
//! `target/debug/git`, in two repositories built the same way:
//!
//! ```text
//! $ git init --object-format=sha256 r && cd r
//! $ git rev-parse --show-object-format
//! sha256                                                    # both sides
//! $ printf 'hi\n' | git hash-object -w --stdin
//! 96c18f0297e38d01f4b2dacddea4259aea6b2961eb0822bd2c0c3f6029030045   # both
//! $ git add f.txt && git commit -m x && git rev-parse HEAD
//! a1299389c9311ede9197acb029ce2f818d4fef355a08e9248cd706251857ef7d   # both
//! $ git ls-tree HEAD && git fsck                            # identical, clean
//! ```
//!
//! So this is not a "does it refuse cleanly" file. Both implementations write
//! and read SHA-256 objects, and the interesting surface is the *boundary*:
//! what each does when a hash of the wrong width, a declaration of the wrong
//! format, or a stream from the other format arrives. That boundary is where
//! the port is wrong, and it is wrong in four distinct ways, all recorded
//! below. Several of them are the same shape and it is the dangerous one: the
//! port **accepts** an operation git refuses, and what it leaves on disk is a
//! repository whose declared object format and actual object store disagree.
//!
//! # How this divides territory with the eight modules that already say
//! "object-format"
//!
//! Every one of them was read before this file was written. None of them asks
//! the questions here; all eight are *callers* that happen to pass the flag.
//!
//! * **`init_family.rs`** owns repository **creation**: `init` and `init-db`
//!   with `--object-format=sha1|sha256|bogus`, bare and not, in a subdirectory
//!   and in place, crossed with `--ref-format`, `-b`, `--shared` and
//!   `--template`. Its header already records that a `sha256`-init'd repository
//!   is fully probeable. It never reads `GIT_DEFAULT_HASH`, never reads
//!   `init.defaultObjectFormat`, and never asks a *created* repository what
//!   format it thinks it is. [`default_hash_env`] and [`show_object_format`]
//!   are those.
//! * **`object_pack.rs`** owns the pack toolchain on `Shape::Packed`'s SHA-1
//!   packs, including three `--object-format` mismatches (`index-pack
//!   --object-format=sha256` over a SHA-1 pack, `verify-pack
//!   --object-format=sha256|bogus` over a SHA-1 index, `show-index
//!   --object-format=sha256`) and the SHA-1 empty tree as a literal
//!   (`EMPTY_TREE_OID`). Every byte it feeds a pack verb is SHA-1. This file
//!   feeds a **SHA-256 pack** to a SHA-1 repository ([`cross_format_streams`]),
//!   which is the direction it structurally cannot reach.
//! * **`plumbing_objects.rs`** owns `verify-pack --object-format=sha1` and
//!   `show-index --object-format=sha1|bogus` pointed at a non-pack file. Flag
//!   parsing, not hash identity.
//! * **`integrity_gc.rs`** owns the complementary `verify-pack
//!   --object-format=sha1` that *matches* the repository, and says so in its
//!   own header. Nothing about a mismatch.
//! * **`interchange.rs`** owns the bundle containers, and every literal it
//!   carries — `BUNDLE_V3`, `BUNDLE_V3_FILTER`, the malformed ones — is
//!   `@object-format=sha1`. [`BUNDLE_SHA256`] here is the same container with
//!   the other algorithm and a real SHA-256 pack behind it.
//! * **`wire_protocol.rs`** owns the v2 request payloads, all of which pin
//!   `object-format=sha1` in the capability list. It never varies that field.
//! * **`fetch_clone.rs`** owns `fetch`/`clone`/`bundle`/`ls-remote` against a
//!   SHA-1 peer from a SHA-1 repository; `object-format` appears in it only as
//!   a comment about the v3 bundle header. [`cross_format_transport`] runs the
//!   same verbs from a repository that *declares* the other algorithm.
//! * **`misc_commands.rs`** owns one case, `init-db --object-format=bogus`.
//!
//! And the structural sibling: **`ref_storage.rs`** asks what a repository
//! declaring `extensions.refStorage = reftable` over a files backend does, and
//! found the declaration ignored rather than refused. [`declared_sha256`] is
//! the same experiment on the other extension, and the answer is different and
//! worse — see defect 1.
//!
//! # What a single invocation can and cannot reach here
//!
//! A case is one command against a pristine fixture copy, and no fixture in
//! `fixture.rs` is a SHA-256 repository. Two consequences, both load-bearing:
//!
//! * **Reachable, and used below:** `init` (which *creates* the format, so the
//!   whole `GIT_DEFAULT_HASH` / `init.defaultObjectFormat` cross is one
//!   invocation); `extensions.objectFormat` written into `.git/config` by
//!   [`ConfigScope::Repo`] before the command runs, which is a *declaration* of
//!   SHA-256 over a SHA-1 object store and needs no second step; an oid of the
//!   wrong width in argv or on stdin; and a SHA-256 pack or bundle delivered on
//!   stdin as a literal.
//! * **Not reachable, and deliberately absent:** anything needing a genuine
//!   SHA-256 repository *plus* a second command — `fetch` from a real SHA-256
//!   peer, `push` into one, `submodule add` of one, `clone` of one. Those are
//!   `sequences.rs`'s territory (`init --object-format=sha256` then the verb),
//!   and no case here pretends to measure them. What is measured instead is the
//!   half a single invocation *can* see: a repository that declares SHA-256
//!   talking to a SHA-1 peer, which is [`cross_format_transport`].
//!
//! # The four defects this file pins
//!
//! Defects 1 and 4 are **acceptances of an operation git refuses**, and each
//! leaves behind a repository whose object format declaration and object store
//! disagree — verified by reopening the wreckage with both binaries and getting
//! a refusal from each. Defect 3 contains a third acceptance of the same shape
//! (`gc`). Those are worse than any refusal, and they are stated first.
//!
//! **1. The port writes a SHA-1 ref value into a SHA-256 repository, and exits
//! 0.** Reproduced in a *genuine* `init --object-format=sha256` repository, not
//! only in a declared one:
//!
//! ```text
//! $ git init --object-format=sha256 r && cd r && ...commit...
//! $ git update-ref refs/heads/n 4b825dc642cb6eb9a060e54bf8d69288fbee4904
//! stock: fatal: 4b825dc642cb6eb9a060e54bf8d69288fbee4904: not a valid SHA1   (rc 128)
//! port : (silent)                                                            (rc 0)
//! $ cat .git/refs/heads/n
//! port : 4b825dc642cb6eb9a060e54bf8d69288fbee4904
//! ```
//!
//! A 40-hex value now sits in a 64-hex repository, with a reflog entry to
//! match, and **neither implementation can read it afterwards.** This is the
//! one failure mode the corpus should care about most: a refusal that becomes
//! an acceptance produces a repository that is not a repository. The trigger is
//! narrow — `4b825dc6…` is the SHA-1 empty tree, which the port's object
//! database recognises as a constant and admits without consulting the store —
//! and the mirror image holds: in a SHA-1 repository the port answers
//! `rev-parse --verify 6ef19b41…` (the SHA-256 empty tree) with exit 0 and the
//! oid, where stock says `fatal: Needed a single revision`. That is why
//! [`empty_tree_constants`] exists as a group of its own: the two constants are
//! the exact input that walks past the width check.
//!
//! **2. An oid of the wrong width panics the port.** Not an error — a Rust
//! assertion, exit 101:
//!
//! ```text
//! $ git update-ref refs/heads/n a1299389c9…57ef7d      # 64 hex, SHA-1 repo
//! stock: fatal: a1299389c9…57ef7d: not a valid SHA1                          (rc 128)
//! port : thread 'main' panicked at src/ported/gix-odb/src/store_impls/loose/find.rs:34:9:
//!        assertion `left == right` failed
//!          left: Sha1
//!         right: Sha256                                                      (rc 101)
//! ```
//!
//! The same panic fires from `update-ref --stdin`, from `cat-file -t/-p/-s`,
//! from `ls-tree`, from `rev-list`, from `log`, from `diff` and from `cat-file
//! --batch-check` reading the oid off stdin, and it fires **in both
//! directions** (`left: Sha256 / right: Sha1` when a 40-hex oid reaches a
//! declared-SHA-256 repository). [`wrong_width_oid`] is that surface. Note the
//! contrast the group is built to show: `mktree`, `mktag`, `commit-tree`,
//! `read-tree`, `branch`, `tag`, `notes`, `replace`, `reset` and `cherry-pick`
//! all reject the same oid identically on both sides, so the defect is in one
//! lookup path and not in width validation generally.
//!
//! **3. A repository declaring a format its objects are not is not refused —
//! it is half-read, differently.** Stock treats the SHA-1 object store as
//! corruption of a SHA-256 repository and dies at 128 with a message naming
//! what it choked on (`fatal: unknown index entry format 0xb7fd0000`, `fatal:
//! your current branch appears to be broken`, `error: bad index file sha1
//! signature`). The port reports a gitoxide parse failure and exits 1. The
//! exit codes disagree on `status`, `log`, `ls-files`, `diff`, `fsck`,
//! `branch`, `symbolic-ref`, `for-each-ref` (stock exits **0** there, with
//! `warning: ignoring broken ref`), `write-tree`, `repack`, `prune` and `gc` —
//! and `gc` is a third acceptance-where-git-refuses: stock exits 128 with
//! `fatal: failed to run repack`, the port exits **0** and writes
//! `.git/info/refs` and `.git/objects/info/packs` into the broken repository.
//!
//! **4. `clone` does not filter the format extensions out of `-c`, so one
//! setting produces a repository neither implementation can read.** The
//! shortest reproduction in this file, and the one that needs no unusual input
//! at all:
//!
//! ```text
//! $ git clone -c extensions.objectFormat=sha256 . copy
//!   Cloning into 'copy'... done.                          both sides, rc 0
//! $ grep -A1 '\[extensions\]' copy/.git/config
//!   stock: (no [extensions] section — git filtered the setting out)
//!   port : objectFormat = sha256
//! $ cd copy && git log --oneline
//!   stock: fatal: repo version is 0, but v1-only extension found: objectformat
//!   port : fatal: repo version is 0, but v1-only extension found: objectformat
//! ```
//!
//! Git persists `-c` settings into a new clone's config — both sides do, and
//! `-c diff.algorithm=patience` proves it — but it removes
//! `extensions.objectFormat` and `extensions.refStorage` on the way, because a
//! clone decides its own format and a caller must not be able to assert one
//! after the fact. The port removes neither. `extensions.worktreeConfig` and an
//! invented `extensions.noSuchThing` are persisted by both sides, so the rule
//! being missed is specific and so is the miss.
//!
//! The louder form of the same territory is `-c core.repositoryFormatVersion=1`
//! alongside it: there stock aborts the clone entirely (`fatal: could not set
//! 'core.repositoryformatversion' to '0'`, rc 128, no `copy` directory left)
//! and the port exits 0 with a v1 repository declaring SHA-256 over 40-hex
//! objects and a 40-hex `refs/heads/main`. Both were read back with both
//! binaries and both are unopenable. See [`clone_into_a_lying_repository`],
//! which carries the four filter rows, the two negative controls, and the
//! control showing the second form's trigger is the duplicated version key
//! rather than the extension.
//!
//! # Which cases compare stderr, and why not all of them
//!
//! [`Case::strict`] is used where the refusal message is the entire answer and
//! nothing else varies: the `--object-format` unknown-option blocks (the usage
//! text *is* the statement that git has no such option on that verb), `fatal:
//! unknown mode for --show-object-format: <x>`, `fatal: unknown hash algorithm
//! '<x>'`, `fatal: repo version is 0, but v1-only extension found`, and the
//! agreeing half of [`wrong_width_oid`], where the message names the oid.
//!
//! The reader groups on a declared-SHA-256 repository use [`Case::new`]: there
//! the question is the exit code and what was left on disk, both of which
//! already diverge, and adding stderr would only restate a difference in prose
//! that the harness's standing policy does not treat as a compatibility
//! surface.
//!
//! Every case carrying stdin is [`Case::new`] whether or not its message is the
//! answer, and not by choice: `Case::with_stdin` is the only constructor that
//! takes a payload and it does not set `compare_stderr`, and this file may not
//! touch `runner.rs` to add one. So the SHA-256 pack and bundle refusals in
//! [`cross_format_streams`] are measured on stdout, exit code and post-state
//! only. Both sides agreed on all three of those, and their stderr was checked
//! by hand and agreed too — but the harness is not checking it, and that is
//! stated here rather than left to be assumed.
//!
//! # What is not measurable here, and is therefore not claimed
//!
//! * **A real cross-format `fetch`/`push`/`clone`/`submodule add`.** Needs two
//!   repositories in two formats; a case builds one fixture. See above. What
//!   *is* measured is the adjacent question — a repository that declares one
//!   format over the other's objects, whether it was declared by configuration
//!   ([`declared_sha256`]) or produced by `clone`
//!   ([`clone_into_a_lying_repository`]).
//! * **Whether the port's SHA-256 *pack* files are byte-identical to stock's.**
//!   No fixture carries a SHA-256 pack, and a pack built during a case embeds
//!   its own checksum in its filename, which the state probe masks.
//! * **`bundle create --version=3` recording `@object-format=sha256`.** It
//!   would need a SHA-256 fixture to create from. [`BUNDLE_SHA256`] measures
//!   the *reading* half only.
//! * **`bundle verify -`'s stderr.** Stock names the stream `<stdin>` and the
//!   port names it `-`. That is a bundle-naming difference, not a hash one, and
//!   it belongs to `interchange.rs`; the case here is not strict so it does not
//!   silently absorb someone else's finding.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

// ---------------------------------------------------------------------------
// The four constants that differ per hash
// ---------------------------------------------------------------------------

/// The empty tree under SHA-1. Read back from stock in a SHA-1 fixture with
/// `git hash-object -t tree /dev/null`.
const EMPTY_TREE_SHA1: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// The empty tree under SHA-256, read back the same way inside an
/// `init --object-format=sha256` repository.
const EMPTY_TREE_SHA256: &str =
    "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321";
/// The empty blob under SHA-1 (`printf '' | git hash-object --stdin`).
const EMPTY_BLOB_SHA1: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
/// The empty blob under SHA-256.
const EMPTY_BLOB_SHA256: &str =
    "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813";

/// A 64-hex oid that is a real SHA-256 commit id — of the single commit in an
/// `init --object-format=sha256` repository built under this harness's pinned
/// identity and clock — and is therefore *not* one of the special constants
/// above. Used to separate "the port mishandles any wrong-width oid" from "the
/// port mishandles the empty-tree constant", which are different defects.
const FOREIGN_COMMIT_SHA256: &str =
    "a1299389c9311ede9197acb029ce2f818d4fef355a08e9248cd706251857ef7d";

/// 64 hex zeroes: the SHA-256 spelling of the null oid, which `update-ref` uses
/// as "this ref must not exist yet". A SHA-1 repository has never seen 64 of
/// them.
const NULL_SHA256: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// 40 hex `f`s: the SHA-1-width control, absent from every store and
/// special-cased by nobody.
const ABSENT_SHA1: &str = "ffffffffffffffffffffffffffffffffffffffff";

/// 64 hex `f`s — well-formed for SHA-256, certainly absent from every store,
/// and not a constant any implementation special-cases. The control for
/// [`empty_tree_constants`].
const ABSENT_SHA256: &str =
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

/// The two config entries that declare SHA-256 over a repository whose objects
/// are SHA-1.
///
/// Delivered from [`ConfigScope::Repo`], which appends to `.git/config`. Both
/// keys are required and the order matters for readability, not for parsing:
/// the fixture's own `[core]` stanza already carries
/// `repositoryformatversion = 0`, so the appended stanza is a second setting of
/// the same key and the last one wins. Without the version bump git refuses
/// with `fatal: repo version is 0, but v1-only extension found: objectformat`
/// on *both* sides, which is a different (and also interesting) measurement —
/// it is [`extension_gating`].
fn declare_sha256() -> Vec<ConfigEntry> {
    vec![
        ConfigEntry::set(ConfigScope::Repo, "core.repositoryFormatVersion", "1"),
        ConfigEntry::set(ConfigScope::Repo, "extensions.objectFormat", "sha256"),
    ]
}

pub fn cases(out: &mut Vec<Case>) {
    show_object_format(out);
    default_hash_env(out);
    format_flag_where_git_has_none(out);
    declared_sha256(out);
    wrong_width_oid(out);
    empty_tree_constants(out);
    cross_format_streams(out);
    cross_format_transport(out);
    extension_gating(out);
    clone_into_a_lying_repository(out);
}

/// `rev-parse --show-object-format` — the one verb whose entire job is to name
/// the hash, and which **no case in the corpus ran** before this group.
///
/// It has three modes (`storage`, `input`, `output`) that git parses
/// separately and that all answer the same thing today, which is exactly the
/// kind of surface a port collapses to one branch and gets away with. The bogus
/// and empty spellings are here because `fatal: unknown mode for
/// --show-object-format: <x>` is the only place the parser says what it read,
/// and the empty one is not the same token as the bogus one.
///
/// Run across four shapes because the answer must be a property of the
/// repository and not of the command: `Packed` has packfiles, `BehindRemote`
/// has a second repository inside it, and `Merged` has a multi-parent history —
/// none of which may change the answer.
///
/// Measured on stock 2.55.0: all four modes print `sha1` and exit 0, and every
/// invalid spelling — `bogus`, the empty string, `SHA1` in the wrong case and
/// `sha-256` with a hyphen — exits 128 with the message above. The port agreed
/// on each one, including from `.git` and from a subdirectory and alongside
/// `--show-ref-format` in either order.
fn show_object_format(out: &mut Vec<Case>) {
    for mode in ["", "=storage", "=input", "=output"] {
        let flag = format!("--show-object-format{mode}");
        for shape in [Shape::Linear, Shape::Branched, Shape::Packed, Shape::Merged] {
            out.push(Case::new("rev-parse", &["rev-parse", &flag], shape));
        }
    }

    // The refusal names the mode it could not parse, so the message is the
    // answer and these are strict.
    out.push(Case::strict("rev-parse", &["rev-parse", "--show-object-format=bogus"], Shape::Linear));
    out.push(Case::strict("rev-parse", &["rev-parse", "--show-object-format="], Shape::Linear));
    out.push(Case::strict("rev-parse", &["rev-parse", "--show-object-format=SHA1"], Shape::Linear));
    out.push(Case::strict("rev-parse", &["rev-parse", "--show-object-format=sha-256"], Shape::Linear));

    // Combined with the neighbouring `--show-*` queries, because git's
    // rev-parse emits them in argv order and a port that special-cases the flag
    // rather than threading it through the option loop gets the order wrong.
    out.push(Case::new(
        "rev-parse",
        &["rev-parse", "--show-object-format", "--show-ref-format"],
        Shape::Linear,
    ));
    out.push(Case::new(
        "rev-parse",
        &["rev-parse", "--show-ref-format", "--show-object-format"],
        Shape::Linear,
    ));
    out.push(Case::new(
        "rev-parse",
        &["rev-parse", "--show-object-format", "--show-toplevel", "--git-dir"],
        Shape::Linear,
    ));
    // Asked from inside the git directory and from a subdirectory: the format
    // is a property of the repository, so discovery must not change it.
    out.push(Case::new("rev-parse", &["rev-parse", "--show-object-format"], Shape::Linear).in_dir(".git"));
    out.push(Case::new("rev-parse", &["rev-parse", "--show-object-format"], Shape::Linear).in_dir("src"));
}

/// The two ways to choose a format for a repository that does not exist yet,
/// neither of which any case reached: the **environment variable**
/// `GIT_DEFAULT_HASH` and the **configuration key** `init.defaultObjectFormat`.
///
/// `env_layer.rs` names `GIT_DEFAULT_HASH` in a comment and never sets it;
/// `init_family.rs` passes `--object-format` on argv and never uses either of
/// these sources. The point of the group is the **asymmetry between the two**,
/// which is a real and easily-missed piece of git's behaviour:
///
/// ```text
/// $ GIT_DEFAULT_HASH=bogus git init sub
/// fatal: unknown hash algorithm 'bogus'                             (rc 128)
/// $ git -c init.defaultObjectFormat=bogus init sub
/// Initialized empty Git repository in .../sub/.git/                 (rc 0)
/// $ grep -i objectformat sub/.git/config      # nothing: silently ignored
/// ```
///
/// Both sides agreed on every cell of that, including the silent ignore. The
/// one difference the group did surface is a **state** one: with
/// `GIT_DEFAULT_HASH=bogus`, stock creates the `sub` directory before dying and
/// the port does not.
///
/// `GIT_DEFAULT_HASH` is safe to set from a case — [`crate::env::harden`] does
/// not pin it, and `harden` starts from `env_clear`, so setting it is purely
/// additive and lands identically on both sides.
fn default_hash_env(out: &mut Vec<Case>) {
    for value in ["sha256", "sha1", "SHA256", "bogus", ""] {
        out.push(
            Case::new("init", &["init", "sub"], Shape::Linear)
                .with_env(&[("GIT_DEFAULT_HASH", value)]),
        );
        out.push(
            Case::new("init", &["init", "-q", "--bare", "sub.git"], Shape::Linear)
                .with_env(&[("GIT_DEFAULT_HASH", value)]),
        );
    }

    // The refusal names the algorithm it could not resolve; that message is the
    // whole contract of the variable.
    out.push(
        Case::strict("init", &["init", "sub"], Shape::Linear)
            .with_env(&[("GIT_DEFAULT_HASH", "bogus")]),
    );
    out.push(
        Case::strict("init-db", &["init-db", "sub"], Shape::Linear)
            .with_env(&[("GIT_DEFAULT_HASH", "sha-256")]),
    );

    // argv beats the environment. Without this pair, a port that reads only one
    // of the two sources scores the same as one that reads both in the right
    // order.
    out.push(
        Case::new("init", &["init", "--object-format=sha1", "sub"], Shape::Linear)
            .with_env(&[("GIT_DEFAULT_HASH", "sha256")]),
    );
    out.push(
        Case::new("init", &["init", "--object-format=sha256", "sub"], Shape::Linear)
            .with_env(&[("GIT_DEFAULT_HASH", "sha1")]),
    );

    // The configuration key, from the command line and from the repository
    // file. `bogus` is kept because being *ignored* is the finding.
    for value in ["sha256", "sha1", "bogus"] {
        out.push(
            Case::new("init", &["init", "sub"], Shape::Linear)
                .with_config(&[("init.defaultObjectFormat", value)]),
        );
        out.push(
            Case::new("init", &["init", "sub"], Shape::Linear).with_scoped_config(vec![
                ConfigEntry::set(ConfigScope::Repo, "init.defaultObjectFormat", value),
            ]),
        );
    }

    // The two sources against each other, both directions.
    out.push(
        Case::new("init", &["init", "sub"], Shape::Linear)
            .with_config(&[("init.defaultObjectFormat", "sha1")])
            .with_env(&[("GIT_DEFAULT_HASH", "sha256")]),
    );
    out.push(
        Case::new("init", &["init", "sub"], Shape::Linear)
            .with_config(&[("init.defaultObjectFormat", "sha256")])
            .with_env(&[("GIT_DEFAULT_HASH", "sha1")]),
    );
    // And the config key losing to argv.
    out.push(
        Case::new("init", &["init", "--object-format=sha1", "sub"], Shape::Linear)
            .with_config(&[("init.defaultObjectFormat", "sha256")]),
    );
}

/// `--object-format` handed to the verbs that **do not have it**.
///
/// Git 2.55 accepts `--object-format` on `init`, `init-db`, `index-pack`,
/// `verify-pack` and `show-index`, and on no other verb — checked by handing
/// `--object-format=sha1` to nineteen of them and reading which answered
/// `unknown option`. (`rev-parse` is the one that does not, and it is not an
/// acceptance: `rev-parse` passes anything it does not recognise through as a
/// revision.) It is not an option of `clone`, of `hash-object` or of
/// `cat-file`, and the refusal is the full `usage:` block — twenty-odd lines of
/// option list that a port either reproduces or does not.
///
/// This is here because it is the cheapest possible way for a port to be wrong
/// in the most damaging direction: *inventing* `--object-format` on
/// `hash-object` would let a caller write a SHA-256 blob into a SHA-1
/// repository, and inventing it on `clone` would let a caller ask for a
/// conversion git has never implemented. Measured on stock: `error: unknown
/// option \`object-format=<x>'` then the usage block, exit 129. The port
/// reproduced all three usage blocks byte for byte, so every case here is
/// [`Case::strict`] — with the wrong option name in the message, the case is
/// worth nothing.
///
/// `sha1` is included alongside `sha256` and `bogus` deliberately: a port that
/// validated the *value* before rejecting the option would pass the `bogus`
/// case and fail the other two.
fn format_flag_where_git_has_none(out: &mut Vec<Case>) {
    for value in ["sha256", "sha1", "bogus"] {
        let flag = format!("--object-format={value}");
        out.push(Case::strict("clone", &["clone", &flag, ".", "copy"], Shape::Linear));
        out.push(Case::strict(
            "hash-object",
            &["hash-object", &flag, "--stdin"],
            Shape::Linear,
        ));
        out.push(Case::strict("cat-file", &["cat-file", &flag, "-t", "HEAD"], Shape::Linear));
    }
    // The flag after the other options rather than before, in case the port
    // stops parsing at the first unknown token instead of at this one.
    out.push(Case::strict(
        "hash-object",
        &["hash-object", "-w", "-t", "blob", "--object-format=sha256", "--stdin"],
        Shape::Linear,
    ));
    out.push(Case::strict(
        "clone",
        &["clone", "--no-local", "--object-format=sha256", ".", "copy"],
        Shape::Linear,
    ));
    // What a caller reaches for once `--object-format` is refused is
    // `clone -c extensions.objectFormat=sha256`, and that is
    // [`clone_into_a_lying_repository`] rather than this group, because it
    // succeeds rather than being refused.
}

/// A repository that **declares SHA-256 and contains SHA-1 objects**, read by
/// every verb that has to open something.
///
/// This is the structural mirror of `ref_storage.rs`'s
/// `extensions.refStorage = reftable` experiment, and the reason it is worth
/// running twice on two different extensions is that the two answers are not
/// the same. `refStorage` is *ignored*: the declaration changes nothing and the
/// files backend keeps working. `objectFormat` is not ignored — it changes the
/// width of every oid the reader expects, so the whole object store and the
/// index become unreadable at once, and the two implementations disagree about
/// what to do next.
///
/// Stock's answers, all measured by hand in a copy of `Shape::Linear` with the
/// two keys appended to `.git/config`:
///
/// ```text
/// rev-parse --show-object-format  sha256                                    (rc 0)
/// status --porcelain              fatal: unknown index entry format 0xb7fd0000    (128)
/// log --oneline                   fatal: your current branch appears to be broken (128)
/// for-each-ref                    warning: ignoring broken ref refs/heads/main    (0)
/// branch --list                   fatal: failed to resolve HEAD as a valid ref    (128)
/// symbolic-ref HEAD               fatal: No such ref: HEAD                        (128)
/// gc --quiet                      fatal: failed to run repack                     (128)
/// count-objects -v                garbage: 5                                      (0)
/// cat-file -t HEAD                fatal: Not a valid object name HEAD             (128)
/// ```
///
/// The port answers exit 1 with a gitoxide message wherever stock answers 128,
/// and — the finding that matters — exit **0** where stock answers 128 for
/// `gc`, having written `.git/info/refs` and `.git/objects/info/packs` into the
/// repository. The `for-each-ref` cell is the reverse: stock 0, port 1.
///
/// The group is deliberately wide rather than deep. A port could plausibly get
/// any one verb right by accident; what it cannot do by accident is agree with
/// stock on twenty verbs that reach the object store through four different
/// front doors (the index, the ref store, the odb, the config).
fn declared_sha256(out: &mut Vec<Case>) {
    // Readers that touch the index.
    for args in [
        &["status", "--porcelain"][..],
        &["status", "--short", "--branch"][..],
        &["ls-files"][..],
        &["ls-files", "--stage"][..],
        &["diff", "--stat"][..],
        &["diff", "--cached"][..],
        &["update-index", "--refresh"][..],
        &["write-tree"][..],
        &["add", "--dry-run", "README.md"][..],
    ] {
        out.push(Case::new(args[0], args, Shape::Linear).with_scoped_config(declare_sha256()));
    }

    // Readers that touch the ref store.
    for args in [
        &["log", "--oneline"][..],
        &["log", "-1", "--format=%H"][..],
        &["for-each-ref"][..],
        &["for-each-ref", "--format=%(refname) %(objectname)"][..],
        &["show-ref"][..],
        &["show-ref", "--head"][..],
        &["branch", "--list"][..],
        &["branch", "-a", "-v"][..],
        &["symbolic-ref", "HEAD"][..],
        &["rev-list", "--all"][..],
        &["describe", "--always"][..],
        &["reflog"][..],
    ] {
        out.push(Case::new(args[0], args, Shape::Linear).with_scoped_config(declare_sha256()));
    }

    // Readers that go straight to the object database, and the two that report
    // on it without opening an object.
    for args in [
        &["cat-file", "-t", "HEAD"][..],
        &["cat-file", "-p", "HEAD"][..],
        &["ls-tree", "HEAD"][..],
        &["rev-parse", "HEAD"][..],
        &["rev-parse", "--show-object-format"][..],
        &["cat-file", "--batch-all-objects", "--batch-check"][..],
        &["count-objects", "-v"][..],
        &["config", "--get", "extensions.objectFormat"][..],
        &["config", "--list", "--local"][..],
        &["hash-object", "--stdin"][..],
        &["fsck"][..],
        &["fsck", "--connectivity-only"][..],
        &["worktree", "list"][..],
        &["commit-tree", "HEAD^{tree}", "-m", "x"][..],
        &["bundle", "create", "-", "--all"][..],
    ] {
        out.push(Case::new(args[0], args, Shape::Linear).with_scoped_config(declare_sha256()));
    }

    // Maintenance verbs, which is where the port stops refusing and starts
    // writing. `gc` is defect 3's second half: stock 128 and nothing written,
    // port 0 with `.git/info/refs` and `.git/objects/info/packs` created.
    for args in [
        &["gc", "--quiet"][..],
        &["gc", "--aggressive", "--quiet"][..],
        &["repack", "-a"][..],
        &["repack", "-adq"][..],
        &["prune", "-n"][..],
        &["prune", "--dry-run", "--verbose"][..],
        &["commit-graph", "write"][..],
        &["multi-pack-index", "write"][..],
        &["pack-refs", "--all"][..],
    ] {
        out.push(Case::new(args[0], args, Shape::Linear).with_scoped_config(declare_sha256()));
    }

    // The same declaration on a shape with more refs to break, so the answer is
    // not an artefact of `Linear`'s single branch.
    for args in [&["for-each-ref"][..], &["log", "--oneline"][..], &["tag", "--list"][..], &["status"][..]] {
        out.push(Case::new(args[0], args, Shape::Branched).with_scoped_config(declare_sha256()));
    }
}

/// An oid of the **wrong width** for the repository it is handed to.
///
/// A SHA-1 repository is given a 64-hex oid, and a repository declaring
/// SHA-256 is given a 40-hex one. Both are well-formed hex; both are simply the
/// other algorithm's shape. This is the single cheapest input that separates a
/// port that carries the repository's hash function through every lookup from
/// one that infers the algorithm from the string it was handed.
///
/// **The port panics.** Exit 101, with a Rust assertion from
/// `src/ported/gix-odb/src/store_impls/loose/find.rs:34` reading
/// `assertion \`left == right\` failed / left: Sha1 / right: Sha256`, against
/// stock's `fatal: <oid>: not a valid SHA1` at 128. Verified by hand on
/// `update-ref` in three spellings (positional, `--no-deref`, with an expected
/// old value), on `update-ref --stdin`, and — with the constants of
/// [`empty_tree_constants`] — on `cat-file`, `ls-tree`, `rev-list`, `log` and
/// `diff`.
///
/// The group is built around a contrast, and the contrast is the point. These
/// verbs **agree** on the identical oid, both sides refusing at 128 with the
/// same sentence:
///
/// ```text
/// cat-file -t <64hex>     fatal: Not a valid object name <64hex>
/// read-tree <64hex>       fatal: Not a valid object name <64hex>
/// commit-tree -m x <64hex> fatal: not a valid object name <64hex>
/// mktree      (stdin)     fatal: input format error: 100644 blob <64hex>\tf.txt
/// mktag       (stdin)     error: tag input does not pass fsck: badObjectSha1: …
/// branch b <64hex>        fatal: not a valid object name: '<64hex>'
/// tag t <64hex>           fatal: Failed to resolve '<64hex>' as a valid ref.
/// cherry-pick <64hex>     fatal: bad revision '<64hex>'
/// ```
///
/// So width validation is not broadly missing; one lookup path is missing it,
/// and these cases are what says so. Without the agreeing half, a future reader
/// would have no way to tell a narrow defect from a wholesale one.
///
/// The oid used is [`FOREIGN_COMMIT_SHA256`] — a real SHA-256 commit id, not a
/// constant either implementation special-cases — precisely so this group and
/// [`empty_tree_constants`] measure different things.
fn wrong_width_oid(out: &mut Vec<Case>) {
    let foreign = FOREIGN_COMMIT_SHA256;

    // The panicking path: writing a ref.
    out.push(Case::new("update-ref", &["update-ref", "refs/heads/n", foreign], Shape::Linear));
    out.push(Case::new(
        "update-ref",
        &["update-ref", "--no-deref", "refs/heads/n", foreign],
        Shape::Linear,
    ));
    out.push(Case::new(
        "update-ref",
        &["update-ref", "refs/heads/main", foreign, NULL_SHA256],
        Shape::Linear,
    ));
    out.push(Case::new("update-ref", &["update-ref", "refs/tags/v", foreign], Shape::Linear));
    out.push(Case::new("update-ref", &["update-ref", "HEAD", foreign], Shape::Linear));
    out.push(Case::with_stdin(
        "update-ref",
        &["update-ref", "--stdin"],
        Shape::Linear,
        b"update refs/heads/n a1299389c9311ede9197acb029ce2f818d4fef355a08e9248cd706251857ef7d\n",
    ));
    out.push(Case::with_stdin(
        "update-ref",
        &["update-ref", "--stdin", "-z"],
        Shape::Linear,
        b"update refs/heads/n\0a1299389c9311ede9197acb029ce2f818d4fef355a08e9248cd706251857ef7d\0\0",
    ));
    out.push(Case::with_stdin(
        "update-ref",
        &["update-ref", "--stdin"],
        Shape::Linear,
        b"create refs/heads/n a1299389c9311ede9197acb029ce2f818d4fef355a08e9248cd706251857ef7d\n",
    ));
    // Deletion with a wrong-width *expected* value: no panic, but stock says
    // `not a valid old SHA1` at 128 and the port says `cannot lock ref` at 1.
    out.push(Case::new(
        "update-ref",
        &["update-ref", "-d", "refs/heads/main", foreign],
        Shape::Linear,
    ));

    // The agreeing half, which is what makes the panicking half legible. These
    // are strict: the refusal names the oid, and that is the only place either
    // implementation says what it thought the string was.
    for args in [
        &["cat-file", "-t", FOREIGN_COMMIT_SHA256][..],
        &["cat-file", "-e", FOREIGN_COMMIT_SHA256][..],
        &["read-tree", FOREIGN_COMMIT_SHA256][..],
        &["commit-tree", "-m", "x", FOREIGN_COMMIT_SHA256][..],
        &["branch", "b", FOREIGN_COMMIT_SHA256][..],
        &["tag", "t", FOREIGN_COMMIT_SHA256][..],
        &["notes", "add", "-f", "-m", "x", FOREIGN_COMMIT_SHA256][..],
        &["replace", "HEAD", FOREIGN_COMMIT_SHA256][..],
        &["cherry-pick", FOREIGN_COMMIT_SHA256][..],
        &["merge-base", FOREIGN_COMMIT_SHA256, "HEAD"][..],
        &["rev-parse", "--verify", FOREIGN_COMMIT_SHA256][..],
        &["reset", "--hard", FOREIGN_COMMIT_SHA256][..],
        &["checkout", FOREIGN_COMMIT_SHA256][..],
        &["diff", FOREIGN_COMMIT_SHA256][..],
        &["show", FOREIGN_COMMIT_SHA256][..],
    ] {
        out.push(Case::strict(args[0], args, Shape::Linear));
    }

    // `symbolic-ref` takes a *ref name*, not an oid, and 64 hex characters are
    // a legal ref name. Stock creates the symref and exits 0; the port refuses
    // at 1 with `Standalone references must be all uppercased`. That is a ref
    // naming disagreement reached through this file's input, not a hash one,
    // and it is here because the same argument reaches it.
    out.push(Case::new("symbolic-ref", &["symbolic-ref", "refs/heads/x", foreign], Shape::Linear));

    // stdin-driven object construction with the foreign oid embedded: mktree
    // and mktag parse the width themselves rather than asking the odb, and both
    // sides refuse identically.
    out.push(Case::with_stdin(
        "mktree",
        &["mktree"],
        Shape::Linear,
        b"100644 blob a1299389c9311ede9197acb029ce2f818d4fef355a08e9248cd706251857ef7d\tf.txt\n",
    ));
    out.push(Case::with_stdin(
        "mktree",
        &["mktree", "--missing"],
        Shape::Linear,
        b"100644 blob a1299389c9311ede9197acb029ce2f818d4fef355a08e9248cd706251857ef7d\tf.txt\n",
    ));
    out.push(Case::with_stdin(
        "mktag",
        &["mktag"],
        Shape::Linear,
        b"object a1299389c9311ede9197acb029ce2f818d4fef355a08e9248cd706251857ef7d\ntype commit\ntag v1\ntagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\nmsg\n",
    ));

    // The mirror: a 40-hex oid handed to a repository declaring SHA-256.
    // `update-ref` panics here too, with the assertion reading `left: Sha256 /
    // right: Sha1` — the same defect seen from the other side, which is what
    // shows it is in the width comparison and not in one algorithm.
    // `cat-file -t` refuses at 128 on both sides for these two, and that is the
    // contrast with [`empty_tree_constants`]: there the *constants* panic it,
    // here only the ref write does.
    for oid in ["1234567890123456789012345678901234567890", ABSENT_SHA1] {
        out.push(
            Case::new("update-ref", &["update-ref", "refs/heads/n", oid], Shape::Linear)
                .with_scoped_config(declare_sha256()),
        );
        out.push(
            Case::new("cat-file", &["cat-file", "-t", oid], Shape::Linear)
                .with_scoped_config(declare_sha256()),
        );
    }
}

/// The **empty tree and empty blob**, whose ids differ per hash, handed to the
/// repository of the other algorithm.
///
/// These four constants are the reason this group is separate from
/// [`wrong_width_oid`]. Every implementation short-circuits them — they are the
/// one pair of objects that exists without being written — and a short-circuit
/// that skips the store also skips the store's opinion about how wide an oid
/// is. That is exactly what happens:
///
/// ```text
/// # In a plain SHA-1 fixture, given the SHA-256 empty tree:
/// $ git rev-parse --verify 6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321
/// stock: fatal: Needed a single revision                                (rc 128)
/// port : 6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321 (rc 0)
/// $ git cat-file -e 6ef19b41…                    stock 128 / port 0
/// $ git cat-file -t 6ef19b41…                    stock 128 / port 101 (panic)
/// $ git update-ref refs/heads/n 6ef19b41…        stock 128 / port 0
///
/// # And the control, four f-filled 64-hex characters wide:
/// $ git rev-parse --verify ffff…ffff             stock 128 / port 128  — agree
/// $ git cat-file -t ffff…ffff                    stock 128 / port 128  — agree
/// ```
///
/// The control pair is what turns this from an anecdote into a measurement: a
/// well-formed 64-hex oid that is *not* a recognised constant is refused
/// identically by both. So the port is not accepting foreign-width oids in
/// general; it is accepting the two it has memorised, and defect 1 — the ref
/// write into a genuine SHA-256 repository — is reached through exactly this
/// door with the constants swapped.
///
/// The empty **blob** is included in both widths for the same reason and is
/// the negative result worth recording: `cat-file -e 473a0f4c…` (SHA-256 empty
/// blob, SHA-1 repository) agrees at 128 on both sides. Only the tree is
/// short-circuited.
fn empty_tree_constants(out: &mut Vec<Case>) {
    // SHA-256 constants in a SHA-1 repository.
    for oid in [EMPTY_TREE_SHA256, EMPTY_BLOB_SHA256, ABSENT_SHA256] {
        for args in [
            &["rev-parse", "--verify", oid][..],
            &["cat-file", "-t", oid][..],
            &["cat-file", "-e", oid][..],
            &["cat-file", "-s", oid][..],
            &["cat-file", "-p", oid][..],
            &["ls-tree", oid][..],
            &["rev-list", oid][..],
            &["log", "--oneline", oid][..],
            &["update-ref", "refs/heads/n", oid][..],
        ] {
            out.push(Case::new(args[0], args, Shape::Linear));
        }
    }

    // The SHA-1 constants in the same SHA-1 repository, which is the baseline
    // the group is read against: `rev-parse --verify 4b825dc6…` is exit 0 on
    // both sides and *should* be, so a port that simply accepted every
    // 40-or-64-hex constant would be indistinguishable without these.
    for oid in [EMPTY_TREE_SHA1, EMPTY_BLOB_SHA1] {
        out.push(Case::new("rev-parse", &["rev-parse", "--verify", oid], Shape::Linear));
        out.push(Case::new("cat-file", &["cat-file", "-t", oid], Shape::Linear));
        out.push(Case::new("cat-file", &["cat-file", "-s", oid], Shape::Linear));
        out.push(Case::new("ls-tree", &["ls-tree", oid], Shape::Linear));
    }

    // And the reverse: the SHA-1 constants inside a repository declaring
    // SHA-256. `update-ref refs/heads/n 4b825dc6…` is defect 1 in the form a
    // single invocation can reach it — stock 128 and nothing written, port 0
    // with a 40-hex value and a reflog entry left in a 64-hex repository.
    for oid in [EMPTY_TREE_SHA1, EMPTY_BLOB_SHA1] {
        for args in [
            &["rev-parse", "--verify", oid][..],
            &["cat-file", "-t", oid][..],
            &["cat-file", "-p", oid][..],
            &["ls-tree", oid][..],
            &["update-ref", "refs/heads/n", oid][..],
        ] {
            out.push(
                Case::new(args[0], args, Shape::Linear).with_scoped_config(declare_sha256()),
            );
        }
    }

    // The oid arriving on stdin instead of in argv: `cat-file --batch` reads it
    // through a different parser, and stock answers `<oid> missing` at exit 0
    // where the port panics at 101.
    out.push(Case::with_stdin(
        "cat-file",
        &["cat-file", "--batch-check"],
        Shape::Linear,
        b"6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321\n",
    ));
    out.push(Case::with_stdin(
        "cat-file",
        &["cat-file", "--batch"],
        Shape::Linear,
        b"6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321\n",
    ));
    out.push(Case::with_stdin(
        "cat-file",
        &["cat-file", "--batch-check"],
        Shape::Linear,
        b"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\n",
    ));
    out.push(Case::with_stdin(
        "cat-file",
        &["cat-file", "--batch-check"],
        Shape::Linear,
        b"4b825dc642cb6eb9a060e54bf8d69288fbee4904\n6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321\n",
    ));
}

/// A **SHA-256 packfile**, produced by stock 2.55.0 in an
/// `init --object-format=sha256` repository under this harness's pinned
/// identity and clock (three objects: one commit, one tree, one blob). Its
/// trailer is a 32-byte SHA-256 checksum where a SHA-1 pack carries 20.
///
/// A literal rather than a fixture file because no `Shape` is a SHA-256
/// repository and this file may not add one, and because `index-pack --stdin`
/// and `unpack-objects` take their whole input this way. 222 bytes; pinned by
/// `tests::sha256_stream_literals_are_intact`.
const PACK_SHA256: &[u8] =
    b"PACK\x00\x00\x00\x02\x00\x00\x00\x03\x9c\x08x\x9c}\xca=\n\xc3\x30\x0c@\xe1\xdd\xa7\xd0\
    \xde%\x8a\xad\xca\x86Rr\x15\x45?\xb4\x43H\x08\x0e\xe4\xf8m\xa0s\xdf\xf0\xa6\xaf\xef\xee\
    \x30VS\xf2R\x98g\x16\x36G\x92p+\xc3\xd8\xb0\x84\x84U\x0c\xcb\xa6w\xcdu\xa6\xaaM\xbe:\xd8\
    \x04\xdd\x32QkI\x8e\xfeZw\xd8\xe0\xb1M\xfe\x04\xe4\xe1\x17\xdc\xae']\x97\xe5\xdd\xbb\xff\
    !\xe9L\x1f<\x8d&A\xad\x02x\x9c\x33\x34\x30\x30\x33\x31QH\xd3+\xa9(a\x98v\xb0\x9fi\xfa\
    \xe3^\xc6/\x9bn\x9d\xbd\xb7\x44u\xd6\xabl\xcd\xc4\xd7\x1cJ{ux\xec\x13\x34\x99\x19\\\x01\
    \x99\x66\x11\xbb\x33x\x9c\xcb\xc8\xe4\x02\x00\x02\x17\x00\xdc\x1e\xb9?k\x8cK\t\x98\x9f\
    \x01\xb9\x30\xe0\xf0\xd5\xfa\xc9\xae\x41&\x84&\xc2\xbb\x83\xf7K\xc3\xaa\x42\x07W";

/// The same three objects in a **v3 bundle whose header says
/// `@object-format=sha256`** — the container field that tells a reader which
/// algorithm the 64-hex ref lines are in.
///
/// `interchange.rs` owns the bundle containers and every literal it holds is
/// `@object-format=sha1`; this is the only SHA-256 one in the corpus. 414
/// bytes.
const BUNDLE_SHA256: &[u8] =
    b"# v3 git bundle\n@object-format=sha256\na1299389c9311ede9197acb029ce2f818d4fef355a08e924\
    8cd706251857ef7d refs/heads/master\na1299389c9311ede9197acb029ce2f818d4fef355a08e9248cd7\
    06251857ef7d HEAD\n\nPACK\x00\x00\x00\x02\x00\x00\x00\x03\x9c\x08x\x9c}\xca=\n\xc3\x30\
    \x0c@\xe1\xdd\xa7\xd0\xde%\x8a\xad\xca\x86Rr\x15\x45?\xb4\x43H\x08\x0e\xe4\xf8m\xa0s\xdf\
    \xf0\xa6\xaf\xef\xee\x30VS\xf2R\x98g\x16\x36G\x92p+\xc3\xd8\xb0\x84\x84U\x0c\xcb\xa6w\
    \xcdu\xa6\xaaM\xbe:\xd8\x04\xdd\x32QkI\x8e\xfeZw\xd8\xe0\xb1M\xfe\x04\xe4\xe1\x17\xdc\
    \xae']\x97\xe5\xdd\xbb\xff!\xe9L\x1f<\x8d&A\xad\x02x\x9c\x33\x34\x30\x30\x33\x31QH\xd3+\
    \xa9(a\x98v\xb0\x9fi\xfa\xe3^\xc6/\x9bn\x9d\xbd\xb7\x44u\xd6\xabl\xcd\xc4\xd7\x1cJ{ux\
    \xec\x13\x34\x99\x19\\\x01\x99\x66\x11\xbb\x33x\x9c\xcb\xc8\xe4\x02\x00\x02\x17\x00\xdc\
    \x1e\xb9?k\x8cK\t\x98\x9f\x01\xb9\x30\xe0\xf0\xd5\xfa\xc9\xae\x41&\x84&\xc2\xbb\x83\xf7K\
    \xc3\xaa\x42\x07W";

/// A SHA-256 **stream** — pack or bundle — delivered to a SHA-1 repository.
///
/// This is the interop refusal in the one direction a single invocation can
/// reach: the far side is a literal on stdin rather than a second repository,
/// so no `init` has to run first. What it answers is whether the boundary is
/// enforced at the *container* (the format is declared, so refuse) or only at
/// the *checksum* (try, and fail when the trailer does not verify).
///
/// Measured on stock 2.55.0, and the answer is the second one — for **both**
/// implementations, identically:
///
/// ```text
/// $ git bundle verify -      < sha256.bundle
///   The bundle contains these 2 refs: … The bundle uses this hash algorithm: sha256   (rc 0)
/// $ git bundle list-heads -  < sha256.bundle       both print the two 64-hex refs     (rc 0)
/// $ git bundle unbundle -    < sha256.bundle
///   fatal: pack is corrupted (SHA1 mismatch) / error: index-pack died                 (rc 1)
/// $ git index-pack --stdin -o out.idx < sha256.pack
///   fatal: pack is corrupted (SHA1 mismatch)                                          (rc 128)
/// $ git unpack-objects       < sha256.pack
///   fatal: final sha1 did not match                                                   (rc 128)
/// ```
///
/// So neither refuses at the declaration — `bundle verify` reads the SHA-256
/// header, believes it, and reports the algorithm — and both then fail at the
/// trailer, where a 32-byte checksum is read as a 20-byte one. That agreement
/// is worth pinning precisely because it is the behaviour a port is most likely
/// to "improve": a port that started refusing at the header would diverge here,
/// and a port that ignored the header and unpacked anyway would corrupt the
/// repository.
///
/// `index-pack --stdin --object-format=sha256` is included as the thing a
/// caller reaches for next, and it is refused for an unrelated reason
/// (`options '--object-format' and '--stdin' cannot be used together`) — which
/// `object_pack.rs` already covers on a SHA-1 pack. It is repeated here with a
/// SHA-256 pack because the option check must fire *before* the stream is
/// looked at, and only feeding a stream of the other format shows that it does.
fn cross_format_streams(out: &mut Vec<Case>) {
    for (cmd, args) in [
        ("bundle", &["bundle", "verify", "-"][..]),
        ("bundle", &["bundle", "list-heads", "-"][..]),
        ("bundle", &["bundle", "unbundle", "-"][..]),
        ("bundle", &["bundle", "list-heads", "-", "refs/heads/master"][..]),
    ] {
        out.push(Case::with_stdin(cmd, args, Shape::Linear, BUNDLE_SHA256));
    }
    // The same bundle into a shape that already has packs, so the failure is
    // not a property of an empty pack directory.
    out.push(Case::with_stdin("bundle", &["bundle", "unbundle", "-"], Shape::Packed, BUNDLE_SHA256));

    for (cmd, args) in [
        ("index-pack", &["index-pack", "--stdin", "-o", "out.idx"][..]),
        ("index-pack", &["index-pack", "--stdin", "--object-format=sha256", "-o", "out.idx"][..]),
        ("index-pack", &["index-pack", "--stdin", "--fix-thin", "-o", "out.idx"][..]),
        ("unpack-objects", &["unpack-objects"][..]),
        ("unpack-objects", &["unpack-objects", "-n"][..]),
    ] {
        out.push(Case::with_stdin(cmd, args, Shape::Linear, PACK_SHA256));
    }
    out.push(Case::with_stdin("unpack-objects", &["unpack-objects", "-q"], Shape::Packed, PACK_SHA256));

    // The bundle read *by* a repository that declares SHA-256 over SHA-1
    // objects: now the container and the declaration agree with each other and
    // disagree with the store, which is the third corner of the square.
    out.push(
        Case::with_stdin("bundle", &["bundle", "list-heads", "-"], Shape::Linear, BUNDLE_SHA256)
            .with_scoped_config(declare_sha256()),
    );
    out.push(
        Case::with_stdin("bundle", &["bundle", "unbundle", "-"], Shape::Linear, BUNDLE_SHA256)
            .with_scoped_config(declare_sha256()),
    );
}

/// A repository that **declares SHA-256** talking to a **SHA-1 peer**.
///
/// This is as far as one invocation reaches toward a real cross-format fetch,
/// and the header says why: a genuine SHA-256 repository would need
/// `init --object-format=sha256` first, which makes it a sequence.
/// `Shape::BehindRemote` supplies the other half for free — a bare SHA-1 peer
/// at `./.remote.git`, reached by a relative URL, with `main` set up to track
/// it — so the only thing a case has to add is the declaration.
///
/// The question is where the two implementations notice. Measured against stock
/// in a hand-built repository shaped like this one — a worktree with a bare
/// peer at `./.remote.git` and `main` tracking it — with the two keys appended
/// to `.git/config`. The oids and the index magic bytes below belong to that
/// replica rather than to the fixture, so only the messages and the exit codes
/// are claimed:
///
/// ```text
/// $ git ls-remote origin
///   <40-hex>  refs/heads/main                                      both, rc 0
/// $ git fetch origin
///   stock: fatal: unknown index entry format 0x…                       (rc 128)
///   port : zvcs: fetch: The reference at "refs/heads/main" …            (rc 1)
/// $ git push origin main
///   stock: fatal: refs/heads/main cannot be resolved to branch          (rc 128)
///   port : error: src refspec main does not match any                   (rc 1)
/// ```
///
/// Two things follow, and both are recorded rather than fixed here. First,
/// **neither side refuses at the format boundary**: `ls-remote` succeeds on
/// both and prints the peer's 40-hex oids into a repository that believes in
/// 64-hex ones, because the advertisement is read before anything local is.
/// Second, the disagreement that does exist is the exit code, not the decision
/// — stock dies on the local side's unreadable index and refs at 128, the port
/// reports a gitoxide parse failure at 1. Nothing was written on either side by
/// the failing `fetch`; that was checked, and it is the reassuring half of the
/// finding.
///
/// The `push` row is the one to watch in future runs. It fails today for a
/// local reason (`main` cannot be resolved), so it does *not* yet measure
/// whether the port would push SHA-1 objects into a peer while claiming
/// SHA-256. If the reader-side defects above are fixed, this case becomes the
/// one that asks that question, and it is here so that it does.
fn cross_format_transport(out: &mut Vec<Case>) {
    for args in [
        &["fetch", "origin"][..],
        &["fetch", "origin", "main"][..],
        &["fetch", "--all"][..],
        &["fetch", "--dry-run", "origin"][..],
        &["fetch", "--prune", "origin"][..],
        &["pull", "origin", "main"][..],
        &["push", "origin", "main"][..],
        &["push", "--dry-run", "origin", "main"][..],
        &["ls-remote", "origin"][..],
        &["ls-remote", "--heads", "origin"][..],
        &["ls-remote", "./.remote.git"][..],
        &["remote", "show", "-n", "origin"][..],
    ] {
        out.push(
            Case::new(args[0], args, Shape::BehindRemote).with_scoped_config(declare_sha256()),
        );
    }

    // The peer's own format is unaffected by the local declaration, and
    // `--git-dir` reaches it directly: this must still answer `sha1`.
    out.push(
        Case::new("rev-parse", &["rev-parse", "--show-object-format"], Shape::BehindRemote)
            .with_globals(&[&["--git-dir", ".remote.git"]])
            .with_scoped_config(declare_sha256()),
    );
    out.push(
        Case::new("rev-parse", &["rev-parse", "--show-object-format"], Shape::BehindRemote)
            .with_globals(&[&["--git-dir", ".remote.git"]]),
    );

    // A submodule host declaring SHA-256 over SHA-1 objects: `submodule` is the
    // verb that would have to reconcile two repositories' formats, and this is
    // the closest a single invocation gets to asking it to.
    for args in [&["submodule", "status"][..], &["submodule", "summary"][..], &["submodule"][..]] {
        out.push(
            Case::new(args[0], args, Shape::Submodule).with_scoped_config(declare_sha256()),
        );
    }
}

/// The two rules that decide whether `extensions.objectFormat` is read at all:
/// the **repository format version** that gates it, and the **scope** it has to
/// come from.
///
/// Both are places a port can be wrong without any hash arithmetic being
/// involved, and both were unmeasured.
///
/// **The version gate.** `extensions.*` is a v1-only namespace. Setting
/// `extensions.objectFormat = sha256` without raising
/// `core.repositoryFormatVersion` to 1 is not a SHA-256 repository and is not a
/// SHA-1 repository — it is an error, and the error names the extension:
///
/// ```text
/// fatal: repo version is 0, but v1-only extension found:
/// 	objectformat
/// ```
///
/// Both sides produced that, byte for byte, from `rev-parse
/// --show-object-format`, `status`, `log`, `cat-file -t HEAD` and
/// `for-each-ref` alike, so these are strict. Without them a port that ignored
/// the version gate entirely — and so read the extension in a v0 repository —
/// would score identically to one that honours it, because every other case in
/// this file sets the version.
///
/// `config --get` is the exception, and it is why the group includes it:
/// `config` runs without full repository setup, so the same condition is a
/// **warning** rather than a fatal and the command carries on to exit 1 with
/// empty stdout. The port exits 1 with empty stdout too and prints **no
/// warning at all**:
///
/// ```text
/// $ git config --get extensions.objectFormat     # v0 repo, extension set
/// stock: warning: repo version is 0, but v1-only extension found:
///        	objectformat                                     (rc 1, no stdout)
/// port : (silent)                                           (rc 1, no stdout)
/// ```
///
/// That difference is invisible to stdout, exit code and state alike, so the
/// case is only worth anything because it is strict. It is narrow: the port
/// reproduces the *fatal* form byte for byte on the five verbs above, so it is
/// running the version check — it is the non-fatal, keep-going form on
/// `config`'s reduced setup path that it does not report.
///
/// **The scope.** `extensions.*` is read from the repository's own config and
/// from nowhere else; `-c extensions.objectFormat=sha256` on the command line
/// is silently ignored, and `git rev-parse --show-object-format` under it still
/// answers `sha1`. Both sides agreed. This matters more than it looks: `-c` is
/// how the rest of the corpus delivers configuration, so a port that honoured
/// the extension from `-c` would be *reachable from the command line* in a way
/// git is not, and every case in this file that uses [`ConfigScope::Repo`]
/// would be testing a path the port also exposes elsewhere.
///
/// **A bogus value** is the third cell: `extensions.objectFormat = bogus` with
/// the version raised. Stock refuses at 128 on both verbs; the port refuses at
/// 128 for `rev-parse --show-object-format` and at 1 for `status`, which is the
/// same exit-code split as [`declared_sha256`] and is why the pair is here.
fn extension_gating(out: &mut Vec<Case>) {
    // The extension without the version bump: refused, and the message names it.
    let ungated = || {
        vec![ConfigEntry::set(ConfigScope::Repo, "extensions.objectFormat", "sha256")]
    };
    for args in [
        &["rev-parse", "--show-object-format"][..],
        &["status", "--porcelain"][..],
        &["log", "--oneline"][..],
        &["cat-file", "-t", "HEAD"][..],
        &["for-each-ref"][..],
        &["config", "--get", "extensions.objectFormat"][..],
    ] {
        out.push(Case::strict(args[0], args, Shape::Linear).with_scoped_config(ungated()));
    }
    // Explicitly version 0, rather than relying on the fixture's default, so
    // the premise is in the case id.
    out.push(
        Case::strict("rev-parse", &["rev-parse", "--show-object-format"], Shape::Linear)
            .with_scoped_config(vec![
                ConfigEntry::set(ConfigScope::Repo, "core.repositoryFormatVersion", "0"),
                ConfigEntry::set(ConfigScope::Repo, "extensions.objectFormat", "sha256"),
            ]),
    );

    // A value the gate lets through and the parser does not.
    let bogus = || {
        vec![
            ConfigEntry::set(ConfigScope::Repo, "core.repositoryFormatVersion", "1"),
            ConfigEntry::set(ConfigScope::Repo, "extensions.objectFormat", "bogus"),
        ]
    };
    for args in [
        &["rev-parse", "--show-object-format"][..],
        &["status", "--porcelain"][..],
        &["log", "--oneline"][..],
        &["fsck"][..],
    ] {
        out.push(Case::new(args[0], args, Shape::Linear).with_scoped_config(bogus()));
    }

    // The command-line scope, which git does not read extensions from.
    out.push(Case::new(
        "rev-parse",
        &["rev-parse", "--show-object-format"],
        Shape::Linear,
    )
    .with_config(&[("extensions.objectFormat", "sha256")]));
    out.push(Case::new(
        "rev-parse",
        &["rev-parse", "--show-object-format"],
        Shape::Linear,
    )
    .with_config(&[
        ("core.repositoryFormatVersion", "1"),
        ("extensions.objectFormat", "sha256"),
    ]));
    out.push(Case::new("status", &["status", "--porcelain"], Shape::Linear).with_config(&[
        ("core.repositoryFormatVersion", "1"),
        ("extensions.objectFormat", "sha256"),
    ]));
    out.push(
        Case::new("rev-parse", &["rev-parse", "--show-object-format"], Shape::Linear).with_config(
            &[
                ("core.repositoryFormatVersion", "1"),
                ("extensions.objectFormat", "bogus"),
            ],
        ),
    );
    // And from the *global* scope, which is a file but the wrong one: an
    // extension is a property of a repository, so a global setting must not
    // change the answer either.
    out.push(
        Case::new("rev-parse", &["rev-parse", "--show-object-format"], Shape::Linear)
            .with_scoped_config(vec![
                ConfigEntry::set(ConfigScope::Global, "core.repositoryFormatVersion", "1"),
                ConfigEntry::set(ConfigScope::Global, "extensions.objectFormat", "sha256"),
            ]),
    );

    // The version alone, with no extension: v2 and above are unknown to git and
    // refused. The pair brackets the gate from the other side.
    for version in ["0", "1", "2", "99"] {
        out.push(
            Case::new("status", &["status", "--porcelain"], Shape::Linear).with_scoped_config(
                vec![ConfigEntry::set(
                    ConfigScope::Repo,
                    "core.repositoryFormatVersion",
                    version,
                )],
            ),
        );
        out.push(
            Case::new("rev-parse", &["rev-parse", "--show-object-format"], Shape::Linear)
                .with_scoped_config(vec![ConfigEntry::set(
                    ConfigScope::Repo,
                    "core.repositoryFormatVersion",
                    version,
                )]),
        );
    }
}

/// `clone` asked, through `-c`, to stamp an object format onto the repository
/// it is about to create — **the shortest path in this file to a repository
/// neither implementation can read.**
///
/// `--object-format` is not a `clone` option ([`format_flag_where_git_has_none`]
/// establishes that both sides agree it is not), so `-c` is where a caller goes
/// next. `clone` persists `-c` settings into the new repository's config, and
/// git makes exactly one exception to that: **the two extensions that describe
/// the repository's own format are filtered out**, because the clone decides
/// them and a caller must not be able to assert them after the fact. Measured
/// by cloning under four different `extensions.*` keys and reading the new
/// config back:
///
/// ```text
/// git clone -c <key> . copy      →  copy/.git/config [extensions] contains…
///
/// key                              stock 2.55.0            port
/// extensions.objectFormat=sha256   (filtered out)          objectFormat = sha256
/// extensions.refStorage=reftable   (filtered out)          refStorage = reftable
/// extensions.worktreeConfig=true   worktreeconfig = true   worktreeConfig = true
/// extensions.noSuchThing=1         nosuchthing = 1         noSuchThing = 1
/// ```
///
/// The port filters neither, so one `-c` and an exit code of 0 produce a
/// repository that **both binaries then refuse to open**:
///
/// ```text
/// $ git clone -c extensions.objectFormat=sha256 . copy      # rc 0, both sides
/// $ cd copy && git log --oneline
/// stock: fatal: repo version is 0, but v1-only extension found:
///                objectformat
/// port : fatal: repo version is 0, but v1-only extension found:
///                objectformat
/// ```
///
/// The two negative controls are what make that a measurement rather than an
/// anecdote: `worktreeConfig` and an invented `noSuchThing` are persisted by
/// **both** sides, so the port is not persisting everything indiscriminately
/// and stock is not filtering `extensions.*` wholesale. The filter is specific
/// to the format extensions, and the port does not have it. (The rows also show
/// a smaller difference in passing: stock lowercases the key as it writes it
/// and the port preserves the spelling it was given.)
///
/// `refStorage` is `ref_storage.rs`'s extension, not this file's, and it is
/// here for one reason: it is the *other half of the same filter*. A reader who
/// saw only the `objectFormat` row could not tell whether git filters one key
/// or a class, and that is the difference between "the port missed a case" and
/// "the port does not implement the rule".
///
/// # The second, louder form: `-c core.repositoryFormatVersion=1`
///
/// Setting the version alongside the extension makes stock abort the clone
/// outright rather than filter anything, because `clone` writes
/// `core.repositoryformatversion = 0` into the new config and cannot when the
/// key already carries a `-c` value:
///
/// ```text
/// $ git clone -c core.repositoryFormatVersion=1 \
///             -c extensions.objectFormat=sha256 . copy
/// stock: warning: core.repositoryformatversion has multiple values
///        fatal: could not set 'core.repositoryformatversion' to '0'      (rc 128)
///        # and no `copy` directory survives
/// port : done. / warning: remote HEAD refers to nonexistent ref          (rc 0)
/// ```
///
/// What the port leaves is a v1 repository declaring SHA-256, carrying 40-hex
/// loose objects and a 40-hex `refs/heads/main`, with
/// `core.repositoryformatversion` written twice. Reading it back was checked
/// with both binaries: stock answers `fatal: your current branch appears to be
/// broken`, `fatal: unknown index entry format 0xbd6a0000` and `error:
/// refs/heads/main: badRefContent`; the port answers that the ref `could not be
/// parsed`.
///
/// The **trigger** there is `-c core.repositoryFormatVersion=1` by itself —
/// stock aborts identically with no extension set at all — so the control case
/// with the version and nothing else is included. Without it a reader would
/// read this half as an `extensions.objectFormat` defect, and it is not: it is
/// a duplicated version key, with the extension deciding whether the surviving
/// repository is merely odd or unreadable. The general `clone -c` behaviour
/// belongs to whichever module owns `clone`'s option handling; what is claimed
/// here is the filter above and the format of the wreckage below.
fn clone_into_a_lying_repository(out: &mut Vec<Case>) {
    // The filter, in the four keys that map it out. Strict: both sides print
    // `Cloning into 'copy'... / done.` and the whole difference is in the
    // config the clone was left holding, which the state probe compares.
    for key in [
        "extensions.objectFormat=sha256",
        "extensions.refStorage=reftable",
        "extensions.worktreeConfig=true",
        "extensions.noSuchThing=1",
    ] {
        out.push(Case::strict("clone", &["clone", "-c", key, ".", "copy"], Shape::Linear));
    }
    // The same filter on a bare clone, which takes a different code path to the
    // new config.
    out.push(Case::new(
        "clone",
        &["clone", "-c", "extensions.objectFormat=sha256", "--bare", ".", "copy.git"],
        Shape::Linear,
    ));
    // And a key that is not an extension at all, as the floor: if this one
    // diverged, the finding above would be about `-c` persistence in general
    // rather than about the format extensions.
    out.push(Case::strict(
        "clone",
        &["clone", "-c", "diff.algorithm=patience", ".", "copy"],
        Shape::Linear,
    ));

    // The control for the second form: the version key alone, no extension.
    // Stock aborts; the port does not. This is what says the trigger is the
    // version and not the format.
    out.push(Case::new(
        "clone",
        &["clone", "-c", "core.repositoryFormatVersion=1", ".", "copy"],
        Shape::Linear,
    ));

    // The unreadable result. The value the extension carries does not matter —
    // `sha1` and `bogus` abort stock just the same, because it is the duplicated
    // version key that stops the clone — so all three are here, and the bare and
    // multi-commit variants below vary what is left stranded rather than what
    // triggers it.
    for value in ["sha256", "sha1", "bogus"] {
        let setting = format!("extensions.objectFormat={value}");
        out.push(Case::new(
            "clone",
            &["clone", "-c", "core.repositoryFormatVersion=1", "-c", &setting, ".", "copy"],
            Shape::Linear,
        ));
    }
    out.push(Case::new(
        "clone",
        &[
            "clone",
            "-c",
            "core.repositoryFormatVersion=1",
            "-c",
            "extensions.objectFormat=sha256",
            "--bare",
            ".",
            "copy.git",
        ],
        Shape::Linear,
    ));
    // And from a shape whose history is not a single commit, so the objects
    // stranded in the SHA-256-declaring repository are more than one.
    out.push(Case::new(
        "clone",
        &[
            "clone",
            "-c",
            "core.repositoryFormatVersion=1",
            "-c",
            "extensions.objectFormat=sha256",
            ".",
            "copy",
        ],
        Shape::Branched,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two SHA-256 stream literals are the only bytes in this file that
    /// cannot be read at a glance, and a line-continuation escape that ate a
    /// byte would turn every case that uses them into a comparison of two
    /// identical failures — a silent pass. Pinned by length, by the container
    /// magic at the front, and by the last four bytes of the SHA-256 trailer.
    #[test]
    fn sha256_stream_literals_are_intact() {
        assert_eq!(PACK_SHA256.len(), 222, "SHA-256 pack literal changed length");
        assert_eq!(&PACK_SHA256[..4], b"PACK");
        assert_eq!(u32::from_be_bytes(PACK_SHA256[8..12].try_into().unwrap()), 3);
        // A SHA-256 pack ends in a 32-byte checksum; a SHA-1 one in 20.
        assert_eq!(&PACK_SHA256[PACK_SHA256.len() - 4..], b"\xaa\x42\x07W");
    }

    /// The bundle carries the pack verbatim after its header, which is what
    /// makes the two literals checkable against each other rather than each
    /// against a number typed twice.
    #[test]
    fn bundle_literal_declares_sha256_and_embeds_the_pack() {
        assert!(BUNDLE_SHA256.starts_with(b"# v3 git bundle\n@object-format=sha256\n"));
        let split = BUNDLE_SHA256
            .windows(6)
            .position(|w| w == b"\n\nPACK")
            .expect("bundle has no pack section");
        assert_eq!(&BUNDLE_SHA256[split + 2..], PACK_SHA256);
    }

    /// Every oid constant is the width its name claims, so a typo cannot make a
    /// "SHA-256" case quietly test a 40-hex string.
    #[test]
    fn oid_constants_have_the_width_their_names_claim() {
        for short in [EMPTY_TREE_SHA1, EMPTY_BLOB_SHA1] {
            assert_eq!(short.len(), 40, "{short} is not 40 hex characters");
        }
        for short in [ABSENT_SHA1] {
            assert_eq!(short.len(), 40, "{short} is not 40 hex characters");
        }
        for long in [
            EMPTY_TREE_SHA256,
            EMPTY_BLOB_SHA256,
            FOREIGN_COMMIT_SHA256,
            NULL_SHA256,
            ABSENT_SHA256,
        ] {
            assert_eq!(long.len(), 64, "{long} is not 64 hex characters");
        }
        for oid in [
            EMPTY_TREE_SHA1,
            EMPTY_BLOB_SHA1,
            EMPTY_TREE_SHA256,
            EMPTY_BLOB_SHA256,
            FOREIGN_COMMIT_SHA256,
            NULL_SHA256,
            ABSENT_SHA256,
        ] {
            assert!(oid.bytes().all(|b| b.is_ascii_hexdigit()), "{oid} is not hex");
        }
    }
}


