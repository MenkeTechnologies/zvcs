//! git's **pure text filters**: the verbs that read bytes on stdin, write bytes
//! on stdout, and barely consult the repository at all.
//!
//! `interpret-trailers`, `stripspace`, `mailinfo`, `mailsplit`, `check-mailmap
//! --stdin`, `check-ref-format` and `column` share one property that separates
//! them from everything else in the corpus: their answer is a function of the
//! **input bytes**, not of the repository. So coverage here comes from input
//! variety, not from shape variety — running the same payload against six shapes
//! asks one question six times.
//!
//! Every case below is therefore on [`Shape::Linear`], the cheapest shape the
//! harness builds, with exactly one exception: `check-mailmap` needs a
//! repository that *has* a `.mailmap`, so those cases are on
//! [`Shape::Attributes`], whose fixture carries all four mailmap line forms. No
//! other verb here reads a ref, an object or the index, and pinning them to the
//! floor shape is what keeps the module cheap enough to be dense.
//!
//! # How this divides territory with the seven adjacent modules
//!
//! The flag surfaces of these verbs are already well covered. What was not
//! covered is the **payload** surface — the hostile bytes a real caller feeds
//! them. Each neighbour and what it owns:
//!
//! * **`stdin_plumbing.rs`** — the nearest neighbour. Owns `stripspace`'s four
//!   modes over a messy/CRLF/whitespace-only/no-final-newline set, `column`'s
//!   three fill orders with `--padding`/`--indent`/`--nl`/`--width` and the
//!   `column.<verb>` keys, `check-ref-format`'s illegal-character classes as
//!   `strict` cases, `var`'s environment-beats-config precedence, plus
//!   `cherry`, `merge-index`, `pack-refs` and `fmt-merge-msg`. It does not
//!   configure a comment character for `stripspace`, does not feed `column`
//!   anything containing an escape sequence, and does not reach the
//!   `check-ref-format` classes below.
//! * **`mail_series.rs`** — owns the `format-patch → mailsplit → mailinfo →
//!   apply → am` pipeline on realistic messages: `mailinfo`'s eleven flags over
//!   a base64 body, a quoted-printable body, a latin-1 8-bit body, a scissors
//!   line, a `Message-Id`, a CRLF message and a `=?UTF-8?q?` **Q-encoded**
//!   subject; `mailsplit`'s `-b`/`-d`/`-f`/`--keep-cr` over a two-message mbox;
//!   and `interpret-trailers`' `--if-exists`/`--where`/`trailer.*` table over a
//!   subject+body+trailer-block payload. It never feeds a **B-encoded** header,
//!   a **folded** header, a message with **no `From:`**, an **in-body `From:`**,
//!   two **adjacent** encoded words, or a `Re: [PATCH v2 3/7]` subject.
//! * **`mail_patch.rs`** — owns `mailinfo`/`mailsplit`/`interpret-trailers` run
//!   over *tracked files* rather than stdin, plus `format-patch`, `am`,
//!   `imap-send`, `send-email`, `request-pull` and `quiltimport`. Its
//!   `interpret-trailers` inputs are a README and a one-line Rust file, so no
//!   trailer block exists in any of them.
//! * **`hooks_identity.rs`** — owns `commit`/`tag`/`rebase`/`push` under hooks
//!   and the `mailmap.file`/`mailmap.blob` **configuration keys** as read by
//!   `log`. It owns the mailmap *source*; this module owns the `--stdin`
//!   *payload*.
//! * **`informational.rs`** — owns `help`, `version`, `bugreport`, `diagnose`,
//!   `web--browse`, `hook run`, `for-each-repo`, `column`'s `--mode`/`--raw-mode`
//!   value sets and the `column.ui` precedence chain, `check-ref-format`'s
//!   `--normalize`/`--refspec-pattern`/`--branch` happy paths, `var`'s
//!   variable list, and `interpret-trailers`' `--no-*` resets and
//!   `trailer.<token>.command`.
//! * **`misc_commands.rs`** — owns the long tail of one-shot verbs; it reaches
//!   `mailinfo`, `mailsplit` and `interpret-trailers` once each, on their
//!   argument-parsing paths.
//! * **`config_reads.rs`** — owns `core.commentChar`/`core.commentString` as read
//!   by `status`/`log`/`diff`/`grep`. `commit_family.rs` owns them as read by
//!   `commit --cleanup`. Neither points them at `stripspace`, which is the verb
//!   whose *entire job* is the transformation those keys parameterise.
//!
//! # What this module adds
//!
//! | group | axis nothing above reaches |
//! |---|---|
//! | [`trailer_input_shapes`] | a payload with a real `---` divider; CRLF; empty; trailer-block-only; invalid UTF-8 in a trailer value |
//! | [`trailer_token_key_grammar`] | the three spellings of `trailer.<token>.key` — bare, `:`-terminated, `: `-terminated — as a matched set |
//! | [`trailer_placement_residue`] | `--where=after`, `--if-missing=add`, and command-line-beats-token-config precedence |
//! | [`stripspace_comment_config`] | `core.commentChar`/`core.commentString` pointed at `stripspace` itself, on both `-s` and `-c`, plus the two configuration refusals |
//! | [`stripspace_hostile_bytes`] | invalid UTF-8, interior tabs, mixed CRLF/LF, whitespace-only lines built from tabs |
//! | [`column_escape_sequences`] | ANSI colour in the payload, and a line wider than `--width` |
//! | [`ref_name_residue`] | DEL, TAB, LF, `.` and `..` alone, `--normalize` over an *invalid* name, and the `--branch` refusals |
//! | [`mailmap_stdin_payloads`] | CRLF, no trailing newline, a blank line, no angle brackets, an interior NUL, invalid UTF-8 |
//! | [`mailinfo_header_decoders`] | B-encoding, folding, adjacency, absent `From:`, in-body `From:`, `Re:` + `[PATCH v2 3/7]` |
//! | [`mailsplit_numbering`] | `-d5`/`-f9` widths, an unescaped `From ` line in the body beside a `>From` one |
//!
//! # Every payload is a byte literal, and every one was checked twice
//!
//! `Case::stdin` is `&'static [u8]`, which is what makes CRLF, an interior NUL,
//! a missing final newline and invalid UTF-8 expressible exactly. Each payload
//! below was run against stock 2.55.0 **twice** in a scratch repository and the
//! two outputs compared byte for byte before the case was written; nothing here
//! consults a clock, a random source or an absolute path.
//!
//! `var GIT_COMMITTER_IDENT` deserves a note because it looks nondeterministic
//! and is not: `env::harden` pins `GIT_COMMITTER_DATE`, so the timestamp it
//! embeds is `1700000000 +0000` on both sides and on both runs. Verified by
//! running it twice. It is left to `stdin_plumbing.rs` and `informational.rs`,
//! which already own `var`, rather than duplicated here.
//!
//! # One thing this harness cannot measure, stated rather than worked around
//!
//! `Case::args` is `Vec<String>`, and a Rust `String` cannot hold invalid UTF-8.
//! So **no case in this corpus can pass a non-UTF-8 argument**, and the verbs
//! here that take their input in argv rather than on stdin —
//! `check-ref-format`, `check-mailmap` without `--stdin` — have that dimension
//! permanently out of reach. It is not empty: measured by hand,
//! `check-ref-format $'refs/heads/a\x80b'` exits 0 under both stock 2.55.0 and
//! 2.50.1 and exits 101 under the port, panicking in `std::env::args()`. That is
//! reported as a finding, not smuggled in as a case that cannot be written.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    trailer_input_shapes(out);
    trailer_token_key_grammar(out);
    trailer_placement_residue(out);
    stripspace_comment_config(out);
    stripspace_hostile_bytes(out);
    column_escape_sequences(out);
    ref_name_residue(out);
    mailmap_stdin_payloads(out);
    mailinfo_header_decoders(out);
    mailsplit_numbering(out);
}

