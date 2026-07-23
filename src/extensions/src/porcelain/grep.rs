//! `git grep` — search the contents of tracked files for a pattern.
//!
//! Covered: the tracked-worktree default, `--cached`, `--untracked` and
//! `--no-index`, pathspec limiting (via gitoxide's pathspec platform, so magic
//! and globs work and a subdirectory invocation searches only that subtree),
//! `--max-depth`, `--max-count`, and the line/name/count/quiet output modes with
//! byte-identical formatting, including git's binary-file handling and
//! `core.quotePath` path quoting.
//!
//! `--textconv` is honoured: with no `diff.<driver>.textconv` command configured
//! it is the no-op git short-circuits, and with one configured for a path that
//! path's bytes are run through the converter — via the shell, the blob written to
//! a temp file and passed as the argument, exactly as git's `run_textconv` does —
//! before being searched.
//!
//! `--and`/`--or`/`--not` and `( … )` build git's boolean pattern expression: the
//! grammar is parsed as in `compile_pattern_*` (implicit adjacency is OR, `--and`
//! binds tighter, `--not` negates, parentheses group), and each line is admitted
//! by evaluating the tree, while highlighting, `-o` and the coloured `--column`
//! keep scanning the flat pattern list — git's own split. `--all-match` then gates
//! a file on every top-level `--or` term matching some line.
//!
//! `-W`/`-p`/`--function-context`/`--show-function` render the enclosing function:
//! git's `grep_source_1`/`show_pre_context`/`show_funcname_line` are ported,
//! including the `=` signature line, `-` body lines and cross-file `--`/`--break`
//! separators, over git's fallback funcname heuristic (a line that begins an
//! identifier). A path whose `diff` attribute names a driver carrying a funcname
//! regex — a built-in userdiff driver or a configured `funcname`/`xfuncname` —
//! would drive git off its regex tables instead, which this port does not
//! reproduce, so such a matching file is refused rather than approximated.
//!
//! Context lines are covered: `-A`/`-B`/`-C` (and `--after-context`/
//! `--before-context`/`--context`/`-<num>`) render the surrounding lines with
//! git's `-` context prefix and `--` hunk separators. `--recurse-submodules` is
//! accepted as a no-op — a repo without populated submodules greps identically,
//! and the index walk already skips the gitlink entries git would recurse into —
//! except that `--untracked` alongside it is the fatal git makes of it.
//!
//! Full regex is supported via the `regex` crate (byte-oriented): `-F` literals,
//! `-E`/ERE and `-P`/Perl pass through, and `-G`/BRE is translated by swapping
//! which of `( ) { } + ? |` are escaped. A `{`/`}` that forms no valid interval
//! is literalised, matching git's POSIX leniency; a genuinely malformed pattern
//! is the `fatal` (exit 128) git makes of it.
//!
//! `<tree>`/revision arguments are searched too: each named tree is walked and
//! its blobs greped, with git's `<rev>:<path>` naming, path-order output, pathspec
//! and current-directory scoping, and the `both --cached and trees are given`
//! fatal. `-f`/`--file` reads patterns from a file (blank lines skipped, as git),
//! `--all-match` gates a file on every pattern matching it, `--heading`/`--break`
//! reshape the grouping, and `--color` emits git's default ANSI colouring.
//!
//! Not covered: `-O`/`--open-files-in-pager` (an interactive pager), and — within
//! the function-context renderers — funcname detection driven by git's built-in
//! userdiff regex tables (see above; such files are refused, not approximated).
//!
//! The function-context renderers only shape the *rendering* of a match, so they
//! are accepted during parsing (git itself diagnoses a missing pattern before it
//! looks at them). When nothing matched there is nothing for them to shape, so the
//! empty output and exit code 1 are git's answer exactly.

use anyhow::{bail, Result};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};

/// git's `FIRST_FEW_BYTES`: only this much of a file is scanned for NUL when
/// deciding whether it is binary (`buffer_is_binary()` in `xdiff-interface.c`).
const FIRST_FEW_BYTES: usize = 8000;

/// Which regex dialect the patterns were written in. Only the subset of each
/// dialect that is a plain literal is executable here; see [`literal_of`].
#[derive(Clone, Copy, PartialEq)]
enum Dialect {
    Basic,
    Extended,
    Fixed,
    Perl,
}

/// Parsed command-line options for a single `grep` invocation.
struct Opts {
    invert: bool,         // -v
    ignore_case: bool,    // -i
    word: bool,           // -w
    text: bool,           // -a: treat binary files as text
    no_binary: bool,      // -I: never match in binary files
    line_number: bool,    // -n
    column: bool,         // --column
    files_with: bool,     // -l/--files-with-matches/--name-only
    files_without: bool,  // -L/--files-without-match
    count: bool,          // -c
    quiet: bool,          // -q
    nul: bool,            // -z
    only_matching: bool,  // -o
    show_names: bool,     // -h clears, -H sets (default: on)
    full_name: bool,      // --full-name
    cached: bool,         // --cached
    untracked: bool,      // --untracked
    no_index: bool,       // --no-index (`--index` turns it back off)
    /// `--[no-]exclude-standard`. git leaves this unset by default and resolves
    /// it to whether an index is being consulted, so it is on everywhere except
    /// under `--no-index`; see the resolution in [`grep`].
    exclude_standard: bool,
    max_count: i64,       // -m/--max-count, -1 = unlimited
    max_depth: i64,       // --max-depth, -1 = unlimited
    heading: bool,        // --heading: file name on its own line above its matches
    brk: bool,            // --break: blank line between the matches of different files
    color: bool,          // --color: wrap the output components in git's ANSI colours
    show_function: bool,  // -p/--show-function: show the enclosing function's signature line
    funcbody: bool,       // -W/--function-context: show the whole enclosing function body
}

/// Where a searchable candidate's bytes come from: a worktree file read by path
/// (the default and `--untracked`/`--no-index` cases), or a blob read by id
/// (`--cached`, and every `<tree>`/revision search).
enum Source {
    Work(BString),
    Blob(gix::hash::ObjectId),
}

// git's default `color.grep.*` slots, as the escape sequences it emits for each
// output component (`grep.c`/`color.c`): filename magenta, separators cyan, line
// and column numbers green, a match bold red, and a reset after each field.
const C_FILENAME: &[u8] = b"\x1b[35m";
const C_SEP: &[u8] = b"\x1b[36m";
const C_LINENO: &[u8] = b"\x1b[32m";
const C_MATCH: &[u8] = b"\x1b[1;31m";
const C_RESET: &[u8] = b"\x1b[m";

/// State gathered during option parsing that is resolved after the "no pattern
/// given" diagnosis (which git makes first, before looking at these).
#[derive(Default)]
struct Deferred {
    /// Records that `--recurse-submodules` was requested, for the `--untracked`
    /// incompatibility check; the flag is otherwise a no-op here.
    set_changing: Option<String>,
    all_match: bool,
}

/// One token of the `--and`/`--or`/`--not`/`(`/`)` boolean grammar, in the order
/// git's `append_grep_pattern` records them. `--or` is git's default operator and
/// records nothing (implicit adjacency is OR); every `Atom` also feeds the flat
/// pattern list that drives highlighting, `-o` and the `--column` colour.
enum Tok {
    Atom(String),
    And,
    Not,
    Open,
    Close,
}

