//! `git grep` — search the contents of tracked files for a pattern.
//!
//! Covered: the tracked-worktree default, `--cached`, `--untracked` and
//! `--no-index`, pathspec limiting (via gitoxide's pathspec platform, so magic
//! and globs work and a subdirectory invocation searches only that subtree),
//! `--max-depth`, `--max-count`, and the line/name/count/quiet output modes with
//! byte-identical formatting, including git's binary-file handling and
//! `core.quotePath` path quoting.
//!
//! `--textconv` is honoured to the extent it can change an answer: it only ever
//! does so when some `diff.<driver>.textconv` command is configured to run, and
//! with none configured — the overwhelmingly common case, and the one git itself
//! short-circuits — it is exactly the no-op git makes of it. A configured
//! converter would have to be executed as an external process, which is refused
//! rather than guessed at.
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
//! Not covered, and rejected loudly rather than approximated: searching `<tree>`
//! revisions, the function-context renderers (`-W`/`-p`/`--function-context`/
//! `--show-function`), `--heading`/`--break`, `-f`, `--and`/`--or`/`--not`,
//! `-O`, and coloured output.
//!
//! Flags in that last group that only shape the *rendering* of a match —
//! `--heading`, `--break`, `-p`, `-W` — are accepted during parsing (git itself
//! diagnoses a missing pattern before it looks at them) and refused at the point
//! they would change what is printed. When nothing matched there is nothing for
//! them to shape, so the empty output and exit code 1 are git's answer exactly,
//! and the run is allowed to finish.

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
}

/// Flags that git accepts but this port cannot render, kept aside so the
/// "no pattern given" diagnosis still happens first, exactly as in git.
#[derive(Default)]
struct Deferred {
    /// The flag as the user spelled it, for the refusal message.
    context: Option<String>,
    /// Records that `--recurse-submodules` was requested, for the `--untracked`
    /// incompatibility check; the flag is otherwise a no-op here.
    set_changing: Option<String>,
    all_match: bool,
}

