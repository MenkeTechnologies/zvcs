//! Differential corpus cases for `git archive` **as a format writer** — the
//! bytes some other program has to parse.
//!
//! Every case here puts the archive on **stdout**, which
//! [`crate::runner::normalize`] compares byte for byte: valid UTF-8 is compared
//! as text and anything else is rendered exactly by `render_binary`, so a zip or
//! a gzip member is as sharply compared as a plain tar. That is the whole
//! instrument. Nothing in this file asserts content through a file the state
//! probe cannot open; see "What is not measurable" below for the one place that
//! bites.
//!
//! # Territory, against every module that already touches `archive`
//!
//! | module | what it owns |
//! |---|---|
//! | `corpus/info_attrs.rs` | the smoke pass: one invocation per built-in format and per top-level option on `Linear`, `-o out.tar` / `-o out.zip`, and the four bare refusals (`archive` with no arguments, `--format=nope`, a missing rev, a missing path) |
//! | `corpus/archive_export.rs` | the nearest neighbour, and the biggest: compression levels for zip and `tar.gz`, `--add-file`/`--add-virtual-file` on `Linear`, the tree-ish and pathspec forms, `tar.umask=0`/`user`, `tar.<fmt>.command=cat` **through `-c`**, `--output=` inference, `--remote=.` on `Linear`/`Branched`, and five stderr-compared refusals. Also `fast-export`, `fast-import`, `get-tar-commit-id`, `upload-archive`, `difftool`, `mergetool` |
//! | `corpus/attributes_filters.rs` | the clean/smudge filter and `core.autocrlf`/`core.eol` conversion `archive` performs on the way out, on `Attributes` and `Whitespace` |
//! | `corpus/eol_conversion.rs` | the `core.eol` × `core.autocrlf` × `text` precedence lattice, using `archive` as one of its three instruments |
//! | `corpus/transport_local.rs` | `upload-archive` as a *server*: the pkt-line request, the repository operand, `-h` |
//! | `corpus/misc_commands.rs` | `upload-archive--writer` as an unreachable internal helper |
//!
//! **What is here and in none of them** is the archive's *structure*: the tar
//! header fields none of them makes interesting, the container over trees
//! only ever tarred, the verbose listing on stderr, `tar.umask`'s number parser,
//! and the server-side format gate that only a **repository-scoped** setting can
//! reach.
//!
//! # The four things this file adds
//!
//! **1. The tar name field, which is 100 bytes wide.** Nothing in the corpus
//! archived a path longer than `patches/symlink.patch`, so the two fallbacks
//! `archive-tar.c` has for a long name were both dead code as far as this
//! harness was concerned. Both are reachable with no fixture change, because
//! `--add-virtual-file` names an entry that need not exist anywhere:
//!
//! * A 149-byte path whose last component is short splits into the ustar
//!   `prefix` field. Measured on stock 2.55.0: the entry's header carries
//!   `name = "leaf.txt"` at offset 0 and `prefix = "dir1/dir2/…/dir25"` at
//!   offset 345, and `tar tvf` reassembles the whole path.
//! * A 124-byte *single component* cannot be split at a `/`, so git emits a pax
//!   extended header first — a `.paxheader` entry holding the record
//!   `134 path=nnn…nnn.txt`, followed by the real entry renamed `.data`. Two
//!   header blocks and a body where a short name needs one block.
//!
//! `--prefix=` reaches the same two branches from the other side, for entries
//! that *are* in the tree, and the zip container is measured over both names
//! because its central directory stores the name a second time with no length
//! limit at all.
//!
//! **2. The mode and type fields, per entry kind.** A tar header's mode is not
//! the blob's — `archive-tar.c` writes `0666`/`0777` masked by `tar.umask` — and
//! its type byte distinguishes a file, a directory and a symlink.
//! `--add-file`, uniquely, takes the mode from the **filesystem**, and
//! `Shape::Hooked` is the only fixture holding an executable file
//! (`.git/hooks/pre-commit`, 0755). Measured: the entry is `-rwxrwxr-x`, named
//! by its **basename** `pre-commit` rather than by the path given, and in the
//! zip container it is the one entry written with a Unix external-attribute
//! block (`2.3 unx -rwxr-xr-x` from `unzip -Z`) beside tree entries written as
//! `0.0 fat -rw----`. Two encodings of one bit in one central directory.
//!
//! **3. The verbose listing, which is stderr and was never compared.**
//! `archive_export.rs` has an `-v` case; `compare_stderr` is off, so the listing
//! itself was invisible. The strict cases here found two disagreements, both
//! recorded in [`verbose_listing`].
//!
//! **4. The server-side format gate, which `-c` cannot reach.** `--remote=.`
//! spawns `upload-archive` in the fixture, and that server reads its own
//! `.git/config` — it does **not** see the client's `-c`. Verified both ways on
//! stock 2.55.0: with `tar.tar.foo.command=cat` on the command line the server
//! answers `Unknown archive format 'tar.foo'`, and with the same setting in
//! `.git/config` it still refuses, because a filter format is offered remotely
//! only when `tar.<fmt>.remote` is *also* true. `ConfigScope::Repo` is the only
//! spelling that puts the setting where the server looks, and no case in the
//! corpus had used it for `tar.*`.
//!
//! # Determinism, established before anything was written
//!
//! `archive` stamps every entry from the archived commit's date, which
//! [`crate::env::FIXED_DATE`] pins, and writes the gzip member with a zeroed
//! mtime. Confirmed here rather than inherited: on `Linear`, two runs of stock
//! git a second apart from two fresh copies of the template are byte-identical
//! for all four built-in formats — `tar` 10240, `zip` 426, `tar.gz` 295,
//! `tgz` 295. `--remote=.` was measured the same way over five runs each and is
//! stable for `tar`, `zip` and `tgz` alike; the instability
//! `archive_export.rs` records is in `upload-archive`'s own sideband framing,
//! which the client demultiplexes away before this comparison sees it.
//!
//! # What is not measurable, and why
//!
//! * **`--output=<file>` content.** [`crate::runner::probe_state`] reads the
//!   worktree through `status --porcelain -uall`, which reports the file's
//!   *name*. Nothing opens it. So `-o out.tgz` and `-o out.tar.bz2` pin the
//!   suffix→format inference only as far as the exit code, and a port that
//!   inferred `zip` where stock inferred `tar` would still pass. The two cases
//!   in [`output_target`] are marked as exit-code-only where they are written.
//!   Routing the stream back to stdout with `-o /dev/stdout` was tried and
//!   rejected: it is byte-stable and both sides agree on it, but the name has no
//!   suffix, so it measures the format git was *told* rather than the one it
//!   inferred — the same bytes plain stdout already carries.
//! * **`tar.<fmt>.command` failing.** `tar.tar.foo.command=false` is a race on
//!   stock alone. Measured ten times into a regular file: exit 128 nine times
//!   and 141 (SIGPIPE) once; ten times into a pipe: exit 0 every time. Git is
//!   racing its own write into the filter against the filter's exit, so no
//!   verdict here is a fact about the port. Excluded. The reachable half —
//!   a `tar.<fmt>.command` naming a program that does not exist — is
//!   deterministic (exit 128, both sides agree) and is kept.
//! * **What the filter is handed.** A `tar.<fmt>.command` that reported its own
//!   argv would answer whether the `-9` reached it, but every way of writing one
//!   closes its stdin and re-enters the race above. Not measured.
//! * **`export-subst`.** Still unreachable, for the reason `archive_export.rs`
//!   gives: it needs the attribute set on a path inside the archived tree, no
//!   fixture's `.gitattributes` sets it (`Shape::Attributes` carries
//!   `*.md diff=markdown export-ignore` and nothing else `archive` consults),
//!   and a case is one argv against a pristine copy. `.git/info/attributes` is
//!   in that fixture and *is* consulted, but it sets `ident` on `*.info` and
//!   `text` on `info-only.txt`, neither of which names a tracked path. Nothing
//!   in this file substitutes a `$Format:…$` placeholder, and nothing can until
//!   a fixture sets the attribute.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// A 149-byte path whose final component is short: too long for the tar `name`
/// field, splittable at a `/` into `prefix` + `name`.
///
/// Read only by the test below. Every argv token in the corpus is a literal, so
/// the path itself lives in the operand; this is the spelling the test checks
/// that operand against.
#[cfg(test)]
const SPLIT_PATH: &str = "dir1/dir2/dir3/dir4/dir5/dir6/dir7/dir8/dir9/dir10/dir11/dir12/dir13/dir14/dir15/dir16/dir17/dir18/dir19/dir20/dir21/dir22/dir23/dir24/dir25/leaf.txt";

