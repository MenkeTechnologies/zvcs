//! Differential corpus cases for attributes that **drive** something: the
//! filter, diff and merge machinery a `.gitattributes` entry installs into
//! git's data path, rather than the attribute lookup that selects it.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! # Division of territory
//!
//! Four other modules already own neighbouring ground, and nothing here repeats
//! them:
//!
//! * `corpus/info_attrs.rs` owns attribute **matching** — `check-attr`,
//!   `check-ignore`, `check-mailmap`, the rule syntax, the precedence between
//!   `.gitattributes`, `sub/.gitattributes` and `.git/info/attributes`, and the
//!   quoting of the report. It asks *what does this path's `diff` attribute say*.
//!   This module asks *what does git then do with that answer*.
//! * `corpus/pathspec_stdin.rs` owns `:(attr:…)` pathspec magic, including
//!   `:(attr:merge=union)` selecting `sub/nested.txt`. An attribute used as a
//!   **selector** is its territory; an attribute used as a **program** is this
//!   one's.
//! * `corpus/diff_family.rs` owns the diff *front end* — which pairs are
//!   compared, the `--raw`/`--stat`/`--numstat` renderings, rename detection.
//!   The `diff.<driver>.*` keys that replace how a pair is compared are here.
//! * `corpus/merge_family.rs` owns the three-way *text* merge (`merge-file`,
//!   `merge-tree`, the ll-merge backends) on its default driver, and documents
//!   the three routes a content result can be asserted through. The
//!   `merge.<driver>.driver` keys that replace that text merge with a command
//!   are here, and they take the `merge-tree --write-tree` route it names.
//! * `corpus/archive_export.rs` owns `archive`'s formats, prefixes and
//!   `export-ignore`. Only the *conversion* `archive` performs on its way out is
//!   here.
//! * `corpus/object_pack.rs` owns `cat-file`'s batch framing, including a few
//!   `--filters`/`--textconv` invocations on their default configuration. Every
//!   case here carries a `-c` that changes what those two flags actually run,
//!   so no id collides with one of its.
//!
//! # What a case may name as a driver, and why the rule is narrow
//!
//! A driver is a command, and a corpus that could name any command is a corpus
//! that can hang a worker or reach outside the fixture. Everything below is
//! drawn from a closed set — `cat`, `true`, `false`, `tr`, `git stripspace`, and
//! `sh -c '…'`/`echo …` one-liners whose whole text is a literal in the case —
//! and every one of them either takes the filename git appends as its operand or
//! ignores its arguments entirely. None reads an unterminated stdin, none is
//! looked up from a pool, and none outlives the invocation. A generated case
//! that executed `/usr/bin/HEAD` because its operand came from a rev pool was
//! removed in an earlier audit; the closed set is what keeps that from being
//! recreated by hand.
//!
//! Two spellings recur and are worth reading once:
//!
//! * `tr a-z A-Z <` as a `textconv`. Git appends the temp filename to the
//!   configured string and runs the result through a shell, so the trailing `<`
//!   becomes a redirection from that file. Output is the file's own bytes
//!   uppercased — deterministic, and it never names the temp file on stdout.
//! * `cat %O > %A` as a `merge.<n>.driver`. Git substitutes the ancestor and
//!   "ours" temp paths before running the string through a shell, so this is
//!   "resolve every conflict to the merge base". The temp names are random, but
//!   they appear only inside the command line, never in any compared output.
//!
//! # Three things that are **not** reachable, stated rather than worked around
//!
//! A case is one argv against a pristine fixture copy. Configuration reaches it
//! through `-c` and through the scoped files [`crate::runner::ConfigScope`]
//! writes; **attributes have no such channel** — there is no `-c` spelling of an
//! attribute, `--attr-source=<rev>`/`GIT_ATTR_SOURCE` only re-points the lookup
//! at another tree in the same repository, and no fixture tree carries a rule
//! other than the ones `Shape::Attributes` was built with. So:
//!
//! * **`filter.<n>.clean` / `.smudge` / `.required` are unreachable.** An
//!   external filter driver runs only for a path whose `filter` attribute names
//!   it, and no `.gitattributes` in any shape sets `filter=` at all. Setting
//!   `filter.x.clean` with `-c` installs a driver nothing selects: measured on
//!   stock 2.55.0, `git -c filter.x.clean=false -c filter.x.required=true add -A`
//!   on `Shape::Attributes` exits 0 and stages normally, because the driver is
//!   never consulted. That inertness is itself pinned below — a port that ran a
//!   configured filter without an attribute asking for it would corrupt every
//!   blob it stored, and nothing else in the corpus would notice.
//! * **`filter.<n>.process` is doubly unreachable, and could not be driven by a
//!   shell one-liner even if a path selected it.** The long-running protocol is
//!   pkt-line framed: the driver must read a 4-byte hex length prefix, answer a
//!   `git-filter-client`/`version=2` handshake, advertise its capabilities, and
//!   then stream length-prefixed packets until a flush. `cat`, `tr`, `true` and
//!   `false` cannot produce a 4-byte hex length, and any one-liner that *did*
//!   loop on stdin would be exactly the "reads stdin without an EOF" hazard the
//!   rule above forbids. So there is no case here for it, and adding one would
//!   need a fixture that ships a `filter=` attribute and a real helper binary.
//! * **`export-subst` is still unreachable**, for the reason
//!   `corpus/archive_export.rs` already records, and re-checked here against the
//!   two mechanisms that did not exist when that note was written.
//!   `--attr-source=<rev>` and `GIT_ATTR_SOURCE` change *which tree* the
//!   `.gitattributes` is read from, not *what it says*, and every tree in every
//!   shape descends from the same commit; `core.attributesFile` can be pointed
//!   at any relative path in the fixture, but no file in any fixture contains a
//!   line that parses as `<pattern> export-subst`. `export-ignore` is set
//!   (`*.md`) and is measured; `export-subst` needs a fixture change.
//!
//! What is left is not small, and it is the half that actually ships in git:
//! the **built-in** filters. `text`/`text=auto`/`-text`/`eol=lf`/`eol=crlf`,
//! `core.autocrlf`, `core.eol` and `core.safecrlf` are a clean/smudge pair
//! implemented inside `convert.c`, and `Shape::Attributes` sets every one of
//! those attribute forms on a real tracked path. `ident` is a second built-in
//! filter, and `.git/info/attributes` sets `*.info ident`. Both are reached the
//! same way an external filter would be, through `hash-object --path`,
//! `cat-file --filters`, `checkout-index`, `add` and `archive`.
//!
//! # How each group asserts, and which class of defect it catches
//!
//! Two probes decide the shape of every group here, and the distinction between
//! them is the distinction between a cosmetic bug and a repository-corrupting
//! one:
//!
//! * **What is STORED.** `probe_state` runs `cat-file --batch-check
//!   --batch-all-objects`, so an object written into the store appears in the
//!   digest by id. A clean filter that converts differently produces a
//!   *different object id* for the same input, so `hash-object -w` is a direct
//!   read on it — and `hash-object` without `-w` puts the same id on stdout, so
//!   the cheap form is measured too. `ls-files --stage` carries the staged blob
//!   id, so `add` is read the same way. Every case in [`clean_side`] and the
//!   `-w` cases elsewhere are in this class, and a divergence there means the
//!   port would write a blob stock git would not.
//! * **What is DISPLAYED or WRITTEN.** `probe_worktree_content` compares every
//!   worktree file byte for byte, which is the only instrument for a smudge
//!   filter: the filtered bytes are written to disk and never reach stdout.
//!   `checkout-index --prefix=out/ -a` is the lever — it writes a *fresh* tree,
//!   so the smudge actually runs, where `checkout-index -f -a` over files that
//!   are already up to date does not rewrite them at all (measured: with
//!   `core.autocrlf=true`, `-f -a` left every file LF, `--prefix=out/ -a` wrote
//!   CRLF into `out/`).
//!
//! Diff and merge drivers need neither: a textconv or an external diff prints on
//! stdout, and `merge-tree --write-tree` prints a tree id whose blobs land in the
//! object store, so both surfaces move together.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    clean_side(out);
    smudge_side(out);
    eol_worktree(out);
    ident_driver(out);
    textconv_driver(out);
    funcname_driver(out);
    driver_shape(out);
    external_diff(out);
    merge_drivers(out);
    attributes_file(out);
    archive_conversion(out);
    inert_filters(out);
}

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

