//! Differential corpus cases for the **object store and the pack plumbing** —
//! `cat-file`, `write-tree`, `pack-objects`, `index-pack`, `verify-pack`,
//! `show-index`, `unpack-file`, `prune-packed`, `count-objects`,
//! `pack-redundant`, `commit-graph` and `multi-pack-index`.
//!
//! Every case here is compared against stock git for stdout, exit code and
//! post-command repository state.
//!
//! # Why this layer gets its own module
//!
//! Everything else in git sits on top of it. A `log` that prints the right
//! commits over an object store that answers `%(objectsize:disk)` wrong is a
//! `log` that will be wrong the moment somebody asks it something the wrong
//! answer feeds. So the assertions below are deliberately made at the lowest
//! level that can still be spelled as one argv: the type of an object, its size,
//! its delta base, the id of the tree an index serializes to, the bytes of a
//! pack, and the exact set of objects on disk afterwards.
//!
//! `runner::probe_state` runs `cat-file --batch-check --batch-all-objects` after
//! every case. That listing is this module's main instrument: it proves an
//! object a command *claimed* to write actually landed, and it catches a
//! `prune-packed` or an `index-pack --stdin` that removed or duplicated one.
//!
//! # Object ids in an argv must be constants of the hash function
//!
//! The two sides run in **separate copies** of the fixture. An id read out of
//! one of them is a fact about that copy, not about git, and hard-coding one
//! would break the moment `fixture.rs` changes a byte. So an object named in an
//! argv or in a stdin payload here is one of:
//!
//!   * the empty blob `e69de29bb2d1d6434b8b29ae775ad8c2e48c5391`,
//!   * the empty tree `4b825dc642cb6eb9a060e54bf8d69288fbee4904`,
//!   * the all-zero oid, or
//!   * a revision the fixture resolves — `HEAD`, `HEAD^{tree}`, `HEAD:README.md`,
//!     `v0.2.0`, `v0.2.0^{}`.
//!
//! Measured against stock 2.55.0, the two hash constants are not
//! interchangeable: the empty **tree** is synthesized by `lookup_object` and
//! answers `cat-file -t` in every repository, while the empty **blob** is in no
//! fixture shape and answers `fatal: Not a valid object name`. Both halves are
//! pinned below, because a port that hard-codes one of them into its object
//! store gets the other one wrong.
//!
//! # What cannot be expressed as a literal, and what is done instead
//!
//! * **A real pack.** [`ONE_OBJECT_PACK`] is 69 bytes and is spelled out; it is
//!   the same payload `corpus/stdin_plumbing.rs` uses for `unpack-objects`,
//!   re-spelled here because a `const` does not cross module privacy and because
//!   this module needs the truncated and checksum-damaged variants beside it.
//!   Anything larger is reached through [`Shape::Packed`], which tracks
//!   `packs/sample.pack`, `packs/sample.idx` and `packs/unindexed.pack` at
//!   stable worktree paths precisely so an argv can name one.
//! * **A real pack index.** `show-index` reads its `.idx` from stdin and from
//!   nowhere else (`git show-index -h`: `< <pack-idx-file>`), so without a
//!   literal its success path is unreachable — the six cases the corpus had for
//!   it were all header errors. [`one_object_idx`] builds the 1128-byte v2 index
//!   of [`ONE_OBJECT_PACK`] at compile time, field by field, so the layout is
//!   documented by the code that produces it rather than buried in 4KB of hex.
//! * **A pack file's own name.** It embeds the pack checksum, so no case can
//!   name `.git/objects/pack/pack-<sha>.pack`. Cases that need a pack by name
//!   use the `Shape::Packed` worktree copies; cases that need *all* packs use
//!   `--all` or `--object-dir`.
//!
//! # Malformed input is most of the contract here
//!
//! For this family a refusal is not an edge case, it is the specification. A
//! truncated pack, an index whose magic is wrong, an `--object-format` that does
//! not match the repository, a tree-ish where a blob is required, a well-formed
//! oid that names nothing, a `%(…)` atom `cat-file` does not have — each is a
//! documented `die()` with a documented exit code, and a port that accepts any
//! of them silently produces or reports objects git would refuse. The
//! error-path share of this module is therefore well above the corpus-wide
//! guideline, and those cases use [`Case::strict`] so the exit code *and* the
//! message are both pinned.
//!
//! `usage:` blocks are the exception and are never `strict`: the harness's
//! standing policy is that error prose is outside the compatibility surface, and
//! a `parse_options()` usage dump tracks git's own option table.
//!
//! # Two facts about stock git that shape the cases below
//!
//! * **`show-index` does not verify the index trailer.** Fed
//!   [`ONE_OBJECT_IDX`] with its last byte zeroed, or truncated eight bytes
//!   short of the trailing checksum, stock 2.55.0 prints both entries and exits
//!   0 — `builtin/show-index.c` reads the header, the fanout and the tables it
//!   needs and never hashes what it read. Both are pinned, because a port that
//!   validates the checksum is *stricter than git* and would reject an index git
//!   accepts.
//! * **`pack.threads` changes nothing observable.** Measured on
//!   [`Shape::Packed`], `pack-objects --all --revs --stdout` produces byte-identical
//!   output with and without `-c pack.threads=1`, over `--no-reuse-delta`,
//!   `--window=0`, `--depth=0`, `--compression=0` and `--filter=blob:none`. So
//!   the thread count is pinned only where *that invariance* is the assertion,
//!   and never used as a determinism crutch elsewhere. The four config keys that
//!   do move the bytes — `pack.window`, `pack.depth`, `core.compression`,
//!   `core.bigFileThreshold` — each get a case.
//!
//! # What is not measured, and why
//!
//! * **`unpack-file`'s success path.** It writes to a `.merge_file_XXXXXX` name
//!   chosen at run time and prints it, so stock cannot reproduce its own stdout
//!   and the case can only ever score Nondeterministic. Only its refusals are
//!   asserted here; `corpus/plumbing_objects.rs` keeps the one success case for
//!   the crash/exit evidence it still carries.
//! * **`multi-pack-index --object-dir=<missing>`.** Stock 2.55.0 exits 139
//!   (`SIGSEGV`) after printing the *absolute* path of the directory it could not
//!   open. The path differs between the two sides by construction — they are two
//!   different fixture copies — so the case could never match, and a crash is
//!   not a contract. Deliberately absent.
//! * **The `multi-pack-index` file and `*.bitmap`.** `runner::probe_storage`
//!   counts `.pack`/`.idx`/`.rev`/`.mtimes` by extension and the midx file has
//!   none, so a midx-only or bitmap-only difference scores MATCH. The
//!   `multi-pack-index` cases below are kept for the exit codes, the loose/pack
//!   counts and the object listing they do pin.

use crate::fixture::Shape;
use crate::runner::Case;

// ---------------------------------------------------------------------------
// Payload literals
// ---------------------------------------------------------------------------

