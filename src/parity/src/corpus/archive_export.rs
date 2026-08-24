//! Differential corpus cases for the verbs that turn a repository into a
//! **stream** and back: `archive`, `upload-archive`, `get-tar-commit-id`,
//! `fast-export`, `fast-import`, and the two tool launchers (`difftool`,
//! `mergetool`) driven non-interactively through a configured command.
//!
//! Every case here is compared against stock git for stdout, exit code and
//! post-command repository state.
//!
//! # Archive byte-stability: established, not assumed
//!
//! A tar or zip stream embeds a modification time per entry, so the first
//! question this module had to answer is whether the same archive produced twice
//! is the same bytes. It is, for all four built-in formats. Measured against
//! stock git 2.55.0 on the `linear` fixture, two runs three seconds apart, each
//! from its own fresh copy of the template:
//!
//! ```text
//! tar STABLE   tar.gz STABLE   tgz STABLE   zip STABLE
//! ```
//!
//! The mechanism is that `archive.c` stamps **every** entry from the archived
//! commit's date, which `crate::env::FIXED_DATE` pins at `1700000000 +0000`, and
//! git writes the gzip member with a zeroed mtime field (`gzip -cn`, the `-n`).
//! That covers the entries git invents as well as the ones it reads out of the
//! tree: `--add-file=README.md` stamps the entry `Nov 14 2023` — the commit date
//! — and not the file's own mtime, which on the machine that measured this was
//! `Aug 24`. So `--add-file` and `--add-virtual-file` are byte-stable too, and
//! were verified as such by the same two-runs-apart comparison. Nothing in this
//! module needs the filesystem's clock to hold still.
//!
//! `upload-archive` is stable for the tar formats and **not** for zip. Fed the
//! same pkt-line request three times, a `--format=tar` reply is byte-identical
//! (cksum `999791312`, 10261 bytes, three times); a `--format=zip` reply is not
//! (`375551037`/557 bytes, `3115918694`/562 bytes, `375551037`/557 bytes). The
//! archive inside is the same either way — what moves is how the reply is cut
//! into sideband packets, and the zip backend flushes on a different boundary
//! each run. So every `upload-archive` request here asks for a tar. The harness
//! agreed independently: an earlier zip request in this file was scored
//! `NONDETERMINISTIC`, stock failing to reproduce its own stdout.
//!
//! # What the stdout comparison can and cannot see
//!
//! [`crate::runner::normalize`] converts a side's stdout with
//! `String::from_utf8_lossy` (runner.rs:2717). That is exact for a stream that is
//! valid UTF-8 and **lossy** for one that is not: every invalid byte becomes one
//! U+FFFD, so two different byte strings can normalize to one string. Verified
//! directly — `[1f 8b c0 80 41]` and `[1f 8b c1 80 41]` are different bytes and
//! compare equal after the conversion.
//!
//! Measured over the fixtures, that splits the formats in two:
//!
//! | stream                        | valid UTF-8 | comparison |
//! |-------------------------------|-------------|------------|
//! | `--format=tar` over a text tree | yes       | byte-exact |
//! | `--format=tar` over `packed`   | no          | lossy      |
//! | `--format=zip`, `tar.gz`, `tgz`| no          | lossy      |
//!
//! So the content assertions below are carried by `--format=tar` on trees whose
//! blobs are text, and the compressed formats are kept for their exit code,
//! their container framing and their effect on repository state. A tar header is
//! ASCII and NUL-padded, which is why the uncompressed form survives the
//! conversion intact — entry name, mode, size and mtime are all still compared
//! byte for byte.
//!
//! # Where the bytes are asserted when the probe cannot see them
//!
//! `merge_family.rs` has the same problem with merged worktree content and
//! solves it by routing every content case through a surface the digest reads.
//! The routes here:
//!
//! * `archive` to **stdout** is the stream itself — nothing is hidden.
//! * `archive --output=<file>` writes into the fixture, and the state probe sees
//!   that file by *name* only (`status --porcelain -uall`). Those cases assert
//!   that a file appeared with the right name and that the command exited 0;
//!   they are marked where they are used and are not content assertions.
//! * `fast-import` writes objects and moves refs, so `cat-file --batch-check
//!   --batch-all-objects` and `for-each-ref` in `probe_state` carry the result.
//!   A stream that imports the wrong tree produces a different object id and is
//!   caught even though the command printed nothing.
//! * `fast-import`'s `ls`, `cat-blob` and `get-mark` answer on stdout (the
//!   cat-blob file descriptor defaults to stdout), so those three are direct.
//! * `difftool`/`mergetool` with a `cat`-based tool put the file content on
//!   stdout; `mergetool` additionally stages its result, so `ls-files --stage`
//!   carries the blob id.
//! * `fastimport.unpackLimit` decides pack-versus-loose, which `probe_storage`
//!   reports: measured 2 pack files / 5 loose fan-out directories at `0`, and
//!   0 / 8 at `100`, on the same stream.
//!
//! # Fixture constraints these cases work around
//!
//! * **`export-subst` is unreachable.** It needs an attribute set on a path
//!   *inside the archived tree*, and no fixture's `.gitattributes` sets it —
//!   [`Shape::Attributes`] carries `*.md diff=markdown export-ignore` and
//!   nothing else that `archive` consults. A case is one argv against a pristine
//!   copy, so it cannot write the rule first, and there is no `-c` spelling of
//!   an attribute. `export-ignore` *is* reachable and is measured: on
//!   `Attributes`, `archive --format=tar HEAD` drops `README.md` and
//!   `docs/manual.md` and leaves `docs/` as an empty directory entry.
//! * **`--import-marks=` has no marks file to read.** Nothing in any fixture is
//!   a fast-import marks file, and a case cannot write one. The reachable half
//!   is covered instead: `--export-marks=` writing one, on both `fast-export`
//!   and `fast-import`, and `--import-marks=` on an absent path as the refusal —
//!   which is a real refusal rather than a placeholder, since it is exactly what
//!   a resumed import does when its state file is gone.
//! * **`upload-archive` outside a repository has no directory to run in.** Every
//!   directory in every fixture is inside the fixture's own repository, so the
//!   "not a git repository" path is reached by naming a subdirectory as the
//!   repository operand (`upload-archive src`) rather than by moving the working
//!   directory outside one.
//! * **A tool name with no `cmd` is not a refusal.** `difftool` reacts to
//!   `diff.tool=<name>` with nothing behind it by printing advice and falling
//!   through to its built-in tool list, which on a developer machine launches
//!   `vimdiff` and blocks until the harness kills it — one such case here was
//!   scored `STOCK-TIMEOUT` before it was replaced. Nothing in this file names a
//!   tool it has not also configured; the difftool refusal is an option-parser
//!   conflict (`--tool` with `--extcmd`) instead.
//! * **`difftool --dir-diff` leaves nothing behind to compare.** Its temporary
//!   trees are created under the system temp directory, not under the fixture,
//!   so the state probe cannot see them; those cases are exit-code and stdout
//!   cases. They also cannot use a `cat "$LOCAL"` tool, because in dir-diff mode
//!   `$LOCAL` is a *directory* and `cat` then writes a machine-specific temp path
//!   to stderr. They use a `true` tool instead.
//! * **Nothing here runs a variable command.** Every `difftool.<tool>.cmd`,
//!   `mergetool.<tool>.cmd` and `tar.<format>.command` in this file is a literal
//!   `true`, `false`, or `cat` of a path git itself supplies. No command is
//!   assembled from a fixture path, an environment variable or a case parameter.

