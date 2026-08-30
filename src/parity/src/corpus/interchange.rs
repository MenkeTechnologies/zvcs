//! The interchange formats: the serialisations git can write **and read back**
//! — `bundle`, `fast-export`, `fast-import`.
//!
//! Everywhere else in this corpus a serializer is measured by its bytes. That
//! is half a format: a writer and a reader can be self-consistently wrong
//! together, and a repository whose bundle only *this* implementation can open
//! is worse than one that prints the wrong thing, because nothing reports it.
//! This module is the other half — **stock git's own bytes, frozen into the
//! corpus and handed to the port to read**.
//!
//! A `Case` is one argv against a pristine copy, so it cannot create the file a
//! later argv would open. [`Case::stdin`] is the way through: every reader git
//! has takes `-` for standard input, and a payload is a `&'static [u8]`
//! compiled into the binary. So a bundle stock wrote in 2026 is replayed into
//! the port's `bundle unbundle` byte for byte on every run, forever, with no
//! file to create and no clock to depend on.
//!
//! # How this divides territory with the six adjacent modules
//!
//! * **`fetch_clone.rs`** owns `bundle create` **to stdout** on
//!   `Shape::Packed` — the delta-bearing pack, `--version=2`/`3`,
//!   `-q`/`--progress`, and the `--branches`/`--tags`/`--remotes` selectors —
//!   plus the three read subcommands pointed at `packs/sample.pack`, a real
//!   packfile that is not a bundle. Every one of its read cases is a
//!   *rejection*: nothing there ever hands git a bundle it will accept.
//! * **`transport_local.rs`** owns `bundle create` to a **file** across
//!   `Linear`/`Branched`/`AwkwardPaths`, and the read side against
//!   `README.md`, a missing `out.bundle`, and `-` with **stdin closed** — an
//!   empty stream. Its header states the constraint this module removes: "the
//!   read subcommands can only be reached on their error paths, because nothing
//!   in a one-argv case can produce a bundle for a later argv to read".
//! * **`sequences.rs`** owns the multi-step round trip — create, verify,
//!   list-heads, `clone --no-local` from the bundle, then an incremental
//!   bundle verified in the clone. That is the only place `clone`/`fetch` reach
//!   the bundle **transport**; see the unmeasurable list below for why a single
//!   case cannot.
//! * **`exit_codes.rs`** owns `bundle verify nosuchfile.bundle`;
//!   **`fixture_gaps2.rs`** and **`shape_reach.rs`** own `bundle create` on the
//!   shapes their sweeps reach. All create-side, all to a file.
//! * **`archive_export.rs`** owns `fast-export`'s flag surface as *stdout*
//!   (`--full-tree`, `--use-done-feature`, `--show-original-ids`,
//!   `--mark-tags`, `--anonymize`, `--reencode=yes|no`,
//!   `--signed-tags=strip|verbatim`, `--tag-of-filtered-object=drop`,
//!   `--export-marks=`, `--refspec=`, `-M`/`-C`) and `fast-import` fed
//!   **hand-written** streams — one per command in the stream language, plus
//!   six malformed ones. Its header names the gap this module fills: no fixture
//!   carries a marks file, and nothing there ever feeds `fast-import` a stream
//!   that `fast-export` actually produced.
//! * **`object_pack.rs`** owns pack and index formats. A bundle *contains* a
//!   pack; what is measured here is the bundle header, the capability lines,
//!   the prerequisite list and the reader's decisions — never the pack encoder,
//!   which is that module's.
//! * **`graft_partial.rs`** and **`misc_commands.rs`** own `fast-export` on a
//!   repository with objects missing, and its `--` terminator.
//!
//! What is new here, in one line each: **bundles on stdin** (nobody had ever
//! handed git a bundle it would accept), **`bundle create --stdin`** (the
//! rev-list-from-stdin path), **`--filter=` in a bundle** (the v3 `@filter`
//! capability, both directions), and **fast-export streams replayed through
//! fast-import** (the cross round trip).
//!
//! # The payloads, and how to regenerate them
//!
//! Every binary literal below was produced by stock git 2.55.0 under
//! [`crate::env::harden`]'s environment, and each was produced **twice, in two
//! separate copies of its source repository, and compared** — a payload that
//! stock cannot reproduce would make every case built on it a false failure
//! rather than a finding.
//!
//! The bundles come from a two-commit donor repository built with the harness's
//! pinned identity and date:
//!
//! ```text
//! git init -q --initial-branch=donor .
//! printf 'donor payload\n' > donor.txt && git add donor.txt && git commit -m 'donor root'
//! printf 'donor second\n' > donor.txt && git commit -am 'donor tip'
//! git tag -a -m 'donor tag' dtag
//! ```
//!
//! giving `e35acb22fd8bc8e31fb1129f2d4c2d5f66f0dfb4` (root),
//! `1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321` (tip) and
//! `f630167b77c69f67b5c50b8e252e483626504376` (the tag object). Those ids are
//! in **no fixture**, which is the point: the donor is a foreign repository, so
//! `unbundle` has real objects to write and the prerequisite bundle names a
//! commit the receiving repository genuinely lacks.
//!
//! | const | produced by | bytes | sha1 |
//! |---|---|---|---|
//! | [`BUNDLE_V2`] | `bundle create --version=2 - --all` | 695 | `ac2088858fa378a4d89be0217721accdfa8bf792` |
//! | [`BUNDLE_V3`] | `bundle create --version=3 - --all` | 715 | `7d34a30e0142ccd6eaf957da782e821a94171da7` |
//! | [`BUNDLE_V3_FILTER`] | `bundle create --version=3 - --all --filter=blob:none` | 688 | `792b3097c7befdd9572d2828c90f1271b42fe80b` |
//! | [`BUNDLE_PREREQ`] | `bundle create - donor~1..donor` | 380 | `1782fb568e624d013da9195358558610409720c0` |
//! | [`FE_ALL`] | `fast-export --all` on `Branched` | 977 | `149ddf68b12e99be7c0f1a97e85d6aed248f9346` |
//! | [`FE_DONE`] | `fast-export --use-done-feature --all` on `Branched` | 995 | `0abb9388f7c56a55c9ca26ea0f26b85c0c8d6207` |
//! | [`FE_ORIGINAL_IDS`] | `fast-export --show-original-ids --mark-tags --all` on `Branched` | 1417 | `aa5586fd58864b15a05378b339526891e42195d0` |
//! | [`FE_FULL_TREE`] | `fast-export --full-tree --all` on `Branched` | 1074 | `e8ea70faecc7ab2d71d46ad0d206d9fe5871a9db` |
//! | [`FE_RENAMES_NO_DATA`] | `fast-export -M -C --no-data --all` on `Renamed` | 1854 | `6f547e20ee4cc72d0abdb663839c6075accd751c` |
//!
//! # What the round trip actually proves
//!
//! [`FE_ALL`] is stock git's serialization of `Shape::Branched`. Replayed into
//! `Shape::Linear` — whose only commit is `Branched`'s own root — `fast-import`
//! has to rebuild four commits, a tag object and five blobs from the stream
//! alone, and because every identity, date and message in the stream is
//! `harden`'s pinned value the reconstruction is **oid-for-oid** the original:
//!
//! ```text
//! refs/heads/feature commit 07e86d1fedb713fbc84a754c98ea4bfe53316416
//! refs/heads/main    commit 5915d79de18d919476d339c8b8efda1d9bb166e2
//! refs/tags/v0.1.0   commit 5915d79de18d919476d339c8b8efda1d9bb166e2
//! refs/tags/v0.2.0   tag    d7277ea97518c8631ff11851f616d1ca422aeef0
//! ```
//!
//! measured on both sides. So the state digest is not merely "the same on both"
//! — it is the same as the shape the stream was taken from, and an importer that
//! builds the right *content* into the wrong tree shape, or attaches the wrong
//! parent, moves every id at once and is caught even though the command printed
//! nothing.
//!
//! [`FE_RENAMES_NO_DATA`] is the same idea with the blob bodies removed: every
//! `M` names a 40-hex id instead of a mark, and the `R`/`C` records carry the
//! rename and copy decisions. It is imported into `Shape::Renamed` — the shape
//! it came from — because a stream that references blobs by id can only be read
//! where those blobs already are.
//!
//! # What is NOT measurable here, and why
//!
//! Recorded rather than shipped as a flaky case:
//!
//! * **`clone`/`fetch`/`ls-remote` from a bundle *file*.** The bundle
//!   **transport** (`transport.c`'s bundle helper) is a different code path
//!   from `bundle unbundle`, and it needs a path on disk. No fixture carries a
//!   `.bundle`, `fixture.rs` is not this module's to change, and a case cannot
//!   write the file before the argv that reads it. `sequences.rs` reaches it as
//!   a multi-step workflow; a single case cannot, and none is faked here.
//! * **`fast-import --stats` output.** The statistics block goes to stderr and
//!   ends in `pack_report: getpagesize() = 16384` plus `core.packedGitLimit =
//!   35184372088832` — machine facts, not repository facts. The flag is still
//!   measured, on its exit code and on the objects it wrote; its stderr is not,
//!   and no case here is [`Case::strict`] on it.
//! * **`fast-import` crash reports.** Every malformed stream makes stock write
//!   `.git/fast_import_crash_<pid>` and name it on stderr;
//!   `archive_export.rs`'s header measured six different names in six runs. So
//!   no failing `fast-import` case is strict, here or there.
//! * **The file a bundle reader names in its diagnostics.** Reading from
//!   standard input, stock says `<stdin>` and the port says `-`
//!   (`error: '<stdin>' does not look like a v2 or v3 bundle file` versus
//!   `error: '-' …`, and `<stdin> is okay` versus `- is okay`). Both are on
//!   stderr, which this harness does not compare by default, and no case below
//!   opts in where the file name appears in the message. The exit code is
//!   compared and agrees. The prerequisite refusal is the one bundle diagnostic
//!   that names no file — it is byte-identical on both sides and *is* strict.
//! * **`--since`/`--until` in a bundle's rev-list arguments.** Approxidate is
//!   resolved against the wall clock; a bundle whose contents depend on when it
//!   was built cannot be a differential case.
//!
//! # The seven defects these cases found
//!
//! Each was reproduced by hand, stock and port side by side, in a copy of the
//! fixture, before it was written down. Three of the seven are only reachable
//! through the round trip and could not have been found by comparing either
//! side's stdout.
//!
//! 1. **`--filter=` in a bundle is unimplemented in both directions.**
//!    `bundle create --version=3 - --all --filter=blob:none` is exit 0 and a
//!    `@filter=blob:none` capability line on stock; on the port it is exit 128,
//!    `fatal: ambiguous argument '--filter=blob:none': unknown revision or path
//!    not in the working tree`. And handed stock's own filtered bundle
//!    ([`BUNDLE_V3_FILTER`]), all three read subcommands die
//!    `fatal: malformed bundle header in "-": capability "filter=blob:none" is
//!    not supported` at exit 128 where stock reads it at exit 0.
//! 2. **An unknown capability is exit 128 rather than 1.** Stock reports
//!    `error: unknown capability 'nosuchcapability=1'` and exits **1**; the port
//!    exits **128**. Same rejection, different contract for a caller.
//! 3. **`fast-export --anonymize-map=` is unimplemented.** Exit 1 and
//!    `zvcs: fast-export: --anonymize-map is not supported`, against stock's
//!    exit 0 and a full anonymized stream.
//! 4. **`fast-export --import-marks=` accepts a file that is not a marks
//!    file.** Pointed at `README.md`, stock dies `fatal: corrupt mark line: #
//!    fixture` (128); the port exits **0** and exports the whole history as if
//!    no marks file had been named. `--import-marks-if-exists=README.md` is the
//!    same: stock 128, port 0. An unreadable resume file silently ignored is
//!    worse than one refused, because a resumed export then re-emits history the
//!    consumer already has.
//! 5. **`fast-import --export-pack-edges=` is refused.**
//!    `fatal: unsupported flag "--export-pack-edges=edges.txt" for a stream that
//!    writes objects (this port writes loose objects, so there are no pack edges
//!    to report)`, exit 128, where stock exits 0 and creates the file.
//! 6. **Re-importing a stream into the repository it came from aborts on the
//!    annotated tag.** This is the round trip's own finding, and the most
//!    consequential one here, because replaying a stream a repository already
//!    holds is exactly what a resumed or repeated import does. On
//!    `Shape::Branched`, fed [`FE_ALL`] — that shape's own `fast-export --all`
//!    output — stock exits 0 and leaves every ref where it was, and the port
//!    exits **128**:
//!
//!    ```text
//!    fatal: The reference "refs/tags/v0.2.0" should have content
//!    5915d79de18d919476d339c8b8efda1d9bb166e2, actual content was
//!    d7277ea97518c8631ff11851f616d1ca422aeef0
//!    ```
//!
//!    `5915d79…` is the *commit* the tag points at and `d7277ea…` is the tag
//!    object itself, so the compare-and-swap precondition on the tag's ref
//!    update is built from the peeled value instead of the tag. It fires only
//!    where the tag already exists, which is why no `Shape::Linear` import
//!    reaches it, and it fires again on [`FE_FULL_TREE`] into the same shape.
//! 7. **`fast-import` writes no `HEAD` reflog entry for a no-op ref update.**
//!    Also the round trip's: [`FE_RENAMES_NO_DATA`] imported back into
//!    `Shape::Renamed` rebuilds the identical commits, so the branch ends where
//!    it started. Stock still appends one line to `.git/logs/HEAD` —
//!    `8aeb24d2… 8aeb24d2… zvcs parity <parity@example.invalid> 1700000000 +0000
//!    \tfast-import` — and the port appends nothing. Diffed by hand: the two
//!    `logs/refs/heads/main` files are identical and the two `logs/HEAD` files
//!    differ by exactly that line.
//!
//! One more the round trip **corroborates** rather than finds:
//! `fastimport.unpackLimit=0` is ignored. Measured on [`FE_ALL`] into
//! `Shape::Linear`, stock leaves 5 loose objects and one pack, the port leaves
//! 13 loose objects and no pack. `archive_export.rs` already catches this on its
//! own hand-written stream; the case here shows it survives a stream fourteen
//! objects wide, and it is not counted as a new finding.
//!
//! And one thing that is **not** a defect, checked because this module exists to
//! check it: the port's bundle *writer* is byte-identical to stock's for v2, v3
//! and the prerequisite-bearing form, and stock reads all three back
//! (`git bundle verify -` on the port's own output: `The bundle uses this hash
//! algorithm: sha1`, exit 0). There is no format here that the port writes and
//! stock cannot read.