/// A 124-byte single component: too long for `name` and with no `/` to split
/// at, so the only encoding left is a pax extended header. Read only by the
/// test below, for the reason above.
#[cfg(test)]
const PAX_PATH: &str = "nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn.txt";

/// The two paths above as whole `--add-virtual-file` operands, and as whole
/// `--prefix` operands. Spelled out rather than assembled so every token in a
/// case's argv is a literal, as everywhere else in the corpus.
const ADD_SPLIT: &str = "--add-virtual-file=dir1/dir2/dir3/dir4/dir5/dir6/dir7/dir8/dir9/dir10/dir11/dir12/dir13/dir14/dir15/dir16/dir17/dir18/dir19/dir20/dir21/dir22/dir23/dir24/dir25/leaf.txt:x";
const ADD_PAX: &str = "--add-virtual-file=nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn.txt:y";
const PREFIX_SPLIT: &str = "--prefix=dir1/dir2/dir3/dir4/dir5/dir6/dir7/dir8/dir9/dir10/dir11/dir12/dir13/dir14/dir15/dir16/dir17/dir18/dir19/dir20/dir21/dir22/dir23/dir24/dir25/leaf/";
const PREFIX_PAX: &str = "--prefix=pppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppp/";

/// The one executable file any fixture holds. `Shape::Hooked` installs its hooks
/// at 0755 (`fixture.rs`), and `--add-file` is the only option that reads a mode
/// off the filesystem rather than out of a tree.
const ADD_EXECUTABLE: &str = "--add-file=.git/hooks/pre-commit";