use crate::fixture::Shape;
use crate::runner::Case;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// A `difftool` whose command prints the *post-image* to stdout, so the tool's
/// output is the file content the launcher decided to show it. `$REMOTE` rather
/// than `$LOCAL` because for an added path `$LOCAL` is `/dev/null` and the case
/// would assert nothing.
const DIFF_CAT_REMOTE: &str = "difftool.parity.cmd=cat \"$REMOTE\"";
/// The pre-image half of the same question.
const DIFF_CAT_LOCAL: &str = "difftool.parity.cmd=cat \"$LOCAL\"";
/// A tool that succeeds and writes nothing — the only safe shape for
/// `--dir-diff`, where the two operands are directories.
const DIFF_TRUE: &str = "difftool.noop.cmd=true";

/// A `mergetool` that resolves by taking one side, matching `merge_family.rs`'s
/// convention so the two modules describe the same tool the same way.
const MERGE_TAKE_LOCAL: &str = "mergetool.parity.cmd=cat \"$LOCAL\" > \"$MERGED\"";
const MERGE_TRUST: &str = "mergetool.parity.trustExitCode=true";
/// A tool that exits 0 without writing `$MERGED`, and one that exits non-zero:
/// with `trustExitCode` set these are the two verdicts `git-mergetool--lib`
/// reports back, and they differ in whether the path ends up staged.
const MERGE_NOOP: &str = "mergetool.parity.cmd=true";
const MERGE_FAIL: &str = "mergetool.parity.cmd=false";

/// The 1024 bytes git puts at the front of every `tar`/`tar.gz` archive: a
/// `pax_global_header` block followed by its 52-byte payload,
/// `52 comment=<40 hex>\n`, NUL-padded to a full record.
///
/// Assembled from named fields rather than pasted as 1024 escapes so the header
/// *format* is legible — and verified byte-for-byte against the real thing:
/// `head -c 1024` of `git archive --format=tar HEAD` on the `linear` fixture
/// compares equal to this constant.
///
/// `get-tar-commit-id` reads this and prints the id. The mtime field is
/// `14524770400` octal, which is `1700000000` — [`crate::env::FIXED_DATE`], the
/// same pin that makes the archives byte-stable.
const fn pax_global_header(oid: &[u8; 40], typeflag: u8, checksum: bool) -> [u8; 1024] {
    const fn put(buf: &mut [u8; 1024], at: usize, src: &[u8]) {
        let mut i = 0;
        while i < src.len() {
            buf[at + i] = src[i];
            i += 1;
        }
    }
    let mut b = [0u8; 1024];
    put(&mut b, 0, b"pax_global_header");
    put(&mut b, 100, b"0000666\0"); // mode
    put(&mut b, 108, b"0000000\0"); // uid
    put(&mut b, 116, b"0000000\0"); // gid
    put(&mut b, 124, b"00000000064\0"); // size: 0o64 == 52
    put(&mut b, 136, b"14524770400\0"); // mtime: 0o14524770400 == 1700000000
    put(&mut b, 148, b"        "); // checksum field reads as spaces while summing
    b[156] = typeflag;
    put(&mut b, 257, b"ustar\0");
    put(&mut b, 263, b"00");
    put(&mut b, 265, b"root\0"); // uname
    put(&mut b, 297, b"root\0"); // gname
    put(&mut b, 329, b"0000000\0"); // devmajor
    put(&mut b, 337, b"0000000\0"); // devminor
    put(&mut b, 512, b"52 comment=");
    put(&mut b, 523, oid);
    b[563] = b'\n';
    let mut sum: u32 = 0;
    let mut i = 0;
    while i < 512 {
        sum += b[i] as u32;
        i += 1;
    }
    if !checksum {
        sum = 0;
    }
    let mut j = 0;
    while j < 7 {
        b[148 + 6 - j] = b'0' + ((sum >> (3 * j)) & 7) as u8;
        j += 1;
    }
    b[155] = 0;
    b
}

/// The initial commit of every fixture shape, as `fixture.rs` documents it.
const FIXTURE_ROOT_OID: &[u8; 40] = b"edfab1b71619a22120a8da1a3d85d68e0200290a";

/// A real archive header: `get-tar-commit-id` prints the id and exits 0.
static PAX_HEADER: [u8; 1024] = pax_global_header(FIXTURE_ROOT_OID, b'g', true);
/// The same header with the typeflag changed. Stock exits **1** and prints
/// nothing at all — not a diagnostic, not a partial id.
static PAX_BAD_TYPE: [u8; 1024] = pax_global_header(FIXTURE_ROOT_OID, b'x', true);
/// The same header with a deliberately wrong checksum. Stock **still prints the
/// id**: `get-tar-commit-id.c` checks the typeflag and the name and never
/// validates the checksum. A port that validates it refuses here and diverges.
static PAX_BAD_SUM: [u8; 1024] = pax_global_header(FIXTURE_ROOT_OID, b'g', false);
/// Only the header block, with the payload record missing: the EOF path.
static PAX_TRUNCATED: [u8; 512] = {
    let mut half = [0u8; 512];
    let mut i = 0;
    while i < 512 {
        half[i] = PAX_HEADER[i];
        i += 1;
    }
    half
};
/// Not an archive at all.
const NOT_AN_ARCHIVE: &[u8] = b"not a tar at all\n";

// `upload-archive` speaks pkt-line: one `argument <arg>\n` packet per argument
// of the `archive` command it is to run, then a flush. The four-hex prefix is
// the length of the packet *including* the prefix, which is why these are
// literals rather than assembled — the length and the payload have to agree, and
// a mismatch is a protocol error rather than a case.
const UA_HEAD: &[u8] = b"0012argument HEAD\n0000";
const UA_TAR_HEAD: &[u8] = b"001aargument --format=tar\n0012argument HEAD\n0000";
const UA_TAR_PREFIX: &[u8] =
    b"001aargument --format=tar\n0019argument --prefix=p/\n0012argument HEAD\n0000";
/// A flush with no arguments at all: `archive` gets no tree-ish and prints its
/// usage back down the error band.
const UA_FLUSH_ONLY: &[u8] = b"0000";
/// A tree-ish that does not resolve.
const UA_BAD_REF: &[u8] = b"0012argument nope\n0000";
/// Bytes that are not a pkt-line length prefix.
const UA_GARBAGE: &[u8] = b"garbagegarbage";

// ---------------------------------------------------------------------------
// fast-import streams
// ---------------------------------------------------------------------------
//
// Every stream below was run against stock git 2.55.0 before it was written
// down, and each literal was round-tripped back out to a file and compared with
// the bytes that were run. `data <n>` counts are exact; one byte off and
// fast-import reads the next command's first character as payload, which stock
// reports as `unsupported command:` with the truncated word.
//
// `from refs/heads/main^0` is how a stream that means "on top of what is already
// here" is spelled: fixtures all carry `refs/heads/main`, and the initial commit
// id is the same in every shape, so `from <40 hex>` is expressible too.

const S_BASIC: &[u8] = b"blob\n\
    mark :1\n\
    data 12\n\
    hello world\n\
    commit refs/heads/imported\n\
    mark :2\n\
    author zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 9\n\
    imported\n\
    M 100644 :1 hello.txt\n\
    \n\
    reset refs/heads/alias\n\
    from :2\n\
    \n";

const S_MARKS: &[u8] = b"blob\n\
    mark :1\n\
    data 4\n\
    one\n\
    commit refs/heads/m1\n\
    mark :2\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 6\n\
    first\n\
    M 100644 :1 one.txt\n\
    \n\
    commit refs/heads/m1\n\
    mark :3\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 7\n\
    second\n\
    from :2\n\
    M 100644 :1 two.txt\n\
    \n";