/// A compiled boolean expression over the atom matchers, mirroring git's
/// `grep_expr` tree. `Atom(i)` indexes the per-atom matcher list built in token
/// order (the same order as the flat `patterns` list).
enum Expr {
    Atom(usize),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

/// `git grep` — print lines matching a pattern.
///
/// Supported flags (output byte-for-byte identical to stock git for these):
///   * source: default (tracked files in the worktree), `--cached`,
///     `--untracked`, `--no-index`/`--index`, `--[no-]exclude-standard`,
///     `<tree>`/`<revision>` arguments
///   * matching: `-i`, `-v`, `-w`, `-F`/`--fixed-strings`, `-E`, `-G`, `-P`,
///     `-e <pattern>` (repeatable; patterns are OR'd), `-f`/`--file <file>`,
///     `--all-match`, `-m`/`--max-count`, `--and`/`--or`/`--not`/`( … )`
///   * binary: `-a`/`--text`, `-I`
///   * scope: `--max-depth`, `-r`/`--recursive`, `--no-recursive`
///   * output: `-n`, `--column`, `-l`/`--files-with-matches`/`--name-only`,
///     `-L`/`--files-without-match`, `-c`/`--count`, `-q`/`--quiet`, `-o`,
///     `-z`/`--null`, `-h`, `-H`, `--full-name`, `--heading`, `--break`,
///     `--color[=always|auto|never]`
///   * context: `-A`/`--after-context`, `-B`/`--before-context`,
///     `-C`/`--context`, `-<num>`, `-W`/`--function-context`,
///     `-p`/`--show-function`
///   * content: `--textconv` (runs a configured `diff.<driver>.textconv`)
///   * accepted no-ops: `--[no-]ext-grep`, `--threads`,
///     `--recurse-submodules`/`--no-recurse-submodules`
///   * `[--] <pathspec>...`
///
/// Exit status matches git: `0` when at least one file produced output (for
/// `-L`, when at least one file was listed), `1` when none did, `128` for a
/// fatal such as a missing pattern, and `129` for a malformed option value.
pub fn grep(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts {
        invert: false,
        ignore_case: false,
        word: false,
        text: false,
        no_binary: false,
        line_number: false,
        column: false,
        files_with: false,
        files_without: false,
        count: false,
        quiet: false,
        nul: false,
        only_matching: false,
        show_names: true,
        full_name: false,
        cached: false,
        untracked: false,
        no_index: false,
        exclude_standard: true,
        max_count: -1,
        max_depth: -1,
        heading: false,
        brk: false,
        color: false,
        show_function: false,
        funcbody: false,
    };
    // git tracks `--[no-]exclude-standard` as a tri-state; this records whether
    // the user pinned it, so the default can follow `--no-index` when they did not.
    let mut exclude_standard_explicit = false;
    let mut textconv = false;
    let mut deferred = Deferred::default();
    let mut dialect = Dialect::Basic;

    // git's `grep_config`: config-provided defaults, applied before the CLI loop
    // below overrides them (`--[no-]line-number`, `-E`/`-F`/`-G`/`-P`, …). Done
    // via a cheap early discover so it also works ahead of the main repo open.
    if let Ok(repo) = gix::discover(".") {
        let snap = repo.config_snapshot();
        if snap.boolean("grep.lineNumber") == Some(true) {
            opts.line_number = true;
        }
        if snap.boolean("grep.column") == Some(true) {
            opts.column = true;
        }
        if snap.boolean("grep.fullName") == Some(true) {
            opts.full_name = true;
        }
        // `grep.patternType` selects the dialect; `default` (or unset) falls back
        // to the legacy `grep.extendedRegexp` boolean. All CLI-overridable below.
        match snap.string("grep.patternType").map(|v| v.to_string()).as_deref() {
            Some("basic") => dialect = Dialect::Basic,
            Some("extended") => dialect = Dialect::Extended,
            Some("fixed") => dialect = Dialect::Fixed,
            Some("perl") => dialect = Dialect::Perl,
            _ => {
                if snap.boolean("grep.extendedRegexp") == Some(true) {
                    dialect = Dialect::Extended;
                }
            }
        }
    }
    let mut patterns: Vec<String> = Vec::new();
    // The boolean-grammar token stream, in git's append order: every `-e`/`-f`
    // pattern is an `Atom`, and `--and`/`--not`/`(`/`)` interleave with them.
    // `--or` records nothing (it is git's default, implicit operator).
    let mut tokens: Vec<Tok> = Vec::new();
    // git's `opt->extended`: set the moment any boolean operator or parenthesis
    // appears, which is what switches matching from the flat OR of the pattern
    // list to evaluating the expression tree.
    let mut extended = false;
    // Whether any `-e`/`-f`/`--file` was given. git suppresses its "no pattern
    // given" fatal once one was, even if the file contributed no usable pattern,
    // so an all-blank `-f` file greps for nothing (exit 1) rather than dying.
    let mut have_pattern_flag = false;
    // `-A`/`-B`/`-C`/`--after-context`/`--before-context`/`--context`/`-NUM`:
    // the number of trailing and leading lines to show around each match. git
    // sets each component independently (last assignment wins; `-C`/`-NUM` set
    // both), so they are tracked as plain counters here.
    let mut pre_context: usize = 0;
    let mut post_context: usize = 0;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        // git scans options with `PARSE_OPT_STOP_AT_NON_OPTION | KEEP_DASHDASH`:
        // scanning halts at the first non-option token, at a bare `-`, and at `--`
        // (which is left in place). Everything from that point is resolved below by
        // the same rules as git's `cmd_grep`, so an option that trails a pathspec is
        // rejected as misplaced rather than quietly accepted.
        //
        // git registers `(` and `)` as `PARSE_OPT_NODASH` options, so they are
        // recognised as grammar tokens (not as the pattern or a path) even though
        // they carry no leading dash; they must be intercepted before the
        // non-option break below.
        if a == "(" {
            tokens.push(Tok::Open);
            extended = true;
            i += 1;
            continue;
        }
        if a == ")" {
            tokens.push(Tok::Close);
            extended = true;
            i += 1;
            continue;
        }
        if a == "--" || a == "-" || !a.starts_with('-') {
            break;
        }
        if let Some(long) = a.strip_prefix("--") {
            // `--name=value` and `--name value` are both accepted by git for
            // every long option that takes one.
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            // Reads the value of an option that requires one, consuming the
            // next argument when it was not spelled inline.
            macro_rules! value {
                () => {
                    match inline.clone() {
                        Some(v) => v,
                        None => {
                            i += 1;
                            match args.get(i) {
                                Some(v) => v.clone(),
                                None => {
                                    eprintln!("error: option `{name}' requires a value");
                                    return Ok(ExitCode::from(129));
                                }
                            }
                        }
                    }
                };
            }
            match name {
                "invert-match" => opts.invert = true,
                "no-invert-match" => opts.invert = false,
                "ignore-case" => opts.ignore_case = true,
                "no-ignore-case" => opts.ignore_case = false,
                "word-regexp" => opts.word = true,
                "no-word-regexp" => opts.word = false,
                "text" => opts.text = true,
                "no-text" => opts.text = false,
                "line-number" => opts.line_number = true,
                "no-line-number" => opts.line_number = false,
                "column" => opts.column = true,
                "no-column" => opts.column = false,
                "files-with-matches" | "name-only" => opts.files_with = true,
                "no-files-with-matches" | "no-name-only" => opts.files_with = false,
                "files-without-match" => opts.files_without = true,
                "no-files-without-match" => opts.files_without = false,
                "count" => opts.count = true,
                "no-count" => opts.count = false,
                "quiet" => opts.quiet = true,
                "no-quiet" => opts.quiet = false,
                "null" => opts.nul = true,
                "no-null" => opts.nul = false,
                "only-matching" => opts.only_matching = true,
                "no-only-matching" => opts.only_matching = false,
                "full-name" => opts.full_name = true,
                "no-full-name" => opts.full_name = false,
                "cached" => opts.cached = true,
                "no-cached" => opts.cached = false,
                "untracked" => opts.untracked = true,
                "no-untracked" => opts.untracked = false,
                "exclude-standard" => {
                    opts.exclude_standard = true;
                    exclude_standard_explicit = true;
                }
                "no-exclude-standard" => {
                    opts.exclude_standard = false;
                    exclude_standard_explicit = true;
                }
                // `--index` is the negation git generates for `--no-index`.
                "no-index" => opts.no_index = true,
                "index" => opts.no_index = false,
                "extended-regexp" => dialect = Dialect::Extended,
                "basic-regexp" => dialect = Dialect::Basic,
                "fixed-strings" => dialect = Dialect::Fixed,
                "perl-regexp" => dialect = Dialect::Perl,
                "no-extended-regexp" | "no-basic-regexp" | "no-fixed-strings"
                | "no-perl-regexp" => dialect = Dialect::Basic,
                // `--recursive` and `--no-recursive` are git's spellings of
                // `--max-depth=-1` and `--max-depth=0`.
                "recursive" => opts.max_depth = -1,
                "no-recursive" => opts.max_depth = 0,
                "max-depth" => match parse_int(name, &value!()) {
                    Ok(n) => opts.max_depth = n,
                    Err(code) => return Ok(code),
                },
                "max-count" => match parse_int(name, &value!()) {
                    Ok(n) => opts.max_count = n,
                    Err(code) => return Ok(code),
                },
                // Worker-thread count cannot change the output of a
                // single-threaded search: git orders results by path either way.
                "threads" => match parse_int(name, &value!()) {
                    Ok(_) => {}
                    Err(code) => return Ok(code),
                },
                // `--no-textconv` is git's default for grep, and `--ext-grep` is
                // documented as ignored by modern builds.
                "textconv" => textconv = true,
                "no-textconv" => textconv = false,
                "ext-grep" | "no-ext-grep" => {}
                "no-heading" => opts.heading = false,
                "no-break" => opts.brk = false,
                "no-show-function" => opts.show_function = false,
                "no-function-context" => opts.funcbody = false,
                "no-recurse-submodules" | "no-all-match" => {}
                "color" => match color_wanted(inline.as_deref()) {
                    Ok(v) => opts.color = v,
                    Err(v) => {
                        eprintln!("error: option `color' expects \"always\", \"auto\", or \"never\", not \"{v}\"");
                        return Ok(ExitCode::from(129));
                    }
                },
                "no-color" => opts.color = false,
                "file" => {
                    let f = value!();
                    match read_pattern_file(&f) {
                        Ok(pats) => {
                            for p in pats {
                                tokens.push(Tok::Atom(p.clone()));
                                patterns.push(p);
                            }
                        }
                        Err(code) => return Ok(code),
                    }
                    have_pattern_flag = true;
                }
                "heading" => opts.heading = true,
                "break" => opts.brk = true,
                // git's `--show-function` (`-p`) and `--function-context` (`-W`).
                // These only reshape *rendering*, so git accepts them during
                // parsing (the missing-pattern diagnosis still fires first).
                "show-function" => opts.show_function = true,
                "function-context" => opts.funcbody = true,
                "and" => {
                    tokens.push(Tok::And);
                    extended = true;
                }
                "not" => {
                    tokens.push(Tok::Not);
                    extended = true;
                }
                // git's default operator: implicit adjacency is OR, so `--or`
                // records no token (`OPT_BOOL(0, "or", &dummy, "")` in git).
                "or" => {}
                "after-context" => {
                    match parse_context_nonneg("option `after-context'", &value!()) {
                        Ok(n) => post_context = n,
                        Err(code) => return Ok(code),
                    }
                }
                "before-context" => {
                    match parse_context_nonneg("option `before-context'", &value!()) {
                        Ok(n) => pre_context = n,
                        Err(code) => return Ok(code),
                    }
                }
                "context" => match parse_context_signed(&value!()) {
                    Ok(n) => {
                        pre_context = n;
                        post_context = n;
                    }
                    Err(code) => return Ok(code),
                },
                "no-context" | "no-after-context" | "no-before-context" => {}
                "all-match" => deferred.all_match = true,
                "recurse-submodules" => {
                    deferred.set_changing.get_or_insert_with(|| a.to_string());
                }
                _ => bail!("{}", unsupported(a)),
            }
            i += 1;
            continue;
        }

        // `-NUM` is git's shortcut for `-C NUM`: it sets both context sides.
        if a.len() > 1 && a[1..].bytes().all(|b| b.is_ascii_digit()) {
            let n = a[1..].parse::<usize>().unwrap_or(usize::MAX);
            pre_context = n;
            post_context = n;
            i += 1;
            continue;
        }

        // Short flags, possibly grouped (`-in`). A flag that takes a value
        // consumes the rest of the group as that value, or the next argument
        // when the group ends with it.
        let group: Vec<char> = a[1..].chars().collect();
        let mut c = 0;
        while c < group.len() {
            // The value of the flag at `group[c]`, per the rule above.
            macro_rules! short_value {
                ($flag:expr) => {{
                    let rest: String = group[c + 1..].iter().collect();
                    if rest.is_empty() {
                        i += 1;
                        match args.get(i) {
                            Some(v) => v.clone(),
                            None => {
                                eprintln!("error: switch `{}' requires a value", $flag);
                                return Ok(ExitCode::from(129));
                            }
                        }
                    } else {
                        rest
                    }
                }};
            }
            match group[c] {
                'i' => opts.ignore_case = true,
                'v' => opts.invert = true,
                'w' => opts.word = true,
                'a' => opts.text = true,
                'I' => opts.no_binary = true,
                'n' => opts.line_number = true,
                'l' => opts.files_with = true,
                'L' => opts.files_without = true,
                'c' => opts.count = true,
                'q' => opts.quiet = true,
                'z' => opts.nul = true,
                'o' => opts.only_matching = true,
                'h' => opts.show_names = false,
                'H' => opts.show_names = true,
                'r' => opts.max_depth = -1,
                'E' => dialect = Dialect::Extended,
                'G' => dialect = Dialect::Basic,
                'F' => dialect = Dialect::Fixed,
                'P' => dialect = Dialect::Perl,
                'p' => opts.show_function = true,
                'W' => opts.funcbody = true,
                'A' => {
                    let v = short_value!('A');
                    match parse_context_nonneg("switch `A'", &v) {
                        Ok(n) => post_context = n,
                        Err(code) => return Ok(code),
                    }
                    c = group.len();
                    continue;
                }
                'B' => {
                    let v = short_value!('B');
                    match parse_context_nonneg("switch `B'", &v) {
                        Ok(n) => pre_context = n,
                        Err(code) => return Ok(code),
                    }
                    c = group.len();
                    continue;
                }
                'C' => {
                    let v = short_value!('C');
                    match parse_context_signed(&v) {
                        Ok(n) => {
                            pre_context = n;
                            post_context = n;
                        }
                        Err(code) => return Ok(code),
                    }
                    c = group.len();
                    continue;
                }
                'm' => {
                    let v = short_value!('m');
                    match parse_int("max-count", &v) {
                        Ok(n) => opts.max_count = n,
                        Err(code) => return Ok(code),
                    }
                    c = group.len();
                    continue;
                }
                'e' => {
                    let p = short_value!('e');
                    tokens.push(Tok::Atom(p.clone()));
                    patterns.push(p);
                    have_pattern_flag = true;
                    c = group.len();
                    continue;
                }
                'f' => {
                    let f = short_value!('f');
                    match read_pattern_file(&f) {
                        Ok(pats) => {
                            for p in pats {
                                tokens.push(Tok::Atom(p.clone()));
                                patterns.push(p);
                            }
                        }
                        Err(code) => return Ok(code),
                    }
                    have_pattern_flag = true;
                    c = group.len();
                    continue;
                }
                other => bail!("{}", unsupported(&format!("-{other}"))),
            }
            c += 1;
        }
        i += 1;
    }

    // Everything option-scanning stopped at: a possible leading `--`, the pattern,
    // any revisions, a separating `--`, and the pathspecs. git's `cmd_grep` walks
    // these in a fixed order; the steps below reproduce it (see builtin/grep.c).
    let mut rest: Vec<String> = args[i..].to_vec();

    // A leading `--` with no `-e`/`-f` pattern yet cannot be separating revisions
    // from paths, so git drops it before taking the pattern.
    if patterns.is_empty() && rest.first().is_some_and(|a| a.as_str() == "--") {
        rest.remove(0);
    }
    // Without an explicit `-e`, the first unrecognised non-option token is the
    // pattern (it may itself start with `-` when it followed the dropped `--`).
    // git only does this when its whole `pattern_list` is empty, so a bare `(`
    // (which makes the list non-empty) means the pattern must come from `-e`.
    if patterns.is_empty() && tokens.is_empty() && !rest.is_empty() {
        let p = rest.remove(0);
        tokens.push(Tok::Atom(p.clone()));
        patterns.push(p);
    }
    // git diagnoses a missing pattern here, before it looks at anything else —
    // but only when no `-e`/`-f` was given at all. A `-f` file that yielded no
    // usable pattern still counts as "given", and greps for nothing instead.
    if patterns.is_empty() && !have_pattern_flag {
        eprintln!("fatal: no pattern given");
        return Ok(ExitCode::from(128));
    }

