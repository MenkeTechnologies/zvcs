//! Differential corpus cases for the **object listing format layer** — the
//! printers `ls-tree`, `cat-file`'s batch modes and `rev-list --objects` share,
//! as distinct from what any of them decides to *include*.
//!
//! Every case here is compared against stock git for stdout, exit code and
//! post-command repository state; the ones whose whole answer is a one-line
//! refusal are `strict` and compare stderr too.
//!
//! # Why a module about formatting rather than about a command
//!
//! Four different pieces of code in git turn "an object" into a line of text:
//! `ls-tree`'s row printer, `cat-file`'s batch record writer, `rev-list
//! --objects`' `<oid> SP <name>` emitter, and `verify-pack -v`'s table. They
//! agree on almost nothing. A port that implements each one against its own
//! reading of the docs passes every single-verb case and still hands a script
//! two different sizes for one blob, or a type name one verb has and another
//! does not. So the cases below are deliberately laid out in *pairs over one
//! object*: when they fail, they fail together and the report shows the two
//! answers side by side.
//!
//! Three such pairs are built here, each on a fact that is not a matter of
//! taste:
//!
//! 1. **The gitlink.** `Shape::Submodule` records `sub` at mode `160000`. The
//!    object it names is in no object store the fixture has. Stock's four
//!    printers give four different answers about it, all pinned below:
//!    `ls-tree HEAD` prints `160000 commit <oid>\tsub`; `cat-file --batch-check`
//!    asked `HEAD:sub` prints `<oid> submodule` — a type name that appears
//!    nowhere else in git's vocabulary, with **no size field and with the
//!    `--batch-check=<format>` ignored entirely**; `cat-file -t HEAD:sub` is
//!    `fatal: git cat-file: could not get object info`, exit 128; and `rev-list
//!    --objects HEAD` does not mention it at all. A port with one object-info
//!    path behind all four cannot produce that spread by accident.
//! 2. **The atom tables.** `ls-tree --format=` and `cat-file --batch-check=`
//!    take overlapping but unequal atom sets, and the overlap is not where a
//!    reader would guess: `%(objectmode)` is **accepted by `cat-file` and
//!    renders empty**, while `%(path)` and `%(objectsize:padded)` — which
//!    `ls-tree` has — are `fatal: bad cat-file format`. Symmetrically
//!    `%(objectsize:disk)`, `%(deltabase)` and `%(rest)` are `fatal: bad ls-tree
//!    format`. A port sharing one atom table between the two verbs gets four of
//!    those six wrong and no single-verb case sees it.
//! 3. **The abbreviation.** On `Shape::PrefixCollision`, `ls-tree --abbrev=4`
//!    *widens* `a36664d…`/`a3660f2…` to five characters because four no longer
//!    identify them, while `cat-file --batch-check` handed the four-character
//!    key answers `a366 ambiguous` on stdout with the candidate report on
//!    stderr. Same two blobs, one printer lengthening and one refusing.
//!
//! # What the neighbouring modules own, and what is left here
//!
//! Read before writing a line of this file; the division is by *surface*, not by
//! command name, because five of these modules name `cat-file` and four name
//! `ls-tree`.
//!
//! * **`plumbing_objects.rs`** — the object *constructors* and the single-object
//!   verbs: `hash-object`, `write-tree`, `read-tree`, `commit-tree`, `mktree`,
//!   `mktag`, plus the empty-stdin error paths of the pack plumbing. Nothing
//!   here touches any of them.
//! * **`object_pack.rs`** — the object store and pack plumbing, and the single
//!   largest body of `cat-file` cases in the corpus: `-t`/`-s`/`-p`/`-e`, the
//!   `--batch`/`--batch-check`/`--batch-command` record framing over
//!   `Shape::Branched`, `-Z`, `--textconv`/`--filters`, `%(objectsize:disk)`/
//!   `%(deltabase)` over `Shape::Packed`, and the hand-built `.idx` literals
//!   that are the only thing making `show-index`'s success path reachable.
//!   Left to it entirely. What is added here is the part its own header calls
//!   out as unreached: **`-z` as distinct from `-Z`** (input separator only),
//!   `--allow-unknown-type` under a batch mode, `--batch-check --buffer`,
//!   `--batch-all-objects --batch` (contents, not checks), the **`ambiguous`**
//!   error line, and the atom-table boundary above.
//! * **`index_plumbing.rs`** — `ls-files`/`update-index`: the *index* as a
//!   listing, which is a different store from the one this module reads.
//! * **`interchange.rs`** — `bundle`/`fast-export`/`fast-import`: object listings
//!   that cross a process boundary as a stream format.
//! * **`stdin_plumbing.rs`** — the commands whose payload is stdin
//!   (`unpack-objects`, `mktree`, `update-index --index-info`, …). `cat-file`'s
//!   batch modes also read stdin, and the split is by verb: it does not carry
//!   `cat-file`.
//! * **`traversal_order.rs`** — `rev-list`'s *walk*: which commits, in which
//!   order, under `--topo-order`/`--date-order`/`--reverse`/`--boundary`. This
//!   module owns the same command's **object output**: `--objects`'
//!   `<oid> SP <path>` line, `--object-names`/`--no-object-names`,
//!   `--objects-edge[-aggressive]`, `--in-commit-order`, `--disk-usage`, and the
//!   `--filter=` spec grammar.
//! * **`integrity_gc.rs`** — `fsck`/`gc`/`prune`/`verify-pack`'s assertions.
//!   Together with `shape_reach.rs` it has `verify-pack` saturated across both
//!   operands and all four printing flags; see the note below.
//! * **`graft_partial.rs`** — `Shape::Shallow` and `Shape::Promisor`, where
//!   `--missing=` and `--filter=` actually bite because objects are genuinely
//!   absent. `--missing=` is left to it and to `fixture_gaps2.rs` in full: what
//!   this module adds about `--filter=` is the *spec grammar* on repositories
//!   where nothing is missing, which is the half that measures parsing and
//!   selection rather than promisor bookkeeping.
//! * **`fixture_gaps.rs`** — `cat-file --follow-symlinks` over `Shape::Symlinks`,
//!   with all seven in-tree link resolutions. `object_pack.rs`'s header still
//!   says in-tree resolution is unreachable; it is not, and it is that module's,
//!   so `--follow-symlinks` appears nowhere here.
//! * **`fixture_gaps3.rs`** — `Shape::PrefixCollision`'s single-object side:
//!   `cat-file -t/-e/-s/-p` on the short keys, `core.disambiguate`, and
//!   `ls-tree --abbrev=4` in its two plain spellings. The batch side of that
//!   shape — where the ambiguity becomes a *record* rather than a fatal — was
//!   untouched and is taken here.
//! * **`pathspec_stdin.rs`** — `ls-tree`'s pathspec *magic* and
//!   `core.quotePath`. Pathspecs here are plain and are used only to select the
//!   gitlink row and to pin the empty-result exit code.
//!
//! # What this module cannot measure, and why
//!
//! * **`show-index`.** It reads its index from stdin and from nowhere else, and
//!   a `Case`'s stdin is a `&'static [u8]` literal, so its success path needs a
//!   byte-for-byte `.idx` spelled in Rust. `object_pack.rs` already builds one
//!   (`one_object_idx()`, 1128 bytes, assembled field by field at compile time)
//!   together with its truncated, bad-magic and bad-checksum variants, and a
//!   `const` does not cross module privacy. Re-spelling it here would add a
//!   second copy of one payload and zero signal, so **no `show-index` case is
//!   added**. Pointing it at `Shape::Packed`'s `packs/sample.idx` is not an
//!   escape either: the path operand is parsed and then ignored, which
//!   `object_pack.rs` already pins.
//! * **`verify-pack`'s listing.** Reachable — `Shape::Packed` tracks
//!   `packs/sample.idx` and `packs/sample.pack` at stable worktree paths
//!   precisely because a pack's real filename embeds its own checksum and can
//!   never be a literal — but already saturated: `object_pack.rs` pins
//!   `-v <pack>` and `--stat-only <idx>`, `shape_reach.rs` sweeps the `.idx`
//!   operand across bare/`-v`/`--verbose`/`-s`/`--stat-only`/`--`, and
//!   `integrity_gc.rs` adds the `.pack` operand under the printing flags and a
//!   matching `--object-format`. Every remaining spelling is a permutation of
//!   those, so **no `verify-pack` case is added** either; the third ordering it
//!   provides is used below by *pairing with* it, not by re-running it.
//! * **`cat-file --batch-all-objects --unordered`.** The flag documents that it
//!   promises no order, so a case comparing its stdout is only legitimate if
//!   stock is in fact stable. Measured before writing one: five runs in one
//!   repository and three runs in three separate copies of the same template
//!   produced one digest each on `Shape::Packed` (`689cf290aa5b`) and on
//!   `Shape::Linear` (`2d437f0e68f9`). It is stable on this platform, so one
//!   case is kept — over the *store* shape, where store order and oid order
//!   genuinely differ, which is the only place the flag means anything.

