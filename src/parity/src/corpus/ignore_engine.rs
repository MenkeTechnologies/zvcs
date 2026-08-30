//! Differential corpus cases for the **exclude engine itself** — `dir.c`'s
//! pattern parser and matcher, and `builtin/check-ignore.c`, the one command
//! whose entire output *is* that engine's answer.
//!
//! Every case here is compared against stock git for stdout, exit code and
//! post-command repository state; the argument-validation cases compare stderr
//! too, because for those the message is the whole behaviour.
//!
//! # Territory, against the neighbours that share the subject
//!
//! Read in full before writing any of this: `gitignore_precedence.rs` (the
//! nearest neighbour), `add_rm_mv_clean.rs`, `info_attrs.rs`, `pathspec_stdin.rs`,
//! `status_formats.rs`, `index_plumbing.rs`, `grep_engine.rs`, plus the
//! `check-ignore` lines in `shape_reach.rs`, `exit_codes.rs` and
//! `misc_commands.rs`.
//!
//! | module | what it owns |
//! |--------|--------------|
//! | `gitignore_precedence.rs` | *which source wins*: adjacent pairs of the four sources in both senses, depth ordering inside the `.gitignore` group, the excluded-directory wall through four verbs, `core.excludesFile` pointed at each class of **existing fixture** file, skip-worktree, and the pattern grammar delivered through `-x` (the `EXC_CMDL` route, which deliberately **bypasses** the file parser) |
//! | `shape_reach.rs` | one `check-ignore` case per *rule form already in the fixture*, against the path that rule was written for — the single-rule answers |
//! | `info_attrs.rs` | `check-ignore`'s flags and framing on shapes with **no rules at all**: `-q`, `-z`, `--stdin`, `-n`, `--no-index`, the `path: pattern` triple |
//! | `exit_codes.rs` / `misc_commands.rs` | three strict exit-code pins (`README.md`, `--quiet nosuch`, `--index src/lib.rs`) |
//! | `pathspec_stdin.rs` | `check-ignore` reached through *pathspec magic* (`:!`, `:(glob)`, `:(top)`) and the awkward-name stdin payloads |
//! | `index_plumbing.rs` | `ls-files`'s `-o`/`-i`/`--directory` **selection**, and `--exclude=`, `--exclude-from=.gitignore`, `--exclude-per-directory=.gitignore` |
//! | `add_rm_mv_clean.rs` | `clean`'s `-d`/`-x`/`-X`/`-e` sets, the force levels, and `add`'s refusal on the four ignored paths |
//! | `status_formats.rs` | `--ignored=…` crossed with the *untracked* mode |
//! | `grep_engine.rs` | `grep`'s five boolean spellings — and it deliberately left the four `--untracked`/`--no-exclude-standard` argv shapes on [`Shape::Attributes`] to `gitignore_precedence.rs` rather than double-filing them. This module does the same and adds **no** `grep --untracked` case on that shape |
//!
//! # What none of them has, and what is here
//!
//! **1. `check-ignore`'s argument contract.** Every one of the six ways
//! `builtin/check-ignore.c` dies before it looks at a pattern — `--quiet` with
//! `--verbose`, `--non-matching` without `--verbose`, `--quiet` with more than
//! one pathname, `-z` without `--stdin`, `--stdin` with pathnames, and no
//! pathname at all — plus the empty-pathspec death and the outside-repository
//! death. Five of the eight had no case anywhere, and the three that did were
//! only pinned on `Shape::Linear` or without stderr. They are the cheapest thing
//! in git to get *nearly* right: a port that accepts `-q -v` and prints
//! something is wrong in a way no matching test can see.
//!
//! **2. The whole 0/1/128 contract, and the path forms that feed it.**
//! `check-ignore` inverts its polarity twice — 0 means *some* path matched, 1
//! means none did, and under `-n` a match is still a match even when the
//! matching rule is a negation. Here: a directory argument, a directory argument
//! written with a trailing `/`, `./`-prefixed, doubled-slash and `/./` forms, a
//! path that does not exist, `.`, `.git`, a tracked path with and without
//! `--index`, and multi-path invocations where the order of the answers is the
//! thing being compared.
//!
//! **3. `--stdin` as a *parser*.** Not "does `--stdin` work" — `info_attrs.rs`
//! has that — but what the reader does with bytes that do not match the
//! terminator it was told to expect. A NUL-separated payload read in LF mode
//! becomes **one** path; an LF-separated payload read under `-z` becomes one
//! path too; a `"`-quoted line is C-unquoted; a CRLF payload keeps its `\r` and
//! the echo comes back C-quoted. Four separate code paths in
//! `check_ignore_stdin_paths`, none of them previously measured on a shape that
//! has rules.
//!
//! **4. The ignore-file parser, with the file's bytes chosen by the case.**
//! This is the part `gitignore_precedence.rs` states it cannot reach and does
//! not attempt: "the file-parser rules … are only reachable through whatever
//! comment and blank lines a fixture file already has". A case cannot create a
//! file — but [`ConfigEntry::raw`] on [`ConfigScope::Modules`] writes arbitrary
//! bytes to `.gitmodules` at a path the case can name, and `.gitmodules` is read
//! as configuration only by `submodule-config.c`, for `submodule.*` keys, and
//! never by any verb here. Pointing `core.excludesFile` (or `--exclude-from=`,
//! or `--exclude-per-directory=`) at it turns those bytes into an ignore file
//! whose **line numbers `check-ignore -v` reports back**. That makes the entire
//! grammar measurable for the first time, through `add_patterns_from_buffer`
//! rather than `add_pattern`:
//!
//! * anchoring (`/x`, `a/b`, bare `x`), directory-only (`dir/`);
//! * `*`, `?`, `[a-c]`, `[!a-c]`, `[^a-c]`, `[0-9]`;
//! * `**/` leading, `/**` trailing, `a/**/b` middle, `a**b` (not a `**` at all),
//!   bare `**`;
//! * a `#` comment, a blank line, an escaped `\#`, an escaped `\!`, trailing
//!   spaces stripped, a trailing space kept by `\ `, **leading** spaces kept, a
//!   trailing `\\`, a trailing tab **not** stripped, a `\r` from CRLF stripped;
//! * a whole file in CRLF;
//! * negation ordering and the excluded-directory wall *within one file*, where
//!   the reported line number says which of two rules the engine actually used.
//!
//! **5. The check-ignore/verb cross-check on identical bytes.** The same
//! synthetic file is read by `check-ignore -v`, by `ls-files -o -i`, by
//! `status --ignored`, by `clean -ndX` and by `add -n`, so a port that reports
//! one winning rule and then selects a different set fails the pair rather than
//! passing both halves separately.
//!
//! # What is not measurable here, and why
//!
//! * **`-v` on a `--exclude-from=`/`-x` rule.** `check-ignore` has neither
//!   option, so the *source name* a command-line pattern reports cannot be read
//!   out of any command. Only the `core.excludesFile` and per-directory routes
//!   name a file, and those are what the parser group uses.
//! * **The outside-repository message.** `fatal: … is outside repository at
//!   '<abs>'` embeds the fixture root, which differs between the two sides by
//!   construction. Those two cases are therefore **not** [`Case::strict`]: only
//!   the empty stdout and the 128 are compared. Making them strict would be
//!   comparing the temporary directory names.
//! * **A `\r`- or tab-bearing pathname as argv.** A case id is one line of the
//!   report; a literal CR or TAB inside an argv token would break that framing.
//!   The `\r`-stripping and tab-not-stripping rules are therefore measured from
//!   the *pattern* side (`crlf.dat` matches, `tabtrail.dat` does not), and the
//!   `\r`-bearing *path* side is measured through `--stdin`, whose payload is a
//!   byte literal and whose echo git C-quotes for us.
//! * **A file with no trailing newline.** `runner::render_config_entry` appends
//!   `\n` to every raw entry, so an ignore file whose last line is unterminated
//!   cannot be written. Nothing here depends on it.
//! * **`$GIT_DIR/info/exclude` with chosen bytes.** No scope writes it, and
//!   `gitignore_precedence.rs` already records that its one fixture pattern
//!   leaves two adjacent source pairs unreachable. Unchanged here.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    argument_contract(out);
    exit_contract(out);
    path_forms(out);
    stdin_reader(out);
    file_parser(out);
    pattern_grammar(out);
    one_file_many_doors(out);
}