/// Two CRLF-terminated lines. The input every `text`/`eol`/`autocrlf` decision
/// is read through: a clean that normalizes produces one object id, a clean that
/// does not produces another, and the two ids differ on stdout.
const CRLF: &[u8] = b"a\r\nb\r\n";

/// The same two lines with LF endings — the *result* of a normalizing clean, so
/// a case that feeds this and one that feeds [`CRLF`] must agree exactly when
/// conversion happened.
const LF: &[u8] = b"a\nb\n";

/// CRLF text with an embedded NUL. `text=auto` inspects the content and declines
/// to convert what looks binary; an explicit `text` converts regardless. Measured
/// on stock 2.55.0: `--path=x.rs` (explicit `text eol=lf`) hashed
/// `55fdf7bb50468096ba330ff09cd44189a5ae7f9e`, `--path=docs/manual.md`
/// (`text=auto`) hashed `ef76e8d76cba01ed8db8f4f88fe1a6f8e91e9060`, which is
/// also what the unfiltered content hashes to.
const CRLF_NUL: &[u8] = b"a\r\nb\x00c\r\n";

/// An expanded `$Id:…$` line. The `ident` clean collapses it back to `$Id$`, so
/// this and [`IDENT_BARE`] must hash identically under `--path=*.info` and
/// differently without it. Measured on stock 2.55.0: both
/// `bbc60bcb1b69141a62299e737f8d69ddbfe5ae77` with `--path=notes.info`, and
/// `986e9bab26377b9759f140461799205625e0020e` vs the same `bbc60bcb…` without.
const IDENT_EXPANDED: &[u8] = b"$Id: 0123456789abcdef $\nbody\n";

/// The collapsed form the `ident` clean produces.
const IDENT_BARE: &[u8] = b"$Id$\nbody\n";

/// Paths for `hash-object --stdin-paths`, chosen so the four attribute verdicts
/// that matter are all in one invocation: `text=auto`, `text eol=crlf`,
/// `binary`, and `text eol=lf`.
const HASH_PATHS: &[u8] = b"README.md\nsub/nested.txt\nassets/logo.bin\nsrc/tabs.rs\n";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn c(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], shape: Shape) {
    out.push(Case::new(cmd, args, shape));
}

fn cfg(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], shape: Shape, k: &str, v: &str) {
    out.push(Case::new(cmd, args, shape).with_config(&[(k, v)]));
}

fn cfg2(
    out: &mut Vec<Case>,
    cmd: &'static str,
    args: &[&str],
    shape: Shape,
    pairs: &[(&str, &str)],
) {
    out.push(Case::new(cmd, args, shape).with_config(pairs));
}

fn stdin_cfg(
    out: &mut Vec<Case>,
    cmd: &'static str,
    args: &[&str],
    shape: Shape,
    payload: &'static [u8],
    pairs: &[(&str, &str)],
) {
    out.push(Case::with_stdin(cmd, args, shape, payload).with_config(pairs));
}

// ---------------------------------------------------------------------------
// The clean side: what a filter changes about what is STORED
// ---------------------------------------------------------------------------