use crate::fixture::Shape;
use crate::runner::Case;

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

// ---------------------------------------------------------------------------
// stdin payloads
// ---------------------------------------------------------------------------
//
// Every key below is either a revision the fixture resolves, one of the two
// hash constants that are facts about SHA-1 rather than about this corpus, or
// an abbreviation `fixture.rs` asserts at build time for
// `Shape::PrefixCollision`. No id read out of a built fixture is baked in.

/// The gitlink, a tree and a blob out of one tree, in that order.
///
/// The first key is the whole point: `HEAD:sub` is a `160000` entry whose object
/// is not in the store, and every batch mode short-circuits it.
const GITLINK_TREE_BLOB: &[u8] = b"HEAD:sub\nHEAD:src\nHEAD:README.md\n";

/// The gitlink alone.
const GITLINK: &[u8] = b"HEAD:sub\n";

/// `info` on the gitlink, through `--batch-command`'s verb table rather than
/// through an implied verb.
const CMD_INFO_GITLINK: &[u8] = b"info HEAD:sub\n";

/// Two keys separated by NUL: what `-z` says the input looks like.
const TWO_REVS_NUL_INPUT: &[u8] = b"HEAD\0HEAD:README.md\0";

/// The same two keys separated by LF, fed to a reader told they are NUL
/// separated. The whole payload is then one key with two embedded newlines.
const TWO_REVS_LF_INPUT: &[u8] = b"HEAD\nHEAD:README.md\n";

/// One rev. Enough for the format cases, whose answer is about the format.
const ONE_REV: &[u8] = b"HEAD\n";

/// Both four-character collisions on `Shape::PrefixCollision`, then a key that
/// resolves — so one record after the refusals proves the reader kept going.
const AMBIGUOUS_KEYS: &[u8] = b"a366\nedfa\nHEAD\n";

/// The blob-vs-blob collision alone: the ambiguity `core.disambiguate` cannot
/// resolve, because both candidates are the same type.
const AMBIGUOUS_BLOBS: &[u8] = b"a366\n";

/// The same two blobs at five characters, where each prefix is unique again.
/// Pairs with [`AMBIGUOUS_BLOBS`]: the widening is the difference between them.
const DISAMBIGUATED_BLOBS: &[u8] = b"a3660\na3666\n";

/// A hit, a miss by name, a miss by the all-zero id, and the empty tree — which
/// is synthesized rather than stored and so is a hit in every repository.
const HIT_MISS_AND_EMPTY_TREE: &[u8] =
    b"HEAD:README.md\nno-such-rev\n0000000000000000000000000000000000000000\n4b825dc642cb6eb9a060e54bf8d69288fbee4904\n";

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    ls_tree_rows(out);
    ls_tree_format(out);
    ls_tree_scope_and_paths(out);
    cat_file_atom_tables(out);
    cat_file_framing(out);
    cat_file_ambiguous(out);
    gitlink_through_four_printers(out);
    rev_list_object_output(out);
    rev_list_filter_grammar(out);
    store_listings(out);
}

