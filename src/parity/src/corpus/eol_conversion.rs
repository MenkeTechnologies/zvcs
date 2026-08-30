//! Differential corpus cases for the **conversion between the working tree and
//! the object store** — the one place git deliberately stores different bytes
//! than it shows you.
//!
//! Every case here is compared against stock git for stdout, exit code and the
//! post-command state digest.
//!
//! # Division of territory
//!
//! Six modules already own neighbouring ground. What each owns, and what this
//! module adds that none of them has:
//!
//! * **`corpus/attributes_filters.rs`** — the nearest neighbour, and the one to
//!   read before adding anything here. It owns attributes that *drive a
//!   program*: `filter.<n>.clean`/`.smudge`, `diff.<n>.textconv`,
//!   `merge.<n>.driver`, `ident`, and the funcname/external-diff drivers. It
//!   also samples the built-in eol filter — one cell of it per surface:
//!   `-c core.autocrlf=true` alone, `-c core.autocrlf=input` alone,
//!   `-c core.eol=crlf` alone, and `-c core.safecrlf=true|warn` alone. Always
//!   **one key at a time**, and — where the payload is the eol filter's rather
//!   than `ident`'s — always its `CRLF` (`a\r\nb\r\n`), `LF` (`a\nb\n`) or
//!   `CRLF_NUL` (`a\r\nb\x00c\r\n`) constant.
//!
//!   This module is the two axes that sampling leaves out. **The config axis
//!   crossed with itself**: `core.autocrlf` and `core.eol` set *together*, which
//!   is the only way the precedence between them is observable, and
//!   `core.safecrlf` crossed with `core.autocrlf`/`core.eol`, which is the only
//!   way the *trigger* for the refusal is separable from the refusal. Exactly
//!   one case in the corpus set two of these three keys before —
//!   `!whitespace::checkout::-c core.safecrlf=true -c core.autocrlf=input
//!   checkout HEAD~2 -- ws/eol.txt` — and it lands on the one surface where
//!   neither key can do anything: a CRLF blob checked out is CRLF whatever the
//!   configuration says (measured on stock 2.55.0 across nine configurations,
//!   all three CRLF pairs intact, exit 0, silent). **And the content axis**: the
//!   payload decides the verdict as much as the attribute does, and the three
//!   payloads above reach exactly two of git's verdicts. See [`CR_THEN_CRLF`]
//!   for the input that splits `text` from `text=auto` — a line nothing else in
//!   this harness draws.
//! * **`corpus/info_attrs.rs`** owns attribute **matching**: `check-attr`,
//!   `check-ignore`, the rule syntax, and the precedence between
//!   `.gitattributes`, `sub/.gitattributes` and `.git/info/attributes`. No case
//!   here runs `check-attr`; the readout used here is `ls-files --eol`, which
//!   reports the `i/` and `w/` conversion states rather than the attribute set.
//! * **`corpus/pathspec_stdin.rs`** owns **path** encoding — the quoting of a
//!   filename, `core.quotePath`, `-z` framing of path lists, and `:(attr:…)`
//!   magic. This module owns **content** encoding: the bytes inside the file,
//!   never the bytes of its name. The single `-z` invocation here
//!   ([`checkout_index_stdin`]) is there because `checkout-index --stdin` is a
//!   *conversion* surface, and it carries a config pair no pathspec case does.
//! * **`corpus/config_reads.rs`** owns reading a key back (`config --get`,
//!   `--type`, `--show-origin`). Nothing here asks what `core.eol` *says*; every
//!   case asks what it *does*.
//! * **`corpus/index_plumbing.rs`** and **`corpus/plumbing_objects.rs`** own
//!   `update-index`/`ls-files`/`cat-file` framing and flag parsing on their
//!   default configuration. Every `ls-files --eol` case here is either a form
//!   that module does not run (`-o`, `-t`, a pathspec) or carries a config pair.
//! * **`corpus/archive_export.rs`** owns `archive`'s formats, prefixes and
//!   `export-ignore`; only the eol conversion `archive` performs on the way out
//!   is here, and only for the config *pairs*.
//!
//! # How a round trip is measured when no probe reads a blob
//!
//! The brief for this module is the round trip: write bytes, `add`, see what was
//! stored, check out into a fresh tree, see what comes back. A single case is a
//! single invocation against a pristine fixture and this module cannot register
//! a [`crate::runner::Sequence`], so the trip is measured as two independent
//! cases — and the instruments for the two halves are different:
//!
//! * **What was STORED.** No probe in [`crate::runner`] reads blob *content*:
//!   `probe_state` runs `cat-file --batch-check --batch-all-objects`, which
//!   prints id, type and size, and `ls-files --stage -v`, which prints the
//!   staged id. That is enough, and it is enough for the strongest possible
//!   reason — **the id is a cryptographic function of the stored bytes**. A port
//!   that stores CRLF where stock stores LF produces a different blob id for the
//!   same working-tree file, and that id is in the digest twice over. So
//!   `hash-object` (id on stdout), `hash-object -w` (id on stdout *and* in the
//!   object census) and `add` (id in `ls-files --stage`) are all direct reads on
//!   the stored bytes. This is the class that silently rewrites a user's
//!   content: nothing about it appears in `status`, `log` or `diff`.
//! * **What COMES BACK.** `probe_worktree_content` compares every worktree file
//!   byte for byte when it is UTF-8 and under 64 KiB, so the smudge direction is
//!   read off the disk. `checkout-index --prefix=out/ -a` is the lever, for the
//!   reason `attributes_filters.rs` measured and recorded: `-f -a` over files
//!   that are already up to date rewrites nothing, while `--prefix=out/` writes
//!   a *fresh* tree and the smudge actually runs.
//! * **A third instrument, used where it is sharper than either.** `archive`
//!   puts the converted bytes on **stdout** inside a tar, so a conversion
//!   difference is a stdout difference and needs no probe at all.
//!
//! # What is not reachable, stated rather than worked around
//!
//! * **`working-tree-encoding` is unreachable, and no case here pretends
//!   otherwise.** It is an attribute, and attributes have no `-c` spelling: a
//!   case can only reach a rule that some fixture already contains.
//!   `--attr-source=<rev>` and `GIT_ATTR_SOURCE` re-point the lookup at another
//!   *tree in the same repository* and every tree in every shape descends from
//!   one commit; `core.attributesFile` can name any relative path in the
//!   fixture, but no file in any fixture contains a line that parses as
//!   `<pattern> working-tree-encoding=<enc>`. Reaching it needs a fixture
//!   change, which this module is not permitted to make. What *is* reached is
//!   the adjacent fact: [`UTF16LE_BOM_CRLF`] is hashed under three different
//!   attribute verdicts, pinning that a UTF-16 file passes through the eol
//!   filter untouched — which is precisely why `working-tree-encoding` has to
//!   exist as a separate mechanism. A CR and an LF in UTF-16 are never adjacent
//!   bytes (`00 0d 00 0a` big-endian, `0d 00 0a 00` little-endian), so there is
//!   no CRLF pair for the filter to find. Verified on stock 2.55.0: the id is
//!   `48a5da110f34389ae6b32fdd472f176743d1c9fb` under `text eol=lf`, under
//!   `text=auto`, and under no attribute at all.
//! * **`w/crlf` and `w/mixed` are unreachable from `ls-files --eol`.** The `w/`
//!   column is computed from the file on disk, no shape ships a tracked file
//!   with CRLF in the *worktree*, and producing one takes a `checkout` before
//!   the `ls-files` — two invocations. The `w/` states that *are* reachable are
//!   covered: `w/lf`, `w/none` (an empty file and a file with no trailing
//!   newline) and `w/-text` (a NUL, and — less obviously — content whose only
//!   line ending is a lone CR), plus the `i/` column empty for an untracked path
//!   under `-o`.
//! * **A NUL past git's 8000-byte window does not exist for this filter.**
//!   `convert.c`'s `gather_stats` scans the *whole* buffer, so a NUL at offset
//!   8016 of an 8019-byte file makes `text=auto` decline exactly as a NUL at
//!   offset 6 does — measured on stock 2.55.0, both hashing to the unconverted
//!   content. The 8000-byte `FIRST_FEW_BYTES` ceiling belongs to `diff`'s binary
//!   test, which is `corpus/diff_family.rs`'s ground. No large payload is
//!   carried here for a boundary that is not there.
//! * **`core.eol` cannot be tested against a non-`native` platform.** `native`
//!   resolves to LF on the machine this harness runs on. Both sides resolve it
//!   the same way, so the case is a real comparison; it just cannot be a
//!   comparison against CRLF-native behaviour.
//!
//! # Determinism
//!
//! Every payload is a `&'static [u8]` literal with its CR, LF, NUL and BOM bytes
//! written out, never an escaped string a formatter could normalise. No case
//! names an absolute path, reads the clock, or draws a random value. The noisy
//! cases — the thirteen-line `core.safecrlf=warn` listings and the
//! `checkout-index --prefix=out/` trees — were run three times against stock in
//! a scratch fixture and compared byte for byte before being written down.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    content_axis(out);
    config_axis_clean(out);
    config_axis_smudge(out);
    archive_precedence(out);
    safecrlf_matrix(out);
    renormalize(out);
    eol_readout(out);
    verbs_that_do_not_convert(out);
    checkout_index_stdin(out);
}

