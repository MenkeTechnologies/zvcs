use anyhow::Result;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::object::tree::{EntryKind, EntryMode};
use gix::prelude::ObjectIdExt;

use super::{Arg, LongOpt};

/// `cmd_ls_tree()`'s `struct option ls_tree_options[]` (builtin/ls-tree.c), in
/// table order, as [`super::resolve_long`] reads it. The four output selectors
/// are `OPT_CMDMODE` and `--format` is `OPT_STRING_F(… PARSE_OPT_NONEG)`, so
/// only `--full-name`, `--full-tree` and `--abbrev` negate. `-d`, `-r`, `-t`
/// and `-z` are short-only and so have no entry.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "long", neg: false, arg: Arg::None },
    LongOpt { name: "name-only", neg: false, arg: Arg::None },
    LongOpt { name: "name-status", neg: false, arg: Arg::None },
    LongOpt { name: "object-only", neg: false, arg: Arg::None },
    LongOpt { name: "full-name", neg: true, arg: Arg::None },
    LongOpt { name: "full-tree", neg: true, arg: Arg::None },
    LongOpt { name: "format", neg: false, arg: Arg::Required },
    LongOpt { name: "abbrev", neg: true, arg: Arg::Optional },
];

/// The exact usage block stock `git ls-tree` prints (parse-options generated).
///
/// Emitted verbatim on `-h`, on an unknown option, and when `<tree-ish>` is
/// missing — git terminates all three with exit 129.
const USAGE: &str = "\
usage: git ls-tree [<options>] <tree-ish> [<path>...]

    -d                    only show trees
    -r                    recurse into subtrees
    -t                    show trees when recursing
    -z                    terminate entries with NUL byte
    -l, --long            include object size
    --name-only           list only filenames
    --name-status         list only filenames
    --object-only         list only objects
    --[no-]full-name      use full path names
    --[no-]full-tree      list entire tree; not just current directory (implies --full-name)
    --format <format>     format to use for the output
    --[no-]abbrev[=<n>]   use <n> digits to display object names

";

/// git's `MINIMUM_ABBREV` — any non-zero `--abbrev` below this is raised to it.
const MINIMUM_ABBREV: usize = 4;

/// How the object-id column is rendered.
#[derive(Clone, Copy, PartialEq)]
enum Abbrev {
    /// Full hex id (the default, and what `--no-abbrev` / `--abbrev=0` select).
    Full,
    /// `--abbrev` with no value: shortest unambiguous prefix.
    Auto,
    /// `--abbrev=<n>`: exactly `n` hex digits (clamped to the hash width).
    Len(usize),
}

/// The mutually exclusive output modes. git declares these with `OPT_CMDMODE`,
/// so two *different* modes on one command line is a usage error while the same
/// mode repeated is accepted.
#[derive(Clone, Copy, PartialEq)]
enum CmdMode {
    NameOnly,
    NameStatus,
    ObjectOnly,
    Long,
}

/// `ls_tree_cmdmode_format[]`: the canonical `--format` template of each cmdmode.
///
/// `cmd_ls_tree()`'s `m2f` loop compares `--format` against these four strings and,
/// on an exact match, runs that cmdmode's dedicated printer instead of the generic
/// `show_tree_fmt()`. The choice is observable: the dedicated printers write the
/// path through `write_name_quoted()`, which leaves it **raw** under `-z`, while
/// `show_tree_fmt()` always runs it through `quote_c_style()`.
const FMT_DEFAULT: &str = "%(objectmode) %(objecttype) %(objectname)%x09%(path)";
const FMT_LONG: &str = "%(objectmode) %(objecttype) %(objectname) %(objectsize:padded)%x09%(path)";
const FMT_NAME_ONLY: &str = "%(path)";
const FMT_OBJECT_ONLY: &str = "%(objectname)";

/// Parsed command-line options for a single `ls-tree` invocation.
struct Opts {
    recurse: bool,    // -r: descend into sub-trees
    show_trees: bool, // -t: emit tree lines even while recursing into them
    dirs_only: bool,  // -d: list tree entries only, never their contents
    long: bool,       // -l/--long: append the blob size column
    nul: bool,        // -z: terminate records with NUL instead of newline
    name_only: bool,  // --name-only/--name-status: print the path alone
    object_only: bool, // --object-only: print the object id alone
    abbrev: Abbrev,   // --abbrev[=N] / --no-abbrev
    // --format=<fmt>, only once the `m2f` fast path has failed to claim it; a
    // template that names one of the four cmdmodes is turned into that cmdmode
    // and cleared here, exactly as `cmd_ls_tree()` does.
    format: Option<String>,
    // Path filters (empty = whole tree); may carry a trailing '/'. Held as bytes
    // because they are compared against tree entry names, which are bytes.
    paths: Vec<Vec<u8>>,
    match_all: bool,  // an empty pathspec (e.g. `:` or `:(top)`) was given: selects everything
    // When `Some(b"dir/")`, displayed paths are rendered relative to this prefix
    // (git's `chomp_prefix` + `ls_tree_prefix`); `None` prints root-relative names.
    strip_prefix: Option<Vec<u8>>,
}