// ---------------------------------------------------------------------------
// 1. ls-tree's row printer
// ---------------------------------------------------------------------------

/// The four mutually exclusive row shapes, over the one tree that has every
/// entry kind in it.
///
/// `Shape::Submodule`'s root tree holds a blob, a tree and a `160000` gitlink,
/// and no `ls-tree` case in the corpus runs on it — the standard `read_only`
/// sweep covers Linear/Branched/Merged/Dirty/Detached, and `fixture_gaps3.rs`
/// takes `Shape::NestedSubmodule` in the three spellings it needs. So the row
/// that is hardest to print was being printed by nothing.
///
/// The `-l` column is the reason to insist on this tree: a blob's size is a
/// number, and a **tree's and a gitlink's are the literal `-`**, right-aligned
/// in the same seven-column field. A port that prints `0` there, or that pads
/// left, agrees with stock on every all-blob tree in the corpus.
///
/// `--name-status` is not a status listing despite the name — it is a second
/// spelling of `--name-only`, and the two must produce identical bytes.
fn ls_tree_rows(out: &mut Vec<Case>) {
    for args in [
        &["ls-tree", "HEAD"][..],
        &["ls-tree", "-r", "HEAD"],
        &["ls-tree", "-t", "-r", "HEAD"],
        &["ls-tree", "-d", "HEAD"],
        &["ls-tree", "-l", "HEAD"],
        &["ls-tree", "--long", "HEAD"],
        &["ls-tree", "-l", "-r", "-t", "HEAD"],
        &["ls-tree", "--object-only", "HEAD"],
        &["ls-tree", "--name-only", "HEAD"],
        &["ls-tree", "--name-status", "HEAD"],
        &["ls-tree", "--name-only", "-t", "-r", "HEAD"],
        &["ls-tree", "-z", "HEAD"],
    ] {
        c("ls-tree", args, Shape::Submodule, out);
    }

    // `-d` on its own lists only trees; `-d -r` recurses *into* them and lists
    // every tree at every depth, which is the one combination where the two
    // flags do not simply intersect. `Shape::AwkwardPaths` has `nested/deep`,
    // so the two answers differ by a row.
    c("ls-tree", &["ls-tree", "-d", "-r", "HEAD"], Shape::AwkwardPaths, out);
    c("ls-tree", &["ls-tree", "-d", "-t", "HEAD"], Shape::AwkwardPaths, out);
    c("ls-tree", &["ls-tree", "--object-only", "-d", "-r", "HEAD"], Shape::AwkwardPaths, out);

    // The three flags that select a column are mutually exclusive, and git says
    // so in one line rather than by printing usage — so the refusal is the whole
    // answer and stderr is compared.
    s("ls-tree", &["ls-tree", "--name-only", "--object-only", "HEAD"], Shape::Linear, out);
    s("ls-tree", &["ls-tree", "-l", "--object-only", "HEAD"], Shape::Linear, out);
    s("ls-tree", &["ls-tree", "--name-status", "--object-only", "HEAD"], Shape::Linear, out);

    // `--abbrev` shortens the id column. Three facts, none of them string
    // truncation: bare `--abbrev` is the configured default (7), `=0` is
    // *full length* rather than nothing, and `=1` is clamped up to git's
    // four-character floor.
    for args in [
        &["ls-tree", "--abbrev", "HEAD"][..],
        &["ls-tree", "--abbrev=0", "HEAD"],
        &["ls-tree", "--abbrev=1", "HEAD"],
        &["ls-tree", "--abbrev=40", "-r", "HEAD"],
        &["ls-tree", "--no-abbrev", "HEAD"],
    ] {
        c("ls-tree", args, Shape::Linear, out);
    }
    // …and on the shape where four characters are no longer unique, so the
    // answer is a *widening* and not a truncation. `fixture_gaps3.rs` pins the
    // two plain spellings; these are the ones that carry another column with
    // them, where a port that abbreviates in the wrong place mangles the row
    // rather than the id.
    for args in [
        &["ls-tree", "--abbrev=4", "-l", "HEAD"][..],
        &["ls-tree", "--abbrev=4", "--object-only", "HEAD"],
        &["ls-tree", "--abbrev=4", "-d", "-r", "HEAD"],
    ] {
        c("ls-tree", args, Shape::PrefixCollision, out);
    }
}

// ---------------------------------------------------------------------------
// 2. ls-tree --format
// ---------------------------------------------------------------------------