use crate::fixture::Shape;
use crate::runner::Case;

// ---------------------------------------------------------------------------
// Bundle payloads — stock git's own bytes
// ---------------------------------------------------------------------------

/// The donor's whole history as a v2 bundle: two commits, a tree, two blobs and
/// an annotated tag, behind a three-line ref list that includes `HEAD`.
const BUNDLE_V2: &[u8] = b"# v2 git bundle\n1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 refs/heads/donor\nf630167b77c69f67b5c50b8e252e483626504376 refs/tags/dtag\
    \n1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 HEAD\n\nPACK\x00\x00\x00\x02\x00\x00\x00\x07\x96\
    \x0ex\x9c\x9d\x8dI\n\x021\x10E\xf79E\xf6\x82d\x1e@\xc4\xab$\x95*\x0c\xf4D[6\xea\xe9m\xd1\x13\
    \xf8\x17\xef\xad\x1e\x9fWD\x99S\x08\x88d\x0be\x85.\x86\x10*Y\xf09z@\x07\xd5\xa9\x9a\xb2\x81\
    \xa4\xc4RV\x9cX\xa2\xf5\x05\xaa1\xd4R\x85\x84VS\xd5\xdad2\xcd\x81i\x9eB \xd5\xa8:Q\xee|\x9dW\
    \xf9\xda\xe0&\xf7\xb6\xf3S\x9e\xbe\xbe\xe0\xa3\x8c\xcb\x80\xc7>me\xe8\xed,uT\xbf\xc9\xc3\x87\
    \x02\xe6q\xec\xcc\xf8o/\xda<\xed\xe7\xdc\x17\xf1\x06K\xa1G\xe2\xcd\x08x\x9c-\x8cA\x0e\xc2 \
    \x10E\xf7\x9cb\xf6&\x06PB\x9b\x18\xe3U\x86\x99\xb1\xc1\x94B\x904\xb6\xa7\xafD\xdf\xe2\xbd\
    \xbf\xfa9\xbc\x84\x1a\x18\x16\xba>\xc9\xe9\x81\x07\xb4\x9eF1\xc1\xb2\xd6\xde\xe1\x18\x1c\x129\
    \x09\xfab\x8dj[\x11\xa0\x9cRl\xaa\xe1\x04\xfcU\x1f\x93T\xd8WzC\xc1\x1a\xdb\x06\xb7_\x1f\xf2\
    \xc1Tf9\xc7e\xc59\xf2\x1d\x8c\xd7\x7f\xe0\xd4\xad\x14\xe7%W\xe8?\x07\xa2*,\xc6\xeb\x03\x81\
    \x12x\x9c{\xc6\xb8\x9dQ\xb7\xa4(5U\xc14\xd9\xc48\xc9\xc04\xc5\xd0\xd4\xd2\xcc$\xd525\xcd\xdc\
    \xd4<\xd54\xcd(-\xcd 91\xd5<1\xd5\xd4\xd08\xd9\xd2\xc0tbl+kQ~~\x09\x17\x00\x1f\xa9\x12\x10\
    \xa5\x02x\x9c340031QH\xc9\xcf\xcb/\xd2+\xa9(a\x98\xaf\xc4\xb6\xfdDa\xf4\x89l\xe7\xaa\xb0\xb0\
    \xc8\xc3[\xa7\xd5\x14\x09\x01\x00\xeb\xf5\x0e\x15\xa5\x02x\x9c340031QH\xc9\xcf\xcb/\xd2+\xa9(aX\
    \xe2u\xf2Sg~\xfb\xb3oG|\xc25\xe5\xb8?~\xb0\x98\xb1\x05\x00\x06\x8e\x10\"=x\x9cK\xc9\xcf\xcb/R(NM\
    \xce\xcfK\xe1\x02\x00#\xb1\x04\xc9>x\x9cK\xc9\xcf\xcb/R(H\xac\xcc\xc9OL\xe1\x02\x00)t\x057\
    \x98v7]\xd6\x8b85\x05\xc8\xf9(\xfb\xf06\xd8\xfe<%6";

