//! Differential corpus cases for the **gitattributes matcher itself** —
//! `attr.c`'s file stack, its line parser, its macro expander, and
//! `builtin/check-attr.c`, the one command whose entire output *is* that
//! engine's answer.
//!
//! Every case here is compared against stock git for stdout, exit code and the
//! post-command state digest; the one case whose whole behaviour is a
//! line-numbered diagnostic is [`Case::strict`] and is compared on stderr too.
//!
//! # The lever this module is built on, and that it is the first to use
//!
//! Three modules independently recorded the same wall, in the same words:
//! *attributes have no `-c` spelling, a case is one argv against a pristine
//! copy, so a case can only reach a rule some fixture already contains.*
//! `attributes_filters.rs` says it of `filter.<n>.clean` and of `export-subst`;
//! `eol_conversion.rs` says it of `working-tree-encoding`; `archive_export.rs`
//! says it of `export-subst`. All three checked `core.attributesFile` and
//! concluded it "cannot supply new rules … no file in any fixture contains a
//! line that parses as `<pattern> <attr>=<value>`".
//!
//! That last clause is the part that has stopped being true.
//! [`ConfigEntry::raw`] on [`ConfigScope::Modules`] writes **arbitrary bytes**
//! to `.gitmodules`, at a path a case can name, and `.gitmodules` is read as
//! *configuration* only by `submodule-config.c`, for `submodule.*` keys, and by
//! no verb in this file. `ignore_engine.rs` found this and pointed
//! `core.excludesFile` at it to unlock the ignore-file grammar.
//! **`core.attributesFile` is the same lever for attributes, and it works.**
//! Verified by hand on stock 2.55.0 against a copy of [`Shape::Attributes`]
//! carrying `.gitmodules` = `src/*.rs mine=one\n*.md mine=two\n`:
//!
//! ```text
//! $ git -c core.attributesFile=.gitmodules check-attr -a src/tabs.rs docs/manual.md
//! src/tabs.rs: text: set
//! src/tabs.rs: mine: one
//! src/tabs.rs: eol: lf
//! src/tabs.rs: whitespace: tab-in-indent,trailing-space
//! docs/manual.md: diff: markdown
//! docs/manual.md: text: auto
//! docs/manual.md: mine: two
//! docs/manual.md: export-ignore: set
//! ```
//!
//! What that unlocks, all of it measured for the first time here: the whole
//! value grammar (`attr`, `-attr`, `!attr`, `attr=value`, `attr=`), the whole
//! pattern grammar, macro definition and expansion including a redefinition of a
//! *built-in* macro, the file parser's comment/blank/CRLF/whitespace rules with
//! **line numbers the diagnostic reports back**, and — because the file sits at
//! the bottom of the precedence stack — the layering of that file against
//! `.gitattributes`, `sub/.gitattributes` and `.git/info/attributes`. It also
//! reaches, for the first time, three attributes three other modules had written
//! off: `filter=<driver>`, `working-tree-encoding=<enc>` and
//! `conflict-marker-size=<n>`. Those three are *their* territory, so what is
//! here is the minimum that proves the matcher routes to them (see below); the
//! breadth belongs to the modules that own each consumer.
//!
//! **`export-subst` stays unreachable, and the two new mechanisms were checked
//! rather than assumed.** The attribute is now settable — `check-attr -a`
//! reports `export-subst: set` in [`carried_attributes`] — but its *effect* is
//! substituting `$Format:…$` inside a **tracked blob**, and no blob in any
//! fixture contains that string (`grep -n 'Format:' src/parity/src/fixture.rs`
//! is empty). The obvious escape is `archive`'s own content-supplying options,
//! and it does not work: measured on stock 2.55.0, neither `--add-file=` nor
//! `--add-virtual-file=` consults the attribute stack at all. With
//! `.gitmodules` = `v.sub export-ignore` and `core.attributesFile` naming it,
//! `archive --format=tar --add-virtual-file='v.sub:hi' HEAD -- docs` still wrote
//! `v.sub` into the tar; with `subj.txt export-subst` and a worktree
//! `subj.txt` holding `H=$Format:%H$`, `archive --add-file=subj.txt` emitted the
//! bytes verbatim. Extra files bypass the matcher, so the only way to
//! `export-subst` anything is a fixture blob containing a placeholder.
//!
//! # Territory, against every module that already touches the subject
//!
//! Read in full before adding anything here: `info_attrs.rs` (the nearest
//! neighbour), `attributes_filters.rs`, `eol_conversion.rs`, `ignore_engine.rs`,
//! `archive_formats.rs`, `pathspec_stdin.rs`, `grep_engine.rs`, plus the
//! `check-attr` lines in `shape_reach.rs` and `exit_codes.rs`. All of them were
//! read for this module.
//!
//! | module | what it owns |
//! |---|---|
//! | `info_attrs.rs` | `check-attr`'s **flags and framing on a shape with no rules**: `-a`/`--all`, `--cached`, `--stdin`, `-z`, `--source=HEAD`/`v0.2.0`/`nope`, a missing path, a directory, the C-quoting of awkward names, and the four bare error paths. It is the "nothing configured" half |
//! | `shape_reach.rs` | one `check-attr` readout per **rule already in `Shape::Attributes`**, against the path that rule was written for — 26 single-answer lookups, plus `-z`, `--cached` and `--` forms of them |
//! | `attributes_filters.rs` | attributes that **drive a program**: `filter.<n>.*`, `diff.<n>.textconv`/`.xfuncname`/`.command`, `merge.<n>.driver`, `ident`. Also the four `core.attributesFile` cases that point at an **existing fixture file** (`.mailmap`, `sub/.gitattributes`, `src/lib.rs`, a missing path) |
//! | `eol_conversion.rs` | `text`/`text=auto`/`eol=`/`core.autocrlf`/`core.eol`/`core.safecrlf` as **conversion**, crossed with each other and with payloads chosen to split the verdicts |
//! | `archive_formats.rs` / `archive_export.rs` | `archive` as a **format writer** and its options; `export-ignore` as archive behaviour on the rule the fixture ships (`*.md`) |
//! | `pathspec_stdin.rs` | `:(attr:…)` pathspec magic, and `check-attr --stdin` fed the shared awkward-**pathname** payloads on `Shape::AwkwardPaths` |
//! | `ignore_engine.rs` | the *exclude* engine and the `.gitmodules` lever, in its `core.excludesFile` form |
//! | `grep_engine.rs` | `grep`'s exclude booleans; no attribute case |
//!
//! # What is here and in none of them
//!
//! **1. The attribute *file* as a parser, with the file's bytes chosen by the
//! case.** Not "does `check-attr` work" — `info_attrs.rs` has that — and not
//! "what does rule R say about path P" — `shape_reach.rs` has that — but what
//! `attr.c:parse_attr_line` makes of bytes nobody has ever handed it: the four
//! value forms, a value containing `=`, an empty value, one line setting the
//! same attribute twice, a line setting and then unsetting, an invalid
//! attribute name (which produces a `file:line` diagnostic and lets the rest of
//! the line stand), a comment, a blank line, leading whitespace, trailing
//! whitespace, a whole file in CRLF, a `"`-quoted pattern, a backslash escape,
//! and macro definition — including a macro that **redefines the built-in
//! `binary`**, a macro used above its definition, a macro expanding to another
//! macro, and a macro in negated form, which is *not* expanded.
//!
//! **2. The stack, from the bottom.** The fixture ships three attribute files
//! and `shape_reach.rs` reads answers out of them, but nothing could add a
//! *fourth, competing* source, so no case could show which of two files won —
//! only what one file said. The lever is the bottom of the stack, so a rule
//! placed there and contradicted higher up makes the ordering visible:
//! `.git/info/attributes` over `core.attributesFile`, `.gitattributes` over
//! `core.attributesFile`, and the global file supplying what neither of them
//! mentions. Plus, inside one file, later-line-wins.
//!
//! **3. `--source` / `--attr-source` / `GIT_ATTR_SOURCE` as a *replacement* of
//! the worktree stack.** `info_attrs.rs` runs `--source=HEAD` and
//! `--source=v0.2.0`, which name trees whose `.gitattributes` is the one already
//! in effect — so a port that ignored the flag entirely scored the same. Here
//! the source is a tree that changes the answer: `HEAD:src` has **no**
//! `.gitattributes` (every worktree rule vanishes, `.git/info/attributes`
//! survives, and `core.attributesFile` survives), and `HEAD:sub` promotes
//! `sub/.gitattributes` to the root. Both spellings of the environment/global
//! form are here too, including their shared `fatal: bad --attr-source or
//! GIT_ATTR_SOURCE`, and one case proves the option reaches a verb that is not
//! `check-attr`.
//!
//! **4. The consumers, each asked whether the *match* reached it.** This is the
//! class the brief names: a wrong match is invisible until much later. Each case
//! sets one attribute through the lever and reads a consumer that must change:
//! `-diff` turns a text diff into `Binary files … differ`; `diff=<driver>` plus
//! `diff.<driver>.xfuncname` changes a hunk header; `export-ignore` removes an
//! entry from a tar; `filter=<driver>` routes a blob through a clean/smudge;
//! `working-tree-encoding` re-encodes on the way out; `conflict-marker-size` and
//! `-merge` and `merge=union` change what `merge-tree --write-tree` writes.
//! Every one is paired with a control that differs only in the pattern or in
//! nothing at all, so "the consumer changed" is separable from "the consumer
//! always does that".
//!
//! # What is not measurable here, and why
//!
//! * **`export-subst`'s substitution.** See above: no fixture blob contains
//!   `$Format:`, and `archive`'s two content-supplying options bypass the
//!   matcher. Only the readout is pinned.
//! * **`delta` / `-delta`.** The attribute changes whether `pack-objects` will
//!   delta a blob. The only readouts are pack bytes and `verify-pack` output,
//!   both of which are `object_pack.rs`'s subject and neither of which is a
//!   function of *this* module's question. Only the readout is pinned, in
//!   [`carried_attributes`].
//! * **A macro in a non-top-level file.** `attr.c` refuses `[attr]` outside the
//!   top-level `.gitattributes`, `.git/info/attributes` and the
//!   `core.attributesFile`, with `Macro in %s is not allowed`. Every file a case
//!   can choose the bytes of is one of the three where macros *are* allowed, so
//!   the refusal needs a fixture change. Stated, not worked around.
//! * **`--cached` doing anything.** It reads `.gitattributes` from the index
//!   instead of the worktree, and in every shape the two are byte-identical, so
//!   the flag is pinned only as "same answer". A shape with a staged-but-not-
//!   checked-out `.gitattributes` would be needed.
//! * **The outside-repository refusal's text.** `fatal: '../outside.txt' is
//!   outside repository at '<abs>'` names the fixture root, which differs
//!   between the two sides by construction. [`path_forms`]'s case is therefore
//!   not strict: its partial stdout and its 128 are what is compared.
//! * **Case folding is a property of the filesystem, not of the port.**
//!   [`pattern_grammar`]'s `SRC/TABS.RS` case answers `set` where the fixture was
//!   built on a case-insensitive filesystem (git records `core.ignorecase=true`
//!   at `init`) and `unspecified` where it was not. Both sides read the same
//!   repository, so the comparison stays valid on either; the *answer* is not
//!   portable and the case is not a pin on one.
//! * **A file with no trailing newline.** `runner::render_config_entry` appends
//!   exactly one `\n` to a raw entry, so an attribute file whose last line is
//!   unterminated cannot be written. Nothing here depends on it.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    attr_source(out);
    file_stack(out);
    value_grammar(out);
    macros(out);
    pattern_grammar(out);
    file_parser(out);
    stdin_reader(out);
    path_forms(out);
    carried_attributes(out);
    diff_consumer(out);
    archive_consumer(out);
    filter_consumer(out);
    encoding_consumer(out);
    merge_consumer(out);
}