/// `hash-object` is the clean filter with the rest of git removed: it takes
/// bytes, runs them through whatever the path's attributes install, and prints
/// the resulting object id. A conversion that differs by one byte prints a
/// different id, so every case here is measured on stdout — and the `-w` cases
/// additionally land the object in the store, where `probe_state`'s
/// `cat-file --batch-check --batch-all-objects` reads it back.
///
/// **This is the class that corrupts a repository.** A port that normalizes
/// where stock does not, or fails to where stock does, writes a blob whose
/// content is not what the user committed, and every later checkout of that blob
/// is wrong. Nothing about it is visible in `status` or `log`.
///
/// The path operand is what selects the rule, and `Shape::Attributes` supplies
/// one tracked path per attribute form: `src/tabs.rs` is `text eol=lf`,
/// `sub/nested.txt` is `text eol=crlf` (through `sub/.gitattributes`),
/// `assets/logo.bin` is `binary` (`-text`), `missing-attr.txt` is `!text` —
/// unspecified, so `core.autocrlf` decides — and everything else inherits
/// `* text=auto`.
fn clean_side(out: &mut Vec<Case>) {
    // The baseline pair: no path means no attribute lookup, so nothing converts,
    // and the LF payload is the id every *converting* case above must land on.
    out.push(Case::with_stdin("hash-object", &["hash-object", "--stdin"], Shape::Attributes, CRLF));
    out.push(Case::with_stdin("hash-object", &["hash-object", "--stdin"], Shape::Attributes, LF));

    // One case per attribute verdict, same bytes in.
    for path in [
        "--path=src/tabs.rs",
        "--path=sub/nested.txt",
        "--path=assets/logo.bin",
        "--path=missing-attr.txt",
    ] {
        out.push(Case::with_stdin(
            "hash-object",
            &["hash-object", "--stdin", path],
            Shape::Attributes,
            CRLF,
        ));
    }

    // `!text` unsets the attribute, so the fallback chain is what answers. Three
    // values of `core.autocrlf` over the same path and the same bytes.
    for v in ["true", "input"] {
        stdin_cfg(
            out,
            "hash-object",
            &["hash-object", "--stdin", "--path=missing-attr.txt"],
            Shape::Attributes,
            CRLF,
            &[("core.autocrlf", v)],
        );
    }
    // `core.eol` is a smudge-side setting; a clean must ignore it.
    stdin_cfg(
        out,
        "hash-object",
        &["hash-object", "--stdin", "--path=missing-attr.txt"],
        Shape::Attributes,
        CRLF,
        &[("core.eol", "crlf")],
    );

    // The binary heuristic: explicit `text` converts NUL-bearing content,
    // `text=auto` inspects it and declines.
    for path in ["--path=src/tabs.rs", "--path=docs/manual.md"] {
        out.push(Case::with_stdin(
            "hash-object",
            &["hash-object", "--stdin", path],
            Shape::Attributes,
            CRLF_NUL,
        ));
    }
    out.push(Case::with_stdin("hash-object", &["hash-object", "--stdin"], Shape::Attributes, CRLF_NUL));

    // `-w` moves the same decision into the object store, where the state digest
    // reads it back by id.
    for path in ["--path=src/tabs.rs"] {
        out.push(Case::with_stdin(
            "hash-object",
            &["hash-object", "-w", "--stdin", path],
            Shape::Attributes,
            CRLF,
        ));
    }
    stdin_cfg(
        out,
        "hash-object",
        &["hash-object", "-w", "--stdin", "--path=missing-attr.txt"],
        Shape::Attributes,
        CRLF,
        &[("core.autocrlf", "input")],
    );

    // `--stdin-paths` reads worktree files through their own attributes, four
    // verdicts in one invocation; `--no-filters` is the same four unconverted.
    out.push(Case::with_stdin("hash-object", &["hash-object", "--stdin-paths"], Shape::Attributes, HASH_PATHS));
    out.push(Case::with_stdin(
        "hash-object",
        &["hash-object", "--stdin-paths", "--no-filters"],
        Shape::Attributes,
        HASH_PATHS,
    ));

    // File operands take the same route without stdin.
    c(
        out,
        "hash-object",
        &["hash-object", "README.md", "sub/nested.txt", "assets/logo.bin", "src/tabs.rs"],
        Shape::Attributes,
    );

    // A shape with no `.gitattributes` at all: `core.autocrlf` is then the whole
    // decision, which is the configuration most Windows checkouts actually run.
    for v in ["true"] {
        stdin_cfg(
            out,
            "hash-object",
            &["hash-object", "--stdin", "--path=a.txt"],
            Shape::Linear,
            CRLF,
            &[("core.autocrlf", v)],
        );
    }

    // ---- the refusal, where the refusal is the contract ----
    // `--path` names a filter to run and `--no-filters` says not to run one.
    out.push(Case::strict(
        "hash-object",
        &["hash-object", "--stdin", "--path=src/tabs.rs", "--no-filters"],
        Shape::Attributes,
    ));
}

// ---------------------------------------------------------------------------
// The smudge side: what a filter changes about what is HANDED BACK
// ---------------------------------------------------------------------------

/// The inverse conversion, on the two routes that make it observable.
///
/// `cat-file --filters` puts the smudged bytes on **stdout**, so `eol=crlf`
/// shows up as literal CR bytes in the compared output — measured on stock
/// 2.55.0, `cat-file --filters --path=sub/nested.txt HEAD:docs/manual.md`
/// returned `# manual\r\n\r\nprose\r\nmore prose\r\n` where the same object under
/// `--path=src/tabs.rs` returned it LF-terminated.
///
/// `checkout-index --prefix=out/ -a` writes the smudged bytes into a **fresh**
/// subtree, where `probe_worktree_content` compares them byte for byte. The
/// prefix is load-bearing: `checkout-index -f -a` over files that are already up
/// to date does not rewrite them, so it converts nothing (measured — with
/// `core.autocrlf=true` every file stayed LF), and a case written that way would
/// score a port that implements no smudge at all as correct.
fn smudge_side(out: &mut Vec<Case>) {
    // One object, three destination paths: the attributes of the *name* decide
    // the line endings of identical stored bytes.
    for path in ["--path=sub/nested.txt", "--path=assets/logo.bin"] {
        c(out, "cat-file", &["cat-file", "--filters", path, "HEAD:docs/manual.md"], Shape::Attributes);
    }
    // The same object with `core.eol` and `core.autocrlf` supplying the answer
    // the attribute leaves open.
    for (k, v) in [("core.eol", "crlf"), ("core.autocrlf", "true")] {
        cfg(
            out,
            "cat-file",
            &["cat-file", "--filters", "--path=missing-attr.txt", "HEAD:docs/manual.md"],
            Shape::Attributes,
            k,
            v,
        );
    }
    // `eol=crlf` is explicit, so it must win over a contrary `core.eol`.
    cfg(
        out,
        "cat-file",
        &["cat-file", "--filters", "--path=sub/nested.txt", "HEAD:docs/manual.md"],
        Shape::Attributes,
        "core.eol",
        "lf",
    );
    // `eol=lf` is explicit, so `core.autocrlf=true` must not override it.
    cfg(
        out,
        "cat-file",
        &["cat-file", "--filters", "--path=src/tabs.rs", "HEAD:docs/manual.md"],
        Shape::Attributes,
        "core.autocrlf",
        "true",
    );
    // A blob that is *already* CRLF: the smudge must not double the CR.
    cfg(
        out,
        "cat-file",
        &["cat-file", "--filters", "--path=ws/eol.txt", "HEAD~2:ws/eol.txt"],
        Shape::Whitespace,
        "core.autocrlf",
        "true",
    );

    // The worktree route. Each of these writes a whole second copy of the tree
    // under `out/`, which `probe_worktree_content` reads byte for byte.
    c(out, "checkout-index", &["checkout-index", "--prefix=out/", "-a"], Shape::Attributes);
    for (k, v) in [("core.autocrlf", "true"), ("core.autocrlf", "input"), ("core.eol", "crlf")] {
        cfg(out, "checkout-index", &["checkout-index", "--prefix=out/", "-a"], Shape::Attributes, k, v);
    }
    // One path at a time, so a failure names the attribute that caused it.
    cfg(
        out,
        "checkout-index",
        &["checkout-index", "--prefix=out/", "sub/nested.txt", "src/tabs.rs", "assets/logo.bin"],
        Shape::Attributes,
        "core.autocrlf",
        "true",
    );
    // Shapes with no attributes, where `core.autocrlf` is the entire smudge, and
    // one whose entries are symlinks — which are not text and must not be
    // converted or dereferenced.
    cfg(out, "checkout-index", &["checkout-index", "--prefix=out/", "-a"], Shape::Linear, "core.autocrlf", "true");
    cfg(out, "checkout-index", &["checkout-index", "--prefix=out/", "-a"], Shape::Whitespace, "core.autocrlf", "true");
    cfg(out, "checkout-index", &["checkout-index", "--prefix=out/", "-a"], Shape::Symlinks, "core.autocrlf", "true");
}