    // git resolves an unset `--exclude-standard` to whether an index is being
    // consulted, so `--no-index` searches ignored files and everything else does
    // not. This is why `git help grep` calls `--exclude-standard` "only useful"
    // with `--no-index` and `--no-exclude-standard` "only useful" with
    // `--untracked`: each is the flag that departs from its default.
    if !exclude_standard_explicit {
        opts.exclude_standard = !opts.no_index;
    }

    let repo = gix::discover(".")?;

    // Split the remaining tokens into revisions and pathspecs the way git does.
    // With a `--` present every token before it must resolve as a revision; with
    // none, revision resolution stops at the first token that is not a rev and the
    // rest are paths. Revisions are only entertained while an index is consulted
    // and `--untracked` is off.
    let allow_revs = !opts.no_index && !opts.untracked;
    let seen_dashdash = rest.iter().any(|a| a.as_str() == "--");
    let mut revs: Vec<String> = Vec::new();
    let mut path_start = rest.len();
    let mut r = 0;
    while r < rest.len() {
        let arg = rest[r].clone();
        if arg.as_str() == "--" {
            r += 1;
            path_start = r;
            break;
        }
        if !allow_revs {
            if seen_dashdash {
                eprintln!("fatal: --no-index or --untracked cannot be used with revs");
                return Ok(ExitCode::from(128));
            }
            path_start = r;
            break;
        }
        if repo.rev_parse_single(arg.as_str()).is_ok() {
            // git's `verify_non_filename`: a token that is both a revision and an
            // existing path is ambiguous unless a `--` disambiguates it.
            if !seen_dashdash && check_filename(&arg) {
                eprintln!("fatal: ambiguous argument '{arg}': both revision and filename");
                eprintln!("Use '--' to separate paths from revisions, like this:");
                eprintln!("'git <command> [<revision>...] -- [<file>...]'");
                return Ok(ExitCode::from(128));
            }
            revs.push(arg);
            r += 1;
            path_start = r;
            continue;
        }
        if seen_dashdash {
            eprintln!("fatal: unable to resolve revision: {arg}");
            return Ok(ExitCode::from(128));
        }
        path_start = r;
        break;
    }

    // Anything past the revisions is a path. Without a `--`, git verifies each and
    // rejects an option that trailed the pattern with the "must come before"
    // message; the first path is diagnosed as a possibly-misspelt revision when
    // revisions were allowed at that position.
    if !seen_dashdash {
        for j in path_start..rest.len() {
            if let Some(code) = verify_filename(&rest[j], j == path_start && allow_revs) {
                return Ok(code);
            }
        }
    }
    let positionals: Vec<String> = rest[path_start..].to_vec();

    // git zeroes `--recurse-submodules` under `--no-index` (there is no index to
    // find gitlinks in), then rejects the surviving flag alongside `--untracked`.
    // This port does not descend into populated submodules, but a repo without
    // them greps identically either way, so the flag is otherwise accepted as a
    // no-op — the index walk already skips the gitlink entries git would recurse.
    let recurse_submodules = deferred.set_changing.is_some() && !opts.no_index;
    if recurse_submodules && opts.untracked {
        eprintln!("fatal: --untracked not supported with --recurse-submodules");
        return Ok(ExitCode::from(128));
    }

    // `--max-count=0` is documented to "exit immediately with a non-zero
    // status", ahead of the source-conflict check.
    if opts.max_count == 0 {
        return Ok(ExitCode::from(1));
    }
    if let Some(code) = source_conflict(&opts) {
        return Ok(code);
    }

    // git resolves `--[no-]exclude-standard` against whether an index is being
    // consulted; pinning it explicitly is only meaningful with `--no-index` or
    // `--untracked`, so git dies here for tracked contents (the default or
    // `--cached`). This sits after the source-conflict check, matching the order
    // of the else-if chain in git's `cmd_grep`.
    if exclude_standard_explicit && !opts.no_index && !opts.untracked {
        eprintln!("fatal: --[no-]exclude-standard cannot be used for tracked contents");
        return Ok(ExitCode::from(128));
    }

    // git's regcomp failure is a fatal (exit 128); the regex crate's message
    // differs from git's regcomp wording, but that goes to stderr, which is not
    // a compatibility surface — the exit code is.
    let matcher = match Matcher::build(&patterns, dialect, opts.ignore_case, opts.word) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };

    // Under the `--and`/`--or`/`--not`/`()` grammar, matching is decided by walking
    // git's expression tree over one matcher per atom, rather than the flat OR that
    // the shared `matcher` still drives for highlighting and `-o`.
    let (expr, atom_matchers): (Option<Expr>, Vec<Matcher>) = if extended {
        let expr = match build_expr(&tokens) {
            Ok(e) => e,
            Err(msg) => {
                eprintln!("fatal: {msg}");
                return Ok(ExitCode::from(128));
            }
        };
        let mut atoms = Vec::with_capacity(patterns.len());
        for p in &patterns {
            match Matcher::build(std::slice::from_ref(p), dialect, opts.ignore_case, opts.word) {
                Ok(m) => atoms.push(m),
                Err(e) => {
                    eprintln!("fatal: {e}");
                    return Ok(ExitCode::from(128));
                }
            }
        }
        (Some(expr), atoms)
    } else {
        (None, Vec::new())
    };
    let ev = LineEval {
        matcher: &matcher,
        expr: expr.as_ref(),
        atoms: &atom_matchers,
        invert: opts.invert,
        column: opts.column,
    };

    let index = repo.open_index()?;

    // `--textconv` can only change what is searched when there is a converter to
    // run, and one exists only if some `diff.<driver>.textconv` command is
    // configured. With none, honouring the setting and ignoring it agree, so the
    // per-path resolver is only built when a converter actually exists.
    let textconv_active = textconv && has_textconv_driver(&repo);

    // The `diff` attribute resolver, shared by `--textconv` and the
    // function-context renderers; built lazily since both are uncommon.
    let mut diff_attrs = if textconv_active || opts.show_function || opts.funcbody {
        Some(DiffAttrs::new(&repo, &index)?)
    } else {
        None
    };

    let specs: Vec<BString> = positionals
        .iter()
        .map(|p| BString::from(p.as_str()))
        .collect();

    // The repo-root-relative prefix of the current directory. It scopes a bare
    // invocation to the current subtree, is the base `--max-depth` counts from
    // when no pathspec narrows it, and is stripped from printed paths unless
    // `--full-name` was given.
    let cwd_prefix: Vec<u8> = match repo.prefix()? {
        Some(p) if !p.as_os_str().is_empty() => {
            let mut b = gix::path::into_bstr(p).into_owned().to_vec();
            b.push(b'/');
            b
        }
        _ => Vec::new(),
    };
    let prefix: Option<&[u8]> = if opts.full_name || cwd_prefix.is_empty() {
        None
    } else {
        Some(cwd_prefix.as_slice())
    };

    // `--all-match` gate: one matcher per pattern. A file is searched only when
    // every pattern finds a line in it; the lines then printed remain the usual
    // OR of all patterns. Built only when more than one pattern makes it useful,
    // and only without the boolean grammar — with it, the gate is the tree's
    // top-level `--or` terms instead (`gate_terms` below).
    let gate: Vec<Matcher> = if !extended && deferred.all_match && patterns.len() > 1 {
        let mut g = Vec::with_capacity(patterns.len());
        for p in &patterns {
            match Matcher::build(std::slice::from_ref(p), dialect, opts.ignore_case, opts.word) {
                Ok(m) => g.push(m),
                Err(e) => {
                    eprintln!("fatal: {e}");
                    return Ok(ExitCode::from(128));
                }
            }
        }
        g
    } else {
        Vec::new()
    };
    // Under the boolean grammar, `--all-match` requires each top-level `--or` term
    // to match some line of the file (git's `collect_hits` walk over the OR spine).
    let gate_terms: Vec<&Expr> = if extended && deferred.all_match {
        let mut v = Vec::new();
        if let Some(e) = expr.as_ref() {
            or_terms(e, &mut v);
        }
        if v.len() > 1 { v } else { Vec::new() }
    } else {
        Vec::new()
    };

    // Every searchable candidate: its printed name (already display-formatted,
    // including any `<rev>:` prefix), its repo-relative path (for `--textconv` and
    // funcname-driver resolution), and where its bytes come from.
    let mut cands: Vec<(Vec<u8>, BString, Source)> = Vec::new();

    if revs.is_empty() {
        let mut files: Vec<(BString, Option<gix::hash::ObjectId>)> = Vec::new();
        if opts.no_index {
            collect_no_index(&repo, &index, &specs, &opts, &mut files)?;
        } else {
            // `empty_patterns_match_prefix = true` reproduces git's behaviour of
            // limiting a bare invocation to the current directory's subtree.
            let mut ps = repo.pathspec(
                true,
                &specs,
                false,
                &index,
                gix::worktree::stack::state::attributes::Source::IdMapping,
            )?;
            if let Some(iter) = ps.index_entries_with_paths(&index) {
                for (path, entry) in iter {
                    // git's `grep_cache()` only visits regular files: symlinks and
                    // gitlinks are skipped, and higher conflict stages are collapsed.
                    if entry.mode != gix::index::entry::Mode::FILE
                        && entry.mode != gix::index::entry::Mode::FILE_EXECUTABLE
                    {
                        continue;
                    }
                    if files.last().is_some_and(|(last, _)| last.as_bstr() == path) {
                        continue;
                    }
                    files.push((path.to_owned(), Some(entry.id)));
                }
            }
            if opts.untracked {
                collect_untracked(&repo, &index, &specs, &opts, &mut files)?;
            }
        }

        // The index walk is already ordered, but both directory walks emit in
        // traversal order; git prints paths sorted by their bytes either way.
        if opts.no_index || opts.untracked {
            files.sort_by(|a, b| a.0.cmp(&b.0));
            files.dedup_by(|a, b| a.0 == b.0);
        }
        apply_max_depth(&mut files, &specs, &cwd_prefix, opts.max_depth);

        for (path, id) in files {
            let name = display_name(path.as_bstr(), prefix, &opts);
            // `--cached` reads the index blob; otherwise the worktree file. A
            // cached entry with no id cannot be read, so git skips it.
            let src = if opts.cached {
                match id {
                    Some(id) => Source::Blob(id),
                    None => continue,
                }
            } else {
                Source::Work(path.clone())
            };
            cands.push((name, path, src));
        }
    } else {
        // A `<tree>`/revision search. git refuses this with `--cached` (there is
        // no worktree/index blob to prefer over the tree), then greps each tree in
        // turn, prefixing every printed path with the revision as the user spelled
        // it. The regcomp failure above already fired, matching git's order.
        if opts.cached {
            eprintln!("fatal: both --cached and trees are given");
            return Ok(ExitCode::from(128));
        }
        collect_trees(&repo, &revs, &specs, prefix, &opts, &index, &mut cands)?;
    }

    // Whether a file clears the `--all-match` gate. Without the boolean grammar,
    // every per-pattern matcher must find a line (with `-v` applied, as before);
    // with it, every top-level `--or` term must match some line, which is git's
    // `collect_hits` walk and does not apply `-v`. An empty gate admits everything.
    let passes_gate = |content: &[u8]| -> bool {
        gate.iter()
            .all(|m| lines(content).any(|l| next_match(l, m, 0).is_some() != opts.invert))
            && gate_terms.iter().all(|t| {
                lines(content).any(|l| {
                    let (mut c, mut ic) = (-1i64, -1i64);
                    eval_expr(t, &atom_matchers, l, &mut c, &mut ic, false)
                })
            })
    };

    // Context, headings and function names only reshape printed match lines, so
    // they are irrelevant to the name/count/quiet modes — and to any run that
    // found nothing, where git's output is empty and its exit code is 1 too.
    let renders_lines = !(opts.quiet || opts.files_with || opts.files_without || opts.count);

    // `-p`/`--show-function` and `-W`/`--function-context` render the enclosing
    // function. This port reproduces git's fallback funcname detection (a line
    // beginning an identifier); a path whose `diff` attribute names a driver with a
    // funcname regex would use git's built-in userdiff tables instead, which are
    // not reproduced, so such a matching file is refused rather than approximated.
    if renders_lines && (opts.show_function || opts.funcbody) {
        // Pre-scan on raw content (no textconv side effects) so the refusal fires
        // before any output is written, keeping stdout all-or-nothing.
        for (_, rela, src) in &cands {
            let Some(content) = load_content(&repo, None, false, rela.as_bstr(), src)? else {
                continue;
            };
            if !passes_gate(&content) {
                continue;
            }
            let binary = !opts.text && is_binary(&content);
            if binary {
                continue; // git shapes no function context around a binary hit
            }
            if lines(&content).any(|l| ev.matches(l)) {
                if let Some(da) = diff_attrs.as_mut() {
                    if da.has_funcname_driver(rela.as_bstr())? {
                        let flag = if opts.funcbody { "--function-context" } else { "--show-function" };
                        bail!("{}", unsupported(flag));
                    }
                }
            }
        }

        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());
        let mut any_hit = false;
        // git's cross-file "a previous file already printed" flag, which gates the
        // `--`/`--break` separators between files.
        let mut hunk_mark = false;
        for (name, rela, src) in &cands {
            let Some(content) =
                load_content(&repo, diff_attrs.as_mut(), textconv_active, rela.as_bstr(), src)?
            else {
                continue;
            };
            if !passes_gate(&content) {
                continue;
            }
            let binary = !opts.text && is_binary(&content);
            if binary && opts.no_binary {
                continue;
            }
            if binary {
                if lines(&content).any(|l| ev.matches(l)) {
                    any_hit = true;
                    // git prints the "Binary file … matches" notice for `-p` alone,
                    // but the context-bearing modes (`-W`, `-A`/`-B`/`-C`) suppress
                    // it — matching stock `git grep` exactly.
                    if !opts.funcbody && pre_context == 0 && post_context == 0 {
                        out.write_all(b"Binary file ")?;
                        out.write_all(name)?;
                        out.write_all(b" matches\n")?;
                        hunk_mark = true;
                    }
                }
                continue;
            }
            if render_funcctx(&mut out, &content, name, &ev, &opts, pre_context, post_context, hunk_mark)? {
                any_hit = true;
                hunk_mark = true;
            }
        }
        out.flush()?;
        return Ok(if any_hit {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        });
    }

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut any_hit = false;

    // With `-A`/`-B`/`-C` the printed match lines gain surrounding context and a
    // `--` hunk separator, so those runs take a dedicated renderer; every other
    // mode (including `-A0`/`-C0`, which git treats as no context) stays on the
    // plain per-line path.
    if renders_lines && (pre_context > 0 || post_context > 0) {
        // `printed_any` spans all files: git's `--` separator precedes every hunk
        // except the first one printed across the whole run, files included.
        let mut printed_any = false;
        for (name, rela, src) in &cands {
            let Some(content) =
                load_content(&repo, diff_attrs.as_mut(), textconv_active, rela.as_bstr(), src)?
            else {
                continue;
            };
            if !passes_gate(&content) {
                continue;
            }
            let binary = !opts.text && is_binary(&content);
            if binary && opts.no_binary {
                continue;
            }
            if binary {
                // git reports a binary hit through the exit status but prints no
                // context lines and no "Binary file matches" notice for it.
                if lines(&content).any(|l| ev.matches(l)) {
                    any_hit = true;
                }
                continue;
            }
            if render_context(
                &mut out,
                &content,
                name,
                &ev,
                &opts,
                pre_context,
                post_context,
                &mut printed_any,
            )? {
                any_hit = true;
            }
        }
        out.flush()?;
        return Ok(if any_hit {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        });
    }

    // `emitted_any` spans all files for `--break`/`--heading`: the blank line and
    // heading precede every file's first emitted line except the first overall.
    let mut emitted_any = false;
    for (name, rela, src) in &cands {
        let Some(content) =
            load_content(&repo, diff_attrs.as_mut(), textconv_active, rela.as_bstr(), src)?
        else {
            continue;
        };
        if !passes_gate(&content) {
            continue;
        }
        let binary = !opts.text && is_binary(&content);
        if binary && opts.no_binary {
            continue;
        }
        if search_file(&mut out, &content, name, binary, &ev, &opts, &mut emitted_any)? {
            any_hit = true;
            if opts.quiet {
                break;
            }
        }
    }

    out.flush()?;
    Ok(if any_hit {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Read the `-f`/`--file` pattern file: each non-empty line is one pattern (OR'd
/// with the rest, exactly like a repeated `-e`). git skips blank lines rather
/// than letting them match every line, and a file that yields none still counts
/// as a pattern having been given. On an unreadable file git dies (exit 128);
/// the exact stderr wording is not a compatibility surface, only the code and the
/// empty stdout are.
fn read_pattern_file(path: &str) -> Result<Vec<String>, ExitCode> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect()),
        Err(e) => {
            eprintln!("fatal: cannot open '{path}': {e}");
            Err(ExitCode::from(128))
        }
    }
}