pub fn cases(out: &mut Vec<Case>) {
    tar_name_field(out);
    entry_mode_and_type(out);
    tar_umask_parse(out);
    container_over_shapes(out);
    verbose_listing(out);
    remote_format_gate(out);
    remote_added_entries(out);
    output_target(out);
    which_repository(out);
}

fn one(out: &mut Vec<Case>, args: &[&str], shape: Shape) {
    out.push(Case::new("archive", args, shape));
}

/// One case with configuration delivered from `.git/config` rather than `-c`.
///
/// The distinction is the whole of [`remote_format_gate`]: `upload-archive` runs
/// as a separate process in the fixture and reads that file, and never sees the
/// client's command line.
fn repo_cfg(out: &mut Vec<Case>, args: &[&str], shape: Shape, cfg: &[(&str, &str)]) {
    out.push(Case::new("archive", args, shape).with_scoped_config(
        cfg.iter().map(|(k, v)| ConfigEntry::set(ConfigScope::Repo, *k, *v)).collect(),
    ));
}

// ---------------------------------------------------------------------------
// The tar name field
// ---------------------------------------------------------------------------

/// The 100-byte `name` field, and the two things git does when a path will not
/// fit in it.
///
/// Both are whole encodings rather than truncations, and a port that implements
/// one and not the other writes an archive `tar` still reads — with the wrong
/// path in it, or with a `.paxheader` entry left in the extracted tree. Measured
/// on stock 2.55.0 for `SPLIT_PATH`: header block 7 of the stream carries
/// `leaf.txt` at offset 0 and `dir1/dir2/…/dir25` at offset 345, and
/// `tar tvf` prints the reassembled 149-byte path. For `PAX_PATH`: block 7 is
/// a `.paxheader` entry, block 8 is the record `134 path=nnn…nnn.txt`, block 9
/// is the real entry under the name `.data`, and `tar tvf` prints the 124-byte
/// name.
///
/// The zip cases are not repetition: a zip central directory stores the name
/// again, in full, with no 100-byte field to overflow, so the two containers
/// disagree about what "too long" even means and only one of them may invent a
/// second header.
///
/// `--prefix` reaches both branches for entries that come out of the tree rather
/// than out of argv, which is a different call site in `archive.c`, and it
/// reaches the pax branch by a different route again: the 121-byte directory
/// entry a 120-character prefix produces has its only `/` at the very end, so
/// there is nothing to split at. Measured — the record is `133 path=ppp…p/`,
/// and the two carrier entries are named from a hash of the path
/// (`e0e1a776261f58b1c8741e37…`) rather than `.paxheader`/`.data`, because git
/// derives that name from the entry it is standing in for. A port that hard-codes
/// `.paxheader` writes an archive `tar` still reads and `diff -r` still finds
/// different.
fn tar_name_field(out: &mut Vec<Case>) {
    one(out, &["archive", "--format=tar", ADD_SPLIT, "HEAD"], Shape::Linear);
    one(out, &["archive", "--format=tar", ADD_PAX, "HEAD"], Shape::Linear);
    one(out, &["archive", "--format=zip", ADD_SPLIT, "HEAD"], Shape::Linear);
    one(out, &["archive", "--format=zip", ADD_PAX, "HEAD"], Shape::Linear);
    one(out, &["archive", "--format=tar", PREFIX_SPLIT, "HEAD"], Shape::Linear);
    one(out, &["archive", "--format=tar", PREFIX_PAX, "HEAD"], Shape::Linear);
    // Through the gzip filter as well: the compressed member is the same tar,
    // and a port that writes the pax record only on the uncompressed path is a
    // real shape of defect rather than an invented one.
    one(out, &["archive", "--format=tar.gz", ADD_PAX, "HEAD"], Shape::Linear);
}