/// Fatal usage error: `git` prints the message, a blank line, then the usage
/// block, and exits 129.
fn usage_msg_opt(msg: &str) -> ExitCode {
    eprint!("fatal: {msg}\n\n{USAGE}");
    ExitCode::from(129)
}

/// Option-parsing error: `git` prints just the `error:` line, then the usage
/// block, and exits 129.
fn error_with_usage(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE}");
    ExitCode::from(129)
}

/// Option-parsing error that git reports *without* the usage block.
fn error_only(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(129)
}

/// Fatal runtime error (bad object name, non-tree object): exit 128.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// `git ls-tree` — list the contents of a tree object.
///
/// `<tree-ish>` is resolved through `rev_parse_single`, so commits, tags, refs,
/// raw tree ids and `<rev>:<path>` all peel to the tree they name; anything that
/// fails to resolve or does not peel to a tree is a fatal (128) error, matching
/// git's `Not a valid object name` / `not a tree object`.
///
/// Run from a subdirectory, the listing is scoped to that directory's subtree
/// and paths print relative to it (git's `prefix`/`chomp_prefix`), unless
/// `--full-name` (full paths, same scope) or `--full-tree` (whole tree, full
/// paths) is given. Operands are likewise taken relative to the current
/// directory.
///
/// Not honoured: pathspec magic (`:(glob)` and friends) — that would silently
/// select a different entry set than git, so it is rejected outright.
pub fn ls_tree(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts {
        recurse: false,
        show_trees: false,
        dirs_only: false,
        long: false,
        nul: false,
        name_only: false,
        object_only: false,
        abbrev: Abbrev::Full,
        format: None,
        paths: Vec::new(),
        match_all: false,
        strip_prefix: None,
    };

    // The active `OPT_CMDMODE` value plus the spelling the user typed, which is
    // what git quotes back in the "cannot be used together" diagnostic.
    let mut cmdmode: Option<(CmdMode, String)> = None;
    let mut treeish: Option<&str> = None;
    let mut positionals: Vec<&str> = Vec::new();
    let mut no_more_opts = false;
    // git's `chomp_prefix = 0` (display full paths) and `full_tree` (widen the
    // scope back to the whole tree, implying --full-name).
    let mut full_name = false;
    let mut full_tree = false;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        if !no_more_opts && a == "--" {
            no_more_opts = true;
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, which is why the test sits before `canonical_long`
        // rather than in `LONG_OPTS`. This table has no `PARSE_OPT_HIDDEN`
        // entry, so `USAGE_FULL` renders the same block `-h` prints.
        if !no_more_opts && a == "--help-all" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        if !no_more_opts && a.len() > 1 && a.starts_with('-') {
            let resolved = match super::canonical_long(a, LONG_OPTS) {
                super::Long::Name(name) => name,
                super::Long::Ambiguous(first, second) => {
                    return Ok(super::ambiguous_option(a, &first, &second, USAGE))
                }
            };
            let a = resolved.as_ref();
            if let Some(long_opt) = a.strip_prefix("--") {
                // `--<name>=<value>` splits here; a bare `--<name>` has no value.
                let (name, inline) = match long_opt.split_once('=') {
                    Some((n, v)) => (n, Some(v)),
                    None => (long_opt, None),
                };
                match name {
                    "long" | "name-only" | "name-status" | "object-only" => {
                        let mode = match name {
                            "long" => CmdMode::Long,
                            "name-only" => CmdMode::NameOnly,
                            "name-status" => CmdMode::NameStatus,
                            _ => CmdMode::ObjectOnly,
                        };
                        if let Some(code) = set_cmdmode(&mut cmdmode, mode, a) {
                            return Ok(code);
                        }
                    }
                    // `--full-name` clears git's `chomp_prefix` so paths print
                    // root-relative; `--full-tree` also drops the cwd prefix,
                    // widening the listing to the whole tree (implies --full-name).
                    "full-name" => full_name = true,
                    "no-full-name" => full_name = false,
                    "full-tree" => full_tree = true,
                    "no-full-tree" => full_tree = false,
                    "abbrev" => {
                        opts.abbrev = match inline {
                            None => Abbrev::Auto,
                            Some(v) => match parse_abbrev(v) {
                                Some(x) => x,
                                None => {
                                    return Ok(error_only("option `abbrev' expects a numerical value"))
                                }
                            },
                        };
                    }
                    "no-abbrev" => opts.abbrev = Abbrev::Full,
                    "format" => {
                        let value = match inline {
                            Some(v) => v.to_string(),
                            None => match it.next() {
                                Some(v) => v.clone(),
                                None => {
                                    return Ok(error_with_usage("option `format' requires a value"))
                                }
                            },
                        };
                        opts.format = Some(value);
                    }
                    _ => return Ok(error_with_usage(&format!("unknown option `{long_opt}'"))),
                }
            } else {
                // Grouped short flags, e.g. `-rt`.
                for c in a[1..].chars() {
                    match c {
                        'r' => opts.recurse = true,
                        't' => opts.show_trees = true,
                        'd' => opts.dirs_only = true,
                        'z' => opts.nul = true,
                        'l' => {
                            if let Some(code) = set_cmdmode(&mut cmdmode, CmdMode::Long, "-l") {
                                return Ok(code);
                            }
                        }
                        'h' => {
                            print!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => return Ok(error_with_usage(&format!("unknown switch `{c}'"))),
                    }
                }
            }
            continue;
        }
        if treeish.is_none() {
            treeish = Some(a.as_str());
        } else {
            positionals.push(a.as_str());
        }
    }

    // git rejects `--format` alongside any cmdmode before it looks at operands.
    if opts.format.is_some() && cmdmode.is_some() {
        return Ok(usage_msg_opt(
            "--format can't be combined with other format-altering options",
        ));
    }

    // `cmd_ls_tree()`'s `m2f` loop, which runs after that check: a `--format`
    // spelled exactly like a cmdmode's canonical template *becomes* that cmdmode.
    let mut mode = cmdmode.map(|(m, _)| m);
    match opts.format.as_deref() {
        Some(FMT_DEFAULT) => (mode, opts.format) = (None, None),
        Some(FMT_LONG) => (mode, opts.format) = (Some(CmdMode::Long), None),
        Some(FMT_NAME_ONLY) => (mode, opts.format) = (Some(CmdMode::NameOnly), None),
        Some(FMT_OBJECT_ONLY) => (mode, opts.format) = (Some(CmdMode::ObjectOnly), None),
        _ => {}
    }

    match mode {
        Some(CmdMode::NameOnly) | Some(CmdMode::NameStatus) => opts.name_only = true,
        Some(CmdMode::ObjectOnly) => opts.object_only = true,
        Some(CmdMode::Long) => opts.long = true,
        None => {}
    }

    let Some(spec) = treeish else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    };

    let repo = crate::setup::discover()?;
    // `repo_config(git_default_config)`: seeds `quote_path_fully` from
    // `core.quotePath` before the first path is rendered.
    crate::quote::init(&repo);

    // git's `prefix` from setup_git_directory(): the path from the worktree root
    // down to the current directory, carrying a trailing '/' (empty at the root).
    // `--full-tree` drops it, so the whole tree is listed from the root.
    let mut cwd_prefix = repo
        .prefix()
        .ok()
        .flatten()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .filter(|s| !s.is_empty())
        .map(|s| format!("{s}/"))
        .unwrap_or_default();
    if full_tree {
        cwd_prefix.clear();
    }

    // git's `chomp_prefix`: display paths relative to the cwd prefix unless
    // `--full-name`/`--full-tree` asked for full (root-relative) names.
    opts.strip_prefix = if !full_name && !cwd_prefix.is_empty() {
        Some(cwd_prefix.clone().into_bytes())
    } else {
        None
    };

    // git: "-d -r should imply -t, but -d by itself should not have to."
    if opts.dirs_only && opts.recurse {
        opts.show_trees = true;
    }

    // Path filters. A `:`-prefixed operand carries pathspec magic: git's
    // ls-tree accepts only `top` (`:/`) and `literal`, rejecting every other
    // magic with a fatal (128) diagnostic. Parse it the way git does, then
    // treat the magic-stripped remainder as an ordinary filter. Trailing '/' is
    // significant (a directory filter `dir/` lists the directory's contents,
    // while `dir` lists the directory entry itself); an empty remainder anchored
    // at the root (`:/`, `:(top)`, or a bare `:` at the repo root) matches
    // everything.
    //
    // Operands are interpreted relative to the cwd prefix (git's
    // PATHSPEC_PREFER_CWD): each is prepended with `cwd_prefix`, except a
    // `top`-magic operand, which is anchored at the root. With no operands and a
    // non-empty prefix, the prefix itself becomes the sole pathspec — this is
    // what limits a bare `git ls-tree` to the current directory's subtree.
    for p in &positionals {
        let (cleaned, from_top) = if p.starts_with(':') {
            match parse_pathspec_magic(p) {
                Ok(parsed) => parsed,
                Err(code) => return Ok(code),
            }
        } else {
            ((*p).to_string(), false)
        };
        // `top` magic anchors at the root; everything else is resolved against the
        // cwd prefix, then run through `normalize_path_copy()` — which is what makes
        // `git ls-tree HEAD ..` from `sub/deep` list `sub/` and `git ls-tree HEAD .`
        // list the directory it was run in. Without the collapse the element stayed
        // the literal `sub/deep/..`, which matches no entry and printed nothing.
        let joined = match from_top {
            true => cleaned,
            false => format!("{cwd_prefix}{cleaned}"),
        };
        // An element that names the directory it started from normalizes to the
        // empty match, which selects the whole tree — `:`, `:/`, `:(top)` and a `.`
        // at the repository root all land here. `normalize_pathspec` spells that
        // empty result `.`, and returns `""` only for an empty input.
        match super::add::normalize_pathspec(&joined) {
            n if n.is_empty() || n == "." => opts.match_all = true,
            n => opts.paths.push(n.into_bytes()),
        }
    }
    if positionals.is_empty() && !cwd_prefix.is_empty() {
        opts.paths.push(cwd_prefix.clone().into_bytes());
    }

    // `cmd_ls_tree` splits the two failures the way `object-name.c` does:
    // `repo_get_oid_with_flags()` only has to *name* an object — a full-length
    // hex string is decoded and returned without the odb being consulted (see
    // [`crate::objname::full_hex`]) — and it is `repo_parse_tree_indirect()`
    // that reports "not a tree object", for a missing object and a non-tree
    // alike. Resolving through the odb here would mis-report an absent but
    // well-formed id as an invalid name.
    let Some(id) = crate::objname::resolve(&repo, spec) else {
        return Ok(fatal(&format!("Not a valid object name {spec}")));
    };
    let peeled = repo
        .find_object(id)
        .ok()
        .and_then(|object| object.peel_to_tree().ok());
    let Some(tree) = peeled else {
        return Ok(fatal("not a tree object"));
    };

    // Entry names are arbitrary bytes, so the whole listing is assembled and
    // written as bytes; routing it through a `String` would turn a name that is
    // not valid UTF-8 into U+FFFD.
    let mut out: Vec<u8> = Vec::new();
    if let Err(bad) = walk(&repo, tree, b"", &opts, &mut out) {
        // `strbuf_expand_bad_format()` dies while rendering the first entry, so
        // nothing this run produced reaches stdout.
        return Ok(match bad.downcast::<BadFormat>() {
            Ok(BadFormat(msg)) => fatal(&msg),
            Err(other) => return Err(other),
        });
    }
    let stdout = std::io::stdout();
    let mut sink = std::io::BufWriter::new(stdout.lock());
    sink.write_all(&out)?;
    sink.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// A `--format` template `strbuf_expand_bad_format()` rejects, carried out of the
/// walk so the fatal is reported once with git's wording and exit 128.
#[derive(Debug)]
struct BadFormat(String);

impl std::fmt::Display for BadFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BadFormat {}

/// Apply an `OPT_CMDMODE`-style flag, rejecting a switch to a *different* mode.
///
/// git quotes the option just seen first and the one already in effect second:
/// `--name-only --name-status` reports `'--name-status' and '--name-only'`.
/// Repeating the same mode is accepted silently.
fn set_cmdmode(
    current: &mut Option<(CmdMode, String)>,
    mode: CmdMode,
    spelling: &str,
) -> Option<ExitCode> {
    if let Some((prev, prev_name)) = current {
        if *prev != mode {
            return Some(error_only(&format!(
                "options '{spelling}' and '{prev_name}' cannot be used together"
            )));
        }
        return None;
    }
    *current = Some((mode, spelling.to_string()));
    None
}

/// Parse an `--abbrev=<n>` value the way git's `parse_opt_abbrev_cb` does:
/// non-numeric is an error, `0` disables abbreviation, and any other value below
/// `MINIMUM_ABBREV` (including negatives) is raised to it.
fn parse_abbrev(v: &str) -> Option<Abbrev> {
    // The `strtol`-then-narrow quirks live in one place; see
    // [`crate::abbrev::parse_opt_abbrev_value`].
    let n = i64::from(crate::abbrev::parse_opt_abbrev_value(v)?);
    Some(match n {
        0 => Abbrev::Full,
        n if n < MINIMUM_ABBREV as i64 => Abbrev::Len(MINIMUM_ABBREV),
        n => Abbrev::Len(n as usize),
    })
}

// Pathspec magic bits, one per entry in `MAGIC_TABLE`.
const M_TOP: u32 = 1 << 0;
const M_LITERAL: u32 = 1 << 1;
const M_GLOB: u32 = 1 << 2;
const M_ICASE: u32 = 1 << 3;
const M_EXCLUDE: u32 = 1 << 4;
const M_ATTR: u32 = 1 << 5;

/// git's `pathspec_magic[]` table: (long name, short mnemonic or `'\0'`, bit).
/// The order matters — git lists rejected magic back to the user in this order.
const MAGIC_TABLE: &[(&str, char, u32)] = &[
    ("top", '/', M_TOP),
    ("literal", '\0', M_LITERAL),
    ("glob", '\0', M_GLOB),
    ("icase", '\0', M_ICASE),
    ("exclude", '!', M_EXCLUDE),
    ("attr", '\0', M_ATTR),
];

/// The magic `git ls-tree` accepts. git parses it with
/// `PATHSPEC_ALL_MAGIC & ~(PATHSPEC_FROMTOP | PATHSPEC_LITERAL)` as the mask of
/// *unsupported* magic, so only `top` and `literal` survive; both are no-ops
/// here (output is already root-relative and matching is already literal).
const MAGIC_SUPPORTED: u32 = M_TOP | M_LITERAL;

/// Parse the pathspec magic on a `:`-prefixed operand exactly as stock
/// `git ls-tree` does, returning the magic-stripped path plus whether `top`
/// magic was present (which anchors the path at the root rather than the cwd),
/// or a fatal (128) `ExitCode` carrying git's verbatim diagnostic on rejected
/// magic.
///
/// Handles both spellings: long form `:(name,name,...)path` and the short
/// mnemonic form `:/`, `:!`, `:^`, `::`. Rejections match git byte-for-byte:
///   * unknown long name  -> `Invalid pathspec magic '<n>' in '<elt>'`
///   * missing `)`        -> `Missing ')' at the end of pathspec magic in '<elt>'`
///   * `literal`+`glob`   -> `<elt>: 'literal' and 'glob' are incompatible`
///   * any other magic    -> `<elt>: pathspec magic not supported by this command: <list>`
fn parse_pathspec_magic(elt: &str) -> std::result::Result<(String, bool), ExitCode> {
    let after = &elt[1..]; // strip the leading ':'
    let mut magic: u32 = 0;
    let path: String;

    if let Some(body) = after.strip_prefix('(') {
        // Long form: comma-separated magic names inside parentheses.
        let Some(close) = body.find(')') else {
            return Err(fatal(&crate::pathspec::missing_closing_paren(elt.into())));
        };
        for field in body[..close].split(',') {
            if field.is_empty() {
                continue; // git skips empty elements (e.g. `,,`)
            }
            // `attr:<value>` is the attr magic with an argument.
            let matched = MAGIC_TABLE.iter().find(|(name, _, _)| {
                *name == field || (*name == "attr" && field.starts_with("attr:"))
            });
            match matched {
                Some((_, _, bit)) => magic |= bit,
                None => {
                    return Err(fatal(&crate::pathspec::invalid_magic(field.into(), elt.into())))
                }
            }
        }
        // git rejects this specific pair while parsing, before the support check.
        if magic & M_LITERAL != 0 && magic & M_GLOB != 0 {
            return Err(fatal(&crate::pathspec::incompatible_literal_glob(elt.into())));
        }
        path = body[close + 1..].to_string();
    } else {
        // Short form: consume mnemonic bytes until a non-mnemonic or a `:`.
        let bytes = after.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i] != b':' {
            let ch = bytes[i] as char;
            // `^` is an accepted short alias for exclude alongside `!`.
            let bit = if ch == '^' {
                Some(M_EXCLUDE)
            } else {
                MAGIC_TABLE
                    .iter()
                    .find(|(_, m, _)| *m != '\0' && *m == ch)
                    .map(|(_, _, b)| *b)
            };
            match bit {
                Some(b) => {
                    magic |= b;
                    i += 1;
                }
                None => break,
            }
        }
        if i < bytes.len() && bytes[i] == b':' {
            i += 1; // a terminating `:` is consumed (so `::path` -> `path`)
        }
        path = after[i..].to_string();
    }

    let unsupported = magic & !MAGIC_SUPPORTED;
    if unsupported != 0 {
        let mut parts: Vec<String> = Vec::new();
        for (name, mnem, bit) in MAGIC_TABLE {
            if unsupported & bit != 0 {
                if *mnem != '\0' {
                    parts.push(format!("'{name}' (mnemonic: '{mnem}')"));
                } else {
                    parts.push(format!("'{name}'"));
                }
            }
        }
        return Err(fatal(&crate::pathspec::magic_not_supported(
            elt.into(),
            &parts.join(", "),
        )));
    }

    Ok((path, magic & M_TOP != 0))
}