// ---------------------------------------------------------------------------
// The lever
// ---------------------------------------------------------------------------

/// `.gitmodules` holding `body`, named as `core.attributesFile`.
///
/// The raw entry is written verbatim by `runner::install_config`, which appends
/// exactly one `\n` — so `body` is the file minus its final newline, and every
/// line number a diagnostic reports is an index into this literal.
///
/// `.gitmodules` is chosen for the reason `ignore_engine.rs` gives: it is the
/// one file scope whose bytes no verb here parses as configuration, and it lives
/// at a relative path inside the fixture that a case may name.
fn attr_file(body: &'static str) -> Vec<ConfigEntry> {
    vec![
        ConfigEntry::raw(ConfigScope::Modules, body),
        ConfigEntry::set(ConfigScope::CommandLine, "core.attributesFile", ".gitmodules"),
    ]
}

/// [`attr_file`], plus extra command-line settings the consumer needs.
fn attr_file_with(body: &'static str, extra: &[(&str, &str)]) -> Vec<ConfigEntry> {
    let mut v = attr_file(body);
    for (k, val) in extra {
        v.push(ConfigEntry::set(ConfigScope::CommandLine, *k, *val));
    }
    v
}

/// One case against [`Shape::Attributes`] whose lowest attribute source is a
/// synthetic file the case chose the bytes of.
fn rules(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], body: &'static str) {
    out.push(Case::new(cmd, args, Shape::Attributes).with_scoped_config(attr_file(body)));
}

/// [`rules`], with stderr compared as well.
fn rules_strict(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], body: &'static str) {
    out.push(Case::strict(cmd, args, Shape::Attributes).with_scoped_config(attr_file(body)));
}

/// One bare case against [`Shape::Attributes`] — no synthetic file at all.
fn at(out: &mut Vec<Case>, cmd: &'static str, args: &[&str]) {
    out.push(Case::new(cmd, args, Shape::Attributes));
}