/// `--format=<fmt>`: the printer with no fixed columns at all.
///
/// Not one case in the corpus used it before this module — `grep '"ls-tree"'`
/// over `src/parity/src/corpus/` finds 47 call sites and no `--format`. That
/// left the entire atom expander unmeasured, including the two atoms that exist
/// nowhere else (`%(objectsize:padded)`, `%x09`) and the interaction that is
/// easiest to get wrong: under `-z`, `--format` switches **both** the record
/// terminator to NUL *and* path quoting off, so the same format string over the
/// same tree produces two different byte streams.
fn ls_tree_format(out: &mut Vec<Case>) {
    // Every atom at once, over the tree with a blob, a tree and a gitlink in it.
    // `%(objectsize)` is `-` for the two non-blobs, as in the `-l` column.
    for args in [
        &["ls-tree", "--format=%(objectmode) %(objecttype) %(objectname) %(objectsize)", "HEAD"][..],
        &["ls-tree", "--format=%(objectmode) %(objecttype) %(objectname) %(objectsize)", "-r", "-t", "HEAD"],
        &["ls-tree", "--format=[%(objectsize:padded)] %(path)", "HEAD"],
        &["ls-tree", "--format=%(objecttype)", "-t", "-r", "HEAD"],
        &["ls-tree", "--format=%(objectmode)", "-d", "HEAD"],
    ] {
        c("ls-tree", args, Shape::Submodule, out);
    }

    // Literal text, the percent escape, and `%x09` — the only way to put a tab
    // in a format, since the shell-facing argument cannot carry one portably.
    for args in [
        &["ls-tree", "--format=%(objectname)%x09%(path)", "-r", "HEAD"][..],
        &["ls-tree", "--format=literal %x09 tab and %% percent: %(path)", "HEAD"],
        // An empty format is legal and still emits one terminator per entry.
        &["ls-tree", "--format=", "HEAD"],
    ] {
        c("ls-tree", args, Shape::Linear, out);
    }

    // The `-z` interaction, as a pair over one format and one tree: with `-z`
    // the records are NUL-terminated and `üñïçødé.txt` is printed raw; without
    // it they are LF-terminated and the same name is C-quoted. A port that
    // treats `-z` as a terminator switch only agrees on the first and not the
    // second.
    c("ls-tree", &["ls-tree", "--format=%(path)", "HEAD"], Shape::AwkwardPaths, out);
    c("ls-tree", &["ls-tree", "-z", "--format=%(path)", "HEAD"], Shape::AwkwardPaths, out);
    c("ls-tree", &["ls-tree", "--format=%(path)", "-r", "HEAD"], Shape::AwkwardPaths, out);
    c("ls-tree", &["ls-tree", "-z", "--format=%(objectname) %(path)", "-r", "HEAD"], Shape::AwkwardPaths, out);

    // `--format` respects `--abbrev`, which is the cross-verb pair with
    // `cat_file_ambiguous` below: here `a366…` comes back as five characters,
    // there the same four-character key is refused as ambiguous.
    c("ls-tree", &["ls-tree", "--abbrev=4", "--format=%(objectname) %(path)", "HEAD"], Shape::PrefixCollision, out);
    c("ls-tree", &["ls-tree", "--abbrev=4", "--format=%(objectname)", "-r", "HEAD"], Shape::PrefixCollision, out);

    // `--format` is exclusive with every other format-altering option. The
    // refusal prints the usage block after the message, and usage prose is
    // outside the compatibility surface the runner asserts, so these compare the
    // exit code and the (empty) stdout only.
    for args in [
        &["ls-tree", "--format=%(objectname)", "--name-only", "HEAD"][..],
        &["ls-tree", "--format=%(objectname)", "-l", "HEAD"],
        &["ls-tree", "--format=%(objectname)", "--object-only", "HEAD"],
    ] {
        c("ls-tree", args, Shape::Linear, out);
    }

    // Atoms `ls-tree` does not have — three of which `cat-file` does. This is
    // one half of the atom-table pair; `cat_file_atom_tables` is the other.
    for args in [
        &["ls-tree", "--format=%(objectsize:disk)", "HEAD"][..],
        &["ls-tree", "--format=%(deltabase)", "HEAD"],
        &["ls-tree", "--format=%(rest)", "HEAD"],
        &["ls-tree", "--format=%(objectname:short)", "HEAD"],
        &["ls-tree", "--format=%(bogus)", "HEAD"],
    ] {
        s("ls-tree", args, Shape::Linear, out);
    }
}

// ---------------------------------------------------------------------------
// 3. ls-tree's path scope
// ---------------------------------------------------------------------------

/// `--full-name`, `--full-tree`, a pathspec, and the two arguments that are not
/// a tree.
///
/// The corpus runs `ls-tree` from the repository root almost everywhere, and
/// from the root `--full-name` is the identity — so the flag was pinned on
/// nothing. Run from `src/` the default prints `lib.rs`, `--full-name` prints
/// `src/lib.rs`, and `--full-tree` prints the whole tree from the root; three
/// different answers from one tree object, which is what makes the pair worth
/// running.
fn ls_tree_scope_and_paths(out: &mut Vec<Case>) {
    for args in [
        &["ls-tree", "HEAD"][..],
        &["ls-tree", "-r", "HEAD"],
        &["ls-tree", "--full-name", "-r", "HEAD"],
        &["ls-tree", "--full-tree", "-r", "HEAD"],
        &["ls-tree", "--no-full-name", "-r", "HEAD"],
    ] {
        out.push(Case::new("ls-tree", args, Shape::Branched).in_dir("src"));
    }

    // Two levels down, with names that need quoting at the other end of the
    // tree — so `--full-tree` widens the answer from one row to six and the
    // quoting is visible in the rows it adds.
    for args in [
        &["ls-tree", "-r", "--name-only", "HEAD"][..],
        &["ls-tree", "-r", "--name-only", "--full-name", "HEAD"],
        &["ls-tree", "-r", "--name-only", "--full-tree", "HEAD"],
    ] {
        out.push(Case::new("ls-tree", args, Shape::AwkwardPaths).in_dir("nested/deep"));
    }

    // A pathspec that selects the gitlink, with and without the trailing slash
    // that would mean "descend" for a tree. A `160000` entry has nothing to
    // descend into, so both spellings print the same single row and `-r` does
    // not change it.
    for args in [
        &["ls-tree", "HEAD", "sub"][..],
        &["ls-tree", "HEAD", "sub/"],
        &["ls-tree", "-r", "HEAD", "sub"],
        &["ls-tree", "-r", "-t", "HEAD", "sub"],
        &["ls-tree", "--object-only", "HEAD", "sub"],
    ] {
        c("ls-tree", args, Shape::Submodule, out);
    }

    // A pathspec matching nothing is **exit 0 and no output**, not an error —
    // the opposite of what `ls-files`' `--error-unmatch` trains a reader to
    // expect, and a plausible place for a port to invent a diagnostic.
    s("ls-tree", &["ls-tree", "HEAD", "nosuch"], Shape::Linear, out);
    s("ls-tree", &["ls-tree", "-r", "HEAD", "nosuch/deep"], Shape::AwkwardPaths, out);
    s("ls-tree", &["ls-tree", "-d", "HEAD", "README.md"], Shape::Linear, out);

    // A tree-ish that is a blob: `fatal: not a tree object`, exit 128. A tag and
    // a commit both peel to one; a blob does not, and the message says nothing
    // about which argument was wrong.
    s("ls-tree", &["ls-tree", "HEAD:README.md"], Shape::Linear, out);
    s("ls-tree", &["ls-tree", "-r", "v0.2.0^{tree}:src/lib.rs"], Shape::Branched, out);

    // The spellings that do peel, over the shape that has a tag object to peel.
    for args in [
        &["ls-tree", "--object-only", "v0.2.0"][..],
        &["ls-tree", "--format=%(objecttype) %(path)", "v0.2.0^{tree}"],
        &["ls-tree", "-l", "--abbrev=8", "v0.1.0"],
    ] {
        c("ls-tree", args, Shape::Branched, out);
    }
}