/// Recursively render `tree` (rooted at `prefix`, e.g. `b"dir/"`) into `out`.
fn walk(
    repo: &gix::Repository,
    tree: gix::Tree<'_>,
    prefix: &[u8],
    opts: &Opts,
    out: &mut Vec<u8>,
) -> Result<()> {
    // Materialise the entries so the borrow on the tree's data ends before we
    // recurse (child lookups need mutable access to a fresh buffer).
    let entries: Vec<(EntryMode, BString, ObjectId)> = tree
        .decode()?
        .entries
        .iter()
        .map(|e| (e.mode, e.filename.to_owned(), e.oid.to_owned()))
        .collect();

    for (mode, filename, oid) in entries {
        let mut name = prefix.to_vec();
        name.extend_from_slice(&filename);

        if mode.is_tree() {
            // git's show_tree_common: a matched tree is recursed into when
            // `show_recursive` (== should_descend) holds, and its own line is
            // suppressed while recursing unless `-t` (LS_SHOW_TREES) is set.
            // The entry is "interesting" — and therefore a candidate to print —
            // when it matches a pathspec directly or is an ancestor of one (so
            // ancestor trees still appear under `-t`, matching git).
            let recurse = should_descend(&name, opts);
            let interesting = path_selects(&name, opts) || is_ancestor_of_spec(&name, opts);
            if interesting && (!recurse || opts.show_trees) {
                write_entry(repo, out, mode, &oid, &name, opts)?;
            }
            if recurse {
                let child = repo.find_object(oid)?.peel_to_tree()?;
                let mut base = name.clone();
                base.push(b'/');
                walk(repo, child, &base, opts, out)?;
            }
        } else if !opts.dirs_only && path_selects(&name, opts) {
            write_entry(repo, out, mode, &oid, &name, opts)?;
        }
    }
    Ok(())
}