// ---------------------------------------------------------------------------
// 1. Which *tree* the stack is read from
// ---------------------------------------------------------------------------

/// `--source=<tree-ish>`, `--attr-source=<tree-ish>` and `GIT_ATTR_SOURCE` all
/// replace the worktree half of the stack with a tree's, and the fixture holds
/// two trees that make the replacement visible.
///
/// Observed on stock 2.55.0 against a copy of [`Shape::Attributes`]:
///
/// | invocation | stdout |
/// |---|---|
/// | `check-attr -a src/tabs.rs` | `text: set`, `eol: lf`, `whitespace: tab-in-indent,trailing-space` |
/// | `check-attr --source=HEAD:src -a src/tabs.rs` | *(empty)* |
/// | `check-attr --source=HEAD:sub -a nested.txt` | `diff: unset`, `text: set`, `eol: crlf` |
/// | `check-attr --source=HEAD:src -a stamp.info` | `ident: set` |
/// | `check-attr -a stamp.info` | `text: auto`, `ident: set` |
///
/// The last pair is the fact worth stating: `HEAD:src` holds no
/// `.gitattributes`, so the root file's `* text=auto` disappears, while
/// `.git/info/attributes`' `*.info ident` is **not** part of the tree and
/// survives. A port that implemented `--source` by simply suppressing every file
/// would agree on row two and fail on row four.
///
/// `HEAD:src/tabs.rs` names a **blob**, which git accepts as a source and reads
/// as an empty rule set rather than refusing — `nope` is what a refusal looks
/// like, and `info_attrs.rs` already pins that one on `Shape::Linear`.
fn attr_source(out: &mut Vec<Case>) {
    at(out, "check-attr", &["check-attr", "--source=HEAD:src", "-a", "src/tabs.rs"]);
    at(out, "check-attr", &["check-attr", "--source=HEAD:sub", "-a", "nested.txt"]);
    at(out, "check-attr", &["check-attr", "--source=HEAD:src", "-a", "stamp.info"]);
    at(out, "check-attr", &["check-attr", "-a", "stamp.info"]);
    at(out, "check-attr", &["check-attr", "--source=HEAD:src/tabs.rs", "-a", "src/tabs.rs"]);
    at(out, "check-attr", &["check-attr", "--source=HEAD^{tree}", "text", "--", "src/tabs.rs"]);
    at(out, "check-attr", &["check-attr", "--source=HEAD~3", "-a", "docs/manual.md"]);
    // `--cached` and `--source` together: the index and the named tree agree in
    // every shape, so this pins that combining them is not itself an error.
    at(out, "check-attr", &["check-attr", "--cached", "--source=HEAD", "-a", "src/tabs.rs"]);

    // The same selection through the global option and through the environment.
    // Both spellings feed one variable in `attr.c`, and both reject a bad value
    // with the same message: `fatal: bad --attr-source or GIT_ATTR_SOURCE`, 128.
    out.push(
        Case::new("check-attr", &["check-attr", "-a", "src/tabs.rs"], Shape::Attributes)
            .with_globals(&[&["--attr-source=HEAD:src"]]),
    );
    out.push(
        Case::new("check-attr", &["check-attr", "-a", "src/tabs.rs"], Shape::Attributes)
            .with_env(&[("GIT_ATTR_SOURCE", "HEAD:src")]),
    );
    // The option and the variable together: measured, the *option* wins, so the
    // answer is `HEAD:src`'s empty one and not `HEAD`'s three lines.
    out.push(
        Case::new("check-attr", &["check-attr", "-a", "src/tabs.rs"], Shape::Attributes)
            .with_globals(&[&["--attr-source=HEAD:src"]])
            .with_env(&[("GIT_ATTR_SOURCE", "HEAD")]),
    );
    out.push(
        Case::new("check-attr", &["check-attr", "-a", "src/tabs.rs"], Shape::Attributes)
            .with_env(&[("GIT_ATTR_SOURCE", "nope")]),
    );
    out.push(
        Case::new("check-attr", &["check-attr", "-a", "src/tabs.rs"], Shape::Attributes)
            .with_globals(&[&["--attr-source=nope"]]),
    );
    // The selection is a repository-wide fact, not a `check-attr` one: `diff`
    // reads the same variable to decide whether `src/tabs.rs` is text.
    out.push(
        Case::new("diff", &["diff", "HEAD~1", "HEAD"], Shape::Attributes)
            .with_globals(&[&["--attr-source=HEAD:src"]]),
    );
    // `GIT_ATTR_NOSYSTEM` suppresses the system-wide file. Nothing in the
    // hermetic environment supplies one, so the answer must be unchanged — which
    // is what makes it a control on a variable a port could mis-wire into
    // suppressing the *repository's* files instead.
    out.push(
        Case::new("check-attr", &["check-attr", "-a", "src/tabs.rs"], Shape::Attributes)
            .with_env(&[("GIT_ATTR_NOSYSTEM", "1")]),
    );
}

// ---------------------------------------------------------------------------
// 2. The stack, read from the bottom
// ---------------------------------------------------------------------------

/// The four sources in precedence order are `core.attributesFile` (lowest), the
/// root `.gitattributes`, a deeper `.gitattributes`, and
/// `.git/info/attributes` (highest). Only the lowest can be written by a case,
/// which is exactly what makes the ordering observable: a rule placed there and
/// contradicted above must lose, and a rule placed there that nothing above
/// mentions must win.
///
/// Observed on stock 2.55.0, `.gitmodules` = `*.rs text=GLOBALWINS glob=yes`
/// and `info-only.txt text=GLOBAL2`:
///
/// ```text
/// $ git -c core.attributesFile=.gitmodules check-attr text glob -- src/tabs.rs info-only.txt
/// src/tabs.rs: text: set          # .gitattributes `*.rs text` beats `text=GLOBALWINS`
/// src/tabs.rs: glob: yes          # nothing above mentions `glob`
/// info-only.txt: text: set        # .git/info/attributes beats both
/// info-only.txt: glob: unspecified
/// ```
///
/// and with `*.info ident=GLOBAL` / `info-only.txt globalonly`:
///
/// ```text
/// x.info: ident: set              # info/attributes' `*.info ident`, not `=GLOBAL`
/// x.info: globalonly: unspecified
/// info-only.txt: ident: unspecified
/// info-only.txt: globalonly: set
/// ```
///
/// `info-only.txt: text: set` is the one answer in the whole corpus that says
/// `.git/info/attributes` outranks the worktree: the root file says
/// `* text=auto`, the info file says `info-only.txt text`, and `set` is the info
/// file's. `shape_reach.rs` never asks about `info-only.txt`.
fn file_stack(out: &mut Vec<Case>) {
    rules(
        out,
        "check-attr",
        &["check-attr", "text", "glob", "--", "src/tabs.rs", "info-only.txt"],
        "*.rs text=GLOBALWINS glob=yes\ninfo-only.txt text=GLOBAL2",
    );
    rules(
        out,
        "check-attr",
        &["check-attr", "ident", "globalonly", "--", "x.info", "info-only.txt"],
        "*.info ident=GLOBAL\ninfo-only.txt globalonly",
    );
    // The bare readout of the same fact, with no synthetic file involved at all.
    at(out, "check-attr", &["check-attr", "-a", "info-only.txt"]);

    // Within one file, the last line wins — a separate implementation from the
    // between-files ordering above.
    rules(
        out,
        "check-attr",
        &["check-attr", "dup", "--", "src/tabs.rs"],
        "src/tabs.rs dup=first\nsrc/tabs.rs dup=second",
    );
    // ... and the last *token* on one line wins, which is a third one.
    rules(
        out,
        "check-attr",
        &["check-attr", "a", "dup", "both", "--", "src/tabs.rs"],
        "src/tabs.rs a=b=c dup=1 dup=2 both -both",
    );

    // A tree source and the global file together: the tree half is emptied and
    // the global half is not, so the answer is exactly the synthetic file's.
    out.push(
        Case::new("check-attr", &["check-attr", "-a", "src/tabs.rs"], Shape::Attributes)
            .with_scoped_config(attr_file("src/tabs.rs g=1"))
            .with_env(&[("GIT_ATTR_SOURCE", "HEAD:src")]),
    );
}