// ---------------------------------------------------------------------------
// 4. the two atom tables
// ---------------------------------------------------------------------------

/// `cat-file --batch-check=<format>`'s atom set, measured against `ls-tree`'s.
///
/// The pairing is the assertion. Over one object, with one atom each way:
///
/// | atom | `ls-tree --format=` | `cat-file --batch-check=` |
/// |---|---|---|
/// | `%(objectmode)` | `100644` | **empty, exit 0** |
/// | `%(path)` | the path | `fatal: bad cat-file format` |
/// | `%(objectsize:padded)` | right-aligned in 7 | `fatal: bad cat-file format` |
/// | `%(objectsize:disk)` | `fatal: bad ls-tree format` | the on-disk size |
/// | `%(deltabase)` | `fatal: bad ls-tree format` | the delta base or zeros |
/// | `%(rest)` | `fatal: bad ls-tree format` | the trailing words |
///
/// `%(objectmode)` is the one that cannot be guessed: it is in `cat-file`'s
/// table, it parses, and it renders to nothing because a batch request carries
/// no tree entry to take a mode from. A port that rejects it is wrong, and a
/// port that prints a mode for it is wrong in the other direction — and only
/// one of those two mistakes is one a reasonable implementer would expect to
/// make. The `ls-tree` half of the table is in [`ls_tree_format`].
fn cat_file_atom_tables(out: &mut Vec<Case>) {
    // Accepted, renders empty. Strict, because "exit 0 with an empty line" and
    // "exit 128 with a message" are told apart by stderr as much as by stdout.
    si_strict("cat-file", &["cat-file", "--batch-check=%(objectmode)"], Shape::Linear, ONE_REV, out);
    si_strict("cat-file", &["cat-file", "--batch=%(objectmode)"], Shape::Linear, ONE_REV, out);
    si_strict(
        "cat-file",
        &["cat-file", "--batch-check=[%(objectmode)] %(objecttype)"],
        Shape::Linear,
        ONE_REV,
        out,
    );

    // Rejected, with `ls-tree`'s wording for the same atom sitting in the other
    // function as the contrast.
    for args in [
        &["cat-file", "--batch-check=%(path)"][..],
        &["cat-file", "--batch-check=%(objectsize:padded)"],
        &["cat-file", "--batch=%(path)"],
        &["cat-file", "--batch-check=%(objecttype) %(path)"],
    ] {
        si_strict("cat-file", args, Shape::Linear, ONE_REV, out);
    }
}

// ---------------------------------------------------------------------------
// 5. batch record framing
// ---------------------------------------------------------------------------

/// The parts of the batch reader `object_pack.rs` does not reach.
///
/// It pins `-Z`, which switches **both** separators. `-z` switches only the
/// *input* one, and the two are told apart by exactly one thing: feeding
/// NUL-separated keys to `-z` and watching the answers come back LF-terminated.
/// A port that aliases the two flags passes every `-Z` case and fails here.
///
/// `--buffer` changes when output is written and not what is written, so its
/// assertion is that the bytes are identical to the unbuffered run — which is
/// only worth anything because a port that drops a partial buffer at exit fails
/// it. `--allow-unknown-type` likewise must be inert on objects whose type is
/// perfectly well known.
fn cat_file_framing(out: &mut Vec<Case>) {
    // `-z`: NUL in, LF out.
    si("cat-file", &["cat-file", "--batch-check", "-z"], Shape::Branched, TWO_REVS_NUL_INPUT, out);
    si("cat-file", &["cat-file", "--batch", "-z"], Shape::Branched, TWO_REVS_NUL_INPUT, out);
    // `-z` fed LF-separated keys: the whole payload is one key with newlines in
    // it, and the answer is that key echoed back followed by ` missing`.
    si("cat-file", &["cat-file", "--batch-check", "-z"], Shape::Branched, TWO_REVS_LF_INPUT, out);

    // `--buffer` on the check mode, and on the command mode without a `flush`
    // verb, where the buffer is drained only at EOF.
    si("cat-file", &["cat-file", "--batch-check", "--buffer"], Shape::Branched, TWO_REVS_LF_INPUT, out);
    si("cat-file", &["cat-file", "--batch", "--buffer"], Shape::Branched, TWO_REVS_LF_INPUT, out);

    // `--allow-unknown-type` under a batch mode. `object_pack.rs` has it on the
    // single-object `-t`; in a batch it also has to leave the size field alone,
    // which is the field the flag disables in the single-object path.
    si("cat-file", &["cat-file", "--batch-check", "--allow-unknown-type"], Shape::Branched, ONE_REV, out);
    si("cat-file", &["cat-file", "--batch", "--allow-unknown-type"], Shape::Branched, ONE_REV, out);

    // Hit, two kinds of miss, and the synthesized empty tree in one stream. The
    // empty tree is the interesting record: it is in no fixture's object store
    // and is a hit anyway, so a port answering out of its store alone prints
    // `missing` for it and agrees on the other three.
    si("cat-file", &["cat-file", "--batch-check"], Shape::Linear, HIT_MISS_AND_EMPTY_TREE, out);
    si("cat-file", &["cat-file", "--batch"], Shape::Linear, HIT_MISS_AND_EMPTY_TREE, out);
    si(
        "cat-file",
        &["cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)"],
        Shape::Linear,
        HIT_MISS_AND_EMPTY_TREE,
        out,
    );

    // `--batch-command` with `--buffer` and two `flush` verbs, so the stream is
    // drained twice mid-run rather than once at EOF. `object_pack.rs`'s payload
    // has one `info` and one `flush`; this one interleaves `contents` with the
    // flushes, which is where a port that buffers per-verb rather than per-batch
    // reorders the output.
    si(
        "cat-file",
        &["cat-file", "--batch-command", "--buffer"],
        Shape::Branched,
        b"contents HEAD:README.md\ninfo HEAD\nflush\ninfo HEAD:README.md\nflush\n",
        out,
    );
}