/// The same objects in the v3 container: `# v3 git bundle` plus the
/// `@object-format=sha1` capability line ahead of the refs. Twenty bytes longer
/// than [`BUNDLE_V2`] and otherwise the same bundle.
const BUNDLE_V3: &[u8] = b"# v3 git bundle\n@object-format=sha1\n1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 refs/heads/donor\
    \nf630167b77c69f67b5c50b8e252e483626504376 refs/tags/dtag\n1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 HEAD\
    \n\nPACK\x00\x00\x00\x02\x00\x00\x00\x07\x96\x0ex\x9c\x9d\x8dI\n\x021\x10E\xf79E\xf6\x82d\
    \x1e@\xc4\xab$\x95*\x0c\xf4D[6\xea\xe9m\xd1\x13\xf8\x17\xef\xad\x1e\x9fWD\x99S\x08\x88d\x0be\
    \x85.\x86\x10*Y\xf09z@\x07\xd5\xa9\x9a\xb2\x81\xa4\xc4RV\x9cX\xa2\xf5\x05\xaa1\xd4R\x85\x84VS\
    \xd5\xdad2\xcd\x81i\x9eB \xd5\xa8:Q\xee|\x9dW\xf9\xda\xe0&\xf7\xb6\xf3S\x9e\xbe\xbe\xe0\xa3\
    \x8c\xcb\x80\xc7>me\xe8\xed,uT\xbf\xc9\xc3\x87\x02\xe6q\xec\xcc\xf8o/\xda<\xed\xe7\xdc\x17\
    \xf1\x06K\xa1G\xe2\xcd\x08x\x9c-\x8cA\x0e\xc2 \x10E\xf7\x9cb\xf6&\x06PB\x9b\x18\xe3U\x86\x99\
    \xb1\xc1\x94B\x904\xb6\xa7\xafD\xdf\xe2\xbd\xbf\xfa9\xbc\x84\x1a\x18\x16\xba>\xc9\xe9\x81\
    \x07\xb4\x9eF1\xc1\xb2\xd6\xde\xe1\x18\x1c\x129\x09\xfab\x8dj[\x11\xa0\x9cRl\xaa\xe1\x04\xfcU\
    \x1f\x93T\xd8WzC\xc1\x1a\xdb\x06\xb7_\x1f\xf2\xc1Tf9\xc7e\xc59\xf2\x1d\x8c\xd7\x7f\xe0\xd4\
    \xad\x14\xe7%W\xe8?\x07\xa2*,\xc6\xeb\x03\x81\x12x\x9c{\xc6\xb8\x9dQ\xb7\xa4(5U\xc14\xd9\xc48\
    \xc9\xc04\xc5\xd0\xd4\xd2\xcc$\xd525\xcd\xdc\xd4<\xd54\xcd(-\xcd 91\xd5<1\xd5\xd4\xd08\xd9\
    \xd2\xc0tbl+kQ~~\x09\x17\x00\x1f\xa9\x12\x10\xa5\x02x\x9c340031QH\xc9\xcf\xcb/\xd2+\xa9(a\
    \x98\xaf\xc4\xb6\xfdDa\xf4\x89l\xe7\xaa\xb0\xb0\xc8\xc3[\xa7\xd5\x14\x09\x01\x00\xeb\xf5\x0e\
    \x15\xa5\x02x\x9c340031QH\xc9\xcf\xcb/\xd2+\xa9(aX\xe2u\xf2Sg~\xfb\xb3oG|\xc25\xe5\xb8?~\xb0\
    \x98\xb1\x05\x00\x06\x8e\x10\"=x\x9cK\xc9\xcf\xcb/R(NM\xce\xcfK\xe1\x02\x00#\xb1\x04\xc9>x\
    \x9cK\xc9\xcf\xcb/R(H\xac\xcc\xc9OL\xe1\x02\x00)t\x057\x98v7]\xd6\x8b85\x05\xc8\xf9(\xfb\xf06\
    \xd8\xfe<%6";

/// A **filtered** v3 bundle: `@filter=blob:none` after `@object-format`, and a
/// pack with the blobs left out. Stock reads it and reports
/// `The bundle uses this filter: blob:none`; the port refuses the capability.
/// See defect 1 in the module header.
const BUNDLE_V3_FILTER: &[u8] = b"# v3 git bundle\n@object-format=sha1\n@filter=blob:none\n1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 refs/heads/donor\
    \nf630167b77c69f67b5c50b8e252e483626504376 refs/tags/dtag\n1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 HEAD\
    \n\nPACK\x00\x00\x00\x02\x00\x00\x00\x05\x96\x0ex\x9c\x9d\x8dI\n\x021\x10E\xf79E\xf6\x82d\
    \x1e@\xc4\xab$\x95*\x0c\xf4D[6\xea\xe9m\xd1\x13\xf8\x17\xef\xad\x1e\x9fWD\x99S\x08\x88d\x0be\
    \x85.\x86\x10*Y\xf09z@\x07\xd5\xa9\x9a\xb2\x81\xa4\xc4RV\x9cX\xa2\xf5\x05\xaa1\xd4R\x85\x84VS\
    \xd5\xdad2\xcd\x81i\x9eB \xd5\xa8:Q\xee|\x9dW\xf9\xda\xe0&\xf7\xb6\xf3S\x9e\xbe\xbe\xe0\xa3\
    \x8c\xcb\x80\xc7>me\xe8\xed,uT\xbf\xc9\xc3\x87\x02\xe6q\xec\xcc\xf8o/\xda<\xed\xe7\xdc\x17\
    \xf1\x06K\xa1G\xe2\xcd\x08x\x9c-\x8cA\x0e\xc2 \x10E\xf7\x9cb\xf6&\x06PB\x9b\x18\xe3U\x86\x99\
    \xb1\xc1\x94B\x904\xb6\xa7\xafD\xdf\xe2\xbd\xbf\xfa9\xbc\x84\x1a\x18\x16\xba>\xc9\xe9\x81\
    \x07\xb4\x9eF1\xc1\xb2\xd6\xde\xe1\x18\x1c\x129\x09\xfab\x8dj[\x11\xa0\x9cRl\xaa\xe1\x04\xfcU\
    \x1f\x93T\xd8WzC\xc1\x1a\xdb\x06\xb7_\x1f\xf2\xc1Tf9\xc7e\xc59\xf2\x1d\x8c\xd7\x7f\xe0\xd4\
    \xad\x14\xe7%W\xe8?\x07\xa2*,\xc6\xeb\x03\x81\x12x\x9c{\xc6\xb8\x9dQ\xb7\xa4(5U\xc14\xd9\xc48\
    \xc9\xc04\xc5\xd0\xd4\xd2\xcc$\xd525\xcd\xdc\xd4<\xd54\xcd(-\xcd 91\xd5<1\xd5\xd4\xd08\xd9\
    \xd2\xc0tbl+kQ~~\x09\x17\x00\x1f\xa9\x12\x10\xa5\x02x\x9c340031QH\xc9\xcf\xcb/\xd2+\xa9(a\
    \x98\xaf\xc4\xb6\xfdDa\xf4\x89l\xe7\xaa\xb0\xb0\xc8\xc3[\xa7\xd5\x14\x09\x01\x00\xeb\xf5\x0e\
    \x15\xa5\x02x\x9c340031QH\xc9\xcf\xcb/\xd2+\xa9(aX\xe2u\xf2Sg~\xfb\xb3oG|\xc25\xe5\xb8?~\xb0\
    \x98\xb1\x05\x00\x06\x8e\x10\"\xa6\xc6\xcf\x15\xf0\xc2\x98\xfc\xc9\xc54\xe8\xbdD\x90\x86\xd9\
    \x95o\xa5";