// ---------------------------------------------------------------------------
// Payloads
//
// Written as byte literals with every CR, LF, NUL and BOM byte spelled out. A
// payload is the *input side* of this whole module: the attribute and the
// configuration only decide what happens to these bytes, so a payload that a
// formatter rewrote would silently change the question every case asks.
// ---------------------------------------------------------------------------

/// CRLF and a lone LF in one file. `text` and `text=auto` both normalise it —
/// git's "auto" heuristic is about *binary*, not about consistency — and an
/// unspecified path leaves it alone. Measured on stock 2.55.0:
/// `de980441c3ab03a8c07dda1ad27b8a11f39deb1e` converted,
/// `1fa89d461c912ca111f2f7182dd25f14a2af518b` untouched.
const MIXED_CRLF_LF: &[u8] = b"a\r\nb\nc\r\n";

/// CR that is never followed by LF. There is no CRLF pair to convert, so every
/// attribute verdict and every configuration produces the same id
/// (`c64959c158be9dd78f8b75b83707b4c31106b655` on stock 2.55.0). The pin is that
/// a port must not "helpfully" treat a bare CR as a line ending: doing so would
/// rewrite the content of every classic-Mac and every embedded-CR file in the
/// repository, and the id is the only thing that would show it.
const LONE_CR: &[u8] = b"a\rb\r";

/// A lone CR **and** a CRLF, which is the input that splits the two conversions
/// apart. Explicit `text` converts it; `text=auto` refuses, because
/// `convert.c`'s auto path declines any buffer carrying a lone CR — normalising
/// it could not be undone on the way out. Measured on stock 2.55.0:
/// `--path=src/tabs.rs` (`text eol=lf`) hashed
/// `4565b4dcbbb38e655115621f1fbf039ec31fd3d0`, `--path=docs/manual.md`
/// (`text=auto`) hashed `d8522105dcd4685c72dfc2b2fbdeb88a9fe270a1`, which is
/// also the unconverted content's id.
///
/// **No other payload in this harness distinguishes `text` from `text=auto`.**
/// The three that existed before — plain CRLF, plain LF, and CRLF with a NUL —
/// separate `text` from `binary` and nothing more: on plain CRLF the two verdicts
/// agree, and on the NUL payload `text=auto` declines for being binary rather
/// than for being irreversible. A port that implemented `text=auto` as an alias
/// for `text` scored full marks on every one of them.
const CR_THEN_CRLF: &[u8] = b"a\r\r\nb\r\n";