/// Collect the searchable blobs of each `<tree>`/revision argument, in the order
/// the revisions were given and then by path within each. Every candidate's name
/// carries git's `<rev>:` prefix (the revision as spelled) ahead of the path, and
/// the pathspec — which folds in the current-directory prefix — limits the walk
/// just as it does for the worktree.
fn collect_trees(
    repo: &gix::Repository,
    revs: &[String],
    specs: &[BString],
    prefix: Option<&[u8]>,
    opts: &Opts,
    index: &gix::index::File,
    cands: &mut Vec<(Vec<u8>, BString, Source)>,
) -> Result<()> {
    for rev in revs {
        let tree = repo.rev_parse_single(rev.as_str())?.object()?.peel_to_tree()?;
        let mut entries = tree.traverse().breadthfirst.files()?;
        // git prints tree matches in path order; the breadth-first walk yields
        // files-before-directories per level, so re-sort by the full path.
        entries.sort_by(|a, b| a.filepath.cmp(&b.filepath));
        let mut ps = repo.pathspec(
            true,
            specs,
            false,
            index,
            gix::worktree::stack::state::attributes::Source::IdMapping,
        )?;
        let rev_prefix = format!("{rev}:").into_bytes();
        for entry in entries {
            // git's `grep_tree()` greps regular blobs only: trees, symlinks and
            // gitlinks are skipped, matching the worktree/index walk.
            if !entry.mode.is_blob() {
                continue;
            }
            if !ps.is_included(entry.filepath.as_bstr(), Some(false)) {
                continue;
            }
            let mut name = rev_prefix.clone();
            name.extend_from_slice(&display_name(entry.filepath.as_bstr(), prefix, opts));
            cands.push((name, entry.filepath.clone(), Source::Blob(entry.oid)));
        }
    }
    Ok(())
}

/// git's mutual-exclusion diagnosis for the three source selectors, including
/// the three-way wording it uses when all of them were given at once.
fn source_conflict(opts: &Opts) -> Option<ExitCode> {
    let msg = match (opts.no_index, opts.untracked, opts.cached) {
        (true, true, true) => {
            "options '--no-index', '--untracked', and '--cached' cannot be used together"
        }
        (true, true, false) => "options '--no-index' and '--untracked' cannot be used together",
        (true, false, true) => "options '--no-index' and '--cached' cannot be used together",
        (false, true, true) => "options '--untracked' and '--cached' cannot be used together",
        _ => return None,
    };
    eprintln!("fatal: {msg}");
    Some(ExitCode::from(128))
}

/// git's `verify_filename()`. A leftover token sitting in path position that
/// starts with `-` is a misplaced option — git dies with the "must come before
/// non-option arguments" message rather than treating it as a path. Otherwise it
/// has to look like a pathspec or name an existing path, or git dies before
/// searching anything. `diagnose_misspelt_rev` picks git's wording: the
/// "ambiguous argument" form when a revision was still admissible at this
/// position (the first path, with revs allowed), the plainer "no such path" form
/// otherwise (subsequent paths, or when revisions were never in play — including
/// every `--no-index`/`--untracked` path).
///
/// Returns the exit code to stop with, having already reported it, or `None`
/// when the argument is acceptable.
///
/// Paths are resolved against the current directory, which is where git resolves
/// them from too for every form but `:/<path>`: that one is root-relative, and
/// git can say so because it has already changed directory to the root by this
/// point. The two agree whenever grep is run from the root.
fn verify_filename(arg: &str, diagnose_misspelt_rev: bool) -> Option<ExitCode> {
    if arg.starts_with('-') {
        eprintln!("fatal: option '{arg}' must come before non-option arguments");
        return Some(ExitCode::from(128));
    }
    if looks_like_pathspec(arg) || check_filename(arg) {
        return None;
    }
    if diagnose_misspelt_rev {
        eprintln!(
            "fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree."
        );
        eprintln!("Use '--' to separate paths from revisions, like this:");
        eprintln!("'git <command> [<revision>...] -- [<file>...]'");
    } else {
        eprintln!("fatal: {arg}: no such path in the working tree.");
        eprintln!("Use 'git <command> -- <path>...' to specify paths that do not exist locally.");
    }
    Some(ExitCode::from(128))
}

/// git's `check_filename()`: whether `arg` names something that exists in the
/// worktree. It strips the leading pathspec magic that still leaves a path behind
/// and stats what remains. Magic with nothing after it counts as existing without
/// a stat — `:/` names the root, and excluding everything with a bare `:!`/`:^`
/// is pointless but legal. A bare empty argument gets no such exemption: it
/// reaches the stat and fails it, which is why `git grep --no-index <pattern> ""`
/// is a fatal rather than a match-nothing.
fn check_filename(arg: &str) -> bool {
    let path = match [":/", ":!", ":^"]
        .into_iter()
        .find_map(|magic| arg.strip_prefix(magic))
    {
        Some("") => return true,
        Some(rest) => rest,
        None => arg,
    };
    std::fs::symlink_metadata(path).is_ok()
}

/// git's `looks_like_pathspec()`: raw pathspec magic, or an unescaped glob
/// metacharacter, means the argument is a pattern rather than a name to stat.
fn looks_like_pathspec(arg: &str) -> bool {
    if arg.starts_with(":(") && arg.contains(')') {
        return true;
    }
    let mut escaped = false;
    for b in arg.bytes() {
        if escaped {
            escaped = false;
        } else if b == b'\\' {
            escaped = true;
        } else if matches!(b, b'*' | b'?' | b'[') {
            return true;
        }
    }
    false
}

/// Whether any `diff.<driver>.textconv` command is configured.
///
/// A path's content is converted only when its `diff` attribute names a driver
/// that carries such a command, so if no driver carries one then no path can be
/// affected and `--textconv` cannot change a single byte of the output.
fn has_textconv_driver(repo: &gix::Repository) -> bool {
    let snapshot = repo.config_snapshot();
    // Reduced to a bool while `snapshot` is still alive: the section iterator
    // borrows it, and returning the expression directly would let the borrow
    // outlive the binding.
    let found = snapshot
        .plumbing()
        .sections_by_name("diff")
        .into_iter()
        .flatten()
        .any(|section| {
            section
                .header()
                .subsection_name()
                .is_some_and(|name| !name.is_empty())
                && section.value("textconv").is_some()
        });
    found
}

/// Collect every file under `specs` for `--no-index`, which searches the
/// filesystem rather than the index.
///
/// The index is still handed to the walk, but only so gitoxide can classify what
/// it finds; nothing is selected from it. Tracked, untracked and — unless
/// `--exclude-standard` was given — ignored files all qualify, which is why the
/// default flips: git resolves an unset `--exclude-standard` to `use_index`.
/// Nested repositories are entered, since a checked-out submodule is just a
/// directory to a filesystem walk, and `.git` is pruned by the walk itself.
fn collect_no_index(
    repo: &gix::Repository,
    index: &gix::index::File,
    specs: &[BString],
    opts: &Opts,
    files: &mut Vec<(BString, Option<gix::hash::ObjectId>)>,
) -> Result<()> {
    let options = repo
        .dirwalk_options()?
        .empty_patterns_match_prefix(true)
        .recurse_repositories(true)
        .emit_tracked(true)
        .emit_untracked(gix::dir::walk::EmissionMode::Matching)
        .emit_ignored(
            (!opts.exclude_standard).then_some(gix::dir::walk::EmissionMode::Matching),
        );
    let mut collect = gix::dir::walk::delegate::Collect::default();
    let should_interrupt = std::sync::atomic::AtomicBool::default();
    repo.dirwalk(index, specs, &should_interrupt, options, &mut collect)?;

    for (entry, _) in collect.unorded_entries {
        if entry.disk_kind != Some(gix::dir::entry::Kind::File) {
            continue;
        }
        match entry.status {
            gix::dir::entry::Status::Tracked | gix::dir::entry::Status::Untracked => {}
            gix::dir::entry::Status::Ignored(_) if !opts.exclude_standard => {}
            _ => continue,
        }
        // `None` for the id: with `--cached` ruled out, content comes from disk.
        files.push((entry.rela_path, None));
    }
    Ok(())
}