/// A real packfile, 69 bytes, holding two blobs: `parity pack payload\n`
/// (`56529051d3b2f2d729ca211ced4750974e4bc4b1`) and the empty blob.
///
/// `PACK`, version 2, object count 2, two zlib streams, and a 20-byte SHA-1
/// trailer (`c03e561f71535068bda53089723e6662a3d8feb2`) over everything before
/// it. Neither blob is in any fixture shape, so the all-objects probe tells a
/// run that indexed it apart from one that only said it had.
const ONE_OBJECT_PACK: &[u8] = b"\x50\x41\x43\x4b\x00\x00\x00\x02\x00\x00\x00\x02\
\xb4\x01\x78\x9c\x2b\x48\x2c\xca\x2c\xa9\x54\x28\
\x48\x4c\xce\x06\x12\x95\x39\xf9\x89\x29\x5c\x00\
\x51\xb0\x07\x6d\x30\x78\x9c\x03\x00\x00\x00\x00\
\x01\xc0\x3e\x56\x1f\x71\x53\x50\x68\xbd\xa5\x30\
\x89\x72\x3e\x66\x62\xa3\xd8\xfe\xb2";

/// [`ONE_OBJECT_PACK`] cut ten bytes short, inside the trailing checksum. Both
/// objects are complete, so the die is `fatal: early EOF` reading the trailer.
const TRUNCATED_PACK: &[u8] = b"\x50\x41\x43\x4b\x00\x00\x00\x02\x00\x00\x00\x02\
\xb4\x01\x78\x9c\x2b\x48\x2c\xca\x2c\xa9\x54\x28\
\x48\x4c\xce\x06\x12\x95\x39\xf9\x89\x29\x5c\x00\
\x51\xb0\x07\x6d\x30\x78\x9c\x03\x00\x00\x00\x00\
\x01\xc0\x3e\x56\x1f\x71\x53\x50\x68\xbd\xa5";

/// [`ONE_OBJECT_PACK`] with the last byte of the trailer zeroed. Every object
/// in it parses; only the checksum disagrees, so this separates a reader that
/// verifies the trailer (`fatal: pack is corrupted (SHA1 mismatch)`) from one
/// that stops at the last object.
const BAD_CHECKSUM_PACK: &[u8] = b"\x50\x41\x43\x4b\x00\x00\x00\x02\x00\x00\x00\x02\
\xb4\x01\x78\x9c\x2b\x48\x2c\xca\x2c\xa9\x54\x28\
\x48\x4c\xce\x06\x12\x95\x39\xf9\x89\x29\x5c\x00\
\x51\xb0\x07\x6d\x30\x78\x9c\x03\x00\x00\x00\x00\
\x01\xc0\x3e\x56\x1f\x71\x53\x50\x68\xbd\xa5\x30\
\x89\x72\x3e\x66\x62\xa3\xd8\xfe\x00";

/// Text where a binary header belongs.
const NOT_BINARY: &[u8] = b"this is not the binary format you are looking for\n";

/// Size of the v2 pack index of a two-object pack: 8-byte header, 256-entry
/// fanout, two object names, two CRCs, two 32-bit offsets, and the two 20-byte
/// checksums.
const IDX_LEN: usize = 8 + 256 * 4 + 2 * 20 + 2 * 4 + 2 * 4 + 20 + 20;

/// The v2 pack index of [`ONE_OBJECT_PACK`], assembled field by field.
///
/// Written as a `const fn` rather than 1128 bytes of hex so the *layout* — which
/// is what a port has to agree with — is legible: magic, version, a fanout whose
/// bucket `i` holds the number of objects whose first byte is `<= i`, the sorted
/// object names, their CRCs, their offsets into the pack, the pack's own
/// checksum, and finally the index's checksum over all of that.
///
/// Verified against stock: `git index-pack -o one.idx one.pack` over
/// [`ONE_OBJECT_PACK`] produces exactly these bytes.
const fn one_object_idx() -> [u8; IDX_LEN] {
    // 56529051d3b2f2d729ca211ced4750974e4bc4b1 — the `parity pack payload\n`
    // blob, at pack offset 12, CRC 54fbf00b.
    const OID0: [u8; 20] = [
        0x56, 0x52, 0x90, 0x51, 0xd3, 0xb2, 0xf2, 0xd7, 0x29, 0xca, 0x21, 0x1c, 0xed, 0x47, 0x50,
        0x97, 0x4e, 0x4b, 0xc4, 0xb1,
    ];
    // e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 — the empty blob, at pack offset
    // 40, CRC 6e760029.
    const OID1: [u8; 20] = [
        0xe6, 0x9d, 0xe2, 0x9b, 0xb2, 0xd1, 0xd6, 0x43, 0x4b, 0x8b, 0x29, 0xae, 0x77, 0x5a, 0xd8,
        0xc2, 0xe4, 0x8c, 0x53, 0x91,
    ];
    // c03e561f71535068bda53089723e6662a3d8feb2 — the pack trailer, repeated in
    // the index so a reader can tell the two files belong together.
    const PACK_SUM: [u8; 20] = [
        0xc0, 0x3e, 0x56, 0x1f, 0x71, 0x53, 0x50, 0x68, 0xbd, 0xa5, 0x30, 0x89, 0x72, 0x3e, 0x66,
        0x62, 0xa3, 0xd8, 0xfe, 0xb2,
    ];
    // 9a4688f64810da256b57a205907908e1a242ab8e — SHA-1 of the 1108 bytes above.
    const IDX_SUM: [u8; 20] = [
        0x9a, 0x46, 0x88, 0xf6, 0x48, 0x10, 0xda, 0x25, 0x6b, 0x57, 0xa2, 0x05, 0x90, 0x79, 0x08,
        0xe1, 0xa2, 0x42, 0xab, 0x8e,
    ];

    let mut idx = [0u8; IDX_LEN];
    // Magic `\377tOc`, then version 2.
    idx[0] = 0xff;
    idx[1] = b't';
    idx[2] = b'O';
    idx[3] = b'c';
    idx[7] = 2;

    // Fanout: cumulative counts by leading byte. OID0 leads with 0x56, OID1 with
    // 0xe6, so the table steps 0 → 1 → 2 at exactly those two buckets.
    let mut i = 0;
    while i < 256 {
        let count: u8 = if i < 0x56 {
            0
        } else if i < 0xe6 {
            1
        } else {
            2
        };
        idx[8 + i * 4 + 3] = count;
        i += 1;
    }

    let mut k = 0;
    while k < 20 {
        idx[1032 + k] = OID0[k];
        idx[1052 + k] = OID1[k];
        idx[1088 + k] = PACK_SUM[k];
        idx[1108 + k] = IDX_SUM[k];
        k += 1;
    }

    // CRC32 of each object's packed representation.
    idx[1072] = 0x54;
    idx[1073] = 0xfb;
    idx[1074] = 0xf0;
    idx[1075] = 0x0b;
    idx[1076] = 0x6e;
    idx[1077] = 0x76;
    idx[1078] = 0x00;
    idx[1079] = 0x29;

    // 32-bit offsets into the pack: 12 (just past the header) and 40.
    idx[1083] = 12;
    idx[1087] = 40;

    idx
}