/// CRLF with no line ending on the last line. The final partial line is what a
/// naive line-splitting conversion drops or re-terminates, and neither error
/// shows up on stdout. Measured on stock 2.55.0:
/// `0a207c060e61f3b88eaee0a8cd0696f46fb155eb` converted,
/// `0c991fcb4fe1739224d4a0df2973df2de4eef4ad` untouched.
const CRLF_NO_TRAILING_NEWLINE: &[u8] = b"a\r\nb";

/// A UTF-8 BOM in front of CRLF text. The BOM is content, not framing: git
/// converts straight through it. Pinned because a port that sniffed the BOM and
/// took an encoding-aware path would store different bytes for a file whose
/// first three bytes are the only thing unusual about it.
const UTF8_BOM_CRLF: &[u8] = b"\xef\xbb\xbfa\r\nb\r\n";

/// UTF-16LE with a BOM, carrying what a UTF-16 editor calls CRLF line endings.
/// Nothing converts it, under any attribute: the encoded CR and LF are
/// `0d 00 0a 00`, so the CR is followed by a NUL and there is no CRLF byte pair
/// anywhere in the file. That is the whole reason `working-tree-encoding` exists
/// as a separate mechanism, and — see the module header — the closest this
/// module can get to it without a fixture change.
const UTF16LE_BOM_CRLF: &[u8] = b"\xff\xfea\x00\r\x00\n\x00b\x00\r\x00\n\x00";

/// UTF-16BE with a BOM and the same content, where the encoded pair is
/// `00 0d 00 0a`. Carried beside the little-endian form because the two put the
/// NUL on the opposite side of the CR, so a port whose scan looks one byte the
/// wrong way finds a "CRLF" in exactly one of them. Both are inert on stock
/// 2.55.0 under every attribute verdict —
/// `3b87d81db374453b3b1ce8217a8e96fdfea9ae97` here,
/// `48a5da110f34389ae6b32fdd472f176743d1c9fb` little-endian — so a port that
/// converted either one diverges on the id alone.
const UTF16BE_BOM_CRLF: &[u8] = b"\xfe\xff\x00a\x00\r\x00\n\x00b\x00\r\x00\n";

/// Nothing at all. The degenerate input for every filter: the empty blob
/// `e69de29bb2d1d6434b8b29ae775ad8c2e48c5391` under every attribute on stock
/// 2.55.0, and a path a conversion that assumes at least one line can fault on.
const EMPTY: &[u8] = b"";

/// One CRLF and nothing else — a file that is a single empty line. Converted to
/// the one-byte blob `8b137891791fe96927ad78e64b0aad7bded08bdc` by `text` and by
/// `text=auto`, left as `d3f5a12faa99758192ecc4ed3fc22c9249232e86` when the path
/// has no `text` attribute.
const CRLF_ONLY: &[u8] = b"\r\n";

/// LF then CR — the reversed pair. No CRLF sequence exists in it, so nothing
/// converts and every verdict lands on
/// `f1752d1cb8eaf7bbc7bfbd6ae323707a4b1ba024`. The value is that a port
/// implementing the scan as "drop every CR adjacent to an LF" instead of "drop
/// the CR of a CRLF pair" diverges here and nowhere else in the corpus.
const LF_THEN_CR: &[u8] = b"a\n\rb\n";

/// Three tracked paths for `checkout-index --stdin -z`, NUL-terminated: one
/// `text eol=lf`, one `text eol=crlf`, one inheriting `* text=auto`. Chosen so a
/// single invocation writes all three conversion verdicts into a fresh tree.
const CHECKOUT_INDEX_PATHS: &[u8] = b"src/tabs.rs\x00sub/nested.txt\x00README.md\x00";

// ---------------------------------------------------------------------------
// Paths, and the attribute verdict each one selects
// ---------------------------------------------------------------------------

/// `Shape::Attributes` paths, one per attribute verdict, as `hash-object
/// --path` operands. `src/tabs.rs` is `text eol=lf` (explicit), `docs/manual.md`
/// inherits `* text=auto`, and `missing-attr.txt` is `!text` — unspecified, so
/// only `core.autocrlf` can decide anything about it.
const VERDICT_PATHS: &[&str] = &["--path=src/tabs.rs", "--path=docs/manual.md", "--path=missing-attr.txt"];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn hash(out: &mut Vec<Case>, args: &[&str], payload: &'static [u8]) {
    out.push(Case::with_stdin("hash-object", args, Shape::Attributes, payload));
}

fn hash_cfg(out: &mut Vec<Case>, args: &[&str], payload: &'static [u8], cfg: &[(&str, &str)]) {
    out.push(
        Case::with_stdin("hash-object", args, Shape::Attributes, payload).with_config(cfg),
    );
}

fn cfg(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], shape: Shape, pairs: &[(&str, &str)]) {
    out.push(Case::new(cmd, args, shape).with_config(pairs));
}

fn cfg_strict(
    out: &mut Vec<Case>,
    cmd: &'static str,
    args: &[&str],
    shape: Shape,
    pairs: &[(&str, &str)],
) {
    out.push(Case::strict(cmd, args, shape).with_config(pairs));
}

// ---------------------------------------------------------------------------
// The content axis: same attribute, different bytes in
// ---------------------------------------------------------------------------

