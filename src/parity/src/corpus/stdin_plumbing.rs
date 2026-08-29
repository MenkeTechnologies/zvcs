//! Differential corpus cases for the plumbing whose entire input arrives on
//! **stdin**, or whose output is a pure transformation of the bytes it was fed.
//!
//! Every case here is compared against stock git for stdout, exit code and
//! post-command repository state.
//!
//! # Why this module exists
//!
//! `corpus/plumbing_objects.rs` opens with a standing limitation: the runner
//! used to spawn both sides with `Stdio::null()`, so `mktree`, `unpack-objects`,
//! `stripspace`, `patch-id`, `column`, `diff-pairs`, `fmt-merge-msg` and the
//! `--stdin`/`--stdin-paths` modes of `hash-object` were measured on the
//! *empty-input* path only — argument parsing, the zero-object result, and the
//! early-EOF error. `Case::stdin` closed that hole and `corpus.rs::stdin_driven`
//! took the first pass over it, one or two payloads per command. This module is
//! the depth pass: the parse paths, the separator modes, the filter paths and
//! the refusals that only a real payload reaches.
//!
//! # Payloads are literals, and what that forbids
//!
//! Every byte fed to a case below is a `&'static [u8]` compiled into this file.
//! A case that read its input off the filesystem at generation time would not
//! replay from its id, and a case that cannot be replayed cannot be the premise
//! of a differential comparison (see [`Case::stdin`]'s own documentation).
//!
//! Three consequences, all of which cost coverage and none of which are worked
//! around by faking the input:
//!
//! * **Object ids in a payload must be constants of the hash function.** Only
//!   two are: the empty blob `e69de29bb2d1d6434b8b29ae775ad8c2e48c5391` and the
//!   empty tree `4b825dc642cb6eb9a060e54bf8d69288fbee4904`, plus the all-zero
//!   oid. A fixture's own blob id is a fact about that fixture, not about git,
//!   and hard-coding one would break the moment `fixture.rs` changes a byte.
//! * **The empty blob is in no fixture.** So `mktree` needs `--missing` to
//!   accept an entry naming it, and `diff-pairs` can only be asked for the
//!   formats that do *not* open a blob — `--raw`, `--name-status`,
//!   `--name-only`, `--summary`. Asking for `-p`/`--stat`/`--numstat` is a
//!   deterministic `fatal: unable to read <oid>`, kept as a refusal rather than
//!   dressed up as a content comparison.
//! * **A pack, however, *can* be a literal.** [`ONE_OBJECT_PACK`] is 69 real
//!   bytes — header, two deflated blobs, trailing checksum — produced once by
//!   stock `pack-objects` and pasted in. That turns `unpack-objects` from an
//!   error-path-only command into one whose success path is provable: the two
//!   blobs it contains are absent from every fixture, so
//!   `cat-file --batch-check --batch-all-objects` shows them appear under a real
//!   run and stay absent under `-n`.
//!
//! # The malformed-input path is most of the contract here
//!
//! For this family a refusal is not an edge case, it is the specification. A
//! tree entry with an unparsable oid, a raw-diff record with no NUL after the
//! status letter, a `FETCH_HEAD` line with one tab instead of two, a pack whose
//! trailing checksum is off by one byte — each is a documented `die()` with a
//! documented exit code, and a port that accepts any of them silently produces
//! objects git would refuse. So the error-path share of this module is
//! deliberately well above the corpus-wide guideline, and the refusals use
//! [`Case::strict`] so the exit code *and* the message are both pinned.
//!
//! `usage:` blocks are the exception and are never `strict`: the harness's
//! standing policy is that error prose is outside the compatibility surface, and
//! a `parse_options()` usage dump is prose that tracks git's own option table.
//! A one-line `fatal:`/`error:` diagnostic is pinned; a usage block is not.
//!
//! # The separator modes are the point
//!
//! `mktree -z`, `hash-object --stdin-paths` and `diff-pairs -z` all read
//! NUL-terminated records, and the failure this module exists to catch is a
//! reader that splits on `\n` anyway — or one that splits on NUL when it was not
//! asked to. Stock git 2.55.0, measured:
//!
//! | payload | `mktree --missing` | `mktree --missing -z` |
//! |---|---|---|
//! | one LF-terminated entry | `f93e3a1a…` (path `README.md`) | `25db6398…` (path `README.md\n`) |
//! | two NUL-terminated entries | `f93e3a1a…` (one entry, rest dropped at the NUL) | `9262b84f…` (both entries) |
//!
//! Four distinct tree ids from two payloads and one flag. A reader that ignores
//! the flag agrees on at most half of them, and every one of those disagreements
//! is a *silently different object id*, not a crash.
//!
//! # What is not measured, and why
//!
//! * **`cherry`'s `-` output.** `cherry` classifies a commit by patch id and
//!   prints `-` when the upstream already contains an equivalent patch. No
//!   fixture shape carries a cherry-picked commit — every branch's work is
//!   unique — so only the `+` branch of `builtin/log.c:cmd_cherry` is reachable.
//!   Building a shape that has one is `fixture.rs`'s call, not this module's.
//! * **`fmt-merge-msg`'s message body.** A `FETCH_HEAD` line names a commit by
//!   oid, and the only oids expressible as literals resolve to nothing (or to a
//!   blob). `builtin/fmt-merge-msg.c` skips a line whose object is not a commit,
//!   so every well-formed payload here exits 0 with empty stdout. What that
//!   still measures is the *parser* — which line shapes are accepted and which
//!   are `fatal: error in line 1` — which was the half that shipped wrong.
//! * **`pack-refs`'s packed-refs file.** `probe_state` reads `for-each-ref`,
//!   which answers the same whether a ref is loose or packed. So these cases
//!   prove no ref was lost or invented; they cannot prove which file it ended up
//!   in.

use crate::fixture::Shape;
use crate::runner::Case;

// ---------------------------------------------------------------------------
// Payload literals
// ---------------------------------------------------------------------------
//
// The two oids that are constants of SHA-1 rather than of a fixture, spelled out
// here once so every payload below can be read against them:
//   empty blob  e69de29bb2d1d6434b8b29ae775ad8c2e48c5391
//   empty tree  4b825dc642cb6eb9a060e54bf8d69288fbee4904

/// One well-formed tree entry naming the empty blob, LF-terminated.
const MK_ONE: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tREADME.md\n";

