//! Differential corpus cases for the **identity-rewriting engine**: `mailmap.c`
//! — the file grammar it parses, the three places the table comes from, and
//! every verb that consults the finished table.
//!
//! Every case here is compared against stock git for stdout, exit code and
//! post-command repository state; the two `mailmap.blob` cases that name a
//! non-blob compare stderr too, because there the message is the whole
//! behaviour.
//!
//! # Territory, against the neighbours that share the subject
//!
//! Read in full before writing any of this: `hooks_identity.rs` (the nearest
//! neighbour), `text_plumbing.rs`, `log_format.rs`, `blame_lines.rs`,
//! `config_reads.rs`, `env_layer.rs`, `commit_message.rs`, plus the mailmap
//! blocks in `shape_reach.rs`, `informational.rs` and `info_attrs.rs`.
//!
//! | module | what it owns |
//! |--------|--------------|
//! | `hooks_identity.rs` | the mailmap **source keys** as bare settings: `mailmap.file` = `.mailmap` / `no-such-mailmap`, `mailmap.blob` = `HEAD:.mailmap` / `HEAD:no-such-path`, each alone, each read by one `check-mailmap`; plus `log.mailmap=false` beside a configured source. Also `user.name`/`user.email`/`--author=` spellings, the hooks half, and `i18n.commitEncoding` |
//! | `shape_reach.rs` | one invocation per *mapping already in the fixture's `.mailmap`*: ten `check-mailmap` operands, seven `log` formats, six `shortlog` forms, three `blame` forms — all with **no configuration at all** |
//! | `informational.rs` | `check-mailmap`'s *lookup rules* against the fixture table (case-insensitive email, the name half, name-only entries), `--stdin` beside operands, and the bare existence of `--mailmap-file` / `--mailmap-blob` |
//! | `text_plumbing.rs` | `check-mailmap --stdin` as a **text filter**: CRLF, no trailing newline, a blank line, no angle brackets, an interior NUL, invalid UTF-8, padding — the *payload* shapes, never the table's |
//! | `log_format.rs` | the pretty-atom table, including `%an|%aN|%ae|%aE|%al|%aL` and `--use-mailmap` against `%an`; `log.mailmap=false`; `mailmap.file=.mailmap`; and `--author=Alias` under `--no-use-mailmap` |
//! | `blame_lines.rs` | `blame`/`annotate` line selection, `-C`/`-M`, porcelain framing — and it records that the mailmap keys belong elsewhere |
//! | `info_attrs.rs` | `check-mailmap` on shapes with **no `.mailmap` at all**, plus `mailmap.file` naming a missing file and naming `src/lib.rs` |
//! | `config_reads.rs` / `env_layer.rs` / `commit_message.rs` | no mailmap case of any kind; checked, nothing to divide |
//!
//! # The lever, stated plainly
//!
//! Everything above reads **the table the fixture already ships**. Four lines in
//! `Shape::Attributes`'s `.mailmap`, and no case anywhere can add a fifth — so
//! the grammar `mailmap.c:read_mailmap_line()` implements was measured only
//! through whatever forms those four lines happen to be, and three of the
//! documented forms plus every malformed one were unreachable.
//!
//! **`mailmap.blob` is not the lever.** It names a blob, and a case is one argv
//! against a pristine copy, so the only blobs it can name are the ones the
//! fixture already committed — `HEAD:.mailmap` and a handful of files that are
//! not mailmaps. Choosing the *bytes* through the blob route needs a `git add`
//! before the read, which is a [`crate::runner::Sequence`], and sequences live
//! in `sequences.rs`. Measured, and it is real: with `.gitmodules` staged,
//! `-c mailmap.blob=:.gitmodules check-mailmap 'Old Name <old@example.invalid>'`
//! answered `From Blob <blob@example.invalid>` on stock 2.55.0. It is simply not
//! reachable from a `Case`.
//!
//! **`mailmap.file` is the lever, and it works.** [`ConfigEntry::raw`] on
//! [`ConfigScope::Modules`] writes arbitrary bytes to `.gitmodules` at a path a
//! case can name, `Shape::Attributes` has no `.gitmodules` of its own, and
//! nothing in this module reads `submodule.*`. `ignore_engine.rs` uses the same
//! file for `core.excludesFile` and `attributes_engine.rs` for
//! `core.attributesFile`; pointing `mailmap.file` at it turns the bytes into a
//! mailmap. Verified against stock 2.55.0 on a copy of the fixture:
//!
//! ```text
//! $ cat .gitmodules
//! Grammar One <one@example.invalid> <old@example.invalid>
//! $ git -c mailmap.file=.gitmodules check-mailmap 'Old Name <old@example.invalid>'
//! Grammar One <one@example.invalid>
//! ```
//!
//! That unlocks the whole grammar: all four line forms, a comment, a blank
//! line, malformed lines, duplicates, a self-mapping, case folding, whitespace
//! — and it lets one chosen table be read by a dozen verbs in turn, which is the
//! only way to catch a port that reads the table correctly in one place and not
//! in another.
//!
//! # What is here that none of them has
//!
//! **1. The file grammar** ([`line_forms`], [`grammar_edges`]). All four
//! documented forms in one table, plus: `#` at column zero versus an *indented*
//! `#` (which is not a comment and becomes a name), a blank line, a line of
//! spaces, a line with no `<`, an unterminated `<`, a nested `<` inside an
//! email, a name with no email, an empty `<>` on either half, a trailing `#`
//! comment after a complete mapping, three ident pairs on one line, a duplicate
//! key, a self-mapping, a chain that is *not* followed transitively, the
//! case-insensitive email key, tabs as separators, outer whitespace stripped,
//! inner whitespace **not** stripped, and a whole file in CRLF.
//!
//! **2. How two tables combine** ([`table_merge`]). `add_mapping` merges
//! per *field*, not per entry: a later `<new@> <old@>` line replaces the email
//! of an entry whose name an earlier table set, and the result is a name and an
//! email from two different files. The fixture's `.mailmap` is always read, so
//! every case here is that merge; the pair that makes it visible is
//! `Old Name <old@example.invalid>` → `Proper Name <form-two@example.invalid>`.
//! A name-specific entry also beats a name-less one for the same email
//! regardless of which file supplied which.
//!
//! **3. Where the table comes from, and in what order** ([`sources`],
//! [`cli_sources`]). Measured on stock 2.55.0: `.mailmap` first, then
//! `mailmap.blob`, then `mailmap.file`, then `check-mailmap`'s own
//! `--mailmap-blob`/`--mailmap-file` — and the command-line flags are
//! **additive to** the configuration keys rather than replacing them, which is
//! the single most likely thing for a port to get backwards. Also: a
//! `mailmap.blob` naming a tree and one naming a commit, which print
//! `error: mailmap is not a blob: …` on stderr and still exit 0; one naming a
//! binary blob; one naming a file that is not a mailmap; a `mailmap.file`
//! naming a directory; and the worktree `.mailmap` becoming invisible from a
//! cwd of `.git` while the blob route still works.
//!
//! **4. One table, every consumer** ([`consumers`]). The same six lines read by
//! `log`, `show`, `shortlog` (walk *and* stdin filter), `blame`, `blame
//! --porcelain`, `annotate`, `check-mailmap`, `cat-file --use-mailmap` in five
//! spellings, `for-each-ref`'s `:mailmap` atoms, `format-patch` and `rev-list`.
//! Five asymmetries fall out, each verified by hand and each a place a port can
//! read the table in one verb and not its twin:
//!
//! | pair | stock 2.55.0 |
//! |------|--------------|
//! | `log --pretty=email` vs `format-patch --stdout` | `From: Form Four <form-four@…>` vs `From: Typo Name <typo@…>` — `format-patch` never consults the table, and has no `--use-mailmap` to make it |
//! | `log --pretty=medium` vs `rev-list --pretty=medium` | `Author: Form Four` vs `Author: Typo Name` — `rev-list` does not set `rev.mailmap` and rejects `--use-mailmap` outright |
//! | `%(authorname:mailmap)` vs `%(creator:mailmap)` | `Form One` vs `zvcs parity` — the modifier is accepted on `creator` and silently does nothing |
//! | `cat-file --use-mailmap` vs `log.mailmap` | the flag rewrites regardless of `log.mailmap`, and `log.mailmap=true` alone never makes `cat-file` rewrite |
//! | `log --author=` vs `shortlog --author=` | `log` matches the **rewritten** identity and `shortlog` the **raw** one, while printing the rewritten one — `--author='Typo Name'` finds nothing in `log` and one commit in `shortlog`, and `--author='Form Four'` is the exact inverse |
//!
//! **5. The polarity of `%aN` against the two switches** ([`consumers`]).
//! `%aN`/`%aE` resolve through the table under `--no-use-mailmap` *and* under
//! `log.mailmap=false` — both switches move only the built-in formats and
//! `%an`. A port that wires either switch into the `%aN` lookup passes the
//! existing `log_format.rs` pair and fails here.
//!
//! **6. The search side** ([`search`]). `--author=`/`--committer=` match the
//! *rewritten* identity, so a chosen table decides which commits a search finds
//! — including the committer half, which nothing covered: every commit in every
//! fixture carries the identity `env::harden` pins, and a table keyed on
//! `parity@example.invalid` rewrites exactly that, turning `--committer=Form
//! One` into a four-commit hit and `--committer=zvcs parity` into none.
//!
//! # What `env::harden` leaves reachable, and what it does not
//!
//! `harden` pins author and committer for every commit the fixture builder
//! makes, so the committer of all twenty-two shapes is
//! `zvcs parity <parity@example.invalid>` and a mapping keyed on any other
//! committer identity would match nothing. That pinned identity is itself
//! rewritable — verified, `Form One <parity@example.invalid>` renames it
//! everywhere — which is what makes the committer-side and `for-each-ref`
//! `%(committername:mailmap)` cases here real rather than vacuous.
//!
//! The *author* side is the exception `Shape::Attributes` was built for:
//! `--author=` beats the environment, so three commits there carry
//! `Old Name <old@…>`, `Alias Name <alias@…>` and `Typo Name <typo@…>`. Those
//! three plus the pinned one are the only commit identities any case can key a
//! mapping on, and every table below keys on exactly those four.
//!
//! # What is not measurable here, and why
//!
//! * **A chosen `mailmap.blob`.** See the lever section: it needs a staged
//!   `.gitmodules`, which needs a sequence. The blob cases below therefore name
//!   fixture objects only.
//! * **A mailmap file with no trailing newline.** `runner::render_config_entry`
//!   appends exactly one `\n` to a raw entry, so the last line is always
//!   terminated. Nothing here depends on it; the unterminated-*input* rule is
//!   `text_plumbing.rs`'s, on the `--stdin` side.
//! * **A bare repository's implicit `HEAD:.mailmap`.** `read_mailmap` falls back
//!   to that blob only when `is_bare_repository()`, and no bare shape carries a
//!   `.mailmap`. `git --bare` from inside a worktree dies with a message that
//!   embeds the fixture root, which differs between the sides by construction.
//!   The `cwd=.git` pair below reaches the *other* half of the same branch — a
//!   repository where the worktree `.mailmap` is not found — and is as close as
//!   a case gets.
//! * **`am`.** It writes the author the mailbox names, verbatim; stock applies no
//!   mailmap to it (`format-patch` above is the reason — the patch it consumes
//!   never carried a rewritten identity in the first place), so an `am` case
//!   would measure `am`, not the table. `am_deep.rs` owns `am`.
//! * **A tag object's `%(taggername:mailmap)`.** `Shape::Attributes` has no
//!   tags, and the shapes that have tags have no `.mailmap`; `for-each-ref
//!   refs/tags/` there prints nothing. The atom is covered on its author and
//!   committer twins instead.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    line_forms(out);
    grammar_edges(out);
    table_merge(out);
    sources(out);
    cli_sources(out);
    consumers(out);
    search(out);
}