/// [`one_object_idx`] with one byte replaced. The whole point of a damaged index
/// is that everything *else* about it is still well formed.
const fn idx_with(at: usize, byte: u8) -> [u8; IDX_LEN] {
    let mut idx = one_object_idx();
    idx[at] = byte;
    idx
}

const IDX_FULL: [u8; IDX_LEN] = one_object_idx();
/// The intact v2 index of [`ONE_OBJECT_PACK`].
const ONE_OBJECT_IDX: &[u8] = &IDX_FULL;

const IDX_BAD_MAGIC_ARR: [u8; IDX_LEN] = idx_with(3, b'X');
/// [`ONE_OBJECT_IDX`] with the fourth magic byte corrupted: `fatal: corrupt
/// index file`.
const BAD_MAGIC_IDX: &[u8] = &IDX_BAD_MAGIC_ARR;

const IDX_BAD_SUM_ARR: [u8; IDX_LEN] = idx_with(IDX_LEN - 1, 0);
/// [`ONE_OBJECT_IDX`] with the last byte of its own checksum zeroed. Stock still
/// prints both entries and exits 0 — see the module doc.
const BAD_SUM_IDX: &[u8] = &IDX_BAD_SUM_ARR;

/// [`ONE_OBJECT_IDX`] cut off inside the fanout table: `fatal: unable to read
/// index`.
const SHORT_IDX: &[u8] = IDX_FULL.split_at(100).0;

/// [`ONE_OBJECT_IDX`] cut eight bytes short of its trailing checksum. Every
/// table a reader needs is intact, and stock reads them and exits 0.
const NO_TRAILER_IDX: &[u8] = IDX_FULL.split_at(IDX_LEN - 8).0;

// --- cat-file batch payloads ----------------------------------------------

/// One revision of each object kind the store holds, in the order a reader is
/// most likely to get wrong: commit, tree, blob, tag.
const FOUR_KINDS: &[u8] = b"HEAD\nHEAD^{tree}\nHEAD:README.md\nv0.2.0\n";

/// [`FOUR_KINDS`] plus the two ways a name can fail to resolve: a rev that names
/// nothing, and a well-formed oid that is absent.
const FOUR_KINDS_AND_MISSES: &[u8] =
    b"HEAD\nHEAD^{tree}\nHEAD:README.md\nv0.2.0\nno-such-rev\n0000000000000000000000000000000000000000\n";

/// A rev with trailing words after it, which `%(rest)` must echo back and every
/// other atom must ignore.
const REV_WITH_REST: &[u8] = b"HEAD trailing words\nHEAD:README.md\n";

/// Two revs, LF-separated. Under `-Z` this is *one* key with an embedded
/// newline, and stock answers `missing` — the assertion that a `-Z` reader
/// really did stop splitting on LF.
const TWO_REVS_LF: &[u8] = b"HEAD\nHEAD:README.md\n";

/// The same two revs, NUL-separated, which is what `-Z` is for.
const TWO_REVS_NUL: &[u8] = b"HEAD\0HEAD:README.md\0";

/// `--batch-command` verbs against the non-buffered default.
const CMD_INFO_CONTENTS: &[u8] = b"info HEAD\ncontents HEAD:README.md\ninfo v0.2.0\n";

/// The same, ending in `flush`, which is only legal under `--buffer`.
const CMD_FLUSH: &[u8] = b"info HEAD\nflush\n";

/// Both verbs against names that resolve to nothing.
const CMD_MISSING: &[u8] = b"info no-such-rev\ncontents no-such-rev\n";

/// A verb `cat-file` does not have.
const CMD_UNKNOWN: &[u8] = b"bogus HEAD\n";

/// `info` with no operand.
const CMD_NO_ARG: &[u8] = b"info\n";

/// `--textconv`/`--filters` batch input is `<object> <path>`; a line with only
/// the object is `fatal: missing path`.
const OID_AND_PATH: &[u8] = b"HEAD:docs/manual.md docs/manual.md\n";

/// The same shape, pointing a Markdown blob at a path the attributes file marks
/// `binary` — the filter chain, not the blob, decides what comes out.
const OID_AND_BINARY_PATH: &[u8] = b"HEAD:docs/manual.md assets/logo.bin\n";

/// Object *names* for `pack-objects`, which does not take revs on stdin
/// (`fatal: expected object ID, got garbage`). The empty tree is the only name
/// that is both a literal and resolvable.
const EMPTY_TREE_OID: &[u8] = b"4b825dc642cb6eb9a060e54bf8d69288fbee4904\n";

/// The empty blob, which no fixture holds: `fatal: unable to read …`.
const EMPTY_BLOB_OID: &[u8] = b"e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n";

/// A rev, which `--revs` accepts and the bare object-list mode does not.
const HEAD_REV: &[u8] = b"HEAD\n";

/// The all-zero oid on its own line: hex-shaped, so it gets past a syntax
/// check and fails the lookup instead.
const ZERO_OID: &[u8] = b"0000000000000000000000000000000000000000\n";

/// A pack file name relative to the object directory that is not there.
const NOPE_PACK: &[u8] = b"nope.pack\n";

/// A blob named without the path `--textconv`/`--filters` need beside it:
/// `fatal: missing path`, printed *after* the record header.
const BLOB_NO_PATH: &[u8] = b"HEAD:docs/manual.md\n";

/// An index that points at nothing.
const NO_INDEX: &[(&str, &str)] = &[("GIT_INDEX_FILE", "{repo}/.git/no-such-index")];

/// An object directory that does not exist. `setup.c:is_git_directory()` checks
/// `GIT_OBJECT_DIRECTORY` in place of `<gitdir>/objects` when it is set, so this
/// does not merely misplace new objects — it stops `.git` from looking like a
/// git directory at all.
const NO_OBJECT_DIR: &[(&str, &str)] = &[("GIT_OBJECT_DIRECTORY", "{repo}/no-such-objects")];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn c(cmd: &'static str, args: &[&str], shape: Shape, out: &mut Vec<Case>) {
    out.push(Case::new(cmd, args, shape));
}

fn s(cmd: &'static str, args: &[&str], shape: Shape, out: &mut Vec<Case>) {
    out.push(Case::strict(cmd, args, shape));
}

fn si(cmd: &'static str, args: &[&str], shape: Shape, input: &'static [u8], out: &mut Vec<Case>) {
    out.push(Case::with_stdin(cmd, args, shape, input));
}

fn si_strict(
    cmd: &'static str,
    args: &[&str],
    shape: Shape,
    input: &'static [u8],
    out: &mut Vec<Case>,
) {
    out.push(Case { compare_stderr: true, ..Case::with_stdin(cmd, args, shape, input) });
}

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    cat_file_objects(out);
    cat_file_refusals(out);
    cat_file_batch(out);
    cat_file_formats(out);
    cat_file_filters(out);
    write_tree(out);
    pack_objects(out);
    index_pack(out);
    verify_and_show_index(out);
    loose_and_packed_accounting(out);
    graph_and_midx(out);
}