// ---------------------------------------------------------------------------
// The conversion as the rest of git sees it
// ---------------------------------------------------------------------------

/// The same built-in filter reached through the porcelain that runs it as a side
/// effect: `add` (clean, into the index), `checkout` (smudge, into the
/// worktree), `status`/`diff` (clean, to decide whether a file changed), and
/// `ls-files --eol` (the decision reported without being performed).
///
/// `core.safecrlf` is the refusal path and the only one of these that changes an
/// exit code: on `Shape::Attributes`, `sub/nested.txt` is `eol=crlf` while its
/// worktree copy is LF, so any clean of it is a round trip that loses
/// information. Measured on stock 2.55.0 — `-c core.safecrlf=true add -A` exited
/// 128 with `fatal: LF would be replaced by CRLF in sub/nested.txt`, `warn`
/// exited 0 with `warning: in the working copy of 'sub/nested.txt', LF will be
/// replaced by CRLF the next time Git touches it`, and `false` was silent.
fn eol_worktree(out: &mut Vec<Case>) {
    // The refusal, the warning, and the path-scoped form of the same question.
    out.push(Case::strict("add", &["add", "-A"], Shape::Attributes).with_config(&[("core.safecrlf", "true")]));
    out.push(Case::strict("add", &["add", "-A"], Shape::Attributes).with_config(&[("core.safecrlf", "warn")]));
    out.push(
        Case::strict("add", &["add", "sub/nested.txt"], Shape::Attributes)
            .with_config(&[("core.safecrlf", "true")]),
    );

    // The clean on the way into the index. `ls-files --stage` in the state digest
    // carries the blob id each of these produced.
    for (k, v) in [("core.autocrlf", "true"), ("core.autocrlf", "input")] {
        cfg(out, "add", &["add", "-A"], Shape::Attributes, k, v);
    }
    c(out, "add", &["add", "--renormalize", "."], Shape::Attributes);
    cfg(out, "add", &["add", "--renormalize", "."], Shape::Attributes, "core.autocrlf", "true");

    // `checkout <tree> -- <path>` is the one single-argv way to put a genuinely
    // CRLF blob into a worktree: `HEAD~2:ws/eol.txt` is stored with CRLF.
    c(out, "checkout", &["checkout", "HEAD~2", "--", "ws/eol.txt"], Shape::Whitespace);
    for (k, v) in [("core.autocrlf", "true"), ("core.autocrlf", "input")] {
        cfg(out, "checkout", &["checkout", "HEAD~2", "--", "ws/eol.txt"], Shape::Whitespace, k, v);
    }
    out.push(
        Case::strict("checkout", &["checkout", "HEAD~2", "--", "ws/eol.txt"], Shape::Whitespace)
            .with_config(&[("core.safecrlf", "true"), ("core.autocrlf", "input")]),
    );

    // The clean run only to answer "is this file modified". A port that skips it
    // reports a file dirty that git calls clean, or the reverse.
    cfg(out, "status", &["status", "--porcelain=v1"], Shape::Attributes, "core.autocrlf", "true");
    cfg(out, "diff", &["diff"], Shape::Attributes, "core.autocrlf", "true");

    // The decision reported rather than performed. The `attr/` column must show
    // the attribute and the `i/`/`w/` columns the measured endings — and none of
    // the three may move when a *configuration* setting changes, since `--eol`
    // reports what is, not what would be.
    cfg(out, "ls-files", &["ls-files", "--eol"], Shape::Attributes, "core.autocrlf", "true");

    // `stash` cleans on the way in and smudges on the way out, in one command.
    cfg(out, "stash", &["stash", "-u"], Shape::Attributes, "core.autocrlf", "true");
}

// ---------------------------------------------------------------------------
// `ident`: the second built-in filter
// ---------------------------------------------------------------------------

/// `.git/info/attributes` sets `*.info ident`, which is a genuine clean/smudge
/// pair shipped inside git: the clean collapses `$Id: <anything> $` to `$Id$`,
/// the smudge expands `$Id$` to `$Id: <blob id> $`.
///
/// It is reached exactly the way an external filter would be — through
/// `--path` — which makes it the closest thing the fixtures offer to a
/// `filter.<n>.clean`, and it is in the STORED class: two different inputs must
/// hash to one object.
fn ident_driver(out: &mut Vec<Case>) {
    out.push(Case::with_stdin(
        "hash-object",
        &["hash-object", "--stdin", "--path=notes.info"],
        Shape::Attributes,
        IDENT_EXPANDED,
    ));
    out.push(Case::with_stdin(
        "hash-object",
        &["hash-object", "--stdin", "--path=notes.info"],
        Shape::Attributes,
        IDENT_BARE,
    ));
    // The same bytes with no `ident` attribute: the `$Id:` line must survive.
    out.push(Case::with_stdin(
        "hash-object",
        &["hash-object", "--stdin", "--path=src/tabs.rs"],
        Shape::Attributes,
        IDENT_EXPANDED,
    ));
    // Written into the store, so the id also appears in the state digest.
    out.push(Case::with_stdin(
        "hash-object",
        &["hash-object", "-w", "--stdin", "--path=notes.info"],
        Shape::Attributes,
        IDENT_EXPANDED,
    ));
    // The smudge direction: an object with no `$Id$` in it must come back
    // unchanged even though the driver ran.
    c(out, "cat-file", &["cat-file", "--filters", "--path=notes.info", "HEAD:docs/manual.md"], Shape::Attributes);
}

// ---------------------------------------------------------------------------
// diff drivers: replacing how a file is compared
// ---------------------------------------------------------------------------