/// Whether `name` is selected by the path filters (empty filters select all).
///
/// A filter `p` selects `name` when it names the entry exactly or when the entry
/// lives inside the directory `p`. A trailing '/' on the filter is ignored for
/// this test (`dir` and `dir/` both select `dir` and everything under it); the
/// distinction between the two only affects whether the directory line is shown,
/// which git decides via the recursion rules in `walk`.
fn path_selects(name: &[u8], opts: &Opts) -> bool {
    opts.match_all
        || opts.paths.is_empty()
        || opts.paths.iter().any(|p| {
            let base = trim_trailing_slashes(p);
            name == base || (name.starts_with(base) && name.get(base.len()) == Some(&b'/'))
        })
}

/// `p` without its trailing '/' separators.
fn trim_trailing_slashes(p: &[u8]) -> &[u8] {
    let end = p.iter().rposition(|b| *b != b'/').map_or(0, |i| i + 1);
    &p[..end]
}

/// Whether the sub-tree `name` is a strict ancestor of some path filter, i.e. a
/// filter points at or below it (git's `tree_entry_interesting` visits such
/// trees even when their own line is not shown). A `dir/` filter is an ancestor
/// of `dir` (its contents live below), which is what makes a bare cwd-scoped
/// `ls-tree` descend into the current directory.
fn is_ancestor_of_spec(name: &[u8], opts: &Opts) -> bool {
    opts.paths
        .iter()
        .any(|p| p.starts_with(name) && p.get(name.len()) == Some(&b'/'))
}