// ---------------------------------------------------------------------------
// cat-file
// ---------------------------------------------------------------------------

/// `-t`/`-s`/`-e`/`-p` and the `<type> <object>` form, once per object kind.
///
/// [`Shape::Branched`] is the only shape carrying an **annotated tag**, and the
/// tag object is the kind a port is most likely to have never stored: the one
/// whose header `-p` must print verbatim rather than reformat, and the one
/// `<type> <object>` must peel through when asked for `commit`. Before this
/// group the corpus asked `cat-file` about a commit five times and about nothing
/// else.
fn cat_file_objects(out: &mut Vec<Case>) {
    for args in [
        // Tree: `-s` is the size of the serialized entry table, not the entry
        // count, and `-p` renders it as text while `<type> <object>` does not.
        &["cat-file", "-t", "HEAD^{tree}"][..],
        &["cat-file", "-s", "HEAD^{tree}"][..],
        &["cat-file", "-p", "HEAD^{tree}"][..],
        // Blob.
        &["cat-file", "-t", "HEAD:README.md"][..],
        &["cat-file", "-s", "HEAD:README.md"][..],
        &["cat-file", "-p", "HEAD:README.md"][..],
        // Tag object, the peel that gets past it, and the two spellings that
        // must not peel.
        &["cat-file", "-t", "v0.2.0"][..],
        &["cat-file", "-p", "v0.2.0"][..],
        &["cat-file", "-t", "v0.2.0^{}"][..],
        &["cat-file", "tag", "v0.2.0"][..],
        &["cat-file", "commit", "v0.2.0"][..],
    ] {
        c("cat-file", args, Shape::Branched, out);
    }

    // The empty tree is synthesized rather than stored, so it answers in a
    // repository that has never written it. A port whose lookup consults only
    // the on-disk store gets all three of these wrong.
    for args in [
        &["cat-file", "-t", "4b825dc642cb6eb9a060e54bf8d69288fbee4904"][..],
        &["cat-file", "-e", "4b825dc642cb6eb9a060e54bf8d69288fbee4904"][..],
    ] {
        c("cat-file", args, Shape::Branched, out);
    }

    // `--allow-unknown-type` must not change the answer for a *known* type: it
    // switches `cat-file` onto `oid_object_info_extended`'s
    // `OBJECT_INFO_ALLOW_UNKNOWN_TYPE` path, and a port that implements that as
    // a second reader diverges here rather than on a corrupt object no literal
    // can construct.
    c("cat-file", &["cat-file", "--allow-unknown-type", "-t", "HEAD"], Shape::Branched, out);

    // Delta-bearing storage: `-s` reports the *inflated* size, which for a
    // deltified blob is nothing like what is on disk — `Packed`'s `big.txt` is
    // 6.7KB inflated and tens of bytes packed.
    c("cat-file", &["cat-file", "-s", "HEAD~3:big.txt"], Shape::Packed, out);
}

/// The `cat-file` refusals, `strict` so the diagnostic is pinned too.
///
/// Every one of these is a case where the *wrong* behaviour is to succeed: a
/// port that answers `blob` for a commit, or that invents the empty blob, has
/// produced a plausible answer to a question git refuses.
fn cat_file_refusals(out: &mut Vec<Case>) {
    for args in [
        // `<type> <object>` where the object is another type. Stock:
        // `fatal: git cat-file HEAD: bad file`, exit 128.
        &["cat-file", "blob", "HEAD"][..],
        &["cat-file", "tree", "HEAD:README.md"][..],
        &["cat-file", "commit", "HEAD^{tree}"][..],
        // The empty blob is *not* synthesized the way the empty tree is:
        // `fatal: Not a valid object name`.
        &["cat-file", "-p", "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"][..],
        // …and `-e` reports the same absence by exit code 1 with no output,
        // which is a different contract from the 128 above.
        &["cat-file", "-e", "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"][..],
        // A well-formed absent oid asked for its type is 128, not 1.
        &["cat-file", "-t", "0000000000000000000000000000000000000000"][..],
        // A path that is not in the tree.
        // Mutually exclusive selectors: exit 129, not 128.
        &["cat-file", "-t", "-s", "HEAD"][..],
        &["cat-file", "-e", "-p", "HEAD"][..],
    ] {
        s("cat-file", args, Shape::Branched, out);
    }

    // Usage blocks: exit code only, never the prose.
    c("cat-file", &["cat-file", "-t"], Shape::Branched, out);
}

/// The batch modes: the interface every tool that reads objects in bulk uses.
///
/// What a port gets wrong without these is the *record framing* — where the
/// header ends, whether a newline follows the contents, what a missing name
/// prints, and which separator `-Z` switches the reader to. None of that is
/// visible to the single-object modes above.
fn cat_file_batch(out: &mut Vec<Case>) {
    si("cat-file", &["cat-file", "--batch"], Shape::Branched, FOUR_KINDS, out);
    si("cat-file", &["cat-file", "--batch-check"], Shape::Branched, FOUR_KINDS_AND_MISSES, out);
    // A miss under `--batch` prints `<key> missing`, keeps reading, and exits 0.
    // A port that dies on the first miss agrees on nothing after it.
    si("cat-file", &["cat-file", "--batch"], Shape::Branched, FOUR_KINDS_AND_MISSES, out);

    // `--follow-symlinks` over a tree with no symlinks must be the identity. No
    // fixture shape carries a symlinked blob, so in-tree resolution is
    // unreachable; what is pinned is that the flag perturbs nothing else and
    // that a miss still reads `<key> missing`.
    si(
        "cat-file",
        &["cat-file", "--batch-check", "--follow-symlinks"],
        Shape::Branched,
        FOUR_KINDS_AND_MISSES,
        out,
    );
    // Outside a batch mode the flag is refused outright.
    c("cat-file", &["cat-file", "--follow-symlinks", "-t", "HEAD"], Shape::Branched, out);

    // The separator modes are the point. `-Z` fed LF-separated input reads the
    // whole payload as one key with an embedded newline and answers ` missing`;
    // fed NUL-separated input it answers both records. A reader that splits on
    // LF anyway agrees on the second and not the first.
    si("cat-file", &["cat-file", "--batch", "-Z"], Shape::Branched, TWO_REVS_LF, out);
    si("cat-file", &["cat-file", "--batch-check", "-Z"], Shape::Branched, TWO_REVS_NUL, out);

    // `--batch-command`: verbs on stdin instead of one implied verb.
    si("cat-file", &["cat-file", "--batch-command"], Shape::Branched, CMD_INFO_CONTENTS, out);
    si("cat-file", &["cat-file", "--batch-command"], Shape::Branched, CMD_MISSING, out);
    si("cat-file", &["cat-file", "--batch-command", "--buffer"], Shape::Branched, CMD_FLUSH, out);
    // `flush` is only legal under `--buffer`; without it stock dies mid-stream
    // *after* answering the records before it, so stdout and the exit code
    // disagree with each other and both are pinned.
    si_strict("cat-file", &["cat-file", "--batch-command"], Shape::Branched, CMD_FLUSH, out);
    si_strict("cat-file", &["cat-file", "--batch-command"], Shape::Branched, CMD_UNKNOWN, out);
    si_strict("cat-file", &["cat-file", "--batch-command"], Shape::Branched, CMD_NO_ARG, out);

    // `--batch-all-objects` supplies its own object list, so these need no
    // stdin. The default order is by oid; `--unordered` is pack order, which on
    // a loose-only shape is the same list and on `Packed` is not.
    c(
        "cat-file",
        &["cat-file", "--batch-all-objects", "--batch-check", "--unordered", "--buffer"],
        Shape::Linear,
        out,
    );
    // `-e` needs an operand and `--batch-all-objects` supplies none: exit 129.
    s("cat-file", &["cat-file", "--batch-all-objects", "-e"], Shape::Linear, out);
}