// ---------------------------------------------------------------------------
// Shared: the table lever and the probe identities
// ---------------------------------------------------------------------------

/// `.gitmodules` holding `body`, named as `mailmap.file`.
///
/// The raw entry is written verbatim by `runner::install_config`, which appends
/// exactly one `\n` — so `body` is the file minus its final newline.
/// `Shape::Attributes` ships no `.gitmodules`, so the file is these bytes and
/// nothing else, and no verb in this module reads `submodule.*`.
fn table(body: &'static str) -> Vec<ConfigEntry> {
    vec![
        ConfigEntry::raw(ConfigScope::Modules, body),
        ConfigEntry::set(ConfigScope::CommandLine, "mailmap.file", ".gitmodules"),
    ]
}

/// One case against [`Shape::Attributes`] whose extra mailmap source is `body`.
fn t(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], body: &'static str) {
    out.push(Case::new(cmd, args, Shape::Attributes).with_scoped_config(table(body)));
}

/// `t`, with `extra` `-c` settings layered on top of the table.
fn tc(
    out: &mut Vec<Case>,
    cmd: &'static str,
    args: &[&str],
    body: &'static str,
    extra: &[(&str, &str)],
) {
    let mut config = table(body);
    for (k, v) in extra {
        config.push(ConfigEntry::set(ConfigScope::CommandLine, *k, *v));
    }
    out.push(Case::new(cmd, args, Shape::Attributes).with_scoped_config(config));
}