/// One payload per shape of line ending git has an opinion about, hashed under
/// each of the three attribute verdicts.
///
/// `hash-object --stdin --path=<p>` is the clean filter with the rest of git
/// removed: bytes in, the id of the bytes that *would be stored* out. Every case
/// here is therefore measured on stdout, and the `-w` cases additionally land
/// the object where `probe_state`'s object census reads it back.
///
/// The corpus had three payloads for this filter before — CRLF, LF, and CRLF
/// with a NUL — and they reach two verdicts between them. These reach the rest:
/// the lone CR that stops the auto path ([`CR_THEN_CRLF`]), the partial last
/// line, the empty file, the single empty line, the two BOMs, and the reversed
/// pair. Ten payloads times three verdicts is the matrix; the `-w`,
/// `--no-filters` and extra-path cases below are the corners of it that are worth
/// a second look.
fn content_axis(out: &mut Vec<Case>) {
    const PAYLOADS: &[&[u8]] = &[
        MIXED_CRLF_LF,
        LONE_CR,
        CR_THEN_CRLF,
        CRLF_NO_TRAILING_NEWLINE,
        UTF8_BOM_CRLF,
        UTF16LE_BOM_CRLF,
        UTF16BE_BOM_CRLF,
        EMPTY,
        CRLF_ONLY,
        LF_THEN_CR,
    ];
    for payload in PAYLOADS {
        for path in VERDICT_PATHS {
            hash(out, &["hash-object", "--stdin", path], payload);
        }
    }

    // The two remaining verdicts, on the two payloads that discriminate:
    // `sub/nested.txt` is `text eol=crlf` through `sub/.gitattributes` — the
    // *clean* side of `eol=crlf` is still a normalisation to LF, which is the
    // half of that attribute a reader is most likely to get backwards — and
    // `assets/logo.bin` is `binary`, where nothing may ever convert.
    for path in ["--path=sub/nested.txt", "--path=assets/logo.bin"] {
        hash(out, &["hash-object", "--stdin", path], CR_THEN_CRLF);
        hash(out, &["hash-object", "--stdin", path], MIXED_CRLF_LF);
    }

    // Written into the store, so the id is compared twice: once on stdout, and
    // once in the object census the state probe takes afterwards. A port whose
    // `hash-object` prints the right id while writing the unconverted bytes
    // under it is only caught by the second.
    for path in ["--path=src/tabs.rs", "--path=docs/manual.md"] {
        hash(out, &["hash-object", "-w", "--stdin", path], CR_THEN_CRLF);
    }
    hash(out, &["hash-object", "-w", "--stdin", "--path=src/tabs.rs"], MIXED_CRLF_LF);
    hash(out, &["hash-object", "-w", "--stdin", "--path=missing-attr.txt"], MIXED_CRLF_LF);

    // `--no-filters` is the escape hatch: the same path, the same payload, and
    // the conversion turned off. It must print the id of the raw bytes, which is
    // the id `--path=missing-attr.txt` prints for the same payload — so the two
    // cases are a cross-check on each other rather than two isolated ids.
    hash(out, &["hash-object", "--stdin", "--path=src/tabs.rs", "--no-filters"], CR_THEN_CRLF);
    hash(out, &["hash-object", "--stdin", "--path=docs/manual.md", "--no-filters"], MIXED_CRLF_LF);
}

// ---------------------------------------------------------------------------
// The config axis, clean side: two keys at once
// ---------------------------------------------------------------------------

/// `core.autocrlf` and `core.eol` set **together**, on the one path whose
/// attributes say nothing (`missing-attr.txt` is `!text`) so the configuration
/// is the only thing deciding.
///
/// No case in the corpus set `core.autocrlf` and `core.eol` together before, and
/// one key cannot show a precedence. The rule stock 2.55.0 implements, measured
/// rather than recalled: on the way **in**, `core.autocrlf=true` and `=input`
/// both normalise and `core.eol` contributes nothing at all, while
/// `autocrlf=false` leaves an unspecified path untouched whatever `core.eol`
/// says. Six configurations over [`MIXED_CRLF_LF`] and `--path=missing-attr.txt`
/// on stock 2.55.0, which is the whole rule in one column:
///
/// | configuration                  | stored id |
/// |--------------------------------|-----------|
/// | `autocrlf=true  eol=lf`        | `de980441…` normalised |
/// | `autocrlf=true  eol=native`    | `de980441…` normalised |
/// | `autocrlf=input eol=crlf`      | `de980441…` normalised |
/// | `autocrlf=false eol=crlf`      | `1fa89d46…` verbatim |
/// | `autocrlf=false eol=native`    | `1fa89d46…` verbatim |
/// | `eol=native` alone             | `1fa89d46…` verbatim |
///
/// `core.eol` never moves a cell; `core.autocrlf` decides every one. A port that
/// let `core.eol` reach the clean side inverts rows two, four and six.
///
/// `core.eol=native` appears here for the first time in the corpus; it resolves
/// to LF on this platform, identically on both sides.
fn config_axis_clean(out: &mut Vec<Case>) {
    const PAIRS: &[&[(&str, &str)]] = &[
        &[("core.autocrlf", "true"), ("core.eol", "lf")],
        &[("core.autocrlf", "input"), ("core.eol", "crlf")],
        &[("core.autocrlf", "false"), ("core.eol", "crlf")],
        &[("core.autocrlf", "false"), ("core.eol", "native")],
        &[("core.autocrlf", "true"), ("core.eol", "native")],
    ];
    for pair in PAIRS {
        hash_cfg(out, &["hash-object", "--stdin", "--path=missing-attr.txt"], MIXED_CRLF_LF, pair);
    }
    // `core.eol` alone, at the value nothing has used: it must be inert on the
    // clean side, exactly as `=crlf` already is.
    hash_cfg(
        out,
        &["hash-object", "--stdin", "--path=missing-attr.txt"],
        MIXED_CRLF_LF,
        &[("core.eol", "native")],
    );
    // The pair against the payload that splits `text` from `text=auto`: the
    // attribute has to keep deciding regardless of what the two config keys say.
    hash_cfg(
        out,
        &["hash-object", "--stdin", "--path=docs/manual.md"],
        CR_THEN_CRLF,
        &[("core.autocrlf", "true"), ("core.eol", "crlf")],
    );
    hash_cfg(
        out,
        &["hash-object", "--stdin", "--path=src/tabs.rs"],
        CR_THEN_CRLF,
        &[("core.autocrlf", "input"), ("core.eol", "crlf")],
    );
}