/// A bundle with a **prerequisite**: `-e35acb22… donor root` ahead of the ref
/// line, and a thin pack whose deltas point at an object the bundle does not
/// carry. No fixture has that commit, so every repository this is fed to lacks
/// it — which is the only way to reach the `Repository lacks these prerequisite
/// commits` path, and the one bundle diagnostic identical on both sides.
const BUNDLE_PREREQ: &[u8] = b"# v2 git bundle\n-e35acb22fd8bc8e31fb1129f2d4c2d5f66f0dfb4 donor root\n1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 refs/heads/donor\
    \n\nPACK\x00\x00\x00\x02\x00\x00\x00\x03\x96\x0ex\x9c\x9d\x8dI\n\x021\x10E\xf79E\xf6\x82d\
    \x1e@\xc4\xab$\x95*\x0c\xf4D[6\xea\xe9m\xd1\x13\xf8\x17\xef\xad\x1e\x9fWD\x99S\x08\x88d\x0be\
    \x85.\x86\x10*Y\xf09z@\x07\xd5\xa9\x9a\xb2\x81\xa4\xc4RV\x9cX\xa2\xf5\x05\xaa1\xd4R\x85\x84VS\
    \xd5\xdad2\xcd\x81i\x9eB \xd5\xa8:Q\xee|\x9dW\xf9\xda\xe0&\xf7\xb6\xf3S\x9e\xbe\xbe\xe0\xa3\
    \x8c\xcb\x80\xc7>me\xe8\xed,uT\xbf\xc9\xc3\x87\x02\xe6q\xec\xcc\xf8o/\xda<\xed\xe7\xdc\x17\
    \xf1\x06K\xa1G\xe2\xa5\x02x\x9c340031QH\xc9\xcf\xcb/\xd2+\xa9(a\x98\xaf\xc4\xb6\xfdDa\xf4\
    \x89l\xe7\xaa\xb0\xb0\xc8\xc3[\xa7\xd5\x14\x09\x01\x00\xeb\xf5\x0e\x15=x\x9cK\xc9\xcf\xcb/R(NM\
    \xce\xcfK\xe1\x02\x00#\xb1\x04\xc9\x11\xbd\xa4\x06\x8a\xb7\xe9\x98'\xc5ovezX%3\x96\x81\x88";

/// [`BUNDLE_V2`] cut off inside its pack. The header still parses — `verify`
/// and `list-heads` answer from it and exit 0 — and only `unbundle`, which
/// hands the pack to `index-pack`, discovers the truncation. A reader that
/// validates the whole file before answering diverges on the first two.
const BUNDLE_TRUNCATED: &[u8] = BUNDLE_V2.split_at(575).0;

/// A bundle header with **no pack at all**: the ref list, the blank line, and
/// end of stream. Distinct from [`BUNDLE_TRUNCATED`] in where the reader gives
/// up — zero pack bytes rather than a partial object.
const BUNDLE_HEADER_ONLY: &[u8] = b"# v2 git bundle\n\
    1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 refs/heads/donor\n\
    f630167b77c69f67b5c50b8e252e483626504376 refs/tags/dtag\n\
    1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 HEAD\n\
    \n";

/// A v3 header carrying a capability no git defines. Rejected before the pack
/// is read, so the header alone is the whole payload — verified to produce the
/// identical answer to the same capability injected into a complete bundle.
/// Stock: `error: unknown capability 'nosuchcapability=1'`, exit 1. Port: exit
/// 128. See defect 2.
const BUNDLE_UNKNOWN_CAP: &[u8] = b"# v3 git bundle\n\
    @object-format=sha1\n\
    @nosuchcapability=1\n\
    1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 refs/heads/donor\n\
    f630167b77c69f67b5c50b8e252e483626504376 refs/tags/dtag\n\
    1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 HEAD\n\
    \n";

/// A signature line naming a version that does not exist. The first line is the
/// only thing a bundle reader looks at before deciding this is not a bundle.
const BUNDLE_BAD_SIGNATURE: &[u8] = b"# v9 git bundle\n\
    1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 refs/heads/donor\n\
    f630167b77c69f67b5c50b8e252e483626504376 refs/tags/dtag\n\
    1dec4fc508d8a27c9e1b2d0075a9b5acc5eb0321 HEAD\n\
    \n";

// ---------------------------------------------------------------------------
// Revision lists for `bundle create --stdin`
// ---------------------------------------------------------------------------

/// Two refs, one per line — the ordinary `rev-list --stdin` payload. Named in
/// full rather than as `main`/`feature` so the case does not also depend on how
/// a short name is disambiguated.
const REVS_TWO: &[u8] = b"refs/heads/main\nrefs/heads/feature\n";

/// An empty selection delivered through stdin rather than through an argument.
/// `fatal: Refusing to create empty bundle.`
const REVS_EMPTY: &[u8] = b"";

/// A revision that does not resolve, so the failure is in `rev-list`'s parser
/// rather than in the bundle writer: `fatal: bad revision 'nosuchrev'`.
const REVS_BAD: &[u8] = b"nosuchrev\n";

/// A range, so `--stdin` reaches the prerequisite-bearing thin-pack path.
const REVS_RANGE: &[u8] = b"main~1..main\n";

// ---------------------------------------------------------------------------
// fast-export streams, replayed through fast-import
// ---------------------------------------------------------------------------


/// `Shape::Branched` as stock git serialises it. Five blobs, four commits, a
/// `reset` for the lightweight tag and a `tag` record for the annotated one,
/// all through marks. Replayed into `Shape::Linear` it rebuilds `Branched`
/// exactly — see the module header for the ref listing both sides produce.
const FE_ALL: &[u8] = b"blob\nmark :1\ndata 10\n# fixture\n\nblob\nmark :2\ndata 26\npub fn one() -> u32 { 1 }\n\nreset refs/heads/main\
    \ncommit refs/heads/main\nmark :3\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 8\ninitial\nM 100644 :1 README.md\
    \nM 100644 :2 src/lib.rs\n\nblob\nmark :4\ndata 52\npub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\
    \n\ncommit refs/heads/main\nmark :5\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 8\nadd two\nfrom :3\
    \nM 100644 :4 src/lib.rs\n\nblob\nmark :6\ndata 13\nfeature work\n\ncommit refs/heads/feature\
    \nmark :7\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ndata 15\nfeature commit\nfrom :5\nM 100644 :6 feature.txt\n\nreset refs/tags/v0.1.0\nfrom :5\
    \n\ntag v0.2.0\nfrom :5\ntagger zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 10\
    \nannotated\n\n";

/// The same export with `--use-done-feature`: a `feature done` line at the top
/// and a `done` terminator at the bottom. The pair only means anything when a
/// reader honours it — a reader that ignores `feature done` accepts a truncated
/// stream silently — and this is the only place the two halves are measured
/// against each other in the form fast-export actually writes.
const FE_DONE: &[u8] = b"feature done\nblob\nmark :1\ndata 10\n# fixture\n\nblob\nmark :2\ndata 26\npub fn one() -> u32 { 1 }\
    \n\nreset refs/heads/main\ncommit refs/heads/main\nmark :3\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 8\ninitial\nM 100644 :1 README.md\
    \nM 100644 :2 src/lib.rs\n\nblob\nmark :4\ndata 52\npub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\
    \n\ncommit refs/heads/main\nmark :5\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 8\nadd two\nfrom :3\
    \nM 100644 :4 src/lib.rs\n\nblob\nmark :6\ndata 13\nfeature work\n\ncommit refs/heads/feature\
    \nmark :7\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ndata 15\nfeature commit\nfrom :5\nM 100644 :6 feature.txt\n\nreset refs/tags/v0.1.0\nfrom :5\
    \n\ntag v0.2.0\nfrom :5\ntagger zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 10\
    \nannotated\n\ndone\n";

/// `--show-original-ids --mark-tags`: every blob, commit and tag carries an
/// `original-oid <sha1>` line, and the annotated tag gets a `mark` of its own.
/// Both are records `fast-import` must accept and neither appears in any
/// hand-written stream in the corpus.
const FE_ORIGINAL_IDS: &[u8] = b"blob\nmark :1\noriginal-oid 9741694d75caeb49d3b7c1f59451c0c56bf6216c\ndata 10\n# fixture\n\
    \nblob\nmark :2\noriginal-oid 46e89a20198dc3175599f285c8d874fc19439a64\ndata 26\npub fn one() -> u32 { 1 }\
    \n\nreset refs/heads/main\ncommit refs/heads/main\nmark :3\noriginal-oid edfab1b71619a22120a8da1a3d85d68e0200290a\
    \nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ndata 8\ninitial\nM 100644 :1 README.md\nM 100644 :2 src/lib.rs\n\nblob\nmark :4\noriginal-oid 74b744054bc0580719c0765bd5efdf0ba1638668\
    \ndata 52\npub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\n\ncommit refs/heads/main\
    \nmark :5\noriginal-oid 5915d79de18d919476d339c8b8efda1d9bb166e2\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 8\nadd two\nfrom :3\
    \nM 100644 :4 src/lib.rs\n\nblob\nmark :6\noriginal-oid bac0ee79a7406f740faf7597ca3b863ff847490c\
    \ndata 13\nfeature work\n\ncommit refs/heads/feature\nmark :7\noriginal-oid 07e86d1fedb713fbc84a754c98ea4bfe53316416\
    \nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ndata 15\nfeature commit\nfrom :5\nM 100644 :6 feature.txt\n\nreset refs/tags/v0.1.0\nfrom :5\
    \n\ntag v0.2.0\nmark :8\nfrom :5\noriginal-oid d7277ea97518c8631ff11851f616d1ca422aeef0\ntagger zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ndata 10\nannotated\n\n";