/// Shorthand for one case against [`Shape::Attributes`].
fn at(out: &mut Vec<Case>, cmd: &'static str, args: &[&str]) {
    out.push(Case::new(cmd, args, Shape::Attributes));
}

/// Shorthand for one stderr-compared case against [`Shape::Attributes`].
fn at_strict(out: &mut Vec<Case>, cmd: &'static str, args: &[&str]) {
    out.push(Case::strict(cmd, args, Shape::Attributes));
}

/// `.gitmodules` holding `body`, named as `core.excludesFile`.
///
/// The raw entry is written verbatim by `runner::install_config`, which appends
/// exactly one `\n` — so `body` is the file minus its final newline, and every
/// line number `check-ignore -v` reports is an index into this literal.
fn excludes(body: &'static str) -> Vec<ConfigEntry> {
    vec![
        ConfigEntry::raw(ConfigScope::Modules, body),
        ConfigEntry::set(ConfigScope::CommandLine, "core.excludesFile", ".gitmodules"),
    ]
}

/// One case against [`Shape::Attributes`] whose only exclude source with any
/// bearing on the subjects is the synthetic `.gitmodules`.
fn rules(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], body: &'static str) {
    out.push(Case::new(cmd, args, Shape::Attributes).with_scoped_config(excludes(body)));
}

// ---------------------------------------------------------------------------
// 1. The argument contract: every way `check-ignore` dies before it matches
// ---------------------------------------------------------------------------