/// `git grep` — print lines matching a pattern.
///
/// Supported flags (output byte-for-byte identical to stock git for these):
///   * source: default (tracked files in the worktree), `--cached`,
///     `--untracked`, `--no-index`/`--index`, `--[no-]exclude-standard`
///   * matching: `-i`, `-v`, `-w`, `-F`/`--fixed-strings`, `-E`, `-G`, `-P`,
///     `-e <pattern>` (repeatable; patterns are OR'd), `-m`/`--max-count`
///   * binary: `-a`/`--text`, `-I`
///   * scope: `--max-depth`, `-r`/`--recursive`, `--no-recursive`
///   * output: `-n`, `--column`, `-l`/`--files-with-matches`/`--name-only`,
///     `-L`/`--files-without-match`, `-c`/`--count`, `-q`/`--quiet`, `-o`,
///     `-z`/`--null`, `-h`, `-H`, `--full-name`, `--color=never|auto`
///   * context: `-A`/`--after-context`, `-B`/`--before-context`,
///     `-C`/`--context`, `-<num>`
///   * accepted no-ops: `--[no-]textconv` with no converter configured,
///     `--[no-]ext-grep`, `--threads`, `--no-heading`, `--no-break`,
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
    };
    // git tracks `--[no-]exclude-standard` as a tri-state; this records whether
    // the user pinned it, so the default can follow `--no-index` when they did not.
    let mut exclude_standard_explicit = false;
    let mut textconv = false;
    let mut deferred = Deferred::default();
    let mut dialect = Dialect::Basic;
    let mut patterns: Vec<String> = Vec::new();
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
                "no-heading" | "no-break" | "no-show-function" | "no-function-context"
                | "no-recurse-submodules" | "no-all-match" => {}
                "color" => match color_wanted(inline.as_deref()) {
                    Ok(true) => bail!("{}", unsupported("--color=always")),
                    Ok(false) => {}
                    Err(v) => {
                        eprintln!("error: option `color' expects \"always\", \"auto\", or \"never\", not \"{v}\"");
                        return Ok(ExitCode::from(129));
                    }
                },
                "no-color" => {}
                "heading" | "break" | "show-function" | "function-context" => {
                    deferred.context.get_or_insert_with(|| a.to_string());
                }
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
                'p' | 'W' => {
                    deferred.context.get_or_insert_with(|| format!("-{}", group[c]));
                }
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
                    patterns.push(short_value!('e'));
                    c = group.len();
                    continue;
                }
                'f' => {
                    let _ = short_value!('f');
                    bail!("{}", unsupported("-f"));
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
    if patterns.is_empty() && !rest.is_empty() {
        patterns.push(rest.remove(0));
    }
    // git diagnoses a missing pattern here, before it looks at anything else.
    if patterns.is_empty() {
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

    if deferred.all_match && patterns.len() > 1 {
        bail!("{}", unsupported("--all-match"));
    }

    // A resolved revision means a `<tree>` search, which git would run here and
    // this port cannot; it is refused only now, after every diagnostic git emits
    // ahead of it has had its chance.
    if let Some(rev) = revs.first() {
        bail!("searching a tree/revision ({rev:?}) is not supported");
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

    let index = repo.open_index()?;

    // `--textconv` can only change what is searched when there is a converter to
    // run, and one exists only if some `diff.<driver>.textconv` command is
    // configured. With none, honouring the setting and ignoring it agree.
    if textconv && has_textconv_driver(&repo) {
        bail!(
            "unsupported flag \"--textconv\" while a `diff.<driver>.textconv` command is \
             configured: converting a blob means running that command as an external \
             process, which is not implemented"
        );
    }

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

    // Reading a candidate's bytes, from the index with `--cached` and from the
    // worktree otherwise. `None` means the file is gone, which git ignores.
    let content_of = |path: &BString, id: &Option<gix::hash::ObjectId>| -> Result<Option<Vec<u8>>> {
        if opts.cached {
            let Some(id) = id else { return Ok(None) };
            return Ok(Some(repo.find_object(*id)?.data.clone()));
        }
        let Some(abs) = repo.workdir_path(path.as_bstr()) else {
            return Ok(None);
        };
        match std::fs::read(&abs) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    };

    // Context, headings and function names only reshape printed match lines, so
    // they are irrelevant to the name/count/quiet modes — and to any run that
    // found nothing, where git's output is empty and its exit code is 1 too.
    let renders_lines = !(opts.quiet || opts.files_with || opts.files_without || opts.count);
    if let Some(flag) = deferred.context.filter(|_| renders_lines) {
        for (path, id) in &files {
            let Some(content) = content_of(path, id)? else { continue };
            if !opts.text && is_binary(&content) && opts.no_binary {
                continue;
            }
            if lines(&content).any(|l| next_match(l, &matcher, 0).is_some() != opts.invert) {
                bail!("{}", unsupported(&flag));
            }
        }
        return Ok(ExitCode::from(1));
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
        for (path, id) in &files {
            let Some(content) = content_of(path, id)? else { continue };
            let binary = !opts.text && is_binary(&content);
            if binary && opts.no_binary {
                continue;
            }
            let name = display_name(path.as_bstr(), prefix, &opts);
            if binary {
                // git reports a binary hit through the exit status but prints no
                // context lines and no "Binary file matches" notice for it.
                if lines(&content)
                    .any(|l| next_match(l, &matcher, 0).is_some() != opts.invert)
                {
                    any_hit = true;
                }
                continue;
            }
            if render_context(
                &mut out,
                &content,
                &name,
                &matcher,
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

    for (path, id) in &files {
        let Some(content) = content_of(path, id)? else { continue };

        let binary = !opts.text && is_binary(&content);
        if binary && opts.no_binary {
            continue;
        }

        let name = display_name(path.as_bstr(), prefix, &opts);
        if search_file(&mut out, &content, &name, binary, &matcher, &opts)? {
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
    matcher: &Matcher,
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
        if next_match(line, matcher, 0).is_some() != opts.invert {
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

    // Walk the shown lines in order; a gap starts a new hunk, and each hunk after
    // the first printed anywhere is introduced by `--`.
    let mut prev_shown: Option<usize> = None;
    for idx in 0..n {
        if !show[idx] {
            continue;
        }
        let new_hunk = prev_shown.is_none_or(|p| idx > p + 1);
        if new_hunk {
            if *printed_any {
                out.write_all(b"--\n")?;
            }
            *printed_any = true;
        }
        prev_shown = Some(idx);

        let line = lines[idx];
        if is_match[idx] {
            if opts.only_matching && !opts.invert {
                let mut at = 0usize;
                while let Some((start, len)) = next_match(line, matcher, at) {
                    if len == 0 {
                        break;
                    }
                    write_prefix(out, name, idx + 1, start + 1, opts)?;
                    out.write_all(&line[start..start + len])?;
                    out.write_all(b"\n")?;
                    at = start + len;
                }
            } else {
                let col = next_match(line, matcher, 0).map_or(0, |(s, _)| s) + 1;
                write_prefix(out, name, idx + 1, col, opts)?;
                out.write_all(line)?;
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
/// [`write_prefix`] but with `-` separators and no column field.
fn write_context_prefix(out: &mut impl Write, name: &[u8], lno: usize, opts: &Opts) -> Result<()> {
    if opts.show_names {
        out.write_all(name)?;
        out.write_all(b"-")?;
    }
    if opts.line_number {
        write!(out, "{lno}")?;
        out.write_all(b"-")?;
    }
    Ok(())
}

/// Search one file's `content`, emitting whatever the active output mode calls
/// for. Returns whether this file contributes a hit to the exit status: for
/// `-L` that is having been *listed* (no match), otherwise having matched.
fn search_file(
    out: &mut impl Write,
    content: &[u8],
    name: &[u8],
    binary: bool,
    matcher: &Matcher,
    opts: &Opts,
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

    for (lno, line) in lines(content).enumerate() {
        if count >= limit {
            break;
        }
        let first = next_match(line, matcher, 0);
        let matched = first.is_some() != opts.invert;
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
            out.write_all(b"Binary file ")?;
            out.write_all(name)?;
            out.write_all(b" matches\n")?;
            break;
        }

        // `-o` has nothing to narrow under `-v`, where the whole line is the
        // result; git prints the full line in that case.
        if opts.only_matching && !opts.invert {
            let mut at = 0usize;
            while let Some((start, len)) = next_match(line, matcher, at) {
                if len == 0 {
                    break; // an empty pattern has no non-empty part to show
                }
                write_prefix(out, name, lno + 1, start + 1, opts)?;
                out.write_all(&line[start..start + len])?;
                out.write_all(b"\n")?;
                at = start + len;
            }
        } else {
            write_prefix(out, name, lno + 1, first.map_or(0, |(s, _)| s) + 1, opts)?;
            out.write_all(line)?;
            out.write_all(b"\n")?;
        }
    }

    // git's precedence: -q suppresses all output, then -L, then -l, then -c.
    let term: &[u8] = if opts.nul { b"\0" } else { b"\n" };
    if opts.files_without {
        if !hit && !opts.quiet {
            out.write_all(name)?;
            out.write_all(term)?;
        }
        return Ok(!hit);
    }
    if opts.quiet {
        return Ok(hit);
    }
    if opts.files_with {
        if hit {
            out.write_all(name)?;
            out.write_all(term)?;
        }
        return Ok(hit);
    }
    if opts.count && count > 0 {
        if opts.show_names {
            out.write_all(name)?;
            out.write_all(if opts.nul { b"\0" } else { b":" })?;
        }
        writeln!(out, "{count}")?;
    }
    Ok(hit)
}

/// Emit the `<name><sep><lineno><sep><column><sep>` header of a match line.
/// With `-z` every separator is a NUL instead of `:`, exactly as git's
/// `show_line()` does when `null_following_name` is set.
fn write_prefix(
    out: &mut impl Write,
    name: &[u8],
    lno: usize,
    column: usize,
    opts: &Opts,
) -> Result<()> {
    let sep: &[u8] = if opts.nul { b"\0" } else { b":" };
    if opts.show_names {
        out.write_all(name)?;
        out.write_all(sep)?;
    }
    if opts.line_number {
        write!(out, "{lno}")?;
        out.write_all(sep)?;
    }
    if opts.column {
        write!(out, "{column}")?;
        out.write_all(sep)?;
    }
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
        "unsupported flag {flag:?} (ported: -e, -i, -v, -w, -a, -I, -n, --column, \
         -l/--files-with-matches/--name-only, -L/--files-without-match, -c, -q, -z, -o, \
         -h, -H, -E, -G, -F, -P, -m/--max-count, --max-depth, -r/--[no-]recursive, \
         -A/-B/-C context, --full-name, --cached, --untracked, --no-index/--index, \
         --[no-]exclude-standard, --recurse-submodules, --color=never|auto, and pathspecs)"
    )
}