/// `--full-tree`: each commit re-states its whole tree behind a `deleteall`
/// instead of a delta against its parent. The importer therefore rebuilds every
/// tree from nothing on every commit, and must still land the same ids.
const FE_FULL_TREE: &[u8] = b"blob\nmark :1\ndata 10\n# fixture\n\nblob\nmark :2\ndata 26\npub fn one() -> u32 { 1 }\n\nreset refs/heads/main\
    \ncommit refs/heads/main\nmark :3\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 8\ninitial\ndeleteall\
    \nM 100644 :1 README.md\nM 100644 :2 src/lib.rs\n\nblob\nmark :4\ndata 52\npub fn one() -> u32 { 1 }\
    \npub fn two() -> u32 { 2 }\n\ncommit refs/heads/main\nmark :5\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 8\nadd two\nfrom :3\
    \ndeleteall\nM 100644 :1 README.md\nM 100644 :4 src/lib.rs\n\nblob\nmark :6\ndata 13\nfeature work\
    \n\ncommit refs/heads/feature\nmark :7\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 15\nfeature commit\
    \nfrom :5\ndeleteall\nM 100644 :1 README.md\nM 100644 :6 feature.txt\nM 100644 :4 src/lib.rs\
    \n\nreset refs/tags/v0.1.0\nfrom :5\n\ntag v0.2.0\nfrom :5\ntagger zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ndata 10\nannotated\n\n";

/// `-M -C --no-data` over `Shape::Renamed`: no blob bodies at all — every `M`
/// names a 40-hex object id — and the rename and copy decisions carried as `R`
/// and `C` records. Imported into `Renamed` itself, because a stream that
/// references blobs by id can only be read where those blobs already are.
const FE_RENAMES_NO_DATA: &[u8] = b"reset refs/heads/main\ncommit refs/heads/main\nmark :1\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 8\ninitial\nM 100644 9741694d75caeb49d3b7c1f59451c0c56bf6216c README.md\
    \nM 100644 46e89a20198dc3175599f285c8d874fc19439a64 src/lib.rs\n\ncommit refs/heads/main\nmark :2\
    \nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ndata 14\nrenames: seed\nfrom :1\nM 100644 7843c7f2b37d7476ff34ace934dca1bf1b430170 orig/alpha.txt\
    \nM 100644 a3c752996e1990c050a43947f270790068b48b73 orig/beta.txt\nM 100644 8115aab5c527b5b8bc91ee0d93bcfc7969ea1344 orig/delta.txt\
    \nM 100644 59e9640eb6f484d3778b0f9948d8399becac665d orig/gamma.txt\n\ncommit refs/heads/main\
    \nmark :3\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ndata 21\nrenames: pure rename\nfrom :2\nR orig/alpha.txt moved/alpha.txt\n\ncommit refs/heads/main\
    \nmark :4\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ndata 26\nrenames: rename with edit\nfrom :3\nR orig/beta.txt moved/beta.txt\nM 100644 e343a2134584ea875c20443db2c2f2bbe0e9e8a4 moved/beta.txt\
    \n\ncommit refs/heads/main\nmark :5\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 35\nrenames: copy with modified source\
    \nfrom :4\nC orig/gamma.txt copies/gamma.txt\nM 100644 e0c31f385ad1c6628bd55a80af6870f70659b01c orig/gamma.txt\
    \n\ncommit refs/heads/main\nmark :6\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\
    \ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 26\nrenames: rewrite in place\
    \nfrom :5\nM 100644 3fd04e3a6fb2d39e0fc4e747ed6dadab944da06e orig/delta.txt\n\n";

// ---------------------------------------------------------------------------
// fast-import streams: the commands and headers `archive_export.rs` does not
// reach
// ---------------------------------------------------------------------------
//
// Same rule as that module's: every stream was run against stock 2.55.0 before
// it was written down, and every `data <n>` count is exact. One byte off and
// fast-import reads the next command's first character as payload — a mistake
// that shows up as `fatal: unsupported command: rom refs/heads/main^0`, which
// is how the count on `S_GITLINK` was caught before it shipped.

/// `M 160000` — a gitlink, pointing at a commit that exists in every fixture.
/// The one filemodify mode no stream in the corpus writes, and the one whose
/// target is a commit rather than a blob, so an importer that validates the
/// referenced object's type has to special-case it.
const S_GITLINK: &[u8] = b"commit refs/heads/gitlinked\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 8\n\
    gitlink\n\
    from refs/heads/main^0\n\
    M 160000 edfab1b71619a22120a8da1a3d85d68e0200290a sub\n\
    \n";

/// `M 100755` — the executable bit, which changes the tree's bytes and so the
/// commit id. Nothing else in the corpus imports one.
const S_EXEC_MODE: &[u8] = b"blob\n\
    mark :1\n\
    data 12\n\
    #!/bin/sh\nex\n\
    commit refs/heads/execmode\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 5\n\
    exec\n\
    from refs/heads/main^0\n\
    M 100755 :1 run.sh\n\
    \n";

/// A commit header carrying `original-oid` and `encoding` together, in the
/// order `fast-import.c` requires them: mark, original-oid, author, committer,
/// encoding, data. `encoding` becomes an `encoding` line in the commit object
/// and therefore moves the commit id; `original-oid` is recorded and dropped.
const S_ORIGINAL_OID_ENCODING: &[u8] = b"commit refs/heads/oid-and-encoding\n\
    mark :1\n\
    original-oid edfab1b71619a22120a8da1a3d85d68e0200290a\n\
    author zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    encoding ISO-8859-1\n\
    data 8\n\
    encoded\n\
    from refs/heads/main^0\n\
    \n";

/// `reset` to the null oid, which **deletes** the ref. The fixture's only
/// branch goes away and `for-each-ref` comes back empty — a stream whose whole
/// effect is a removal, which nothing else in the corpus asks for.
const S_RESET_TO_ZERO: &[u8] =
    b"reset refs/heads/main\nfrom 0000000000000000000000000000000000000000\n\n";

/// `get-mark` for a mark the stream never declared: `fatal: mark :42 not
/// declared`. The pair to `archive_export.rs`'s `S_GETMARK`, which asks for one
/// that exists.
const S_GETMARK_UNDECLARED: &[u8] = b"get-mark :42\n";

/// `cat-blob` addressed by **mark** rather than by object id — the other half
/// of `archive_export.rs`'s `S_CATBLOB`, which uses a 40-hex id. The mark is
/// declared by the blob immediately above it, so the answer comes out of the
/// stream's own bookkeeping and not out of the object store.
const S_CATBLOB_MARK: &[u8] = b"blob\nmark :5\ndata 11\nmarked blob\ncat-blob :5\n";

/// `ls` against a mark, after the commit that declared it. `archive_export.rs`
/// asks `ls` about a literal commit id; a mark is resolved through a different
/// path, and the query runs while that commit is still the active branch.
const S_LS_MARK: &[u8] = b"commit refs/heads/lsmark\n\
    mark :1\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 3\n\
    ls\n\
    from refs/heads/main^0\n\
    \n\
    ls :1 \"README.md\"\n";

/// A `feature` this version of fast-import does not have.
const S_FEATURE_UNKNOWN: &[u8] = b"feature nosuchfeature\n";

/// `feature date-format=raw` — the in-stream spelling of the `--date-format`
/// flag, which is how a producer states its own format rather than relying on
/// the consumer's command line.
const S_FEATURE_DATE_FORMAT: &[u8] = b"feature date-format=raw\n\
    commit refs/heads/featdate\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 5\n\
    feat\n\
    from refs/heads/main^0\n\
    \n";

/// An `option git` fast-import does not have: rejected.
const S_OPTION_UNKNOWN: &[u8] = b"option git nosuchoption\n";

/// An `option non-git`, which the specification says to **ignore**. A parser
/// that rejects every option it does not know fails here and nowhere else.
const S_OPTION_NON_GIT: &[u8] = b"option non-git whatever\nblob\nmark :1\ndata 3\nng\n";

/// A filemodify naming a 40-hex id no repository holds. Distinct from
/// `archive_export.rs`'s `S_BADMARK`: a mark is caught in the stream's own
/// table, an object id is caught against the store.
const S_MISSING_OID: &[u8] = b"commit refs/heads/missingoid\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 8\n\
    missing\n\
    M 100644 0123456789012345678901234567890123456789 gone.txt\n\
    \n";

/// A ref name `check_refname_format()` rejects. The stream is well-formed; what
/// it asks for is not a legal ref.
const S_BAD_REFNAME: &[u8] = b"commit refs/heads/..bad\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 4\n\
    bad\n\
    from refs/heads/main^0\n\
    \n";