/// Add the untracked files under `specs` to `files`, as `--untracked` asks.
///
/// `--exclude-standard` is on by default, matching git, so ignored files stay
/// out unless `--no-exclude-standard` was given. Only regular files are added:
/// directories, symlinks and nested repositories are not searchable content,
/// exactly as on the index path.
fn collect_untracked(
    repo: &gix::Repository,
    index: &gix::index::File,
    specs: &[BString],
    opts: &Opts,
    files: &mut Vec<(BString, Option<gix::hash::ObjectId>)>,
) -> Result<()> {
    let options = repo
        .dirwalk_options()?
        .empty_patterns_match_prefix(true)
        .emit_untracked(gix::dir::walk::EmissionMode::Matching)
        .emit_ignored(
            (!opts.exclude_standard).then_some(gix::dir::walk::EmissionMode::Matching),
        );
    let mut collect = gix::dir::walk::delegate::Collect::default();
    let should_interrupt = std::sync::atomic::AtomicBool::default();
    repo.dirwalk(index, specs, &should_interrupt, options, &mut collect)?;

    for (entry, _) in collect.unorded_entries {
        if entry.disk_kind != Some(gix::dir::entry::Kind::File) {
            continue;
        }
        match entry.status {
            gix::dir::entry::Status::Untracked => {}
            gix::dir::entry::Status::Ignored(_) if !opts.exclude_standard => {}
            _ => continue,
        }
        files.push((entry.rela_path, None));
    }
    Ok(())
}

/// Drop candidates that lie deeper than `--max-depth` allows.
///
/// git counts depth from the end of the pathspec that selected the file — with
/// no pathspec, from the repo-relative current directory — so `--max-depth=0`
/// with `-- a` keeps `a/z.txt` but not `a/b/z.txt`. Per `git help grep`, the
/// option "is ignored if <pathspec> contains active wildcards".
fn apply_max_depth(
    files: &mut Vec<(BString, Option<gix::hash::ObjectId>)>,
    specs: &[BString],
    cwd_prefix: &[u8],
    max_depth: i64,
) {
    let Ok(max_depth) = usize::try_from(max_depth) else {
        return;
    };
    let has_wildcard = specs.iter().any(|s| {
        let bytes: &[u8] = s;
        bytes.iter().any(|&b| matches!(b, b'*' | b'?' | b'[' | b':'))
    });
    if has_wildcard {
        return;
    }

    // The directory each pathspec descends from; a spec naming a file exactly
    // sits at depth zero, which the `== spec` test below covers.
    let bases: Vec<Vec<u8>> = if specs.is_empty() {
        vec![cwd_prefix.to_vec()]
    } else {
        specs
            .iter()
            .map(|s| {
                let bytes: &[u8] = s;
                let mut b = bytes.to_vec();
                if !b.is_empty() && b.last() != Some(&b'/') {
                    b.push(b'/');
                }
                b
            })
            .collect()
    };

    files.retain(|(path, _)| {
        let path: &[u8] = path;
        if specs.iter().any(|s| {
            let spec: &[u8] = s;
            spec == path
        }) {
            return true;
        }
        bases.iter().any(|base| {
            path.len() >= base.len()
                && &path[..base.len()] == base.as_slice()
                && path[base.len()..].iter().filter(|&&b| b == b'/').count() <= max_depth
        })
    });
}

/// Whether colourised output was asked for. `Err` carries an unrecognised
/// `--color=<when>` value so the caller can report it the way git does.
fn color_wanted(when: Option<&str>) -> Result<bool, String> {
    match when {
        None | Some("always") | Some("true") => Ok(true),
        Some("never") | Some("false") => Ok(false),
        Some("auto") => Ok(std::io::stdout().is_terminal()),
        Some(other) => Err(other.to_string()),
    }
}

/// Parse an option value that git declares as an integer, reporting git's own
/// usage error (exit 129) when it is not one.
fn parse_int(name: &str, value: &str) -> Result<i64, ExitCode> {
    value.parse::<i64>().map_err(|_| {
        eprintln!("error: option `{name}' expects an integer value with an optional k/m/g suffix");
        ExitCode::from(129)
    })
}

/// Parse a `-A`/`-B` (`--after-context`/`--before-context`) value, which git
/// requires to be a non-negative integer. `spelled` is how git names the flag in
/// its usage error — `switch `A'` for the short form, `option `after-context'`
/// for the long — reported at exit 129 exactly as git does.
fn parse_context_nonneg(spelled: &str, value: &str) -> Result<usize, ExitCode> {
    match value.parse::<i64>() {
        Ok(n) if n >= 0 => Ok(n as usize),
        _ => {
            eprintln!(
                "error: {spelled} expects a non-negative integer value with an optional k/m/g suffix"
            );
            Err(ExitCode::from(129))
        }
    }
}

/// Parse a `-C`/`--context` value. git parses this one as a plain signed number
/// (always naming it `switch `C'`), so a negative value is accepted and means
/// unlimited context rather than being rejected the way `-A`/`-B` are.
fn parse_context_signed(value: &str) -> Result<usize, ExitCode> {
    match value.parse::<i64>() {
        Ok(n) if n < 0 => Ok(usize::MAX),
        Ok(n) => Ok(n as usize),
        Err(_) => {
            eprintln!("error: switch `C' expects a numerical value");
            Err(ExitCode::from(129))
        }
    }
}

/// Decides whether a line is a match and where its printed column falls, in a way
/// that is shared by every output mode. When no boolean grammar is in play this is
/// the flat matcher exactly as before; with `--and`/`--or`/`--not`/`()` the
/// decision comes from evaluating git's `grep_expr` tree, while highlighting,
/// `-o` and the coloured column keep using the flat `matcher` — which is git's
/// own split, since its `next_match` walks the whole flat pattern list.
struct LineEval<'a> {
    /// The OR of every atom pattern: drives `-o`, `--color` highlighting and the
    /// match-body rendering, mirroring git's flat `opt->pattern_list` scan.
    matcher: &'a Matcher,
    /// The compiled boolean expression, present only under the boolean grammar.
    expr: Option<&'a Expr>,
    /// One matcher per atom, indexed by [`Expr::Atom`]; empty when `expr` is None.
    atoms: &'a [Matcher],
    invert: bool,
    /// `--column`: git disables the AND/OR short-circuit so the earliest atom on a
    /// line is found even when a later branch would have satisfied the match first.
    column: bool,
}

impl LineEval<'_> {
    /// Whether `line` matches, after `-v` inversion.
    fn matches(&self, line: &[u8]) -> bool {
        self.test(line).0
    }

    /// The match decision and the 1-based column git would print for `line`.
    fn test(&self, line: &[u8]) -> (bool, usize) {
        match self.expr {
            Some(e) => {
                let mut col: i64 = -1;
                let mut icol: i64 = -1;
                let hit = eval_expr(e, self.atoms, line, &mut col, &mut icol, self.column);
                // git: `cno = opt->invert ? icol : col`, and a negative result
                // (no atom on the shown side) is clamped so the column reads 1.
                let cno = if self.invert { icol } else { col };
                let col1 = if cno < 0 { 1 } else { cno as usize + 1 };
                (hit != self.invert, col1)
            }
            None => {
                let first = next_match(line, self.matcher, 0);
                (first.is_some() != self.invert, first.map_or(0, |(s, _)| s) + 1)
            }
        }
    }
}

/// git's `match_expr_eval` with `collect_hits == 0`. `col`/`icol` accumulate the
/// least atom start seen on the positive/negative side (a `--not` swaps them, so a
/// negated atom's position never lands in the printed column). `full` disables the
/// AND/OR short-circuit, which git does under `--column` so the earliest atom is
/// always found.
fn eval_expr(e: &Expr, atoms: &[Matcher], line: &[u8], col: &mut i64, icol: &mut i64, full: bool) -> bool {
    match e {
        Expr::Atom(i) => match atoms[*i].find_at(line, 0) {
            Some((so, _)) => {
                let so = so as i64;
                if *col < 0 || so < *col {
                    *col = so;
                }
                true
            }
            None => false,
        },
        // The swap of `col`/`icol` is git's mechanism for keeping a negated atom's
        // column out of the value it prints.
        Expr::Not(x) => !eval_expr(x, atoms, line, icol, col, full),
        Expr::And(l, r) => {
            let mut h = eval_expr(l, atoms, line, col, icol, full);
            if h || full {
                h &= eval_expr(r, atoms, line, col, icol, full);
            }
            h
        }
        Expr::Or(l, r) => {
            if !full {
                eval_expr(l, atoms, line, col, icol, full)
                    || eval_expr(r, atoms, line, col, icol, full)
            } else {
                let h = eval_expr(l, atoms, line, col, icol, full);
                h | eval_expr(r, atoms, line, col, icol, full)
            }
        }
    }
}

/// The top-level `--or` terms of the expression, in order. `--all-match` requires
/// each of these to match at least one line of the file (git's `collect_hits`
/// walk over the OR spine), which is the file-level gate the flag documents.
fn or_terms<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    match e {
        Expr::Or(l, r) => {
            or_terms(l, out);
            or_terms(r, out);
        }
        _ => out.push(e),
    }
}

/// Recursive-descent parser for git's `--and`/`--or`/`--not`/`()` grammar, ported
/// from `compile_pattern_{or,and,not,atom}` in grep.c. Atoms are numbered in the
/// order they are consumed, which is the order they appear in the flat `patterns`
/// list, so `Expr::Atom(i)` indexes both the atom-matcher list and that list.
struct TokParser<'a> {
    toks: &'a [Tok],
    pos: usize,
    atom: usize,
}

impl TokParser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn atom_expr(&mut self) -> Result<Option<Expr>, String> {
        match self.peek() {
            Some(Tok::Atom(_)) => {
                let i = self.atom;
                self.atom += 1;
                self.pos += 1;
                Ok(Some(Expr::Atom(i)))
            }
            Some(Tok::Open) => {
                self.pos += 1;
                let x = self.or_expr()?;
                match self.peek() {
                    Some(Tok::Close) => {
                        self.pos += 1;
                        Ok(x)
                    }
                    _ => Err("unmatched ( for expression group".into()),
                }
            }
            _ => Ok(None),
        }
    }

    fn not_expr(&mut self) -> Result<Option<Expr>, String> {
        if matches!(self.peek(), Some(Tok::Not)) {
            self.pos += 1;
            if self.peek().is_none() {
                return Err("--not not followed by pattern expression".into());
            }
            return match self.not_expr()? {
                Some(x) => Ok(Some(Expr::Not(Box::new(x)))),
                None => Err("--not not followed by pattern expression".into()),
            };
        }
        self.atom_expr()
    }

    fn and_expr(&mut self) -> Result<Option<Expr>, String> {
        let x = self.not_expr()?;
        if matches!(self.peek(), Some(Tok::And)) {
            if x.is_none() {
                return Err("--and not preceded by pattern expression".into());
            }
            self.pos += 1;
            if self.peek().is_none() {
                return Err("--and not followed by pattern expression".into());
            }
            return match self.and_expr()? {
                Some(y) => Ok(Some(Expr::And(Box::new(x.unwrap()), Box::new(y)))),
                None => Err("--and not followed by pattern expression".into()),
            };
        }
        Ok(x)
    }

    fn or_expr(&mut self) -> Result<Option<Expr>, String> {
        let x = self.and_expr()?;
        // git ORs two adjacent expressions with no explicit operator between them,
        // stopping at a closing paren or the end of the token stream.
        if x.is_some() && self.peek().is_some() && !matches!(self.peek(), Some(Tok::Close)) {
            return match self.or_expr()? {
                Some(y) => Ok(Some(Expr::Or(Box::new(x.unwrap()), Box::new(y)))),
                None => Err("not a pattern expression".into()),
            };
        }
        Ok(x)
    }
}