/// One stdin-fed case on the floor shape.
fn si(cmd: &'static str, args: &[&str], input: &'static [u8], out: &mut Vec<Case>) {
    out.push(Case::with_stdin(cmd, args, Shape::Linear, input));
}

// ---------------------------------------------------------------------------
// interpret-trailers
// ---------------------------------------------------------------------------

/// A message with a real `---` divider under its trailer block, and a diffstat
/// below it. This is the shape `format-patch` produces and `am` consumes.
const MSG_DIVIDER: &[u8] = b"divider subject\n\nBody paragraph.\n\n\
Signed-off-by: A U Thor <author@example.invalid>\n\
---\n a.txt | 1 +\n";

/// The same message with CRLF endings throughout. `trailer.c` finds the trailer
/// block by scanning backwards for a blank line, and a `\r\n` line is not blank
/// to a scanner that compares against `"\n"`.
const MSG_CRLF: &[u8] = b"crlf subject\r\n\r\nBody paragraph.\r\n\r\n\
Signed-off-by: A U Thor <author@example.invalid>\r\n";

/// Nothing at all. Adding a trailer to an empty message still has to produce a
/// well-formed one rather than a bare trailer with no preceding newline.
const MSG_EMPTY: &[u8] = b"";

/// A trailer block and nothing else — no subject, no body. `trailer.c` refuses
/// to treat the *first* paragraph as trailers, so this whole input is the
/// subject and the block it looks like is not one.
const MSG_ONLY_TRAILERS: &[u8] = b"Signed-off-by: A U Thor <author@example.invalid>\n\
Acked-by: R Viewer <reviewer@example.invalid>\n";

/// A trailer whose value carries a byte that is not valid UTF-8. The input is a
/// byte stream; a reader that decodes it as UTF-8 either replaces the byte or
/// refuses, and both are visible in the output.
const MSG_INVALID_UTF8: &[u8] = b"subject\n\nBody.\n\n\
Signed-off-by: Bad\x80Name <bad@example.invalid>\n";

/// A message whose last line has no terminating newline, ending mid-trailer.
const MSG_NO_EOL: &[u8] = b"subject\n\nBody.\n\nSigned-off-by: A <a@example.invalid>";

/// The ordinary payload: subject, body, one-line trailer block, LF endings.
const MSG_PLAIN: &[u8] = b"subject\n\nBody.\n\nSigned-off-by: A <a@example.invalid>\n";

/// `interpret-trailers`: the **input shapes** the flag-oriented groups elsewhere
/// never feed it.
///
/// `mail_series.rs` and `informational.rs` between them cover the option table
/// and the `trailer.*` configuration keys, but every one of their payloads is
/// well-formed LF text with a subject, a body and a trailer block. The decisions
/// `trailer.c` makes *before* any of those options apply are made by looking at
/// the input:
///
///   * `find_patch_start()` splits the message at a line that is exactly `---`,
///     and everything after it is untouchable. So a trailer added to
///     [`MSG_DIVIDER`] must land **above** the divider by default and **below**
///     it under `--no-divider` — measured under stock 2.55.0, the two outputs
///     differ in exactly where the four bytes `X: y\n` sit relative to `---\n`.
///     No other payload in the corpus contains a divider at all, so
///     `--no-divider` was previously a flag with nothing to toggle.
///   * The trailer block is found by scanning back for a blank line. Under CRLF
///     the "blank" line is `\r\n`, which is the single most likely place for a
///     port to split on the wrong constant.
///   * A first paragraph is never a trailer block ([`MSG_ONLY_TRAILERS`]), which
///     is the rule that stops `interpret-trailers` from mangling a one-line
///     commit message that happens to contain a colon.
fn trailer_input_shapes(out: &mut Vec<Case>) {
    let it = "interpret-trailers";

    // The divider, with and without the flag that ignores it. These two cases
    // are a matched pair: the same payload and the same trailer, and the only
    // thing that moves is which side of `---` it lands on.
    si(it, &[it, "--trailer", "X: y"], MSG_DIVIDER, out);
    si(it, &[it, "--no-divider", "--trailer", "X: y"], MSG_DIVIDER, out);
    // Reading the block back out of a message that has a divider: the block is
    // above it, so `--only-trailers` must not report the diffstat lines.
    si(it, &[it, "--only-trailers"], MSG_DIVIDER, out);
    si(it, &[it, "--parse"], MSG_DIVIDER, out);

    // CRLF, on the three paths that each have to recognise a blank line: adding
    // to an existing block, printing the block, and normalising it.
    si(it, &[it, "--trailer", "X: y"], MSG_CRLF, out);
    si(it, &[it, "--only-trailers"], MSG_CRLF, out);
    si(it, &[it, "--parse"], MSG_CRLF, out);
    si(it, &[it, "--only-trailers", "--unfold"], MSG_CRLF, out);

    // No input at all.
    si(it, &[it, "--trailer", "X: y"], MSG_EMPTY, out);
    si(it, &[it, "--only-trailers"], MSG_EMPTY, out);
    si(it, &[it], MSG_EMPTY, out);

    // A first paragraph that looks like a trailer block is not one.
    si(it, &[it, "--only-trailers"], MSG_ONLY_TRAILERS, out);
    si(it, &[it, "--parse"], MSG_ONLY_TRAILERS, out);
    si(it, &[it, "--trailer", "X: y"], MSG_ONLY_TRAILERS, out);

    // Bytes that are not UTF-8, on the reading path and the rewriting path.
    si(it, &[it, "--trailer", "X: y"], MSG_INVALID_UTF8, out);
    si(it, &[it, "--only-trailers"], MSG_INVALID_UTF8, out);
    si(it, &[it, "--parse"], MSG_INVALID_UTF8, out);

    // A truncated last line: the trailer is there but its newline is not, and
    // the output has to supply one.
    si(it, &[it, "--trailer", "X: y"], MSG_NO_EOL, out);
    si(it, &[it, "--only-trailers"], MSG_NO_EOL, out);
}