// ---------------------------------------------------------------------------
// Mode and type
// ---------------------------------------------------------------------------

/// The mode field and the type byte, over the entry kinds the fixtures can
/// produce.
///
/// A tar mode is not the blob's mode: `archive-tar.c` writes `0666` for a file
/// and `0777` for a directory, masked by `tar.umask`, so the only entry whose
/// mode carries information is one git did not read out of a tree.
/// `--add-file` is that entry, and `Shape::Hooked`'s `pre-commit` is the only
/// executable file in any fixture. Measured on stock: `-rwxrwxr-x 0 root root
/// 81 Nov 14 2023 pre-commit` — the executable bit survives, the mode is
/// masked to 0775 rather than passed through as the filesystem's 0755, and the
/// **name is the basename**, not the path that was given.
///
/// The zip half is the same bit in a different place. `unzip -Z` on stock's
/// output shows the added entry as `2.3 unx -rwxr-xr-x` and every tree entry as
/// `0.0 fat -rw----`: git writes a Unix external-attribute block for one and a
/// DOS one for the others, in one central directory.
///
/// `Shape::Symlinks` carries the type byte a tree can produce — `2`, with a
/// zero-length body and the target in the `linkname` field — plus zero-length
/// regular files and an empty directory. It is here through the **gzip** filter,
/// which `archive_export.rs` does not do, and under a non-default `tar.umask`,
/// which changes the mode of a symlink entry and of a directory entry
/// differently from that of a file.
fn entry_mode_and_type(out: &mut Vec<Case>) {
    one(out, &["archive", "--format=tar", ADD_EXECUTABLE, "HEAD"], Shape::Hooked);
    one(out, &["archive", "--format=zip", ADD_EXECUTABLE, "HEAD"], Shape::Hooked);
    out.push(
        Case::new("archive", &["archive", "--format=tar", ADD_EXECUTABLE, "HEAD"], Shape::Hooked)
            .with_config(&[("tar.umask", "0")]),
    );
    one(out, &["archive", "--format=tar.gz", "HEAD"], Shape::Symlinks);
    out.push(
        Case::new("archive", &["archive", "--format=tar", "HEAD"], Shape::Symlinks)
            .with_config(&[("tar.umask", "077")]),
    );
}

// ---------------------------------------------------------------------------
// tar.umask
// ---------------------------------------------------------------------------

/// `tar.umask` as a **number**, which is the part that disagrees.
///
/// `archive_export.rs` measures what the setting *does* (`0` and `user`, two
/// different masks in the header). What it never asks is how the value is
/// parsed, and `archive-tar.c` reads it with `git_config_int` — the same parser
/// `core.bigFileThreshold` uses, which accepts a leading `0` as octal, a leading
/// `0x` as hex, a `k`/`m`/`g` suffix as a multiplier, and a negative number.
///
/// Measured on stock 2.55.0 and corroborated by /usr/bin/git 2.50.1, reading the
/// first entry's mode out of `tar tvf`:
///
/// | value | stock exit | first entry |
/// |---|---|---|
/// | `0022` | 0 | `-rw-r--r--` |
/// | `077`  | 0 | `-rw-------` |
/// | `18`   | 0 | `-rw-r--r--` (decimal 18 == octal 022) |
/// | `0x12` | 0 | `-rw-r--r--` |
/// | `0777` | 0 | `----------` |
/// | `1k`   | 0 | `-rw-rw-rw-` (1024 == 0o2000, no permission bit) |
/// | `1m`   | 0 | `-rw-rw-rw-` |
/// | `-1`   | 0 | `----------` |
/// | ``     | 128 | `fatal: bad numeric config value '' for 'tar.umask': invalid unit` |
///
/// The empty case is stderr-compared: an empty value is not "no value", and the
/// refusal names the key and the reason, both of which are the behaviour.
fn tar_umask_parse(out: &mut Vec<Case>) {
    for value in ["0022", "077", "18", "0x12", "0777", "1k", "1m", "-1"] {
        out.push(
            Case::new("archive", &["archive", "--format=tar", "HEAD"], Shape::Linear)
                .with_config(&[("tar.umask", value)]),
        );
    }
    out.push(
        Case::strict("archive", &["archive", "--format=tar", "HEAD"], Shape::Linear)
            .with_config(&[("tar.umask", "")]),
    );
}

// ---------------------------------------------------------------------------
// The container over trees that were only ever tarred
// ---------------------------------------------------------------------------