// ---------------------------------------------------------------------------
// 3. The value grammar
// ---------------------------------------------------------------------------

/// `attr.c:parse_attr` recognises four forms and `check-attr` prints four
/// verdicts, and nothing in the corpus had all four on one path.
///
/// Observed on stock 2.55.0 with `.gitmodules` =
/// `src/tabs.rs plain -minus !bang val=hello`:
///
/// ```text
/// src/tabs.rs: plain: set
/// src/tabs.rs: minus: unset
/// src/tabs.rs: bang: unspecified
/// src/tabs.rs: val: hello
/// ```
///
/// `!attr` is the form that matters most: it is not "unset", it is "forget every
/// rule that touched this attribute", and a port that treats `!` as a synonym
/// for `-` prints `unset` here and agrees everywhere else. The second case pins
/// it against a rule it has to override — `* base=root` sets it for every path,
/// and `!base` on one path takes it back to `unspecified` while `docs/manual.md`
/// keeps `root`.
///
/// The third case is the one a port is most likely to get subtly wrong: `true`,
/// `false` and `unset` are *ordinary strings* as values, and must print as
/// themselves rather than collapsing into the `set`/`unset` verdicts they name.
/// The fourth is an empty value, which prints as a bare trailing space in the
/// human format and as an empty field under `-z`.
fn value_grammar(out: &mut Vec<Case>) {
    rules(
        out,
        "check-attr",
        &["check-attr", "plain", "minus", "bang", "val", "--", "src/tabs.rs"],
        "src/tabs.rs plain -minus !bang val=hello",
    );
    rules(
        out,
        "check-attr",
        &["check-attr", "base", "--", "src/tabs.rs", "docs/manual.md"],
        "* base=root\nsrc/tabs.rs !base",
    );
    rules(
        out,
        "check-attr",
        &["check-attr", "-a", "src/tabs.rs"],
        "src/tabs.rs t=true f=false u=unset",
    );
    rules(out, "check-attr", &["check-attr", "empty", "--", "src/tabs.rs"], "src/tabs.rs empty=");
}

// ---------------------------------------------------------------------------
// 4. Macros
// ---------------------------------------------------------------------------

/// `[attr]<name> <attrs>...` defines a macro, and a macro expands **before** the
/// attribute it names is looked up. Nothing in the corpus defined one: the
/// fixture uses the built-in `binary`, which `shape_reach.rs` reads out, but a
/// built-in cannot show the definition path.
///
/// Observed on stock 2.55.0, all against `assets/logo.bin` or `src/tabs.rs`:
///
/// | `.gitmodules` | `check-attr -a` says |
/// |---|---|
/// | `[attr]mybin -diff -merge -text` + `assets/logo.bin mybin` | `binary: set`, `diff: unset`, `merge: unset`, `text: unset`, `mybin: set` |
/// | `[attr]binary -diff mymark` | `binary: set`, `diff: unset`, `text: auto`, `mymark: set` |
///
/// The second row is the interesting one and the reason it is here: redefining
/// the **built-in** `binary` macro is honoured, so `merge` is no longer unset at
/// all and `text` falls back to the root file's `auto` instead of the built-in's
/// `unset`. A port that hard-codes `binary` as a synonym for `-diff -merge
/// -text` prints the first row's answer for the second row's input.
///
/// The remaining three are ordering and expansion facts, each verified:
///
/// * a macro **used above its definition** still expands (`attr.c` resolves
///   macros after the whole file is read): `src/tabs.rs mymac` on line 1 and
///   `[attr]mymac -diff zzz` on line 2 gives `diff: unset`, `zzz: set`;
/// * a macro expanding to another macro expands transitively: `[attr]m1 m2`,
///   `[attr]m2 -diff deep` gives `diff: unset`, `deep: set`, `m1: set`,
///   `m2: set`;
/// * a **negated** macro is not expanded — `src/tabs.rs mv` then
///   `src/tabs.rs -mv` gives `mv: unset` and `v: unspecified`, not `v: 7`.
///
/// The bad-name case is the parser's, not the expander's: `[attr]bad@mac` is
/// rejected with the same `file:line` diagnostic an ordinary bad attribute name
/// gets, and the *following* line is still read.
fn macros(out: &mut Vec<Case>) {
    rules(
        out,
        "check-attr",
        &["check-attr", "-a", "assets/logo.bin"],
        "[attr]mybin -diff -merge -text\nassets/logo.bin mybin",
    );
    rules(out, "check-attr", &["check-attr", "-a", "assets/logo.bin"], "[attr]binary -diff mymark");
    rules(
        out,
        "check-attr",
        &["check-attr", "diff", "zzz", "mymac", "--", "src/tabs.rs"],
        "src/tabs.rs mymac\n[attr]mymac -diff zzz",
    );
    rules(
        out,
        "check-attr",
        &["check-attr", "diff", "deep", "m1", "m2", "--", "src/tabs.rs"],
        "[attr]m1 m2\n[attr]m2 -diff deep\nsrc/tabs.rs m1",
    );
    rules(
        out,
        "check-attr",
        &["check-attr", "mv", "v", "w", "--", "src/tabs.rs"],
        "[attr]mv v=7 -w\nsrc/tabs.rs mv",
    );
    rules(
        out,
        "check-attr",
        &["check-attr", "mv", "v", "--", "src/tabs.rs"],
        "[attr]mv v=7\nsrc/tabs.rs mv\nsrc/tabs.rs -mv",
    );
    rules(out, "check-attr", &["check-attr", "ok", "--", "src/tabs.rs"], "[attr]bad@mac -diff\nsrc/tabs.rs ok");
    // `[attr]` with no name at all: accepted silently, and the rest of the file
    // is read. The control that says the row above is a *name* check.
    rules(out, "check-attr", &["check-attr", "ok2", "--", "src/tabs.rs"], "[attr] -diff\nsrc/tabs.rs ok2");
}