/// `trailer.<token>.key`: the three spellings of one configured key, as a
/// matched set.
///
/// A token's `key` is not just a name — git writes it out **verbatim** and then
/// appends the value, so whether the configured value ends in `Acked-by`,
/// `Acked-by:` or `Acked-by: ` decides the bytes between the key and the value
/// in the output. `trailer.c:parse_trailer()` strips a trailing separator and
/// the whitespace around it when it reads the configuration, and re-supplies a
/// single `: ` when it writes; a port that keeps the configured string as-is
/// produces `Acked-by:value` for one of the three spellings and the right thing
/// for the other two.
///
/// That is a **whitespace-only** difference in a message body, which is why the
/// three spellings are filed together instead of one being taken as
/// representative. Measured under stock 2.55.0, all three produce the identical
/// line `Acked-by: R V <r@example.invalid>`; a port that agrees on two of them
/// and not the third looks correct in any group that samples one.
///
/// The triple is not hypothetical. Measured by hand against the port, the bare
/// and `:`-terminated spellings agree with stock byte for byte and the
/// `: `-terminated one drops a single `0x20`:
///
/// ```text
/// stock  4163 6b65 642d 6279 3a20 5220 5620 3c72   Acked-by: R V <r
/// zvcs   4163 6b65 642d 6279 3a52 2056 203c 7240   Acked-by:R V <r@
/// ```
///
/// One byte, in a line that goes into a commit message. Isolating the axis is
/// what turns three pre-existing failures elsewhere in the corpus — each of
/// which happened to use the `: ` spelling and read as a separate `trailer.*`
/// defect — into one root cause with a named boundary.
///
/// `mail_series.rs` configures `trailer.ack.key=Acked-by: ` once, and
/// `informational.rs` configures `trailer.x.key=X: ` twice — all three use the
/// `: `-terminated spelling, so the axis itself was never isolated.
fn trailer_token_key_grammar(out: &mut Vec<Case>) {
    let it = "interpret-trailers";
    let add = &[it, "--trailer", "ack: R V <r@example.invalid>"];

    // The three spellings, same payload, same trailer, same everything else.
    for key in ["Acked-by", "Acked-by:", "Acked-by: "] {
        out.push(
            Case::with_stdin(it, add, Shape::Linear, MSG_PLAIN)
                .with_config(&[("trailer.ack.key", key)]),
        );
        // The same three through `--only-trailers`, where the key and value are
        // the entire output and nothing else can absorb a stray byte.
        out.push(
            Case::with_stdin(it, &[it, "--only-trailers", "--trailer", "ack: v"], Shape::Linear, MSG_PLAIN)
                .with_config(&[("trailer.ack.key", key)]),
        );
    }

    // Two values for one token: the separator has to be re-supplied for each,
    // so a spacing bug prints twice and a placement bug prints once.
    out.push(
        Case::with_stdin(it, &[it, "--trailer", "ack: one", "--trailer", "ack: two"], Shape::Linear, MSG_PLAIN)
            .with_config(&[("trailer.ack.key", "Acked-by: ")]),
    );

    // A configured key under a *non-default* separator set. `trailer.separators`
    // decides what splits `--trailer eq=v`, and the key's own trailing `= ` is
    // then the thing written out.
    out.push(
        Case::with_stdin(it, &[it, "--trailer", "eq=v"], Shape::Linear, MSG_PLAIN)
            .with_config(&[("trailer.separators", ":="), ("trailer.eq.key", "Eq= ")]),
    );

    // A configured key that *replaces* an existing trailer rather than adding
    // one: the replacement is written through the same key-plus-separator path,
    // so this is the spacing question asked on the `ifExists` branch.
    out.push(
        Case::with_stdin(it, &[it, "--trailer", "sob: New <n@example.invalid>"], Shape::Linear, MSG_PLAIN)
            .with_config(&[("trailer.sob.key", "Signed-off-by: "), ("trailer.sob.ifexists", "replace")]),
    );

    // Exactly one `trailer.ack.key` is configured, and stock 2.55.0 prints
    // nothing at all on stderr for it. A diagnostic emitted where stock emits
    // none is a presence difference rather than a wording one — the harness's
    // standing exemption covers prose that differs, not prose that exists on
    // only one side — so this one case pins stderr.
    out.push(Case {
        compare_stderr: true,
        ..Case::with_stdin(it, add, Shape::Linear, MSG_PLAIN)
            .with_config(&[("trailer.ack.key", "Acked-by:")])
    });
}

/// `interpret-trailers`: the `--where` / `--if-missing` values and the
/// precedence rule the adjacent groups leave out.
///
/// Three axes, three levels each — command line, `trailer.<token>.<axis>`,
/// `trailer.<axis>` — and the command line has to beat the token key which has
/// to beat the global one. `mail_series.rs` sets each level in isolation and
/// `informational.rs` covers the `--no-*` resets; neither puts two levels in
/// conflict, so a port that reads the levels in the wrong order passes both.
///
/// The individual values left unreached are `--where=after` against a message
/// that *has* a block for it to be after (`mail_patch.rs` uses it against a
/// README, where there is no block and every `--where` value collapses to the
/// same answer) and `--if-missing=add`, the default spelled explicitly.
fn trailer_placement_residue(out: &mut Vec<Case>) {
    let it = "interpret-trailers";

    // `--where=after` and `--where=before` against a real two-line block: the
    // added trailer lands on opposite sides of the existing one.
    si(it, &[it, "--where=after", "--trailer", "X: y"], MSG_PLAIN, out);
    // The default `--if-missing` value, spelled out, against a message that has
    // no such trailer.
    si(it, &[it, "--if-missing=add", "--trailer", "X: y"], MSG_PLAIN, out);
    // `--if-exists=add` forced against a token that is already present, so the
    // output carries the token twice.
    si(it, &[it, "--if-exists=add", "--trailer", "Signed-off-by: A <a@example.invalid>"], MSG_PLAIN, out);

    // Precedence, one pair per axis. In each pair the two levels disagree and
    // the command line has to win.
    let cfg = |pairs: &[(&str, &str)], args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::with_stdin(it, args, Shape::Linear, MSG_PLAIN).with_config(pairs));
    };
    cfg(&[("trailer.where", "start")], &[it, "--where=end", "--trailer", "X: y"], out);
    cfg(
        &[("trailer.ack.key", "Acked-by:"), ("trailer.ack.where", "start")],
        &[it, "--where=end", "--trailer", "ack: v"],
        out,
    );
    // The token key beats the global key: `trailer.where=start` says start,
    // `trailer.ack.where=end` says end, and the token's own answer stands.
    cfg(
        &[("trailer.where", "start"), ("trailer.ack.key", "Acked-by:"), ("trailer.ack.where", "end")],
        &[it, "--trailer", "ack: v"],
        out,
    );
    cfg(
        &[("trailer.ifexists", "doNothing")],
        &[it, "--if-exists=replace", "--trailer", "Signed-off-by: New <n@example.invalid>"],
        out,
    );
    cfg(
        &[("trailer.ifmissing", "doNothing")],
        &[it, "--if-missing=add", "--trailer", "X: y"],
        out,
    );
    // `--trim-empty` against a token whose value is empty *and* one whose value
    // is not, in one invocation: only the empty one is dropped.
    si(it, &[it, "--trim-empty", "--trailer", "Empty:", "--trailer", "Full: v"], MSG_PLAIN, out);
}