const S_DELETEALL: &[u8] = b"commit refs/heads/main\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 10\n\
    wipe tree\n\
    from refs/heads/main^0\n\
    deleteall\n\
    M 100644 inline only.txt\n\
    data 5\n\
    only\n\
    \n";

const S_FILEDELETE: &[u8] = b"commit refs/heads/main\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 12\n\
    drop a file\n\
    from refs/heads/main^0\n\
    D src/lib.rs\n\
    \n";

const S_COPYRENAME: &[u8] = b"commit refs/heads/main\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 14\n\
    copy and move\n\
    from refs/heads/main^0\n\
    C README.md COPY.md\n\
    R src/lib.rs src/renamed.rs\n\
    \n";

/// `M 120000` — the mode `fast-import.c` writes a symlink under. The blob is the
/// link *target*, and carries no trailing newline, which is what `data 9` says.
const S_SYMLINK: &[u8] = b"blob\n\
    mark :1\n\
    data 9\n\
    README.md\
    commit refs/heads/symlinked\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 10\n\
    a symlink\n\
    from refs/heads/main^0\n\
    M 120000 :1 link-to-readme\n\
    \n";

const S_TAG: &[u8] = b"commit refs/heads/tagged\n\
    mark :1\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 9\n\
    tag base\n\
    from refs/heads/main^0\n\
    \n\
    tag imported-tag\n\
    from :1\n\
    tagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 15\n\
    annotated here\n";

/// Three `ls` queries against a literal commit id: a blob, a tree, and a path
/// that is not there. The answers go to the cat-blob descriptor, which defaults
/// to stdout, so this is one of the few `fast-import` invocations whose result
/// is directly comparable rather than only visible through the object store.
const S_LS: &[u8] = b"ls edfab1b71619a22120a8da1a3d85d68e0200290a \"README.md\"\n\
    ls edfab1b71619a22120a8da1a3d85d68e0200290a \"src\"\n\
    ls edfab1b71619a22120a8da1a3d85d68e0200290a \"no/such/path\"\n";

/// The blob id of `README.md` in every fixture — `# fixture\n`, ten bytes.
const S_CATBLOB: &[u8] = b"cat-blob 9741694d75caeb49d3b7c1f59451c0c56bf6216c\n";

const S_GETMARK: &[u8] = b"blob\n\
    mark :7\n\
    data 4\n\
    abc\n\
    get-mark :7\n";

const S_DONE: &[u8] = b"feature done\n\
    blob\n\
    mark :1\n\
    data 3\n\
    hi\n\
    commit refs/heads/withdone\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 5\n\
    done\n\
    M 100644 :1 hi.txt\n\
    \n\
    done\n";

/// The same stream with the `done` terminator removed. `feature done` promises
/// one, so its absence is `fatal: stream ends early` rather than a clean EOF.
const S_DONE_MISSING: &[u8] = b"feature done\n\
    blob\n\
    mark :1\n\
    data 3\n\
    hi\n\
    commit refs/heads/nodone\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 5\n\
    done\n\
    M 100644 :1 hi.txt\n\
    \n";

const S_CHECKPOINT: &[u8] = b"blob\n\
    mark :1\n\
    data 3\n\
    cp\n\
    checkpoint\n\
    commit refs/heads/checkpointed\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 11\n\
    checkpoint\n\
    M 100644 :1 cp.txt\n\
    \n";

/// `progress` is echoed verbatim to stdout even under `--quiet`.
const S_PROGRESS: &[u8] = b"progress halfway there\n\
    blob\n\
    mark :1\n\
    data 3\n\
    pg\n\
    progress all done\n";

const S_OPTION: &[u8] = b"option git quiet\n\
    blob\n\
    mark :1\n\
    data 3\n\
    op\n\
    commit refs/heads/optioned\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 7\n\
    option\n\
    M 100644 :1 op.txt\n\
    \n";

const S_RAWDATE: &[u8] = b"commit refs/heads/rawdate\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 8\n\
    rawdate\n\
    from refs/heads/main^0\n\
    \n";

/// A timezone `raw` rejects and `raw-permissive` accepts: `-1800` is not a whole
/// number of minutes off the hour in the form git normally writes.
const S_RAWPERMISSIVE: &[u8] = b"commit refs/heads/rawp\n\
    committer zvcs parity <parity@example.invalid> 1700000000 -1800\n\
    data 5\n\
    rawp\n\
    from refs/heads/main^0\n\
    \n";

/// A two-parent commit built entirely inside the stream: `from` plus `merge`.
const S_MERGE: &[u8] = b"commit refs/heads/mergebase\n\
    mark :1\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 5\n\
    base\n\
    from refs/heads/main^0\n\
    M 100644 inline a.txt\n\
    data 2\n\
    a\n\
    \n\
    commit refs/heads/mergeside\n\
    mark :2\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 5\n\
    side\n\
    from refs/heads/main^0\n\
    M 100644 inline b.txt\n\
    data 2\n\
    b\n\
    \n\
    commit refs/heads/merged-in\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 6\n\
    merge\n\
    from :1\n\
    merge :2\n\
    M 100644 inline b.txt\n\
    data 2\n\
    b\n\
    \n";

/// `from` naming a 40-hex object id rather than a ref or a mark.
const S_OIDFROM: &[u8] = b"commit refs/heads/from-oid\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 9\n\
    from oid\n\
    from edfab1b71619a22120a8da1a3d85d68e0200290a\n\
    M 100644 inline oid.txt\n\
    data 4\n\
    oid\n\
    \n";

/// The delimited form of `data`, for a payload whose length the producer does
/// not know in advance. Both the blob and the commit message use it.
const S_DELIMITED: &[u8] = b"blob\n\
    mark :1\n\
    data <<EOFBLOB\n\
    line one\n\
    line two\n\
    EOFBLOB\n\
    commit refs/heads/delimited\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data <<EOFMSG\n\
    delimited message\n\
    EOFMSG\n\
    M 100644 :1 delim.txt\n\
    \n";

/// The `N` command, writing a note onto the fixture's own root commit.
const S_NOTES: &[u8] = b"commit refs/notes/commits\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 9\n\
    add note\n\
    N inline refs/heads/main^0\n\
    data 12\n\
    a note here\n\
    \n";

/// The zero-length blob, `e69de29bb2d1d6434b8b29ae775ad8c2e48c5391`, written
/// through the stream rather than read out of a tree.
const S_EMPTYBLOB: &[u8] = b"blob\n\
    mark :1\n\
    data 0\n\
    commit refs/heads/emptied\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 6\n\
    empty\n\
    M 100644 :1 zero.txt\n\
    \n";

/// A commit on `refs/heads/main` with no `from`, so the new tip is not a
/// descendant of the old one. Without `--force` fast-import warns and exits 1
/// having written the objects but not moved the ref — a state the probe reads
/// off `for-each-ref` and `cat-file --batch-all-objects` at once.
const S_NONFF: &[u8] = b"commit refs/heads/main\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 15\n\
    unrelated root\n\
    M 100644 inline root.txt\n\
    data 5\n\
    root\n\
    \n";

/// `data 100` with five bytes behind it.
const S_TRUNCATED: &[u8] = b"blob\n\
    mark :1\n\
    data 100\n\
    short\n";

const S_UNKNOWN: &[u8] = b"frobnicate the repository\n";

/// A commit with no `committer` line, which fast-import requires.
const S_NOCOMMITTER: &[u8] = b"commit refs/heads/bad\n\
    data 4\n\
    bad\n\
    \n";