/// Every entry kind a tree can hold: regular file, executable, symlink,
/// subtree, and a gitlink. Already in git's sort order, so the case measures
/// serialization rather than sorting.
const MK_MANY: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tREADME.md\n\
100755 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\trun.sh\n\
120000 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tlink\n\
040000 tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\tdir\n\
160000 commit 0000000000000000000000000000000000000000\tsub\n";

/// Two entries, NUL-terminated — what `-z` expects and what a newline reader
/// swallows whole.
const MK_Z: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tREADME.md\0\
100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tzeta.txt\0";

/// Entries in the wrong order. `builtin/mktree.c` sorts before writing, so this
/// is not an error — it is a *different tree id* for anyone who does not sort.
const MK_UNSORTED: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tzeta.txt\n\
100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\talpha.txt\n";

/// The same name twice. git writes both entries and produces a tree `fsck`
/// would reject; it does not deduplicate and it does not refuse.
const MK_DUP: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tdup.txt\n\
100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tdup.txt\n";

/// Mode `100664`, which is not one of the five modes a tree may carry. git
/// canonicalizes it to `100644` rather than refusing.
const MK_BAD_MODE: &[u8] = b"100664 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tREADME.md\n";

/// An oid that is not hex of the right length.
const MK_BAD_OID: &[u8] = b"100644 blob notanoid\tREADME.md\n";

/// A space where the tab between oid and path belongs.
const MK_NO_TAB: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 README.md\n";

/// Two trees separated by a blank line: `--batch` input, and outside `--batch`
/// the `(blank line only valid in batch mode)` refusal.
const MK_BATCH: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\ta.txt\n\n\
100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tb.txt\n";

/// The same two trees with NUL terminators, so what separates them is an empty
/// NUL-terminated record rather than an empty line.
const MK_BATCH_Z: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\ta.txt\0\0\
100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tb.txt\0";

/// A one-file unified diff, no commit header.
const PATCH_ONE: &[u8] = b"diff --git a/README.md b/README.md\n\
index 0000000..1111111 100644\n\
--- a/README.md\n\
+++ b/README.md\n\
@@ -1 +1,2 @@\n\
\x20# fixture\n\
+added line\n";

/// Two files under a commit header. The header's oid is what `patch-id` prints
/// as its second column, and *two* files is the minimum for `--stable` and the
/// default `--unstable` to disagree — the whole reason the flag exists.
const PATCH_MULTI: &[u8] = b"commit 1111111111111111111111111111111111111111\n\
Author: A U Thor <author@example.invalid>\n\
\n\
    two files\n\
\n\
diff --git a/README.md b/README.md\n\
index 0000000..1111111 100644\n\
--- a/README.md\n\
+++ b/README.md\n\
@@ -1 +1,2 @@\n\
\x20# fixture\n\
+added line\n\
diff --git a/src/lib.rs b/src/lib.rs\n\
index 2222222..3333333 100644\n\
--- a/src/lib.rs\n\
+++ b/src/lib.rs\n\
@@ -1 +1,2 @@\n\
\x20pub fn one() -> u32 { 1 }\n\
+pub fn two() -> u32 { 2 }\n";

/// A rename with an edit: the header carries `rename from`/`rename to` lines as
/// well as `---`/`+++`, and `builtin/patch-id.c` hashes both names.
const PATCH_RENAME: &[u8] = b"diff --git a/old/name.txt b/new/name.txt\n\
similarity index 87%\n\
rename from old/name.txt\n\
rename to new/name.txt\n\
index 4444444..5555555 100644\n\
--- a/old/name.txt\n\
+++ b/new/name.txt\n\
@@ -1,2 +1,2 @@\n\
\x20keep this line\n\
-drop this line\n\
+add this line\n";

/// A binary patch: no hunk header at all, just git's base85 literal blocks.
const PATCH_BINARY: &[u8] = b"diff --git a/data.bin b/data.bin\n\
index 6666666..7777777 100644\n\
GIT binary patch\n\
literal 4\n\
LcmZQzU|;|M0RaFA\n\
\n\
literal 0\n\
HcmV?d00001\n\n";

/// [`PATCH_ONE`] with CRLF line endings. `builtin/patch-id.c` strips the CR
/// before hashing unless `--verbatim` is given, so this payload and
/// [`PATCH_ONE`] must produce the *same* id — except under `--verbatim`, where
/// they must differ.
const PATCH_CRLF: &[u8] = b"diff --git a/README.md b/README.md\r\n\
index 0000000..1111111 100644\r\n\
--- a/README.md\r\n\
+++ b/README.md\r\n\
@@ -1 +1,2 @@\r\n\
\x20# fixture\r\n\
+added line\r\n";

/// Prose. `patch-id` finds no diff and prints nothing, at exit 0 — a different
/// behaviour from refusing, and one a port is likely to get wrong in the
/// direction of an error.
const NOT_A_PATCH: &[u8] = b"this is not a patch at all\njust prose\n";

/// Leading blanks, trailing blanks, runs of blank lines, trailing whitespace on
/// a content line, a comment line, and whitespace-only lines.
const MESSY: &[u8] = b"\n\n  subject line   \n\n\n\nbody   text\n# a comment\n\t\n   \n\nlast\n\n\n";

/// Four plain lines, for `--comment-lines` and for `column`.
const PLAIN_LINES: &[u8] = b"plain\nlines\nof\ntext\n";

/// Content with CRLF endings. The CR is trailing whitespace to
/// `strbuf_rtrim()`, so `stripspace` removes it — a port that trims only spaces
/// and tabs leaves it behind.
const CRLF_TEXT: &[u8] = b"one\r\ntwo   \r\n\r\n\r\nthree\r\n";

/// Nothing but whitespace: the whole input strips to zero bytes.
const WS_ONLY: &[u8] = b"   \n\t\n \t \n";

/// A single line with no trailing newline; `stripspace` supplies one.
const NO_EOL_TEXT: &[u8] = b"no trailing newline here";

/// A message carrying both a `;` line and a `#` line, so which of the two is a
/// comment depends entirely on `core.commentChar`/`core.commentString`.
const TWO_COMMENT_STYLES: &[u8] = b"subject\n\n; semicolon comment\n# hash comment\nbody\n";