// ---------------------------------------------------------------------------
// stripspace
// ---------------------------------------------------------------------------

/// Plain text carrying comment lines in two different syntaxes at once, so
/// whichever of the two is configured is the one that disappears.
const SS_TWO_SYNTAXES: &[u8] = b"plain\n// slash comment\n# hash comment\nmore\n";

/// Trailing tabs, trailing spaces, a leading tab, and a three-line blank run.
const SS_WHITESPACE: &[u8] = b"a\t\nb   \n\tindented\n\n\n\nc\n";

/// CRLF throughout, including the blank lines.
const SS_CRLF: &[u8] = b"a\r\nb\r\n\r\n\r\nc\r\n";

/// Nothing but comment lines: the whole input disappears under `-s`.
const SS_ALL_COMMENTS: &[u8] = b"# one\n# two\n";

/// Lines that are whitespace and nothing else, built from spaces and tabs —
/// blank to `strbuf_rtrim()`, non-empty to a length check.
const SS_BLANK_LINES: &[u8] = b"   \n\t\n  \t  \n";

/// A byte that is not valid UTF-8, both inside a line and as trailing content.
const SS_INVALID_UTF8: &[u8] = b"a\x80b\ntrailing \x80 \n";

/// Mixed line endings in one stream: an LF line, a CRLF line, a lone CR.
const SS_MIXED_EOL: &[u8] = b"lf line\ncrlf line\r\nlone cr\rtail\n";

/// `stripspace` under a configured comment character.
///
/// `core.commentChar` and `core.commentString` are read by
/// `config.c:git_default_core_config()` and consumed by
/// `strbuf.c:strbuf_stripspace()`, which is `stripspace`'s entire body. The keys
/// are already measured through `status`/`log`/`diff`/`grep` (`config_reads.rs`)
/// and through `commit --cleanup` (`commit_family.rs`) — but those are *readers*
/// that happen to strip comments, and this is the verb whose only job is the
/// strip. Pointing the keys at it is the shortest path from configuration to
/// observable bytes, and it is a path nothing in the corpus took.
///
/// Both directions matter and they are different code:
///
///   * `-s`/`--strip-comments` **removes** lines that start with the character,
///     so the payload carries `//` and `#` lines together and exactly one of
///     them survives each configuration.
///   * `-c`/`--comment-lines` **prepends** it, so the multi-byte
///     `core.commentString` is written out in full — a port storing a `char`
///     rather than a string truncates here and nowhere else.
///
/// The two refusals are configuration errors, not input errors, and stock
/// 2.55.0 exits 128 on both: an empty `core.commentChar`, and a
/// `core.commentString` containing a newline (which would make the comment
/// marker unable to mark anything). They are `strict` because a `fatal:` naming
/// the offending key is what tells the user which line of their config is wrong.
fn stripspace_comment_config(out: &mut Vec<Case>) {
    let sp = "stripspace";

    // `-s` under each of the two keys: the payload has both syntaxes, so the
    // answer names which key was honoured.
    out.push(
        Case::with_stdin(sp, &[sp, "-s"], Shape::Linear, SS_TWO_SYNTAXES)
            .with_config(&[("core.commentString", "//")]),
    );
    out.push(
        Case::with_stdin(sp, &[sp, "-s"], Shape::Linear, SS_TWO_SYNTAXES)
            .with_config(&[("core.commentChar", ";")]),
    );
    // A single-character `core.commentString` is the same thing spelled the
    // other way, and has to behave identically to `core.commentChar`.
    out.push(
        Case::with_stdin(sp, &[sp, "-s"], Shape::Linear, SS_TWO_SYNTAXES)
            .with_config(&[("core.commentString", "#")]),
    );
    // A comment marker with a trailing space: `# ` matches `# hash comment` and
    // does not match a bare `#`.
    out.push(
        Case::with_stdin(sp, &[sp, "-s"], Shape::Linear, SS_TWO_SYNTAXES)
            .with_config(&[("core.commentString", "# ")]),
    );

    // `-c` under a multi-byte marker: every produced line has to carry all of it.
    out.push(
        Case::with_stdin(sp, &[sp, "-c"], Shape::Linear, SS_WHITESPACE)
            .with_config(&[("core.commentString", "//")]),
    );
    out.push(
        Case::with_stdin(sp, &[sp, "-c"], Shape::Linear, SS_CRLF)
            .with_config(&[("core.commentString", "//")]),
    );
    out.push(
        Case::with_stdin(sp, &[sp, "-c"], Shape::Linear, SS_WHITESPACE)
            .with_config(&[("core.commentChar", ";")]),
    );
    // `-c` over a blank-only input under a configured marker: the blank lines
    // are commented with the marker alone and no trailing space.
    out.push(
        Case::with_stdin(sp, &[sp, "-c"], Shape::Linear, SS_BLANK_LINES)
            .with_config(&[("core.commentString", "//")]),
    );

    // An input that is entirely comments, under the marker that matches and
    // under one that does not.
    out.push(
        Case::with_stdin(sp, &[sp, "-s"], Shape::Linear, SS_ALL_COMMENTS)
            .with_config(&[("core.commentChar", "#")]),
    );
    out.push(
        Case::with_stdin(sp, &[sp, "-s"], Shape::Linear, SS_ALL_COMMENTS)
            .with_config(&[("core.commentChar", ";")]),
    );

    // The two configuration refusals.
    out.push(Case {
        compare_stderr: true,
        ..Case::with_stdin(sp, &[sp, "-s"], Shape::Linear, SS_TWO_SYNTAXES)
            .with_config(&[("core.commentChar", "")])
    });
    out.push(Case {
        compare_stderr: true,
        ..Case::with_stdin(sp, &[sp, "-s"], Shape::Linear, SS_TWO_SYNTAXES)
            .with_config(&[("core.commentString", "a\nb")])
    });
}