// ---------------------------------------------------------------------------
// 5. The pattern grammar
// ---------------------------------------------------------------------------

/// The pattern half of a gitattributes line has the same surface as a
/// `.gitignore` pattern and *not* the same semantics, and the differences are
/// where a port that reuses one matcher for both goes wrong.
///
/// One file carries nine patterns; four cases ask four paths about all nine, so
/// each answer is read against the eight rules that did **not** fire. Observed
/// on stock 2.55.0 (`set` marked, everything else `unspecified`):
///
/// | pattern | `src/tabs.rs` | `sub/nested.txt` | `vendor/generated.js` | `sub` |
/// |---|---|---|---|---|
/// | `/tabs.rs anchored` | | | | |
/// | `tabs.rs basename` | set | | | |
/// | `sub/ dironly` | | | | |
/// | `*.rs star` | set | | | |
/// | `?abs.rs qmark` | set | | | |
/// | `src/tab[a-z].rs bracket` | set | | | |
/// | `**/nested.txt dstar` | | set | | |
/// | `vendor/**/*.js dsmid` | | | set | |
/// | `a**b weird` | | | | |
///
/// Two rows are the whole point. **`sub/ dironly` matches nothing** — a
/// gitattributes pattern has no directory-only form, so a trailing `/` produces a
/// pattern that no path can equal, where the identical `.gitignore` line matches
/// a whole subtree. And **`/tabs.rs anchored` matches nothing** while
/// `tabs.rs basename` matches `src/tabs.rs`: a leading slash anchors to the
/// attribute file's own directory, and a slashless pattern matches the basename
/// at any depth. `a**b` is a third: `**` is only special as a whole path
/// component, so `a**b` is two ordinary stars and matches nothing here.
///
/// The last case is the filesystem one. `SRC/TABS.RS upper` answers `set` where
/// the fixture was built on a case-insensitive filesystem and `unspecified`
/// where it was not; both sides read the same repository either way, so the
/// comparison holds without the answer being portable. See the module header.
fn pattern_grammar(out: &mut Vec<Case>) {
    const NINE: &str = "/tabs.rs anchored\n\
                        tabs.rs basename\n\
                        sub/ dironly\n\
                        *.rs star\n\
                        ?abs.rs qmark\n\
                        src/tab[a-z].rs bracket\n\
                        **/nested.txt dstar\n\
                        vendor/**/*.js dsmid\n\
                        a**b weird";
    const ASK: &[&str] = &[
        "check-attr", "anchored", "basename", "dironly", "star", "qmark", "bracket", "dstar",
        "dsmid", "weird", "--",
    ];
    for path in ["src/tabs.rs", "sub/nested.txt", "vendor/generated.js", "sub"] {
        let mut argv = ASK.to_vec();
        argv.push(path);
        rules(out, "check-attr", &argv, NINE);
    }
    // A `"`-quoted pattern is C-unquoted before matching, and a backslash escapes
    // the next character rather than introducing one.
    rules(out, "check-attr", &["check-attr", "quoted", "--", "src/tabs.rs"], "\"src/tabs.rs\" quoted");
    rules(
        out,
        "check-attr",
        &["check-attr", "esc", "anystar", "--", "src/tabs.rs"],
        "src/ta\\bs.rs esc\nsrc/*.rs anystar",
    );
    rules(out, "check-attr", &["check-attr", "upper", "--", "src/tabs.rs"], "SRC/TABS.RS upper");
}

// ---------------------------------------------------------------------------
// 6. The file parser: bytes that are not a rule
// ---------------------------------------------------------------------------

/// What `attr.c` does with a line before it ever reaches the pattern: skip it,
/// strip it, or diagnose it.
///
/// Observed on stock 2.55.0.
///
/// * **CRLF.** A whole file written `src/tabs.rs crlfattr=yes\r\n*.md
///   second\r\n` behaves exactly as the LF form: `crlfattr: yes` on
///   `src/tabs.rs`, `second: set` on `docs/manual.md`. The `\r` is stripped from
///   the end of each line, and it is stripped from the *value* too — a port that
///   only strips it from the pattern prints `crlfattr: yes\r`.
/// * **Comments, blanks and whitespace.** `# a comment line`, an empty line,
///   `src/tabs.rs  spaced=1   ` (two spaces before the attribute, three after)
///   and `\t docs/manual.md leadws` (a tab and a space *before* the pattern)
///   give `spaced: 1` and `leadws: set` — so trailing whitespace is dropped
///   without becoming part of the value, and leading whitespace is skipped
///   rather than becoming part of the pattern. Note the contrast with
///   `.gitignore`, where leading whitespace is **kept** (`ignore_engine.rs`
///   measures that); the two parsers differ here and a shared implementation
///   fails one of them.
/// * **An invalid attribute name.** `src/tabs.rs bad@name` is refused with
///   `bad@name is not a valid attribute name: .gitmodules:1` on **stderr**, exit
///   stays 0, the rest of the file is read, and the *rest of the same line* is
///   read too — `ok` on line 2 is still `set`. The message names the file and
///   the line, which is a fact only a file-based source has, so this case is
///   [`Case::strict`]. It is the only strict case in this module.
fn file_parser(out: &mut Vec<Case>) {
    rules(
        out,
        "check-attr",
        &["check-attr", "crlfattr", "second", "--", "src/tabs.rs", "docs/manual.md"],
        "src/tabs.rs crlfattr=yes\r\n*.md second\r",
    );
    rules(
        out,
        "check-attr",
        &["check-attr", "spaced", "leadws", "--", "src/tabs.rs", "docs/manual.md"],
        "# a comment line\n\nsrc/tabs.rs  spaced=1   \n\t docs/manual.md leadws",
    );
    rules_strict(
        out,
        "check-attr",
        &["check-attr", "-a", "src/tabs.rs"],
        "src/tabs.rs bad@name\nsrc/tabs.rs ok",
    );
}

// ---------------------------------------------------------------------------
// 7. `--stdin` as a reader
// ---------------------------------------------------------------------------

/// NUL-separated payload, two paths.
const NUL_TWO: &[u8] = b"src/tabs.rs\0docs/manual.md\0";
/// The same, without the final terminator — the last record has to be flushed
/// at EOF rather than dropped.
const NUL_UNTERMINATED: &[u8] = b"src/tabs.rs\0docs/manual.md";
/// LF-separated payload, two paths.
const LF_TWO: &[u8] = b"src/tabs.rs\ndocs/manual.md\n";
/// CRLF-separated payload: the `\r` stays part of each pathname.
const CRLF_TWO: &[u8] = b"src/tabs.rs\r\nsub/nested.txt\r\n";
/// A C-quoted pathname, which the reader must unquote.
const QUOTED_ONE: &[u8] = b"\"src/tabs.rs\"\n";