/// `builtin/check-ignore.c` validates its options before it opens a single
/// pattern file, and each refusal is a distinct message and a 128.
///
/// Observed against stock git 2.55.0 on a copy of [`Shape::Attributes`]:
///
/// | invocation | stderr | exit |
/// |------------|--------|------|
/// | `check-ignore -q -v build/output.o` | `fatal: cannot have both --quiet and --verbose` | 128 |
/// | `check-ignore -n build/output.o` | `fatal: --non-matching is only valid with --verbose` | 128 |
/// | `check-ignore -q -n important.log` | `fatal: --non-matching is only valid with --verbose` | 128 |
/// | `check-ignore -q build/output.o important.log` | `fatal: --quiet is only valid with a single pathname` | 128 |
/// | `check-ignore -z build/output.o` | `fatal: -z only makes sense with --stdin` | 128 |
/// | `check-ignore --stdin build/output.o` | `fatal: cannot specify pathnames with --stdin` | 128 |
/// | `check-ignore` | `fatal: no path specified` | 128 |
/// | `check-ignore -v ''` | `fatal: empty string is not a valid pathspec…` | 128 |
///
/// The third row is the ordering fact: `-q -n` reports the `--non-matching`
/// conflict, not the `--quiet` one, so the checks are not commutative and a port
/// that validates in its own order agrees on the exit code and disagrees on the
/// text. The fourth is the one most easily missed — `--quiet` is legal with
/// `--stdin` (argc is zero there) and illegal with two pathnames, which is a
/// count check rather than a flag check.
///
/// The two outside-repository refusals are deliberately **not** strict; see the
/// module header. Their stdout is empty and their exit is 128 on both sides,
/// which is all that can be compared when the message names the fixture root.
fn argument_contract(out: &mut Vec<Case>) {
    at_strict(out, "check-ignore", &["check-ignore", "-q", "-v", "build/output.o"]);
    at_strict(out, "check-ignore", &["check-ignore", "--quiet", "--verbose", "build/output.o"]);
    at_strict(out, "check-ignore", &["check-ignore", "-n", "build/output.o"]);
    at_strict(out, "check-ignore", &["check-ignore", "-q", "-n", "important.log"]);
    at_strict(out, "check-ignore", &["check-ignore", "-q", "build/output.o", "important.log"]);
    at_strict(out, "check-ignore", &["check-ignore", "-z", "build/output.o"]);
    at_strict(out, "check-ignore", &["check-ignore", "--stdin", "build/output.o"]);
    at_strict(out, "check-ignore", &["check-ignore"]);
    at_strict(out, "check-ignore", &["check-ignore", "-v", ""]);

    // `--no-index` and `--index` are not exclusive; the last one parsed wins and
    // the invocation succeeds. The control that says the pair above is a real
    // check rather than "two flags at once is always fatal".
    at(out, "check-ignore", &["check-ignore", "--no-index", "--index", "build/output.o"]);
    at(out, "check-ignore", &["check-ignore", "--index", "--no-index", "logs/keep.log"]);

    // Outside the repository: 128, empty stdout, a message naming the root.
    at(out, "check-ignore", &["check-ignore", "-v", "../elsewhere.txt"]);
    out.push(Case::new("check-ignore", &["check-ignore", "-v", "../../elsewhere.txt"], Shape::Attributes).in_dir("sub"));
}

// ---------------------------------------------------------------------------
// 2. 0 = something matched, 1 = nothing did
// ---------------------------------------------------------------------------

/// The exit code is a property of the *set* of paths, not of the last one, and
/// `-n` does not change it.
///
/// Observed:
///
/// | invocation | stdout | exit |
/// |------------|--------|------|
/// | `check-ignore -v build build/output.o notes.tmp tracked-looking.txt` | three lines, `tracked-looking.txt` absent | 0 |
/// | `… -n …` | four lines, `::\ttracked-looking.txt` last | 0 |
/// | `check-ignore -n -v tracked-looking.txt src/tabs.rs` | two `::` lines | **1** |
/// | `check-ignore -v sub sub/local-scratch.txt` | one line, `sub` absent | 0 |
/// | `check-ignore --index -v logs/keep.log` | empty | 1 |
///
/// The third row is the polarity trap: `-n` prints a line for every path, so a
/// port that derives the exit code from "did I print anything" returns 0 where
/// stock returns 1. The last row is the index veto — `logs/keep.log` is tracked
/// *and* matched by `*.log`, and being tracked removes it from the answer, which
/// `--index` (the default) states explicitly and `--no-index` reverses.
///
/// `check-ignore -v sub` reports nothing at all: `sub` is a real directory, no
/// rule names it, and `sub/.gitignore` lives *inside* it and cannot claim it.
fn exit_contract(out: &mut Vec<Case>) {
    at(out, "check-ignore", &["check-ignore", "-v", "build", "build/output.o", "notes.tmp", "tracked-looking.txt"]);
    at(out, "check-ignore", &["check-ignore", "-n", "-v", "build", "build/output.o", "notes.tmp", "tracked-looking.txt"]);
    at(out, "check-ignore", &["check-ignore", "-n", "-v", "tracked-looking.txt", "src/tabs.rs"]);
    at(out, "check-ignore", &["check-ignore", "-v", "sub", "sub/local-scratch.txt"]);
    at(out, "check-ignore", &["check-ignore", "--index", "-v", "logs/keep.log"]);
    at(out, "check-ignore", &["check-ignore", "--index", "-v", "-n", "logs/keep.log"]);

    // `-q` short-circuits: one path, no output, exit is the whole answer. Both
    // senses, and the negated-match sense, which is a match.
    at(out, "check-ignore", &["check-ignore", "-q", "notes.tmp"]);
    at(out, "check-ignore", &["check-ignore", "-q", "tracked-looking.txt"]);
    at(out, "check-ignore", &["check-ignore", "-q", "logs/keep.log"]);
    at(out, "check-ignore", &["check-ignore", "--quiet", "sub/local-scratch.txt"]);

    // Repeating one path repeats the answer rather than collapsing it.
    at(out, "check-ignore", &["check-ignore", "-v", "notes.tmp", "notes.tmp"]);
    at(out, "check-ignore", &["check-ignore", "-v", "-n", "tracked-looking.txt", "tracked-looking.txt"]);
}

// ---------------------------------------------------------------------------
// 3. Path forms: what the subject may look like, and how it is echoed back
// ---------------------------------------------------------------------------