/// `stripspace` on bytes a text filter is not supposed to assume anything about.
///
/// The default mode does three things in one pass — trim trailing whitespace,
/// collapse blank-line runs, drop leading and trailing blanks — and each has a
/// different wrong answer available on hostile input. `stdin_plumbing.rs` covers
/// a messy payload, a CRLF payload, a whitespace-only payload, an unterminated
/// last line, an empty stream and a NUL. What is left is the set where the
/// *definition* of whitespace is what is under test:
///
///   * a **tab** is trailing whitespace to `strbuf_rtrim()`, so `a\t` becomes
///     `a`; a filter that trims only `' '` leaves the tab.
///   * a line of tabs and spaces **is** a blank line, so a run of them collapses.
///   * a lone `\r` in the middle of a line is not a line ending, so `lone cr\rtail`
///     stays one line — a filter that splits on `\r` produces two.
///   * a byte that is not valid UTF-8 must survive the round trip unchanged.
fn stripspace_hostile_bytes(out: &mut Vec<Case>) {
    let sp = "stripspace";

    si(sp, &[sp], SS_WHITESPACE, out);
    si(sp, &[sp, "-s"], SS_WHITESPACE, out);
    si(sp, &[sp], SS_BLANK_LINES, out);
    si(sp, &[sp, "-c"], SS_BLANK_LINES, out);
    si(sp, &[sp], SS_INVALID_UTF8, out);
    si(sp, &[sp, "-c"], SS_INVALID_UTF8, out);
    si(sp, &[sp, "-s"], SS_INVALID_UTF8, out);
    si(sp, &[sp], SS_MIXED_EOL, out);
    si(sp, &[sp, "-c"], SS_MIXED_EOL, out);
    si(sp, &[sp, "-c"], SS_CRLF, out);
    si(sp, &[sp, "-s"], SS_ALL_COMMENTS, out);

    // A line of exactly one very long run of spaces: trailing-whitespace
    // trimming has to reduce it to nothing rather than to one space.
    si(sp, &[sp], b"x                                                            \ny\n", out);
    // Interior whitespace is never touched, only trailing: the two runs in this
    // payload have to be treated differently.
    si(sp, &[sp], b"a   b   \n", out);
}

// ---------------------------------------------------------------------------
// column
// ---------------------------------------------------------------------------

/// Lines wrapped in SGR escape sequences. Two are coloured, one is plain, one
/// carries a two-parameter sequence and the full `\x1b[0m` reset.
const COL_ANSI: &[u8] = b"\x1b[31mred\x1b[m\n\x1b[32mgreen\x1b[m\nplain\n\x1b[1;34mbluebold\x1b[0m\n";

/// One line far wider than any sensible `--width`, followed by two short ones.
const COL_OVERLONG: &[u8] =
    b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nbb\ncc\n";

/// A payload with a byte that is not valid UTF-8, so the display-width
/// computation has to work on bytes it cannot decode.
const COL_INVALID_UTF8: &[u8] = b"a\x80b\nshort\nlonger-entry\n";

/// Two lines only — the degenerate grid, where `--nl` and `--padding` are the
/// only things that can differ.
const COL_TWO: &[u8] = b"a\nb\n";

/// `column`: escape sequences and entries wider than the layout.
///
/// `column.c:item_length()` calls `utf8_strnwidth(..., 1)`, whose third argument
/// tells it to **skip ANSI escape sequences** when measuring. That is the whole
/// reason `git branch --column` can colour its output and still line the columns
/// up: the escape bytes occupy no display width. A port that measures
/// `strlen()`, or that measures decoded characters without the skip, produces a
/// grid whose columns are as wrong as the escapes are long — and produces it
/// silently, because the output still *looks* like columns.
///
/// Nothing in `stdin_plumbing.rs`'s `column` group or `informational.rs`'s
/// `columnation` group feeds an escape sequence: their payloads are sixteen
/// short plain words, a CRLF variant and a NUL variant. `log_format.rs` is the
/// only module in the corpus whose payloads contain `\x1b`, and it is testing
/// `log --color`, not layout.
///
/// The other half is what happens when one entry cannot fit. `column.c` clamps
/// the column count to at least one, so an entry wider than `--width` is printed
/// on its own overlong row rather than truncated — and the *remaining* entries
/// still have to be laid out against the original width, not against the
/// overlong one.
///
/// `--width` is pinned on every case: left unset, `column` asks the terminal,
/// and although both sides run without one, pinning removes the question.
fn column_escape_sequences(out: &mut Vec<Case>) {
    let col = "column";

    // ANSI-coloured entries in each of the three fill orders. The escapes are
    // eight bytes and zero display columns, so the widest *visible* entry is
    // `bluebold` at eight; a byte-length measurement thinks it is nineteen.
    si(col, &[col, "--mode=column", "--width=40"], COL_ANSI, out);
    si(col, &[col, "--mode=row", "--width=40"], COL_ANSI, out);
    si(col, &[col, "--mode=dense", "--width=40"], COL_ANSI, out);
    // A narrow width, where a wrong measurement changes the column *count* and
    // not merely the padding.
    si(col, &[col, "--mode=column", "--width=20"], COL_ANSI, out);
    si(col, &[col, "--mode=row", "--width=20"], COL_ANSI, out);
    // With padding and an indent on top, so the escapes have to be excluded from
    // a width that other things are also being subtracted from.
    si(col, &[col, "--mode=column", "--width=40", "--padding=4"], COL_ANSI, out);
    si(col, &[col, "--mode=column", "--width=40", "--indent=>> "], COL_ANSI, out);

    // An entry wider than the whole layout.
    si(col, &[col, "--mode=column", "--width=20"], COL_OVERLONG, out);
    si(col, &[col, "--mode=row", "--width=20"], COL_OVERLONG, out);
    si(col, &[col, "--mode=dense", "--width=20"], COL_OVERLONG, out);
    // The minimum width: every entry ends up on its own row, which is the clamp
    // to one column rather than a division by zero.
    si(col, &[col, "--mode=column", "--width=1"], COL_OVERLONG, out);

    // Bytes that do not decode: the width function has to fall back rather than
    // refuse.
    si(col, &[col, "--mode=column", "--width=40"], COL_INVALID_UTF8, out);
    si(col, &[col, "--mode=dense", "--width=40"], COL_INVALID_UTF8, out);

    // A multi-character `--nl`, which is appended after every row and is the one
    // string `column` writes that is not derived from the input.
    si(col, &[col, "--mode=column", "--width=40", "--nl=<END>"], COL_TWO, out);
    si(col, &[col, "--mode=row", "--width=40", "--nl=<END>", "--indent=| "], COL_TWO, out);
}

// ---------------------------------------------------------------------------
// check-ref-format
// ---------------------------------------------------------------------------