/// `check_attr_stdin_paths` has four code paths and the terminator it was told
/// to expect decides which. `info_attrs.rs` proves `--stdin` terminates on an
/// empty stdin; `pathspec_stdin.rs` feeds it the shared awkward-**pathname**
/// payloads. Neither reads a payload whose framing disagrees with the flag, and
/// neither does it on a shape that has rules — so neither could show that the
/// *wrong* framing still produces a well-formed answer about a wrong path.
///
/// Observed on stock 2.55.0 against a copy of [`Shape::Attributes`]:
///
/// | flags | payload | stdout |
/// |---|---|---|
/// | `-z --stdin -a` | `NUL_TWO`, `.gitmodules` = `src/tabs.rs e=` | `src/tabs.rs\0text\0set\0src/tabs.rs\0e\0\0src/tabs.rs\0eol\0lf\0…` |
/// | `--stdin -a` | `LF_TWO`, same file | the same eight answers in the human format, `src/tabs.rs: e: ` among them |
/// | `-z --stdin text` | `LF_TWO` | **one** answer, for the path `src/tabs.rs\ndocs/manual.md\n`, `text: auto` |
/// | `-z --stdin text` | `NUL_UNTERMINATED` | two answers; the unterminated tail is still a record |
/// | `--stdin text` | `CRLF_TWO` | `"src/tabs.rs\r": text: auto` — the `\r` is part of the name, so `*.rs` does **not** match, and the echo is C-quoted |
/// | `--stdin text` | `QUOTED_ONE` | `src/tabs.rs: text: set` — unquoted, and now `*.rs` matches |
///
/// The first two rows carry an attribute with an **empty value** through both
/// framings, which is the one output shape where the two formats are not
/// mechanically derivable from each other: `-z` writes an empty field between
/// two NULs, the human format writes a line ending in a bare space.
fn stdin_reader(out: &mut Vec<Case>) {
    out.push(
        Case::with_stdin("check-attr", &["check-attr", "-z", "--stdin", "-a"], Shape::Attributes, NUL_TWO)
            .with_scoped_config(attr_file("src/tabs.rs e=")),
    );
    out.push(
        Case::with_stdin("check-attr", &["check-attr", "--stdin", "-a"], Shape::Attributes, LF_TWO)
            .with_scoped_config(attr_file("src/tabs.rs e=")),
    );
    out.push(Case::with_stdin("check-attr", &["check-attr", "-z", "--stdin", "text"], Shape::Attributes, LF_TWO));
    out.push(Case::with_stdin(
        "check-attr",
        &["check-attr", "-z", "--stdin", "text"],
        Shape::Attributes,
        NUL_UNTERMINATED,
    ));
    out.push(Case::with_stdin("check-attr", &["check-attr", "--stdin", "text"], Shape::Attributes, CRLF_TWO));
    out.push(Case::with_stdin("check-attr", &["check-attr", "--stdin", "text"], Shape::Attributes, QUOTED_ONE));
}

// ---------------------------------------------------------------------------
// 8. What counts as a pathname
// ---------------------------------------------------------------------------

/// `check-attr` takes **pathnames**, not pathspecs, and normalises them far less
/// than `check-ignore` does. Four forms in one invocation, and the fifth kills
/// the process after the first four have already printed.
///
/// Observed on stock 2.55.0:
///
/// ```text
/// $ git check-attr text -- ./src/tabs.rs src//tabs.rs sub/ ../outside.txt
/// ./src/tabs.rs: text: set
/// src//tabs.rs: text: set
/// sub/: text: auto
/// fatal: '../outside.txt' is outside repository at '<fixture root>'
/// rc=128
/// ```
///
/// Three things are pinned at once: the echo is the token **as given** rather
/// than a normalised form; `./` and `//` are absorbed by the matcher anyway, so
/// `*.rs` fires; a trailing `/` leaves a name no rule but `*` can match; and the
/// output written before the refusal is **kept**, so a port that validates every
/// pathname up front prints nothing and still exits 128. Not strict — the
/// message names the fixture root, which differs between the two sides.
///
/// The empty pathname is the contrast with `check-ignore`, which dies on it
/// (`ignore_engine.rs` pins `fatal: empty string is not a valid pathspec`):
/// `check-attr text -- ''` answers `: text: auto` and exits 0, because it never
/// builds a pathspec at all.
fn path_forms(out: &mut Vec<Case>) {
    at(
        out,
        "check-attr",
        &["check-attr", "text", "--", "./src/tabs.rs", "src//tabs.rs", "sub/", "../outside.txt"],
    );
    at(out, "check-attr", &["check-attr", "text", "--", ""]);
    // No pathname and no `--stdin`: `error: No file specified` plus the usage
    // block, exit 129. `exit_codes.rs` pins the `check-attr diff` spelling on
    // `Shape::Linear`; this is the `-a` spelling, where there is no attribute
    // name that could have been mistaken for the missing path.
    at(out, "check-attr", &["check-attr", "-a"]);
}

// ---------------------------------------------------------------------------
// 9. Attributes git carries and never interprets
// ---------------------------------------------------------------------------

/// The engine stores any name it can parse, whether or not git has a consumer
/// for it. That is what `linguist-*`, `gitlab-*` and every other third-party
/// convention rely on, and it is also the readout for two attributes whose
/// *effects* this harness cannot reach.
///
/// Observed on stock 2.55.0 with `.gitmodules` = `src/tabs.rs export-subst
/// delta -delta2 linguist-language=Rust gitlab-generated`:
///
/// ```text
/// src/tabs.rs: text: set
/// src/tabs.rs: export-subst: set
/// src/tabs.rs: delta: set
/// src/tabs.rs: delta2: unset
/// src/tabs.rs: linguist-language: Rust
/// src/tabs.rs: gitlab-generated: set
/// src/tabs.rs: eol: lf
/// src/tabs.rs: whitespace: tab-in-indent,trailing-space
/// ```
///
/// The ordering is itself the assertion: `-a` prints the attributes in the order
/// the engine **first saw** each name, not alphabetically and not in the order
/// they were resolved, so the synthetic file's five names land between the root
/// file's `text` and its `eol`/`whitespace`. A port with a sorted or a
/// resolution-ordered table agrees on every value and disagrees on every line.
///
/// `export-subst` and `delta` are here only as readouts; see the module header
/// for why neither effect is reachable.
fn carried_attributes(out: &mut Vec<Case>) {
    rules(
        out,
        "check-attr",
        &["check-attr", "-a", "src/tabs.rs"],
        "src/tabs.rs export-subst delta -delta2 linguist-language=Rust gitlab-generated",
    );
}

// ---------------------------------------------------------------------------
// 10. `diff` as a consumer
// ---------------------------------------------------------------------------