/// A scissors line and the boilerplate git writes under it. `stripspace` has no
/// scissors handling of its own — the lines survive or not purely as comments —
/// which is exactly the confusion worth pinning.
const SCISSORS: &[u8] = b"message body\n\
# ------------------------ >8 ------------------------\n\
# Do not touch the line above.\n\
# Everything below will be removed.\n\
diff --git a/x b/x\n";

/// Sixteen short words, one per line: enough to fill several columns at both the
/// default width and at `--width=40`, so the layout differs between them.
const WORDS: &[u8] = b"alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot\ngolf\nhotel\n\
india\njuliett\nkilo\nlima\nmike\nnovember\noscar\npapa\n";

/// The same idea with CRLF endings — `column` strips the CR, so the column width
/// is computed from the visible text and not from the CR.
const WORDS_CRLF: &[u8] = b"alpha\r\nbravo\r\ncharlie\r\n";

/// NUL-separated words fed to a command that has no `-z`: the whole payload is
/// one line, and only the bytes before the first NUL reach the terminal.
const WORDS_NUL: &[u8] = b"alpha\0bravo\0charlie\0";

/// Lines of unequal length, one containing an embedded tab.
const WORDS_MIXED: &[u8] = b"one two\tthree\nlonger entry here\nx\n";

/// A blob whose id is in no fixture, so `-w` writing it is visible in the state
/// probe. `hash-object --stdin` on these bytes is `038f48ad…`.
const HELLO_BLOB: &[u8] = b"hello blob\n";

/// The same content with CRLF endings, for the `--path=` attribute path: under
/// `* text=auto` git normalizes it on check-in, so the id depends on which path
/// the bytes claim to have come from.
const CRLF_BLOB: &[u8] = b"line one\r\nline two\r\n";

/// Immediate EOF. Distinct from closed stdin for anything that tells "no
/// payload" from "no pipe", and for `hash-object --stdin` it is the one input
/// whose answer is a constant of the hash function.
const EMPTY: &[u8] = b"";

/// Three paths for `--stdin-paths`, the last of which does not exist — so the
/// case pins both the ids of the first two *and* that git fails after printing
/// them rather than before.
const PATHS_LF: &[u8] = b"README.md\nsrc/lib.rs\nno/such/file\n";

/// The same paths NUL-separated. `hash-object --stdin-paths` has no `-z`, so git
/// reads one line and opens the bytes up to the first NUL.
const PATHS_NUL: &[u8] = b"README.md\0src/lib.rs\0";

/// A real packfile, 69 bytes, holding two blobs: `parity pack payload\n`
/// (`56529051d3b2f2d729ca211ced4750974e4bc4b1`) and the empty blob. Produced
/// once by stock `git pack-objects --stdout` and pasted here.
///
/// Embeddable precisely because it is small: `PACK`, version 2, object count 2,
/// two zlib streams, and a 20-byte SHA-1 trailer over everything before it.
/// Neither blob is in any fixture shape, so `cat-file --batch-check
/// --batch-all-objects` distinguishes a run that unpacked them from one that
/// only claimed to.
const ONE_OBJECT_PACK: &[u8] = b"\x50\x41\x43\x4b\x00\x00\x00\x02\x00\x00\x00\x02\
\xb4\x01\x78\x9c\x2b\x48\x2c\xca\x2c\xa9\x54\x28\
\x48\x4c\xce\x06\x12\x95\x39\xf9\x89\x29\x5c\x00\
\x51\xb0\x07\x6d\x30\x78\x9c\x03\x00\x00\x00\x00\
\x01\xc0\x3e\x56\x1f\x71\x53\x50\x68\xbd\xa5\x30\
\x89\x72\x3e\x66\x62\xa3\xd8\xfe\xb2";

/// [`ONE_OBJECT_PACK`] with the last ten bytes of its trailing checksum cut
/// off. Both objects are complete, so the die is `fatal: early EOF` reading the
/// trailer — and, measured against stock 2.55.0, both blobs are *still in the
/// object store afterwards*: `builtin/unpack-objects.c` writes each object as it
/// reads it and has no rollback. A port that buffers the pack and commits only
/// on success agrees on the exit code and the message and diverges on state,
/// which is exactly the divergence the probe is here to catch.
const TRUNCATED_PACK: &[u8] = b"\x50\x41\x43\x4b\x00\x00\x00\x02\x00\x00\x00\x02\
\xb4\x01\x78\x9c\x2b\x48\x2c\xca\x2c\xa9\x54\x28\
\x48\x4c\xce\x06\x12\x95\x39\xf9\x89\x29\x5c\x00\
\x51\xb0\x07\x6d\x30\x78\x9c\x03\x00\x00\x00\x00\
\x01\xc0\x3e\x56\x1f\x71\x53\x50\x68\xbd\xa5";

/// [`ONE_OBJECT_PACK`] with the last byte of the trailing checksum zeroed. Every
/// object in it is intact and parseable; only the trailer disagrees, so this
/// separates a reader that verifies the checksum from one that stops at the last
/// object.
const BAD_CHECKSUM_PACK: &[u8] = b"\x50\x41\x43\x4b\x00\x00\x00\x02\x00\x00\x00\x02\
\xb4\x01\x78\x9c\x2b\x48\x2c\xca\x2c\xa9\x54\x28\
\x48\x4c\xce\x06\x12\x95\x39\xf9\x89\x29\x5c\x00\
\x51\xb0\x07\x6d\x30\x78\x9c\x03\x00\x00\x00\x00\
\x01\xc0\x3e\x56\x1f\x71\x53\x50\x68\xbd\xa5\x30\
\x89\x72\x3e\x66\x62\xa3\xd8\xfe\x00";

/// Text where a pack header belongs.
const NOT_A_PACK: &[u8] = b"this is not the binary format you are looking for\n";

/// One raw-diff record: an addition of the empty blob. The grammar is
/// `:<mode1> <mode2> <oid1> <oid2> <status>\0<path>[\0<path2>]\0`.
const DP_ADD: &[u8] =
    b":000000 100644 0000000000000000000000000000000000000000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 A\0new.txt\0";

/// A rename record, which carries *two* paths in one record — the shape a reader
/// that assumes one path per record gets wrong.
const DP_RENAME: &[u8] =
    b":100644 100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 R100\0old.txt\0new.txt\0";

/// Three records back to back: an add, a delete, and a typechange to a symlink.
const DP_MULTI: &[u8] =
    b":000000 100644 0000000000000000000000000000000000000000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 A\0new.txt\0\
:100644 000000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0000000000000000000000000000000000000000 D\0gone.txt\0\
:100644 120000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 T\0link.txt\0";