// ---------------------------------------------------------------------------
// The config axis, smudge side: what comes back out
// ---------------------------------------------------------------------------

/// The same two keys on the **way out**, where they disagree — and where the
/// answer is the opposite way round from the clean side.
///
/// `checkout-index --prefix=out/ -a` writes a fresh tree, so the smudge actually
/// runs, and `probe_worktree_content` compares every byte of it. Measured on
/// stock 2.55.0 over `Shape::Attributes`, counting CRLF pairs in `out/`:
///
/// | configuration                             | `out/README.md` | `out/src/tabs.rs` | `out/sub/nested.txt` |
/// |-------------------------------------------|-----------------|-------------------|----------------------|
/// | `autocrlf=true  eol=lf`                   | CRLF            | LF                | CRLF                 |
/// | `autocrlf=input eol=crlf`                 | LF              | LF                | CRLF                 |
/// | `autocrlf=false eol=crlf`                 | CRLF            | LF                | CRLF                 |
/// | `eol=native` / `autocrlf=false`           | LF              | LF                | CRLF                 |
///
/// Three separable rules, none of them observable with one key set: `autocrlf`
/// beats `core.eol` in both directions (`true` wins over `eol=lf`, `input` wins
/// over `eol=crlf`), `core.eol` applies only once `autocrlf` is off, and the
/// per-path attribute beats both — `src/tabs.rs` is `text eol=lf` and stays LF
/// under every row, `sub/nested.txt` is `text eol=crlf` and stays CRLF under
/// every row including the two that ask for LF everywhere.
fn config_axis_smudge(out: &mut Vec<Case>) {
    const PAIRS: &[&[(&str, &str)]] = &[
        &[("core.autocrlf", "true"), ("core.eol", "lf")],
        &[("core.autocrlf", "input"), ("core.eol", "crlf")],
        &[("core.autocrlf", "false"), ("core.eol", "crlf")],
        &[("core.autocrlf", "true"), ("core.eol", "native")],
        &[("core.eol", "native")],
        &[("core.autocrlf", "false")],
    ];
    for pair in PAIRS {
        cfg(out, "checkout-index", &["checkout-index", "--prefix=out/", "-a"], Shape::Attributes, pair);
    }

    // The same question in a repository with no `.gitattributes` at all, where
    // the configuration is unopposed and every tracked file moves together.
    for pair in [
        &[("core.autocrlf", "false"), ("core.eol", "crlf")][..],
        &[("core.autocrlf", "input"), ("core.eol", "crlf")][..],
    ] {
        cfg(out, "checkout-index", &["checkout-index", "--prefix=out/", "-a"], Shape::Linear, pair);
    }
    // And in the shape whose *history* holds a CRLF blob. The index does not —
    // `ws/eol.txt` was normalised to LF two commits before HEAD — so this is the
    // control for the `checkout HEAD~2 -- ws/eol.txt` case in
    // [`verbs_that_do_not_convert`]: the same path in the same repository comes
    // back CRLF from the old commit and, here, LF-then-CRLF-by-configuration
    // from the index. A port that read line endings off the wrong side of that
    // pair passes one case and fails the other.
    cfg(
        out,
        "checkout-index",
        &["checkout-index", "--prefix=out/", "-a"],
        Shape::Whitespace,
        &[("core.autocrlf", "false"), ("core.eol", "crlf")],
    );
}

// ---------------------------------------------------------------------------
// The same precedence, read off stdout instead of off the disk
// ---------------------------------------------------------------------------

/// `archive` performs the conversion on its way out and puts the result on
/// **stdout** inside the tar, which makes it the cheapest possible instrument
/// for the precedence pairs: no probe, no worktree walk, just bytes.
///
/// Measured on stock 2.55.0 over `Shape::Attributes`, as the sha1 of the tar
/// stream: `autocrlf=true eol=lf` and `autocrlf=false eol=crlf` both produce
/// `9c7baaf8300b…`, the CRLF stream, while `autocrlf=input eol=crlf` produces
/// `3bee816980ee…`, byte-identical to an unconfigured `archive`. The same three
/// rules as the worktree table above, on a surface where a divergence cannot
/// hide behind a probe that skipped a file.
fn archive_precedence(out: &mut Vec<Case>) {
    const PAIRS: &[&[(&str, &str)]] = &[
        &[("core.autocrlf", "true"), ("core.eol", "lf")],
        &[("core.autocrlf", "input"), ("core.eol", "crlf")],
        &[("core.autocrlf", "false"), ("core.eol", "crlf")],
        &[("core.eol", "native")],
    ];
    for pair in PAIRS {
        cfg(out, "archive", &["archive", "--format=tar", "HEAD"], Shape::Attributes, pair);
    }
}

// ---------------------------------------------------------------------------
// safecrlf: the refusal, and what triggers it
// ---------------------------------------------------------------------------