/// A `filemodify` naming a mark the stream never declared.
const S_BADMARK: &[u8] = b"commit refs/heads/badmark\n\
    committer zvcs parity <parity@example.invalid> 1700000000 +0000\n\
    data 9\n\
    bad mark\n\
    M 100644 :99 nope.txt\n\
    \n";

// ---------------------------------------------------------------------------

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    archive_formats(out);
    archive_added_entries(out);
    archive_treeish_and_pathspec(out);
    archive_attributes_and_conversion(out);
    archive_tar_config(out);
    archive_shapes(out);
    archive_output_file(out);
    archive_remote(out);
    archive_refusals(out);
    fast_export(out);
    fast_import_streams(out);
    fast_import_flags(out);
    fast_import_refusals(out);
    get_tar_commit_id(out);
    upload_archive(out);
    difftool(out);
    mergetool(out);
}

fn one(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], shape: Shape) {
    out.push(Case::new(cmd, args, shape));
}

/// [`Case::strict`] for a case that also carries stdin.
///
/// `Case::strict` and `Case::with_stdin` are two constructors over the same
/// struct and neither composes with the other, so the combination is spelled
/// once here rather than at every refusal that is fed a stream.
fn strict_stdin(cmd: &'static str, args: &[&str], shape: Shape, stdin: &'static [u8]) -> Case {
    Case { compare_stderr: true, ..Case::with_stdin(cmd, args, shape, stdin) }
}

// ---------------------------------------------------------------------------
// archive
// ---------------------------------------------------------------------------

/// The compression level knob, per format.
///
/// `-0`..`-9` is not one option: `archive.c` hands the digit to the format
/// backend, `archive-zip.c` reads it as a zlib level and `tar.<format>.command`
/// receives it as an argument to the filter. A port that parses the digit and
/// drops it produces a valid archive of the wrong size, which is invisible to a
/// case that only checks the exit code — so each level is a whole-stream
/// comparison here, and `--format=zip -0` (stored, uncompressed) is deliberately
/// left to `info_attrs.rs` rather than repeated.
fn archive_formats(out: &mut Vec<Case>) {
    for level in ["-1", "-9"] {
        one(out, "archive", &["archive", "--format=zip", level, "HEAD"], Shape::Linear);
    }
    one(out, "archive", &["archive", "--format=tar.gz", "-9", "HEAD"], Shape::Linear);
    // `-v` reports each entry on stderr while the archive goes to stdout; the
    // zip backend and the tar backend report separately.
    one(out, "archive", &["archive", "-v", "--format=zip", "HEAD"], Shape::Linear);
    // `--list` is format-independent, but the argument parser still has to
    // accept a format beside it and ignore it.
    one(out, "archive", &["archive", "--list", "--format=tar"], Shape::Linear);
}

/// Entries that are in the archive but not in the tree.
///
/// `--add-file` reads the worktree and `--add-virtual-file` takes its content
/// from argv, and both are stamped with the *commit's* time rather than the
/// file's — verified, and the reason the whole stream stays byte-stable. Two
/// `--add-file`s in one invocation pin the order they appear in, which is the
/// order given rather than sorted; `--prefix` has to apply to them as well as to
/// the tree; and the quoted spelling of `--add-virtual-file` is a separate
/// parser path in `archive.c` from the bare one.
fn archive_added_entries(out: &mut Vec<Case>) {
    one(
        out,
        "archive",
        &["archive", "--format=tar", "--add-file=README.md", "--add-file=src/lib.rs", "HEAD"],
        Shape::Linear,
    );
    one(
        out,
        "archive",
        &["archive", "--format=tar", "--add-file=src/lib.rs", "--prefix=p/", "HEAD"],
        Shape::Linear,
    );
    one(
        out,
        "archive",
        &["archive", "--format=tar", "--add-virtual-file=a/b.txt:content", "HEAD"],
        Shape::Linear,
    );
    one(
        out,
        "archive",
        &["archive", "--format=zip", "--add-virtual-file=x.txt:hello", "HEAD"],
        Shape::Linear,
    );
    // The quoted form: the path is a C-quoted string and the colon inside it is
    // not the separator.
    one(
        out,
        "archive",
        &["archive", "--format=tar", "--add-virtual-file=\"quoted.txt\":body", "HEAD"],
        Shape::Linear,
    );
    one(out, "archive", &["archive", "--format=tar", "--prefix=a/b/", "HEAD"], Shape::Linear);
}

/// What "the tree-ish" is allowed to be, and what a pathspec does to it.
///
/// A tag, a branch, a peeled tree, a subtree named with `:`, and a second parent
/// are five different lookups in `archive.c`'s `parse_treeish_arg`, and only the
/// last two change what the *root* of the archive is. The pathspec cases pin
/// that a restriction filters entries without moving that root — a port that
/// implements `HEAD src` as `HEAD:src` produces the same file set with every
/// name one component short.
fn archive_treeish_and_pathspec(out: &mut Vec<Case>) {
    one(out, "archive", &["archive", "--format=tar", "v0.1.0"], Shape::Branched);
    one(out, "archive", &["archive", "--format=tar", "HEAD^{tree}"], Shape::Branched);
    one(out, "archive", &["archive", "--format=zip", "HEAD:src"], Shape::Branched);
    one(out, "archive", &["archive", "--format=tar", "HEAD^2"], Shape::Merged);
    one(out, "archive", &["archive", "--format=tar", "alien-tip"], Shape::Unrelated);
    one(out, "archive", &["archive", "--format=tar", "HEAD", "src", ":!src/lib.rs"], Shape::Branched);
    one(out, "archive", &["archive", "--format=tar", "HEAD", "moved"], Shape::Renamed);
}

/// `export-ignore`, and the eol conversion `archive` performs on the way out.
///
/// On [`Shape::Attributes`], `*.md diff=markdown export-ignore` drops `README.md`
/// and `docs/manual.md` from the stream and leaves `docs/` behind as an empty
/// directory entry — measured against stock, which lists seventeen entries with
/// `docs/` among them and neither `.md` file. That is the whole of what a port
/// gets wrong if it treats `export-ignore` as "skip the path": the directory
/// entry stays.
///
/// `core.autocrlf` reaches `archive` through the same `convert_to_working_tree`
/// call the checkout uses, and `* text=auto` in the fixture makes every text blob
/// eligible. Both `true` and `input` are here because they differ: only one of
/// them rewrites the bytes on the way *out*. Verified that `true` changes the
/// archive bytes against the unset default.
///
/// `--worktree-attributes` adds the worktree's `.gitattributes` to the lookup.
/// In this fixture the worktree copy and the committed copy are identical, so
/// this case measures that the flag is accepted and changes nothing — the
/// disagreement it would catch is a port that reads the *wrong* file and finds
/// no rules at all.
fn archive_attributes_and_conversion(out: &mut Vec<Case>) {
    one(out, "archive", &["archive", "--format=tar", "--prefix=p/", "HEAD"], Shape::Attributes);
    one(out, "archive", &["archive", "--format=tar", "HEAD", "docs"], Shape::Attributes);
    one(
        out,
        "archive",
        &["-c", "core.autocrlf=true", "archive", "--format=tar", "HEAD"],
        Shape::Attributes,
    );
    one(
        out,
        "archive",
        &["archive", "--worktree-attributes", "--format=tar", "HEAD"],
        Shape::Attributes,
    );
}