/// The same record with newlines where the NULs belong.
const DP_LF: &[u8] =
    b":000000 100644 0000000000000000000000000000000000000000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 A\nnew.txt\n";

/// A status letter git does not define.
const DP_BAD_STATUS: &[u8] =
    b":000000 100644 0000000000000000000000000000000000000000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 X\0new.txt\0";

/// An unparsable oid in the post-image slot.
const DP_BAD_OID: &[u8] = b":000000 100644 0000000000000000000000000000000000000000 notanoid A\0new.txt\0";

/// A record whose path is missing: the NUL after the status is the last byte.
const DP_NO_PATH: &[u8] =
    b":000000 100644 0000000000000000000000000000000000000000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 A\0";

/// A record with no leading `:`.
const DP_NO_COLON: &[u8] =
    b"000000 100644 0000000000000000000000000000000000000000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 A\0new.txt\0";

/// A `FETCH_HEAD` line: `<oid>\t<not-for-merge>\t<description>`.
const FMM_BRANCH: &[u8] =
    b"0000000000000000000000000000000000000000\t\tbranch 'feature' of ../upstream\n";

/// Two mergeable lines.
const FMM_TWO: &[u8] = b"0000000000000000000000000000000000000000\t\tbranch 'feature' of ../upstream\n\
0000000000000000000000000000000000000000\t\tbranch 'other' of ../upstream\n";

/// A `not-for-merge` line ahead of a mergeable one: the middle field is the
/// filter, and a reader that ignores it merges a branch git would skip.
const FMM_NOT_FOR_MERGE: &[u8] =
    b"0000000000000000000000000000000000000000\tnot-for-merge\tbranch 'skipme' of ../upstream\n\
0000000000000000000000000000000000000000\t\tbranch 'feature' of ../upstream\n";

/// A tag rather than a branch — a different description grammar in
/// `builtin/fmt-merge-msg.c`'s `handle_line()`.
const FMM_TAG: &[u8] = b"0000000000000000000000000000000000000000\t\ttag 'v0.1.0' of ../upstream\n";

/// An abbreviated oid, which `get_oid_hex()` refuses.
const FMM_SHORT_OID: &[u8] = b"deadbeef\t\tbranch 'feature' of ../upstream\n";

/// One tab where two belong: the description lands in the not-for-merge slot.
const FMM_ONE_TAB: &[u8] =
    b"0000000000000000000000000000000000000000\tbranch 'feature' of ../upstream\n";

/// A well-formed line terminated with CRLF.
const FMM_CRLF: &[u8] =
    b"0000000000000000000000000000000000000000\t\tbranch 'feature' of ../upstream\r\n";

/// Text that is not a `FETCH_HEAD` line.
const FMM_JUNK: &[u8] = b"not a fetch-head line\n";

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Push one stdin-fed case.
fn si(cmd: &'static str, args: &[&str], shape: Shape, input: &'static [u8], out: &mut Vec<Case>) {
    out.push(Case::with_stdin(cmd, args, shape, input));
}

/// Push one stdin-fed case with stderr compared byte for byte.
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
    mktree(out);
    patch_id(out);
    stripspace(out);
    column(out);
    hash_object(out);
    unpack_objects(out);
    diff_pairs(out);
    fmt_merge_msg(out);
    ref_and_env_plumbing(out);
}

// ---------------------------------------------------------------------------
// mktree
// ---------------------------------------------------------------------------

/// `mktree`: text on stdin in, one tree id out — and the tree itself written to
/// the object store, which `cat-file --batch-check --batch-all-objects` in
/// `probe_state` verifies. `builtin/mktree.c` always calls `write_object_file()`;
/// there is no dry-run mode, so an id printed without an object behind it is a
/// state divergence and not merely a stdout one.
///
/// What is measured, in order of what a port is most likely to get wrong: the
/// separator flag (see the table in the module header), the sort git applies
/// before serializing, the mode canonicalization, and the three `die()`s.
fn mktree(out: &mut Vec<Case>) {
    // --missing is required throughout: the empty blob is a constant of SHA-1
    // but is an object in no fixture, and without the flag every entry naming it
    // is `fatal: entry '<path>' object <oid> is unavailable`.
    si("mktree", &["mktree", "--missing"], Shape::Linear, MK_ONE, out);
    si_strict("mktree", &["mktree"], Shape::Linear, MK_ONE, out);

    // All five entry kinds a tree may hold, including the two that are not
    // blobs: a subtree (mode 040000, type `tree`) and a gitlink (160000,
    // `commit`). Both are parsed by name in `mktree.c:mktree_line`, so a port
    // that only knows `blob` fails here and nowhere else.
    si("mktree", &["mktree", "--missing"], Shape::Linear, MK_MANY, out);

    // The separator cross-product. Each of the four combinations produces a
    // different tree id under stock 2.55.0, so agreeing on one proves nothing
    // about the others.
    si("mktree", &["mktree", "--missing", "-z"], Shape::Linear, MK_Z, out);
    si("mktree", &["mktree", "--missing"], Shape::Linear, MK_Z, out);
    si("mktree", &["mktree", "--missing", "-z"], Shape::Linear, MK_ONE, out);

    // git sorts entries itself (`mktree.c` qsorts on `ent_compare` before
    // writing), so unsorted input is accepted and the *id* is the assertion — a
    // port that writes them in arrival order prints a different, valid-looking
    // oid and no error at all.
    si("mktree", &["mktree", "--missing"], Shape::Linear, MK_UNSORTED, out);

    // Neither duplicates nor a non-canonical mode is refused: git writes both
    // entries, and rewrites 100664 to 100644. Both produce objects `fsck` would
    // reject, which is the documented behaviour and therefore the parity target.
    si("mktree", &["mktree", "--missing"], Shape::Linear, MK_DUP, out);
    si("mktree", &["mktree", "--missing"], Shape::Linear, MK_BAD_MODE, out);

    // The two `input format error` refusals, message pinned.
    si_strict("mktree", &["mktree", "--missing"], Shape::Linear, MK_BAD_OID, out);
    si_strict("mktree", &["mktree", "--missing"], Shape::Linear, MK_NO_TAB, out);

    // --batch: a blank record ends one tree and starts the next, so this prints
    // two ids. Outside --batch the same payload is a refusal naming the mode.
    si("mktree", &["mktree", "--missing", "--batch"], Shape::Linear, MK_BATCH, out);
    si("mktree", &["mktree", "--missing", "--batch", "-z"], Shape::Linear, MK_BATCH_Z, out);
    si_strict("mktree", &["mktree", "--missing"], Shape::Linear, MK_BATCH, out);
}