/// Compile the boolean-grammar token stream into git's expression tree, reporting
/// the fatal (exit 128) git makes of a malformed one. A leftover token after a
/// complete parse is git's `incomplete pattern expression` — most often a stray
/// `)`.
fn build_expr(toks: &[Tok]) -> Result<Expr, String> {
    let mut p = TokParser { toks, pos: 0, atom: 0 };
    let x = p.or_expr()?;
    if p.pos < toks.len() {
        let leftover = match &toks[p.pos] {
            Tok::Atom(s) => s.as_str(),
            Tok::Close => ")",
            Tok::Open => "(",
            Tok::And => "--and",
            Tok::Not => "--not",
        };
        return Err(format!("incomplete pattern expression group: {leftover}"));
    }
    x.ok_or_else(|| "no pattern given".to_string())
}

/// git's fallback `match_funcname` (used when the path has no diff driver with a
/// funcname pattern): a non-empty line whose first byte begins an identifier is a
/// function-signature line.
fn is_funcname_line(line: &[u8]) -> bool {
    match line.first() {
        Some(&b) => b.is_ascii_alphabetic() || b == b'_' || b == b'$',
        None => false,
    }
}

/// git's `is_empty_line`: a line that is only whitespace.
fn is_blank_line(line: &[u8]) -> bool {
    line.iter().all(|b| b.is_ascii_whitespace())
}

/// The set of git's built-in userdiff drivers that carry a funcname pattern (from
/// `userdiff.c`). A path whose `diff` attribute names one of these — or a
/// user-configured driver with a `funcname`/`xfuncname` — would drive git's
/// funcname detection off these regex tables, which this port does not reproduce;
/// such a file is refused rather than approximated (see [`grep`]).
const BUILTIN_FUNCNAME_DRIVERS: &[&str] = &[
    "ada", "bash", "bibtex", "cpp", "csharp", "css", "dts", "elixir", "fortran",
    "fountain", "golang", "html", "java", "kotlin", "markdown", "matlab", "objc",
    "pascal", "perl", "php", "python", "ruby", "rust", "scheme", "tex",
];

/// Resolves the `diff` attribute of a path so `--textconv` and the function-context
/// renderers can find the driver git would use. Built once per run over the same
/// attribute stack the rest of grep consults.
struct DiffAttrs<'r> {
    repo: &'r gix::Repository,
    stack: gix::AttributeStack<'r>,
    outcome: gix::attrs::search::Outcome,
}

impl<'r> DiffAttrs<'r> {
    fn new(repo: &'r gix::Repository, index: &gix::index::File) -> Result<Self> {
        let stack = repo.attributes_only(
            index,
            gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
        )?;
        Ok(Self { repo, stack, outcome: gix::attrs::search::Outcome::default() })
    }

    /// The value of the path's `diff` attribute, i.e. the diff driver name, or
    /// `None` when it is unset, unspecified, or a bare boolean.
    fn driver_of(&mut self, rela: &BStr) -> Result<Option<String>> {
        let mode = Some(gix::index::entry::Mode::FILE);
        // The first descent loads the `.gitattributes` along the path so the
        // collection knows every attribute name before the outcome is sized.
        self.stack.at_entry(rela, mode)?;
        self.outcome
            .initialize_with_selection(self.stack.attributes_collection(), ["diff"]);
        let platform = self.stack.at_entry(rela, mode)?;
        platform.matching_attributes(&mut self.outcome);
        for m in self.outcome.iter_selected() {
            if let gix::attrs::StateRef::Value(v) = m.assignment.state {
                return Ok(Some(String::from_utf8_lossy(v.as_bstr().as_bytes()).into_owned()));
            }
        }
        Ok(None)
    }

    /// The `diff.<driver>.textconv` command configured for the path, if any.
    fn textconv_cmd(&mut self, rela: &BStr) -> Result<Option<String>> {
        let Some(drv) = self.driver_of(rela)? else {
            return Ok(None);
        };
        let snap = self.repo.config_snapshot();
        Ok(snap
            .string(format!("diff.{drv}.textconv").as_str())
            .map(|v| v.to_string()))
    }

    /// Whether the path's diff driver carries a funcname pattern this port cannot
    /// reproduce — a built-in funcname driver, or a user driver with a configured
    /// `funcname`/`xfuncname`.
    fn has_funcname_driver(&mut self, rela: &BStr) -> Result<bool> {
        let Some(drv) = self.driver_of(rela)? else {
            return Ok(false);
        };
        if BUILTIN_FUNCNAME_DRIVERS.contains(&drv.as_str()) {
            return Ok(true);
        }
        let snap = self.repo.config_snapshot();
        Ok(snap.string(format!("diff.{drv}.funcname").as_str()).is_some()
            || snap.string(format!("diff.{drv}.xfuncname").as_str()).is_some())
    }
}

/// git's `run_textconv`: write the blob to a temp file and run the configured
/// converter over it via the shell (`<cmd> "$@"` with the temp path as `$1`),
/// returning its stdout as the bytes to search. A failing converter is the fatal
/// git makes of it.
fn run_textconv(cmd: &str, content: &[u8]) -> Result<Vec<u8>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "zvcs-grep-textconv-{}-{}",
        std::process::id(),
        TEXTCONV_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    {
        let mut f = std::fs::File::create(&path)?;
        f.write_all(content)?;
    }
    // `sh -c '<cmd> "$@"' <cmd> <tmp>` reproduces git's `use_shell` invocation,
    // passing the temp path as the single positional argument to the command.
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{cmd} \"$@\""))
        .arg(cmd)
        .arg(&path)
        .output();
    let _ = std::fs::remove_file(&path);
    match output {
        Ok(o) if o.status.success() => Ok(o.stdout),
        Ok(_) | Err(_) => {
            bail!("unable to read files to diff: textconv command '{cmd}' failed")
        }
    }
}

/// A process-wide counter that keeps the textconv temp-file names distinct.
static TEXTCONV_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Read a candidate's bytes, applying `--textconv` when a converter is configured
/// for its path. `None` means a worktree file is gone, which git ignores; a blob
/// (index or tree) is always present. With `textconv_active` false (or no
/// converter for the path) the raw content is returned unchanged.
fn load_content(
    repo: &gix::Repository,
    diff_attrs: Option<&mut DiffAttrs>,
    textconv_active: bool,
    rela: &BStr,
    src: &Source,
) -> Result<Option<Vec<u8>>> {
    let raw = match src {
        Source::Work(path) => {
            let Some(abs) = repo.workdir_path(path.as_bstr()) else {
                return Ok(None);
            };
            match std::fs::read(&abs) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            }
        }
        Source::Blob(id) => repo.find_object(*id)?.data.clone(),
    };
    if textconv_active {
        if let Some(da) = diff_attrs {
            if let Some(cmd) = da.textconv_cmd(rela)? {
                return Ok(Some(run_textconv(&cmd, &raw)?));
            }
        }
    }
    Ok(Some(raw))
}

/// The per-file rendering context for the function-context modes, so the emit and
/// pre-context helpers below can share one bundle instead of a dozen arguments.
struct Fc<'a> {
    ev: &'a LineEval<'a>,
    opts: &'a Opts,
    name: &'a [u8],
    lv: &'a [&'a [u8]],
    /// The 1-based match column for each line, `0` on a non-match line.
    col1: &'a [usize],
    pre: usize,
    post: usize,
    /// git's cross-file "a previous file already printed" flag.
    hunk_mark: bool,
}

impl Fc<'_> {
    fn is_func(&self, idx: usize) -> bool {
        idx < self.lv.len() && is_funcname_line(self.lv[idx])
    }
    fn is_empty(&self, idx: usize) -> bool {
        is_blank_line(self.lv[idx])
    }
}

/// Emit one line with git's `show_line`: the `--`/`--break` hunk separator, then
/// the `<name><sign><lno><sign>[<col><sign>]` header, then the body. `sign` is `:`
/// for a match, `-` for context, `=` for a funcname line. `last_shown` is the
/// 1-based line last printed (0 = none), updated here.
fn fc_emit<W: Write>(out: &mut W, cx: &Fc, idx: usize, sign: u8, last_shown: &mut usize) -> Result<()> {
    let lno = idx + 1;
    // Hunk separators exist only in the context-bearing modes; `-p` alone
    // (funcname, no pre/post/funcbody) prints none.
    if cx.opts.brk && *last_shown == 0 {
        if cx.hunk_mark {
            out.write_all(b"\n")?;
        }
    } else if cx.pre > 0 || cx.post > 0 || cx.opts.funcbody {
        if *last_shown == 0 {
            if cx.hunk_mark {
                out.write_all(b"--\n")?;
            }
        } else if lno > *last_shown + 1 {
            out.write_all(b"--\n")?;
        }
    }
    let is_match_line = sign == b':';
    let under_o = cx.opts.only_matching && !cx.opts.invert;
    if under_o {
        if is_match_line {
            // The header repeats per matched substring, as git's `-o` does.
            let line = cx.lv[idx];
            let mut at = 0usize;
            while let Some((start, len)) = next_match(line, cx.ev.matcher, at) {
                if len == 0 {
                    break;
                }
                write_line_header(out, cx.name, lno, start + 1, sign, cx.opts)?;
                write_field(out, &line[start..start + len], C_MATCH, cx.opts.color)?;
                out.write_all(b"\n")?;
                at = start + len;
            }
        }
        // A context/funcname line contributes no matched substring under `-o`, but
        // still counts as shown so the hunk bookkeeping stays in step.
        *last_shown = lno;
        return Ok(());
    }
    // The heading name prints once, when nothing has been shown yet.
    if cx.opts.heading && *last_shown == 0 {
        write_field(out, cx.name, C_FILENAME, cx.opts.color)?;
        out.write_all(b"\n")?;
    }
    let col = if is_match_line { cx.col1[idx] } else { 0 };
    write_line_header(out, cx.name, lno, col, sign, cx.opts)?;
    if is_match_line {
        write_body(out, cx.lv[idx], cx.ev.matcher, cx.opts.color && !cx.opts.invert)?;
    } else {
        out.write_all(cx.lv[idx])?;
    }
    out.write_all(b"\n")?;
    *last_shown = lno;
    Ok(())
}

/// git's `show_pre_context`: emit the leading `-A`/`-B` context and the enclosing
/// funcname line (`=`) ahead of the match at `m` (0-based).
fn fc_show_pre<W: Write>(out: &mut W, cx: &Fc, m: usize, last_shown: &mut usize) -> Result<()> {
    let lno = m + 1;
    let mut from = 1usize;
    let mut funcname_lno = 0usize; // 1-based; 0 = none
    let mut funcname_needed = cx.opts.show_function;
    let mut comment_needed = false;
    if cx.pre < lno {
        from = lno - cx.pre;
    }
    if from <= *last_shown {
        from = *last_shown + 1;
    }
    let orig_from = from;
    if cx.opts.funcbody {
        if cx.is_func(m) {
            comment_needed = true;
        } else {
            funcname_needed = true;
        }
        from = *last_shown + 1;
    }
    // Rewind toward `from`, latching the funcname line to display.
    let mut cur = lno;
    while cur > 1 && cur > from {
        cur -= 1;
        let idx = cur - 1;
        if comment_needed && (cx.is_empty(idx) || cx.is_func(idx)) {
            comment_needed = false;
            from = orig_from;
            if cur < from {
                cur += 1;
                break;
            }
        }
        if funcname_needed && cx.is_func(idx) {
            funcname_lno = cur;
            funcname_needed = false;
            if cx.opts.funcbody {
                comment_needed = true;
            } else {
                from = orig_from;
            }
        }
    }
    // `-p` may need to look even further back for a signature.
    if cx.opts.show_function && funcname_needed {
        let mut c = cur;
        while c > 1 {
            c -= 1;
            if c <= *last_shown {
                break;
            }
            if cx.is_func(c - 1) {
                fc_emit(out, cx, c - 1, b'=', last_shown)?;
                break;
            }
        }
    }
    // Forward: print the rewound span up to (not including) the match line.
    while cur < lno {
        let sign = if cur == funcname_lno { b'=' } else { b'-' };
        fc_emit(out, cx, cur - 1, sign, last_shown)?;
        cur += 1;
    }
    Ok(())
}