/// One `check-mailmap` case reading `body`, asked about `idents`.
fn ask(out: &mut Vec<Case>, body: &'static str, idents: &[&str]) {
    let mut argv: Vec<&str> = vec!["check-mailmap"];
    argv.extend_from_slice(idents);
    t(out, "check-mailmap", &argv, body);
}

/// The four commit identities any mapping can key on: the three
/// `Shape::Attributes` writes with `--author=`, and the one `env::harden` pins
/// on every commit's committer line and on the seed commit's author line.
const OLD: &str = "Old Name <old@example.invalid>";
const ALIAS: &str = "Alias Name <alias@example.invalid>";
const TYPO: &str = "Typo Name <typo@example.invalid>";
const PINNED: &str = "zvcs parity <parity@example.invalid>";

// ---------------------------------------------------------------------------
// 1. The four line forms
// ---------------------------------------------------------------------------

/// Every documented line form, a comment and a blank line, in one table.
///
/// Written once and reused by [`consumers`], so the same six lines are what a
/// dozen verbs are asked to read.
///
/// * `Form One <parity@…>` — a *name-only* entry: it replaces the name of
///   anyone whose email is `parity@…` and keeps the email.
/// * `<form-two@…> <old@…>` — the **email-to-email** form, which the fixture's
///   own `.mailmap` does not contain at all, so it had no case anywhere.
/// * `Form Three <form-three@…> <alias@…>` — name and email for a commit email.
/// * `Form Four <form-four@…> Typo Name <typo@…>` — name and email for a
///   *specific* (name, email) pair.
///
/// Observed against stock 2.55.0, `log --format='%an|%aN|%ae|%aE'`, newest
/// first:
///
/// | raw | resolved |
/// |-----|----------|
/// | `Typo Name <typo@…>` | `Form Four <form-four@…>` |
/// | `Alias Name <alias@…>` | `Proper Name <proper@…>` — the fixture's *name-specific* entry wins over form three's name-less one |
/// | `Old Name <old@…>` | `Proper Name <form-two@…>` — name from `.mailmap`, email from form two |
/// | `zvcs parity <parity@…>` | `Form One <parity@…>` |
const FORMS: &str = "# comment: not a mapping\n\
\n\
Form One <parity@example.invalid>\n\
<form-two@example.invalid> <old@example.invalid>\n\
Form Three <form-three@example.invalid> <alias@example.invalid>\n\
Form Four <form-four@example.invalid> Typo Name <typo@example.invalid>";

/// The four forms, asked of the lookup and of a walk.
fn line_forms(out: &mut Vec<Case>) {
    // Every identity the table can key on, plus the two that show a *name-less*
    // entry firing where the fixture's name-specific one does not apply:
    // `Other Name <alias@…>` falls through to form three, `Other Name <typo@…>`
    // matches neither form four nor anything in `.mailmap` and is echoed back.
    ask(out, FORMS, &[PINNED, OLD, ALIAS, TYPO]);
    ask(
        out,
        FORMS,
        &[
            "Other Name <alias@example.invalid>",
            "Other Name <typo@example.invalid>",
            "<typo@example.invalid>",
            "Nobody <nobody@example.invalid>",
        ],
    );
    // The email half is matched case-insensitively and the name half exactly,
    // so a shouted email still reaches form two and a lower-cased name misses
    // form four.
    ask(
        out,
        FORMS,
        &["<OLD@EXAMPLE.INVALID>", "typo name <typo@example.invalid>", "<PARITY@example.INVALID>"],
    );

    t(out, "log", &["log", "--format=%an|%aN|%ae|%aE"], FORMS);
    t(out, "log", &["log", "--format=%cn|%cN|%ce|%cE"], FORMS);
    t(out, "log", &["log", "--format=%aN <%aE>", "--reverse"], FORMS);
}

// ---------------------------------------------------------------------------
// 2. The grammar the parser implements
// ---------------------------------------------------------------------------

/// `#` is a comment only at column zero.
///
/// Observed: with this table, `check-mailmap 'Old Name <old@example.invalid>'`
/// answers `# indented <hash@example.invalid>` — the leading whitespace is
/// stripped *after* the comment test, so `# indented` becomes the replacement
/// name. A parser that trims first and then tests loses the entry entirely.
const E_INDENTED_HASH: &str = "   # indented <hash@example.invalid> <old@example.invalid>";

/// A comment at column zero, a blank line, and a mapping that must survive both.
const E_COMMENT_BLANK: &str = "# Ignored <c0@example.invalid> <old@example.invalid>\n\
\n\
Kept <kept@example.invalid> <old@example.invalid>";

/// A line of nothing but spaces, which is not a mapping and is not an error.
const E_SPACE_LINE: &str = "   \nGood <good@example.invalid> <old@example.invalid>";