/// The `tar.*` configuration family.
///
/// `tar.umask` is not a permission on a file the command writes — it is masked
/// into the **mode field of every tar header**, so it changes the stream and
/// nothing on disk. Verified: `tar.umask=0` yields `-rw-rw-rw-`/`drwxrwxrwx`
/// entries against the default's `-rw-rw-r--`, and `user` (take the process
/// umask) is a third answer again.
///
/// `tar.<format>.command` is how a format is *defined*: `tar.tar.foo.command`
/// invents `tar.foo` out of nothing, which `--list` then has to report, and
/// `tar.tar.gz.command` replaces the built-in gzip filter. Both are set to `cat`
/// — a fixed, harmless command, never one drawn from a case parameter — and
/// `cat` is the interesting choice rather than a lazy one: it makes `--format=
/// tar.gz` emit a *plain tar*, verified byte-identical to `--format=tar`, so the
/// case proves the configured filter actually ran rather than being ignored.
fn archive_tar_config(out: &mut Vec<Case>) {
    for mask in ["0", "user"] {
        out.push(
            Case::new("archive", &["archive", "--format=tar", "HEAD"], Shape::Linear)
                .with_config(&[("tar.umask", mask)]),
        );
    }
    out.push(
        Case::new("archive", &["archive", "--format=tar.gz", "HEAD"], Shape::Linear)
            .with_config(&[("tar.tar.gz.command", "cat")]),
    );
    out.push(
        Case::new("archive", &["archive", "--format=tar.foo", "HEAD"], Shape::Linear)
            .with_config(&[("tar.tar.foo.command", "cat")]),
    );
    out.push(
        Case::new("archive", &["archive", "--list"], Shape::Linear)
            .with_config(&[("tar.tar.foo.command", "cat")]),
    );
}

/// One archive per shape whose *entry types* differ.
///
/// A tar header carries a type byte, a mode and a name, and the fixtures
/// disagree about all three. [`Shape::Symlinks`] has `120000` entries and
/// zero-length blobs, so the type byte is `2` and the size is `0` — neither of
/// which any other shape produces. [`Shape::Packed`] has a 400-line blob that
/// crosses the 512-byte block boundary several times, so the NUL padding after
/// the last partial block is real rather than degenerate.
/// [`Shape::DecomposedPaths`] decides how a combining mark is spelled in a
/// header — the composed and decomposed forms are different byte strings and
/// only one of them can be right.
/// [`Shape::Submodule`] and [`Shape::AwkwardPaths`] already have a
/// `--format=tar HEAD` case in `info_attrs.rs`; what is added here is the zip
/// container over the same trees, whose central directory encodes the same
/// names a second time.
fn archive_shapes(out: &mut Vec<Case>) {
    one(out, "archive", &["archive", "--format=tar", "--prefix=p/", "HEAD"], Shape::Symlinks);
    one(out, "archive", &["archive", "--format=zip", "HEAD"], Shape::Symlinks);
    one(out, "archive", &["archive", "--format=tar", "HEAD"], Shape::DecomposedPaths);
    one(out, "archive", &["archive", "--format=tar", "HEAD"], Shape::Packed);
}

/// `--output`, where the stream stops being stdout.
///
/// These are **not** content assertions: the state probe sees the written file
/// by name through `status --porcelain -uall` and never opens it. What they do
/// pin is the format *inference*, which is a decision made before a byte is
/// written and is wrong in two opposite directions — `archive.c` infers the
/// format from the filename suffix when `--format` is absent and must ignore the
/// suffix entirely when it is present. `--output=out.tgz` with `--format=tar`
/// therefore has to produce an uncompressed tar in a file named `.tgz`.
fn archive_output_file(out: &mut Vec<Case>) {
    one(out, "archive", &["archive", "--format=tar", "--output=out.tar", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--format=tar", "--output=out.tgz", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--output=out.tar.gz", "HEAD"], Shape::Linear);
}

/// `--remote=.`, which is `archive` acting as a *client* of `upload-archive`.
///
/// The path is real work rather than an alias: the client sends the arguments as
/// pkt-lines, the server runs the archive and the result comes back
/// band-multiplexed. Verified byte-stable across two runs, so the whole
/// round-trip is comparable.
///
/// `tar.<format>.remote` is the server-side gate on which formats a remote
/// caller may ask for. It is set here on the *client* command line, which is
/// where a caller would naturally put it, and the case records what stock does
/// with that — the setting reaching or not reaching the server it is meant for
/// is the behaviour, not an assumption this corpus makes.
fn archive_remote(out: &mut Vec<Case>) {
    one(out, "archive", &["archive", "--remote=.", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--remote=.", "--format=zip", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--remote=.", "--prefix=r/", "HEAD"], Shape::Branched);
    out.push(
        Case::new("archive", &["archive", "--remote=.", "--format=tgz", "HEAD"], Shape::Linear)
            .with_config(&[("tar.tgz.remote", "false")]),
    );
}

/// Refusals that matter, with stderr compared.
///
/// Each one is a different guard: a tree-ish that resolves to a **blob** exits
/// 128 naming the id it found, a `--add-file` operand that is not on disk is
/// checked before any output is produced, `--add-virtual-file` without a colon
/// is a parse error rather than a file-not-found, and a format that is not
/// built in and has no `tar.<format>.command` behind it is unknown. `--remote`
/// pointing at a directory that is not a repository fails at 1 rather than 128,
/// because the failure is the *remote's* and arrives over the error band.
fn archive_refusals(out: &mut Vec<Case>) {
    out.push(Case::strict("archive", &["archive", "--format=tar", "HEAD:README.md"], Shape::Linear));
    out.push(Case::strict(
        "archive",
        &["archive", "--format=tar", "--add-file=nosuchfile", "HEAD"],
        Shape::Linear,
    ));
    out.push(Case::strict(
        "archive",
        &["archive", "--format=tar", "--add-virtual-file=novalue", "HEAD"],
        Shape::Linear,
    ));
    out.push(Case::strict("archive", &["archive", "--format=tar.foo", "HEAD"], Shape::Linear));
    out.push(Case::strict("archive", &["archive", "--remote=nosuchdir", "HEAD"], Shape::Linear));
}

// ---------------------------------------------------------------------------
// fast-export
// ---------------------------------------------------------------------------