/// A blob whose six bytes include a NUL and are not valid UTF-8, so the object
/// the importer writes cannot be produced by any text path. `data 6` counts
/// `\x00\x01\x02\x03\x04\n`.
const S_BINARY_BLOB: &[u8] = b"blob\n\
    mark :1\n\
    data 6\n\
    \x00\x01\x02\x03\x04\n\
    commit refs/heads/binary\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 3\n\
    bn\n\
    M 100644 :1 bin.dat\n\
    \n";

// ---------------------------------------------------------------------------

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    bundle_read_real_bundles(out);
    bundle_read_broken_bundles(out);
    bundle_create_from_stdin(out);
    bundle_filter_capability(out);
    fast_export_round_trips(out);
    fast_export_uncovered_flags(out);
    fast_import_stream_commands(out);
    fast_import_uncovered_flags(out);
}

/// One `bundle` case reading `payload` from standard input.
fn bundle_in(args: &[&str], shape: Shape, payload: &'static [u8]) -> Case {
    Case::with_stdin("bundle", args, shape, payload)
}

// ---------------------------------------------------------------------------
// bundle: the read side, fed bundles it will accept
// ---------------------------------------------------------------------------

/// The three readers against two real bundles and a prerequisite-bearing one.
///
/// This is the cross the rest of the corpus cannot make: **stock git wrote
/// these bytes and the port has to read them.** `verify` parses the header and
/// checks the prerequisites; `list-heads` prints the ref list, optionally
/// filtered; `unbundle` does both and then hands the pack to `index-pack`, so
/// only it leaves anything behind.
///
/// What `unbundle` leaves behind is the whole point of running it on three
/// different shapes. The donor's objects are in no fixture, so on `Linear` and
/// `Branched` seven new objects arrive and the state probe's
/// `cat-file --batch-all-objects` census moves; `probe_interop` then asks stock
/// git to `fsck --strict` what each side wrote. On `Packed` the receiving
/// repository already has packs, so the same import has to coexist with them.
///
/// Measured against stock 2.55.0 on `Linear`: `unbundle` prints the three ref
/// lines, exits 0, and leaves the donor's seven objects behind in
/// `pack-9876375dd68b383505c8f928fbf036d8fe3c2536.pack` — the same pack name on
/// both sides, which is a stronger statement than the object census alone since
/// the name is the pack's own content hash.
///
/// `unbundle` does **not** update refs — it prints the names the bundle
/// declares and stops — so the donor's `refs/heads/donor` colliding with
/// nothing, and its `HEAD` line, are reported rather than applied. A port that
/// treated `unbundle` as a fetch would move `refs/heads/main` here and be
/// caught by `for-each-ref` in the digest.
fn bundle_read_real_bundles(out: &mut Vec<Case>) {
    for (sub, payload) in [
        ("verify", BUNDLE_V2),
        ("list-heads", BUNDLE_V2),
        ("unbundle", BUNDLE_V2),
        ("verify", BUNDLE_V3),
        ("list-heads", BUNDLE_V3),
        ("unbundle", BUNDLE_V3),
    ] {
        out.push(bundle_in(&["bundle", sub, "-"], Shape::Linear, payload));
    }

    // The same import into two more receiving repositories: one that already
    // has the branch names the bundle mentions, and one that already has packs.
    out.push(bundle_in(&["bundle", "unbundle", "-"], Shape::Branched, BUNDLE_V2));
    out.push(bundle_in(&["bundle", "unbundle", "-"], Shape::Packed, BUNDLE_V2));
    out.push(bundle_in(&["bundle", "unbundle", "-"], Shape::Branched, BUNDLE_V3));

    // `--progress` on the reader, where `fetch_clone.rs` only has it on the
    // writer. Not strict: the counters are on stderr and the file name in
    // `index-pack`'s reporting is the port's `-` against stock's `<stdin>`.
    out.push(bundle_in(&["bundle", "unbundle", "--progress", "-"], Shape::Linear, BUNDLE_V2));
    out.push(bundle_in(&["bundle", "unbundle", "-q", "-"], Shape::Linear, BUNDLE_V2));

    // Ref-name arguments narrow what `list-heads` and `unbundle` report. Both
    // spellings: one that matches a ref the bundle carries, and one that
    // matches nothing — which is exit 0 with empty stdout, not an error.
    out.push(bundle_in(
        &["bundle", "list-heads", "-", "refs/heads/donor"],
        Shape::Linear,
        BUNDLE_V2,
    ));
    out.push(bundle_in(&["bundle", "list-heads", "-", "refs/heads/nosuch"], Shape::Linear, BUNDLE_V2));
    out.push(bundle_in(&["bundle", "unbundle", "-", "refs/tags/dtag"], Shape::Linear, BUNDLE_V2));
    out.push(bundle_in(&["bundle", "unbundle", "-", "refs/heads/nosuch"], Shape::Linear, BUNDLE_V2));
    // `verify` takes no ref arguments; a trailing one is accepted and ignored.
    out.push(bundle_in(&["bundle", "verify", "-", "extra-arg"], Shape::Linear, BUNDLE_V2));

    // The prerequisite bundle. `list-heads` answers from the header and does
    // not care that the prerequisite is missing (exit 0); `verify` and
    // `unbundle` both refuse with exit 1 and the identical two lines on both
    // sides —
    //
    //   error: Repository lacks these prerequisite commits:
    //   error: e35acb22fd8bc8e31fb1129f2d4c2d5f66f0dfb4
    //
    // — which is the one bundle diagnostic that names no file, and therefore
    // the one this module can compare byte for byte. See the module header for
    // why every other bundle refusal here is not strict.
    out.push(bundle_in(&["bundle", "list-heads", "-"], Shape::Linear, BUNDLE_PREREQ));
    out.push(Case {
        compare_stderr: true,
        ..bundle_in(&["bundle", "verify", "-"], Shape::Linear, BUNDLE_PREREQ)
    });
    out.push(Case {
        compare_stderr: true,
        ..bundle_in(&["bundle", "unbundle", "-"], Shape::Linear, BUNDLE_PREREQ)
    });
    // The same prerequisite against a repository with a different object set:
    // still missing, because the donor is foreign to every fixture.
    out.push(bundle_in(&["bundle", "verify", "-"], Shape::Branched, BUNDLE_PREREQ));
}

/// Bundles that are damaged, and **where** each reader gives up.
///
/// The three payloads are graded by how far a reader gets before it fails, and
/// that grading is the case: a reader that validates the whole file up front
/// answers all three the same way and diverges on two of them.
///
/// * [`BUNDLE_BAD_SIGNATURE`] — rejected on the **first line**, before the ref
///   list is parsed. Exit 1 for all three subcommands.
/// * [`BUNDLE_UNKNOWN_CAP`] — signature and object-format accepted, then a
///   capability nothing defines. Stock exits **1**; the port exits **128**.
///   Defect 2 in the module header.
/// * [`BUNDLE_HEADER_ONLY`] and [`BUNDLE_TRUNCATED`] — header **valid**, pack
///   absent or cut. `verify` and `list-heads` succeed and exit 0 on both;
///   `unbundle` fails at `index-pack` with exit 1.
fn bundle_read_broken_bundles(out: &mut Vec<Case>) {
    for payload in [BUNDLE_BAD_SIGNATURE, BUNDLE_UNKNOWN_CAP, BUNDLE_HEADER_ONLY, BUNDLE_TRUNCATED] {
        for sub in ["verify", "list-heads", "unbundle"] {
            out.push(bundle_in(&["bundle", sub, "-"], Shape::Linear, payload));
        }
    }
}

// ---------------------------------------------------------------------------
// bundle: `create --stdin`
// ---------------------------------------------------------------------------

/// `bundle create` taking its revision list from **standard input**.
///
/// `builtin/bundle.c` hands its trailing arguments to `rev-list`, so `--stdin`
/// is not a bundle option at all — it is the revision walker's, and it makes
/// the *selection* a stream rather than an argv. Nothing in the corpus had ever
/// fed `bundle create` anything on stdin, so the whole path was unmeasured:
/// every existing case names its revisions as arguments.
///
/// Both destinations, because they are different code: `-` puts the bundle on
/// stdout where the harness compares it byte for byte, and a file leaves an
/// untracked path the state probe reports.
///
/// Verified byte-identical between the two implementations on
/// `Shape::Branched`: 1025 bytes of bundle from [`REVS_TWO`], the same on both
/// sides.
fn bundle_create_from_stdin(out: &mut Vec<Case>) {
    fn create(args: &[&str], payload: &'static [u8]) -> Case {
        Case::with_stdin("bundle", args, Shape::Branched, payload)
    }

    out.push(create(&["bundle", "create", "-", "--stdin"], REVS_TWO));
    out.push(create(&["bundle", "create", "out.bundle", "--stdin"], REVS_TWO));
    out.push(create(&["bundle", "create", "--version=3", "-", "--stdin"], REVS_TWO));
    out.push(create(&["bundle", "create", "-q", "out.bundle", "--stdin"], REVS_TWO));
    // A range on stdin, so the bundle carries a prerequisite line.
    out.push(create(&["bundle", "create", "-", "--stdin"], REVS_RANGE));
    // `--stdin` alongside an argument selection: the two sources are unioned.
    out.push(create(&["bundle", "create", "-", "--stdin", "--tags"], REVS_TWO));

    // Refusals. Both messages are byte-identical on the two sides and name no
    // file, so both are strict.
    out.push(Case { compare_stderr: true, ..create(&["bundle", "create", "-", "--stdin"], REVS_EMPTY) });
    out.push(Case { compare_stderr: true, ..create(&["bundle", "create", "-", "--stdin"], REVS_BAD) });

    // Bundle versions that do not exist, on either side of the two that do.
    // `fatal: unsupported bundle version <n>`, identical on both sides.
    for version in ["--version=1", "--version=4"] {
        out.push(Case {
            compare_stderr: true,
            ..Case::new("bundle", &["bundle", "create", version, "-", "--all"], Shape::Branched)
        });
    }
}