/// The subject is echoed **exactly as typed** while the winning file is named
/// from the repository root, so every spelling of one path is a separate
/// observation.
///
/// Observed:
///
/// | argument | stdout | exit |
/// |----------|--------|------|
/// | `build/` | `.gitignore:3:build/\tbuild/` | 0 |
/// | `./build/output.o` | `.gitignore:3:build/\t./build/output.o` | 0 |
/// | `build//output.o` | `.gitignore:3:build/\tbuild//output.o` | 0 |
/// | `build/./output.o` | `.gitignore:3:build/\tbuild/./output.o` | 0 |
/// | `nonexistent-dir/nonexistent.log` | `.gitignore:1:*.log\t…` | 0 |
/// | `.` | — | 1 |
/// | `logs` | — | 1 |
/// | `.gitignore` | — | 1 |
/// | `.git` | — | 1 |
/// | `sub/` | `::\tsub/` | 1 |
/// | `sub/deep-ignored/` | `.gitignore:5:**/deep-ignored/\tsub/deep-ignored/` | 0 |
/// | `-- build/output.o` | `.gitignore:3:build/\tbuild/output.o` | 0 |
///
/// The doubled-slash and `/./` rows are the ones a port normalises away and then
/// echoes normalised; git does not touch the string it prints. `.git` answering
/// 1 is the statement that the exclude engine has no opinion about the
/// repository directory — it is skipped by the *walk*, not by a pattern, so
/// nothing here reports it. `sub/` against `sub/deep-ignored/` is the trailing
/// slash carrying no weight of its own: both are real directories and the
/// difference is only whether a rule names one.
///
/// The last four run from `logs/` and `sub/`, where the same rules are still
/// named `.gitignore:1`, `.gitignore:3` and `.gitignore:5` — the file is
/// repository-relative while the subject stays as typed, and `deep-ignored`
/// typed without a trailing slash still answers with the `**/deep-ignored/`
/// rule because the name is a directory on disk.
fn path_forms(out: &mut Vec<Case>) {
    for arg in [
        "build/",
        "./build/output.o",
        "build//output.o",
        "build/./output.o",
        "nonexistent-dir/nonexistent.log",
        ".",
        "logs",
        ".gitignore",
        ".git",
        "sub/",
        "sub/deep-ignored/",
    ] {
        out.push(Case::new("check-ignore", &["check-ignore", "-v", "-n", arg], Shape::Attributes));
    }
    at(out, "check-ignore", &["check-ignore", "-v", "--", "build/output.o"]);
    at(out, "check-ignore", &["check-ignore", "-v", "-n", "--", "tracked-looking.txt"]);

    // Below the toplevel: the subject stays as typed, the source is named from
    // the root, and a `..` that is still inside the repository is legal.
    out.push(Case::new("check-ignore", &["check-ignore", "-v", "./debug.log"], Shape::Attributes).in_dir("logs"));
    out.push(Case::new("check-ignore", &["check-ignore", "-v", "../build/output.o"], Shape::Attributes).in_dir("logs"));
    out.push(Case::new("check-ignore", &["check-ignore", "-v", "-n", "../.gitignore"], Shape::Attributes).in_dir("sub"));
    out.push(Case::new("check-ignore", &["check-ignore", "-v", "-n", "deep-ignored"], Shape::Attributes).in_dir("sub"));
}

// ---------------------------------------------------------------------------
// 4. `--stdin` as a reader: what happens when the terminator is wrong
// ---------------------------------------------------------------------------

/// Paths, one per line under `--stdin` and one per NUL under `-z --stdin`. Every
/// payload is a `&'static [u8]` literal, so the bytes are the case.
///
/// Observed on [`Shape::Attributes`]:
///
/// | payload | mode | result |
/// |---------|------|--------|
/// | six LF paths | `--stdin -v -n` | six answers, exit 0 |
/// | six LF paths | `--stdin` | `build/output.o`, `notes.tmp`, exit 0 |
/// | four NUL paths | `-z --stdin -v -n` | four NUL-field records, exit 0 |
/// | four NUL paths | `-z --stdin` | two NUL-terminated names |
/// | four NUL paths | `--stdin -v -n` | **one** answer — the whole blob is one path, printed up to its first NUL |
/// | six LF paths | `-z --stdin -v -n` | **one** answer — the whole blob is one path, echoed with its newlines |
/// | `"build/output.o"` | `--stdin -v -n` | C-unquoted to `build/output.o` |
/// | CRLF paths | `--stdin -v -n` | `"build/output.o\r"` — the CR stays in the path and forces C-quoting on the echo |
/// | empty | `--stdin -v -n` | nothing, exit **1** |
///
/// Rows five and six are the discriminating pair. Both are "the reader was given
/// the other terminator", and in both cases git reads exactly **one** path — the
/// entire blob — and matches it. Row five still matches `build/`, because the
/// blob *begins* `build/output.o`, and prints only as far as the first NUL; row
/// six matches for the same reason and echoes the newlines. A port that splits on
/// both terminators regardless of the flag answers six times instead of once.
///
/// Row eight is why a CRLF-terminated payload is not a way of writing an
/// LF-terminated one: `notes.tmp\r` does not match `/notes.tmp`, so the same
/// file list produces a different answer purely because of the line endings.
fn stdin_reader(out: &mut Vec<Case>) {
    /// Six paths, LF-terminated: ignored by a directory rule, un-ignored by a
    /// negation, matched by nothing, negated by a nested file, tracked-and-
    /// matched, and anchored.
    const LF: &[u8] =
        b"build/output.o\nimportant.log\ntracked-looking.txt\nsub/hypothetical.log\nlogs/keep.log\nnotes.tmp\n";
    /// The same idea, NUL-terminated.
    const NUL: &[u8] = b"build/output.o\0important.log\0sub/local-scratch.txt\0tracked-looking.txt\0";
    /// C-quoted lines, which the LF reader unquotes.
    const QUOTED: &[u8] = b"\"build/output.o\"\n\"tracked-looking.txt\"\n";
    /// CRLF, whose `\r` becomes part of each pathname.
    const CRLF: &[u8] = b"build/output.o\r\nnotes.tmp\r\nimportant.log\r\n";
    /// Nothing at all.
    const EMPTY: &[u8] = b"";

    let at = Shape::Attributes;
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "--stdin", "-v", "-n"], at, LF));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "--stdin"], at, LF));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "--stdin", "-v"], at, LF));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "--stdin", "--no-index", "-v", "-n"], at, LF));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "-z", "--stdin", "-v", "-n"], at, NUL));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "-z", "--stdin"], at, NUL));
    // The two terminator mismatches.
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "--stdin", "-v", "-n"], at, NUL));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "-z", "--stdin", "-v", "-n"], at, LF));
    // Quoting and line endings.
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "--stdin", "-v", "-n"], at, QUOTED));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "--stdin", "-v", "-n"], at, CRLF));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "-z", "--stdin", "-v", "-n"], at, CRLF));
    // Empty input, and `--quiet` in both senses — legal with `--stdin` because
    // the pathname count that `--quiet` restricts is the argv one.
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "--stdin", "-v", "-n"], at, EMPTY));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "--stdin"], at, EMPTY));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "-q", "--stdin"], at, LF));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "-q", "--stdin"], at, b"tracked-looking.txt\n"));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "-q", "--stdin"], at, EMPTY));
}