/// The same trees, in the containers nothing put them in.
///
/// `info_attrs.rs` archives `Shape::AwkwardPaths` and `Shape::Submodule` as
/// `tar` and nothing else; `archive_export.rs` archives `Shape::DecomposedPaths`
/// and `Shape::Packed` as `tar` and nothing else. A tar header holds a path as
/// bytes in one field; a zip holds it twice — once in the local header and once
/// in the central directory — and sets a flag bit to say the bytes are UTF-8. A
/// `tar.gz` holds it inside a deflate stream. Those are three different writers
/// and one of them getting a name wrong is invisible to the other two.
///
/// The names at stake, verified present in the fixtures: `üñïçødé.txt`,
/// `with space.txt` and `quote"name.txt` on `AwkwardPaths`; a combining mark in
/// decomposed form on `DecomposedPaths`; a gitlink on `Submodule`, which is a
/// tree entry with no blob behind it.
///
/// The remaining shapes are here because the *source* of the entries differs,
/// not the container: `Sparse` archives paths the index has marked
/// skip-worktree, `NoIndexTrees` has trees the index never held, `Shallow`
/// archives a tip whose parent is not in the repository, `Promisor` a tree whose
/// blobs are absent from a partial clone, and `Damaged` a repository stock's own
/// `fsck` rejects. Each one is a way for a reader of the tree to fail before the
/// writer ever runs. `Shallow` and `Promisor` already have a `--format=tar` case
/// in `fixture_gaps2.rs`, so `Shallow` is here as the zip container over the
/// same tree; `Damaged`, `Sparse` and `NoIndexTrees` had no `archive` case at
/// all.
fn container_over_shapes(out: &mut Vec<Case>) {
    one(out, &["archive", "--format=zip", "HEAD"], Shape::AwkwardPaths);
    one(out, &["archive", "--format=zip", "-0", "HEAD"], Shape::AwkwardPaths);
    one(out, &["archive", "--format=tar.gz", "HEAD"], Shape::AwkwardPaths);
    one(out, &["archive", "--format=zip", "HEAD"], Shape::DecomposedPaths);
    one(out, &["archive", "--format=zip", "HEAD"], Shape::Submodule);
    one(out, &["archive", "--format=tar.gz", "HEAD"], Shape::Submodule);
    one(out, &["archive", "--format=zip", "HEAD"], Shape::Attributes);
    one(out, &["archive", "--format=tgz", "HEAD"], Shape::Attributes);
    one(out, &["archive", "--format=zip", "-0", "HEAD"], Shape::Packed);
    one(out, &["archive", "--format=zip", "-6", "HEAD"], Shape::Packed);
    one(out, &["archive", "--format=tar", "HEAD"], Shape::Sparse);
    one(out, &["archive", "--format=zip", "HEAD"], Shape::Sparse);
    one(out, &["archive", "--format=tar", "HEAD"], Shape::NoIndexTrees);
    one(out, &["archive", "--format=zip", "HEAD"], Shape::Shallow);
    one(out, &["archive", "--format=tar", "HEAD"], Shape::Promisor);
    one(out, &["archive", "--format=tar", "HEAD"], Shape::Damaged);
}

// ---------------------------------------------------------------------------
// -v, which is stderr
// ---------------------------------------------------------------------------

/// `-v`, compared on **stderr**, which is where it writes.
///
/// `archive_export.rs` has one `-v` case and does not compare stderr, so the
/// listing — the only thing `-v` produces — was never looked at. Two
/// disagreements, both reproduced against /usr/bin/git 2.50.1 as well as stock
/// 2.55.0:
///
/// * **`-v --format=zip` prints nothing from the port.** Stock and the oracle
///   both write the entry list (`README.md\nsrc/\nsrc/lib.rs\n`, 26 bytes, on
///   `Linear`); the port writes zero bytes. The tar backend's listing is
///   correct, so the verbose hook is wired into one archiver and not the other.
/// * **`-v` lists entries stock does not.** With
///   `--add-virtual-file=v.txt:V`, stock and the oracle list the three tree
///   entries and stop; the port lists a fourth line, `v.txt`. Same with
///   `--add-file`: the port names `pre-commit`, stock does not. Git reports what
///   it walked, the port reports what it wrote.
///
/// The `tar` cases on `Symlinks` and `AwkwardPaths` are the controls, and they
/// agree: the listing is one path per line with no quoting of a space, a
/// double quote or a non-ASCII byte, which is the opposite of what
/// `core.quotePath` does to the same names elsewhere.
fn verbose_listing(out: &mut Vec<Case>) {
    out.push(Case::strict("archive", &["archive", "-v", "--format=tar", "HEAD"], Shape::Symlinks));
    out.push(Case::strict(
        "archive",
        &["archive", "-v", "--format=tar", "HEAD"],
        Shape::AwkwardPaths,
    ));
    out.push(Case::strict("archive", &["archive", "-v", "--format=zip", "HEAD"], Shape::Linear));
    out.push(Case::strict(
        "archive",
        &["archive", "-v", "--format=zip", "HEAD"],
        Shape::Symlinks,
    ));
    out.push(Case::strict(
        "archive",
        &["archive", "-v", "--format=tar", "--add-virtual-file=v.txt:V", "HEAD"],
        Shape::Linear,
    ));
    out.push(Case::strict(
        "archive",
        &["archive", "-v", "--format=tar", ADD_EXECUTABLE, "HEAD"],
        Shape::Hooked,
    ));
}