// ---------------------------------------------------------------------------
// bundle: the v3 `@filter` capability
// ---------------------------------------------------------------------------

/// A **partial** bundle, in both directions. Defect 1 in the module header.
///
/// `--filter=` reaches `bundle create` the same way `--stdin` does — as a
/// `rev-list` argument — and it changes the *container*: the pack loses the
/// filtered objects and the header gains a `@filter=<spec>` capability, which
/// forces v3 whether or not `--version=3` was asked for. Measured on
/// `Shape::Branched` with stock 2.55.0, `--filter=blob:none` with no `--version`
/// at all produces
///
/// ```text
/// # v3 git bundle
/// @object-format=sha1
/// @filter=blob:none
/// ```
///
/// The port takes neither end. Writing, `--filter=blob:none` is not recognised
/// as an option and falls through to revision parsing —
/// `fatal: ambiguous argument '--filter=blob:none': unknown revision or path not
/// in the working tree`, exit 128 against stock's 0. Reading, stock's own
/// filtered bundle ([`BUNDLE_V3_FILTER`]) is rejected at the header —
/// `fatal: malformed bundle header in "-": capability "filter=blob:none" is not
/// supported`, exit 128 against stock's 0 — on all three subcommands, including
/// `unbundle`, where stock unpacks the filtered pack and the port writes
/// nothing.
///
/// Three filter specs on the write side because they are three different
/// capability strings and a port that learns to pass one through is not
/// finished; three subcommands on the read side because the refusal is in the
/// header parser they share and each has its own exit path out of it.
fn bundle_filter_capability(out: &mut Vec<Case>) {
    for spec in ["--filter=blob:none", "--filter=tree:0", "--filter=blob:limit=100"] {
        out.push(Case::new(
            "bundle",
            &["bundle", "create", "--version=3", "-", "--all", spec],
            Shape::Branched,
        ));
    }
    // Without `--version`: the filter is what upgrades the container to v3, so
    // this is a different decision from the three above and not a duplicate.
    out.push(Case::new(
        "bundle",
        &["bundle", "create", "-", "--all", "--filter=blob:none"],
        Shape::Branched,
    ));
    // A filter with `--version=2`, where the capability has nowhere to go.
    out.push(Case::new(
        "bundle",
        &["bundle", "create", "--version=2", "-", "--all", "--filter=blob:none"],
        Shape::Branched,
    ));

    for sub in ["verify", "list-heads", "unbundle"] {
        out.push(bundle_in(&["bundle", sub, "-"], Shape::Linear, BUNDLE_V3_FILTER));
    }
    out.push(bundle_in(&["bundle", "unbundle", "-"], Shape::Packed, BUNDLE_V3_FILTER));
}

// ---------------------------------------------------------------------------
// fast-export → fast-import: the cross round trip
// ---------------------------------------------------------------------------

/// Stock git's own export streams, replayed through the port's importer.
///
/// This is the highest-value shape a single case can take in this territory,
/// and it is the one thing neither `archive_export.rs` half can do alone. That
/// module measures `fast-export`'s **stdout** against stock's, and it measures
/// `fast-import` on **hand-written** streams — so the port's serializer is
/// never asked to be readable by anything, and its parser is never shown a
/// stream a real serializer produced. The two halves can agree with stock
/// separately and still not compose.
///
/// Freezing stock's output into the corpus closes it in the direction that
/// matters: the port's `fast-import` is handed bytes it did not write, and the
/// object graph it rebuilds is compared against the graph stock rebuilds from
/// the same bytes — which, because the streams carry `harden`'s pinned identity
/// and date, is the *original* graph, oid for oid. The other direction is
/// already covered: `fast-export`'s stdout is byte-compared, so a stream the
/// port writes that stock could not read would have to be byte-identical to one
/// stock can, which is a contradiction.
///
/// Four dialects, because they are four different parsers' worth of work:
///
/// * [`FE_ALL`] — marks, `reset`, `from`, `tag`, inline blob bodies.
/// * [`FE_DONE`] — the same plus the `feature done` / `done` contract.
/// * [`FE_ORIGINAL_IDS`] — `original-oid` on every object and a `mark` on the
///   tag, from `--show-original-ids --mark-tags`.
/// * [`FE_FULL_TREE`] — `deleteall` plus a full tree per commit, from
///   `--full-tree`.
/// * [`FE_RENAMES_NO_DATA`] — no blob bodies, `R` and `C` records, imported
///   back into the shape it came from.
///
/// Each stream is imported into more than one shape, and that is where the two
/// findings came from. Into `Shape::Linear` the objects are all new. Into
/// `Shape::Branched` — which for [`FE_ALL`] and [`FE_FULL_TREE`] is the shape
/// the stream was *taken from* — every object is already there, so the import
/// is idempotent and every ref update is a no-op. That is the replay a resumed
/// import performs, and it is where the port aborts on the annotated tag
/// (defect 6). [`FE_RENAMES_NO_DATA`] back into `Shape::Renamed` is the same
/// idempotent shape one level quieter: both sides succeed, and the only
/// difference is the `HEAD` reflog line stock writes for the no-op update and
/// the port does not (defect 7).
///
/// Each is imported twice more than once would be worth: `--quiet` is the
/// reference run, and the default-verbose run pins that the statistics path
/// does not change what lands. `fastimport.unpackLimit` decides pack versus
/// loose for the same objects, which `probe_storage` reports as two different
/// numbers, so a port that always packs and a port that never packs are told
/// apart on the round trip rather than only on a hand-written stream.
fn fast_export_round_trips(out: &mut Vec<Case>) {
    fn import(stream: &'static [u8], shape: Shape) -> Case {
        Case::with_stdin("fast-import", &["fast-import", "--quiet"], shape, stream)
    }

    for stream in [FE_ALL, FE_DONE, FE_ORIGINAL_IDS, FE_FULL_TREE] {
        out.push(import(stream, Shape::Linear));
    }
    out.push(import(FE_RENAMES_NO_DATA, Shape::Renamed));

    // The same stream into a repository that already holds every object it
    // describes: the import must be a no-op on the object census and still
    // report success.
    out.push(import(FE_ALL, Shape::Branched));

    // Verbose, so the statistics path runs. Not strict — see the module header
    // on `--stats`.
    out.push(Case::with_stdin("fast-import", &["fast-import"], Shape::Linear, FE_ALL));

    // Storage layout for one round trip, both sides of the threshold.
    for limit in ["0", "100"] {
        out.push(import(FE_ALL, Shape::Linear).with_config(&[("fastimport.unpackLimit", limit)]));
    }

    // The round trip through a *second* repository shape, so the receiving
    // side's existing refs are not the ones the stream names.
    out.push(import(FE_ALL, Shape::Packed));
    out.push(import(FE_FULL_TREE, Shape::Branched));
}

// ---------------------------------------------------------------------------
// fast-export: the flags `archive_export.rs` does not spell
// ---------------------------------------------------------------------------