/// Custom `--batch=<format>` / `--batch-check=<format>` atoms.
///
/// `%(objectsize:disk)` and `%(deltabase)` are the two atoms that report on
/// *storage* rather than on content, and they are why this group runs on
/// [`Shape::Packed`]: a port with no delta reuse answers the all-zero oid for
/// every `%(deltabase)` and a plausible-but-wrong number for every disk size,
/// and nothing else in the corpus sees that.
fn cat_file_formats(out: &mut Vec<Case>) {
    si(
        "cat-file",
        &["cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize) %(objectsize:disk) %(deltabase) %(rest)"],
        Shape::Branched,
        REV_WITH_REST,
        out,
    );
    // `%(rest)` is populated only when the input line carried trailing words;
    // the second record has none and must render empty, not as the literal atom.
    si("cat-file", &["cat-file", "--batch-check=[%(rest)]"], Shape::Branched, REV_WITH_REST, out);
    // An empty format still emits the record terminator and the contents.
    si("cat-file", &["cat-file", "--batch="], Shape::Branched, TWO_REVS_LF, out);

    // Over the whole store, on the shape whose objects are deltas — and, for
    // contrast, on one whose objects are all loose, where every disk size is a
    // loose file's size and every delta base is zero.
    c(
        "cat-file",
        &["cat-file", "--batch-all-objects", "--batch-check=%(objecttype) %(objectsize) %(objectsize:disk) %(deltabase)"],
        Shape::Packed,
        out,
    );
    c(
        "cat-file",
        &["cat-file", "--batch-all-objects", "--batch-check=%(objecttype) %(objectsize:disk) %(deltabase)"],
        Shape::Linear,
        out,
    );

    // Atoms `cat-file` does not have. `%(objectname:short)` is the interesting
    // one: `for-each-ref` has that modifier and `cat-file` does not, so a port
    // sharing one atom table between them accepts a format git rejects.
    si_strict("cat-file", &["cat-file", "--batch-check=%(bogus)"], Shape::Branched, HEAD_REV, out);
    si_strict(
        "cat-file",
        &["cat-file", "--batch-check=%(objectname:short)"],
        Shape::Branched,
        HEAD_REV,
        out,
    );
}

/// `--textconv` and `--filters`: the object as a *working tree* would see it.
///
/// [`Shape::Attributes`] is the only shape whose `.gitattributes` matches
/// tracked paths, so it is the only place these two flags do anything. The pair
/// is deliberately asked about one blob down three different paths — a Markdown
/// path, a `text eol=lf` path, and a `binary` path — because the answer is a
/// function of the *path*, not of the object, and a port that keys the filter
/// chain off the blob agrees on all three and is wrong.
fn cat_file_filters(out: &mut Vec<Case>) {
    for args in [
        &["cat-file", "--textconv", "HEAD:docs/manual.md"][..],
        &["cat-file", "--filters", "HEAD:docs/manual.md"][..],
        &["cat-file", "--filters", "HEAD:src/tabs.rs"][..],
        // `--path=` supplies the path separately, so one blob can be asked for
        // down a path it does not live at.
        &["cat-file", "--filters", "--path=assets/logo.bin", "HEAD:README.md"][..],
    ] {
        c("cat-file", args, Shape::Attributes, out);
    }

    // Batch input for these two flags is `<object> <path>`. A line carrying only
    // the object dies `fatal: missing path for '<oid>'` *after* that record's
    // header has already been printed.
    si("cat-file", &["cat-file", "--batch", "--textconv"], Shape::Attributes, OID_AND_PATH, out);
    si(
        "cat-file",
        &["cat-file", "--batch", "--filters"],
        Shape::Attributes,
        OID_AND_BINARY_PATH,
        out,
    );
    si_strict(
        "cat-file",
        &["cat-file", "--batch", "--textconv"],
        Shape::Attributes,
        BLOB_NO_PATH,
        out,
    );

    // A tree-ish where a blob is required.
    s("cat-file", &["cat-file", "--filters", "HEAD"], Shape::Attributes, out);
}

// ---------------------------------------------------------------------------
// write-tree
// ---------------------------------------------------------------------------