// ---------------------------------------------------------------------------
// 6. the `ambiguous` record
// ---------------------------------------------------------------------------

/// `<key> ambiguous`: the other error line a batch consumer has to parse.
///
/// `missing` is pinned by `object_pack.rs`. `ambiguous` was not pinned anywhere,
/// because it needs two objects sharing a four-character prefix and only
/// `Shape::PrefixCollision` has them — and that shape's `cat-file` cases are all
/// single-object, where the same condition is a `fatal:` and exit 128 instead.
/// The batch modes turn it into a **record**: `a366 ambiguous` on stdout, exit
/// 0, the reader continuing to the next key, and the candidate list on stderr as
/// `error:`/`hint:` lines. All four facts are load-bearing for a consumer and
/// all four are compared here.
///
/// The candidate report is deterministic only because `env::harden` pins the
/// committer date — the commit candidate is listed as
/// `edfab1b commit 2023-11-14 - initial`. Without that pin these could not be
/// `strict`.
fn cat_file_ambiguous(out: &mut Vec<Case>) {
    for args in [
        &["cat-file", "--batch-check"][..],
        &["cat-file", "--batch"],
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
    ] {
        si_strict("cat-file", args, Shape::PrefixCollision, AMBIGUOUS_KEYS, out);
    }

    // The blob/blob collision alone — the one no `core.disambiguate` value can
    // settle, since both candidates are the same type. Paired with the same
    // shape's `ls-tree --abbrev=4`, which prints these two ids as five
    // characters rather than refusing them.
    si_strict("cat-file", &["cat-file", "--batch-check"], Shape::PrefixCollision, AMBIGUOUS_BLOBS, out);
    si_strict("cat-file", &["cat-file", "--batch-command"], Shape::PrefixCollision, b"info a366\ncontents edfa\n", out);

    // The same two blobs one character longer, where both keys resolve. This is
    // the case that says the refusal above is about the *prefix length* and not
    // about the objects: a port that rejects anything short agrees on the
    // ambiguous payload and fails this one.
    si_strict("cat-file", &["cat-file", "--batch-check"], Shape::PrefixCollision, DISAMBIGUATED_BLOBS, out);
}

// ---------------------------------------------------------------------------
// 7. one object, four printers
// ---------------------------------------------------------------------------

/// The gitlink, asked of everything that can be asked about it.
///
/// `Shape::Submodule` records `sub` at mode `160000`. The commit it names lives
/// in the submodule's own object store and in no store the parent can read, so
/// every printer has to decide what to do about an entry whose object it cannot
/// open — and stock gives four different answers, none of which follows from the
/// other three:
///
/// ```text
/// ls-tree HEAD                     160000 commit 7c9f5d7…  sub
/// cat-file --batch-check <<HEAD:sub 7c9f5d7… submodule          (no size; exit 0)
/// cat-file -t HEAD:sub             fatal: git cat-file: could not get object info  (exit 128)
/// rev-list --objects HEAD          (the id does not appear)
/// ```
///
/// The `submodule` pseudo-type is the sharpest of the four: it is a type name
/// that exists in no other output of any git command, it is emitted *instead of*
/// the requested format — `--batch-check=%(objectmode)` prints
/// `7c9f5d7… submodule` too, not a mode — and it carries no size field where
/// every other batch record has one. A port that routes the batch reader through
/// one object-info function gets the same answer from all three `cat-file`
/// cases; stock gives two.
fn gitlink_through_four_printers(out: &mut Vec<Case>) {
    // The batch short-circuit, including the format it ignores.
    for args in [
        &["cat-file", "--batch-check"][..],
        &["cat-file", "--batch"],
        &["cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)"],
        &["cat-file", "--batch-check=%(objectmode)"],
    ] {
        si("cat-file", args, Shape::Submodule, GITLINK, out);
    }
    // The gitlink beside a tree and a blob in one stream, so the missing size
    // field is visible as a *difference between adjacent records* rather than as
    // a fact about a one-record run.
    si("cat-file", &["cat-file", "--batch-check"], Shape::Submodule, GITLINK_TREE_BLOB, out);
    si("cat-file", &["cat-file", "--batch"], Shape::Submodule, GITLINK_TREE_BLOB, out);
    si(
        "cat-file",
        &["cat-file", "--batch-check=%(objecttype)|%(objectsize)"],
        Shape::Submodule,
        GITLINK_TREE_BLOB,
        out,
    );
    // Through `--batch-command`'s verb table rather than an implied verb.
    si("cat-file", &["cat-file", "--batch-command"], Shape::Submodule, CMD_INFO_GITLINK, out);

    // The single-object path, which refuses where the batch path answers.
    s("cat-file", &["cat-file", "-t", "HEAD:sub"], Shape::Submodule, out);
    s("cat-file", &["cat-file", "-s", "HEAD:sub"], Shape::Submodule, out);
    s("cat-file", &["cat-file", "-e", "HEAD:sub"], Shape::Submodule, out);

    // And the traversal, which leaves it out. The `--object-names` spelling is
    // here rather than in `rev_list_object_output` because the fact being
    // measured is about this object: a port that emits the gitlink with an empty
    // name would be caught by the name column and by nothing else.
    c("rev-list", &["rev-list", "--objects", "HEAD"], Shape::Submodule, out);
    c("rev-list", &["rev-list", "--objects", "--object-names", "HEAD"], Shape::Submodule, out);
    c("rev-list", &["rev-list", "--objects", "--no-object-names", "HEAD"], Shape::Submodule, out);
}

