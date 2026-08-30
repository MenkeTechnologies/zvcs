//! `git grep`'s **matching engine**: which regex dialect a pattern is compiled
//! in, what the boolean operators mean, and what the printed line looks like
//! once a match is found.
//!
//! Fourteen modules run `git grep`; none of them asks what a pattern *means*.
//! They pick a pattern that matches under every dialect (`fn`, `content`,
//! `kept`, `ignored`) and then vary something else — the shape, the pathspec,
//! the ignore file, the submodule. That is a real measurement of everything
//! *around* grep and no measurement of grep itself: `-E`, `-F`, `-P` and the
//! default all score identically when the pattern has no metacharacter in it,
//! so a port that compiles all four through one engine passes every one of
//! them. This module supplies the patterns that separate the engines and then
//! runs each one through all four.
//!
//! # How this divides territory with the modules that already run `grep`
//!
//! Read in full before writing a line of this file; each is named with what it
//! owns and what it leaves.
//!
//! * **`info_attrs.rs`** is the real incumbent — the largest single block of
//!   grep cases in the corpus, and the only module that treats grep as a subject
//!   rather than as a probe. It owns the *flag inventory*: one case each for
//!   `-n -l -c -i -w -v -h -e -z -L -q -m -I -a -O -p -W
//!   --column --heading --break -A -B -C
//!   --color --textconv --max-depth --no-recursive --full-name --cached
//!   --untracked --recurse-submodules`, the five boolean spellings, and the
//!   revision forms `HEAD`, `main feature`, `v0.2.0`, `HEAD:src`. Every one of
//!   those patterns (`fn`, `two`, `pub`, `content`, `.`) means the same thing
//!   in all four dialects, and every one of those flags appears exactly once
//!   and alone. So what it measures is *option parsing*, and what it cannot
//!   measure is dialect semantics, operator precedence, flag interaction, or
//!   any output shape that needs two files and a context separator to appear.
//!   Nothing here repeats one of its argvs.
//! * **`config_reads.rs`** owns `grep.lineNumber` and `grep.column` (`-c`
//!   scope, plus `grep.lineNumber` from the global file) and the five
//!   `color.grep.*` slots. Its header names `grep.patternType`,
//!   `grep.extendedRegexp` and `grep.threads` as keys it *wants* and it emits
//!   no case for any of them. Those three are here, and so is the pair
//!   `patternType` + `extendedRegexp` together, which is the only way to see
//!   that the second is ignored when the first is set, which git documents and
//!   which both oracles do.
//! * **`gitignore_precedence.rs`** owns what the untracked walk *visits*:
//!   `--untracked -l`, `--untracked --no-exclude-standard -l`, `--no-index -l`
//!   and `--no-index --exclude-standard -l`, all on [`Shape::Attributes`],
//!   all searching for `ignored`. That is the same shape this file uses for
//!   binary detection, so the ignore walk is deliberately **not** re-asked
//!   here — see the note in [`binary_and_textconv`] about the one divergence
//!   this file observed there and left to that module.
//! * **`misc_commands.rs`** owns `--no-index` outside a repository, including
//!   `grep.fallbackToNoIndex`, run from `src` with `GIT_CEILING_DIRECTORIES`
//!   set. `--no-index` is therefore absent here.
//! * **`pathspec_stdin.rs`** owns the pathspec *language* through grep —
//!   `:(glob)`, `:(icase)`, `:(attr:)`, `:(top)`, `:!`, a literal miss — and
//!   `eol_conversion.rs` owns grep's view of CRLF (`ws/eol.txt` under
//!   `core.autocrlf`, worktree versus `HEAD~2`). Both vary the path or the
//!   bytes and hold the pattern trivial; this file holds the path trivial and
//!   varies the pattern.
//! * **`shape_reach.rs`**, **`sparse_family.rs`**, **`submodule_deep.rs`**,
//!   **`fixture_gaps2.rs`**, **`fixture_gaps3.rs`**, **`graft_partial.rs`**,
//!   **`globals_layer.rs`** each use grep as a *reader* to prove a shape is
//!   reachable — a sparse cone, a submodule, an intent-to-add entry, a missing
//!   object, a global option. None of them varies a grep flag for grep's own
//!   sake, and this file touches none of their shapes except through
//!   [`Shape::Branched`], where it uses only revision forms they do not.
//! * **`exit_codes.rs`** owns grep's refusals (`grep -e` with no operand,
//!   `--nosuchopt`, `-q` on no match, a bad revision) with stderr compared.
//!   The refusals here are the ones a *pattern* causes, and their stderr is
//!   deliberately not compared — see "the two error texts" below.
//! * **`attributes_filters.rs`** contains the string `grep` zero times. The
//!   `-diff` attribute's effect on what grep calls binary was unowned; it is
//!   here.
//!
//! # The fixtures, and what each is here for
//!
//! * [`Shape::Patches`] — `app/main.c`, eleven lines of C carrying the
//!   punctuation no other fixture has:
//!
//!   ```text
//!   1  static const int VERSION = 1;
//!   2
//!   3  int add(int a, int b)
//!   4  {
//!   5  \treturn a + b;
//!   6  }
//!   7
//!   8  int main(void)
//!   9  {
//!   10 \treturn add(1, 2);
//!   11 }
//!   ```
//!
//!   `a + b` is matched literally by a basic regex and by a fixed string and by
//!   neither an extended one nor a Perl one (`+` repeats the space). `add(int`
//!   is a literal in basic and fixed and a syntax error in extended and Perl.
//!   `add\(int` inverts that exactly. Two functions and a `static` line make it
//!   the one fixture where `-W` and `-p` have a funcname to find and where
//!   `-A`/`-B` have a gap wide enough to print a `--` separator. Verified in a
//!   copy of the shape against stock 2.55.0.
//! * [`Shape::Attributes`] — `.gitattributes` marks `*.log -diff`,
//!   `vendor/** -diff`, `sub/nested.txt -diff` and `assets/*.bin binary`, and
//!   the files exist and are tracked. `-diff` is how a *text* file is declared
//!   binary, which is the only way to separate "grep asked the attribute stack"
//!   from "grep looked for a NUL byte" — `assets/logo.bin` holds real NULs and
//!   answers the second question, `logs/keep.log` holds none and answers the
//!   first. `*.md diff=markdown` gives `--textconv` a driver slot to fill.
//! * [`Shape::Linear`] and [`Shape::Branched`] — one-line and two-line
//!   `README.md`/`src/lib.rs`, used only where the *smallest* file is what
//!   makes the answer readable (the phantom trailing line) or where a second
//!   tree is needed.
//!
//! # Threading and output order
//!
//! `grep.threads` and git's default thread pool reorder nothing observable:
//! ten identical runs of `git grep -n .` over [`Shape::Attributes`]' twelve
//! tracked files produced one digest, and ten runs of
//! `git -c grep.threads=8 grep -n e` produced one digest. Every case in this
//! file was run ten times against stock before it was written and every one of
//! them is single-digest; the multi-file cases are kept anyway to
//! single-directory pathspecs so the property is not being leaned on harder
//! than it was measured.
//!
//! # The two error texts
//!
//! A pattern that will not compile makes both sides exit 128 and print
//! nothing on stdout, and their stderr disagrees by construction:
//!
//! ```text
//! $ git grep -E 'add(int' -- app/main.c
//! stock: fatal: command line, 'add(int': parentheses not balanced
//! port:  fatal: invalid regex: regex parse error:
//!            (?:add(int)
//!            ^
//!        error: unclosed group
//! ```
//!
//! The port wraps every pattern in `(?:…)` and reports the crate's parse
//! error, so the text can never match. Per the harness's standing policy that
//! error prose is not a compatibility surface, these cases compare stdout and
//! exit code only — the *refusal* is the behaviour and both sides refuse. The
//! `(?:…)` wrapper leaking into a user-visible message is recorded here rather
//! than pinned as a case, because pinning it would freeze a message stock has
//! no obligation to match.