// ---------------------------------------------------------------------------
// patch-id
// ---------------------------------------------------------------------------

/// `patch-id`: a diff on stdin, a stable hash of its *content* on stdout. The
/// second column is the commit id lifted from a `commit <oid>` header line, or
/// the null oid when the payload has no header.
///
/// The three modes are three different hashes of the same bytes and every one of
/// them needs a real patch to be reached at all:
///   * `--unstable` (the default) hashes the files in the order they appear;
///   * `--stable` sorts the per-file digests first, so it agrees with itself
///     across a reordered patch — visible only on a payload with two files;
///   * `--verbatim` hashes the raw lines without stripping whitespace, so it is
///     the only mode in which the CRLF payload differs from the LF one.
fn patch_id(out: &mut Vec<Case>) {
    // Two files, so --stable and the default have something to disagree about.
    // Measured under stock 2.55.0: 292bc8c4… default, 9fa0b046… stable.
    si("patch-id", &["patch-id"], Shape::Linear, PATCH_MULTI, out);
    si("patch-id", &["patch-id", "--stable"], Shape::Linear, PATCH_MULTI, out);
    si("patch-id", &["patch-id", "--verbatim"], Shape::Linear, PATCH_MULTI, out);

    // A rename header: the `rename from`/`rename to` pair names paths that the
    // `---`/`+++` lines do not, and both go into the hash.
    si("patch-id", &["patch-id"], Shape::Linear, PATCH_RENAME, out);

    // A binary patch has no `@@` line; the base85 blocks are hashed as content,
    // and the two modes disagree here as well.
    si("patch-id", &["patch-id"], Shape::Linear, PATCH_BINARY, out);
    si("patch-id", &["patch-id", "--stable"], Shape::Linear, PATCH_BINARY, out);

    // The CRLF triple. Bare must equal the id of the LF payload (473e9b0c…);
    // --verbatim must not (695ed76c… against daf01456…).
    si("patch-id", &["patch-id"], Shape::Linear, PATCH_CRLF, out);
    si("patch-id", &["patch-id", "--verbatim"], Shape::Linear, PATCH_CRLF, out);
    si("patch-id", &["patch-id", "--verbatim"], Shape::Linear, PATCH_ONE, out);

    // Prose contains no diff: nothing printed, exit 0. This is the answer a port
    // most often turns into an error.
    si("patch-id", &["patch-id"], Shape::Linear, NOT_A_PATCH, out);
}

// ---------------------------------------------------------------------------
// stripspace
// ---------------------------------------------------------------------------

/// `stripspace`: the whitespace and comment normalizer every commit message
/// passes through. Pure stdin to stdout, so stdout *is* the whole assertion.
///
/// Two axes a port routinely gets wrong:
///   * **What counts as trailing whitespace.** git calls `strbuf_rtrim()`, which
///     trims on `isspace()` — so a CR at end of line goes with it. Trimming only
///     spaces and tabs leaves a stray CR on every line of a CRLF message.
///   * **What counts as a comment.** `core.commentChar` and its successor
///     `core.commentString` decide, and since git 2.45 a multi-character value
///     is accepted in *both* keys. A port that hard-codes `#` passes every
///     default-configuration case and fails every configured one.
fn stripspace(out: &mut Vec<Case>) {
    // The default: blank-line runs collapse, leading/trailing blanks go,
    // trailing whitespace goes, comment lines stay — and `-s` removes them.
    si("stripspace", &["stripspace"], Shape::Linear, MESSY, out);
    si("stripspace", &["stripspace", "-s"], Shape::Linear, MESSY, out);

    // -c is the inverse: every line gains the comment prefix and a space.
    si("stripspace", &["stripspace", "-c"], Shape::Linear, PLAIN_LINES, out);

    // The three degenerate inputs.
    si("stripspace", &["stripspace"], Shape::Linear, CRLF_TEXT, out);
    si("stripspace", &["stripspace"], Shape::Linear, WS_ONLY, out);
    si("stripspace", &["stripspace"], Shape::Linear, NO_EOL_TEXT, out);

    // A scissors line is only ever a comment to `stripspace` — the cut is
    // `commit --cleanup=scissors`'s job, not this command's.
    si("stripspace", &["stripspace", "-s"], Shape::Linear, SCISSORS, out);

    // Which of `;` and `#` is the comment is decided by configuration, so the
    // same payload strips a different line under this setting than by default.
    out.push(
        Case::with_stdin("stripspace", &["stripspace", "-s"], Shape::Linear, TWO_COMMENT_STYLES)
            .with_config(&[("core.commentChar", ";")]),
    );
    // A multi-byte comment string, only expressible since the key stopped being
    // a single character.
    out.push(
        Case::with_stdin("stripspace", &["stripspace", "-c"], Shape::Linear, PLAIN_LINES)
            .with_config(&[("core.commentString", "§§")]),
    );
    // `core.commentChar` holding more than one character: accepted in 2.55 and
    // treated as the string, where an older single-char parser refuses it.
    out.push(
        Case::with_stdin("stripspace", &["stripspace", "-c"], Shape::Linear, PLAIN_LINES)
            .with_config(&[("core.commentChar", "bad")]),
    );

    // The mutual exclusion, message pinned; `parse_options` emits the one error
    // line and no usage block for this one.
    si_strict("stripspace", &["stripspace", "-s", "-c"], Shape::Linear, PLAIN_LINES, out);
}

// ---------------------------------------------------------------------------
// column
// ---------------------------------------------------------------------------