/// A line with no `<` at all, and a name with no email. Both are silently
/// skipped; the mapping after them still applies.
const E_NO_BRACKET: &str = "no angle brackets on this line\n\
Only A Name\n\
Good <good@example.invalid> <old@example.invalid>";

/// An unterminated `<`. Observed: the whole line is discarded — the following
/// mapping is what answers.
const E_UNCLOSED: &str = "Unclosed <unclosed@example.invalid\n\
Good <good@example.invalid> <old@example.invalid>";

/// A `<` *inside* an email. Observed: no effect at all — the fixture's
/// `.mailmap` answers, so the line was rejected rather than half-parsed.
const E_NESTED_BRACKET: &str = "Nest <a<b>@example.invalid> <old@example.invalid>";

/// An empty replacement email, and an empty commit email. Observed: neither
/// line does anything, not even the name half of the first.
const E_EMPTY_EMAIL: &str = "Empty Target <> <old@example.invalid>\n\
Some One <some@example.invalid> <>\n\
Live <live@example.invalid> <alias@example.invalid>";

/// A `#` *after* a complete mapping. Observed: the mapping applies and the tail
/// is ignored — the comment test never runs past column zero, and the third
/// ident pair on a line is dropped for an unrelated reason (see below).
const E_TRAILING_HASH: &str = "Hash Name <hash@example.invalid> <old@example.invalid> # trailing";

/// Three ident pairs on one line. Only the first two are read: the line maps
/// `B <old@…>` to `A <a@…>` and the `C <alias@…>` tail is discarded, so
/// `Other <alias@…>` is left to the fixture's own table.
const E_THREE_PAIRS: &str = "A <a@example.invalid> B <old@example.invalid> C <alias@example.invalid>";

/// The same key twice. Observed: the second line wins.
const E_DUPLICATE: &str = "First <first@example.invalid> <old@example.invalid>\n\
Second <second@example.invalid> <old@example.invalid>";

/// A mapping whose target is itself. It must not loop, and must not be treated
/// as an absent entry either — it pins the raw identity against the fixture's
/// own `.mailmap`, which would otherwise rewrite `old@…`.
const E_SELF: &str = "Old Name <old@example.invalid> <old@example.invalid>";

/// A chain. Lookup is one pass, not a walk: `old@…` resolves to `mid@…` and
/// stops there, never reaching `end@…`.
const E_CHAIN: &str = "Mid <mid@example.invalid> <old@example.invalid>\n\
End <end@example.invalid> <mid@example.invalid>";

/// A shouted commit email as the *key*. The email half is folded, so this fires
/// for the lower-cased identity every commit actually carries.
const E_KEY_CASE: &str = "Upper <up@example.invalid> <OLD@EXAMPLE.INVALID>";

/// Tabs between the fields, and leading/trailing whitespace around the whole
/// line. Both are stripped.
const E_OUTER_SPACE: &str = "  \tPadded Name\t<pad@example.invalid>\t<old@example.invalid>  \t";

/// Whitespace *inside* the angle brackets is **not** stripped, so the key
/// becomes `  old@example.invalid  ` and matches nothing. The fixture's
/// `.mailmap` answers instead — which is how the case tells "not stripped"
/// apart from "stripped and matched".
const E_INNER_SPACE: &str = "Inner <  pad@example.invalid  > <  old@example.invalid  >";

/// A whole file in CRLF. The `\r` lands after the closing `>` and is stripped
/// with the rest of the trailing whitespace, so every line still parses.
const E_CRLF: &str = "Crlf One <crlf@example.invalid> <old@example.invalid>\r\n\
Crlf Two <crlf2@example.invalid> <typo@example.invalid>\r";

/// One table per parser rule, each asked about the identity it targets.
///
/// The probes are deliberately short: a rule that fires is visible as one
/// rewritten line, and a rule that does not is visible as the fixture's own
/// `.mailmap` answering instead (`Proper Name <proper@example.invalid>` for
/// `old@…`, `Typo Name <canonical@example.invalid>` for `typo@…`).
fn grammar_edges(out: &mut Vec<Case>) {
    for body in [
        E_INDENTED_HASH,
        E_COMMENT_BLANK,
        E_SPACE_LINE,
        E_NO_BRACKET,
        E_UNCLOSED,
        E_NESTED_BRACKET,
        E_TRAILING_HASH,
        E_DUPLICATE,
        E_SELF,
        E_CHAIN,
        E_KEY_CASE,
        E_OUTER_SPACE,
        E_INNER_SPACE,
    ] {
        ask(out, body, &[OLD]);
    }

    // Rules whose answer needs a second identity to be readable.
    ask(out, E_EMPTY_EMAIL, &[OLD, "Other <alias@example.invalid>", "Nobody <nobody@example.invalid>"]);
    ask(out, E_THREE_PAIRS, &["B <old@example.invalid>", "Other <alias@example.invalid>"]);
    ask(out, E_CRLF, &[OLD, "Other Name <typo@example.invalid>"]);
    ask(out, E_KEY_CASE, &["<OLD@EXAMPLE.INVALID>", "x <OlD@ExAmPlE.InVaLiD>"]);

    // Every malformed table must also be harmless to a *walk*, not only to the
    // lookup — a parser that dies on a bad line dies inside `log`, not inside
    // `check-mailmap`, and the exit code is what says so.
    t(out, "log", &["log", "--format=%aN <%aE>"], E_NO_BRACKET);
    t(out, "log", &["log", "--format=%aN <%aE>"], E_UNCLOSED);
    t(out, "log", &["log", "--format=%aN <%aE>"], E_INDENTED_HASH);
    t(out, "shortlog", &["shortlog", "-sne", "HEAD"], E_DUPLICATE);
}

// ---------------------------------------------------------------------------
// 3. How two tables combine
// ---------------------------------------------------------------------------

/// An email-only rewrite of an identity the fixture's `.mailmap` already gives
/// a *name* to.
///
/// `mailmap.c:add_mapping` updates the fields it was given and leaves the rest
/// of the entry alone, so the answer is assembled from two files:
/// `.mailmap`'s `Proper Name <proper@…> <old@…>` supplies the name and this
/// supplies the email.
///
/// Observed: `check-mailmap 'Old Name <old@example.invalid>'` →
/// `Proper Name <merged@example.invalid>`.
const M_EMAIL_ONLY: &str = "<merged@example.invalid> <old@example.invalid>";