/// `write-tree`: the index serialized to trees.
///
/// `corpus/plumbing_objects.rs` sweeps this over the five read shapes plus
/// `Conflicted`, `AwkwardPaths` and `Submodule`. What it never asks is the index
/// layouts where the tree-building loop actually branches:
///
///   * **`NoIndexTrees`** — no cache-tree extension, so every subtree has to be
///     built rather than reused. A port that only ever reuses a valid cache tree
///     produces nothing here.
///   * **`Sparse`** — entries carry the skip-worktree bit and are still part of
///     the tree. A port that writes what is on disk loses `outside/` entirely.
///   * **`Packed`** — the index names blobs that live in a pack, not loose.
///   * **`Whitespace`/`Renamed`** — nested directories whose subtree boundaries
///     and sort order differ from `Linear`'s two entries.
///
/// The printed tree id is the whole assertion: it hashes the exact bytes of the
/// serialized entries, so an off-by-one in a mode string or a subtree that sorts
/// one place wrong changes it.
fn write_tree(out: &mut Vec<Case>) {
    for (shape, args) in [
        (Shape::NoIndexTrees, &["write-tree"][..]),
        (Shape::NoIndexTrees, &["write-tree", "--prefix=ni"][..]),
        (Shape::Sparse, &["write-tree"][..]),
        (Shape::Sparse, &["write-tree", "--prefix=outside"][..]),
        (Shape::Packed, &["write-tree"][..]),
        (Shape::Packed, &["write-tree", "--prefix=packs"][..]),
        (Shape::Renamed, &["write-tree", "--prefix=moved"][..]),
    ] {
        c("write-tree", args, shape, out);
    }

    // An unmerged index is refused whole, `--prefix` or not: `write-tree` walks
    // every entry before it builds anything, so restricting the output subtree
    // does not restrict the check. A port that scopes the unmerged scan to the
    // prefix succeeds here where git fails.
    s("write-tree", &["write-tree", "--prefix=src"], Shape::Conflicted, out);

    // `GIT_INDEX_FILE` naming a file that is not there is not an error: git
    // starts from an empty index, so `write-tree` prints the empty tree — *and
    // writes it*, which the all-objects probe sees as one new object in a
    // repository that never had it. A port that fails, or that prints the empty
    // tree without storing it, diverges on exactly one of the two.
    out.push(Case::new("write-tree", &["write-tree"], Shape::Linear).with_env(NO_INDEX));
    out.push(
        Case::new("write-tree", &["write-tree", "--prefix=src"], Shape::Linear).with_env(NO_INDEX),
    );

    // `GIT_OBJECT_DIRECTORY` pointing at nothing does not merely misplace new
    // objects: `setup.c:is_git_directory()` consults it in place of
    // `<gitdir>/objects` when deciding whether `.git` is a repository at all, so
    // discovery fails first and the message is `not a git repository`, not
    // `unable to write object`. Two commands, so the diagnostic is pinned as a
    // property of discovery rather than of `write-tree`.
    out.push(Case::strict("write-tree", &["write-tree"], Shape::Linear).with_env(NO_OBJECT_DIR));
    out.push(
        Case::strict("cat-file", &["cat-file", "-t", "HEAD"], Shape::Linear)
            .with_env(NO_OBJECT_DIR),
    );

    // Run from a subdirectory: `--prefix` names a path in the *index*, never one
    // relative to the working directory.
    out.push(Case::new("write-tree", &["write-tree", "--prefix=src"], Shape::Linear).in_dir("src"));
}

// ---------------------------------------------------------------------------
// pack-objects
// ---------------------------------------------------------------------------

/// `pack-objects` on the one shape that has deltas to find.
///
/// Pack **bytes** are compared, through stdout for `--stdout` and through the
/// printed name for a named output — a pack file's name is the SHA-1 of its own
/// contents, so two sides printing the same line produced the same bytes.
/// `corpus/maintenance.rs` relaxes the *storage* probe for `repack` precisely
/// because the vendored gitoxide packs differently; nothing relaxes stdout, and
/// that difference is left visible here on purpose.
///
/// Measured on stock 2.55.0 over [`Shape::Packed`], every invocation below is
/// byte-stable across runs and unchanged by `-c pack.threads=1` — so a
/// divergence here is a difference in object selection, delta choice or
/// compression, never in scheduling.
fn pack_objects(out: &mut Vec<Case>) {
    // Object *selection*: which flags change the object count in the header.
    // `--filter=blob:none` drops it from 31 to 20; `--unpacked` and
    // `--incremental` drop it to 0 on a fully packed store.
    for args in [
        &["pack-objects", "--all", "--revs", "--stdout", "-q"][..],
        &["pack-objects", "--all", "--revs", "--stdout", "-q", "--filter=blob:none"][..],
        &["pack-objects", "--revs", "--unpacked", "--stdout", "-q"][..],
    ] {
        c("pack-objects", args, Shape::Packed, out);
    }

    // Delta *encoding*: same objects, different storage. Each of these produces
    // a pack distinct from the baseline above, so a port that ignores any one of
    // them is caught by that one alone.
    for args in [
        &["pack-objects", "--all", "--revs", "--stdout", "-q", "--window=0"][..],
        &["pack-objects", "--all", "--revs", "--stdout", "-q", "--no-reuse-delta"][..],
        &["pack-objects", "--all", "--revs", "--stdout", "-q", "--compression=0"][..],
        &["pack-objects", "--all", "--revs", "--stdout", "-q", "--delta-base-offset"][..],
    ] {
        c("pack-objects", args, Shape::Packed, out);
    }

    // The same questions asked through config. The first four move the bytes on
    // `Shape::Packed`; `pack.threads=1` is here to pin that it does not, so the
    // corpus records the invariance rather than assuming it.
    for kv in [
        ("pack.window", "0"),
        ("pack.depth", "1"),
        ("core.compression", "1"),
        ("core.bigFileThreshold", "1"),
        ("pack.threads", "1"),
    ] {
        out.push(
            Case::new(
                "pack-objects",
                &["pack-objects", "--all", "--revs", "--stdout", "-q"],
                Shape::Packed,
            )
            .with_config(&[kv]),
        );
    }

    // A named output writes `<prefix>-<sha>.pack` beside its index and prints
    // the `<sha>`; both land where `status --porcelain -uall` in the state probe
    // sees them.
    c("pack-objects", &["pack-objects", "--all", "--revs", "-q", "packs/parity"], Shape::Packed, out);
    c(
        "pack-objects",
        &["pack-objects", "--all", "--revs", "-q", "--index-version=1", "packs/parity"],
        Shape::Packed,
        out,
    );

    // Fed from stdin. Without `--revs`, `pack-objects` reads *object names*, not
    // revisions — `fatal: expected object ID, got garbage: HEAD`. That is what
    // this group pins: one payload, accepted under `--revs` and refused without
    // it.
    si("pack-objects", &["pack-objects", "--revs", "--stdout", "-q"], Shape::Packed, HEAD_REV, out);
    si_strict("pack-objects", &["pack-objects", "--stdout", "-q"], Shape::Packed, HEAD_REV, out);
    // The empty tree is a name every repository resolves, so it is the one
    // object list expressible as a literal; the empty blob is in no fixture, so
    // naming it is a hard failure rather than an empty pack.
    si("pack-objects", &["pack-objects", "--stdout", "-q"], Shape::Packed, EMPTY_TREE_OID, out);
    si_strict(
        "pack-objects",
        &["pack-objects", "--stdout", "-q"],
        Shape::Packed,
        EMPTY_BLOB_OID,
        out,
    );

    // `--max-pack-size` is refused for a pack going to a stream: the reader has
    // no way to pick up a second file.
    s(
        "pack-objects",
        &["pack-objects", "--all", "--revs", "--stdout", "--max-pack-size=1k"],
        Shape::Packed,
        out,
    );
}

// ---------------------------------------------------------------------------
// index-pack
// ---------------------------------------------------------------------------