/// Whether the sub-tree `name` must be descended into (git's `show_recursive`).
///
/// Always when `-r` is set; otherwise only when a path filter points strictly
/// below this tree (so an exact `<dir>` filter shows the tree line without
/// recursing, while `<dir>/<file>` or a `<dir>/` directory filter descends).
fn should_descend(name: &[u8], opts: &Opts) -> bool {
    opts.recurse || is_ancestor_of_spec(name, opts)
}

/// Render one entry into `out`, honouring `--format`, `--name-only`,
/// `--object-only`, `--long` and `-z`.
fn write_entry(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    mode: EntryMode,
    oid: &ObjectId,
    name: &[u8],
    opts: &Opts,
) -> Result<()> {
    let term = if opts.nul { b'\0' } else { b'\n' };

    // git prints paths relative to `ls_tree_prefix` when `chomp_prefix` is set.
    let disp = display_name(name, opts);

    if let Some(fmt) = &opts.format {
        expand_format(repo, fmt, out, mode, oid, &disp, opts)?;
        out.push(term);
        return Ok(());
    }

    if opts.name_only {
        write_name(out, &disp, opts);
        out.push(term);
        return Ok(());
    }
    if opts.object_only {
        out.extend_from_slice(object_id_str(repo, oid, opts)?.as_bytes());
        out.push(term);
        return Ok(());
    }

    let mode_str = git_mode(mode);
    let type_str = git_type(mode);
    let oid_str = object_id_str(repo, oid, opts)?;

    if opts.long {
        let size = entry_size(repo, mode, oid)?;
        out.extend_from_slice(format!("{mode_str} {type_str} {oid_str} {size:>7}\t").as_bytes());
    } else {
        out.extend_from_slice(format!("{mode_str} {type_str} {oid_str}\t").as_bytes());
    }
    write_name(out, &disp, opts);
    out.push(term);
    Ok(())
}