/// Three attributes reach `diff`, and each one changes the output in a different
/// place.
///
/// **`-diff` makes a text file binary.** With `.gitmodules` = `src/tabs.rs
/// -diff`, `diff HEAD~1 HEAD` prints `Binary files a/src/tabs.rs and
/// b/src/tabs.rs differ` in place of the hunk. The fixture already sets `-diff`
/// on `*.log` and `vendor/**`, but no commit touches either, so no case in the
/// corpus had ever seen the attribute suppress a body.
///
/// **`diff=<driver>` selects a driver by name.** With `src/tabs.rs diff=mydrv`
/// and `-c diff.mydrv.xfuncname='^fn .*'`, `diff -U1` produces the hunk header
/// `@@ -1,3 +1,3 @@` with no function context, where the same invocation on the
/// default driver produces the same header — the *pairing* is what is measured:
/// the attribute has to name the driver before the config key can do anything,
/// and `attributes_filters.rs` could only ever install the key. (It owns the
/// driver's behaviour; this owns the routing.)
///
/// **`whitespace=<rules>` decides what `--check` reports.** This one found a
/// defect, and the pair below is written so the defect cannot be confused with a
/// missing `--check`:
///
/// | invocation | stock 2.55.0 | zvcs |
/// |---|---|---|
/// | `diff --check HEAD~3 HEAD` | `src/tabs.rs:2: tab in indent.` + the line, rc **2** | *(nothing)*, rc **0** |
/// | `-c core.whitespace=tab-in-indent diff --check HEAD~3 HEAD -- src` | the same two lines, rc 2 | the same two lines, rc 2 |
///
/// So `--check` is implemented and `core.whitespace` is honoured; what is
/// dropped is the `*.rs whitespace=tab-in-indent,trailing-space` **attribute**,
/// which is the fixture's own rule and needs no synthetic file at all. The
/// second row is the control that proves it.
///
/// A third case pins the *precedence* between the two: with
/// `-c core.whitespace=-tab-in-indent`, which switches the rule **off**
/// globally, stock still prints `src/tabs.rs:2: tab in indent.` and still exits
/// 2, because the path's attribute outranks the configuration. zvcs is silent
/// there too. A port that read the attribute but layered it *under*
/// `core.whitespace` would pass the first row and fail this one.
///
/// The lever cannot strengthen this group any further, and the reason is worth
/// recording: `core.attributesFile` sits at the **bottom** of the stack, so a
/// `whitespace=` rule delivered through it always loses to the fixture's own
/// `*.rs whitespace=…`, and no other tracked path in the shape carries a
/// whitespace error for a lever-supplied rule to find.
fn diff_consumer(out: &mut Vec<Case>) {
    rules(out, "diff", &["diff", "HEAD~1", "HEAD"], "src/tabs.rs -diff");
    out.push(
        Case::new("diff", &["diff", "-U1", "HEAD~1", "HEAD"], Shape::Attributes)
            .with_scoped_config(attr_file_with(
                "src/tabs.rs diff=mydrv",
                &[("diff.mydrv.xfuncname", "^fn .*")],
            )),
    );
    at(out, "diff", &["diff", "--check", "HEAD~3", "HEAD"]);
    out.push(
        Case::new("diff", &["diff", "--check", "HEAD~3", "HEAD", "--", "src"], Shape::Attributes)
            .with_config(&[("core.whitespace", "tab-in-indent")]),
    );
    out.push(
        Case::new("diff", &["diff", "--check", "HEAD~3", "HEAD"], Shape::Attributes)
            .with_config(&[("core.whitespace", "-tab-in-indent")]),
    );
}

// ---------------------------------------------------------------------------
// 11. `archive` as a consumer
// ---------------------------------------------------------------------------

/// `export-ignore` removes an entry from the stream, and the entry it removes is
/// decided by the same matcher.
///
/// The fixture sets `*.md export-ignore`, so `archive` on `Shape::Attributes`
/// already omits `docs/manual.md` and every existing case sees that. What no
/// case could do is aim the rule somewhere else and watch a *different* entry
/// leave. Observed on stock 2.55.0 with `.gitmodules` = `src/tabs.rs
/// export-ignore` and `core.attributesFile` naming it, `archive --format=tar
/// HEAD | tar -t` lists everything except `src/tabs.rs` — and without the
/// setting, `src/tabs.rs` is present. The tar goes to stdout, where
/// `runner::normalize` compares it byte for byte, so the missing entry, the
/// shifted offsets and the changed size are all in the comparison.
///
/// This is also the case that proves `archive` consults `core.attributesFile` at
/// all, which the module header depends on for its `export-subst` finding.
fn archive_consumer(out: &mut Vec<Case>) {
    rules(out, "archive", &["archive", "--format=tar", "HEAD"], "src/tabs.rs export-ignore");
}

// ---------------------------------------------------------------------------
// 12. `filter=<driver>` as a consumer
// ---------------------------------------------------------------------------

/// The payload for the clean-side cases: lower-case so an upper-casing filter is
/// visible in the resulting object id, and two lines so a filter that eats the
/// last newline is visible too.
const LOWER_TWO_LINES: &[u8] = b"hello world\nsecond line\n";

/// `attributes_filters.rs` records that `filter.<n>.clean`/`.smudge`/`.required`
/// are **unreachable**, because "an external filter driver runs only for a path
/// whose `filter` attribute names it, and no `.gitattributes` in any shape sets
/// `filter=` at all", and pins the *inertness* of a configured-but-unselected
/// driver instead. The lever removes that wall. What is added here is only the
/// **routing** — that the matcher's answer is what selects the driver — because
/// the drivers themselves are that module's subject.
///
/// Observed on stock 2.55.0, `-c filter.up.clean='tr a-z A-Z'`, stdin
/// [`LOWER_TWO_LINES`]:
///
/// | `.gitmodules` | `hash-object --path=a.txt --stdin` |
/// |---|---|
/// | `a.txt filter=up` | `294b14bc836ed2568e7e8a7881060c833a393278` |
/// | `b.txt filter=up` | `f0e9ea9cdcd1c7d022373365088a65e94c5ab13e` |
///
/// One pattern character apart, two different object ids: the second is the
/// unfiltered blob, and it is the control that says the first id is the filter
/// running rather than `hash-object` being wrong. `hash-object` without `-w`
/// prints the id without storing it, so the comparison is pure stdout.
///
/// The smudge side is read through `cat-file --filters`, which runs the
/// worktree-ward half and prints it: with `src/tabs.rs filter=up` and
/// `filter.up.smudge='tr a-z A-Z'`, `cat-file --filters HEAD:src/tabs.rs` prints
/// `FN INDENTED() {` where the stored blob is lower-case.
///
/// The last case is `.required`: a clean filter that exits non-zero is a fatal
/// error rather than a fallback, and stock exits 128 with an empty stdout.
fn filter_consumer(out: &mut Vec<Case>) {
    out.push(
        Case::with_stdin(
            "hash-object",
            &["hash-object", "--path=a.txt", "--stdin"],
            Shape::Attributes,
            LOWER_TWO_LINES,
        )
        .with_scoped_config(attr_file_with("a.txt filter=up", &[("filter.up.clean", "tr a-z A-Z")])),
    );
    out.push(
        Case::with_stdin(
            "hash-object",
            &["hash-object", "--path=a.txt", "--stdin"],
            Shape::Attributes,
            LOWER_TWO_LINES,
        )
        .with_scoped_config(attr_file_with("b.txt filter=up", &[("filter.up.clean", "tr a-z A-Z")])),
    );
    out.push(
        Case::new("cat-file", &["cat-file", "--filters", "HEAD:src/tabs.rs"], Shape::Attributes)
            .with_scoped_config(attr_file_with(
                "src/tabs.rs filter=up",
                &[("filter.up.smudge", "tr a-z A-Z")],
            )),
    );
    out.push(
        Case::with_stdin(
            "hash-object",
            &["hash-object", "--path=a.txt", "--stdin"],
            Shape::Attributes,
            LOWER_TWO_LINES,
        )
        .with_scoped_config(attr_file_with(
            "a.txt filter=no",
            &[("filter.no.clean", "false"), ("filter.no.required", "true")],
        )),
    );
}