/// Render one file under `-p`/`--show-function` (`show_function`) and/or
/// `-W`/`--function-context` (`funcbody`), a port of git's `grep_source_1` line
/// loop together with `show_pre_context` and `show_funcname_line`. It also honours
/// any `-A`/`-B`/`-C` given alongside, exactly as git folds the two together.
///
/// `hunk_mark` is git's cross-file "a previous file already printed" flag, which
/// gates the `--`/`--break` separators. Returns whether this file matched.
#[allow(clippy::too_many_arguments)]
fn render_funcctx<W: Write>(
    out: &mut W,
    content: &[u8],
    name: &[u8],
    ev: &LineEval,
    opts: &Opts,
    pre: usize,
    post: usize,
    hunk_mark: bool,
) -> Result<bool> {
    let lv: Vec<&[u8]> = lines(content).collect();
    let n = lv.len();
    let limit = if opts.max_count < 0 {
        usize::MAX
    } else {
        opts.max_count as usize
    };

    // The matching lines and their columns, capped at `--max-count` as git does.
    let mut is_match = vec![false; n];
    let mut col1 = vec![0usize; n];
    let mut hit = false;
    let mut matches = 0usize;
    for (idx, line) in lv.iter().enumerate() {
        if matches >= limit {
            break;
        }
        let (m, c) = ev.test(line);
        if m {
            is_match[idx] = true;
            col1[idx] = c;
            hit = true;
            matches += 1;
        }
    }
    if !hit {
        return Ok(false);
    }

    let cx = Fc { ev, opts, name, lv: &lv, col1: &col1, pre, post, hunk_mark };
    let mut last_shown: usize = 0; // 1-based line last printed; 0 = none
    let mut last_hit: usize = 0; // 1-based line of the last match printed
    let mut show_function = false; // funcbody: currently inside a body to emit

    for idx in 0..n {
        let lno = idx + 1;
        if is_match[idx] {
            if pre > 0 || opts.funcbody {
                fc_show_pre(out, &cx, idx, &mut last_shown)?;
            } else if opts.show_function {
                // `show_funcname_line`: nearest funcname line above, `=`-signed.
                let mut c = idx; // 0-based, walking upward
                while c > 0 {
                    c -= 1;
                    if c + 1 <= last_shown {
                        break;
                    }
                    if cx.is_func(c) {
                        fc_emit(out, &cx, c, b'=', &mut last_shown)?;
                        break;
                    }
                }
            }
            fc_emit(out, &cx, idx, b':', &mut last_shown)?;
            last_hit = lno;
            if opts.funcbody {
                show_function = true;
            }
        } else {
            if show_function {
                // Peek past trailing blank lines; the next function's signature
                // ends this body before it is reached.
                let mut p = idx;
                while p < n && cx.is_empty(p) {
                    p += 1;
                }
                if p < n && cx.is_func(p) {
                    show_function = false;
                }
            }
            if show_function || (last_hit != 0 && lno <= last_hit + post) {
                fc_emit(out, &cx, idx, b'-', &mut last_shown)?;
            }
        }
    }
    Ok(true)
}

/// Emit git's `<name><sign><lno><sign>[<col><sign>]` line header. `sign` is `:`
/// for a match, `-` for context, `=` for a funcname line; `col` is `0` for a
/// context/funcname line (no column field) and the 1-based match column otherwise.
fn write_line_header(
    out: &mut impl Write,
    name: &[u8],
    lno: usize,
    col: usize,
    sign: u8,
    opts: &Opts,
) -> Result<()> {
    let sep: &[u8] = if opts.nul { b"\0" } else { std::slice::from_ref(&sign) };
    if opts.show_names && !opts.heading {
        write_field(out, name, C_FILENAME, opts.color)?;
        write_field(out, sep, C_SEP, opts.color)?;
    }
    if opts.line_number {
        write_field(out, lno.to_string().as_bytes(), C_LINENO, opts.color)?;
        write_field(out, sep, C_SEP, opts.color)?;
    }
    if opts.column && col != 0 {
        write_field(out, col.to_string().as_bytes(), C_LINENO, opts.color)?;
        write_field(out, sep, C_SEP, opts.color)?;
    }
    Ok(())
}

/// Render one file's matches with `-A`/`-B`/`-C` context, byte-identical to git:
/// a match line keeps the `:`-separated header (with a column under `--column`),
/// a context line uses `-` separators and never a column, and a `--` line
/// precedes every hunk except the first one printed across the whole run.
/// `printed_any` carries that "first hunk" state between files. Returns whether
/// this file produced a match, for the exit status.
#[allow(clippy::too_many_arguments)]
fn render_context(
    out: &mut impl Write,
    content: &[u8],
    name: &[u8],
    ev: &LineEval,
    opts: &Opts,
    pre: usize,
    post: usize,
    printed_any: &mut bool,
) -> Result<bool> {
    let lines: Vec<&[u8]> = lines(content).collect();
    let n = lines.len();
    let limit = if opts.max_count < 0 {
        usize::MAX
    } else {
        opts.max_count as usize
    };

    // The matching lines, capped at `--max-count` matches per file as git does.
    let mut is_match = vec![false; n];
    let mut hit = false;
    let mut matches = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        if matches >= limit {
            break;
        }
        if ev.matches(line) {
            is_match[idx] = true;
            hit = true;
            matches += 1;
        }
    }
    if !hit {
        return Ok(false);
    }

    // Every match drags its pre/post neighbours into the shown set; overlapping
    // windows merge, which is what makes adjacent matches share one hunk.
    let mut show = vec![false; n];
    for idx in 0..n {
        if !is_match[idx] {
            continue;
        }
        let lo = idx.saturating_sub(pre);
        let hi = idx.saturating_add(post).min(n - 1);
        for s in show.iter_mut().take(hi + 1).skip(lo) {
            *s = true;
        }
    }

    // Walk the shown lines in order; a gap starts a new hunk. Across files git's
    // `--` separator precedes every hunk except the first printed anywhere —
    // except that at a file's *first* hunk `--break` substitutes a blank line and
    // `--heading` adds the file's name line (intra-file hunks keep the `--`).
    let mut prev_shown: Option<usize> = None;
    let mut first_hunk_of_file = true;
    for idx in 0..n {
        if !show[idx] {
            continue;
        }
        let new_hunk = prev_shown.is_none_or(|p| idx > p + 1);
        if new_hunk {
            if first_hunk_of_file {
                if opts.brk {
                    if *printed_any {
                        out.write_all(b"\n")?;
                    }
                } else if *printed_any {
                    out.write_all(b"--\n")?;
                }
                if opts.heading {
                    write_field(out, name, C_FILENAME, opts.color)?;
                    out.write_all(b"\n")?;
                }
                first_hunk_of_file = false;
            } else {
                out.write_all(b"--\n")?;
            }
            *printed_any = true;
        }
        prev_shown = Some(idx);

        let line = lines[idx];
        if is_match[idx] {
            if opts.only_matching && !opts.invert {
                let mut at = 0usize;
                while let Some((start, len)) = next_match(line, ev.matcher, at) {
                    if len == 0 {
                        break;
                    }
                    write_prefix(out, name, idx + 1, start + 1, opts)?;
                    write_field(out, &line[start..start + len], C_MATCH, opts.color)?;
                    out.write_all(b"\n")?;
                    at = start + len;
                }
            } else {
                let col = ev.test(line).1;
                write_prefix(out, name, idx + 1, col, opts)?;
                // A `-v` line is not itself a match, so it is never highlighted.
                write_body(out, line, ev.matcher, opts.color && !opts.invert)?;
                out.write_all(b"\n")?;
            }
        } else if !(opts.only_matching && !opts.invert) {
            write_context_prefix(out, name, idx + 1, opts)?;
            out.write_all(line)?;
            out.write_all(b"\n")?;
        }
        // Under `-o` a context line has no matched substring to show, so it emits
        // nothing (its hunk still contributes the leading `--`); this matches
        // git's `-o -A` exactly. git's `-o -B`/`-o -C` additionally double some
        // separators — a documented quirk this port does not reproduce.
    }
    Ok(true)
}

/// The `<name>-<lineno>-` header git puts on a context line: like
/// [`write_prefix`] but with `-` separators and no column field. `--heading`
/// suppresses the inline name and `--color` wraps each field, as for match lines.
fn write_context_prefix(out: &mut impl Write, name: &[u8], lno: usize, opts: &Opts) -> Result<()> {
    let sep: &[u8] = if opts.nul { b"\0" } else { b"-" };
    if opts.show_names && !opts.heading {
        write_field(out, name, C_FILENAME, opts.color)?;
        write_field(out, sep, C_SEP, opts.color)?;
    }
    if opts.line_number {
        write_field(out, lno.to_string().as_bytes(), C_LINENO, opts.color)?;
        write_field(out, sep, C_SEP, opts.color)?;
    }
    Ok(())
}

/// Search one file's `content`, emitting whatever the active output mode calls
/// for. Returns whether this file contributes a hit to the exit status: for
/// `-L` that is having been *listed* (no match), otherwise having matched.
#[allow(clippy::too_many_arguments)]
fn search_file(
    out: &mut impl Write,
    content: &[u8],
    name: &[u8],
    binary: bool,
    ev: &LineEval,
    opts: &Opts,
    emitted_any: &mut bool,
) -> Result<bool> {
    let limit = if opts.max_count < 0 {
        usize::MAX
    } else {
        opts.max_count as usize
    };
    let mut count = 0usize;
    let mut hit = false;
    // Once a binary file is known to match, git prints a single notice in place
    // of the matching lines and moves on; the counting modes are unaffected.
    let mut binary_notice_pending = binary;
    // Whether this file's `--break`/`--heading` prelude has been emitted yet.
    let mut fired = false;

    for (lno, line) in lines(content).enumerate() {
        if count >= limit {
            break;
        }
        let (matched, col1) = ev.test(line);
        if !matched {
            continue;
        }
        hit = true;
        count += 1;

        if opts.quiet || opts.files_with || opts.files_without {
            break;
        }
        if opts.count {
            continue;
        }
        if binary_notice_pending {
            binary_notice_pending = false;
            open_group(out, name, opts, emitted_any, &mut fired)?;
            out.write_all(b"Binary file ")?;
            out.write_all(name)?;
            out.write_all(b" matches\n")?;
            break;
        }

        open_group(out, name, opts, emitted_any, &mut fired)?;
        // `-o` has nothing to narrow under `-v`, where the whole line is the
        // result; git prints the full line in that case.
        if opts.only_matching && !opts.invert {
            let mut at = 0usize;
            while let Some((start, len)) = next_match(line, ev.matcher, at) {
                if len == 0 {
                    break; // an empty pattern has no non-empty part to show
                }
                write_prefix(out, name, lno + 1, start + 1, opts)?;
                write_field(out, &line[start..start + len], C_MATCH, opts.color)?;
                out.write_all(b"\n")?;
                at = start + len;
            }
        } else {
            write_prefix(out, name, lno + 1, col1, opts)?;
            // A `-v` line is not itself a match, so it is never highlighted.
            write_body(out, line, ev.matcher, opts.color && !opts.invert)?;
            out.write_all(b"\n")?;
        }
    }

    // git's precedence: -q suppresses all output, then -L, then -l, then -c.
    let term: &[u8] = if opts.nul { b"\0" } else { b"\n" };
    if opts.files_without {
        if !hit && !opts.quiet {
            write_field(out, name, C_FILENAME, opts.color)?;
            out.write_all(term)?;
        }
        return Ok(!hit);
    }
    if opts.quiet {
        return Ok(hit);
    }
    if opts.files_with {
        if hit {
            write_field(out, name, C_FILENAME, opts.color)?;
            out.write_all(term)?;
        }
        return Ok(hit);
    }
    if opts.count && count > 0 {
        if opts.show_names {
            write_field(out, name, C_FILENAME, opts.color)?;
            write_field(out, if opts.nul { b"\0" } else { b":" }, C_SEP, opts.color)?;
        }
        writeln!(out, "{count}")?;
    }
    Ok(hit)
}