/// `column`: lines on stdin, a laid-out grid on stdout. Every byte of the output
/// is a function of the payload and the layout parameters, so nothing about it
/// is reachable with stdin closed — which is how the existing cases in
/// `corpus/worktree_index.rs` ran.
///
/// `--width` is pinned on most cases on purpose. Left unset, `column` asks the
/// terminal, and both sides run without one — so the answer is git's 80-column
/// fallback, and a port that fell back to something else would show up as a diff
/// that has nothing to do with the layout algorithm. Pinning the width makes the
/// layout the only variable; one unpinned case is kept to measure the fallback.
fn column(out: &mut Vec<Case>) {
    // The three fill orders over one payload: `column` fills down then across,
    // `row` fills across then down, `dense` computes the column width per column
    // rather than globally.
    si("column", &["column", "--mode=column", "--width=40"], Shape::Linear, WORDS, out);
    si("column", &["column", "--mode=row", "--width=40"], Shape::Linear, WORDS, out);
    si("column", &["column", "--mode=dense", "--width=40"], Shape::Linear, WORDS, out);
    // The unpinned width: the no-terminal fallback, which is 80.
    si("column", &["column", "--mode=column"], Shape::Linear, WORDS, out);

    // The three layout parameters, each changing the output in a way the others
    // cannot fake.
    si("column", &["column", "--mode=row", "--width=40", "--padding=3"], Shape::Linear, WORDS, out);
    si("column", &["column", "--mode=column", "--width=40", "--indent=>>"], Shape::Linear, WORDS, out);
    si("column", &["column", "--mode=column", "--width=40", "--nl=|"], Shape::Linear, WORDS, out);

    // Payload shapes: a CR that must not count toward the column width, a NUL
    // that ends the printed text mid-line, and lines of unequal length one of
    // which contains a tab.
    si("column", &["column", "--mode=column", "--width=40"], Shape::Linear, WORDS_CRLF, out);
    si("column", &["column", "--mode=column", "--width=40"], Shape::Linear, WORDS_NUL, out);
    si("column", &["column", "--mode=column", "--width=40"], Shape::Linear, WORDS_MIXED, out);

    // Configuration: `--command=<name>` names the `column.<name>` key, which
    // layers over `column.ui`. Two cases — the specific key alone, and both,
    // where the specific one wins.
    out.push(
        Case::with_stdin("column", &["column", "--command=tag", "--width=40"], Shape::Linear, WORDS)
            .with_config(&[("column.tag", "always,dense")]),
    );
    out.push(
        Case::with_stdin("column", &["column", "--command=tag", "--width=40"], Shape::Linear, WORDS)
            .with_config(&[("column.ui", "always"), ("column.tag", "row")]),
    );
}

// ---------------------------------------------------------------------------
// hash-object
// ---------------------------------------------------------------------------

/// `hash-object`: the id oracle, driven from stdin in its two modes.
///
/// `--stdin` hashes the payload; `--stdin-paths` reads *file names* from the
/// payload and hashes what they name. They are mutually exclusive, and the
/// separator rules differ from every other `--stdin` in git: there is no `-z`,
/// so a NUL in a path record is not a separator but a terminator of the name git
/// then tries to `open()`.
///
/// The `-w` cases are the ones the state probe carries: the blob has to be in
/// the object store afterwards, not merely printed.
fn hash_object(out: &mut Vec<Case>) {
    // The floor: stdin bytes to an id, and the same bytes written.
    si("hash-object", &["hash-object", "--stdin"], Shape::Linear, HELLO_BLOB, out);
    si("hash-object", &["hash-object", "-w", "-t", "blob", "--stdin"], Shape::Linear, HELLO_BLOB, out);
    // Empty input: the one id that is a constant of the hash function, written.
    si("hash-object", &["hash-object", "-w", "--stdin"], Shape::Linear, EMPTY, out);

    // `-t` against content that is not that type. Without `--literally` git runs
    // the payload through `fsck` and refuses; with it, any bytes get any type.
    si("hash-object", &["hash-object", "-t", "tree", "--stdin"], Shape::Linear, HELLO_BLOB, out);
    si("hash-object", &["hash-object", "-t", "tree", "--literally", "--stdin"], Shape::Linear, HELLO_BLOB, out);

    // `--` ends the option list, so after it `--stdin` is a *file name*: the
    // payload is ignored and the command fails on a missing file.
    si_strict("hash-object", &["hash-object", "--", "--stdin"], Shape::Linear, HELLO_BLOB, out);

    // Attribute-driven conversion. On Shape::Attributes `* text=auto` and
    // `*.rs text eol=lf` are in force, so identical bytes hash differently
    // depending on which path `--path=` claims they came from: `src/x.rs` is
    // normalized to LF (e5c5c558…), `assets/x.bin` is `binary` and is not
    // (cf9b2a85…). Without `--path=` no lookup happens at all, so that case must
    // equal the binary one.
    si("hash-object", &["hash-object", "--stdin", "--path=src/x.rs"], Shape::Attributes, CRLF_BLOB, out);
    si("hash-object", &["hash-object", "--stdin", "--path=assets/x.bin"], Shape::Attributes, CRLF_BLOB, out);
    si("hash-object", &["hash-object", "--stdin"], Shape::Attributes, CRLF_BLOB, out);
    // `-w` with a path whose subtree `.gitattributes` overrides the root one;
    // the state probe carries whether the normalized blob is the one stored.
    si("hash-object", &["hash-object", "-w", "--stdin", "--path=sub/nested.txt"], Shape::Attributes, CRLF_BLOB, out);

    // --stdin-paths: names, not content. The LF payload's third name does not
    // exist, so git prints two ids and *then* dies — a port that validates the
    // whole list first prints nothing and diverges on stdout, not just on exit.
    si("hash-object", &["hash-object", "--stdin-paths"], Shape::Linear, PATHS_LF, out);
    // No `-z` exists here: the NUL payload is one line, and git opens the name
    // up to the first NUL — one id, exit 0.
    si("hash-object", &["hash-object", "--stdin-paths"], Shape::Linear, PATHS_NUL, out);
}

// ---------------------------------------------------------------------------
// unpack-objects
// ---------------------------------------------------------------------------