/// `write_name_quoted(name, fp, terminator)` as the four dedicated printers reach
/// it, via `show_tree_common_default_long()` / `show_tree_name_only()`.
///
/// Those two spell the `-z` case out themselves as a bare `fputs()`, and
/// `write_name_quoted()` would take the same branch anyway (a NUL terminator is
/// the falsy one): under `-z` git writes the path **raw**, and quotes it only when
/// records are newline-terminated. `show_tree_fmt()`'s `%(path)` does not share
/// this — see [`expand_format`].
fn write_name(out: &mut Vec<u8>, disp: &[u8], opts: &Opts) {
    match opts.nul {
        true => out.extend_from_slice(disp),
        false => out.extend_from_slice(&crate::quote::quoted_name_bytes(disp)),
    }
}

/// Render `name` (a root-relative path) as git would display it: relative to the
/// cwd prefix when `--full-name`/`--full-tree` did not clear `chomp_prefix`,
/// otherwise verbatim. Mirrors the `relative_path()` half of git's
/// `write_name_quoted_relative` — quoting is applied afterwards, by
/// [`write_name`] or by `%(path)` — including the `./` and `../` forms git emits
/// for the prefix directory itself and for entries above it.
fn display_name(name: &[u8], opts: &Opts) -> Vec<u8> {
    match &opts.strip_prefix {
        None => name.to_vec(),
        Some(prefix) => relative_path(name, trim_trailing_slashes(prefix)),
    }
}