/// The serializer, across the options that change what the stream *says*.
///
/// Three groups, and they fail differently:
///
/// * **What is in the stream.** `--no-data` replaces every blob body with an id,
///   `--full-tree` re-emits the whole tree per commit instead of a delta against
///   the parent, `-M`/`-C` turn a delete-plus-add into an `R`/`C` record, and
///   `--show-original-ids`/`--mark-tags` add lines a consumer may or may not
///   understand. A port that emits a correct-looking stream with the wrong
///   *record kind* round-trips fine and still fails here.
/// * **How identities and encodings are rendered.** `--reencode` and
///   `--signed-tags` are enumerations whose non-default members are the ones a
///   port forgets; `--anonymize` replaces every path, ref, identity and message
///   with a counter-generated stand-in. Determinism of `--anonymize` was checked
///   before it was trusted — two runs two seconds apart produced identical
///   streams, `ref0`/`user0`/`path0` numbered in first-appearance order — so a
///   disagreement here is a real ordering difference, not a re-rolled value.
/// * **Which commits are reachable at all.** A range and `--refspec` change the
///   frontier, and the shapes below change what the frontier contains: a
///   criss-cross whose two merge bases are incomparable, an octopus with four
///   parents, unrelated roots with no merge base at all, a gitlink, and a
///   symlink whose record is `M 120000`.
fn fast_export(out: &mut Vec<Case>) {
    // Baselines, so every flag below has something to be compared against.
    one(out, "fast-export", &["fast-export", "--all"], Shape::Branched);
    one(out, "fast-export", &["fast-export", "main"], Shape::Branched);
    one(out, "fast-export", &["fast-export", "main~1..main"], Shape::Branched);

    // Stream content.
    one(out, "fast-export", &["fast-export", "--full-tree", "--all"], Shape::Branched);
    one(out, "fast-export", &["fast-export", "--use-done-feature", "--all"], Shape::Branched);
    one(out, "fast-export", &["fast-export", "--show-original-ids", "--all"], Shape::Branched);
    one(out, "fast-export", &["fast-export", "--mark-tags", "--all"], Shape::Branched);
    one(out, "fast-export", &["fast-export", "--progress=1", "--all"], Shape::Branched);
    one(
        out,
        "fast-export",
        &["fast-export", "--all", "--no-data", "--reencode=no", "-M", "-C", "--use-done-feature"],
        Shape::Branched,
    );

    // Rendering enumerations.
    for mode in ["yes", "no"] {
        let flag = format!("--reencode={mode}");
        one(out, "fast-export", &["fast-export", &flag, "--all"], Shape::Branched);
    }
    for mode in ["strip", "verbatim"] {
        let flag = format!("--signed-tags={mode}");
        one(out, "fast-export", &["fast-export", &flag, "--all"], Shape::Branched);
    }
    one(out, "fast-export", &["fast-export", "--tag-of-filtered-object=drop", "--all"], Shape::Branched);
    one(out, "fast-export", &["fast-export", "--anonymize", "--all"], Shape::Branched);

    // Marks and refspecs. `--export-marks` writes a repo-relative file the state
    // probe sees by name; the `--import-marks` half of the pair is a refusal
    // below, because no fixture carries a marks file to read.
    one(out, "fast-export", &["fast-export", "--export-marks=marks.txt", "--all"], Shape::Branched);
    one(
        out,
        "fast-export",
        &["fast-export", "--refspec=refs/heads/main:refs/heads/x", "--all"],
        Shape::Branched,
    );

    // Rename and copy detection, where the fixture's similarity indices are
    // known: 100% for the pure rename and `R072` for the rename-with-edit, so
    // plain `-M` classifies both and `-C` additionally has a real copy to find.
    one(out, "fast-export", &["fast-export", "-M", "--all"], Shape::Renamed);
    one(out, "fast-export", &["fast-export", "-C", "-M", "--all"], Shape::Renamed);

    // Frontiers and object kinds the other shapes cannot express.
    one(out, "fast-export", &["fast-export", "--all"], Shape::Symlinks);
    one(out, "fast-export", &["fast-export", "--all"], Shape::Submodule);
    one(out, "fast-export", &["fast-export", "--all"], Shape::Unrelated);
    one(out, "fast-export", &["fast-export", "--all"], Shape::CrissCross);
    one(out, "fast-export", &["fast-export", "--all"], Shape::Octopus);
    one(out, "fast-export", &["fast-export", "--all"], Shape::AwkwardPaths);

    // Refusals.
    out.push(Case::strict(
        "fast-export",
        &["fast-export", "--import-marks=nosuchfile", "--all"],
        Shape::Branched,
    ));
    out.push(Case::strict("fast-export", &["fast-export", "--nope", "--all"], Shape::Branched));
}

// ---------------------------------------------------------------------------
// fast-import
// ---------------------------------------------------------------------------

/// One case per command in the fast-import stream language.
///
/// `fast-import` prints nothing on the happy path, so for most of these the
/// entire verdict is the state digest: the objects the stream wrote appear in
/// `cat-file --batch-check --batch-all-objects` and the refs it moved appear in
/// `for-each-ref`. That makes the comparison sharper than it looks — an importer
/// that writes the right *content* into the wrong tree shape produces a
/// different commit id, and every id in the digest moves at once.
///
/// The three exceptions answer on stdout and are worth having for that alone:
/// `ls` prints a mode/type/id/path line per query (and `missing <path>` for one
/// that is not there), `cat-blob` prints a header plus the blob, and `get-mark`
/// prints the id a mark stands for.
fn fast_import_streams(out: &mut Vec<Case>) {
    /// The default form: quiet, so the statistics block on stderr is not the
    /// thing being compared.
    fn q(out: &mut Vec<Case>, stream: &'static [u8], shape: Shape) {
        out.push(Case::with_stdin("fast-import", &["fast-import", "--quiet"], shape, stream));
    }

    q(out, S_BASIC, Shape::Linear);
    q(out, S_MARKS, Shape::Linear);
    q(out, S_DELETEALL, Shape::Linear);
    q(out, S_FILEDELETE, Shape::Linear);
    q(out, S_COPYRENAME, Shape::Linear);
    q(out, S_SYMLINK, Shape::Linear);
    q(out, S_TAG, Shape::Linear);
    q(out, S_MERGE, Shape::Linear);
    q(out, S_OIDFROM, Shape::Linear);
    q(out, S_DELIMITED, Shape::Linear);
    q(out, S_NOTES, Shape::Linear);
    q(out, S_EMPTYBLOB, Shape::Linear);
    q(out, S_CHECKPOINT, Shape::Linear);
    q(out, S_OPTION, Shape::Linear);
    q(out, S_DONE, Shape::Linear);

    // Answers on stdout.
    q(out, S_LS, Shape::Linear);
    q(out, S_CATBLOB, Shape::Linear);
    q(out, S_GETMARK, Shape::Linear);
    q(out, S_PROGRESS, Shape::Linear);

    // Without `--quiet`, the statistics block goes to stderr and the pack the
    // run produced is named in it. Kept non-strict for that reason: what this
    // case measures is that the default-verbose path still writes the same
    // objects.
    out.push(Case::with_stdin("fast-import", &["fast-import"], Shape::Linear, S_BASIC));
}

/// The flags, against a stream whose result is already pinned above.
///
/// Everything here is a *storage* or *bookkeeping* decision rather than a
/// content one, which is why they share one stream: the objects must come out
/// identical and only the layout around them may move.
///
/// `fastimport.unpackLimit` is the sharpest of them because it changes the
/// answer visibly — measured on `S_BASIC`, `0` leaves two pack files and five
/// loose fan-out directories, `100` leaves no pack and eight. `probe_storage`
/// reports both numbers, so a port that always packs and a port that never packs
/// are distinguishable rather than both "succeeded".
///
/// `--force` is the pair to it that touches refs instead: `S_NONFF` moves
/// `refs/heads/main` backwards onto an unrelated root, which fast-import refuses
/// with a warning and exit 1 *after* writing the objects. So the two cases
/// differ in `for-each-ref` while agreeing in `--batch-all-objects`, which is
/// exactly the distinction a port that treats the refusal as a hard abort loses.
fn fast_import_flags(out: &mut Vec<Case>) {
    fn f(out: &mut Vec<Case>, args: &[&str], stream: &'static [u8]) {
        out.push(Case::with_stdin("fast-import", args, Shape::Linear, stream));
    }

    f(out, &["fast-import", "--quiet"], S_NONFF);
    f(out, &["fast-import", "--quiet", "--force"], S_NONFF);


    // Marks files, which land in the fixture and in `.git/info/fast-import/`.
    f(out, &["fast-import", "--quiet", "--export-marks=marks.out"], S_MARKS);

    // Date formats. `raw` is what the streams above already speak; `-1800` is
    // the offset only `raw-permissive` accepts.
    f(out, &["fast-import", "--quiet", "--date-format=raw"], S_RAWDATE);
    f(out, &["fast-import", "--quiet", "--date-format=raw-permissive"], S_RAWPERMISSIVE);

    for limit in ["0", "100"] {
        out.push(
            Case::with_stdin("fast-import", &["fast-import", "--quiet"], Shape::Linear, S_BASIC)
                .with_config(&[("fastimport.unpackLimit", limit)]),
        );
    }
}