/// `unpack-objects`: a packfile on stdin, loose objects in the store afterwards.
///
/// This is the one command in the module whose success path was previously
/// unreachable. The empty pack `corpus.rs` uses proves only that a header
/// parses; [`ONE_OBJECT_PACK`] contains two blobs that exist in no fixture, so
/// `cat-file --batch-check --batch-all-objects` separates every outcome:
/// unpacked (both appear), dry-run (neither appears), refused (neither appears,
/// and the exit code says why).
///
/// Verified against stock 2.55.0: a plain run leaves
/// `56529051d3b2f2d729ca211ced4750974e4bc4b1` and
/// `e69de29bb2d1d6434b8b29ae775ad8c2e48c5391` in the store; `-n` leaves the
/// object list byte-identical to the fixture's.
fn unpack_objects(out: &mut Vec<Case>) {
    si("unpack-objects", &["unpack-objects"], Shape::Linear, ONE_OBJECT_PACK, out);
    si("unpack-objects", &["unpack-objects", "-q"], Shape::Linear, ONE_OBJECT_PACK, out);
    si("unpack-objects", &["unpack-objects", "-n"], Shape::Linear, ONE_OBJECT_PACK, out);
    // --strict runs fsck over what it unpacks; two blobs pass it.
    si("unpack-objects", &["unpack-objects", "--strict"], Shape::Linear, ONE_OBJECT_PACK, out);
    // Onto a shape that already has packs, where the loose copies land beside
    // packed objects rather than into a store that has none.
    si("unpack-objects", &["unpack-objects"], Shape::Packed, ONE_OBJECT_PACK, out);

    // The three ways the same 69 bytes can be wrong, each a distinct `fatal:`.
    // Two of them still leave objects behind — see [`TRUNCATED_PACK`] — so the
    // state probe carries as much of the assertion as the exit code does.
    si_strict("unpack-objects", &["unpack-objects"], Shape::Linear, TRUNCATED_PACK, out);
    si_strict("unpack-objects", &["unpack-objects"], Shape::Linear, BAD_CHECKSUM_PACK, out);
    si_strict("unpack-objects", &["unpack-objects"], Shape::Linear, NOT_A_PACK, out);
    // A size limit below the payload: refused before any object is written.
    si_strict("unpack-objects", &["unpack-objects", "--max-input-size=10"], Shape::Linear, ONE_OBJECT_PACK, out);
}

// ---------------------------------------------------------------------------
// diff-pairs
// ---------------------------------------------------------------------------

/// `diff-pairs`: raw diff records on stdin, formatted diff output on stdout.
/// Introduced so a caller can re-format a `diff-tree --raw -z` stream without
/// re-walking the trees, and `-z` is not optional — without it the command
/// refuses outright.
///
/// The record grammar is
/// `:<mode1> <mode2> <oid1> <oid2> <status>\0<path>[\0<path2>]\0`, with the
/// second path present only for `R`/`C`. Every field is a place to go wrong, and
/// the refusals below name four different ones.
///
/// The formats that open a blob are unreachable from a literal payload: the only
/// oids expressible here are the empty blob and the null oid, and the empty blob
/// is in no fixture, so `-p` is `fatal: unable to read <oid>`. It is kept as a
/// refusal rather than omitted, because agreeing on *which* oid is reported
/// unreadable is itself the parse result.
fn diff_pairs(out: &mut Vec<Case>) {
    // The formats that read only the record.
    si("diff-pairs", &["diff-pairs", "-z", "--raw"], Shape::Linear, DP_RENAME, out);
    si("diff-pairs", &["diff-pairs", "-z", "--raw"], Shape::Linear, DP_MULTI, out);
    si("diff-pairs", &["diff-pairs", "-z", "--name-status"], Shape::Linear, DP_MULTI, out);
    si("diff-pairs", &["diff-pairs", "-z", "--name-only"], Shape::Linear, DP_MULTI, out);
    // --summary reads the modes only, so an add, a delete and a typechange each
    // print a different line.
    si("diff-pairs", &["diff-pairs", "-z", "--summary"], Shape::Linear, DP_MULTI, out);

    // A content format: the parse succeeds and the blob read fails.
    si_strict("diff-pairs", &["diff-pairs", "-z", "-p"], Shape::Linear, DP_ADD, out);

    // Four malformed records, four distinct diagnostics.
    si_strict("diff-pairs", &["diff-pairs", "-z", "--raw"], Shape::Linear, DP_BAD_STATUS, out);
    si_strict("diff-pairs", &["diff-pairs", "-z", "--raw"], Shape::Linear, DP_BAD_OID, out);
    si_strict("diff-pairs", &["diff-pairs", "-z", "--raw"], Shape::Linear, DP_NO_PATH, out);
    si_strict("diff-pairs", &["diff-pairs", "-z", "--raw"], Shape::Linear, DP_NO_COLON, out);

    // Newlines where NULs belong: the record header parses, then the path read
    // runs to EOF. This is the exact failure a reader that splits on `\n`
    // produces on *correct* input, seen from the other side.
    si_strict("diff-pairs", &["diff-pairs", "-z"], Shape::Linear, DP_LF, out);
}

// ---------------------------------------------------------------------------
// fmt-merge-msg
// ---------------------------------------------------------------------------

/// `fmt-merge-msg`: a `FETCH_HEAD` file on stdin, a merge commit message on
/// stdout. What is measured here is the line parser, for the reason given in the
/// module header — the oids a literal may contain resolve to no commit, so
/// `builtin/fmt-merge-msg.c` accepts the line and then contributes nothing to
/// the message.
///
/// That still separates the two outcomes that matter: a line git *accepts*
/// (exit 0, empty stdout) from one it refuses (`fatal: error in line 1: …`, exit
/// 128). The refusals are pinned; the acceptances are pinned by exit code and by
/// the absence of output, which a port that refuses any of them fails.
fn fmt_merge_msg(out: &mut Vec<Case>) {
    // Description grammars `handle_line()` recognizes.
    si("fmt-merge-msg", &["fmt-merge-msg"], Shape::Branched, FMM_TWO, out);
    si("fmt-merge-msg", &["fmt-merge-msg"], Shape::Branched, FMM_NOT_FOR_MERGE, out);
    si("fmt-merge-msg", &["fmt-merge-msg"], Shape::Branched, FMM_TAG, out);

    // Terminator handling: a CRLF line is accepted, the CR not counted as part
    // of the description.
    si("fmt-merge-msg", &["fmt-merge-msg"], Shape::Branched, FMM_CRLF, out);

    // `-m` replaces the generated subject and does *not* preempt the parse: a
    // junk payload still dies even though the message was supplied.
    si("fmt-merge-msg", &["fmt-merge-msg", "-m", "custom"], Shape::Branched, FMM_BRANCH, out);
    si_strict("fmt-merge-msg", &["fmt-merge-msg", "-m", "custom"], Shape::Branched, FMM_JUNK, out);

    // `merge.log` decides whether the message carries a shortlog.
    out.push(
        Case::with_stdin("fmt-merge-msg", &["fmt-merge-msg"], Shape::Branched, FMM_BRANCH)
            .with_config(&[("merge.log", "5")]),
    );

    // Two malformed lines: an oid that is not full-length, and one tab where the
    // format wants two. Both are `fatal: error in line 1: <the line>`, so the
    // message quotes the payload back and pins the field split as well.
    si_strict("fmt-merge-msg", &["fmt-merge-msg"], Shape::Branched, FMM_SHORT_OID, out);
    si_strict("fmt-merge-msg", &["fmt-merge-msg"], Shape::Branched, FMM_ONE_TAB, out);
}