/// git's `relative_path`: the path of `name` as seen from directory `base`, both
/// given root-relative. Ascends with `../` segments and collapses an empty
/// result to `./`, matching git byte-for-byte (`dir` from `dir/sub` -> `../`,
/// `dir/sub` from `dir/sub` -> `./`, `dir/a` from `dir` -> `a`).
fn relative_path(name: &[u8], base: &[u8]) -> Vec<u8> {
    let target: Vec<&[u8]> = name.split_str("/").filter(|s| !s.is_empty()).collect();
    let base: Vec<&[u8]> = base.split_str("/").filter(|s| !s.is_empty()).collect();

    let common = target
        .iter()
        .zip(base.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut out = Vec::new();
    for _ in 0..base.len() - common {
        out.extend_from_slice(b"../");
    }
    for (i, seg) in target[common..].iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(seg);
    }
    if out.is_empty() {
        out.extend_from_slice(b"./");
    }
    out
}

/// Expand one `--format` template for a single entry — `show_tree_fmt()`.
///
/// The loop is git's `while (strbuf_expand_step(...))`: literal text up to the
/// next `%` is copied through, then exactly one of `%%`, a `strbuf_expand_literal`
/// escape (`%n`, `%x<hh>`) or a documented `%(atom)` must follow. Anything else is
/// `strbuf_expand_bad_format()`, which dies — and since the template is the same
/// for every entry, that happens while rendering the first one, before any output
/// is produced.
///
/// `%(path)` goes through `quote_c_style()` **unconditionally**: unlike the four
/// dedicated printers, `show_tree_fmt()` has no `-z` special case, so a template
/// that reaches here quotes the path even when records end in NUL.
fn expand_format(
    repo: &gix::Repository,
    fmt: &str,
    out: &mut Vec<u8>,
    mode: EntryMode,
    oid: &ObjectId,
    name: &[u8],
    opts: &Opts,
) -> Result<()> {
    let b = fmt.as_bytes();
    let mut i = 0;
    while let Some(percent) = b[i..].iter().position(|c| *c == b'%') {
        out.extend_from_slice(&b[i..i + percent]);
        // `strbuf_expand_step` leaves `format` pointing just past the `%`.
        let rest = &b[i + percent + 1..];
        i += percent + 1;

        if rest.first() == Some(&b'%') {
            out.push(b'%');
            i += 1;
        } else if rest.first() == Some(&b'n') {
            // `strbuf_expand_literal`, case 'n'.
            out.push(b'\n');
            i += 1;
        } else if let Some(byte) = hex2chr(rest) {
            // `strbuf_expand_literal`, case 'x': `%x00` == NUL, `%x0a` == LF.
            out.push(byte);
            i += 3;
        } else if let Some(len) = expand_atom(repo, rest, out, mode, oid, name, opts)? {
            i += len;
        } else {
            return Err(bad_format(rest).into());
        }
    }
    out.extend_from_slice(&b[i..]);
    Ok(())
}