// ---------------------------------------------------------------------------
// 5. The ignore-file parser, byte by byte
// ---------------------------------------------------------------------------

/// `dir.c:add_patterns_from_buffer` — the code path `-x` skips — measured by
/// writing an exact file and reading the line numbers back out of
/// `check-ignore -v`.
///
/// The file is `.gitmodules`, written verbatim by `runner::install_config` from
/// a raw [`ConfigScope::Modules`] entry and named by `core.excludesFile`; see
/// the module header for why that is a legal ignore file and an inert config
/// one. Subjects are invented names no fixture rule mentions, so the synthetic
/// file is the only source with an opinion and every answer is a direct readout.
///
/// The file, with its line numbers:
///
/// ```text
/// 1  # comment.dat
/// 2
/// 3  \#hash.dat
/// 4  \!bang.dat
/// 5  trailsp.dat␠␠␠
/// 6  keepsp.dat\␠
/// 7  ␠␠leadsp.dat
/// 8  back.dat\\
/// 9  crlf.dat␍
/// 10 tabtrail.dat␉
/// ```
///
/// Observed, `check-ignore -v -n` with `core.excludesFile=.gitmodules`:
///
/// | subject | answer |
/// |---------|--------|
/// | `comment.dat` | `::` — line 1 is a comment |
/// | `#hash.dat` | `.gitmodules:3:\#hash.dat` |
/// | `hash.dat` | `::` — the `\#` is a literal `#`, not an escape that vanishes |
/// | `!bang.dat` | `.gitmodules:4:\!bang.dat` |
/// | `bang.dat` | `::` |
/// | `trailsp.dat` | `.gitmodules:5:trailsp.dat` — trailing spaces stripped, and the *reported pattern* is stripped too |
/// | `trailsp.dat␠␠␠` | `::` |
/// | `keepsp.dat` | `::` |
/// | `keepsp.dat␠` | ``.gitmodules:6:keepsp.dat\␠`` — `\ ` keeps one space, and the pattern is echoed with its backslash |
/// | `leadsp.dat` | `::` |
/// | `␠␠leadsp.dat` | `.gitmodules:7:␠␠leadsp.dat` — **leading** whitespace is not stripped |
/// | `back.dat` | `::` |
/// | `back.dat\` | `.gitmodules:8:back.dat\\` , subject echoed C-quoted |
/// | `crlf.dat` | `.gitmodules:9:crlf.dat` — the `\r` was stripped with the trailing whitespace |
/// | `tabtrail.dat` | `::` — a trailing **tab** is *not* stripped |
///
/// The last two rows together are the sharp pair: `\r` goes and `\t` stays, so
/// "strip trailing whitespace" is the wrong rule and a port that uses its
/// language's `trim_end` matches `tabtrail.dat` and diverges. The `keepsp` and
/// `leadsp` rows are the other asymmetry — the escape is honoured only at the
/// end, and only for a space.
///
/// Line 2 is blank and still counts: every line number above is an index into
/// the file including the comment and the blank, so a port that filters before
/// numbering reports the right pattern under the wrong line.
const PARSER_FILE: &str = "# comment.dat\n\
                           \n\
                           \\#hash.dat\n\
                           \\!bang.dat\n\
                           trailsp.dat   \n\
                           keepsp.dat\\ \n\
                           \x20\x20leadsp.dat\n\
                           back.dat\\\\\n\
                           crlf.dat\r\n\
                           tabtrail.dat\t";