/// `index-pack`: a pack in, an index out — and, under `--stdin`, objects in the
/// store afterwards.
///
/// Two input channels, both reachable. On disk it reads [`Shape::Packed`]'s
/// `packs/unindexed.pack`, a 26-object pack carrying seven deltas, so the
/// delta-resolution pass runs for real rather than over a two-blob toy. On stdin
/// it reads [`ONE_OBJECT_PACK`] and its two damaged variants, which is the only
/// way to reach the streaming reader and the only way to ask what the store
/// looks like after a *failed* read: stock writes objects as it inflates them
/// and has no rollback, so a port that buffers the whole pack and commits on
/// success agrees on the exit code and the message and diverges on the
/// all-objects probe.
fn index_pack(out: &mut Vec<Case>) {
    for args in [
        // `--verify` against the index already beside the pack.
        &["index-pack", "--verify", "packs/sample.pack"][..],
        // Building an index for a pack that has none.
        &["index-pack", "-o", "packs/built.idx", "--keep", "packs/unindexed.pack"][..],
        // The `.rev` sidecar is one of the four extensions `probe_storage`
        // counts, so "wrote the index, skipped the reverse index" is visible.
        &["index-pack", "-o", "packs/built.idx", "--rev-index", "packs/unindexed.pack"][..],
        // Connectivity, not just parseability.
    ] {
        c("index-pack", args, Shape::Packed, out);
    }

    // Refusals against a pack on disk, each naming a different check: the file
    // suffix, the pack signature, a missing index, a size cap, a flag that is
    // only meaningful with `--stdin`, and a `--strict` argument that is not a
    // `<msg-id>=<severity>` pair.
    for args in [
        &["index-pack", "--verify", "packs/sample.idx"][..],
        &["index-pack", "--verify", "packs/unindexed.pack"][..],
        &["index-pack", "-o", "packs/built.idx", "--max-input-size=10", "packs/unindexed.pack"][..],
        &["index-pack", "--fix-thin", "packs/unindexed.pack"][..],
    ] {
        s("index-pack", args, Shape::Packed, out);
    }
    // `--object-format` disagreeing with the repository walks the whole pack
    // reporting per-index size errors before dying. Bounded (40 stderr lines on
    // stock 2.55.0) but prose, so the exit code and the state are what is pinned.
    c(
        "index-pack",
        &["index-pack", "-o", "packs/built.idx", "--object-format=sha256", "packs/unindexed.pack"],
        Shape::Packed,
        out,
    );

    // Fed a pack on stdin. The success cases leave two blobs no fixture has, so
    // the all-objects probe is the assertion, not the one-line stdout.
    si("index-pack", &["index-pack", "--stdin"], Shape::Linear, ONE_OBJECT_PACK, out);
    si("index-pack", &["index-pack", "--stdin", "--fix-thin"], Shape::Linear, ONE_OBJECT_PACK, out);
    // The same pack into a repository that already has packs.
    si("index-pack", &["index-pack", "--stdin"], Shape::Packed, ONE_OBJECT_PACK, out);

    // Damaged streams. `early EOF` and `SHA1 mismatch` are different failures
    // and must stay so: the first never reached the trailer, the second read a
    // complete pack whose checksum did not match.
    si_strict("index-pack", &["index-pack", "--stdin"], Shape::Linear, TRUNCATED_PACK, out);
    si_strict("index-pack", &["index-pack", "--stdin"], Shape::Linear, BAD_CHECKSUM_PACK, out);
    si_strict("index-pack", &["index-pack", "--stdin"], Shape::Linear, NOT_BINARY, out);
    si_strict(
        "index-pack",
        &["index-pack", "--stdin", "--max-input-size=10"],
        Shape::Linear,
        ONE_OBJECT_PACK,
        out,
    );
    // `--object-format` and `--stdin` are mutually exclusive: the stream carries
    // no way to disagree about the algorithm.
    si_strict(
        "index-pack",
        &["index-pack", "--stdin", "--object-format=sha256"],
        Shape::Linear,
        ONE_OBJECT_PACK,
        out,
    );

    // `unpack-objects` reads these same three payloads in
    // `corpus/stdin_plumbing.rs`. The pair matters: the two commands consume
    // identical bytes and must disagree about nothing except where the objects
    // end up — loose for one, packed for the other.
}

// ---------------------------------------------------------------------------
// verify-pack and show-index
// ---------------------------------------------------------------------------

/// `verify-pack` and `show-index`: reading a pack index back.
///
/// `verify-pack -v` is the deepest single-argv assertion about pack storage
/// there is — per object it prints the name, type, inflated size, packed size,
/// offset, delta depth and delta base. A port that stores the same objects with
/// a different delta chain matches every other case in this module and fails
/// this one.
///
/// `show-index` reads only from stdin, so [`ONE_OBJECT_IDX`] is what makes its
/// success path reachable at all; before it, all six `show-index` cases in the
/// corpus were header errors. The two damaged-but-accepted variants are
/// deliberate: stock does *not* check the index trailer, and a port that does is
/// wrong in the strictest possible direction.
fn verify_and_show_index(out: &mut Vec<Case>) {
    for args in [
        &["verify-pack", "-v", "packs/sample.pack"][..],
        &["verify-pack", "-v", "--stat-only", "packs/sample.idx"][..],
        ] {
        c("verify-pack", args, Shape::Packed, out);
    }
    for args in [
        // A pack whose index was never built. Note the diagnostic names
        // `packs/unindexed.idx`, a path the caller never typed.
        &["verify-pack", "packs/unindexed.pack"][..],
        // A file that is not a pack at all.
        &["verify-pack", "README.md"][..],
        // sha256 over a sha1 index: the size check fails before the read does.
        &["verify-pack", "--object-format=sha256", "packs/sample.idx"][..],
        &["verify-pack", "--object-format=bogus", "packs/sample.idx"][..],
    ] {
        s("verify-pack", args, Shape::Packed, out);
    }

    // show-index: the real index.
    si("show-index", &["show-index"], Shape::Linear, ONE_OBJECT_IDX, out);
    // Stock reads the tables it needs and never hashes them, so both of these
    // *succeed* and print the same two entries as the intact index above.
    si("show-index", &["show-index"], Shape::Linear, BAD_SUM_IDX, out);
    si("show-index", &["show-index"], Shape::Linear, NO_TRAILER_IDX, out);
    // Damage the header, or cut the fanout short, and it does refuse.
    si_strict("show-index", &["show-index"], Shape::Linear, BAD_MAGIC_IDX, out);
    si_strict("show-index", &["show-index"], Shape::Linear, SHORT_IDX, out);
    si_strict(
        "show-index",
        &["show-index", "--object-format=sha256"],
        Shape::Linear,
        ONE_OBJECT_IDX,
        out,
    );
    // An index on stdin is the only input `show-index` has: a path operand is
    // accepted by the parser, ignored, and stdin is read anyway.
    si("show-index", &["show-index", "packs/sample.idx"], Shape::Packed, ONE_OBJECT_IDX, out);
}

// ---------------------------------------------------------------------------
// prune-packed, count-objects, pack-redundant, unpack-file
// ---------------------------------------------------------------------------