// ---------------------------------------------------------------------------
// The server-side format gate
// ---------------------------------------------------------------------------

/// Which formats a `--remote` caller may ask for, decided by the **server's**
/// configuration.
///
/// `--remote=.` spawns `upload-archive` in the fixture. That process reads the
/// repository's own `.git/config` and never sees the client's `-c`, so
/// [`ConfigScope::Repo`] is the only spelling that can reach it — which is why
/// `archive_export.rs`'s command-line `tar.tgz.remote=false` case measures the
/// client refusing to pass a setting on, and these measure the server acting on
/// one. Different scope, different subject, no overlap.
///
/// Git's rule (`archive.c`, `archive-tar.c`): `tar` and `zip` are built in and
/// always offered remotely; `tgz` and `tar.gz` are *filter* formats registered
/// with a default `remote` of true; a format invented by `tar.<fmt>.command` is
/// offered remotely only if `tar.<fmt>.remote` is also true. Every row below was
/// measured on stock 2.55.0 and reproduced on /usr/bin/git 2.50.1.
///
/// | `.git/config` | argv | stock |
/// |---|---|---|
/// | `tar.tar.foo.command=cat` | `--remote=. --format=tar.foo` | exit 1, `Unknown archive format 'tar.foo'` |
/// | `tar.tar.foo.command=cat` | `--remote=. --list` | `tar tgz tar.gz zip` — `tar.foo` absent |
/// | + `tar.tar.foo.remote=true` | `--remote=. --format=tar.foo` | exit 0, 10240 bytes |
/// | + `tar.tar.foo.remote=true` | `--remote=. --list` | `tar tgz tar.gz tar.foo zip` |
/// | `tar.tgz.remote=false` | `--remote=. --format=tgz` | exit 1, `Unknown archive format 'tgz'` |
/// | `tar.tgz.remote=false` | `--remote=. --list` | `tar tar.gz zip` |
/// | `tar.tgz.remote=false` | `--list` (local) | `tar tgz tar.gz zip` — unchanged |
/// | `tar.zip.remote=false` | `--remote=. --format=zip` | exit 0 — a built-in archiver ignores the flag |
/// | `tar.tar.remote=false` | `--remote=.` | exit 0 — likewise |
///
/// The last three rows are the controls that make the others mean something: a
/// port that simply refused everything named in a `tar.<fmt>.remote=false` would
/// pass the gate cases and fail these.
fn remote_format_gate(out: &mut Vec<Case>) {
    let cmd_only: &[(&str, &str)] = &[("tar.tar.foo.command", "cat")];
    let cmd_remote: &[(&str, &str)] =
        &[("tar.tar.foo.command", "cat"), ("tar.tar.foo.remote", "true")];
    let cmd_no_remote: &[(&str, &str)] =
        &[("tar.tar.foo.command", "cat"), ("tar.tar.foo.remote", "false")];

    repo_cfg(out, &["archive", "--remote=.", "--format=tar.foo", "HEAD"], Shape::Linear, cmd_only);
    repo_cfg(out, &["archive", "--remote=.", "--list"], Shape::Linear, cmd_only);
    repo_cfg(out, &["archive", "--remote=.", "--format=tar.foo", "HEAD"], Shape::Linear, cmd_remote);
    repo_cfg(out, &["archive", "--remote=.", "--list"], Shape::Linear, cmd_remote);
    repo_cfg(out, &["archive", "--remote=.", "--list"], Shape::Linear, cmd_no_remote);
    // The format is still defined *locally* however the remote flag is set:
    // `--list` without `--remote` answers out of the same file and must not move.
    repo_cfg(out, &["archive", "--list"], Shape::Linear, cmd_only);

    for (key, format) in [("tar.tgz.remote", "tgz"), ("tar.tar.gz.remote", "tar.gz")] {
        let cfg: &[(&str, &str)] = &[(key, "false")];
        let arg = if format == "tgz" { "--format=tgz" } else { "--format=tar.gz" };
        repo_cfg(out, &["archive", "--remote=.", arg, "HEAD"], Shape::Linear, cfg);
        repo_cfg(out, &["archive", "--remote=.", "--list"], Shape::Linear, cfg);
        repo_cfg(out, &["archive", "--list"], Shape::Linear, cfg);
    }

    // The controls: a built-in archiver has no remote flag to clear.
    repo_cfg(
        out,
        &["archive", "--remote=.", "--format=zip", "HEAD"],
        Shape::Linear,
        &[("tar.zip.remote", "false")],
    );
    repo_cfg(
        out,
        &["archive", "--remote=.", "HEAD"],
        Shape::Linear,
        &[("tar.tar.remote", "false")],
    );

    // A format whose command does not exist is a refusal rather than a race:
    // git resolves the filter before it writes anything, so there is no
    // half-written stream and no signal to lose. (`command=false` *is* a race
    // and is excluded; see the module header.)
    out.push(
        Case::new("archive", &["archive", "--format=tar.foo", "HEAD"], Shape::Linear)
            .with_config(&[("tar.tar.foo.command", "no-such-filter-command")]),
    );
}