/// `strbuf_expand_literal`'s `case 'x'`: `x` followed by two hex digits.
fn hex2chr(rest: &[u8]) -> Option<u8> {
    let [b'x', hi, lo, ..] = rest else { return None };
    let v = |c: u8| (c as char).to_digit(16);
    Some((v(*hi)? * 16 + v(*lo)?) as u8)
}

/// The `%(...)` atoms `show_tree_fmt()` knows, in its `skip_prefix` order.
/// Returns how many bytes of `rest` the atom consumed, or `None` if none matched.
fn expand_atom(
    repo: &gix::Repository,
    rest: &[u8],
    out: &mut Vec<u8>,
    mode: EntryMode,
    oid: &ObjectId,
    name: &[u8],
    opts: &Opts,
) -> Result<Option<usize>> {
    let atoms: &[&str] = &[
        "(objectmode)",
        "(objecttype)",
        "(objectsize:padded)",
        "(objectsize)",
        "(objectname)",
        "(path)",
    ];
    let Some(atom) = atoms.iter().find(|a| rest.starts_with(a.as_bytes())) else {
        return Ok(None);
    };
    match *atom {
        "(objectmode)" => out.extend_from_slice(git_mode(mode).as_bytes()),
        "(objecttype)" => out.extend_from_slice(git_type(mode).as_bytes()),
        "(objectsize:padded)" => {
            out.extend_from_slice(format!("{:>7}", entry_size(repo, mode, oid)?).as_bytes())
        }
        "(objectsize)" => out.extend_from_slice(entry_size(repo, mode, oid)?.as_bytes()),
        "(objectname)" => out.extend_from_slice(object_id_str(repo, oid, opts)?.as_bytes()),
        _ => out.extend_from_slice(&crate::quote::quoted_name_bytes(name)),
    }
    Ok(Some(atom.len()))
}

/// `strbuf_expand_bad_format(format, "ls-tree")`: the three ways a template
/// element is rejected, in the order git tests them.
fn bad_format(rest: &[u8]) -> BadFormat {
    let shown = String::from_utf8_lossy(rest);
    if rest.first() != Some(&b'(') {
        return BadFormat(format!(
            "bad ls-tree format: element '{shown}' does not start with '('"
        ));
    }
    match rest.iter().position(|c| *c == b')') {
        None => BadFormat(format!(
            "bad ls-tree format: element '{shown}' does not end in ')'"
        )),
        Some(end) => BadFormat(format!(
            "bad ls-tree format: %{}",
            String::from_utf8_lossy(&rest[..=end])
        )),
    }
}

/// The size column: blobs (including symlinks) report their byte count, trees
/// and submodule commits report `-`, exactly as git does.
fn entry_size(repo: &gix::Repository, mode: EntryMode, oid: &ObjectId) -> Result<String> {
    Ok(if mode.is_blob_or_symlink() {
        repo.find_header(*oid)?.size().to_string()
    } else {
        "-".to_string()
    })
}

/// The 6-digit octal mode exactly as stock `git ls-tree` prints it.
fn git_mode(mode: EntryMode) -> &'static str {
    match mode.kind() {
        EntryKind::Tree => "040000",
        EntryKind::Blob => "100644",
        EntryKind::BlobExecutable => "100755",
        EntryKind::Link => "120000",
        EntryKind::Commit => "160000",
    }
}

/// The object type column: `blob`, `tree`, or `commit` (as git names them).
fn git_type(mode: EntryMode) -> &'static str {
    match mode.kind() {
        EntryKind::Tree => "tree",
        EntryKind::Commit => "commit",
        EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => "blob",
    }
}

/// The object id column, full or abbreviated per `--abbrev`.
///
/// `--abbrev=<n>` is a *floor*, not a width: git prints
/// `repo_find_unique_abbrev()`, which starts at `n` hex digits and keeps
/// extending while another object in the database shares the prefix. Two blobs
/// that agree in their first four nibbles are therefore printed with five, which
/// a plain truncation renders identically and wrongly.
fn object_id_str(repo: &gix::Repository, oid: &ObjectId, opts: &Opts) -> Result<String> {
    Ok(match opts.abbrev {
        Abbrev::Full => oid.to_hex().to_string(),
        Abbrev::Len(n) if n >= oid.kind().len_in_hex() => oid.to_hex().to_string(),
        Abbrev::Len(n) => gix::odb::store::prefix::disambiguate::Candidate::new(*oid, n)
            .ok()
            .and_then(|c| repo.objects.disambiguate_prefix(c).ok().flatten())
            .map_or_else(|| oid.to_hex_with_len(n).to_string(), |p| p.to_string()),
        Abbrev::Auto => oid.attach(repo).shorten_or_id().to_string(),
    })
}