/// `*.md diff=markdown` is set in `Shape::Attributes`, so every
/// `diff.markdown.*` key is reachable with `-c` and applies to `docs/manual.md`
/// and `README.md` and to nothing else — which is itself half the assertion,
/// since a port that applied a configured driver to every path would show it on
/// `src/tabs.rs` too.
///
/// `textconv` replaces both sides of the comparison with a command's output.
/// The commands are drawn from the closed set: `cat` (identity), `tr a-z A-Z <`
/// (a visible, deterministic transform), `true` (empty output, so the two sides
/// become identical and the diff disappears), `false` (a driver that dies) and
/// `git stripspace <` (git's own).
///
/// `cachetextconv` is the one that changes state rather than output: measured on
/// stock 2.55.0 it created `refs/notes/textconv/markdown` with one note per
/// blob, which `probe_state`'s `for-each-ref` and `cat-file --batch-all-objects`
/// both read.
fn textconv_driver(out: &mut Vec<Case>) {
    const CAT: (&str, &str) = ("diff.markdown.textconv", "cat");
    const UPPER: (&str, &str) = ("diff.markdown.textconv", "tr a-z A-Z <");
    const EMPTY: (&str, &str) = ("diff.markdown.textconv", "true");
    const STRIP: (&str, &str) = ("diff.markdown.textconv", "git stripspace <");

    for pair in [CAT, UPPER, EMPTY, STRIP] {
        cfg2(out, "diff", &["diff", "HEAD~2", "HEAD~1"], Shape::Attributes, &[pair]);
    }
    // Scoped to the path the attribute names, and to one that it does not.
    cfg2(out, "diff", &["diff", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes, &[UPPER]);
    cfg2(out, "diff", &["diff", "HEAD~1", "HEAD", "--", "src/tabs.rs"], Shape::Attributes, &[UPPER]);
    // `--textconv` is the default for `diff`; `--no-textconv` turns the driver off.
    cfg2(out, "diff", &["diff", "--no-textconv", "HEAD~2", "HEAD~1"], Shape::Attributes, &[UPPER]);
    // Rendering flags on top of a replaced input.
    // The plumbing pair, which does *not* run textconv unless asked.
    cfg2(out, "diff-tree", &["diff-tree", "-p", "HEAD~2", "HEAD~1"], Shape::Attributes, &[UPPER]);
    cfg2(out, "diff-tree", &["diff-tree", "-p", "--textconv", "HEAD~2", "HEAD~1"], Shape::Attributes, &[UPPER]);

    // The readers that reach the same driver from elsewhere in git.
    cfg2(out, "log", &["log", "-p", "-1", "HEAD~1"], Shape::Attributes, &[UPPER]);
    cfg2(out, "blame", &["blame", "--textconv", "-s", "docs/manual.md"], Shape::Attributes, &[UPPER]);
    cfg2(out, "cat-file", &["cat-file", "--textconv", "HEAD:docs/manual.md"], Shape::Attributes, &[UPPER]);

    // The state-changing one. The notes ref and its blobs are the assertion.
    cfg2(
        out,
        "diff",
        &["diff", "HEAD~2", "HEAD~1"],
        Shape::Attributes,
        &[CAT, ("diff.markdown.cachetextconv", "true")],
    );
    cfg2(
        out,
        "diff",
        &["diff", "HEAD~2", "HEAD~1"],
        Shape::Attributes,
        &[UPPER, ("diff.markdown.cachetextconv", "true")],
    );

    // ---- the driver that dies ----
    // `false` produces no output and a non-zero status; git treats the input as
    // unreadable rather than as empty.
    out.push(
        Case::strict("diff", &["diff", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes)
            .with_config(&[("diff.markdown.textconv", "false")]),
    );
    out.push(
        Case::strict("cat-file", &["cat-file", "--textconv", "HEAD:docs/manual.md"], Shape::Attributes)
            .with_config(&[("diff.markdown.textconv", "false")]),
    );
    // A driver name the attribute does not select: configured and never run.
    cfg2(out, "diff", &["diff", "HEAD~2", "HEAD~1"], Shape::Attributes, &[("diff.nosuch.textconv", "false")]);
}

// ---------------------------------------------------------------------------
// diff drivers: the hunk header
// ---------------------------------------------------------------------------

/// `xfuncname` and `funcname` decide the text after `@@ … @@`, and
/// `--function-context` decides how much of the file a hunk grows to cover.
/// Both are only visible when a hunk does *not* already span the whole file, so
/// every case here uses `-U0` or `-U1`.
///
/// The built-in language drivers ship their patterns inside git
/// (`userdiff.c`), and one of them is reachable without a fixture change:
/// `*.md diff=markdown` names git's own `markdown` driver, whose `xfuncname`
/// matches an ATX heading. Measured on stock 2.55.0,
/// `diff -U0 HEAD~2 HEAD~1 -- docs/manual.md` emitted `@@ -3,0 +4 @@ # manual`
/// — the heading, supplied by the built-in pattern and by nothing in the
/// repository. A `-c diff.markdown.xfuncname=^prose` overrides it to
/// `@@ -3,0 +4 @@ prose`, which separates "uses the built-in table" from "uses
/// the configured value" in one pair of cases.
///
/// `src/tabs.rs` has no `diff` attribute, so it exercises the *default* matcher
/// in the same shape: measured `@@ -2 +2 @@ fn indented() {`.
fn funcname_driver(out: &mut Vec<Case>) {
    // Built-in `markdown` pattern versus a configured override, same hunk.
    c(out, "diff", &["diff", "-U0", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes);
    cfg(out, "diff", &["diff", "-U0", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes, "diff.markdown.xfuncname", "^prose");
    cfg(out, "diff", &["diff", "-U0", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes, "diff.markdown.funcname", "^prose");
    // A pattern that matches nothing must leave the suffix empty, not fall back
    // to the built-in's answer.
    cfg(out, "diff", &["diff", "-U0", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes, "diff.markdown.xfuncname", "^zzz");
    // `funcname` is a basic regex and `xfuncname` an extended one, so a pattern
    // that only parses as one of the two separates the implementations.
    cfg(out, "diff", &["diff", "-U0", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes, "diff.markdown.xfuncname", "^(pro|man)");
    cfg(out, "diff", &["diff", "-U0", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes, "diff.markdown.funcname", "^\\(pro\\|man\\)");
    // Both set: `xfuncname` wins.
    cfg2(
        out,
        "diff",
        &["diff", "-U0", "HEAD~2", "HEAD~1", "--", "docs/manual.md"],
        Shape::Attributes,
        &[("diff.markdown.funcname", "^prose"), ("diff.markdown.xfuncname", "^# man")],
    );
    // The default matcher, on a path the attribute does not reach.
    c(out, "diff", &["diff", "-U0", "HEAD~1", "HEAD", "--", "src/tabs.rs"], Shape::Attributes);
    c(out, "diff", &["diff", "--function-context", "HEAD~1", "HEAD", "--", "src/tabs.rs"], Shape::Attributes);
    // A multi-line C file, where a function context is a real span.
    c(out, "diff", &["diff", "-U0", "HEAD~2", "HEAD", "--", "ws/indent.c"], Shape::Whitespace);
    // A pattern git cannot compile is a refusal, not a silent fallback.
    out.push(
        Case::strict("diff", &["diff", "-U0", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes)
            .with_config(&[("diff.markdown.xfuncname", "^[")]),
    );
}

// ---------------------------------------------------------------------------
// diff drivers: the shape of the comparison itself
// ---------------------------------------------------------------------------

/// The remaining `diff.<driver>.*` keys, plus the attribute values that select a
/// built-in behaviour with no configuration at all.
///
/// `Shape::Attributes` supplies three of the latter on tracked paths — `-diff`
/// on `*.log`, on `vendor/**` and on `sub/nested.txt`, and `binary` on
/// `assets/*.bin` — and they are what make `Binary files … differ` appear
/// without the content being binary. Measured on stock 2.55.0:
/// `diff HEAD~3 HEAD~2 -- sub/nested.txt` printed
/// `Binary files a/sub/nested.txt and b/sub/nested.txt differ` for a file whose
/// content is two lines of ASCII.
fn driver_shape(out: &mut Vec<Case>) {
    // `-diff` as an attribute, and `diff.<n>.binary` as its configured twin.
    c(out, "diff", &["diff", "HEAD~3", "HEAD~2", "--", "sub/nested.txt"], Shape::Attributes);
    cfg(out, "diff", &["diff", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes, "diff.markdown.binary", "true");
    cfg(out, "diff", &["diff", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes, "diff.markdown.binary", "false");
    cfg(out, "diff", &["diff", "--binary", "HEAD~2", "HEAD~1"], Shape::Attributes, "diff.markdown.binary", "true");
    // `--text` overrides the driver's own claim that the file is binary.
    cfg(out, "diff", &["diff", "--text", "HEAD~2", "HEAD~1"], Shape::Attributes, "diff.markdown.binary", "true");
    // `binary` as an attribute is `-diff -text -merge`, on a file that really is.
    c(out, "diff", &["diff", "HEAD~3", "HEAD", "--", "assets/logo.bin"], Shape::Attributes);

    // `wordRegex` — the driver decides what a word is.
    cfg(out, "diff", &["diff", "--word-diff=plain", "HEAD~2", "HEAD~1"], Shape::Attributes, "diff.markdown.wordregex", "[a-z]+");
    // A driver-level regex must beat the global `diff.wordRegex`.
    cfg2(
        out,
        "diff",
        &["diff", "--word-diff=plain", "HEAD~2", "HEAD~1"],
        Shape::Attributes,
        &[("diff.wordRegex", "."), ("diff.markdown.wordregex", "[a-z]+")],
    );

    // `algorithm` — the driver picks the xdiff backend for its own paths only.
    for algo in ["histogram"] {
        cfg(out, "diff", &["diff", "HEAD~2", "HEAD~1"], Shape::Attributes, "diff.markdown.algorithm", algo);
    }
    // A driver-level algorithm must beat the global one for the paths it owns.
    cfg2(
        out,
        "diff",
        &["diff", "HEAD~2", "HEAD~1"],
        Shape::Attributes,
        &[("diff.algorithm", "minimal"), ("diff.markdown.algorithm", "histogram")],
    );
    out.push(
        Case::strict("diff", &["diff", "HEAD~2", "HEAD~1"], Shape::Attributes)
            .with_config(&[("diff.markdown.algorithm", "nosuch")]),
    );
}

// ---------------------------------------------------------------------------
// The external diff: a whole comparison replaced by a command
// ---------------------------------------------------------------------------

/// `diff.<driver>.command` and `GIT_EXTERNAL_DIFF` replace the comparison
/// entirely: git writes both sides to temp files and hands the command seven
/// arguments, then prints nothing of its own. Neither is on by default outside
/// `diff` itself — `log`/`show` need `--ext-diff`, and `--no-ext-diff` turns it
/// back off, which is a gate a port can get backwards in either direction.
///
/// Measured on stock 2.55.0: `-c "diff.markdown.command=sh -c 'echo EXTERNAL'"
/// diff HEAD~2 HEAD~1` printed exactly `EXTERNAL`; the same key under
/// `log -p -1` printed the ordinary patch, and under `log -p --ext-diff -1`
/// printed `EXTERNAL`. `diff.markdown.command=false` exited 128 with
/// `fatal: external diff died, stopping at docs/manual.md`.
///
/// The command text never names a temp file, so nothing machine-dependent can
/// reach the compared output.
fn external_diff(out: &mut Vec<Case>) {
    const ECHO: (&str, &str) = ("diff.markdown.command", "sh -c 'echo EXTERNAL'");
    const QUIET: (&str, &str) = ("diff.markdown.command", "true");

    cfg2(out, "diff", &["diff", "HEAD~2", "HEAD~1"], Shape::Attributes, &[ECHO]);
    cfg2(out, "diff", &["diff", "HEAD~2", "HEAD~1"], Shape::Attributes, &[QUIET]);
    cfg2(out, "diff", &["diff", "--no-ext-diff", "HEAD~2", "HEAD~1"], Shape::Attributes, &[ECHO]);
    cfg2(out, "log", &["log", "-p", "-1", "HEAD~1"], Shape::Attributes, &[ECHO]);
    cfg2(out, "log", &["log", "-p", "--ext-diff", "-1", "HEAD~1"], Shape::Attributes, &[ECHO]);
    // A textconv and an external diff both configured: the external command wins
    // and the textconv never runs.
    cfg2(
        out,
        "diff",
        &["diff", "HEAD~2", "HEAD~1"],
        Shape::Attributes,
        &[ECHO, ("diff.markdown.textconv", "tr a-z A-Z <")],
    );
    // `--exit-code` asks for a status the external command has already taken
    // over; the two must not fight.

    // The environment spelling, which needs no attribute at all and therefore
    // applies to every path in every shape.
    out.push(
        Case::new("diff", &["diff", "HEAD~3", "HEAD"], Shape::Attributes)
            .with_env(&[("GIT_EXTERNAL_DIFF", "sh -c 'echo XD'")]),
    );
    out.push(
        Case::new("diff", &["diff", "HEAD~1", "HEAD"], Shape::Branched)
            .with_env(&[("GIT_EXTERNAL_DIFF", "sh -c 'echo XD'")]),
    );
    // A per-driver command must beat the environment for the paths it owns.
    out.push(
        Case::new("diff", &["diff", "HEAD~3", "HEAD"], Shape::Attributes)
            .with_config(&[ECHO])
            .with_env(&[("GIT_EXTERNAL_DIFF", "sh -c 'echo XD'")]),
    );

    // ---- the command that dies ----
    out.push(
        Case::strict("diff", &["diff", "HEAD~2", "HEAD~1", "--", "docs/manual.md"], Shape::Attributes)
            .with_config(&[("diff.markdown.command", "false")]),
    );
    out.push(
        Case::strict("diff", &["diff", "HEAD~1", "HEAD"], Shape::Branched)
            .with_env(&[("GIT_EXTERNAL_DIFF", "false")]),
    );
}

// ---------------------------------------------------------------------------
// merge drivers: replacing how a file is combined
// ---------------------------------------------------------------------------

/// A `merge` attribute names a low-level driver, and `merge.default` names one
/// for every path whose attribute is unspecified — which is the single-argv
/// route to an external merge driver, since no fixture sets `merge=<custom>`.
///
/// `Shape::CrissCross` is the shape with a real content conflict
/// (`clash.txt`, plus a multi-hunk `cc.txt`) *and* two merge bases, so
/// `merge.<n>.recursive` — the driver used to merge the virtual ancestors — has
/// something to do.
///
/// Two surfaces, both from `corpus/merge_family.rs`'s list:
/// `merge-tree --write-tree` prints the resulting **tree id** on stdout and puts
/// its blobs in the store, and `merge` itself stages and commits, so
/// `ls-files --stage` and `probe_worktree_content` both move. Measured on stock
/// 2.55.0, `merge-tree --write-tree cc-left cc-right` printed
/// `ab6b006f22560d2af4e13c757d06122573acb438` on the default driver,
/// `cd91643af0fa7f216c29a1bf13c5633bd83730db` under `merge.default=union`, and
/// `b72d208c509a0e5200537861470b59e2a0705c0b` under a `cat %O > %A` driver —
/// three distinct ids for three distinct merge programs.
fn merge_drivers(out: &mut Vec<Case>) {
    // `cat %O > %A` resolves every conflict to the merge base; `echo %L > %A` and
    // `echo %P > %A` prove the placeholder substitution itself, since the marker
    // size (7) and the pathname are what land in the file. Measured: `clash.txt`
    // and `cc.txt` both contained `7` under the first, and their own names under
    // the second.
    const TAKE_BASE: (&str, &str) = ("merge.tb.driver", "cat %O > %A");
    const MARKER: (&str, &str) = ("merge.lm.driver", "echo %L > %A");
    const PATHNAME: (&str, &str) = ("merge.pn.driver", "echo %P > %A");

    // The built-in driver names, reached through `merge.default`. The
    // un-configured baseline for this pair lives in `corpus/fixture_gaps.rs`.
    for name in ["union", "binary", "text"] {
        cfg(out, "merge-tree", &["merge-tree", "--write-tree", "cc-left", "cc-right"], Shape::CrissCross, "merge.default", name);
    }
    cfg(out, "merge", &["merge", "--no-edit", "cc-right"], Shape::CrissCross, "merge.default", "union");

    // The external driver, on both surfaces.
    cfg2(out, "merge-tree", &["merge-tree", "--write-tree", "cc-left", "cc-right"], Shape::CrissCross, &[("merge.default", "tb"), TAKE_BASE]);
    cfg2(out, "merge-tree", &["merge-tree", "--write-tree", "cc-left", "cc-right"], Shape::CrissCross, &[("merge.default", "lm"), MARKER]);
    cfg2(out, "merge-tree", &["merge-tree", "--write-tree", "cc-left", "cc-right"], Shape::CrissCross, &[("merge.default", "pn"), PATHNAME]);
    cfg2(out, "merge", &["merge", "--no-edit", "cc-right"], Shape::CrissCross, &[("merge.default", "tb"), TAKE_BASE]);
    cfg2(out, "merge", &["merge", "--no-edit", "cc-right"], Shape::CrissCross, &[("merge.default", "pn"), PATHNAME]);

    // `%L` under a non-default marker style: the driver must be handed the size
    // git would have used, not the constant 7.
    cfg2(
        out,
        "merge-tree",
        &["merge-tree", "--write-tree", "cc-left", "cc-right"],
        Shape::CrissCross,
        &[("merge.default", "lm"), MARKER, ("merge.conflictStyle", "diff3")],
    );

    // `.recursive` picks the driver for merging the two virtual ancestors, which
    // only a criss-cross history has.
    cfg2(
        out,
        "merge-tree",
        &["merge-tree", "--write-tree", "cc-left", "cc-right"],
        Shape::CrissCross,
        &[("merge.default", "tb"), TAKE_BASE, ("merge.tb.recursive", "binary")],
    );
    cfg2(
        out,
        "merge",
        &["merge", "--no-edit", "cc-right"],
        Shape::CrissCross,
        &[("merge.default", "tb"), TAKE_BASE, ("merge.tb.recursive", "binary")],
    );
    // `.name` is reporting only; it must not change the result.

    // A driver that fails is a conflict, not an abort; one that succeeds without
    // writing anything leaves "ours" in place.
    cfg2(out, "merge-tree", &["merge-tree", "--write-tree", "cc-left", "cc-right"], Shape::CrissCross, &[("merge.default", "no"), ("merge.no.driver", "false")]);
    cfg2(out, "merge", &["merge", "--no-edit", "cc-right"], Shape::CrissCross, &[("merge.default", "no"), ("merge.no.driver", "false")]);
    cfg2(out, "merge-tree", &["merge-tree", "--write-tree", "cc-left", "cc-right"], Shape::CrissCross, &[("merge.default", "id"), ("merge.id.driver", "true")]);
    // `merge.default` naming a driver with no `.driver` key at all.
    out.push(
        Case::strict("merge-tree", &["merge-tree", "--write-tree", "cc-left", "cc-right"], Shape::CrissCross)
            .with_config(&[("merge.default", "undefined")]),
    );

    // A second history with a conflict, so the driver is not measured on one
    // topology alone.
    cfg2(out, "merge-tree", &["merge-tree", "--write-tree", "main", "div-hot"], Shape::MergeableDirty, &[("merge.default", "tb"), TAKE_BASE]);

    // `sub/nested.txt merge=union` and `vendor/** -merge` are attribute-selected
    // drivers on a shape with no branch to merge, so what is pinned is that a
    // merge of the shape's own history still honours them.
    cfg(out, "merge-tree", &["merge-tree", "--write-tree", "HEAD~2", "HEAD"], Shape::Attributes, "merge.default", "union");

    // `merge-file` is the standalone three-way text merge and consults **no**
    // attribute and no driver: it is handed three files and merges them. Pinning
    // that it ignores `merge.default` is what keeps a port from wiring the
    // driver lookup into the wrong entry point.
    cfg2(out, "merge-file", &["merge-file", "-p", "cc.txt", "cc.txt", "cc.txt"], Shape::CrissCross, &[("merge.default", "union")]);
}

// ---------------------------------------------------------------------------
// `core.attributesFile`: where the rules are read from
// ---------------------------------------------------------------------------

/// The one channel through which a case can change *which file* supplies the
/// attributes, since it is a configuration key and takes a path.
///
/// It cannot supply new rules — nothing in any fixture parses as a `filter=` or
/// `export-subst` line — but it does reach two behaviours nothing else does: the
/// precedence of the "global" attributes file against the repository's own, and
/// the diagnostic for a malformed one. Measured on stock 2.55.0,
/// `-c core.attributesFile=.mailmap check-attr -a README.md` printed
/// `<solo@example.invalid> is not a valid attribute name: .mailmap:4` — a
/// message that names a file and a line number, which only a file-based source
/// can produce.
///
/// The paths named are relative and exist in the shape, so no case names an
/// absolute path or a file outside the fixture.
fn attributes_file(out: &mut Vec<Case>) {
    out.push(
        Case::strict("check-attr", &["check-attr", "-a", "README.md"], Shape::Attributes)
            .with_config(&[("core.attributesFile", ".mailmap")]),
    );
    // The repository's own file must outrank the global one.
    cfg(out, "check-attr", &["check-attr", "-a", "sub/nested.txt", "tracked-looking.txt"], Shape::Attributes, "core.attributesFile", "sub/.gitattributes");
    // A shape with no `.gitattributes` of its own, pointed at a file that is not
    // one: every line is diagnosed by file and line number and no rule survives.
    // Measured on stock 2.55.0: `one() is not a valid attribute name:
    // src/lib.rs:1`, then the ordinary "nothing is set" silence on stdout.
    out.push(
        Case::strict("check-attr", &["check-attr", "-a", "README.md", "src/lib.rs"], Shape::Linear)
            .with_config(&[("core.attributesFile", "src/lib.rs")]),
    );
    // A path that does not exist is not an error.
    cfg(out, "check-attr", &["check-attr", "-a", "README.md"], Shape::Attributes, "core.attributesFile", "no/such/attributes");
    // The same source driving a conversion rather than a report.
    out.push(
        Case::with_stdin("hash-object", &["hash-object", "--stdin", "--path=a.txt"], Shape::Linear, CRLF)
            .with_config(&[("core.attributesFile", "sub/.gitattributes"), ("core.autocrlf", "input")]),
    );
}

// ---------------------------------------------------------------------------
// `archive`: the conversion on the way out
// ---------------------------------------------------------------------------

/// `archive` runs the smudge half on every file it packs and honours
/// `export-ignore`, so the stream it writes is a function of the attributes as
/// much as of the tree. `corpus/archive_export.rs` owns the formats and the
/// prefixes; what is here is only the conversion, and only under configuration
/// it does not already carry.
///
/// `--worktree-attributes` switches the lookup from the archived tree's
/// `.gitattributes` to the worktree's, which is a different code path even when
/// the two files happen to agree.
fn archive_conversion(out: &mut Vec<Case>) {
    for (k, v) in [("core.autocrlf", "input"), ("core.eol", "crlf")] {
        cfg(out, "archive", &["archive", "--format=tar", "HEAD"], Shape::Attributes, k, v);
    }
    cfg(out, "archive", &["archive", "--format=tar", "--worktree-attributes", "HEAD"], Shape::Attributes, "core.autocrlf", "true");
    cfg(out, "archive", &["archive", "--format=tar", "HEAD", "--", "sub"], Shape::Attributes, "core.eol", "crlf");
    cfg(out, "archive", &["archive", "--format=tar", "HEAD"], Shape::Whitespace, "core.autocrlf", "true");
}

// ---------------------------------------------------------------------------
// The negative pin: a configured filter that nothing selects
// ---------------------------------------------------------------------------

/// `filter.<n>.*` is unreachable as a *driver* for the reason the module header
/// states, and that unreachability is worth a case rather than only a comment.
///
/// A port that ran a configured `filter.x.clean` on paths whose `filter`
/// attribute does not name `x` would rewrite every blob it stored — the worst
/// class of defect this module can look for — and stock git's behaviour is to do
/// nothing at all. Measured on stock 2.55.0,
/// `-c filter.x.clean=false -c filter.x.required=true add -A` on
/// `Shape::Attributes` exited 0 and staged normally: with `required=true` a
/// driver that exits non-zero is fatal *if it runs*, so exit 0 is proof it did
/// not.
///
/// The same shape is used for `smudge`, on the commands that would run one, and
/// for `process`, whose configured value is never even spawned. `false` is the
/// value throughout precisely because a spawned `false` cannot be mistaken for a
/// successful no-op.
fn inert_filters(out: &mut Vec<Case>) {
    const CLEAN: (&str, &str) = ("filter.x.clean", "false");
    const SMUDGE: (&str, &str) = ("filter.x.smudge", "false");
    const REQ: (&str, &str) = ("filter.x.required", "true");
    const PROCESS: (&str, &str) = ("filter.x.process", "false");

    cfg2(out, "add", &["add", "-A"], Shape::Attributes, &[CLEAN, REQ]);
    cfg2(out, "checkout-index", &["checkout-index", "--prefix=out/", "-a"], Shape::Attributes, &[SMUDGE, REQ]);
    cfg2(out, "checkout", &["checkout", "HEAD~2", "--", "ws/eol.txt"], Shape::Whitespace, &[SMUDGE, REQ]);
    cfg2(out, "status", &["status", "--porcelain=v1"], Shape::Attributes, &[CLEAN, REQ]);
    // The long-running protocol, configured and never spawned. A port that
    // started the process would block or die; stock does neither.
    cfg2(out, "add", &["add", "-A"], Shape::Attributes, &[PROCESS, REQ]);
    // A driver with a `required` flag and no commands at all.
    cfg2(out, "add", &["add", "-A"], Shape::Attributes, &[REQ]);
    // And the clean-side plumbing, where a wrongly-run filter would be loudest.
    out.push(
        Case::with_stdin("hash-object", &["hash-object", "--stdin", "--path=src/tabs.rs"], Shape::Attributes, CRLF)
            .with_config(&[CLEAN, REQ]),
    );
    out.push(
        Case::with_stdin("hash-object", &["hash-object", "-w", "--stdin", "--path=notes.info"], Shape::Attributes, IDENT_EXPANDED)
            .with_config(&[CLEAN, REQ]),
    );
}