/// `check-ref-format`: the residual grammar and the flag combinations.
///
/// `refs.c:check_refname_format()` is a list of rules and every one of them is
/// worth its own case, because a port that implements nine of ten scores 90% on
/// a corpus that tests one name. `plumbing_refs.rs`, `informational.rs` and
/// `stdin_plumbing.rs` between them own most of the list. What is left:
///
///   * **The control-byte rule is a range, not a set.** `\x01` is already
///     covered; `\x7f` (DEL) and `\t` are the two other members with distinct
///     origins, and `\n` is the one a shell pipeline can actually produce.
///     Measured under stock 2.55.0, all four exit 1.
///   * **A name that is nothing but dots.** `.` fails the leading-dot rule and
///     `..` fails both that and the `..` rule, and both are what a script
///     produces when a variable it interpolated was empty.
///   * **`--normalize` over an invalid name.** Normalising is not validating:
///     git collapses the slashes and *then* checks, so `refs/heads/a~b` is still
///     a rejection with no output and `refs/heads/x/` — a trailing slash, which
///     normalisation does not remove — is one too. That is the pair that
///     separates "normalise then validate" from "validate then normalise".
///   * **`--branch`'s refusals are `die()`, not exit 1.** It runs
///     `interpret_branch_name()` first, so `x@{u}` and `-x` exit **128** with a
///     `fatal:`, while an invalid *plain* name exits 1. Two exit codes from one
///     flag is exactly the sort of thing a port collapses to one.
///
/// `--branch x@{u}` is the one case here where `strict` is buying something
/// other than a free assertion, and it is deliberate. Both sides exit 128, so
/// the exit code cannot separate them, and the two stderr lines report
/// *different findings* rather than the same finding in different words:
///
/// ```text
/// stock  fatal: no such branch: 'x'
/// zvcs   fatal: 'x@{u}' is not a valid branch name
/// ```
///
/// Stock expanded `@{u}` and failed to resolve the branch `x`; the port never
/// attempted the expansion and rejected the literal string. The harness's
/// standing exemption covers prose that differs about one fact, not two answers
/// that disagree about which fact is being reported — a caller that parses the
/// message reaches the opposite conclusion about whether the name was
/// well-formed. The other four `--branch` refusals agree byte for byte.
///   * **Two operands.** The command takes exactly one refname (or one branch);
///     a second is a usage error, exit 129.
///
/// Every case is `strict`: this command writes nothing on stdout unless
/// `--normalize` was asked for, so the exit code is the whole answer and pinning
/// stderr as well costs nothing on the cases that print nothing.
fn ref_name_residue(out: &mut Vec<Case>) {
    let crf = "check-ref-format";
    let s = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::strict(crf, args, Shape::Linear));
    };

    // The rest of the control-byte range, plus the whitespace members.
    s(&[crf, "refs/heads/a\u{7f}b"], out);
    s(&[crf, "refs/heads/a\tb"], out);
    s(&[crf, "refs/heads/a\nb"], out);
    s(&[crf, "refs/heads/ "], out);
    // Names made only of dots.
    s(&[crf, "."], out);
    s(&[crf, ".."], out);
    s(&[crf, "refs/heads/."], out);
    // A high code point is *legal* — the rule is a control-byte rule, not an
    // ASCII rule, so this is the case that stops an over-strict port passing.
    s(&[crf, "refs/heads/caf\u{e9}"], out);

    // `--normalize` over names that do not survive validation. No output, exit 1.
    s(&[crf, "--normalize", "refs/heads/a~b"], out);
    s(&[crf, "--normalize", "refs/heads/x/"], out);
    s(&[crf, "--normalize", "refs/heads/x.lock"], out);
    // `--normalize` combined with the two flags that widen what is legal: a bare
    // name needs `--allow-onelevel`, and a pattern needs `--refspec-pattern`.
    s(&[crf, "--normalize", "--allow-onelevel", "///x"], out);
    s(&[crf, "--normalize", "--refspec-pattern", "//refs/heads/*"], out);

    // `--refspec-pattern`: one star per component and one component per name.
    s(&[crf, "--refspec-pattern", "refs/heads/*/*"], out);
    s(&[crf, "--refspec-pattern", "refs/heads/a*b"], out);

    // `--branch`: the two exit codes. A name `interpret_branch_name()` chokes on
    // is a `fatal:` at 128; an ordinary invalid name is a plain 1.
    s(&[crf, "--branch", "x@{u}"], out);
    s(&[crf, "--branch", "-x"], out);
    s(&[crf, "--branch", ""], out);
    s(&[crf, "--branch", "a/b"], out);
    s(&[crf, "--branch", "a..b"], out);

    // Two operands is a usage error, not two validations.
    out.push(Case::new(crf, &[crf, "refs/heads/a", "refs/heads/b"], Shape::Linear));
    // `--allow-onelevel` widens the level rule and nothing else: the `.lock`
    // rule still applies to the one component it now permits.
    s(&[crf, "--allow-onelevel", "name.lock"], out);
}

// ---------------------------------------------------------------------------
// check-mailmap
// ---------------------------------------------------------------------------

/// Two idents the fixture's `.mailmap` rewrites, with CRLF endings.
const MM_CRLF: &[u8] =
    b"Old Name <old@example.invalid>\r\nAlias Name <alias@example.invalid>\r\n";

/// An empty line between two idents. An empty ident is not a lookup failure; it
/// is an input line that has to produce an output line.
const MM_BLANK_LINE: &[u8] =
    b"Old Name <old@example.invalid>\n\nTypo Name <typo@example.invalid>\n";

/// One ident, no terminating newline.
const MM_NO_EOL: &[u8] = b"Old Name <old@example.invalid>";

/// A contact with no angle brackets, followed by one that has them. The first is
/// not an ident at all and is wrapped rather than looked up.
const MM_NO_BRACKETS: &[u8] = b"no-angle-brackets\nSolo Name <solo@example.invalid>\n";

/// An interior NUL. The lines are newline-separated, so the NUL is content
/// inside a name and must not terminate anything.
const MM_NUL: &[u8] = b"Old Name <old@example.invalid>\0Alias Name <alias@example.invalid>\n";

/// A name carrying a byte that is not valid UTF-8, and an email whose case
/// differs from the mailmap's — the email half of a lookup is
/// case-insensitive, the name half is not.
const MM_INVALID_UTF8: &[u8] = b"Bad\x80 <old@example.invalid>\n<OLD@EXAMPLE.INVALID>\n";

/// Surrounding whitespace on an otherwise ordinary ident.
const MM_PADDED: &[u8] = b"   Old Name <old@example.invalid>   \n";