/// `--add-file` and `--add-virtual-file` under `--remote`, which git refuses.
///
/// Both options are rejected server-side with `options '--add-file' and
/// '--remote' cannot be used together`, and the refusal arrives over the error
/// band as the client's exit 1. Measured on stock 2.55.0 and reproduced on
/// 2.50.1.
///
/// `--add-file=README.md` takes a second path to the same refusal and is worth
/// its own case: the server reads the file **before** it checks the conflict,
/// and it reads it from wherever `enter_repo` left it — which for a non-bare
/// repository is the git directory, not the worktree. So a path that plainly
/// exists in the fixture comes back as `File not found: README.md`. A port that
/// resolves the operand on the client side, where the file is right there, gets
/// a different answer for a reason that has nothing to do with the archive.
///
/// stderr is not compared here: the message is assembled from two processes'
/// output over a sideband and the interleaving of the `remote:` lines with the
/// client's own is not stable enough to be a contract. The exit code and the
/// stream are.
fn remote_added_entries(out: &mut Vec<Case>) {
    one(
        out,
        &["archive", "--remote=.", "--format=tar", "--add-virtual-file=v.txt:V", "HEAD"],
        Shape::Linear,
    );
    one(
        out,
        &["archive", "--remote=.", "--format=tar", "--add-file=README.md", "HEAD"],
        Shape::Linear,
    );
}

// ---------------------------------------------------------------------------
// Where the stream lands
// ---------------------------------------------------------------------------

/// `--output`, which is the one surface here the digest cannot read.
///
/// The state probe lists the written file by name and never opens it, so these
/// are **exit-code and filename** cases and not content assertions — stated here
/// rather than implied, because the format inference they exercise is exactly
/// the kind of thing that would look covered and not be. What they do pin:
/// `archive.c` infers the format from the filename suffix, `.tar.bz2` and a
/// bare `out` are both suffixes it does not know, and neither is an error —
/// stock writes a plain tar for both (verified with `file(1)`: `POSIX tar
/// archive`, 10240 bytes).
///
/// The refusal is a content assertion in the only sense available: a path whose
/// parent directory does not exist. Stock exits 128 with `fatal: could not open
/// 'nodir/out.tar' for writing: No such file or directory`, on both git
/// versions, and nothing is created. stderr is compared because the sentence
/// names the path and the reason, and a port that reports a bare errno has not
/// said which file it failed on.
fn output_target(out: &mut Vec<Case>) {
    one(out, &["archive", "-o", "out.tar.bz2", "HEAD"], Shape::Linear);
    one(out, &["archive", "-o", "out", "HEAD"], Shape::Linear);
    out.push(Case::strict(
        "archive",
        &["archive", "--format=tar", "--output=nodir/out.tar", "HEAD"],
        Shape::Linear,
    ));
}

// ---------------------------------------------------------------------------
// Which repository the stream comes out of
// ---------------------------------------------------------------------------