// ---------------------------------------------------------------------------
// 8. rev-list's object output
// ---------------------------------------------------------------------------

/// `--objects` and the flags that change the *line*, not the walk.
///
/// `traversal_order.rs` owns which commits come out and in what order. What is
/// taken here is the second column: `--objects` prints `<oid> SP <name>` with
/// the name **empty for a tree reached as a root** and present for everything
/// else, `--object-names` is the default spelled out, `--no-object-names` drops
/// the column entirely, `--objects-edge` prefixes boundary commits with `-`, and
/// `--boundary` does the same for a different set. Four ways to decorate one
/// column, each of which a consumer parses positionally.
///
/// `--disk-usage` is in this group and not in `integrity_gc.rs` because it is
/// the same traversal with the objects summed instead of printed: `=human`
/// renders `6.82 KiB` where the default renders `6982`, and the unit choice is a
/// formatting decision made in `rev-list` itself.
fn rev_list_object_output(out: &mut Vec<Case>) {
    // `--object-names` explicitly, where the corpus had only its negation.
    c("rev-list", &["rev-list", "--objects", "--object-names", "HEAD"], Shape::Branched, out);
    c("rev-list", &["rev-list", "--objects", "--object-names", "--all"], Shape::Branched, out);
    c("rev-list", &["rev-list", "--objects", "--no-object-names", "HEAD"], Shape::Branched, out);
    // Last one wins, which is the only thing that distinguishes a pair of
    // negatable flags from a pair of independent ones.
    c("rev-list", &["rev-list", "--objects", "--no-object-names", "--object-names", "HEAD"], Shape::Branched, out);

    // The boundary decorations. `--objects-edge` prints uninteresting commits
    // with a `-` prefix and their trees; `--objects-edge-aggressive` walks
    // further back for them, so on a seven-commit history the two answers differ
    // in how many `-` rows appear.
    for args in [
        &["rev-list", "--objects-edge", "HEAD~2..HEAD"][..],
        &["rev-list", "--objects-edge-aggressive", "HEAD~2..HEAD"],
        &["rev-list", "--objects-edge-aggressive", "--all"],
        &["rev-list", "--objects", "--boundary", "HEAD~2..HEAD"],
    ] {
        c("rev-list", args, Shape::Packed, out);
    }
    c("rev-list", &["rev-list", "--objects", "--boundary", "main..feature"], Shape::Branched, out);
    c("rev-list", &["rev-list", "--objects", "--all", "--not", "main"], Shape::Branched, out);

    // `--in-commit-order` groups each commit's objects under it instead of
    // emitting all commits and then all objects. Over seven revisions of one
    // file that is a different ordering of the same 30 lines.
    c("rev-list", &["rev-list", "--in-commit-order", "--objects", "--all"], Shape::Packed, out);
    c("rev-list", &["rev-list", "--in-commit-order", "--objects", "--no-object-names", "--all"], Shape::Packed, out);

    // `--disk-usage`, in both renderings, over a store with packs and over one
    // without. The human rendering is where a port picking a different unit
    // boundary or a different number of decimals shows up.
    c("rev-list", &["rev-list", "--disk-usage=human", "--objects", "--all"], Shape::Packed, out);
    c("rev-list", &["rev-list", "--disk-usage=human", "--objects", "HEAD"], Shape::Branched, out);
    c("rev-list", &["rev-list", "--disk-usage", "--objects", "--all"], Shape::Branched, out);
    c("rev-list", &["rev-list", "--disk-usage=human", "HEAD"], Shape::Packed, out);
    s("rev-list", &["rev-list", "--disk-usage=bogus", "--objects", "HEAD"], Shape::Branched, out);
}

// ---------------------------------------------------------------------------
// 9. the --filter= spec grammar
// ---------------------------------------------------------------------------