/// A whole ignore file in CRLF, including its comment and its blank line.
///
/// Observed: `x.crlf` → `.gitmodules:3:*.crlf`, `keep.crlf` →
/// `.gitmodules:4:!keep.crlf`, `d/x.crlf` → `.gitmodules:3:*.crlf`. Every
/// pattern is reported without its `\r`, the `\r\n`-only line 2 is blank, and
/// line 1 is a comment despite ending in `\r`. A port that reads the file with a
/// CRLF-naive splitter gets patterns that end in `\r` and matches nothing at all
/// while still reporting the same line numbers under `-n`.
const CRLF_FILE: &str = "# crlf comment\r\n\r\n*.crlf\r\n!keep.crlf\r";

/// The same three parse rules, aimed at paths the fixture really has, so the
/// verbs can be asked what `check-ignore` was asked.
///
/// ```text
/// 1  # tracked-looking.txt
/// 2
/// 3  tracked-looking.txt␠␠
/// 4  \#nothing.txt
/// 5  excluded-by-info.txt\␠
/// ```
///
/// Line 1 names the subject and must do nothing (comment). Line 3 names it
/// again with two trailing spaces and must match (stripped). Line 5 names a
/// second real path with an escaped trailing space, so the pattern is
/// `excluded-by-info.txt␠` and must **not** match the file, which has no such
/// name — that path stays claimed by `.git/info/exclude`.
///
/// Observed with `core.excludesFile=.gitmodules`:
///
/// | invocation | result |
/// |------------|--------|
/// | `check-ignore -v -n tracked-looking.txt` | `.gitmodules:3:tracked-looking.txt` |
/// | `check-ignore -v -n excluded-by-info.txt` | `.git/info/exclude:1:excluded-by-info.txt` |
/// | `check-ignore -v -n important.log` | `.gitignore:2:!important.log` |
/// | `check-ignore -v -n '#nothing.txt'` | `.gitmodules:4:\#nothing.txt` |
/// | `ls-files -o --exclude-standard` | `.gitmodules`, `important.log` |
/// | `status --porcelain --ignored` | `!! tracked-looking.txt` joins the six |
///
/// Three different mistakes produce three different reported *lines* for the
/// same outcome: honour line 1 and it is `:1:`, keep the trailing spaces on line
/// 3 and it is `::`, drop the escape on line 5 and `excluded-by-info.txt`
/// changes source. Only a parser that gets all three right prints the table
/// above, and the `ls-files`/`status` rows say the same answer reached the walk.
const PARSER_BITE: &str =
    "# tracked-looking.txt\n\ntracked-looking.txt  \n\\#nothing.txt\nexcluded-by-info.txt\\ ";

fn file_parser(out: &mut Vec<Case>) {
    rules(
        out,
        "check-ignore",
        &[
            "check-ignore",
            "-v",
            "-n",
            "comment.dat",
            "#hash.dat",
            "hash.dat",
            "!bang.dat",
            "bang.dat",
        ],
        PARSER_FILE,
    );
    rules(
        out,
        "check-ignore",
        &[
            "check-ignore",
            "-v",
            "-n",
            "trailsp.dat",
            "trailsp.dat   ",
            "keepsp.dat",
            "keepsp.dat ",
            "leadsp.dat",
            "  leadsp.dat",
        ],
        PARSER_FILE,
    );
    rules(
        out,
        "check-ignore",
        &["check-ignore", "-v", "-n", "back.dat", "back.dat\\", "crlf.dat", "tabtrail.dat"],
        PARSER_FILE,
    );
    // The same parse rules aimed at paths that really exist, so a verb can be
    // asked the same question `check-ignore` was. `PARSER_FILE`'s subjects are
    // invented names and no verb can see them; `PARSER_BITE` claims
    // `tracked-looking.txt` — the one untracked path the fixture leaves
    // un-ignored — and only if the parser gets three rules right at once.
    rules(
        out,
        "check-ignore",
        &["check-ignore", "-v", "-n", "tracked-looking.txt", "excluded-by-info.txt", "important.log", "#nothing.txt"],
        PARSER_BITE,
    );
    rules(out, "ls-files", &["ls-files", "-o", "--exclude-standard"], PARSER_BITE);
    rules(out, "status", &["status", "--porcelain", "--ignored"], PARSER_BITE);
    rules(out, "clean", &["clean", "-ndX"], PARSER_BITE);
    // `--exclude-from` reads the identical bytes through the identical parser
    // and lands in the same `EXC_FILE` group, with no `core.excludesFile` at
    // all — so the two doors have to exclude the same set.
    out.push(
        Case::new("ls-files", &["ls-files", "-o", "--exclude-from=.gitmodules"], Shape::Attributes)
            .with_scoped_config(vec![ConfigEntry::raw(ConfigScope::Modules, PARSER_BITE)]),
    );

    rules(out, "check-ignore", &["check-ignore", "-v", "-n", "x.crlf", "keep.crlf", "d/x.crlf"], CRLF_FILE);
    rules(out, "check-ignore", &["check-ignore", "-q", "x.crlf"], CRLF_FILE);
    rules(out, "check-ignore", &["check-ignore", "-q", "keep.crlf"], CRLF_FILE);
}

// ---------------------------------------------------------------------------
// 6. The pattern grammar, one axis per file
// ---------------------------------------------------------------------------