// ---------------------------------------------------------------------------
// The neighbours: cherry, merge-index, pack-refs, check-ref-format, var
// ---------------------------------------------------------------------------

/// Five commands that read no stdin but belong to the same contract: their whole
/// output is a transformation of text they were handed, and each is measured
/// here on the axis the existing corpus leaves open.
///
/// `cherry` is here because it is `patch-id`'s only in-tree consumer: it hashes
/// every commit on both sides and prints `+` for a patch the upstream lacks. If
/// the `patch-id` cases above pass and these do not, the divergence is in the
/// walk rather than in the hash.
fn ref_and_env_plumbing(out: &mut Vec<Case>) {
    // ---- cherry: the abbreviation axis, and an annotated tag as upstream ----
    // `corpus/history_rewrite.rs` pins only `--abbrev=8`; a shorter width and an
    // explicit 40 are the two ends of `find_unique_abbrev()`'s range.
    out.push(Case::new("cherry", &["cherry", "-v", "--abbrev=4", "main", "feature"], Shape::Branched));
    out.push(Case::new("cherry", &["cherry", "--abbrev=40", "-v", "main", "feature"], Shape::Branched));
    // An annotated tag as the upstream: the argument is peeled to a commit
    // before the walk starts.
    out.push(Case::new("cherry", &["cherry", "-v", "v0.2.0", "feature"], Shape::Branched));

    // ---- merge-index: the argument protocol and the failure propagation ----
    // A bare path operand with no `--`, which is a different branch of
    // `builtin/merge-index.c`'s argv walk than the `-a` and `--` forms already
    // covered in `corpus/merge_family.rs`.
    out.push(Case::new("merge-index", &["merge-index", "echo", "conflict.txt"], Shape::Conflicted));
    // `-q` without `-o`: `-q` only suppresses the message `-o` would let
    // through, so alone it changes nothing — a port that treats it as its own
    // quiet flag diverges.
    out.push(Case::new("merge-index", &["merge-index", "-q", "echo", "-a"], Shape::Conflicted));
    // A path that is not in the index at all is `fatal: … not in the cache`.
    out.push(Case::strict("merge-index", &["merge-index", "echo", "--", "no-such-file"], Shape::Conflicted));
    // A merge program that fails must fail the command.
    out.push(Case::strict("merge-index", &["merge-index", "false", "-a"], Shape::Conflicted));

    // ---- pack-refs: selection, measured by what for-each-ref still answers ----
    // `--auto` leaves the decision to the ref backend, so the assertion is that
    // both sides make the same one and lose nothing either way.
    out.push(Case::new("pack-refs", &["pack-refs", "--auto"], Shape::Branched));
    // `--include` and `--exclude` together, where the exclusion is a subset of
    // the inclusion.
    out.push(Case::new(
        "pack-refs",
        &["pack-refs", "--include", "refs/heads/*", "--exclude", "refs/heads/main"],
        Shape::Branched,
    ));
    // `--exclude` narrowing `--all`, the only way to pack heads and leave tags
    // loose.
    out.push(Case::new("pack-refs", &["pack-refs", "--all", "--exclude", "refs/tags/*"], Shape::Branched));

    // ---- check-ref-format: one case per illegal-name class ----
    // `refs.c:check_refname_format` rejects on nine distinct rules and
    // `corpus/plumbing_refs.rs` reaches four of them. The rest are below, each
    // as a `strict` case: the command prints nothing on either stream, so the
    // exit code is the entire answer and stderr costs nothing to pin.
    for name in [
        // A component may not begin with `.` …
        "refs/heads/.hidden",
        // … nor end with `.`,
        "refs/heads/tail.",
        // … nor contain the reserved punctuation or a wildcard.
        "refs/heads/tilde~1",
        "refs/heads/star*",
        // `@{` is the reflog syntax and is reserved wherever it appears.
        "refs/heads/at@{brace}",
        // A `.lock` component is reserved even when it is not the last one —
        // `refs/heads/name.lock` is already covered; this is the interior form.
        "refs/heads/name.lock/x",
    ] {
        out.push(Case::strict("check-ref-format", &["check-ref-format", name], Shape::Linear));
    }
    // The two `@` cases, which differ: a refname that *is* `@` is rejected, a
    // component that is `@` inside a longer name is not.
    out.push(Case::strict("check-ref-format", &["check-ref-format", "@"], Shape::Linear));
    out.push(Case::strict("check-ref-format", &["check-ref-format", "refs/heads/@"], Shape::Linear));

    // ---- var: where the answer comes from ----
    // `corpus/plumbing_objects.rs` asks for every variable under the default
    // configuration; what was unmeasured is the *precedence* behind them.
    // `env::harden` pins `GIT_EDITOR`, `GIT_SEQUENCE_EDITOR` and `GIT_PAGER` in
    // the environment, and the environment outranks config — so each pair below
    // must answer with the pinned value and not the configured one. A port that
    // reads config first fails all three while passing every existing `var` case.
    out.push(Case::new("var", &["var", "GIT_EDITOR"], Shape::Linear).with_config(&[("core.editor", "vi")]));
    out.push(
        Case::new("var", &["var", "GIT_SEQUENCE_EDITOR"], Shape::Linear)
            .with_config(&[("sequence.editor", "seq-ed")]),
    );
    out.push(Case::new("var", &["var", "GIT_PAGER"], Shape::Linear).with_config(&[("core.pager", "less")]));
    // `GIT_DEFAULT_BRANCH` is the one with no environment pin above it, so here
    // the configured value must win — the other half of the same test.
    out.push(
        Case::new("var", &["var", "GIT_DEFAULT_BRANCH"], Shape::Linear)
            .with_config(&[("init.defaultBranch", "trunk")]),
    );
}