// ---------------------------------------------------------------------------
// 13. `working-tree-encoding` and `text`/`eol` as consumers
// ---------------------------------------------------------------------------

/// `eol_conversion.rs` records that `working-tree-encoding` is **unreachable**
/// and that "no case here pretends otherwise". It is reachable now. The
/// encoding *matrix* is that module's subject; what is here is the two answers
/// that show the matcher reaching `convert.c` at all.
///
/// Observed on stock 2.55.0, `cat-file --filters HEAD:src/tabs.rs`:
///
/// | `.gitmodules` | stock | zvcs |
/// |---|---|---|
/// | `src/tabs.rs working-tree-encoding=UTF-16` | UTF-16 with a BOM: `ff fe 66 00 6e 00 …` | the stored UTF-8 bytes, unchanged |
/// | `src/tabs.rs working-tree-encoding=NOSUCHENC` | the file unchanged, rc **0**, `error: failed to encode …` on stderr | empty stdout, rc **1** |
///
/// The second row is the sharper of the two: a *refused* encoding is not a fatal
/// error in git — the content is emitted as it stands and the failure is a
/// diagnostic — and a port that treats it as fatal loses the file.
///
/// `ls-files --eol` is the readout `eol_conversion.rs` uses, and one case here
/// carries a config pair it does not: a lever-supplied `text eol=crlf` on
/// `docs/manual.md` that the root file's `* text=auto` **overrides for `text`
/// but not for `eol`**, so the attribute column reads `attr/text=auto eol=crlf`
/// — one path whose two attributes come from two different files.
fn encoding_consumer(out: &mut Vec<Case>) {
    rules(
        out,
        "cat-file",
        &["cat-file", "--filters", "HEAD:src/tabs.rs"],
        "src/tabs.rs working-tree-encoding=UTF-16",
    );
    rules(
        out,
        "cat-file",
        &["cat-file", "--filters", "HEAD:src/tabs.rs"],
        "src/tabs.rs working-tree-encoding=NOSUCHENC",
    );
    rules(
        out,
        "ls-files",
        &["ls-files", "--eol", "docs/manual.md", "src/tabs.rs"],
        "docs/manual.md text eol=crlf",
    );
}

// ---------------------------------------------------------------------------
// 14. `merge` as a consumer
// ---------------------------------------------------------------------------

/// One case against [`Shape::CrissCross`] with a synthetic attribute file.
///
/// `.gitmodules` is untracked in that shape, so writing it changes nothing the
/// merge sees except through `core.attributesFile`.
fn cc_rules(out: &mut Vec<Case>, args: &[&str], body: &'static str) {
    out.push(
        Case::new("merge-tree", args, Shape::CrissCross).with_scoped_config(attr_file(body)),
    );
}

/// Three attributes reach `ll_merge`, and `merge-tree --write-tree` puts all
/// three answers on stdout: the tree id changes when the merged content changes,
/// and a conflicted path is listed with its three stage ids.
///
/// `attributes_filters.rs` owns `merge.<n>.driver` — a driver that is a
/// *program*. `merge_family.rs` owns the default three-way text merge. Neither
/// could set the attribute that selects a built-in driver or sizes a marker,
/// because no shape ships the rule; the lever supplies it.
///
/// Measured by hand on a rebuilt copy of [`Shape::CrissCross`]:
///
/// | `.gitmodules` | stock 2.55.0 tree | zvcs tree |
/// |---|---|---|
/// | *(none)* | `fad5c257e54478e70a84a0e35a979b30eba1142c`, rc 1 | same |
/// | `clash.txt conflict-marker-size=13` | `36a74ccd…`, stage 1 = `f47d0929…` | `36a74ccd…`, stage 1 = **`bbe5012a…`** |
/// | `clash.txt -merge` | `e16091178a5e…` **plus** `warning: Cannot merge binary files: clash.txt (cc-left vs. cc-right)` | the same tree, **no warning line** |
/// | `clash.txt merge=union` | `84b5258297ff1861cc4898cd27aafc7d183572b7`, rc **0** | same |
///
/// Row two is a defect with a precise shape. `cc-left`/`cc-right` is a
/// criss-cross, so the merge builds a **virtual base** by merging the two real
/// bases, and `ll_merge` adds two characters to the marker size for that inner
/// merge so the nesting is readable. Dumping the stage-1 blobs:
///
/// ```text
/// stock  <<<<<<<<<<<<<<< Temporary merge branch 1   (15 = 13 + 2)
/// zvcs   <<<<<<<<<<<<< Temporary merge branch 1     (13, no bump)
/// ```
///
/// and with **no** attribute both sides write 9 (= 7 + 2), so the bump is
/// implemented and is simply not applied when the attribute supplies the size.
/// Row three is a plain omission: `-merge` selects the binary driver, which
/// stock announces on stdout.
///
/// Row four is the control that says these are attribute lookups and not merge
/// bugs: `merge=union` resolves the same conflict cleanly on both sides, tree
/// and exit code identical.
fn merge_consumer(out: &mut Vec<Case>) {
    const ARGS: &[&str] = &["merge-tree", "--write-tree", "cc-left", "cc-right"];
    // The no-attribute control is `fixture_gaps.rs`'s case, not a second copy
    // of it here: the corpus already runs this exact argv on this exact shape.
    cc_rules(out, ARGS, "clash.txt conflict-marker-size=13");
    cc_rules(out, ARGS, "clash.txt -merge");
    cc_rules(out, ARGS, "clash.txt merge=union");
    // The same lookup on a path the merge resolves cleanly: a marker size that
    // is never used must not change the tree.
    cc_rules(out, ARGS, "calm.txt conflict-marker-size=13");
}