/// The mirror: a *name-only* entry keyed on an email the fixture rewrites.
/// `.mailmap` sets name and email for `old@…`; this replaces only the name of
/// anyone already at `proper@…`, which is a different key, so it does not fire
/// for `old@…` and does fire for `proper@…` directly.
const M_NAME_ONLY: &str = "Renamed Proper <proper@example.invalid>";

/// A name-less entry for an email whose name-specific entry lives in
/// `.mailmap`. The specific one wins for `Alias Name`; this one catches every
/// other name at that address.
const M_SPECIFIC_VS_DEFAULT: &str = "Default Alias <default@example.invalid> <alias@example.invalid>";

/// A name-specific entry that *replaces* one `.mailmap` already has for the
/// same (name, email) key — both fields, so nothing of the earlier entry shows.
const M_SPECIFIC_OVERRIDE: &str =
    "Replaced <replaced@example.invalid> Typo Name <typo@example.invalid>";

/// Two tables layered: the fixture's `.mailmap` is always read first, and every
/// case in this module is therefore a merge. These are the four shapes that
/// merge can take.
fn table_merge(out: &mut Vec<Case>) {
    ask(out, M_EMAIL_ONLY, &[OLD, "Other <old@example.invalid>"]);
    ask(out, M_NAME_ONLY, &[OLD, "Anyone <proper@example.invalid>"]);
    ask(out, M_SPECIFIC_VS_DEFAULT, &[ALIAS, "Other <alias@example.invalid>"]);
    ask(out, M_SPECIFIC_OVERRIDE, &[TYPO, "Other <typo@example.invalid>"]);

    // The merge as a walk sees it: one commit's name and email come from two
    // different files on the same line of output.
    t(out, "log", &["log", "--format=%aN <%aE>"], M_EMAIL_ONLY);
    t(out, "shortlog", &["shortlog", "-sne", "HEAD"], M_EMAIL_ONLY);
}

// ---------------------------------------------------------------------------
// 4. Where the table comes from
// ---------------------------------------------------------------------------

/// A table that rewrites `old@…` differently from the fixture's `.mailmap`, so
/// "which source answered" is readable off one line.
const S_FILE: &str = "From File <from-file@example.invalid> <old@example.invalid>";

/// The three sources, their order, and the objects a blob may name.
///
/// Order measured on stock 2.55.0: `.mailmap`, then `mailmap.blob`, then
/// `mailmap.file`. With `mailmap.blob=HEAD:.mailmap` *and*
/// `mailmap.file=.gitmodules` both set, `check-mailmap 'Old Name
/// <old@example.invalid>'` answered `From File <from-file@example.invalid>` —
/// the file won. `hooks_identity.rs` sets each key alone; the pair is what
/// establishes the order, and the pair is here.
fn sources(out: &mut Vec<Case>) {
    // File beats blob, for the same key, in the lookup and in a walk.
    tc(out, "check-mailmap", &["check-mailmap", OLD], S_FILE, &[("mailmap.blob", "HEAD:.mailmap")]);
    tc(
        out,
        "log",
        &["log", "--format=%aN <%aE>"],
        S_FILE,
        &[("mailmap.blob", "HEAD:.mailmap")],
    );

    // The blob route, pointed at each class of object the fixture contains. A
    // missing path, a blob that is not a mailmap, and a binary blob are all
    // "no extra entries" and exit 0 — the worktree `.mailmap` still answers.
    for blob in [
        "HEAD:.mailmap",
        "HEAD~3:.mailmap",
        "HEAD:.gitattributes",
        "HEAD:sub/.gitattributes",
        "HEAD:.gitignore",
        "HEAD:assets/logo.bin",
        "HEAD:docs/manual.md",
        "HEAD:no-such-path",
        ":.mailmap",
        ":0:.mailmap",
    ] {
        out.push(
            Case::new("check-mailmap", &["check-mailmap", OLD, TYPO], Shape::Attributes)
                .with_config(&[("mailmap.blob", blob)]),
        );
    }

    // A `mailmap.blob` that resolves to something that is not a blob prints a
    // diagnostic on stderr and still exits 0 with the rest of the table intact.
    // Observed: `error: mailmap is not a blob: HEAD:sub`. No path in the
    // message, so both sides can be compared byte for byte.
    for blob in ["HEAD:sub", "HEAD", "HEAD^{tree}"] {
        out.push(
            Case::strict("check-mailmap", &["check-mailmap", OLD], Shape::Attributes)
                .with_config(&[("mailmap.blob", blob)]),
        );
    }

    // `mailmap.file` pointed at things that are not mailmap files. A directory
    // and a binary blob are both silently "no entries", not errors.
    for file in [".gitattributes", "sub", "assets/logo.bin", "docs/manual.md", ".git"] {
        out.push(
            Case::new("check-mailmap", &["check-mailmap", OLD], Shape::Attributes)
                .with_config(&[("mailmap.file", file)]),
        );
    }

    // `mailmap.file` is resolved from the worktree root, not the working
    // directory: `check-mailmap` chdirs to the top before it reads, so the same
    // relative name finds the same file from `sub/` and `../.gitmodules`
    // escapes the repository and finds nothing.
    out.push(
        Case::new("check-mailmap", &["check-mailmap", OLD, TYPO], Shape::Attributes)
            .with_scoped_config(table(FORMS))
            .in_dir("sub"),
    );
    out.push(
        Case::new("check-mailmap", &["check-mailmap", OLD, TYPO], Shape::Attributes)
            .with_scoped_config(vec![
                ConfigEntry::raw(ConfigScope::Modules, FORMS),
                ConfigEntry::set(ConfigScope::CommandLine, "mailmap.file", "../.gitmodules"),
            ])
            .in_dir("sub"),
    );

    // From a cwd of `.git` there is no worktree to find `.mailmap` in, so the
    // fixture's own table disappears — and the blob route, which reads from the
    // object store, still works. Observed: `Old Name <old@example.invalid>`
    // unchanged in the first, `Proper Name <proper@example.invalid>` in the
    // second.
    out.push(Case::new("check-mailmap", &["check-mailmap", OLD], Shape::Attributes).in_dir(".git"));
    out.push(
        Case::new("check-mailmap", &["check-mailmap", OLD], Shape::Attributes)
            .with_config(&[("mailmap.blob", "HEAD:.mailmap")])
            .in_dir(".git"),
    );
}