use crate::fixture::Shape;
use crate::runner::Case;

pub fn cases(out: &mut Vec<Case>) {
    engine_matrix(out);
    engine_syntax_leak(out);
    pattern_type_config(out);
    trailing_empty_line(out);
    boolean_precedence(out);
    pattern_file(out);
    only_matching(out);
    context_and_function(out);
    binary_and_textconv(out);
    depth_and_relative_paths(out);
    tree_and_object_args(out);
}

/// One case against [`Shape::Patches`], whose `app/main.c` is the C file every
/// dialect question is asked about.
fn pa(out: &mut Vec<Case>, args: &[&str]) {
    out.push(Case::new("grep", args, Shape::Patches));
}

/// One case against [`Shape::Attributes`], whose `.gitattributes` is what makes
/// a text file binary and a markdown file textconv-able.
fn at(out: &mut Vec<Case>, args: &[&str]) {
    out.push(Case::new("grep", args, Shape::Attributes));
}

// ---------------------------------------------------------------------------
// 1. The same pattern through all four engines
// ---------------------------------------------------------------------------

/// Four patterns, each run under the default (basic), `-E`, `-F` and `-P`.
///
/// This is the group the module exists for. Each pattern is chosen so the four
/// answers are not the same answer, which is what makes the group able to tell
/// four engines from one engine wearing four flags:
///
/// | pattern     | default | `-E`  | `-F`  | `-P`  |
/// |-------------|---------|-------|-------|-------|
/// | `a + b`     | line 5  | none  | line 5| none  |
/// | `add(int`   | line 3  | 128   | line 3| 128   |
/// | `add\(int`  | 128     | line 3| none  | line 3|
/// | `\d, \d`    | none    | none  | none  | line 10 |
///
/// Every cell was run against stock 2.55.0 in a copy of the shape before the
/// case was written. The third row is the second row's inverse and is the one
/// that catches a port that simply strips backslashes: nothing that treats
/// `add(int` and `add\(int` as the same pattern can produce both rows.
fn engine_matrix(out: &mut Vec<Case>) {
    // `+` is a literal in a basic regex and a repeat in an extended one.
    pa(out, &["grep", "-n", "a + b", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "a + b", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-F", "a + b", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "a + b", "--", "app/main.c"]);

    // A bare `(` is a literal in a basic regex and an unclosed group elsewhere.
    pa(out, &["grep", "-n", "add(int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "add(int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-F", "add(int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "add(int", "--", "app/main.c"]);

    // `\(` is the group opener in a basic regex and a literal elsewhere — the
    // exact inverse of the block above, and no engine agrees with itself
    // across the two.
    pa(out, &["grep", "-n", "add\\(int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "add\\(int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-F", "add\\(int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "add\\(int", "--", "app/main.c"]);

    // `\d` is a digit class in Perl and the letter `d` in both POSIX dialects.
    pa(out, &["grep", "-n", "\\d, \\d", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "\\d, \\d", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "\\d, \\d", "--", "app/main.c"]);

    // Intervals: `\{n\}` in basic, `{n}` in extended. Neither spelling means
    // anything in the other dialect, and both are legal there as literals.
    pa(out, &["grep", "-n", "int\\{2\\}", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "int{1,}", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "int{1,}", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "int{,3}", "--", "app/main.c"]);

    // Alternation: `|` is a literal in basic and an operator in extended, and
    // `\|` is git's basic-regex alternation.
    pa(out, &["grep", "-n", "static|main", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "static\\|main", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "static|main", "--", "app/main.c"]);

    // Backreferences: legal in all three regex dialects, meaningless in `-F`.
    // `\(int\) .*\1` and `(int) .*\1` select *different* lines, so a port that
    // maps both onto one dialect cannot produce both answers.
    pa(out, &["grep", "-n", "\\(int\\) .*\\1", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "(int) .*\\1", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "(int) .*\\1", "--", "app/main.c"]);

    // Perl-only constructs. Lookbehind, negative lookahead and `\K` have no
    // POSIX spelling at all, so these are the cases that decide whether `-P`
    // is PCRE or a rename of the same engine.
    pa(out, &["grep", "-n", "-P", "(?<=return )add", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "a(?!dd)", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "-o", "in\\Kt", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "-o", "(?<name>int)", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "(?:ret)urn", "--", "app/main.c"]);

    // `\A` and `\Z` anchor to the *subject*, and git hands PCRE the whole file
    // buffer rather than one line, so `\Aint` matches nothing here even though
    // two lines begin with `int`. A per-line matcher answers differently.
    pa(out, &["grep", "-n", "-P", "\\Aint", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "b;\\Z", "--", "app/main.c"]);

    // Constructs all three dialects share, as the control: a port that fails
    // the rows above and passes these has a dialect problem, not a regex
    // problem.
    pa(out, &["grep", "-n", "[[:digit:]], [[:digit:]]", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "[[:upper:]]+", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "\\(add\\)", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "ret(urn)", "--", "app/main.c"]);
}

// ---------------------------------------------------------------------------
// 2. Perl syntax reaching the POSIX engines
// ---------------------------------------------------------------------------

/// Patterns that are *legal but different* in a POSIX dialect: a port built on
/// one modern regex library and switched by a flag accepts them with their
/// Perl meaning, while a POSIX engine either treats the escape as a literal or
/// refuses the construct outright.
///
/// The distinction matters more than the error cases in
/// [`engine_matrix`]: a refusal is loud and gets fixed, while `-E '\bint\b'`
/// quietly returning three lines where git returns none is a wrong answer that
/// looks like a right one.
fn engine_syntax_leak(out: &mut Vec<Case>) {
    // `\w`, `\b` and `\s` in an extended regex. POSIX has no such escapes; git
    // resolves `\w` to the literal `w`, so these patterns match nothing.
    pa(out, &["grep", "-n", "-E", "\\w+ add", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "\\bint\\b", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "\\s\\sreturn", "--", "app/main.c"]);

    // The same escapes under the default dialect, where git's own basic-regex
    // support does provide `\w` — so the two POSIX dialects are not
    // interchangeable either.
    pa(out, &["grep", "-n", "\\w", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "int\\+", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "add\\?", "--", "app/main.c"]);

    // Inline flag groups. `(?i)` is not POSIX syntax in any dialect; stock
    // refuses the whole pattern under `-E` and treats it as a literal group
    // under the default.
    pa(out, &["grep", "-n", "-E", "(?i)INT ADD", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "(?i)INT ADD", "--", "app/main.c"]);

    // Case folding and word boundaries that every engine does implement, as
    // the control for the block above.
    pa(out, &["grep", "-n", "-P", "-i", "INT ADD", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-i", "-F", "INT ADD", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-F", "int", "-w", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "int", "-w", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "-w", "a", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-w", "-E", "a|b", "--", "app/main.c"]);

    // `-x` is the other whole-line anchor, and it composes with a dialect.
    pa(out, &["grep", "-n", "-x", "int main(void)", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-x", "-E", "int main.void.", "--", "app/main.c"]);
}

// ---------------------------------------------------------------------------
// 3. Choosing the engine by configuration
// ---------------------------------------------------------------------------

/// `grep.patternType`, `grep.extendedRegexp`, and which of them wins.
///
/// One pattern — `a + b`, which matches under basic and fixed and not under
/// extended or Perl — carried through all five `patternType` values, so the
/// five cases have two distinguishable outcomes rather than five identical
/// ones. Then the three precedence rules, each measured against 2.55.0 and
/// corroborated by 2.50.1:
///
/// * an explicit `-G`/`-E`/`-F`/`-P` on the command line overrides the config;
/// * `grep.extendedRegexp` is ignored whenever `grep.patternType` is set to
///   anything but `default`;
/// * with `patternType=default`, `extendedRegexp` decides.
///
/// `grep.threads` is here rather than in `config_reads.rs` because its header
/// names it as a key it could not reach. Its legal values change nothing
/// observable by design — that is the point of measuring it: a port may not
/// turn a thread count into an output difference, and `0`, `1` and a negative
/// value must all still produce git's answer.
fn pattern_type_config(out: &mut Vec<Case>) {
    for kind in ["basic", "extended", "fixed", "perl", "default"] {
        out.push(
            Case::new("grep", &["grep", "-n", "a + b", "--", "app/main.c"], Shape::Patches)
                .with_config(&[("grep.patternType", kind)]),
        );
    }

    // An explicit dialect flag beats the configured one, in both directions.
    out.push(
        Case::new("grep", &["grep", "-n", "-G", "a + b", "--", "app/main.c"], Shape::Patches)
            .with_config(&[("grep.patternType", "extended")]),
    );
    out.push(
        Case::new("grep", &["grep", "-n", "-F", "a + b", "--", "app/main.c"], Shape::Patches)
            .with_config(&[("grep.patternType", "perl")]),
    );

    // An unknown value is a refusal, not a fallback to the default.
    out.push(
        Case::new("grep", &["grep", "-n", "a + b", "--", "app/main.c"], Shape::Patches)
            .with_config(&[("grep.patternType", "bogus")]),
    );

    // extendedRegexp alone, then losing to patternType, then deciding because
    // patternType stepped aside.
    out.push(
        Case::new("grep", &["grep", "-n", "a + b", "--", "app/main.c"], Shape::Patches)
            .with_config(&[("grep.extendedRegexp", "true")]),
    );
    out.push(
        Case::new("grep", &["grep", "-n", "a + b", "--", "app/main.c"], Shape::Patches)
            .with_config(&[("grep.extendedRegexp", "true"), ("grep.patternType", "basic")]),
    );
    out.push(
        Case::new("grep", &["grep", "-n", "a + b", "--", "app/main.c"], Shape::Patches)
            .with_config(&[("grep.extendedRegexp", "true"), ("grep.patternType", "default")]),
    );

    // The configured dialect has to be the *whole* dialect, not just a
    // different literal-escaping rule: these two are the lookbehind and the
    // `\b` from the groups above, selected by configuration instead of a flag.
    out.push(
        Case::new(
            "grep",
            &["grep", "-n", "(?<=return )add", "--", "app/main.c"],
            Shape::Patches,
        )
        .with_config(&[("grep.patternType", "perl")]),
    );
    out.push(
        Case::new("grep", &["grep", "-n", "\\bint\\b", "--", "app/main.c"], Shape::Patches)
            .with_config(&[("grep.patternType", "extended")]),
    );

    // A configured dialect also has to reach the `-e` list and the boolean
    // grammar, not only the bare positional pattern.
    out.push(
        Case::new(
            "grep",
            &["grep", "-n", "-e", "add(", "--or", "-e", "a + b", "--", "app/main.c"],
            Shape::Patches,
        )
        .with_config(&[("grep.patternType", "fixed")]),
    );

    // grep.threads: legal, degenerate and rejected values, all of which must
    // leave the output alone.
    for threads in ["1", "0", "-1", "bogus"] {
        out.push(
            Case::new("grep", &["grep", "-n", "int", "--", "app/main.c"], Shape::Patches)
                .with_config(&[("grep.threads", threads)]),
        );
    }
    out.push(
        Case::new("grep", &["grep", "-n", "int", "--", "app/main.c", "src/lib.rs"], Shape::Patches)
            .with_config(&[("grep.threads", "1")]),
    );
}

// ---------------------------------------------------------------------------
// 4. The empty line that is not in the file
// ---------------------------------------------------------------------------

/// `^$` matches one line past the end of every file, on the POSIX engines only.
///
/// `README.md` in [`Shape::Linear`] is `# fixture\n` — one line — and stock
/// answers `README.md:2:`. `app/main.c` is eleven lines and stock's `-c` says
/// three blank lines where the file holds two. The same pattern under `-P`
/// matches nothing, so this is not a property of the file, it is a property of
/// git's POSIX matching loop, and it is the sharpest available test of whether
/// a port reimplemented that loop or wrapped a regex library around
/// `str::lines()`. Verified against stock 2.55.0 and corroborated by 2.50.1.
///
/// Asked in the worktree, from the index and from a tree, because the three
/// read the blob through different code and only the first of them has a file
/// on disk to have a trailing newline in.
fn trailing_empty_line(out: &mut Vec<Case>) {
    out.push(Case::new("grep", &["grep", "-n", "^$", "--", "README.md"], Shape::Linear));
    out.push(Case::new("grep", &["grep", "-n", "-P", "^$", "--", "README.md"], Shape::Linear));
    out.push(Case::new(
        "grep",
        &["grep", "-n", "--cached", "^$", "--", "README.md"],
        Shape::Linear,
    ));
    out.push(Case::new("grep", &["grep", "-n", "^$", "HEAD", "--", "README.md"], Shape::Linear));

    pa(out, &["grep", "-n", "-E", "^$", "--", "app/main.c"]);
    pa(out, &["grep", "-c", "-E", "^$", "--", "app/main.c"]);

    // The empty pattern is the neighbouring degenerate case and does *not*
    // gain the phantom line, which is what makes the pair readable.
    out.push(Case::new("grep", &["grep", "-c", "", "--", "README.md"], Shape::Linear));
    out.push(Case::new("grep", &["grep", "-n", "", "--", "README.md"], Shape::Linear));
    out.push(Case::new("grep", &["grep", "-n", "-E", "", "--", "README.md"], Shape::Linear));
    out.push(Case::new("grep", &["grep", "-n", "-F", "", "--", "README.md"], Shape::Linear));
}

// ---------------------------------------------------------------------------
// 5. The boolean grammar, and what binds tighter
// ---------------------------------------------------------------------------

/// `--and`, `--or`, `--not`, parentheses and `--all-match`.
///
/// `info_attrs.rs` has one case per operator and one parenthesised pair, all
/// with two operands, which is the largest expression whose value does not
/// depend on precedence. Every case here has three, so `--and` binding tighter
/// than `--or` is observable:
///
/// ```text
/// $ git grep -n -e static --or -e return --and -e add -- app/main.c
/// app/main.c:1:static const int VERSION = 1;
/// app/main.c:10:  return add(1, 2);
/// ```
///
/// Line 1 is in the answer only because `--or` binds loosest; line 5
/// (`return a + b;`) is out of it only because `--and` binds tighter. Parsing
/// the same three operands left to right gives line 10 alone, which is what
/// the parenthesised sibling case prints, so the two cases together pin the
/// precedence rather than the parser's mood.
fn boolean_precedence(out: &mut Vec<Case>) {
    pa(out, &["grep", "-n", "-e", "static", "--or", "-e", "return", "--and", "-e", "add", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-e", "static", "--and", "-e", "int", "--or", "-e", "add", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "(", "-e", "static", "--or", "-e", "return", ")", "--and", "-e", "add", "--", "app/main.c"]);

    // `--not` binds tighter than `--and`, and applies to a whole group when
    // one follows it.
    pa(out, &["grep", "-n", "-e", "int", "--and", "--not", "-e", "add", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "--not", "-e", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "--not", "(", "-e", "int", "--or", "-e", "add", ")", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-e", "a", "--and", "--not", "-e", "b", "--and", "-e", "int", "--", "app/main.c"]);

    // Two bare `-e`s are an implicit `--or`, and `--all-match` promotes that
    // to "every top-level operand must match somewhere in the file" — a
    // per-file test, so it is only visible with two files or with `-l`.
    pa(out, &["grep", "-n", "-e", "int", "-e", "add", "--", "app/main.c"]);
    pa(out, &["grep", "-l", "-e", "int", "-e", "VERSION", "--", "app/main.c", "src/lib.rs"]);
    pa(out, &["grep", "-l", "--all-match", "-e", "int", "-e", "VERSION", "--", "app/main.c", "src/lib.rs"]);
    pa(out, &["grep", "-n", "--all-match", "-e", "int", "-e", "zzz", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "--all-match", "-e", "int", "--or", "-e", "zz", "-e", "VERSION", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "--all-match", "--not", "-e", "int", "-e", "add", "--", "app/main.c"]);

    // `-v` over an expression inverts the whole expression, and `-c` counts
    // the lines the expression selected rather than the pattern hits.
    pa(out, &["grep", "-n", "-v", "-e", "int", "-e", "add", "--", "app/main.c"]);
    pa(out, &["grep", "-c", "-e", "int", "--and", "-e", "add", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-E", "-e", "int|VERSION", "--and", "--not", "-e", "add", "--", "app/main.c"]);

    // Malformed expressions: a leading operator, an unbalanced group each way.
    pa(out, &["grep", "-n", "--and", "-e", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "(", "-e", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-e", "int", ")", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "(", "-e", "int", ")", "--", "app/main.c"]);

    // An expression composes with the output modes, not only with `-n`.
    pa(out, &["grep", "-n", "-W", "-e", "return", "--and", "-e", "add", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-P", "-e", "\\d", "--and", "-e", "add", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-F", "-e", "add(", "--and", "--not", "-e", "return", "--", "app/main.c"]);
}

// ---------------------------------------------------------------------------
// 6. Patterns read from a file
// ---------------------------------------------------------------------------

/// `-f <file>`, including `-f -`.
///
/// No case in the corpus supplies `-f` at all, and the reason is structural: a
/// case is one argv against a pristine copy and cannot write a pattern file
/// first. Two ways around it, both used here — point `-f` at a file the
/// *fixture* already tracks, and use `-f -`, which reads the pattern list from
/// stdin, which the harness does deliver byte for byte.
///
/// The fixture files are chosen for what their lines are as patterns:
///
/// * `quilt/series` proves that once `-f` is given, the first positional is no
///   longer the pattern — stock reads `x` as a revision and dies
///   `ambiguous argument 'x'` rather than searching for it;
/// * `app/main.c` is eleven lines of C, several of which are regexes with
///   metacharacters (`int add(int a, int b)` is an unbalanced group under an
///   extended dialect, `{` is a literal under the default one);
/// * `docs/manual.md` in [`Shape::Attributes`] has a **blank line** in it, and
///   stock drops empty lines rather than compiling a pattern that matches
///   everything — a pattern file whose only line is blank exits 1 over `src`,
///   measured against 2.55.0. So stock searches for `# manual`, `prose` and
///   `more prose` here and finds none of them; an implementation that keeps
///   the empty line prints the whole directory instead.
fn pattern_file(out: &mut Vec<Case>) {
    pa(out, &["grep", "-n", "-f", "quilt/series", "x"]);
    pa(out, &["grep", "-n", "-f", "app/main.c", "--", "src"]);
    at(out, &["grep", "-n", "-f", "docs/manual.md", "--", "src"]);
    pa(out, &["grep", "-n", "-f", "nosuchfile", "x"]);

    // `-f -` reads the list from stdin. The payload is a literal in this
    // binary, never a file read at run time.
    out.push(Case::with_stdin(
        "grep",
        &["grep", "-n", "-f", "-", "--", "app/main.c"],
        Shape::Patches,
        b"add\nVERSION\n",
    ));
    // A dialect flag applies to the patterns that came from stdin too: `a + b`
    // is a fixed string here and would not match as an extended regex.
    out.push(Case::with_stdin(
        "grep",
        &["grep", "-n", "-F", "-f", "-", "--", "app/main.c"],
        Shape::Patches,
        b"a + b\n",
    ));
    // And a stdin pattern is an operand of the boolean grammar like any other.
    out.push(Case::with_stdin(
        "grep",
        &["grep", "-n", "-f", "-", "--and", "-e", "int", "--", "app/main.c"],
        Shape::Patches,
        b"add\n",
    ));
}

// ---------------------------------------------------------------------------
// 7. `-o`, and the separators it does or does not print
// ---------------------------------------------------------------------------

/// `--only-matching` against every other output mode.
///
/// Not one case in the corpus passes `-o`. It is the one mode that prints
/// something other than the matched line, so it interacts with every framing
/// decision separately: `-n` numbers each *hit* rather than each line, `-c`
/// keeps counting lines, `--column` reports the column of the hit being
/// printed, and a line with three hits prints three times.
///
/// The context flags are where it gets interesting. `-o -A1` still runs the
/// context bookkeeping and so still prints `--` separators between groups,
/// even though no context line is ever printed:
///
/// ```text
/// $ git grep -o -A1 -n int -- app/main.c
/// app/main.c:1:int
/// --
/// app/main.c:3:int
/// app/main.c:3:int
/// app/main.c:3:int
/// --
/// app/main.c:8:int
/// ```
fn only_matching(out: &mut Vec<Case>) {
    pa(out, &["grep", "-n", "-o", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-o", "-E", "[a-z]+", "--", "app/main.c"]);
    pa(out, &["grep", "-o", "-c", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-c", "-o", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-o", "--column", "-n", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-o", "-h", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-o", "-z", "-n", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-o", "-v", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-o", "-w", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-o", "-E", "^int", "--", "app/main.c"]);
    pa(out, &["grep", "-o", "-z", "-n", "--column", "int", "--", "app/main.c"]);

    // The three context flags with `-o`, each producing a different number of
    // `--` separators for the same set of hits.
    pa(out, &["grep", "-o", "-A1", "-n", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-o", "-B1", "-n", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-o", "-C1", "-n", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-o", "--break", "-n", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-o", "--heading", "-n", "int", "--", "app/main.c"]);
    at(out, &["grep", "-o", "-A1", "-n", "e", "--", ".gitignore"]);
}

// ---------------------------------------------------------------------------
// 8. Context, headings, and the function the match is in
// ---------------------------------------------------------------------------

/// `-A`/`-B`/`-C`, `--heading`/`--break`, `-W` and `-p` on a file that has
/// functions.
///
/// `info_attrs.rs` asks all of these against `src/lib.rs` in
/// [`Shape::Branched`] — two lines, no funcname driver, no gap wide enough for
/// a `--` separator, so `-W` and `-p` print the same thing `-n` does and the
/// context flags print the whole file. `app/main.c` has two C functions, a
/// `static` line above both, and blank lines between them, so `-W` has a
/// function to bound, `-p` has a header line to find, and `-A1`/`-B1` have a
/// gap to separate.
fn context_and_function(out: &mut Vec<Case>) {
    pa(out, &["grep", "-W", "-n", "return", "--", "app/main.c"]);
    pa(out, &["grep", "-p", "-n", "return", "--", "app/main.c"]);
    pa(out, &["grep", "-W", "-n", "VERSION", "--", "app/main.c"]);
    pa(out, &["grep", "-p", "-n", "a + b", "--", "app/main.c"]);
    pa(out, &["grep", "-W", "-n", "--heading", "return", "--", "app/main.c"]);
    pa(out, &["grep", "-W", "-n", "return", "HEAD", "--", "app/main.c"]);
    pa(out, &["grep", "-p", "-n", "return", "HEAD", "--", "app/main.c"]);

    pa(out, &["grep", "-A2", "-n", "add", "--", "app/main.c"]);
    pa(out, &["grep", "-B1", "-A1", "-n", "return", "--", "app/main.c"]);
    pa(out, &["grep", "-C0", "-n", "return", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-C1", "a + b", "--", "app/main.c", "src/lib.rs"]);
    pa(out, &["grep", "--heading", "--break", "-n", "-A1", "return", "--", "app/main.c"]);
    pa(out, &["grep", "-n", "-A1", "-B1", "--heading", "--break", "int", "--", "app/main.c"]);

    // The framing flags whose long spellings and negations no case uses.
    pa(out, &["grep", "--null", "--line-number", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-z", "--heading", "-n", "int", "--", "app/main.c"]);
    pa(out, &["grep", "--column", "-n", "return", "--", "app/main.c"]);
    pa(out, &["grep", "--column", "-n", "-o", "nt", "--", "app/main.c"]);
    pa(out, &["grep", "--column", "-n", "-i", "INT", "--", "app/main.c"]);
    pa(out, &["grep", "--column", "-c", "int", "--", "app/main.c"]);
    pa(out, &["grep", "--column", "-l", "int", "--", "app/main.c"]);
    pa(out, &["grep", "-P", "--column", "-n", "a.b", "--", "app/main.c"]);

    // `-h`/`-H`, `-l`/`-L` and `-q` against each other, over two files so the
    // filename column has something to suppress.
    at(out, &["grep", "-n", "-h", "e", "--", ".gitignore", ".mailmap"]);
    at(out, &["grep", "-n", "-H", "e", "--", ".gitignore", ".mailmap"]);
    at(out, &["grep", "-n", "-h", "e", "HEAD", "--", ".gitignore"]);
    at(out, &["grep", "-L", "zzz", "--", ".gitignore", ".mailmap"]);
    at(out, &["grep", "-L", "-z", "zzz", "--", ".gitignore", ".mailmap"]);
    at(out, &["grep", "-c", "-z", "e", "--", ".gitignore", ".mailmap"]);
    at(out, &["grep", "-l", "-z", "e", "--", ".gitignore", ".mailmap"]);
    at(out, &["grep", "--break", "-n", "e", "--", ".gitignore", ".mailmap"]);
    at(out, &["grep", "--all-match", "-L", "-e", "e", "-e", "z", "--", ".gitignore", ".mailmap"]);
    pa(out, &["grep", "-l", "-L", "int", "--", "app/main.c", "src/lib.rs"]);
    at(out, &["grep", "-q", "-c", "e", "--", ".gitignore"]);
    pa(out, &["grep", "-q", "-n", "zzz", "HEAD"]);
    pa(out, &["grep", "-m1", "-c", "int", "--", "app/main.c"]);
    pa(out, &["grep", "--max-count=2", "-n", "int", "--", "app/main.c"]);
    pa(out, &["grep", "--max-count=0", "-n", "int", "--", "app/main.c"]);
}

// ---------------------------------------------------------------------------
// 9. What counts as binary, and what `--textconv` does about it
// ---------------------------------------------------------------------------

/// Binary detection has two independent inputs and the corpus measured
/// neither: the bytes, and the `diff` attribute.
///
/// [`Shape::Attributes`] separates them. `assets/logo.bin` is `binary` in
/// `.gitattributes` *and* full of NULs, so any implementation calls it binary.
/// `logs/keep.log`, `vendor/generated.js` and `sub/nested.txt` are plain ASCII
/// carrying `-diff`, which is the *only* thing that makes them binary:
///
/// ```text
/// $ git grep -n generated -- vendor/generated.js
/// Binary file vendor/generated.js matches
/// ```
///
/// `-a`, `--text` and `-I` are then the three ways to argue with that verdict,
/// and each is asked here against both kinds of file, in the worktree, from
/// the index and from a tree.
///
/// `--textconv` shares the fixture because it shares the mechanism — both read
/// the `diff` attribute and look up a userdiff driver. `.gitattributes` sets
/// `*.md diff=markdown`, so a `diff.markdown.textconv` value is enough to make
/// grep search something other than the file. `head -1` is the filter: it is
/// in every POSIX PATH, it is deterministic, and it truncates
/// `docs/manual.md` to its first line, so `prose` (line 3) disappears and
/// `manual` (line 1) survives — an observable transform rather than a filter
/// that happens to be the identity.
///
/// Deliberately **not** here: `--untracked` and `--no-exclude-standard` over
/// this shape, which `gitignore_precedence.rs` owns. This module observed both
/// diverging on this fixture while probing (`--untracked -c e` disagrees about
/// the tracked-but-ignored `logs/keep.log`, and `--untracked
/// --no-exclude-standard -l e` disagrees about `build/output.o` and
/// `sub/deep-ignored/thing.txt`) and left them there rather than filing the
/// same walk twice under two owners.
fn binary_and_textconv(out: &mut Vec<Case>) {
    // Binary by attribute alone, in the three places a blob can be read from.
    at(out, &["grep", "-n", "tracked", "--", "logs/keep.log"]);
    at(out, &["grep", "-a", "-n", "tracked", "--", "logs/keep.log"]);
    at(out, &["grep", "-I", "-n", "tracked", "--", "logs/keep.log"]);
    at(out, &["grep", "--text", "-n", "tracked", "--", "logs/keep.log"]);
    at(out, &["grep", "-c", "tracked", "--", "logs/keep.log"]);
    at(out, &["grep", "-l", "tracked", "--", "logs/keep.log"]);
    at(out, &["grep", "-n", "generated", "--", "vendor/generated.js"]);
    at(out, &["grep", "--cached", "-n", "generated", "--", "vendor/generated.js"]);
    at(out, &["grep", "-n", "generated", "HEAD", "--", "vendor/generated.js"]);
    at(out, &["grep", "-n", "-a", "nested", "--", "sub/nested.txt"]);
    at(out, &["grep", "-n", "-I", "nested", "--", "sub/nested.txt"]);
    at(out, &["grep", "-c", "nested", "--", "sub/nested.txt"]);
    at(out, &["grep", "-o", "-n", "nested", "--", "sub/nested.txt"]);
    at(out, &["grep", "-n", "-W", "nested", "--", "sub/nested.txt"]);

    // Binary by content: NUL bytes, no attribute needed.
    at(out, &["grep", "-n", "FMT", "--", "assets/logo.bin"]);
    at(out, &["grep", "-I", "-n", "FMT", "--", "assets/logo.bin"]);
    at(out, &["grep", "-c", "FMT", "--", "assets/logo.bin"]);
    at(out, &["grep", "-a", "-c", "FMT", "--", "assets/logo.bin"]);
    at(out, &["grep", "-l", "FMT", "--", "assets/logo.bin"]);
    at(out, &["grep", "-a", "-o", "FMT", "--", "assets/logo.bin"]);
    pa(out, &["grep", "-a", "-I", "-n", "int", "--", "app/main.c"]);

    // textconv: the filter fires, and the four ways to reach or avoid it.
    const TC: &[(&str, &str)] = &[("diff.markdown.textconv", "head -1")];
    for args in [
        &["grep", "--textconv", "-n", "prose", "--", "docs/manual.md"][..],
        &["grep", "--textconv", "-n", "manual", "--", "docs/manual.md"],
        &["grep", "--no-textconv", "-n", "prose", "--", "docs/manual.md"],
        &["grep", "--textconv", "--cached", "-n", "prose", "--", "docs/manual.md"],
        &["grep", "--textconv", "-n", "prose", "HEAD", "--", "docs/manual.md"],
        &["grep", "-n", "prose", "HEAD", "--", "docs/manual.md"],
        &["grep", "--textconv", "-c", "prose", "--", "docs"],
    ] {
        out.push(Case::new("grep", args, Shape::Attributes).with_config(TC));
    }
    // A driver that cannot run: both sides must refuse, not silently search
    // the raw blob.
    out.push(
        Case::new(
            "grep",
            &["grep", "--textconv", "-n", "prose", "--", "docs/manual.md"],
            Shape::Attributes,
        )
        .with_config(&[("diff.markdown.textconv", "tr a-z A-Z")]),
    );
}

// ---------------------------------------------------------------------------
// 10. How deep it walks, and what it calls the files it finds
// ---------------------------------------------------------------------------

/// `--max-depth`, `-r`/`--no-recursive`, and the path prefix grep prints when
/// it is run from a subdirectory.
///
/// Two separate questions that share a fixture because both are decided by the
/// pathspec's leading directory.
///
/// **Depth** is counted from the pathspec, not from the repository root, so
/// `--max-depth 0` with no pathspec and `--max-depth 0 -- .` must give the
/// same answer — the top-level files — and `--max-depth 1 -- .` must add one
/// level. `info_attrs.rs` asks `--max-depth 0` once, with no pathspec, which
/// is the one spelling where a port that counts `.` as a level still agrees.
///
/// **The prefix** is relative to the working directory. From `app/`, stock
/// names its own file `main.c` and a sibling directory's file
/// `../mail/one.eml`; `--full-name` is what asks for repository-relative names
/// instead. Asked through `-n`, `-l`, `-c`, `-z`, `--heading` and a tree,
/// because the prefix is applied in a different place for each.
fn depth_and_relative_paths(out: &mut Vec<Case>) {
    pa(out, &["grep", "-n", "--max-depth", "0", "int"]);
    pa(out, &["grep", "-n", "--max-depth", "0", "int", "--", "."]);
    pa(out, &["grep", "-n", "--max-depth", "1", "int", "--", "."]);
    pa(out, &["grep", "-n", "--max-depth", "2", "int", "--", "."]);
    pa(out, &["grep", "-l", "--max-depth", "1", "int", "--", "."]);
    pa(out, &["grep", "-n", "--max-depth", "1", "int", "--", "app"]);
    pa(out, &["grep", "-n", "--max-depth", "0", "int", "--", "app", "src"]);
    at(out, &["grep", "-l", "--max-depth", "0", "e", "--", "."]);
    at(out, &["grep", "-n", "--max-depth", "1", "nested", "--", "sub"]);
    pa(out, &["grep", "-n", "-r", "int", "--", "app"]);
    pa(out, &["grep", "-n", "--no-recursive", "int", "--", "."]);

    for args in [
        &["grep", "-n", "return", "--", ".."][..],
        &["grep", "-n", "--full-name", "return", "--", ".."],
        &["grep", "-l", "return", "--", ".."],
        &["grep", "-c", "return", "--", ".."],
        &["grep", "-n", "-z", "return", "--", ".."],
        &["grep", "--heading", "-n", "return", "--", ".."],
        &["grep", "-n", "return", "HEAD", "--", ".."],
        &["grep", "-n", "int", "--", "../src"],
        &["grep", "-n", "--no-full-name", "return"],
    ] {
        out.push(Case::new("grep", args, Shape::Patches).in_dir("app"));
    }
    out.push(
        Case::new("grep", &["grep", "-n", "return"], Shape::Patches)
            .in_dir("app")
            .with_config(&[("grep.fullName", "true")]),
    );
}

// ---------------------------------------------------------------------------
// 11. Searching objects instead of files
// ---------------------------------------------------------------------------

/// Several trees at once, and an argument that names a **blob**.
///
/// `info_attrs.rs` covers `HEAD`, `main feature`, `v0.2.0` and `HEAD:src` with
/// `-n` alone. The two gaps are the object form `HEAD:src/lib.rs` — a blob
/// rather than a tree, which git searches as a single file — and what the
/// per-tree `<rev>:` prefix does once another framing flag is also asking for
/// the filename column.
fn tree_and_object_args(out: &mut Vec<Case>) {
    let br = Shape::Branched;
    out.push(Case::new("grep", &["grep", "-n", "fn", "HEAD:src/lib.rs"], br));
    out.push(Case::new("grep", &["grep", "-n", "fn", "HEAD", "HEAD~1", "v0.1.0"], br));
    out.push(Case::new("grep", &["grep", "-n", "-h", "fn", "HEAD", "HEAD~1"], br));
    out.push(Case::new("grep", &["grep", "-c", "fn", "HEAD", "HEAD~1"], br));
    out.push(Case::new("grep", &["grep", "-l", "fn", "HEAD", "HEAD~1"], br));
    out.push(Case::new("grep", &["grep", "--heading", "-n", "fn", "HEAD", "HEAD~1"], br));
    out.push(Case::new("grep", &["grep", "-n", "fn", "v0.2.0", "--", "src"], br));
    out.push(Case::new("grep", &["grep", "-n", "--cached", "fn", "HEAD"], br));
    out.push(Case::new("grep", &["grep", "-n", "--untracked", "fn", "HEAD"], br));
    out.push(Case::new("grep", &["grep", "-n", "fn", "HEAD", "nosuchrev"], br));
}