/// `check-mailmap --stdin`: the payload shapes.
///
/// The lookup *rules* are owned elsewhere and are not repeated here:
/// `informational.rs:mailmap_lookup` covers case-insensitive email matching, the
/// name-half rule, name-only entries and the two command-line mailmap sources;
/// `hooks_identity.rs` covers `mailmap.file`/`mailmap.blob`; `info_attrs.rs`
/// covers a `mailmap.file` naming something absent or something that is not a
/// mailmap; `shape_reach.rs` covers the direct one-ident lookups.
///
/// What none of them do is give `--stdin` anything but well-formed LF lines.
/// `builtin/check-mailmap.c` reads with `strbuf_getline_lf()` and then hands the
/// line to `split_ident_line()`, and the boundary between the two is where the
/// input shape decides the answer: a `\r` left on the end of a line becomes part
/// of the email and the lookup misses; a final line with no newline is still a
/// line; a blank line still produces an output line; a line with no `<` is not
/// an ident and is wrapped as `<line>` rather than parsed.
///
/// These are the only cases in the module on [`Shape::Attributes`], because they
/// are the only ones that need a repository to contain anything — its `.mailmap`
/// carries all four line forms and the identities below are drawn from it.
fn mailmap_stdin_payloads(out: &mut Vec<Case>) {
    let cm = "check-mailmap";
    let m = |input: &'static [u8], out: &mut Vec<Case>| {
        out.push(Case::with_stdin(cm, &[cm, "--stdin"], Shape::Attributes, input));
    };

    m(MM_CRLF, out);
    m(MM_BLANK_LINE, out);
    m(MM_NO_EOL, out);
    m(MM_NO_BRACKETS, out);
    m(MM_NUL, out);
    m(MM_INVALID_UTF8, out);
    m(MM_PADDED, out);
    // Nothing on stdin: zero lines in, zero lines out, exit 0 — not an error.
    m(b"", out);

    // `--stdin` alongside an operand. The operands are answered first and the
    // stdin lines after, so a reader that drains stdin before parsing argv
    // reverses the output — asked here with a payload whose *shape* is also
    // unusual, which is the combination neither group covers.
    out.push(Case::with_stdin(
        cm,
        &[cm, "--stdin", "Solo Name <solo@example.invalid>"],
        Shape::Attributes,
        MM_CRLF,
    ));
    out.push(Case::with_stdin(
        cm,
        &[cm, "--stdin", "Old Name <old@example.invalid>"],
        Shape::Attributes,
        MM_NO_EOL,
    ));
}

// ---------------------------------------------------------------------------
// mailinfo
// ---------------------------------------------------------------------------

/// A **B**-encoded (base64) RFC 2047 subject: `=?UTF-8?B?…?=` decoding to
/// `café subject`. `mail_series.rs` covers the **Q** form only, and the two are
/// separate decoders in `mailinfo.c:decode_header()`.
const MI_B_SUBJECT: &[u8] = b"From: A U Thor <a@example.invalid>\n\
Subject: =?UTF-8?B?Y2Fmw6kgc3ViamVjdA==?=\n\
Date: Mon, 1 Jan 2001 00:00:00 +0000\n\n\
body line\n";

/// A subject folded across a continuation line per RFC 822: the second physical
/// line begins with whitespace and belongs to the same header. `mailinfo.c`
/// joins them before it strips `[PATCH]`, so a reader that treats each physical
/// line as a header keeps the bracket prefix *and* loses the tail.
const MI_FOLDED_SUBJECT: &[u8] = b"From: A U Thor <a@example.invalid>\n\
Subject: [PATCH] a very long subject that is\n folded onto a continuation line\n\
Date: Mon, 1 Jan 2001 00:00:00 +0000\n\n\
body line\n";

/// No `From:` header at all. `mailinfo` writes no `Author:`/`Email:` lines and
/// still succeeds — this is the mail an out-of-band patch arrives as.
const MI_NO_FROM: &[u8] = b"Subject: [PATCH] no from header\n\
Date: Mon, 1 Jan 2001 00:00:00 +0000\n\n\
body line\n";

/// An in-body `From:` on the first line of the body, disagreeing with the
/// envelope one. The rule is that the in-body line wins when it is the first
/// thing in the body and is followed by a blank line.
const MI_IN_BODY_FROM: &[u8] = b"From: Envelope <env@example.invalid>\n\
Subject: [PATCH] inbody from\n\
Date: Mon, 1 Jan 2001 00:00:00 +0000\n\n\
From: In Body <body@example.invalid>\n\n\
real body\n";

/// Two encoded words separated by whitespace. RFC 2047 says the whitespace
/// *between* adjacent encoded words is not part of either and is dropped, so
/// `=?UTF-8?Q?one?= =?UTF-8?Q?two?=` decodes to `onetwo`, not `one two`. A
/// decoder that handles one encoded word per header gets this wrong by one byte.
const MI_ADJACENT_WORDS: &[u8] = b"From: A <a@example.invalid>\n\
Subject: =?UTF-8?Q?one?= =?UTF-8?Q?two?=\n\
Date: Mon, 1 Jan 2001 00:00:00 +0000\n\n\
body\n";

/// A `Re:` prefix in front of a bracket prefix that carries a version and a
/// numbering: both the `Re:` and the whole `[...]` have to go, and `-k` has to
/// keep both.
const MI_RE_PREFIX: &[u8] = b"From: A <a@example.invalid>\n\
Subject: Re: [PATCH v2 3/7] real subject\n\
Date: Mon, 1 Jan 2001 00:00:00 +0000\n\n\
body\n";

/// A latin-1 **B**-encoded display name in `From:` alongside a **Q**-encoded
/// subject: two charsets and two encodings in one message, so the transcoding
/// to the output charset happens twice by different routes.
const MI_LATIN1_MIXED: &[u8] = b"From: =?ISO-8859-1?B?QSBUaOly?= <a@example.invalid>\n\
Subject: =?ISO-8859-1?Q?caf=E9?= and more\n\
Date: Mon, 1 Jan 2001 00:00:00 +0000\n\n\
body\n";