/// What the store *reports*, and what removing a loose duplicate does to it.
///
/// [`Shape::Packed`] is built so these have real work: a second `repack` without
/// `-d` leaves five loose objects that are also in a pack, and a reset plus a
/// reflog expiry leaves one object no ref reaches. So `count-objects -v` there
/// reports `prune-packable: 5` and `prune-packed` has five files to delete,
/// where on every other shape both answer zero. `probe_state`'s all-objects
/// listing is what proves the deletion took only the duplicates: one object too
/// many and a line goes missing.
///
/// `prune-packed -n` prints its `rm -f` list relative to the **current
/// directory**, not to the repository root, which is why two of these run from
/// elsewhere.
fn loose_and_packed_accounting(out: &mut Vec<Case>) {
    for args in [
        // `-q` suppresses progress, not the `-n` listing.
        &["prune-packed", "-n", "-q"][..],
        &["prune-packed", "--quiet"][..],
    ] {
        c("prune-packed", args, Shape::Packed, out);
    }
    out.push(Case::new("prune-packed", &["prune-packed", "-n"], Shape::Packed).in_dir("packs"));

    // `count-objects` is pure reporting and byte-comparable everywhere. These
    // are the stores the maintenance sweep never asked about: one with packs
    // *and* loose duplicates, one with nested attribute files, one sparse, and
    // one with no cache tree.
    for (shape, args) in [
        (Shape::Attributes, &["count-objects", "-v"][..]),
        (Shape::NoIndexTrees, &["count-objects", "-v"][..]),
    ] {
        c("count-objects", args, shape, out);
    }
    // Counting is a property of the object directory, not of the working
    // directory: from inside `.git` the numbers must not move.
    out.push(Case::new("count-objects", &["count-objects", "-v"], Shape::Packed).in_dir(".git"));

    // `pack-redundant` needs two packs before it has anything to compare, and
    // `Shape::Packed` is the only shape that has them. Everywhere else it dies
    // `Zero packs found!`, which is all `corpus/maintenance.rs` has been able to
    // measure.
    c("pack-redundant", &["pack-redundant", "--i-still-use-this", "--all"], Shape::Packed, out);
    c(
        "pack-redundant",
        &["pack-redundant", "--i-still-use-this", "--all", "--verbose"],
        Shape::Packed,
        out,
    );
    // Without `--all` there is no pack list at all, even in a repository full of
    // packs: exit 128.
    s("pack-redundant", &["pack-redundant", "--i-still-use-this"], Shape::Packed, out);

    // `unpack-file`'s refusals. Its success path prints a randomly named
    // temporary file and cannot be compared; the refusals are exact, and
    // together they pin the one rule the command has — the argument must name a
    // blob, and everything else is `unable to read blob object <oid>` carrying
    // the *resolved* oid, not the spelling the caller typed.
    for args in [
        &["unpack-file", "HEAD"][..],
        &["unpack-file", "HEAD^{tree}"][..],
        &["unpack-file", "v0.2.0"][..],
        // The empty tree exists and is the wrong type; the message is the same
        // `unable to read blob object` a missing object gets.
        &["unpack-file", "4b825dc642cb6eb9a060e54bf8d69288fbee4904"][..],
        // A path that is not in the tree fails to *resolve*, which is a
        // different message from failing to read.
        &["unpack-file", "HEAD:no/such/path"][..],
    ] {
        s("unpack-file", args, Shape::Branched, out);
    }
    // More than one operand is a usage error, exit 129.
    c("unpack-file", &["unpack-file", "HEAD:README.md", "extra"], Shape::Branched, out);
}

// ---------------------------------------------------------------------------
// commit-graph and multi-pack-index
// ---------------------------------------------------------------------------

/// The two derived indexes, on the shape that has packs for them to describe.
///
/// `corpus/maintenance.rs` runs `commit-graph write` across the read shapes and
/// `multi-pack-index` almost entirely on its error paths, because no shape it
/// used had a pack. Over [`Shape::Packed`] the midx verbs reach their real code:
/// `write` has two packs to index, `expire` has packs to consider dropping, and
/// `repack` has a batch to assemble.
///
/// Neither artifact is visible to `probe_storage` — it counts by file extension
/// and the midx file has none. What these cases pin is the exit code, the
/// loose/pack counts around the operation, and the all-objects listing: an
/// `expire` or a `repack` that dropped an object is caught even though the midx
/// itself is never compared.
fn graph_and_midx(out: &mut Vec<Case>) {
    for args in [
        &["commit-graph", "write", "--reachable"][..],
        &["commit-graph", "write", "--reachable", "--changed-paths"][..],
        &["commit-graph", "write", "--reachable", "--changed-paths", "--split"][..],
        &["commit-graph", "verify"][..],
    ] {
        c("commit-graph", args, Shape::Packed, out);
    }
    // Two config keys that look interchangeable and are not: only
    // `core.commitGraph` suppresses an explicit `write`, while
    // `gc.writeCommitGraph` gates the *implicit* write inside `gc` and must
    // leave this one alone. A port that treats them as one switch diverges.
    for kv in [
        ("core.commitGraph", "false"),
        ("gc.writeCommitGraph", "false"),
    ] {
        out.push(
            Case::new("commit-graph", &["commit-graph", "write", "--reachable"], Shape::Packed)
                .with_config(&[kv]),
        );
    }
    // `--stdin-commits` reads *object names*, not revisions. The empty tree is
    // accepted because it resolves; `HEAD` is rejected before any lookup happens
    // (`unexpected non-hex object ID`); the all-zero oid gets past the hex check
    // and fails the lookup (`invalid object`). Three failure points on one flag.
    si(
        "commit-graph",
        &["commit-graph", "write", "--stdin-commits"],
        Shape::Packed,
        EMPTY_TREE_OID,
        out,
    );
    si_strict(
        "commit-graph",
        &["commit-graph", "write", "--stdin-commits"],
        Shape::Packed,
        HEAD_REV,
        out,
    );
    si_strict(
        "commit-graph",
        &["commit-graph", "write", "--stdin-commits"],
        Shape::Packed,
        ZERO_OID,
        out,
    );
    // `--stdin-packs` reads pack *file names* relative to the object directory,
    // and no such name is expressible as a literal (a pack's name embeds its own
    // checksum), so the reachable half is the refusal.
    si_strict(
        "commit-graph",
        &["commit-graph", "write", "--stdin-packs"],
        Shape::Packed,
        NOPE_PACK,
        out,
    );

    for args in [
        &["multi-pack-index", "write", "--bitmap"][..],
        &["multi-pack-index", "repack"][..],
        // A preferred pack is named by its path *inside the object directory*,
        // so a worktree path is not one — stock warns and carries on rather than
        // failing, which is the half a port is most likely to get wrong.
        &["multi-pack-index", "write", "--preferred-pack=packs/sample.pack"][..],
    ] {
        c("multi-pack-index", args, Shape::Packed, out);
    }
    out.push(
        Case::new("multi-pack-index", &["multi-pack-index", "write"], Shape::Packed)
            .with_config(&[("core.multiPackIndex", "false")]),
    );
    // `--stdin-packs` with no names: `error: no pack files to index.`, exit 255.
    si_strict(
        "multi-pack-index",
        &["multi-pack-index", "write", "--stdin-packs"],
        Shape::Packed,
        b"",
        out,
    );
}