/// Malformed streams and unusable options.
///
/// Six different places the parser gives up, and they are not interchangeable:
/// a `data` count that outruns the stream is reported with the number of bytes
/// still owed, a command word that is not a command is echoed back in full, a
/// `commit` without a `committer` is a missing *required field* rather than a
/// syntax error, a mark that was never declared is caught at use, a
/// `feature done` whose `done` never arrives is `stream ends early` even though
/// the stream is otherwise complete, and a marks file that is not there is an
/// I/O failure before the stream is read at all. A port that answers "malformed
/// input" to all six agrees with stock on none of them.
///
/// **Not `Case::strict`, and that is measured rather than conceded.** On every
/// one of these, `fast-import.c` writes `.git/fast_import_crash_<pid>` and names
/// that file on stderr — six runs of this group produced six different names
/// (`fast_import_crash_74351`, `..._74428`, `..._74471`, `..._74493`,
/// `..._74495`, `..._74586`). A process id is a value stock does not reproduce
/// itself, so a byte-for-byte stderr comparison here could never pass for any
/// implementation, and a case that can only fail measures nothing. The exit code
/// and the post-command state carry the verdict instead: each of these leaves a
/// different amount of the stream applied, which `for-each-ref` and
/// `cat-file --batch-all-objects` report.
fn fast_import_refusals(out: &mut Vec<Case>) {
    fn bad(out: &mut Vec<Case>, stream: &'static [u8]) {
        out.push(Case::with_stdin("fast-import", &["fast-import", "--quiet"], Shape::Linear, stream));
    }
    bad(out, S_TRUNCATED);
    bad(out, S_UNKNOWN);
    bad(out, S_NOCOMMITTER);
    bad(out, S_BADMARK);
    bad(out, S_DONE_MISSING);
    out.push(Case::with_stdin(
        "fast-import",
        &["fast-import", "--quiet", "--import-marks=nosuch.marks"],
        Shape::Linear,
        S_BASIC,
    ));
}

// ---------------------------------------------------------------------------
// get-tar-commit-id
// ---------------------------------------------------------------------------

/// The header reader, fed a real header and four things that are not one.
///
/// The corpus previously reached this command only with stdin at `/dev/null`
/// (`info_attrs.rs`), so it had never once succeeded. [`PAX_HEADER`] is the same
/// 1024 bytes `git archive --format=tar HEAD` puts at the front of its output on
/// every fixture — verified equal to the real thing — so the success path is now
/// reachable without the harness having to run one command into another.
///
/// The four refusals are each a distinct decision, and one of them is a
/// *non*-refusal that a defensive port would get wrong: `get-tar-commit-id.c`
/// checks the typeflag and the name and never verifies the header checksum, so a
/// header with a deliberately corrupt checksum still prints its id and exits 0.
/// A port that validates the checksum fails only on this case.
fn get_tar_commit_id(out: &mut Vec<Case>) {
    out.push(Case::with_stdin("get-tar-commit-id", &["get-tar-commit-id"], Shape::Linear, &PAX_HEADER));
    out.push(Case::with_stdin(
        "get-tar-commit-id",
        &["get-tar-commit-id"],
        Shape::Linear,
        &PAX_BAD_SUM,
    ));
    // Typeflag not `g`: exit 1, and **no** diagnostic at all.
    out.push(strict_stdin("get-tar-commit-id", &["get-tar-commit-id"], Shape::Linear, &PAX_BAD_TYPE));
    // Header block present, payload record missing.
    out.push(strict_stdin(
        "get-tar-commit-id",
        &["get-tar-commit-id"],
        Shape::Linear,
        &PAX_TRUNCATED,
    ));
    out.push(strict_stdin("get-tar-commit-id", &["get-tar-commit-id"], Shape::Linear, NOT_AN_ARCHIVE));
    // A real header with a stray operand: the usage check happens first.
    out.push(strict_stdin(
        "get-tar-commit-id",
        &["get-tar-commit-id", "extra"],
        Shape::Linear,
        &PAX_HEADER,
    ));
}

// ---------------------------------------------------------------------------
// upload-archive
// ---------------------------------------------------------------------------

/// The server half of `archive --remote`, driven by a real pkt-line request.
///
/// `transport_local.rs` runs this command with stdin closed, which reaches the
/// argument parser and stops: with no request to read, the protocol is never
/// entered. These cases send one. The reply is band-multiplexed — `0008ACK\n`,
/// then the archive on band 1, then errors on band 3 — so a port that gets the
/// framing right and the archive wrong, or the reverse, is separable here from
/// one that gets both right. Verified byte-stable across two runs, so the whole
/// reply is comparable, not just its length.
///
/// The three refusals travel *down the same connection*: an empty request makes
/// the spawned `archive` print its usage, and both it and the bad-ref failure
/// arrive as band-3 packets plus a `fatal: sent error to the client` on the
/// server's own stderr. A port that fails these by exiting before writing
/// anything produces the right exit code and the wrong stdout.
fn upload_archive(out: &mut Vec<Case>) {
    fn req(out: &mut Vec<Case>, shape: Shape, stream: &'static [u8]) {
        out.push(Case::with_stdin("upload-archive", &["upload-archive", "."], shape, stream));
    }
    req(out, Shape::Linear, UA_HEAD);
    req(out, Shape::Linear, UA_TAR_HEAD);
    req(out, Shape::Linear, UA_TAR_PREFIX);
    req(out, Shape::Branched, UA_HEAD);

    out.push(strict_stdin("upload-archive", &["upload-archive", "."], Shape::Linear, UA_FLUSH_ONLY));
    out.push(strict_stdin("upload-archive", &["upload-archive", "."], Shape::Linear, UA_BAD_REF));
    out.push(strict_stdin("upload-archive", &["upload-archive", "."], Shape::Linear, UA_GARBAGE));
    // The nearest thing any fixture has to "outside a repository": a tracked
    // subdirectory named as the repository to serve.
    out.push(strict_stdin("upload-archive", &["upload-archive", "src"], Shape::Linear, UA_HEAD));
}

// ---------------------------------------------------------------------------
// difftool
// ---------------------------------------------------------------------------