/// Every filter spec git accepts, on repositories where nothing is missing.
///
/// The split from `graft_partial.rs` and `fixture_gaps2.rs` is deliberate and is
/// about what the case measures. On `Shape::Promisor` a filter interacts with
/// objects that are genuinely absent, and the answer is dominated by promisor
/// bookkeeping — which is theirs. Here the store is complete, so what is left is
/// exactly the spec parser and the selection rule: which objects a spec omits,
/// and what happens to a spec that does not parse.
///
/// The corpus had two specs, `blob:none` and `tree:0`, in four places. The
/// grammar has six more forms and three of them are not obvious:
/// `object:type=<t>` keeps the commits regardless, `sparse:oid=<rev>` reads a
/// *blob* as a pattern file and fails loudly when that blob is not there, and
/// `combine:<a>+<b>` intersects. `--filter` given twice is last-wins and not an
/// implicit combine, which is the mistake a port makes if it accumulates.
fn rev_list_filter_grammar(out: &mut Vec<Case>) {
    for args in [
        &["rev-list", "--objects", "--filter=object:type=blob", "HEAD"][..],
        &["rev-list", "--objects", "--filter=object:type=tree", "HEAD"],
        &["rev-list", "--objects", "--filter=object:type=commit", "HEAD"],
        &["rev-list", "--objects", "--filter=object:type=tag", "--all"],
        // Two specs intersected, versus the same two given separately — where
        // the second simply replaces the first.
        &["rev-list", "--objects", "--filter=combine:blob:none+object:type=tree", "HEAD"],
        &["rev-list", "--objects", "--filter=tree:0", "--filter=blob:none", "HEAD"],
        // `--no-filter` cancels a spec that was already given.
        &["rev-list", "--objects", "--filter=blob:none", "--no-filter", "HEAD"],
        // `--filter-provided-objects` applies the filter to the tips too, which
        // on a commit tip is the difference between listing it and not.
        &["rev-list", "--objects", "--filter=tree:0", "--filter-provided-objects", "HEAD"],
        &["rev-list", "--objects", "--filter=object:type=blob", "--filter-provided-objects", "HEAD"],
        // A blob read as a sparse-checkout pattern file. `HEAD:README.md` is
        // `# fixture\n`, which is a comment line and therefore matches nothing —
        // so every blob is omitted and every tree survives.
        &["rev-list", "--objects", "--filter=sparse:oid=HEAD:README.md", "HEAD"],
        &["rev-list", "--objects", "--filter=sparse:oid=main:src/lib.rs", "HEAD"],
    ] {
        c("rev-list", args, Shape::Branched, out);
    }

    // Depth-limited tree filters need a tree deeper than one level, which
    // `Shape::AwkwardPaths` has at `nested/deep`.
    for args in [
        &["rev-list", "--objects", "--filter=tree:1", "HEAD"][..],
        &["rev-list", "--objects", "--filter=tree:2", "HEAD"],
        &["rev-list", "--objects", "--filter=tree:3", "HEAD"],
        &["rev-list", "--objects", "--filter=combine:blob:none+tree:2", "HEAD"],
        &["rev-list", "--objects", "--filter=tree:1", "--filter-print-omitted", "HEAD"],
    ] {
        c("rev-list", args, Shape::AwkwardPaths, out);
    }

    // A size threshold with a unit suffix, over the shape whose blobs straddle
    // it — `big.txt` is 6734 bytes and `README.md` is 10, so `1k` splits them.
    // `--filter-print-omitted` prints the omitted ids with a `~` prefix, which
    // is a third line grammar on top of `<oid>` and `<oid> SP <name>`.
    for args in [
        &["rev-list", "--objects", "--filter=blob:limit=1k", "HEAD"][..],
        &["rev-list", "--objects", "--filter=blob:limit=1k", "--filter-print-omitted", "HEAD"],
        &["rev-list", "--objects", "--filter=blob:limit=1m", "HEAD"],
        &["rev-list", "--objects", "--filter=blob:limit=0", "--filter-print-omitted", "HEAD"],
    ] {
        c("rev-list", args, Shape::Packed, out);
    }

    // Specs that do not parse, and one that parses and then cannot be read.
    // Each has its own message, and a port with one "invalid filter" string for
    // all of them agrees on the exit code and on nothing else.
    for args in [
        &["rev-list", "--objects", "--filter=blob:nonesuch", "HEAD"][..],
        &["rev-list", "--objects", "--filter=bogus:spec", "HEAD"],
        &["rev-list", "--objects", "--filter=object:type=bogus", "HEAD"],
        &["rev-list", "--objects", "--filter=tree:notanumber", "HEAD"],
        &["rev-list", "--objects", "--filter=sparse:oid=HEAD:nosuch", "HEAD"],
        &["rev-list", "--objects", "--filter=combine:blob:none+bogus:spec", "HEAD"],
    ] {
        s("rev-list", args, Shape::Branched, out);
    }
}

// ---------------------------------------------------------------------------
// 10. three orderings of one store
// ---------------------------------------------------------------------------

/// The same object store, enumerated three ways.
///
/// `Shape::Packed` holds 26 objects in two packs with a two-deep delta chain.
/// Three commands will list all of them and no two agree on the order:
///
/// * `cat-file --batch-all-objects --batch-check` — ascending oid. That exact
///   argv over this shape is `object_format.rs`'s, so what is run here is the
///   `%(objectname)`-only spelling of it, beside the `--unordered` one.
/// * `cat-file --batch-all-objects --unordered --batch-check` — store order:
///   pack order for packed objects, directory order for loose ones.
/// * `verify-pack -v packs/sample.idx` — ascending offset within the pack, with
///   the delta chain summary after it.
///
/// The third is `object_pack.rs`'s and `shape_reach.rs`'s and is not re-run
/// here; the first two are, so that a port whose store iterator produces one
/// order for both fails exactly one of them, and a port whose `--batch-all-objects`
/// set differs from what `verify-pack` can see fails against a case those
/// modules already own. Cases are paired by format rather than by flag —
/// `%(objectname)` alone on both — because the comparison is about the sequence
/// and a size column would hide a reordering behind a diff on every line.
///
/// `--unordered` promises no order. It is compared anyway, but only after
/// measuring that stock is stable: five runs in one repository and three runs in
/// three separate copies of the template each produced one digest. If that ever
/// stops being true on another platform the case will flake on the stock-only
/// baseline run, which is the loud failure rather than the quiet one.
fn store_listings(out: &mut Vec<Case>) {
    for args in [
        &["cat-file", "--batch-all-objects", "--batch-check=%(objectname)"][..],
        &["cat-file", "--batch-all-objects", "--unordered", "--batch-check=%(objectname)"],
        &["cat-file", "--batch-all-objects", "--unordered", "--batch-check"],
    ] {
        c("cat-file", args, Shape::Packed, out);
    }

    // The whole store as *contents* rather than as checks. Binary tree payloads
    // land on stdout verbatim, which is the assertion: a port that
    // newline-normalizes or re-encodes a tree body is invisible to every
    // `--batch-check` case in the corpus and obvious here.
    c("cat-file", &["cat-file", "--batch-all-objects", "--batch"], Shape::Linear, out);
    c("cat-file", &["cat-file", "--batch-all-objects", "--batch=%(objecttype)"], Shape::Linear, out);

    // And the traversal's view of the same store, which is a fourth order again
    // and — unlike the three above — is reachability-limited rather than
    // store-limited, so the two answers differ by the unreachable commit
    // `Shape::Packed` deliberately keeps.
    c("rev-list", &["rev-list", "--objects", "--no-object-names", "--all"], Shape::Packed, out);
}