/// `core.safecrlf` crossed with the two keys that give it something to refuse.
///
/// `safecrlf` is not a conversion, it is a *veto*: it fires when the round trip
/// would not be reversible — when the bytes that would be stored, smudged back
/// out, would not equal the bytes on disk. So it has no effect at all unless
/// some other key makes the checkout differ from the working tree. The corpus
/// had four `safecrlf` cases before this group: three `add` cases that set it
/// alone, and one `checkout` case that pairs it with `core.autocrlf=input` on a
/// blob no configuration can change. That measures the veto on whatever the
/// default configuration happens to trigger, and cannot separate "the port
/// implements safecrlf" from "the port implements the thing safecrlf is
/// checking".
///
/// The full 3x3, measured on stock 2.55.0 over `Shape::Attributes` — and every
/// cell of it is a *stderr* fact, which is why these are strict:
///
/// | | `autocrlf=true` | `autocrlf=input` | `autocrlf=false` |
/// |---|---|---|---|
/// | `safecrlf=true`  | `fatal: … .gitattributes`, 128 | `fatal: … sub/nested.txt`, 128 | `fatal: … sub/nested.txt`, 128 |
/// | `safecrlf=warn`  | thirteen warnings, 0 | one warning, 0 | one warning, 0 |
/// | `safecrlf=false` | silent, 0 | silent, 0 | silent, 0 |
///
/// (The `fatal:` line in full is `fatal: LF would be replaced by CRLF in
/// <path>`; the warning is `warning: in the working copy of '<path>', LF will
/// be replaced by CRLF the next time Git touches it`. Both are compared byte for
/// byte, so the elision is in this table only.)
///
/// The three columns are three different *triggers*, and they name different
/// paths: with `autocrlf=true` every `text=auto` file would come back CRLF, so
/// the first path in index order aborts the command; with `autocrlf` off only
/// `sub/nested.txt`, whose `.gitattributes` says `eol=crlf`, is irreversible.
/// A port that hard-coded the message, or that checked the wrong direction, or
/// that reported the paths in a different order, diverges on a different cell of
/// this table in each case.
///
/// `core.eol=crlf` is carried as a fourth trigger: it reaches the veto through
/// a code path `core.autocrlf` does not, and produces the same thirteen-path
/// listing.
///
/// The last two cases are the per-path form, and they are a *pair* on purpose.
/// Under the same `-c core.safecrlf=true -c core.autocrlf=true`,
/// `add sub/nested.txt` dies — that path's `.gitattributes` says `eol=crlf`, so
/// its LF working-tree bytes would come back CRLF — while `add src/tabs.rs`
/// exits 0 in silence, because `text eol=lf` makes its round trip exact.
/// Measured on stock 2.55.0. One flag, one configuration, two paths, opposite
/// answers: a port that applies the veto per *command* rather than per *path*
/// agrees with stock on exactly one of them.
fn safecrlf_matrix(out: &mut Vec<Case>) {
    for safe in ["true", "warn"] {
        for auto in ["true", "input", "false"] {
            cfg_strict(
                out,
                "add",
                &["add", "-A"],
                Shape::Attributes,
                &[("core.safecrlf", safe), ("core.autocrlf", auto)],
            );
        }
    }
    // The off cell, which must be silent where the two above are not.
    cfg_strict(
        out,
        "add",
        &["add", "-A"],
        Shape::Attributes,
        &[("core.safecrlf", "false"), ("core.autocrlf", "true")],
    );
    // The veto reached through `core.eol` rather than through `core.autocrlf`.
    for safe in ["true", "warn"] {
        cfg_strict(
            out,
            "add",
            &["add", "-A"],
            Shape::Attributes,
            &[("core.safecrlf", safe), ("core.eol", "crlf")],
        );
    }
    // One path instead of the whole tree: the veto fires for `sub/nested.txt`,
    // whose `eol=crlf` attribute is what makes the trip irreversible, and stays
    // silent for `src/tabs.rs`, whose `eol=lf` makes it exact. See the table
    // above.
    cfg_strict(
        out,
        "add",
        &["add", "sub/nested.txt"],
        Shape::Attributes,
        &[("core.safecrlf", "true"), ("core.autocrlf", "true")],
    );
    cfg_strict(
        out,
        "add",
        &["add", "src/tabs.rs"],
        Shape::Attributes,
        &[("core.safecrlf", "true"), ("core.autocrlf", "true")],
    );
}

// ---------------------------------------------------------------------------
// --renormalize: the flag that turns the veto off
// ---------------------------------------------------------------------------

/// `add --renormalize` under exactly the configurations that make plain `add`
/// abort.
///
/// This is the sharpest contrast in the module and it is one flag wide.
/// `--renormalize` sets git's `SAFE_CRLF_RENORMALIZE`, which suppresses the
/// `safecrlf` check entirely — the whole point of the flag is to re-store every
/// blob through today's attributes, and refusing to do so because the result is
/// irreversible would defeat it. Measured on stock 2.55.0 over
/// `Shape::Attributes`: `-c core.safecrlf=true add -A` exits 128 with
/// `fatal: LF would be replaced by CRLF in .gitattributes`, while
/// `-c core.safecrlf=true add --renormalize -A` exits 0, says nothing, and
/// stages nothing.
///
/// "Stages nothing" is a fact worth having in the digest rather than an
/// anticlimax: the fixture's blobs are already normalised, so a correct
/// `--renormalize` is a no-op on the index, and a port that re-stored every file
/// with different bytes would show up as a dirty `ls-files --stage` in the state
/// probe even though its stdout is empty and its exit code is 0.
///
/// The corpus's six existing `--renormalize` cases — `attributes` three times
/// (one of them `-c core.autocrlf=true`), `dirty`, `split-index` and
/// `whitespace` — never set `core.safecrlf`, so not one of them could reach the
/// suppression: with the veto never armed, a port that simply ignored
/// `--renormalize`'s effect on it scored full marks.
fn renormalize(out: &mut Vec<Case>) {
    const CONFIGS: &[&[(&str, &str)]] = &[
        &[("core.safecrlf", "true")],
        &[("core.safecrlf", "true"), ("core.autocrlf", "true")],
        &[("core.safecrlf", "warn"), ("core.autocrlf", "true")],
        &[("core.safecrlf", "true"), ("core.eol", "crlf")],
    ];
    for pair in CONFIGS {
        cfg_strict(out, "add", &["add", "--renormalize", "-A"], Shape::Attributes, pair);
    }
    // The other spellings of the same flag, under a configuration that changes
    // what "normal" means: `.` is a pathspec walk, `-u` is index-driven, and a
    // single path is neither.
    cfg(out, "add", &["add", "--renormalize", "."], Shape::Attributes, &[("core.autocrlf", "input")]);
    cfg(out, "add", &["add", "--renormalize", "."], Shape::Attributes, &[("core.eol", "crlf")]);
    cfg(out, "add", &["add", "--renormalize", "-u"], Shape::Attributes, &[("core.autocrlf", "true")]);
    cfg_strict(
        out,
        "add",
        &["add", "--renormalize", "sub/nested.txt"],
        Shape::Attributes,
        &[("core.safecrlf", "true"), ("core.autocrlf", "true")],
    );
    // The same flag where there are no attributes to renormalise against, so the
    // configuration is the only input. Both shapes hold an all-LF index, which
    // is what makes `--renormalize` a provable no-op on it: the staged ids in
    // `ls-files --stage` must come out of the command unchanged, and a port that
    // re-cleans through the wrong direction moves every one of them.
    cfg(out, "add", &["add", "--renormalize", "-A"], Shape::Whitespace, &[("core.autocrlf", "input")]);
    cfg(out, "add", &["add", "--renormalize", "-A"], Shape::Linear, &[("core.autocrlf", "true"), ("core.eol", "crlf")]);
}