/// `difftool` driven by a *configured* tool rather than `--extcmd`.
///
/// `misc_commands.rs` covers the `--extcmd` route, where the command is on the
/// command line. This is the other one: `difftool.<tool>.cmd` plus a name, which
/// is what `git-difftool--helper.sh` looks up through `mergetool--lib`'s
/// `get_merge_tool_cmd`, and it is reached by four different routes that must
/// all land on the same tool — `--tool=`, `diff.tool`, `--gui` with
/// `diff.guitool`, and `difftool.prompt=false` — the configuration spelling of
/// `--no-prompt`, which is the only thing keeping the launcher from stopping to
/// ask a question no case can answer.
///
/// The tool is `cat "$REMOTE"`, so the *post-image content* is stdout and the
/// case asserts what the launcher decided to show rather than only that it
/// exited 0. `$REMOTE` and not `$LOCAL` because for an added path `$LOCAL` is
/// `/dev/null`; both are used, on shapes where each has something in it.
///
/// `--dir-diff` cannot use that tool — its operands are directories — so it uses
/// a `true` tool. Its temporary trees live outside the fixture, so those cases
/// are exit-code cases, and they were checked to leave the fixture's own status
/// untouched.
fn difftool(out: &mut Vec<Case>) {
    fn dt(out: &mut Vec<Case>, cfg: &str, args: &[&str], shape: Shape) {
        let mut argv = vec!["-c", cfg];
        argv.extend_from_slice(args);
        out.push(Case::new("difftool", &argv, shape));
    }

    // The two sides of one comparison, on a shape that has both.
    dt(out, DIFF_CAT_REMOTE, &["difftool", "--tool=parity", "--no-prompt"], Shape::Dirty);
    dt(out, DIFF_CAT_LOCAL, &["difftool", "--tool=parity", "--no-prompt"], Shape::Dirty);
    dt(out, DIFF_CAT_REMOTE, &["difftool", "--tool=parity", "--no-prompt", "--cached"], Shape::Dirty);

    // Tool selection by configuration rather than by flag.
    out.push(
        Case::new("difftool", &["difftool", "--no-prompt"], Shape::Dirty)
            .with_config(&[("difftool.parity.cmd", "cat \"$REMOTE\""), ("diff.tool", "parity")]),
    );
    out.push(
        Case::new("difftool", &["difftool", "--gui", "--no-prompt"], Shape::Dirty)
            .with_config(&[("difftool.parity.cmd", "cat \"$REMOTE\""), ("diff.guitool", "parity")]),
    );
    // `difftool.prompt=false` is the configuration spelling of `--no-prompt`.
    out.push(
        Case::new("difftool", &["difftool", "--tool=parity"], Shape::Dirty)
            .with_config(&[("difftool.parity.cmd", "cat \"$REMOTE\""), ("difftool.prompt", "false")]),
    );

    // A revision range, and shapes whose diffs are of a different kind: renames
    // with a known similarity, unicode and quote-worthy path names, a typechange
    // from a file to a symlink.
    dt(out, DIFF_CAT_REMOTE, &["difftool", "--tool=parity", "--no-prompt", "HEAD~1", "HEAD"], Shape::Branched);
    dt(out, DIFF_CAT_REMOTE, &["difftool", "--tool=parity", "--no-prompt", "HEAD~1", "HEAD"], Shape::AwkwardPaths);

    // `trustExitCode` decides whether a failing tool stops the walk. With it on,
    // stock exits 128 with `external diff died`; with it off, the failure is
    // swallowed and the command succeeds.
    out.push(
        Case::new("difftool", &["difftool", "--tool=noop", "--no-prompt"], Shape::Dirty)
            .with_config(&[("difftool.noop.cmd", "false"), ("difftool.trustExitCode", "true")]),
    );
    out.push(
        Case::new("difftool", &["difftool", "--tool=noop", "--no-prompt"], Shape::Dirty)
            .with_config(&[("difftool.noop.cmd", "false"), ("difftool.trustExitCode", "false")]),
    );

    // Directory mode, with a tool that cannot be confused by a directory.
    dt(out, DIFF_TRUE, &["difftool", "--tool=noop", "--no-prompt", "--dir-diff"], Shape::Dirty);
    dt(out, DIFF_TRUE, &["difftool", "--tool=noop", "--no-prompt", "--dir-diff", "--symlinks"], Shape::Symlinks);

    // `--tool` and `--extcmd` name the same thing twice, which the option parser
    // rejects before any tool is looked up. Deliberately *not* the
    // `diff.tool=<name with no cmd>` form: that is not a refusal at all — git
    // prints advice and falls through to the built-in tool list, which on a
    // developer machine means launching vimdiff, and the case then measures how
    // long the harness waits for an editor.
    out.push(
        Case::strict("difftool", &["difftool", "--tool=noop", "--extcmd=true", "--no-prompt"], Shape::Dirty)
            .with_config(&[("difftool.noop.cmd", "true")]),
    );
}

// ---------------------------------------------------------------------------
// mergetool
// ---------------------------------------------------------------------------

/// `mergetool` on the one mid-merge shape, over the knobs `merge_family.rs`
/// does not reach.
///
/// That module establishes the baseline — a `cat "$LOCAL"`/`cat "$REMOTE"` tool
/// with `trustExitCode`, `--no-prompt` and `keepBackup=false` — and the staged
/// blob is what distinguishes the outcomes. These cases keep the same tool and
/// move everything around it:
///
/// * **How the tool is chosen**: `merge.tool`, `merge.guitool` + `--gui`,
///   `mergetool.prompt=false` instead of `--no-prompt`.
/// * **Where the tool's files live**: `mergetool.writeToTemp` moves `$LOCAL`,
///   `$BASE` and `$REMOTE` out of the worktree into a temporary directory, so a
///   port that hard-codes the sibling-file layout resolves nothing.
/// * **What the tool's exit code means**: a tool that exits 0 without writing
///   `$MERGED` and one that exits non-zero are the two ends of `trustExitCode`,
///   and stock treats them differently — verified, `false` gives exit 1 and
///   `merge of conflict.txt failed`.
/// * **`-O<orderfile>`**, which is passed straight to `diff --name-only` and so
///   decides *which* unmerged paths are offered. Three spellings, three
///   different answers from stock: `-OREADME.md` resolves the conflict,
///   `-O README.md` (detached, so the orderfile name is empty) reports
///   `No files need merging` and resolves nothing, and `-O.gitignore` — a file
///   the shape does not have — prints `failed to read orderfile` on stderr and
///   still exits 0. A port that treats all three the same is wrong twice.
fn mergetool(out: &mut Vec<Case>) {
    fn mt(out: &mut Vec<Case>, extra: &[&str], args: &[&str]) {
        let mut argv = vec!["-c", MERGE_TAKE_LOCAL, "-c", MERGE_TRUST];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(args);
        out.push(Case::new("mergetool", &argv, Shape::Conflicted));
    }

    mt(out, &[], &["mergetool", "--no-prompt", "--tool=parity", "-OREADME.md"]);
    mt(out, &[], &["mergetool", "--no-prompt", "--tool=parity", "-O", "README.md"]);
    mt(out, &[], &["mergetool", "--no-prompt", "--tool=parity", "-O.gitignore"]);
    mt(out, &["-c", "mergetool.writeToTemp=true"], &["mergetool", "--no-prompt", "--tool=parity"]);
    mt(out, &["-c", "mergetool.prompt=false"], &["mergetool", "--tool=parity"]);
    mt(out, &["-c", "merge.tool=parity"], &["mergetool", "--no-prompt"]);
    mt(out, &["-c", "merge.guitool=parity"], &["mergetool", "--gui", "--no-prompt"]);

    // A tool that succeeds without writing `$MERGED`: the path stays unmerged
    // even though the tool said yes.
    out.push(Case::new(
        "mergetool",
        &["-c", MERGE_NOOP, "-c", MERGE_TRUST, "mergetool", "--no-prompt", "--tool=parity"],
        Shape::Conflicted,
    ));
    // `trustExitCode` off, with a tool that did resolve: stock asks anyway, gets
    // EOF, and reports the merge as failed.
    out.push(Case::new(
        "mergetool",
        &[
            "-c", MERGE_TAKE_LOCAL, "-c", "mergetool.parity.trustExitCode=false",
            "mergetool", "--no-prompt", "--tool=parity",
        ],
        Shape::Conflicted,
    ));
    // A tool that fails, trusted: exit 1 and nothing staged.
    out.push(Case::strict(
        "mergetool",
        &["-c", MERGE_FAIL, "-c", MERGE_TRUST, "mergetool", "--no-prompt", "--tool=parity"],
        Shape::Conflicted,
    ));
}