/// Anchoring and directory-ness.
///
/// ```text
/// 1  /anchored.txt
/// 2  plain.txt
/// 3  dir/
/// 4  mid/dle.txt
/// 5  sub/*.dat
/// ```
///
/// Observed, `check-ignore -v -n --no-index`:
///
/// | subject | answer |
/// |---------|--------|
/// | `anchored.txt` | `1:/anchored.txt` |
/// | `d/anchored.txt` | `::` — a leading `/` anchors to the file's own directory |
/// | `plain.txt` | `2:plain.txt` |
/// | `d/plain.txt` | `2:plain.txt` — a pattern with no `/` matches at any depth |
/// | `dir` | `::` — `dir/` needs a *directory*, and nothing named `dir` exists |
/// | `dir/x.txt` | `3:dir/` — the leading component is a directory by position |
/// | `d/dir/x.txt` | `3:dir/` — and `dir/` is not anchored either |
/// | `mid/dle.txt` | `4:mid/dle.txt` |
/// | `d/mid/dle.txt` | `::` — a `/` anywhere but the end anchors the whole pattern |
/// | `sub/a.dat` | `5:sub/*.dat` |
/// | `sub/d/a.dat` | `::` — `*` does not cross a `/` |
///
/// The `dir` row against the `dir/x.txt` row is the pair worth stating: the same
/// rule answers "no" about the name and "yes" about a path beneath it, because
/// directory-ness is decided by *position in the subject*, not by a stat.
const ANCHOR_FILE: &str = "/anchored.txt\nplain.txt\ndir/\nmid/dle.txt\nsub/*.dat";

/// Wildcards and character classes.
///
/// ```text
/// 1  w*.dat
/// 2  q?.dat
/// 3  [a-c]cls.dat
/// 4  [!a-c]ncls.dat
/// 5  [^a-c]hat.dat
/// 6  range[0-9].dat
/// ```
///
/// Observed: `w.dat` and `wx.dat` match line 1 while `wx/y.dat` does not (`*`
/// stops at `/`); `q1.dat` matches line 2 and `q12.dat` does not (`?` is exactly
/// one character); `acls.dat` matches line 3 and `dcls.dat` does not;
/// `dncls.dat` matches line 4 and `ancls.dat` does not; `dhat.dat` matches line
/// 5 and `ahat.dat` does not — so `[^…]` is a synonym for `[!…]` here, which is
/// a `wildmatch` extension and not in `gitignore(5)`; `range5.dat` matches line
/// 6 and `rangex.dat` does not.
const WILDCARD_FILE: &str = "w*.dat\nq?.dat\n[a-c]cls.dat\n[!a-c]ncls.dat\n[^a-c]hat.dat\nrange[0-9].dat";

/// `**` in each of its three documented positions, and once where it is not one.
///
/// ```text
/// 1  lead/**/x.dat
/// 2  **/star.dat
/// 3  trail/**
/// 4  mid**dle.dat
/// ```
///
/// Observed: line 1 matches `lead/x.dat`, `lead/a/x.dat` and `lead/a/b/x.dat` —
/// `/**/ ` stands for zero or more directories, the *zero* case being the one a
/// naive translation to `.*` loses. Line 2 matches `star.dat`, `d/star.dat` and
/// `d/e/star.dat`. Line 3 matches `trail/x.dat` and `trail/a/b.dat` but **not**
/// `trail` itself — `/**` requires something inside. Line 4 is not a `**` at all:
/// it is two adjacent `*`s in a component, so it matches `middle.dat` and
/// `midXdle.dat` and not `midX/dle.dat`.
const DOUBLESTAR_FILE: &str = "lead/**/x.dat\n**/star.dat\ntrail/**\nmid**dle.dat";

/// A bare `**` on its own line, which claims everything.
///
/// Its own file because a `**` anywhere in a file is the last word on every
/// subject in it, so it cannot share one with the rules above.
const BARE_DOUBLESTAR_FILE: &str = "**";

/// Negation ordering and the excluded-directory wall inside **one** file.
///
/// ```text
/// 1  wall/
/// 2  !wall/keep.dat
/// 3  *.dat
/// 4  !important.dat
/// 5  both.dat
/// ```
///
/// Observed:
///
/// | subject | answer |
/// |---------|--------|
/// | `wall` | `::` |
/// | `wall/keep.dat` | **`.gitmodules:1:wall/`** |
/// | `wall/other.dat` | `.gitmodules:1:wall/` |
/// | `important.dat` | `.gitmodules:4:!important.dat` |
/// | `both.dat` | `.gitmodules:5:both.dat` |
/// | `plain.dat` | `.gitmodules:3:*.dat` |
///
/// Row two is the whole point, and it is only visible because `-v` prints a line
/// *number*: the file says `!wall/keep.dat` on line 2 and git reports line **1**.
/// The re-inclusion is not merely overruled — it is never consulted, because the
/// leading directory matched and the walk stopped. A port that gets the outcome
/// right by evaluating both rules and preferring the directory one still names
/// line 2 here, and is agreeing with stock by luck. Rows four and five are the
/// ordinary last-match-wins rule underneath it, in both senses.
const WALL_FILE: &str = "wall/\n!wall/keep.dat\n*.dat\n!important.dat\nboth.dat";