/// The enumeration members and marks options left over.
///
/// `archive_export.rs` covers `--signed-tags=strip|verbatim`,
/// `--reencode=yes|no` and `--tag-of-filtered-object=drop`. Each of those is an
/// enumeration with more members than that, and the leftovers are the ones a
/// port forgets: `abort`, `warn`, `warn-strip`, `rewrite`. None of the fixtures
/// carries a signed tag, so `--signed-tags` here measures the *option parser*
/// and the default path rather than the signature branch — stated rather than
/// implied, because a case that cannot fire is worth having only if a reader
/// knows it cannot.
///
/// The two that are defects are the marks options. Defect 3:
/// `--anonymize-map=<from>:<to>` is exit 0 and a stream on stock, exit 1 and
/// `zvcs: fast-export: --anonymize-map is not supported` on the port — measured
/// with a ref mapping (`main:zzz`, which stock renders as
/// `reset refs/heads/zzz`) and with the one-argument form (`README.md`, which
/// maps a name to itself and so leaves that one path un-anonymized while
/// everything around it becomes `path0`). Defect 4: `--import-marks=` pointed at
/// a file that exists and is **not** a marks file is
/// `fatal: corrupt mark line: # fixture` at exit 128 on stock, and exit **0**
/// with a complete export on the port — an unreadable resume file silently
/// ignored, which is worse than refusing it, because a resumed export would
/// then re-emit history the consumer already has.
///
/// `archive_export.rs` reaches `--import-marks` only on an absent path, where
/// both sides fail. `README.md` is the same option against a file that opens.
fn fast_export_uncovered_flags(out: &mut Vec<Case>) {
    fn fe(out: &mut Vec<Case>, args: &[&str], shape: Shape) {
        out.push(Case::new("fast-export", args, shape));
    }

    for mode in ["abort", "warn", "warn-strip"] {
        let flag = format!("--signed-tags={mode}");
        fe(out, &["fast-export", &flag, "--all"], Shape::Branched);
    }
    for mode in ["abort", "rewrite"] {
        let flag = format!("--tag-of-filtered-object={mode}");
        fe(out, &["fast-export", &flag, "--all"], Shape::Branched);
    }
    fe(out, &["fast-export", "--reencode=abort", "--all"], Shape::Branched);

    // `--reference-excluded-parents` only means anything when a parent is
    // outside the frontier, so it is measured on a range as well as on `--all`
    // — with `fast-export main~1..main`, the same range without the flag, as its
    // pair. That bare case is `archive_export.rs`'s and is not repeated here.
    fe(out, &["fast-export", "--reference-excluded-parents", "--all"], Shape::Branched);
    fe(out, &["fast-export", "--reference-excluded-parents", "main~1..main"], Shape::Branched);
    fe(out, &["fast-export", "--reference-excluded-parents", "--all"], Shape::Merged);

    // Defect 3.
    fe(out, &["fast-export", "--anonymize", "--anonymize-map=main:zzz", "--all"], Shape::Branched);
    fe(out, &["fast-export", "--anonymize", "--anonymize-map=README.md", "--all"], Shape::Branched);
    // Without `--anonymize` it is an option-parser refusal, and the message is
    // identical on both sides: `fatal: the option '--anonymize-map' requires
    // '--anonymize'`.
    out.push(Case::strict(
        "fast-export",
        &["fast-export", "--anonymize-map=main:zzz", "--all"],
        Shape::Branched,
    ));

    // Defect 4: a marks file that opens and does not parse.
    fe(out, &["fast-export", "--import-marks=README.md", "--all"], Shape::Branched);
    fe(out, &["fast-export", "--import-marks-if-exists=README.md", "--all"], Shape::Branched);
    // The same option pointed at a *directory*, which fails at open rather than
    // at parse.
    fe(out, &["fast-export", "--import-marks=src", "--all"], Shape::Branched);

    // A wildcard refspec, where `archive_export.rs` has only the one-to-one
    // form. The matcher has to expand it against every exported ref.
    fe(out, &["fast-export", "--refspec=refs/heads/*:refs/remotes/o/*", "--all"], Shape::Branched);

    // `--no-data` on its own, on the shape whose renames make the record kinds
    // visible without any blob bodies to hide them.
    fe(out, &["fast-export", "--no-data", "-M", "-C", "--all"], Shape::Renamed);
}

// ---------------------------------------------------------------------------
// fast-import: stream commands and headers
// ---------------------------------------------------------------------------

/// One case per stream above, all on `Shape::Linear` so the receiving
/// repository is the same in each and the difference is the stream.
///
/// Nine succeed and five refuse, and the refusals are not interchangeable: an
/// undeclared mark is caught in the stream's own table, an absent object id is
/// caught against the store, a `feature` and an `option` are each rejected by
/// their own table, and an illegal ref name is rejected by
/// `check_refname_format()` after the objects are already written. A port that
/// answers "malformed input" to all five agrees with stock on none of them.
///
/// None is [`Case::strict`]: stock names its crash report file on stderr and
/// the name carries a process id. The exit code and the post-command state
/// carry the verdict, and they differ between these five — `S_BAD_REFNAME`
/// leaves its blob and commit behind and moves no ref, `S_FEATURE_UNKNOWN`
/// leaves nothing at all.
fn fast_import_stream_commands(out: &mut Vec<Case>) {
    fn q(out: &mut Vec<Case>, stream: &'static [u8], shape: Shape) {
        out.push(Case::with_stdin("fast-import", &["fast-import", "--quiet"], shape, stream));
    }

    q(out, S_GITLINK, Shape::Linear);
    q(out, S_EXEC_MODE, Shape::Linear);
    q(out, S_ORIGINAL_OID_ENCODING, Shape::Linear);
    q(out, S_RESET_TO_ZERO, Shape::Linear);
    q(out, S_CATBLOB_MARK, Shape::Linear);
    q(out, S_LS_MARK, Shape::Linear);
    q(out, S_FEATURE_DATE_FORMAT, Shape::Linear);
    q(out, S_OPTION_NON_GIT, Shape::Linear);
    q(out, S_BINARY_BLOB, Shape::Linear);

    // Refusals.
    q(out, S_GETMARK_UNDECLARED, Shape::Linear);
    q(out, S_FEATURE_UNKNOWN, Shape::Linear);
    q(out, S_OPTION_UNKNOWN, Shape::Linear);
    q(out, S_MISSING_OID, Shape::Linear);
    q(out, S_BAD_REFNAME, Shape::Linear);

    // Two of them against a repository that already has a rich ref set, where
    // the deletion has something to delete and the gitlink lands beside real
    // subdirectories.
    q(out, S_RESET_TO_ZERO, Shape::Branched);
    q(out, S_GITLINK, Shape::Submodule);
}

// ---------------------------------------------------------------------------
// fast-import: the flags `archive_export.rs` does not spell
// ---------------------------------------------------------------------------

/// Storage, bookkeeping and resume options, all against [`FE_ALL`] so the
/// objects that must come out are already pinned by the round trip above and
/// only the layout around them may move.
///
/// The defect here is `--export-pack-edges=<file>`: stock exits 0 and creates
/// the file, and the port exits 128 with
/// `fatal: unsupported flag "--export-pack-edges=edges.txt" for a stream that
/// writes objects (this port writes loose objects, so there are no pack edges to
/// report)`. The state probe sees the file by name through
/// `status --porcelain -uall`, so the difference is both an exit code and a
/// missing path.
///
/// `--done` is the command-line half of the `feature done` contract: it makes
/// the terminator **mandatory**, so a stream that is otherwise complete and
/// simply ends is `fatal: stream ends early`. Run against [`FE_ALL`], which has
/// no `done`, and against [`FE_DONE`], which does — the pair is what says the
/// flag is read rather than ignored.
///
/// `--relative-marks` moves where `--export-marks` writes: measured on both
/// sides, `rel.marks` lands at `.git/info/fast-import/rel.marks` rather than at
/// the repository root, which the state probe sees as the *absence* of an
/// untracked file at the root.
fn fast_import_uncovered_flags(out: &mut Vec<Case>) {
    fn f(out: &mut Vec<Case>, args: &[&str], stream: &'static [u8]) {
        out.push(Case::with_stdin("fast-import", args, Shape::Linear, stream));
    }

    f(out, &["fast-import", "--quiet", "--done"], FE_ALL);
    f(out, &["fast-import", "--quiet", "--done"], FE_DONE);
    f(out, &["fast-import", "--stats"], FE_ALL);
    f(out, &["fast-import", "--quiet", "--max-pack-size=1k"], FE_ALL);
    f(out, &["fast-import", "--quiet", "--depth=1"], FE_ALL);
    f(out, &["fast-import", "--quiet", "--active-branches=1"], FE_ALL);
    f(out, &["fast-import", "--quiet", "--big-file-threshold=1"], FE_ALL);
    f(out, &["fast-import", "--quiet", "--allow-unsafe-features"], FE_ALL);

    // The defect.
    f(out, &["fast-import", "--quiet", "--export-pack-edges=edges.txt"], FE_ALL);

    // Marks, in the two placements and the two resume spellings.
    f(out, &["fast-import", "--quiet", "--relative-marks", "--export-marks=rel.marks"], FE_ALL);
    f(out, &["fast-import", "--quiet", "--export-marks=abs.marks"], FE_ALL);
    f(out, &["fast-import", "--quiet", "--import-marks-if-exists=nosuch.marks"], FE_ALL);
    // A marks file that opens and is not one — the `fast-import` counterpart of
    // defect 4 on the export side.
    f(out, &["fast-import", "--quiet", "--import-marks=README.md"], FE_ALL);
    f(out, &["fast-import", "--quiet", "--import-marks-if-exists=README.md"], FE_ALL);

    // Submodule rewriting, which needs a marks file per submodule and so is
    // only reachable on its refusal: `fatal: cannot read 'nosuch.marks'`.
    f(out, &["fast-import", "--quiet", "--rewrite-submodules-from=sub:nosuch.marks"], FE_ALL);
    f(out, &["fast-import", "--quiet", "--rewrite-submodules-to=sub:nosuch.marks"], FE_ALL);
}