// ---------------------------------------------------------------------------
// 5. The command-line sources, against the configuration keys
// ---------------------------------------------------------------------------

/// `--mailmap-file` / `--mailmap-blob` beside `mailmap.file` / `mailmap.blob`.
///
/// The finding, measured on stock 2.55.0: the flags are **additive** and are
/// read *after* the keys.
///
/// | invocation | answer for `Old Name <old@example.invalid>` |
/// |------------|--------------------------------------------|
/// | `-c mailmap.file=.gitmodules` | `Proper Name <form-two@example.invalid>` |
/// | `-c mailmap.file=.gitmodules --mailmap-file .mailmap` | `Proper Name <proper@example.invalid>` — the flag's file was read second and replaced the email |
/// | `-c mailmap.file=.mailmap --mailmap-file .gitmodules` | `Proper Name <form-two@example.invalid>` — the other order, the other winner |
///
/// A port that assigns the flag's value over the key's — the obvious reading of
/// "the command line wins" — gets the second row wrong, because `.gitmodules`
/// would then never be read and `Typo Name` would not become `Form Four`.
fn cli_sources(out: &mut Vec<Case>) {
    let cm = "check-mailmap";

    // Both orders of the same two files, with two probes: the first shows which
    // source won for a colliding key, the second shows that the *loser* was
    // still read.
    t(out, cm, &[cm, "--mailmap-file", ".mailmap", OLD, TYPO], FORMS);
    out.push(
        Case::new(cm, &[cm, "--mailmap-file", ".gitmodules", OLD, TYPO], Shape::Attributes)
            .with_scoped_config(vec![
                ConfigEntry::raw(ConfigScope::Modules, FORMS),
                ConfigEntry::set(ConfigScope::CommandLine, "mailmap.file", ".mailmap"),
            ]),
    );

    // The flag repeated: the last one is the one that is read.
    t(out, cm, &[cm, "--mailmap-file", ".mailmap", "--mailmap-file", ".gitmodules", OLD], FORMS);
    t(out, cm, &[cm, "--mailmap-file", ".gitmodules", "--mailmap-file", ".mailmap", OLD], FORMS);

    // An empty value is "no file", not the current directory and not an error.
    out.push(Case::new(cm, &[cm, "--mailmap-file", "", OLD], Shape::Attributes));

    // The blob flag against the blob key, same question.
    out.push(
        Case::new(cm, &[cm, "--mailmap-blob", "HEAD:.mailmap", OLD], Shape::Attributes)
            .with_config(&[("mailmap.blob", "HEAD:.gitattributes")]),
    );
    // Both flags at once, with the file chosen: the blob supplies the name for
    // `old@…` and the file the email, which is the merge rule reached through
    // the two command-line doors instead of the two configuration ones.
    t(out, cm, &[cm, "--mailmap-blob", "HEAD:.mailmap", "--mailmap-file", ".gitmodules", OLD, TYPO], M_EMAIL_ONLY);
    // A flag naming a non-blob: the same `error: mailmap is not a blob:` path
    // as the configuration key, reached through the option parser.
    out.push(Case::strict(cm, &[cm, "--mailmap-blob", "HEAD:sub", OLD], Shape::Attributes));
}

// ---------------------------------------------------------------------------
// 6. One table, every consumer
// ---------------------------------------------------------------------------

/// A `git log --pretty=short`-shaped stream for `shortlog`'s filter mode.
///
/// Two of the identities [`FORMS`] rewrites, in a payload no repository
/// supplies — `shortlog` reading stdin is a different entry point from
/// `shortlog` walking a revision range (`builtin/shortlog.c:read_from_stdin`),
/// and it consults the same table. Nothing in the corpus fed `shortlog` stdin
/// at all.
const SHORTLOG_STREAM: &[u8] = b"commit 1111111111111111111111111111111111111111\n\
Author: Typo Name <typo@example.invalid>\n\
\n\
    subject one\n\
\n\
commit 2222222222222222222222222222222222222222\n\
Author: Old Name <old@example.invalid>\n\
\n\
    subject two\n\
\n";

/// The same stream with an identity no table knows, and one with no email — the
/// two shapes the record parser has to survive before the lookup happens.
const SHORTLOG_STREAM_ODD: &[u8] = b"commit 3333333333333333333333333333333333333333\n\
Author: Nobody At All <nobody@example.invalid>\n\
\n\
    subject three\n\
\n\
commit 4444444444444444444444444444444444444444\n\
Author: No Email\n\
\n\
    subject four\n\
\n";

/// `HEAD`, for the `cat-file` batch modes.
const BATCH_HEAD: &[u8] = b"HEAD\n";

/// Two batch commands, so `info` and `contents` are both asked the question.
const BATCH_COMMANDS: &[u8] = b"info HEAD\ncontents HEAD\n";