fn pattern_grammar(out: &mut Vec<Case>) {
    rules(
        out,
        "check-ignore",
        &[
            "check-ignore",
            "-v",
            "-n",
            "--no-index",
            "anchored.txt",
            "d/anchored.txt",
            "plain.txt",
            "d/plain.txt",
            "dir",
            "dir/x.txt",
            "d/dir/x.txt",
            "mid/dle.txt",
            "d/mid/dle.txt",
            "sub/a.dat",
            "sub/d/a.dat",
        ],
        ANCHOR_FILE,
    );
    rules(
        out,
        "check-ignore",
        &[
            "check-ignore",
            "-v",
            "-n",
            "w.dat",
            "wx.dat",
            "wx/y.dat",
            "q1.dat",
            "q12.dat",
            "acls.dat",
            "dcls.dat",
        ],
        WILDCARD_FILE,
    );
    rules(
        out,
        "check-ignore",
        &["check-ignore", "-v", "-n", "ancls.dat", "dncls.dat", "ahat.dat", "dhat.dat", "range5.dat", "rangex.dat"],
        WILDCARD_FILE,
    );
    rules(
        out,
        "check-ignore",
        &[
            "check-ignore",
            "-v",
            "-n",
            "lead/x.dat",
            "lead/a/x.dat",
            "lead/a/b/x.dat",
            "star.dat",
            "d/star.dat",
            "d/e/star.dat",
        ],
        DOUBLESTAR_FILE,
    );
    rules(
        out,
        "check-ignore",
        &["check-ignore", "-v", "-n", "trail", "trail/x.dat", "trail/a/b.dat", "middle.dat", "midXdle.dat", "midX/dle.dat"],
        DOUBLESTAR_FILE,
    );
    rules(out, "check-ignore", &["check-ignore", "-v", "-n", "anything.dat", "a/b/c.dat"], BARE_DOUBLESTAR_FILE);
    rules(
        out,
        "check-ignore",
        &["check-ignore", "-v", "-n", "wall", "wall/keep.dat", "wall/other.dat", "important.dat", "both.dat", "plain.dat"],
        WALL_FILE,
    );
    // The wall as `-q` sees it: `wall/keep.dat` is ignored, full stop.
    rules(out, "check-ignore", &["check-ignore", "-q", "wall/keep.dat"], WALL_FILE);
    rules(out, "check-ignore", &["check-ignore", "-q", "important.dat"], WALL_FILE);
}

// ---------------------------------------------------------------------------
// 7. One file, every door
// ---------------------------------------------------------------------------

/// The same synthetic ignore file read by `check-ignore` and by the verbs that
/// consume the engine, so a port that reports one winning rule and selects a
/// different set fails a pair rather than passing two halves.
///
/// The file claims every `.txt` and then un-claims the one path the fixture
/// leaves untracked and un-ignored:
///
/// ```text
/// 1  # engine readout
/// 2
/// 3  *.txt
/// 4  !tracked-looking.txt
/// ```
///
/// Because `core.excludesFile` is the *lowest* source, this changes nothing the
/// `.gitignore` stack already decided and everything it was silent about.
/// Observed against stock, with `core.excludesFile=.gitmodules`:
///
/// | invocation | result |
/// |------------|--------|
/// | `check-ignore -v -n tracked-looking.txt sub/nested.txt docs/manual.md` | `::`, `3:*.txt`, `::` |
/// | `ls-files -o --exclude-standard` | `.gitmodules`, `important.log`, `tracked-looking.txt` |
/// | `status --porcelain --ignored=matching -uall` | the three `??` plus the six `!!` the stack already had |
/// | `clean -ndX` | the six ignored paths, unchanged |
/// | `add -n .` | `add '.gitmodules'`, `add 'important.log'`, `add 'tracked-looking.txt'` |
///
/// `.gitmodules` itself is untracked and matched by nothing, which is why it
/// appears in three of the five rows — it is written by the case and is part of
/// the premise on both sides identically.
///
/// `--exclude-per-directory=.gitmodules` is the fourth door and the only one
/// that re-points the *per-directory* filename: it reads this file at the root
/// and looks for a `.gitmodules` in every subdirectory, finding none, so the
/// whole `.gitignore` tree stops applying and only these four lines and
/// `info/exclude` are left.
const READOUT_FILE: &str = "# engine readout\n\n*.txt\n!tracked-looking.txt";

fn one_file_many_doors(out: &mut Vec<Case>) {
    rules(
        out,
        "check-ignore",
        &["check-ignore", "-v", "-n", "tracked-looking.txt", "sub/nested.txt", "docs/manual.md", "excluded-by-info.txt"],
        READOUT_FILE,
    );
    rules(out, "ls-files", &["ls-files", "-o", "--exclude-standard"], READOUT_FILE);
    rules(out, "ls-files", &["ls-files", "-o", "-i", "--exclude-standard"], READOUT_FILE);
    rules(out, "status", &["status", "--porcelain", "--ignored=matching", "-uall"], READOUT_FILE);
    rules(out, "status", &["status", "--porcelain", "--ignored=traditional", "-uall"], READOUT_FILE);
    rules(out, "clean", &["clean", "-ndX"], READOUT_FILE);
    rules(out, "clean", &["clean", "-ndx"], READOUT_FILE);
    rules(out, "add", &["add", "-n", "."], READOUT_FILE);

    // The per-directory door, whose filename swap drops the whole `.gitignore`
    // tree. No `core.excludesFile` here — the file is named on the command line.
    out.push(
        Case::new(
            "ls-files",
            &["ls-files", "-o", "--exclude-standard", "--exclude-per-directory=.gitmodules"],
            Shape::Attributes,
        )
        .with_scoped_config(vec![ConfigEntry::raw(ConfigScope::Modules, READOUT_FILE)]),
    );
    out.push(
        Case::new(
            "ls-files",
            &["ls-files", "-o", "-i", "--exclude-standard", "--exclude-per-directory=.gitmodules"],
            Shape::Attributes,
        )
        .with_scoped_config(vec![ConfigEntry::raw(ConfigScope::Modules, READOUT_FILE)]),
    );
}