// ---------------------------------------------------------------------------
// ls-files --eol: the direct readout of the two states
// ---------------------------------------------------------------------------

/// `ls-files --eol` prints the `i/` and `w/` columns this whole module is about:
/// what the index blob's line endings are, what the file on disk's are, and
/// which attribute produced the pair. It is the one command whose output *is*
/// the conversion state rather than a consequence of it.
///
/// The corpus ran it in six single-invocation cases before — `attributes` twice,
/// `dirty` twice, `symlinks` and `whitespace` — always in its bare form or with
/// a single `-z`, plus two sequence steps naming one path each. What is added
/// here is the rest of the column vocabulary and the forms that reach it:
///
/// * **`-o`** puts an untracked path through the same report, where the `i/`
///   column is *empty* — there is no index entry to have line endings. Measured
///   on stock 2.55.0 over `Shape::Attributes`: `i/      w/lf    attr/text=auto`.
///   No case in the corpus ran `--eol` with `-o`, so an empty `i/` column had
///   never been compared.
/// * **`Shape::NoIndexTrees`** reaches `i/none w/none` — `ni/eol_b.txt` has no
///   line ending at all — which no other shape's tracked set contains. It also
///   reaches `i/-text w/-text` by a *different route* than anything measured
///   before: `ni/bin_a.bin` and `ni/bin_b.bin` are called binary because their
///   content has a NUL, where `Shape::Attributes`' `assets/logo.bin` is called
///   binary because `.gitattributes` says so. Same two columns, two derivations,
///   and a port can implement one without the other. Verified on stock 2.55.0
///   over a scratch index holding the same content classes: `\x00\x01binary…`
///   reports `i/-text w/-text`, `last line` with no newline reports
///   `i/none w/none`, and — the one that is easy to get wrong — `x\ry\r`
///   reports `i/-text w/-text` too, because content whose only line ending is a
///   lone CR is binary by this heuristic.
/// * **`-t`** prefixes the index tag, which is a second, independent opinion
///   about the same entry, and **a pathspec** restricts the walk — the form a
///   user actually types, and the one where a port that computes the columns
///   lazily can get them right for the whole tree and wrong for one path.
/// * **A configuration** that must *not* move the report: `core.eol` and
///   `core.autocrlf` change what a future checkout writes, not what the index
///   and the disk currently hold, so `ls-files --eol` is identical with and
///   without them. Verified on stock 2.55.0, byte for byte.
fn eol_readout(out: &mut Vec<Case>) {
    out.push(Case::new("ls-files", &["ls-files", "--eol", "-o"], Shape::Attributes));
    out.push(Case::new("ls-files", &["ls-files", "--eol", "-o", "-z"], Shape::Attributes));
    out.push(Case::new("ls-files", &["ls-files", "--eol", "-t"], Shape::Attributes));
    out.push(Case::new("ls-files", &["ls-files", "--eol", "--", "src", "sub"], Shape::Attributes));
    out.push(Case::new("ls-files", &["ls-files", "--eol", "--", "assets"], Shape::Attributes));
    cfg(out, "ls-files", &["ls-files", "--eol"], Shape::Attributes, &[("core.eol", "crlf")]);
    cfg(out, "ls-files", &["ls-files", "--eol"], Shape::Attributes, &[("core.autocrlf", "input")]);
    cfg(
        out,
        "ls-files",
        &["ls-files", "--eol"],
        Shape::Attributes,
        &[("core.autocrlf", "true"), ("core.eol", "lf")],
    );

    // The binary and no-newline rows, which live in exactly one shape.
    out.push(Case::new("ls-files", &["ls-files", "--eol"], Shape::NoIndexTrees));
    out.push(Case::new(
        "ls-files",
        &["ls-files", "--eol", "--", "ni/bin_a.bin", "ni/eol_b.txt", "ni/eol_a.txt"],
        Shape::NoIndexTrees,
    ));
    out.push(Case::new("ls-files", &["ls-files", "--eol", "-t"], Shape::NoIndexTrees));
    out.push(Case::new("ls-files", &["ls-files", "--eol"], Shape::Linear));
}

// ---------------------------------------------------------------------------
// Which verbs convert, and which look at the stored bytes
// ---------------------------------------------------------------------------