/// [`FORMS`] read by every verb that consults a mailmap.
///
/// This is the group that finds a port which reads the table in one place and
/// not in another — see the asymmetry table in the module header. Each family
/// below is one entry point into `mailmap.c`, and they are deliberately given
/// *identical* bytes so a divergence names the reader rather than the parser.
fn consumers(out: &mut Vec<Case>) {
    // --- log: the atoms, the built-in formats, and the two switches ---------
    t(out, "log", &["log", "-1", "--format=%an|%aN|%cn|%cN", "HEAD"], FORMS);
    t(out, "log", &["log", "-1", "--use-mailmap", "--format=%an|%ae|%cn|%ce", "HEAD"], FORMS);
    // `%aN` is not what either switch moves: it resolves through the table
    // under `--no-use-mailmap` and under `log.mailmap=false` alike. Both
    // verified on stock 2.55.0 — `Form Four <form-four@example.invalid>` in
    // each case.
    t(out, "log", &["log", "-1", "--no-use-mailmap", "--format=%aN <%aE>", "HEAD"], FORMS);
    tc(out, "log", &["log", "-1", "--format=%aN <%aE>", "HEAD"], FORMS, &[("log.mailmap", "false")]);
    // What they do move: the built-in formats.
    t(out, "log", &["log", "-1", "HEAD"], FORMS);
    t(out, "log", &["log", "-1", "--no-use-mailmap", "HEAD"], FORMS);
    tc(out, "log", &["log", "-1", "HEAD"], FORMS, &[("log.mailmap", "false")]);
    t(out, "log", &["log", "-1", "--pretty=fuller", "HEAD"], FORMS);
    t(out, "log", &["log", "-1", "--pretty=full", "HEAD"], FORMS);
    t(out, "log", &["log", "-1", "--pretty=email", "HEAD"], FORMS);
    t(out, "log", &["log", "-3", "--pretty=email"], FORMS);

    // --- show: the same printer, reached through the other verb -------------
    t(out, "show", &["show", "-s", "HEAD"], FORMS);
    t(out, "show", &["show", "-s", "--format=%an|%aN", "HEAD"], FORMS);
    t(out, "show", &["show", "-s", "--no-use-mailmap", "HEAD"], FORMS);

    // --- format-patch: the consumer that does *not* read the table ----------
    // Stock 2.55.0 prints the raw identity here and rejects `--use-mailmap`
    // outright, while `log --pretty=email` above prints the rewritten one. A
    // port that routes `format-patch` through its `log` machinery unchanged
    // diverges on exactly this pair.
    t(out, "format-patch", &["format-patch", "--stdout", "-1", "HEAD"], FORMS);
    t(out, "format-patch", &["format-patch", "--stdout", "-3"], FORMS);
    tc(
        out,
        "format-patch",
        &["format-patch", "--stdout", "-1", "HEAD"],
        FORMS,
        &[("log.mailmap", "true")],
    );

    // --- rev-list: the other half of the same asymmetry ---------------------
    // `rev-list --pretty=medium` prints `Author: Typo Name <typo@…>` where
    // `log` prints `Author: Form Four <form-four@…>`: `builtin/rev-list.c` never
    // sets `rev.mailmap`. The `%aN` atom still resolves, because the atom does
    // its own lookup.
    t(out, "rev-list", &["rev-list", "--max-count=1", "--pretty=medium", "HEAD"], FORMS);
    t(out, "rev-list", &["rev-list", "--max-count=1", "--format=%an|%aN", "HEAD"], FORMS);

    // --- shortlog: on by default, and with no way to turn it off ------------
    t(out, "shortlog", &["shortlog", "-sne", "HEAD"], FORMS);
    t(out, "shortlog", &["shortlog", "-se", "--group=committer", "HEAD"], FORMS);
    t(out, "shortlog", &["shortlog", "-e", "HEAD"], FORMS);
    t(out, "shortlog", &["shortlog", "--group=author", "-sne", "HEAD"], FORMS);
    tc(out, "shortlog", &["shortlog", "-sne", "HEAD"], FORMS, &[("log.mailmap", "false")]);
    // The filter mode: the same table, applied to a stream instead of a walk.
    out.push(
        Case::with_stdin("shortlog", &["shortlog", "-se"], Shape::Attributes, SHORTLOG_STREAM)
            .with_scoped_config(table(FORMS)),
    );
    out.push(
        Case::with_stdin("shortlog", &["shortlog", "-sne"], Shape::Attributes, SHORTLOG_STREAM_ODD)
            .with_scoped_config(table(FORMS)),
    );
    // The filter mode with no extra source, so the fixture's own `.mailmap` is
    // the whole table — the floor the case above is measured against.
    out.push(Case::with_stdin(
        "shortlog",
        &["shortlog", "-se"],
        Shape::Attributes,
        SHORTLOG_STREAM,
    ));

    // --- blame and annotate -------------------------------------------------
    // Three paths, one per author identity in the fixture: `sub/nested.txt` is
    // seed + `old@…`, `docs/manual.md` is seed + `alias@…`, `src/tabs.rs` is
    // seed + `typo@…`. Between them every form in the table is printed.
    for path in ["sub/nested.txt", "docs/manual.md", "src/tabs.rs"] {
        t(out, "blame", &["blame", "-s", path], FORMS);
        t(out, "blame", &["blame", "--porcelain", path], FORMS);
    }
    t(out, "blame", &["blame", "sub/nested.txt"], FORMS);
    t(out, "blame", &["blame", "-e", "sub/nested.txt"], FORMS);
    t(out, "blame", &["blame", "--no-use-mailmap", "sub/nested.txt"], FORMS);
    t(out, "blame", &["blame", "--line-porcelain", "docs/manual.md"], FORMS);
    tc(out, "blame", &["blame", "sub/nested.txt"], FORMS, &[("log.mailmap", "false")]);
    t(out, "annotate", &["annotate", "sub/nested.txt"], FORMS);
    t(out, "annotate", &["annotate", "docs/manual.md"], FORMS);

    // --- cat-file --use-mailmap: the flag-only consumer ---------------------
    // It rewrites the *committer* line too, and it recomputes the object size:
    // `cat-file -s HEAD` is 236 and `cat-file -s --use-mailmap HEAD` is 238
    // under this table, so a port that rewrites the body and reports the stored
    // size diverges on the header while matching the payload.
    t(out, "cat-file", &["cat-file", "--use-mailmap", "commit", "HEAD"], FORMS);
    t(out, "cat-file", &["cat-file", "-p", "--use-mailmap", "HEAD"], FORMS);
    t(out, "cat-file", &["cat-file", "-s", "--use-mailmap", "HEAD"], FORMS);
    t(out, "cat-file", &["cat-file", "-s", "HEAD"], FORMS);
    t(out, "cat-file", &["cat-file", "-t", "--use-mailmap", "HEAD"], FORMS);
    // `log.mailmap` does not reach it in either direction: the flag rewrites
    // with the key off, and the key alone never rewrites.
    tc(
        out,
        "cat-file",
        &["cat-file", "--use-mailmap", "commit", "HEAD"],
        FORMS,
        &[("log.mailmap", "false")],
    );
    tc(out, "cat-file", &["cat-file", "commit", "HEAD"], FORMS, &[("log.mailmap", "true")]);
    // A non-commit under the flag: nothing to rewrite, and not an error.
    t(out, "cat-file", &["cat-file", "--use-mailmap", "blob", "HEAD:sub/nested.txt"], FORMS);
    for args in [
        &["cat-file", "--batch", "--use-mailmap"][..],
        &["cat-file", "--batch-check", "--use-mailmap"],
        &["cat-file", "--batch-check"],
    ] {
        out.push(
            Case::with_stdin("cat-file", args, Shape::Attributes, BATCH_HEAD)
                .with_scoped_config(table(FORMS)),
        );
    }
    out.push(
        Case::with_stdin(
            "cat-file",
            &["cat-file", "--batch-command", "--use-mailmap"],
            Shape::Attributes,
            BATCH_COMMANDS,
        )
        .with_scoped_config(table(FORMS)),
    );

    // --- for-each-ref: the `:mailmap` atom modifiers ------------------------
    // `%(authorname)` and `%(authorname:mailmap)` on the same line, so the pair
    // is one comparison; `%(creator:mailmap)` beside `%(committername:mailmap)`
    // is the asymmetry — the modifier is accepted on `creator` and does
    // nothing, so stock prints `zvcs parity` there and `Form One` next to it.
    t(
        out,
        "for-each-ref",
        &["for-each-ref", "--format=%(authorname)|%(authorname:mailmap)|%(committername:mailmap)"],
        FORMS,
    );
    t(
        out,
        "for-each-ref",
        &["for-each-ref", "--format=%(creator)|%(creator:mailmap)|%(committername:mailmap)"],
        FORMS,
    );
    t(
        out,
        "for-each-ref",
        &[
            "for-each-ref",
            "--format=[%(authoremail)][%(authoremail:trim)][%(authoremail:localpart)]\
             [%(authoremail:mailmap)][%(authoremail:mailmap,trim)][%(authoremail:mailmap,localpart)]",
        ],
        FORMS,
    );
    t(
        out,
        "for-each-ref",
        &["for-each-ref", "--format=%(committeremail:mailmap)|%(committeremail:mailmap,trim)"],
        FORMS,
    );
    // The modifier does not follow `log.mailmap`: it is its own switch.
    tc(
        out,
        "for-each-ref",
        &["for-each-ref", "--format=%(authorname:mailmap)"],
        FORMS,
        &[("log.mailmap", "false")],
    );

    // --- check-mailmap: the lookup itself, on the same bytes ----------------
    ask(out, FORMS, &[PINNED, OLD, ALIAS, TYPO, "Nobody <nobody@example.invalid>"]);
}