/// The repository `archive` decides it is in, before it writes a byte.
///
/// Every `archive` case in the corpus runs at a worktree root of a non-bare
/// repository. Three other placements exist in the fixtures and none was
/// reached:
///
/// * **Inside a bare repository.** `Shape::BehindRemote` keeps a real bare peer
///   at `.remote.git`, three commits ahead of the local `main`. Running there
///   means there is no worktree to fall back on for `--worktree-attributes` and
///   no index, and the tree named is the peer's `main` rather than the
///   fixture's — a port that resolved the ref against the wrong repository
///   would produce a valid archive of the wrong content.
/// * **Named as a git directory from outside it.** The same peer reached with
///   `--git-dir` instead of by moving into it, which is a different branch of
///   `setup_git_directory`.
/// * **Inside a linked worktree.** `Shape::Worktree`'s `wt` has its own `HEAD`
///   and a `.git` *file* pointing into the main repository's
///   `worktrees/` directory.
fn which_repository(out: &mut Vec<Case>) {
    out.push(
        Case::new("archive", &["archive", "--format=tar", "main"], Shape::BehindRemote)
            .in_dir(".remote.git"),
    );
    out.push(
        Case::new("archive", &["archive", "--format=zip", "main"], Shape::BehindRemote)
            .in_dir(".remote.git"),
    );
    out.push(
        Case::new("archive", &["archive", "--format=tar", "main"], Shape::BehindRemote)
            .with_globals(&[&["--git-dir", ".remote.git"]]),
    );
    out.push(
        Case::new("archive", &["archive", "--format=tar", "HEAD"], Shape::Worktree).in_dir("wt"),
    );
    out.push(
        Case::new("archive", &["archive", "--format=zip", "HEAD"], Shape::Worktree).in_dir("wt"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two long-path cases measure a *length*, and the length is spelled in
    /// a literal that nothing else checks.
    ///
    /// [`tar_name_field`] is the only group in this file whose subject is a
    /// number: 100, the width of the tar `name` field. If `ADD_SPLIT` were
    /// shortened below that the case would still run, still pass, and measure a
    /// plain header — the encoding it exists for would simply never be reached,
    /// and nothing would say so. The same for `ADD_PAX`, which additionally
    /// needs *no* `/` in the over-long component: one slash in the right place
    /// and git splits it into `prefix` + `name` instead of writing a pax record,
    /// which is the other case, measured twice.
    ///
    /// So the premises are asserted rather than trusted: the operand really
    /// carries the path the doc comment names, that path really is longer than
    /// the field, and the pax one really has no separator to split at.
    #[test]
    fn the_long_path_operands_carry_the_lengths_they_are_named_for() {
        // The tar `name` field is 100 bytes; `prefix` is 155.
        assert!(SPLIT_PATH.len() > 100, "SPLIT_PATH fits in the name field: {}", SPLIT_PATH.len());
        assert!(SPLIT_PATH.len() <= 100 + 1 + 155, "SPLIT_PATH cannot be split at all");
        assert!(PAX_PATH.len() > 100, "PAX_PATH fits in the name field: {}", PAX_PATH.len());
        assert!(!PAX_PATH.contains('/'), "PAX_PATH has a separator and would be split, not paxed");

        // And the operands are those paths, not paraphrases of them.
        assert_eq!(ADD_SPLIT, format!("--add-virtual-file={SPLIT_PATH}:x"));
        assert_eq!(ADD_PAX, format!("--add-virtual-file={PAX_PATH}:y"));
        // `--prefix` is applied to every entry, so its own length is what has to
        // overflow; both spellings share the tail with the operands above.
        assert!(PREFIX_SPLIT.len() - "--prefix=".len() > 100);
        assert!(PREFIX_PAX.len() - "--prefix=".len() > 100);
        assert!(!PREFIX_PAX["--prefix=".len()..].trim_end_matches('/').contains('/'));
    }

    /// Nothing in this file names a path outside the fixture, and nothing runs a
    /// command it did not spell out.
    ///
    /// Two hazards specific to this module. `--add-file` reads the
    /// **filesystem**, so an operand naming an absolute path would pull a file
    /// off the machine into a compared stream — which is the same leak
    /// `crate::env::harden` exists to close, arriving through argv instead of
    /// through the environment. And `tar.<fmt>.command` is a program git runs;
    /// the two this file configures are `cat` and a name chosen to be absent, in
    /// that order of intent, and neither is assembled from anything.
    #[test]
    fn no_case_names_the_machine() {
        let mut cases = Vec::new();
        super::cases(&mut cases);
        assert!(!cases.is_empty());
        for case in &cases {
            for arg in &case.args {
                if let Some(path) = arg.strip_prefix("--add-file=") {
                    assert!(!path.starts_with('/'), "--add-file names an absolute path: {arg}");
                }
                assert!(!arg.contains("/Users/"), "case argv names a home directory: {arg}");
            }
            for entry in &case.config {
                let Some(key) = &entry.key else { continue };
                if key.starts_with("tar.") && key.ends_with(".command") {
                    assert!(
                        entry.value == "cat" || entry.value == "no-such-filter-command",
                        "unexpected filter command {:?} for {key}",
                        entry.value
                    );
                }
            }
        }
    }
}