/// The other half of the contract: the commands that must **not** convert.
///
/// `Shape::Whitespace` holds the one blob in the harness that was committed with
/// CRLF in it — `HEAD~2:ws/eol.txt` is `alpha\r\nbeta\r\ngamma\r\n`, stored that
/// way because the shape has no `.gitattributes` and the fixture was built with
/// conversion off. That makes it the only place where "did this verb convert?"
/// has a visible answer in the *other* direction: the CRs are in the object, so
/// a verb that strips them is losing bytes rather than adding them.
///
/// Measured on stock 2.55.0, all with `-c core.autocrlf=true` set, so a port
/// that applies the smudge filter indiscriminately has every opportunity to:
///
/// * `grep -n alpha HEAD~2 -- ws/eol.txt` prints `…:1:alpha\r` — the CR is in
///   the match line, because `grep` reads the blob.
/// * `blame --porcelain HEAD~2 -- ws/eol.txt` puts `\tbeta\r` in its content
///   lines, for the same reason.
/// * `show HEAD~2:ws/eol.txt` and `cat-file blob HEAD~2:ws/eol.txt` both print
///   the stored bytes unchanged.
/// * `cat-file --filters` is the one that *does* run the filter — and it still
///   prints CRLF, because the smudge direction only ever turns LF into CRLF and
///   never the reverse. Set against `-c core.autocrlf=input` and
///   `-c core.eol=lf`, which are the two configurations most likely to make a
///   port think it should be normalising on the way out.
///
/// The last of those is the round trip's asymmetry stated as a case: content
/// that reaches the object store with CRLF in it stays that way forever, and no
/// checkout configuration will clean it up. That is *why* `--renormalize` exists,
/// and it is the fact a port has to get right for a repository that already
/// contains CRLF blobs — which every repository converted from CVS or SVN does.
fn verbs_that_do_not_convert(out: &mut Vec<Case>) {
    const AUTOCRLF: &[(&str, &str)] = &[("core.autocrlf", "true")];

    cfg(out, "grep", &["grep", "-n", "alpha", "HEAD~2", "--", "ws/eol.txt"], Shape::Whitespace, AUTOCRLF);
    // The CR proved by an anchor rather than by eye. `beta.$` needs one
    // character between `beta` and the end of the line and matches only where
    // the CR is still there; `beta$` needs none and matches only where it is
    // not. Measured on stock 2.55.0: `beta.$` exits 0 against `HEAD~2` and 1
    // against `HEAD`, `beta$` exits 1 against `HEAD~2`. Three exit codes a port
    // that smudged the search buffer gets exactly backwards, with no bytes to
    // squint at.
    cfg(out, "grep", &["grep", "-n", "-E", "beta.$", "HEAD~2", "--", "ws/eol.txt"], Shape::Whitespace, AUTOCRLF);
    cfg(out, "grep", &["grep", "-n", "-E", "beta.$", "HEAD", "--", "ws/eol.txt"], Shape::Whitespace, AUTOCRLF);
    cfg(out, "grep", &["grep", "-n", "-E", "beta$", "HEAD~2", "--", "ws/eol.txt"], Shape::Whitespace, AUTOCRLF);
    cfg(out, "blame", &["blame", "--porcelain", "HEAD~2", "--", "ws/eol.txt"], Shape::Whitespace, AUTOCRLF);
    cfg(out, "show", &["show", "HEAD~2:ws/eol.txt"], Shape::Whitespace, AUTOCRLF);
    cfg(out, "cat-file", &["cat-file", "blob", "HEAD~2:ws/eol.txt"], Shape::Whitespace, AUTOCRLF);

    // The smudge is one-way: these must all still print CRLF.
    cfg(
        out,
        "cat-file",
        &["cat-file", "--filters", "HEAD~2:ws/eol.txt"],
        Shape::Whitespace,
        &[("core.autocrlf", "input")],
    );
    cfg(
        out,
        "cat-file",
        &["cat-file", "--filters", "--path=ws/eol.txt", "HEAD~2:ws/eol.txt"],
        Shape::Whitespace,
        &[("core.eol", "lf")],
    );
    // `--path` naming a path this shape does not contain: `Shape::Whitespace`
    // has no `src/` and no `.gitattributes`, so the lookup finds no rule at all
    // and the two config keys are unopposed. A distinct code path from naming a
    // path that exists, and the one a port is most likely to short-circuit.
    cfg(
        out,
        "cat-file",
        &["cat-file", "--filters", "--path=src/tabs.rs", "HEAD~2:ws/eol.txt"],
        Shape::Whitespace,
        &[("core.autocrlf", "true"), ("core.eol", "lf")],
    );
    // And the checkout of that blob keeps its CRs whatever the configuration
    // asks for — read off the disk by `probe_worktree_content`.
    cfg(
        out,
        "checkout",
        &["checkout", "HEAD~2", "--", "ws/eol.txt"],
        Shape::Whitespace,
        &[("core.autocrlf", "input"), ("core.eol", "lf")],
    );
}

// ---------------------------------------------------------------------------
// checkout-index --stdin: the fresh tree, driven by a path list
// ---------------------------------------------------------------------------

/// `checkout-index --stdin -z` writes the same fresh tree as `--prefix=out/ -a`
/// but takes its path list on stdin, which is the form a tool drives it in.
///
/// Here for the conversion rather than for the framing: the three paths in
/// [`CHECKOUT_INDEX_PATHS`] select three different attribute verdicts, so one
/// invocation writes an LF file, a CRLF file, and a file whose endings the
/// configuration decides — and `probe_worktree_content` compares all three.
/// `corpus/pathspec_stdin.rs` owns `-z` as a *framing* question; this is the one
/// case where the framing is incidental and the bytes inside the files are the
/// point, which is why it carries a config pair no case there does.
fn checkout_index_stdin(out: &mut Vec<Case>) {
    out.push(
        Case::with_stdin(
            "checkout-index",
            &["checkout-index", "--prefix=si/", "--stdin", "-z"],
            Shape::Attributes,
            CHECKOUT_INDEX_PATHS,
        )
        .with_config(&[("core.autocrlf", "true"), ("core.eol", "lf")]),
    );
    out.push(
        Case::with_stdin(
            "checkout-index",
            &["checkout-index", "--prefix=si/", "--stdin", "-z"],
            Shape::Attributes,
            CHECKOUT_INDEX_PATHS,
        )
        .with_config(&[("core.autocrlf", "false"), ("core.eol", "crlf")]),
    );
}