// ---------------------------------------------------------------------------
// 7. The search side
// ---------------------------------------------------------------------------

/// `--author=` / `--committer=` against a table the case chose.
///
/// `revision.c` runs the grep over the *resolved* identity whenever
/// `rev.mailmap` is set, and `log.mailmap` defaults to true — so a chosen table
/// decides which commits a search finds. Observed on stock 2.55.0 with
/// [`FORMS`]:
///
/// | invocation | hits |
/// |------------|------|
/// | `--author='Typo Name'` | none |
/// | `--author='Form Four'` | `attributes: by typo` |
/// | `--no-use-mailmap --author='Typo Name'` | `attributes: by typo` |
/// | `--no-use-mailmap --author='Form Four'` | none |
/// | `--author=form-two@example.invalid` | `attributes: by old address` |
/// | `--author=old@example.invalid` | none |
/// | `--committer='Form One'` | all four commits |
/// | `--committer='zvcs parity'` | none |
///
/// The committer rows are the half nothing covered: every commit in every
/// fixture carries the identity `env::harden` pins, so a committer search was
/// either all or nothing regardless of the table — until a table rewrote the
/// pinned identity itself.
///
/// `shortlog` answers the *opposite* way and has no switch to change it; the
/// three cases at the end of this group are that inversion.
fn search(out: &mut Vec<Case>) {
    for args in [
        &["log", "--oneline", "--author=Typo Name"][..],
        &["log", "--oneline", "--author=Form Four"],
        &["log", "--oneline", "--no-use-mailmap", "--author=Typo Name"],
        &["log", "--oneline", "--no-use-mailmap", "--author=Form Four"],
        &["log", "--oneline", "--author=form-two@example.invalid"],
        &["log", "--oneline", "--author=old@example.invalid"],
        &["log", "--oneline", "--committer=Form One"],
        &["log", "--oneline", "--committer=zvcs parity"],
        &["log", "--oneline", "--author=Form", "--committer=Form"],
    ] {
        t(out, "log", args, FORMS);
    }

    // With the key off the search sees raw identities again, which is the same
    // switch `--no-use-mailmap` throws and a second way to reach it.
    tc(out, "log", &["log", "--oneline", "--author=Form Four"], FORMS, &[("log.mailmap", "false")]);
    tc(out, "log", &["log", "--oneline", "--author=Typo Name"], FORMS, &[("log.mailmap", "false")]);

    // `shortlog` does **not** use the same matcher, and the pair proves it:
    // `builtin/shortlog.c` keeps its own mailmap and never sets `rev.mailmap`,
    // so its `--author=` grep runs over the raw header while its *output* prints
    // the rewritten identity. Observed on stock 2.55.0:
    // `shortlog -sne --author='Typo Name' HEAD` prints
    // `1\tForm Four <form-four@example.invalid>` and
    // `shortlog -sne --author='Form Four' HEAD` prints nothing — the exact
    // inverse of the `log` rows above.
    t(out, "shortlog", &["shortlog", "-sne", "--author=Typo Name", "HEAD"], FORMS);
    t(out, "shortlog", &["shortlog", "-sne", "--author=Form Four", "HEAD"], FORMS);
    t(out, "shortlog", &["shortlog", "-sne", "--author=Old Name", "HEAD"], FORMS);
}