/// `mailinfo`: the header decoders, which are the half of the parser that runs
/// before any flag applies.
///
/// `mail_series.rs:mailinfo_forms` owns the flag matrix — `-k`, `-b`, `-u`,
/// `-n`, `--encoding=`, `--scissors`, `--quoted-cr=`, `-m` — over a message set
/// whose *headers* are all simple: one address, one unencoded or Q-encoded
/// subject, one line each. Everything below is a header shape that set does not
/// contain, and each one is a separate branch of `mailinfo.c`:
///
/// | payload | branch |
/// |---|---|
/// | [`MI_B_SUBJECT`] | base64 encoded-word decoding |
/// | [`MI_FOLDED_SUBJECT`] | RFC 822 continuation-line unfolding |
/// | [`MI_ADJACENT_WORDS`] | inter-word whitespace elision |
/// | [`MI_LATIN1_MIXED`] | two charsets in one message |
/// | [`MI_RE_PREFIX`] | `Re:` stripping ahead of bracket stripping |
/// | [`MI_NO_FROM`] | the absent-header path |
/// | [`MI_IN_BODY_FROM`] | in-body `From:` overriding the envelope |
///
/// The command writes two files and prints its `Author:`/`Email:`/`Subject:`/
/// `Date:` summary on stdout, so both halves are compared: the summary as stdout
/// and the two files through the post-command state digest. `msg.txt` and
/// `patch.txt` are the names every existing `mailinfo` case uses and neither
/// exists in [`Shape::Linear`], so nothing is overwritten.
fn mailinfo_header_decoders(out: &mut Vec<Case>) {
    let mi = "mailinfo";
    let files: &[&str] = &[mi, "msg.txt", "patch.txt"];

    si(mi, files, MI_B_SUBJECT, out);
    // `-u` (the default) and `-n` differ only in whether the decoded header is
    // re-encoded to UTF-8, so a base64 payload has to be asked both ways.
    si(mi, &[mi, "-u", "msg.txt", "patch.txt"], MI_B_SUBJECT, out);
    si(mi, &[mi, "-n", "msg.txt", "patch.txt"], MI_B_SUBJECT, out);
    si(mi, &[mi, "-k", "msg.txt", "patch.txt"], MI_B_SUBJECT, out);

    // Folding, with and without the flag that keeps the subject verbatim: `-k`
    // still has to join the two physical lines even though it strips nothing.
    si(mi, files, MI_FOLDED_SUBJECT, out);
    si(mi, &[mi, "-k", "msg.txt", "patch.txt"], MI_FOLDED_SUBJECT, out);
    si(mi, &[mi, "-b", "msg.txt", "patch.txt"], MI_FOLDED_SUBJECT, out);

    // Adjacent encoded words: the byte between them is the whole assertion.
    si(mi, files, MI_ADJACENT_WORDS, out);
    si(mi, &[mi, "-n", "msg.txt", "patch.txt"], MI_ADJACENT_WORDS, out);

    // Two charsets, transcoded and not.
    si(mi, files, MI_LATIN1_MIXED, out);
    si(mi, &[mi, "-n", "msg.txt", "patch.txt"], MI_LATIN1_MIXED, out);
    si(mi, &[mi, "--encoding=latin1", "msg.txt", "patch.txt"], MI_LATIN1_MIXED, out);

    // `Re:` in front of `[PATCH v2 3/7]`. `-k` keeps everything, `-b` keeps only
    // the bracket contents that are not `PATCH`, the default keeps neither.
    si(mi, files, MI_RE_PREFIX, out);
    si(mi, &[mi, "-k", "msg.txt", "patch.txt"], MI_RE_PREFIX, out);
    si(mi, &[mi, "-b", "msg.txt", "patch.txt"], MI_RE_PREFIX, out);

    // A message with no `From:`, and one whose body opens with one.
    si(mi, files, MI_NO_FROM, out);
    si(mi, &[mi, "-b", "msg.txt", "patch.txt"], MI_NO_FROM, out);
    si(mi, files, MI_IN_BODY_FROM, out);
    si(mi, &[mi, "-b", "msg.txt", "patch.txt"], MI_IN_BODY_FROM, out);
    si(mi, &[mi, "--scissors", "msg.txt", "patch.txt"], MI_IN_BODY_FROM, out);
}

// ---------------------------------------------------------------------------
// mailsplit
// ---------------------------------------------------------------------------

/// A two-message mbox whose first body contains a `>From `-escaped line — the
/// mboxo escaping `mailsplit` has to recognise as body content rather than as a
/// message boundary.
const MS_TWO: &[u8] = b"From nobody Mon Sep 17 00:00:00 2001\n\
From: A <a@example.invalid>\nSubject: one\n\n\
body one\n>From escaped\n\n\
From nobody Mon Sep 17 00:00:00 2001\n\
From: B <b@example.invalid>\nSubject: two\n\n\
body two\n";

/// One message whose body carries an **unescaped** `From ` line that is not a
/// valid mbox separator (no date). Splitting here would produce two messages
/// from one, which is the failure mode that silently drops half a patch.
const MS_FROM_IN_BODY: &[u8] = b"From nobody Mon Sep 17 00:00:00 2001\n\
From: A <a@example.invalid>\nSubject: one\n\n\
body\nFrom bare line in body\nmore\n";

/// A single message with CRLF endings throughout, including the separator line.
const MS_CRLF: &[u8] = b"From nobody Mon Sep 17 00:00:00 2001\r\n\
From: A <a@example.invalid>\r\nSubject: crlf\r\n\r\n\
body\r\n";

/// `mailsplit`: the output-name arithmetic, and where one message ends.
///
/// Two things are observable and both are state rather than stdout — the command
/// prints only the number of messages it wrote, and the *files* are the answer.
///
///   * `-d<n>` sets the digit width and `-f<n>` the starting number, and the two
///     compose: `-d5 -f9` writes `00009`. `mail_series.rs` covers `-d3 -f5` and
///     `-d4`; the widths below are the ones where a `%0*d` with the wrong
///     argument order or an off-by-one in the count is visible.
///   * The boundary rule is `From ` at the start of a line **followed by
///     something that parses as a date**. `mail_series.rs` covers the `>From`
///     mboxrd escaping; [`MS_FROM_IN_BODY`] covers the other half — a bare
///     `From ` line that is *not* a separator — and a splitter that keys on the
///     five bytes alone turns one message into two.
///
/// `-o.` writes into the fixture root, which is where every existing
/// `mailsplit` case writes, so the numbered files land beside `README.md` and
/// the state digest carries them.
fn mailsplit_numbering(out: &mut Vec<Case>) {
    let ms = "mailsplit";

    // Digit width and start number, separately and together.
    si(ms, &[ms, "-o.", "-d5"], MS_TWO, out);
    si(ms, &[ms, "-o.", "-f9"], MS_TWO, out);
    si(ms, &[ms, "-o.", "-d5", "-f9"], MS_TWO, out);
    // `-d1` with two messages: the width is a minimum, not a truncation.
    si(ms, &[ms, "-o.", "-d1"], MS_TWO, out);
    // `-b` (mboxrd) over the same payload, where the `>From ` line is unescaped
    // back to `From ` on the way out — so `-b` and the default write *different
    // bytes* for the same input, which is the whole reason the flag exists.
    si(ms, &[ms, "-o.", "-b"], MS_TWO, out);
    si(ms, &[ms, "-o.", "-b", "-d5", "-f9"], MS_TWO, out);

    // A bare `From ` line in the body: one message, not two.
    si(ms, &[ms, "-o."], MS_FROM_IN_BODY, out);
    si(ms, &[ms, "-o.", "-b"], MS_FROM_IN_BODY, out);

    // CRLF, with and without the flag that keeps the CR. Without it every line
    // in the written file loses its `\r`, including the header lines.
    si(ms, &[ms, "-o."], MS_CRLF, out);
    si(ms, &[ms, "-o.", "--keep-cr"], MS_CRLF, out);
    si(ms, &[ms, "-o.", "-b", "--keep-cr"], MS_CRLF, out);

    // Nothing on stdin: zero messages, no files written, exit 0.
    si(ms, &[ms, "-o."], b"", out);
}