/// Emit the `<name><sep><lineno><sep><column><sep>` header of a match line.
/// With `-z` every separator is a NUL instead of `:`, exactly as git's
/// `show_line()` does when `null_following_name` is set. `--heading` suppresses
/// the inline name (it is printed once as a heading instead), and `--color`
/// wraps each field in git's default colours.
fn write_prefix(
    out: &mut impl Write,
    name: &[u8],
    lno: usize,
    column: usize,
    opts: &Opts,
) -> Result<()> {
    let sep: &[u8] = if opts.nul { b"\0" } else { b":" };
    if opts.show_names && !opts.heading {
        write_field(out, name, C_FILENAME, opts.color)?;
        write_field(out, sep, C_SEP, opts.color)?;
    }
    if opts.line_number {
        write_field(out, lno.to_string().as_bytes(), C_LINENO, opts.color)?;
        write_field(out, sep, C_SEP, opts.color)?;
    }
    if opts.column {
        write_field(out, column.to_string().as_bytes(), C_LINENO, opts.color)?;
        write_field(out, sep, C_SEP, opts.color)?;
    }
    Ok(())
}

/// Write one output field, wrapped in `code`/reset when `color` is set and left
/// bare otherwise.
fn write_field(out: &mut impl Write, bytes: &[u8], code: &[u8], color: bool) -> Result<()> {
    if color {
        out.write_all(code)?;
        out.write_all(bytes)?;
        out.write_all(C_RESET)?;
    } else {
        out.write_all(bytes)?;
    }
    Ok(())
}

/// Write a matched line's body. Without `--color` the whole line goes out as-is;
/// with it, each matched span is wrapped in the match colour and the gaps stay
/// plain, exactly as git highlights a selected line.
fn write_body(out: &mut impl Write, line: &[u8], matcher: &Matcher, color: bool) -> Result<()> {
    if !color {
        return out.write_all(line).map_err(Into::into);
    }
    let mut at = 0usize;
    while let Some((start, len)) = next_match(line, matcher, at) {
        if len == 0 {
            break;
        }
        out.write_all(&line[at..start])?;
        write_field(out, &line[start..start + len], C_MATCH, true)?;
        at = start + len;
    }
    out.write_all(&line[at..])?;
    Ok(())
}

/// Print a file's `--break` blank line and `--heading` name line ahead of its
/// first emitted line. Idempotent per file via `fired`; `emitted_any` spans the
/// whole run so the separators land between files but not before the first.
fn open_group(
    out: &mut impl Write,
    name: &[u8],
    opts: &Opts,
    emitted_any: &mut bool,
    fired: &mut bool,
) -> Result<()> {
    if *fired {
        return Ok(());
    }
    *fired = true;
    if opts.brk && *emitted_any {
        out.write_all(b"\n")?;
    }
    if opts.heading {
        write_field(out, name, C_FILENAME, opts.color)?;
        out.write_all(b"\n")?;
    }
    *emitted_any = true;
    Ok(())
}

/// Split `content` the way git does: on `\n`, with a trailing newline *not*
/// producing a final empty line, and an empty file producing no lines at all.
fn lines(content: &[u8]) -> impl Iterator<Item = &[u8]> {
    let body = content.strip_suffix(b"\n").unwrap_or(content);
    let empty = content.is_empty();
    body.split(|&b| b == b'\n')
        .take(if empty { 0 } else { usize::MAX })
}

/// A compiled search: the `-e` patterns (OR'd) as one byte regex, plus git's
/// `-w` word-boundary constraint and the empty-pattern "matches every line" rule.
struct Matcher {
    /// `None` when every pattern is empty (then `match_all` carries the result).
    re: Option<regex::bytes::Regex>,
    word: bool,
    /// An empty `-e` pattern matches all lines (git's documented behaviour).
    match_all: bool,
}

impl Matcher {
    fn build(patterns: &[String], dialect: Dialect, ignore_case: bool, word: bool) -> Result<Self> {
        let match_all = patterns.iter().any(|p| p.is_empty());
        let nonempty: Vec<&String> = patterns.iter().filter(|p| !p.is_empty()).collect();
        let re = if nonempty.is_empty() {
            None
        } else {
            let combined = nonempty
                .iter()
                .map(|p| Ok(format!("(?:{})", translate_pattern(p, dialect)?)))
                .collect::<Result<Vec<_>>>()?
                .join("|");
            let compile = |pat: &str| {
                regex::bytes::RegexBuilder::new(pat)
                    .case_insensitive(ignore_case)
                    .unicode(false) // git greps bytes, not scalar values
                    .build()
            };
            match compile(&combined) {
                Ok(re) => Some(re),
                // git's POSIX engine treats a `{`/`}` that does not form a valid
                // interval as a literal; the regex crate rejects it. Recover that
                // leniency by literalising braces and retrying — a genuine error
                // (an unmatched `(` or `[`) still fails and surfaces as fatal.
                Err(_) => {
                    let lenient = combined.replace('{', "\\{").replace('}', "\\}");
                    Some(
                        compile(&lenient)
                            .map_err(|e| anyhow::anyhow!("invalid regex: {e}"))?,
                    )
                }
            }
        };
        Ok(Self { re, word, match_all })
    }

    /// The next match in `line` at or after `at`, as `(start, len)`. Ties go to
    /// the leftmost match; `-w` skips matches not sitting on word boundaries.
    fn find_at(&self, line: &[u8], at: usize) -> Option<(usize, usize)> {
        if at > line.len() {
            return None;
        }
        if self.match_all {
            return Some((at, 0));
        }
        let re = self.re.as_ref()?;
        let mut from = at;
        loop {
            let m = re.find_at(line, from)?;
            let (s, e) = (m.start(), m.end());
            if !self.word || word_bounded(line, s, e) {
                return Some((s, e - s));
            }
            // This match straddles a word char; look past its start for another.
            from = s + 1;
            if from > line.len() {
                return None;
            }
        }
    }
}

/// Translate a pattern in `dialect` into the byte-regex syntax the `regex` crate
/// accepts: `-F` escapes to a literal, ERE/PCRE pass through, and BRE is mapped
/// by swapping which of `( ) { } + ? |` are escaped.
fn translate_pattern(pattern: &str, dialect: Dialect) -> Result<String> {
    Ok(match dialect {
        Dialect::Fixed => regex::escape(pattern),
        Dialect::Extended | Dialect::Perl => pattern.to_string(),
        Dialect::Basic => bre_to_regex(pattern),
    })
}

/// GNU BRE → `regex`-crate syntax. In BRE the grouping/quantifier operators are
/// the *escaped* forms (`\(` `\)` `\{` `\}` `\+` `\?` `\|`) while the bare
/// characters are literals; ERE (and this crate) are the reverse. Bytes inside a
/// `[...]` bracket expression are copied verbatim.
fn bre_to_regex(p: &str) -> String {
    let b = p.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    let mut in_class = false;
    while i < b.len() {
        let c = b[i];
        if in_class {
            out.push(c as char);
            if c == b']' {
                in_class = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'[' => {
                in_class = true;
                out.push('[');
            }
            b'\\' if i + 1 < b.len() => {
                let n = b[i + 1];
                match n {
                    // BRE's escaped operators become bare operators.
                    b'(' | b')' | b'{' | b'}' | b'+' | b'?' | b'|' => out.push(n as char),
                    // Everything else keeps its backslash (`\.`, `\\`, `\b`, …).
                    _ => {
                        out.push('\\');
                        out.push(n as char);
                    }
                }
                i += 1;
            }
            // Bare operators are literals in BRE, so escape them for the crate.
            b'(' | b')' | b'{' | b'}' | b'+' | b'?' | b'|' => {
                out.push('\\');
                out.push(c as char);
            }
            _ => out.push(c as char),
        }
        i += 1;
    }
    out
}

/// The next match of the compiled `matcher` in `line` at or after `at`.
fn next_match(line: &[u8], matcher: &Matcher, at: usize) -> Option<(usize, usize)> {
    matcher.find_at(line, at)
}

/// Find `needle` in `hay` at or after `from`, honouring `-i` and `-w`.
/// An empty needle matches at `from` with length zero (git: "an empty string as
/// search expression matches all lines").
fn find_from(hay: &[u8], needle: &[u8], from: usize, opts: &Opts) -> Option<(usize, usize)> {
    if from > hay.len() {
        return None;
    }
    let n = needle.len();
    if n == 0 {
        return Some((from, 0));
    }
    let mut i = from;
    while i + n <= hay.len() {
        let eq = if opts.ignore_case {
            hay[i..i + n]
                .iter()
                .zip(needle)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        } else {
            &hay[i..i + n] == needle
        };
        if eq && (!opts.word || word_bounded(hay, i, i + n)) {
            return Some((i, n));
        }
        i += 1;
    }
    None
}

/// Whether `hay[start..end]` sits on word boundaries, with git's word alphabet
/// (ASCII alphanumerics plus `_`).
fn word_bounded(hay: &[u8], start: usize, end: usize) -> bool {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    (start == 0 || !is_word(hay[start - 1])) && (end == hay.len() || !is_word(hay[end]))
}

/// git's `buffer_is_binary()`: a NUL within the first 8000 bytes.
fn is_binary(content: &[u8]) -> bool {
    let head = &content[..content.len().min(FIRST_FEW_BYTES)];
    head.contains(&0)
}

/// Reduce a pattern to the literal byte string it denotes, or fail.
///
/// `-F` patterns are literal by definition. In the regex dialects only patterns
/// free of that dialect's metacharacters are literal, and those are the only
/// ones this port can execute — there is no regex engine among the vendored
/// gitoxide crates to hand the rest to.
fn literal_of(pattern: &str, dialect: Dialect) -> Result<Vec<u8>> {
    let meta: &[char] = match dialect {
        Dialect::Fixed => &[],
        Dialect::Basic => &['.', '*', '[', ']', '^', '$', '\\'],
        Dialect::Extended | Dialect::Perl => &[
            '.', '*', '[', ']', '^', '$', '\\', '+', '?', '{', '}', '(', ')', '|',
        ],
    };
    if let Some(c) = pattern.chars().find(|c| meta.contains(c)) {
        bail!(
            "pattern {pattern:?} contains the regex metacharacter {c:?}; \
             the vendored gitoxide crates ship no regex engine, so only literal \
             patterns are supported (use -F to match it literally)"
        );
    }
    Ok(pattern.as_bytes().to_vec())
}

/// The path as git prints it: repo-root-relative with the current-directory
/// prefix stripped, C-quoted unless `-z` asked for verbatim bytes.
fn display_name(path: &BStr, prefix: Option<&[u8]>, opts: &Opts) -> Vec<u8> {
    let bytes = path.as_bytes();
    let rel = match prefix {
        Some(p) if bytes.starts_with(p) => &bytes[p.len()..],
        _ => bytes,
    };
    if opts.nul {
        rel.to_vec()
    } else {
        quote_path(rel).into_bytes()
    }
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(bytes: &[u8]) -> String {
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        // All bytes are printable ASCII here, so this is lossless.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// The terse rejection used for every flag this port does not implement.
fn unsupported(flag: &str) -> String {
    format!(
        "unsupported flag {flag:?} (ported: -e, -f/--file, -i, -v, -w, -a, -I, -n, --column, \
         -l/--files-with-matches/--name-only, -L/--files-without-match, -c, -q, -z, -o, \
         -h, -H, -E, -G, -F, -P, -m/--max-count, --max-depth, -r/--[no-]recursive, \
         -A/-B/-C context, -W/-p function context, --and/--or/--not/() grammar, \
         --heading, --break, --all-match, --full-name, --cached, --untracked, \
         --no-index/--index, --[no-]exclude-standard, --recurse-submodules, --textconv, \
         --color, <tree>/<revision> search, and pathspecs)"
    )
}
