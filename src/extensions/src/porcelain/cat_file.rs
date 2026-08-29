use anyhow::Result;
use std::io::{BufRead, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::objs::tree::EntryKind;
use gix::objs::{Kind, TreeRefIter};

/// The usage block up to and including the `--use-mailmap` line.
pub(super) const USAGE_HEAD: &str = "\
usage: git cat-file <type> <object>
   or: git cat-file (-e | -p | -t | -s) <object>
   or: git cat-file (--textconv | --filters)
                    [<rev>:<path|tree-ish> | --path=<path|tree-ish> <rev>]
   or: git cat-file (--batch | --batch-check | --batch-command) [--batch-all-objects]
                    [--buffer] [--follow-symlinks] [--unordered]
                    [--textconv | --filters] [-Z]

Check object existence or emit object contents
    -e                    check if <object> exists
    -p                    pretty-print <object> content

Emit [broken] object attributes
    -t                    show object type (one of 'blob', 'tree', 'commit', 'tag', ...)
    -s                    show object size
    --[no-]use-mailmap    use mail map file
";

/// [`USAGE_HEAD`] as `usage_with_options_internal()` renders it for
/// `USAGE_FULL`: the hidden `--allow-unknown-type` is left in. Captured
/// byte-for-byte from stock git 2.55.0's `git cat-file --help-all`, cut at the
/// same `--mailmap` line [`USAGE_HEAD`] is cut at.
pub(super) const USAGE_HEAD_ALL: &str = r#"usage: git cat-file <type> <object>
   or: git cat-file (-e | -p | -t | -s) <object>
   or: git cat-file (--textconv | --filters)
                    [<rev>:<path|tree-ish> | --path=<path|tree-ish> <rev>]
   or: git cat-file (--batch | --batch-check | --batch-command) [--batch-all-objects]
                    [--buffer] [--follow-symlinks] [--unordered]
                    [--textconv | --filters] [-Z]

Check object existence or emit object contents
    -e                    check if <object> exists
    -p                    pretty-print <object> content

Emit [broken] object attributes
    -t                    show object type (one of 'blob', 'tree', 'commit', 'tag', ...)
    -s                    show object size
    --[no-]allow-unknown-type
                          historical option -- no-op
    --[no-]use-mailmap    use mail map file
"#;

/// `--mailmap` renders differently depending on **which option table** the block
/// was rendered from, not on which stream it went to.
///
/// `preprocess_options()` (parse-options.c:903-963) replaces each `OPTION_ALIAS`
/// slot with a copy of its source option — here `--use-mailmap`, an `OPT_BOOL`,
/// which carries `PARSE_OPT_NOARG`. Every block rendered from *inside*
/// `parse_options()` therefore sees the copy and prints no argument marker.
/// The builtin's own `usage_with_options()` calls run after `parse_options()`
/// has returned and are handed the **original** array, whose alias slot has no
/// `PARSE_OPT_NOARG`, so `usage_argh()` fires and prints `_("...")` for its null
/// `argh` (parse-options.c:1443-1445, 1286).
///
/// Verified against stock 2.55.0: `git cat-file -h`, `--help-all`, `--zzbogus`
/// and `-Q` all print `--[no-]mailmap`; bare `git cat-file` and
/// `git cat-file --batch --path=x` print `--[no-]mailmap ...`. The two blocks
/// are otherwise byte-identical.
const ALIAS_HELP: &str = "    --[no-]mailmap        alias of --use-mailmap\n";
const ALIAS_ERROR: &str = "    --[no-]mailmap ...    alias of --use-mailmap\n";

/// Everything after the `--mailmap` line.
pub(super) const USAGE_TAIL: &str = "
Batch objects requested on stdin (or --batch-all-objects)
    --batch[=<format>]    show full <object> or <rev> contents
    --batch-check[=<format>]
                          like --batch, but don't emit <contents>
    -Z                    stdin and stdout is NUL-terminated
    --batch-command[=<format>]
                          read commands from stdin
    --batch-all-objects   with --batch[-check]: ignores stdin, batches all known objects

Change or optimize batch output
    --[no-]buffer         buffer --batch output
    --[no-]follow-symlinks
                          follow in-tree symlinks
    --[no-]unordered      do not order objects before emitting them

Emit object (blob or tree) with conversion or filter (stand-alone, or with batch)
    --textconv            run textconv on object's content
    --filters             run filters on object's content
    --[no-]path blob|tree use a <path> for (--textconv | --filters); Not with 'batch'
    --[no-]filter <args>  object filtering

";

/// [`USAGE_TAIL`] for `USAGE_FULL`: the hidden `-z` is left in. Same capture,
/// same cut.
pub(super) const USAGE_TAIL_ALL: &str = r#"
Batch objects requested on stdin (or --batch-all-objects)
    --batch[=<format>]    show full <object> or <rev> contents
    --batch-check[=<format>]
                          like --batch, but don't emit <contents>
    -z                    stdin is NUL-terminated
    -Z                    stdin and stdout is NUL-terminated
    --batch-command[=<format>]
                          read commands from stdin
    --batch-all-objects   with --batch[-check]: ignores stdin, batches all known objects

Change or optimize batch output
    --[no-]buffer         buffer --batch output
    --[no-]follow-symlinks
                          follow in-tree symlinks
    --[no-]unordered      do not order objects before emitting them

Emit object (blob or tree) with conversion or filter (stand-alone, or with batch)
    --textconv            run textconv on object's content
    --filters             run filters on object's content
    --[no-]path blob|tree use a <path> for (--textconv | --filters); Not with 'batch'
    --[no-]filter <args>  object filtering

"#;

fn usage(alias: &str) -> String {
    format!("{USAGE_HEAD}{alias}{USAGE_TAIL}")
}

/// [`usage`] over the `USAGE_FULL` halves, for `--help-all`. The alias line is
/// still chosen by the caller: which spelling it gets depends on the option
/// table the block was rendered from, not on which block it is.
fn usage_all(alias: &str) -> String {
    format!("{USAGE_HEAD_ALL}{alias}{USAGE_TAIL_ALL}")
}

/// `cmd_cat_file()`'s `struct option options[]` (builtin/cat-file.c:1095-1130),
/// in table order, as [`super::resolve_long`] reads it. The four `--batch*`
/// entries, `--textconv` and `--filters` carry `PARSE_OPT_NONEG`.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "allow-unknown-type", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "use-mailmap", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "mailmap", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "batch", neg: false, arg: super::Arg::Optional },
    super::LongOpt { name: "batch-check", neg: false, arg: super::Arg::Optional },
    super::LongOpt { name: "batch-command", neg: false, arg: super::Arg::Optional },
    super::LongOpt { name: "batch-all-objects", neg: false, arg: super::Arg::None },
    super::LongOpt { name: "buffer", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "follow-symlinks", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "unordered", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "textconv", neg: false, arg: super::Arg::None },
    super::LongOpt { name: "filters", neg: false, arg: super::Arg::None },
    super::LongOpt { name: "path", neg: true, arg: super::Arg::Required },
    super::LongOpt { name: "filter", neg: true, arg: super::Arg::Required },
];

/// `OPT_ALIAS(0, "mailmap", "use-mailmap")` (builtin/cat-file.c:1111).
/// `preprocess_options()` turns each alias into a copy of its source and records
/// the pair in `ctx->alias_groups`, which is the only thing that stops the two
/// from making each other ambiguous.
const ALIAS_GROUPS: &[&[&str]] = &[&["mailmap", "use-mailmap"]];

/// Print the usage block the way git's `usage_with_options()` does — on stderr,
/// with the `...` alias rendering.
fn usage_err() {
    eprint!("{}", usage(ALIAS_ERROR));
}

/// The block as `parse_options()` renders it — from the preprocessed table, so
/// the alias prints bare. This is the one an unknown option or switch gets.
fn parse_usage_err() {
    eprint!("{}", usage(ALIAS_HELP));
}

/// git prefixes a `fatal:` line, then a blank line, then the usage block.
fn die_usage(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    eprintln!();
    usage_err();
    Ok(ExitCode::from(129))
}

/// git's single `OPT_CMDMODE` group for this builtin. `-t`/`-s`/`-p`/`-e` are the
/// query modes, `--textconv`/`--filters` the two content transforms, and
/// `--batch-all-objects` shares the same slot (value `'b'`) — which is why git
/// rejects it next to `-p` but accepts it next to `--batch`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Type,
    Size,
    Print,
    Exists,
    Textconv,
    Filters,
    AllObjects,
}

impl Mode {
    /// The spelling git uses when naming the option in diagnostics.
    fn flag(self) -> &'static str {
        match self {
            Mode::Type => "-t",
            Mode::Size => "-s",
            Mode::Print => "-p",
            Mode::Exists => "-e",
            Mode::Textconv => "--textconv",
            Mode::Filters => "--filters",
            Mode::AllObjects => "--batch-all-objects",
        }
    }

    /// git's `opt_cw`: the two transform modes are the only ones a batch stream
    /// tolerates, because they configure it rather than replace it.
    fn is_transform(self) -> bool {
        matches!(self, Mode::Textconv | Mode::Filters)
    }
}

/// The three batch dispatch modes (`--batch`, `--batch-check`, `--batch-command`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum BatchKind {
    /// `--batch`: emit the info line, then the object contents.
    Contents,
    /// `--batch-check`: emit the info line only.
    Check,
    /// `--batch-command`: read `info`/`contents`/`flush` commands from stdin.
    Command,
}

/// `git cat-file` — inspect objects in the database.
///
/// Implemented modes:
///   * `git cat-file -t <object>` → object type
///   * `git cat-file -s <object>` → object size in bytes
///   * `git cat-file -p <object>` → pretty-printed content
///   * `git cat-file -e <object>` → exit 0 if the object exists, 1 if it does not
///   * `git cat-file <type> <object>` → raw content, after peeling to `<type>`
///   * `git cat-file (--batch | --batch-check | --batch-command)` → batch stream
///   * `git cat-file --filters <rev>:<path>` → object with worktree filters applied
///   * `git cat-file --batch --filters` → batch stream, each blob smudged by path
///   * `git cat-file --textconv <rev>:<path>` → object rendered by the
///     `diff.<driver>.textconv` program the path's `diff` gitattribute names,
///     falling back to `-p` output when no driver applies
///   * `git cat-file --batch --textconv` → batch stream, each blob run through the
///     textconv program for the path given after the object name
///
/// `--use-mailmap`/`--mailmap` rewrites author/committer/tagger identities in
/// commit and tag output. `--batch-all-objects`, `--buffer`, `--unordered`, `-Z`
/// (NUL stdin+stdout), `-z` (NUL stdin only) and `--filter` shape the batch
/// stream. `--allow-unknown-type` is accepted as a hidden no-op (git only uses
/// it to read loose objects of an unknown type, which gix cannot decode).
///
/// `--follow-symlinks` (batch modes only) resolves each `<rev>:<path>` request by
/// walking the tree and following in-tree symlink blobs — a port of git's
/// `get_tree_entry_follow_symlinks` (tree-walk.c) driving `batch_one_object`
/// (builtin/cat-file.c): a symlink that escapes the tree (absolute target or `..`
/// past the root) prints `symlink <len>\n<path>`, a broken one `dangling
/// <len>\n<name>`, a cycle `loop <len>\n<name>`, and a non-directory prefix
/// `notdir <len>\n<name>`.
///
/// `--textconv` runs the external program named by `diff.<driver>.textconv` for
/// the driver the path's `diff` gitattribute selects, over a temporary copy of
/// the blob in its checked-out form — a port of `userdiff_find_by_path()` plus
/// `prep_temp_blob()`/`run_textconv()`. `diff.<driver>.cachetextconv` is not read:
/// it only decides whether the result is memoised in a notes tree, and no output
/// depends on it.
///
/// Not ported: the `%(objectsize:disk)` / `%(deltabase)` format atoms (require
/// pack-entry internals gix's header lookup does not expose), and `--filter` specs
/// beyond `blob:none` / `blob:limit=<n>` / `object:type=<t>`.
pub fn cat_file(args: &[String]) -> Result<ExitCode> {
    let mut mode: Option<Mode> = None;
    let mut batch: Option<BatchKind> = None;
    let mut batch_dup = false;
    let mut batch_format: Option<String> = None;
    let mut buffer = false;
    let mut unordered = false;
    let mut nul_in = false;
    let mut nul_out = false;
    let mut nul_flag: Option<&'static str> = None;
    let mut path: Option<String> = None;
    let mut filter: Option<String> = None;
    let mut use_mailmap = false;
    let mut follow_symlinks = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut end_of_options = false;

    // Record a batch mode, flagging a second one so the "only one batch option"
    // diagnostic fires exactly as git's does.
    macro_rules! set_batch {
        ($kind:expr, $fmt:expr) => {{
            if batch.is_some() {
                batch_dup = true;
            } else {
                batch = Some($kind);
                batch_format = $fmt;
            }
        }};
    }

    // git's cmdmode slot. `parse_options` rejects the first conflicting pair it
    // meets and names the newcomer before the option already in effect; repeating
    // the same mode is not a conflict.
    macro_rules! set_mode {
        ($next:expr) => {{
            let next = $next;
            if let Some(prev) = mode {
                if prev != next {
                    eprintln!(
                        "error: options '{}' and '{}' cannot be used together",
                        next.flag(),
                        prev.flag()
                    );
                    return Ok(ExitCode::from(129));
                }
            }
            mode = Some(next);
        }};
    }

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let arg = arg.as_str();

        if end_of_options {
            positional.push(arg);
            continue;
        }
        if arg == "--" {
            end_of_options = true;
            continue;
        }

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): tested on the token as typed, after the `--`
        // break above and ahead of the abbreviation resolver, because it is a
        // `strcmp` — `--help-a` and `--help-all=x` stay unknown options. It
        // renders `USAGE_FULL`, which keeps the hidden `--allow-unknown-type`
        // and `-z`; the `--mailmap` line is still the in-`parse_options()`
        // spelling, because this block is rendered from inside it.
        if arg == "--help-all" {
            print!("{}", usage_all(ALIAS_HELP));
            return Ok(ExitCode::from(129));
        }

        let raw = arg;
        let resolved = match super::canonical_long_aliased(arg, LONG_OPTS, ALIAS_GROUPS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(
                    arg,
                    &first,
                    &second,
                    &usage(ALIAS_HELP),
                ))
            }
        };
        let arg = resolved.as_ref();

        if let Some(long) = arg.strip_prefix("--") {
            // Split `--opt=value` so the value never reaches the name match.
            let (name, attached) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match name {
                "batch" => set_batch!(BatchKind::Contents, attached),
                "batch-check" => set_batch!(BatchKind::Check, attached),
                "batch-command" => set_batch!(BatchKind::Command, attached),
                "batch-all-objects" => set_mode!(Mode::AllObjects),
                // Hidden compat boolean. git accepts it (and its `--no-` form)
                // as a no-op for every object gix can represent; it only alters
                // reading of loose objects whose type header is not one of the
                // known types, which gix cannot decode at all — so there is no
                // additional behavior to port for the representable domain.
                "allow-unknown-type" | "no-allow-unknown-type" => {}
                "buffer" => buffer = true,
                "no-buffer" => buffer = false,
                "unordered" => unordered = true,
                "no-unordered" => unordered = false,
                "follow-symlinks" => follow_symlinks = true,
                "no-follow-symlinks" => follow_symlinks = false,
                "textconv" => set_mode!(Mode::Textconv),
                "filters" => set_mode!(Mode::Filters),
                "use-mailmap" | "mailmap" => use_mailmap = true,
                "no-use-mailmap" | "no-mailmap" => use_mailmap = false,
                "no-path" => path = None,
                "no-filter" => filter = None,
                // `--path` / `--filter` are `OPT_STRING`: value may be attached
                // with `=` or supplied as the following argument.
                "path" | "filter" => {
                    let value = match attached {
                        Some(v) => v,
                        None => match iter.peek() {
                            Some(next) => {
                                let value = next.to_string();
                                iter.next();
                                value
                            }
                            // `get_arg()` returns `PARSE_OPT_ERROR`, which
                            // `parse_options()` answers with a bare `exit(129)`
                            // — no usage block, unlike the unknown-option arm.
                            None => {
                                eprintln!("error: option `{name}' requires a value");
                                return Ok(ExitCode::from(129));
                            }
                        },
                    };
                    if name == "path" {
                        path = Some(value);
                    } else {
                        filter = Some(value);
                    }
                }
                _ => {
                    eprintln!("error: unknown option `{long}'");
                    parse_usage_err();
                    return Ok(ExitCode::from(129));
                }
            }
            continue;
        }

        // A lone `-` is a positional; anything else starting with `-` is a
        // (possibly bundled) run of short options, exactly as parse-options
        // treats it.
        if arg.len() > 1 {
            if let Some(shorts) = arg.strip_prefix('-') {
                for c in shorts.chars() {
                    let next = match c {
                        't' => Some(Mode::Type),
                        's' => Some(Mode::Size),
                        'p' => Some(Mode::Print),
                        'e' => Some(Mode::Exists),
                        'Z' => {
                            // `-Z`: NUL-terminate both stdin records and stdout.
                            nul_in = true;
                            nul_out = true;
                            nul_flag = Some("-Z");
                            None
                        }
                        'z' => {
                            // `-z`: deprecated form — NUL-terminate stdin only;
                            // stdout records stay newline-delimited.
                            nul_in = true;
                            nul_flag = Some("-z");
                            None
                        }
                        'h' => {
                            print!("{}", usage(ALIAS_HELP));
                            return Ok(ExitCode::from(129));
                        }
                        _ => {
                            eprintln!("error: unknown switch `{c}'");
                            parse_usage_err();
                            return Ok(ExitCode::from(129));
                        }
                    };
                    if let Some(next) = next {
                        set_mode!(next);
                    }
                }
                continue;
            }
        }

        // A non-option argument is handed back unchanged by the resolver, so
        // this keeps `positional` borrowed from `args`.
        positional.push(raw);
    }

    // ---- cross-option validation, in git's order ---------------------------

    if batch_dup {
        eprintln!("error: only one batch option may be specified");
        return Ok(ExitCode::from(129));
    }

    // Split the cmdmode slot into the three roles it plays. `--textconv` and
    // `--filters` become the batch stream's transform (git's `transform_mode`)
    // rather than a mode of their own; `--batch-all-objects` is a batch modifier;
    // everything else is a standalone query.
    let textconv = mode == Some(Mode::Textconv);
    let filters = mode == Some(Mode::Filters);
    let all_objects = mode == Some(Mode::AllObjects);
    let transform = mode.filter(|m| m.is_transform());
    let mode = mode.filter(|m| !m.is_transform() && *m != Mode::AllObjects);

    if let (Some(m), Some(_)) = (mode, batch) {
        return die_usage(&format!("'{}' is incompatible with batch mode", m.flag()));
    }

    if all_objects && batch.is_none() {
        return die_usage("'--batch-all-objects' requires a batch mode");
    }

    if path.is_some() && transform.is_none() {
        return die_usage("'--path=<path|tree-ish>' needs '--filters' or '--textconv'");
    }

    // `--follow-symlinks` only shapes a batch stream (it changes how each
    // `<rev>:<path>` request is resolved). git's `usage_msg_optf` rejects it
    // outside batch mode; the check sits right after `--path`, mirroring the
    // order of git's batch-mode-compatibility guards in `cmd_cat_file`.
    if follow_symlinks && batch.is_none() {
        return die_usage("'--follow-symlinks' requires a batch mode");
    }

    if filter.is_some() && batch.is_none() {
        // git prints this bare line (no `fatal:`) and the usage exit code.
        eprintln!("usage: objects filter only supported in batch mode");
        return Ok(ExitCode::from(129));
    }

    // `-z`/`-Z` only shape a batch stream; outside batch mode git rejects them
    // with `usage_msg_optf`, naming the exact flag that was supplied. This check
    // follows the `--path`/`--filter` diagnostics, matching git's option order.
    if let Some(flag) = nul_flag {
        if batch.is_none() {
            return die_usage(&format!("'{flag}' requires a batch mode"));
        }
    }

    if batch.is_some() && !all_objects && !positional.is_empty() {
        return die_usage("batch modes take no arguments");
    }

    // Both transforms are ported inside a batch too (git's `transform_mode`):
    // `--filters` smudges each blob through the worktree pipeline, `--textconv`
    // runs its `diff.*.textconv` program, both keyed by the per-record path.

    // ---- dispatch ----------------------------------------------------------

    if let Some(kind) = batch {
        return run_batch(
            kind,
            batch_format.as_deref(),
            all_objects,
            buffer,
            unordered,
            nul_in,
            nul_out,
            filter.as_deref(),
            use_mailmap,
            transform,
            follow_symlinks,
        );
    }

    if filters {
        return run_filters(&positional, path.as_deref());
    }
    if textconv {
        return run_textconv(&positional, path.as_deref(), use_mailmap);
    }

    let repo = crate::setup::discover()?;

    // Mode-less `<type> <object>` form and the arity rules for the cmdmode form.
    let Some(mode) = mode else {
        return match positional.len() {
            0 => {
                usage_err();
                Ok(ExitCode::from(129))
            }
            2 => type_mode(&repo, positional[0], positional[1], use_mailmap),
            n => die_usage(&format!(
                "only two arguments allowed in <type> <object> mode, not {n}"
            )),
        };
    };

    if positional.is_empty() {
        return die_usage(&format!("<object> required with '{}'", mode.flag()));
    }
    if positional.len() > 1 {
        return die_usage("too many arguments");
    }
    let spec = positional[0];

    let Some(oid) = crate::objname::resolve(&repo, spec) else {
        eprintln!("fatal: Not a valid object name {spec}");
        return Ok(ExitCode::from(128));
    };

    match mode {
        // `-e` is silent on both paths: 0 when present, 1 when absent.
        Mode::Exists => {
            if repo.has_object(oid) {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(1))
            }
        }
        Mode::Type | Mode::Size => {
            let Ok(header) = repo.find_header(oid) else {
                eprintln!("fatal: git cat-file: could not get object info");
                return Ok(ExitCode::from(128));
            };
            match mode {
                Mode::Type => println!("{}", header.kind()),
                _ => println!("{}", header.size()),
            }
            Ok(ExitCode::SUCCESS)
        }
        Mode::Print => {
            let Ok(object) = repo.find_object(oid) else {
                eprintln!("fatal: Not a valid object name {spec}");
                return Ok(ExitCode::from(128));
            };
            print_object(&repo, oid, &object.data, object.kind, use_mailmap)
        }
        // Handled before the repository is opened.
        Mode::Textconv | Mode::Filters | Mode::AllObjects => unreachable!(
            "the transform and batch-modifier cmdmodes are dispatched earlier"
        ),
    }
}

/// git's `case 'p'` of `cat_one_file()`: a tree is rendered the way `git ls-tree`
/// renders it, and every other object is emitted raw — with mailmap rewriting of
/// commit/tag identities when `--use-mailmap` was given. `--textconv` falls
/// through to exactly this when no textconv driver applies to the path.
fn print_object(
    repo: &gix::Repository,
    oid: gix::hash::ObjectId,
    data: &[u8],
    kind: Kind,
    use_mailmap: bool,
) -> Result<ExitCode> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if kind == Kind::Tree {
        write_tree_listing(&mut out, data, oid.kind())?;
    } else if use_mailmap && matches!(kind, Kind::Commit | Kind::Tag) {
        let mm = repo.open_mailmap();
        out.write_all(&apply_mailmap(data, &mm))?;
    } else {
        // blob / commit / tag: raw content, no added newline.
        out.write_all(data)?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The `git cat-file <type> <object>` form: resolve the object, peel it to the
/// requested type, and emit its raw bytes. Unlike `-p` this never pretty-prints;
/// a tree comes out in its on-disk binary encoding.
fn type_mode(
    repo: &gix::Repository,
    type_name: &str,
    spec: &str,
    use_mailmap: bool,
) -> Result<ExitCode> {
    // git resolves the object before it validates the type name.
    let Some(oid) = crate::objname::resolve(repo, spec) else {
        eprintln!("fatal: Not a valid object name {spec}");
        return Ok(ExitCode::from(128));
    };

    let Ok(want) = Kind::from_bytes(type_name.as_bytes()) else {
        eprintln!("fatal: invalid object type \"{type_name}\"");
        return Ok(ExitCode::from(128));
    };

    // `read_object_with_reference()` returning NULL — which covers an id that
    // resolved but is not in the odb — is git's "bad file", the same message the
    // failed peel below reports.
    let Ok(object) = repo.find_object(oid) else {
        eprintln!("fatal: git cat-file {spec}: bad file");
        return Ok(ExitCode::from(128));
    };

    // Mirrors git's `read_object_with_reference`: follow tags to their target
    // and commits to their tree until `want` is reached, else "bad file".
    let Ok(peeled) = object.peel_to_kind(want) else {
        eprintln!("fatal: git cat-file {spec}: bad file");
        return Ok(ExitCode::from(128));
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if use_mailmap && matches!(want, Kind::Commit | Kind::Tag) {
        let mm = repo.open_mailmap();
        out.write_all(&apply_mailmap(&peeled.data, &mm))?;
    } else {
        out.write_all(&peeled.data)?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// `git cat-file --filters (<rev>:<path> | --path=<path> <rev>)`: emit the blob
/// after applying the worktree smudge pipeline (eol, working-tree-encoding,
/// ident, and configured `filter.*.smudge` drivers) for `<path>`.
fn run_filters(positional: &[&str], path: Option<&str>) -> Result<ExitCode> {
    if positional.is_empty() {
        return die_usage("<rev> required with '--filters'");
    }
    if positional.len() > 1 {
        return die_usage("too many arguments");
    }
    let spec = positional[0];

    // git resolves the object first, then insists on a path for it.
    let repo = crate::setup::discover()?;
    let id = match resolve_transform_spec(&repo, spec) {
        Ok(id) => id,
        Err(code) => return Ok(code),
    };
    let rela = match required_path(spec, path) {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };

    let Ok(object) = repo.find_object(id) else {
        eprintln!("fatal: Not a valid object name {spec}");
        return Ok(ExitCode::from(128));
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    // git's `filter_object()` only runs the pipeline over a blob; anything else
    // (a tree from `<rev>:`, a commit reached through `--path`) is written raw.
    if object.kind == Kind::Blob {
        let blob = object.data.clone();
        let (mut pipeline, _index) = repo.filter_pipeline(None)?;
        let mut converted = pipeline.convert_to_worktree(
            &blob,
            rela.as_bytes().as_bstr(),
            gix::filter::plumbing::driver::apply::Delay::Forbid,
        )?;
        std::io::copy(&mut converted, &mut out)?;
        drop(converted);
    } else {
        out.write_all(&object.data)?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The `<path>` both transforms need, from `--path=<p>` or from the `<rev>:<path>`
/// spec. git asks `get_oid_with_context()` for it with `GET_OID_REQUIRE_PATH`, so
/// a spec with no `:` at all dies with exit 128 and no usage block. A `<rev>:` with
/// an empty tail does carry a path — the root tree's — and is accepted.
fn required_path(spec: &str, path: Option<&str>) -> std::result::Result<String, ExitCode> {
    match path {
        Some(p) => Ok(p.to_string()),
        None => match spec.split_once(':') {
            Some((_, p)) => Ok(p.to_string()),
            None => {
                eprintln!("fatal: <object>:<path> required, only <object> '{spec}' given");
                Err(ExitCode::from(128))
            }
        },
    }
}

/// Resolve a transform mode's `<rev>[:<path>]` argument, reproducing the
/// diagnostics `get_oid_with_context_1()` and `diagnose_invalid_oid_path()`
/// (object-name.c) print. The blanket "Not a valid object name" only covers a
/// bare `<rev>` that does not resolve; once a `:` is present git reports either
/// the bad revision or the missing path, and says whether the path is at least
/// present in the working tree.
fn resolve_transform_spec(
    repo: &gix::Repository,
    spec: &str,
) -> std::result::Result<gix::hash::ObjectId, ExitCode> {
    if let Some(id) = crate::objname::resolve(repo, spec) {
        return Ok(id);
    }
    let Some((rev, file)) = spec.split_once(':') else {
        eprintln!("fatal: Not a valid object name {spec}");
        return Err(ExitCode::from(128));
    };
    // `:<path>` is the index form, whose miss `diagnose_invalid_index_path()`
    // words differently because two sources were consulted.
    if rev.is_empty() {
        eprintln!("fatal: path '{file}' does not exist (neither on disk nor in the index)");
        return Err(ExitCode::from(128));
    }
    if crate::objname::resolve(repo, rev).is_none() {
        eprintln!("fatal: invalid object name '{rev}'.");
        return Err(ExitCode::from(128));
    }
    if std::path::Path::new(file).exists() {
        eprintln!("fatal: path '{file}' exists on disk, but not in '{rev}'");
    } else {
        eprintln!("fatal: path '{file}' does not exist in '{rev}'");
    }
    Err(ExitCode::from(128))
}

// ---- textconv --------------------------------------------------------------

/// git's textconv machinery — `userdiff_find_by_path()` (userdiff.c) plus
/// `fill_textconv()`/`run_textconv()` (diff.c): map a path to the driver named by
/// its `diff` gitattribute, look up that driver's `diff.<name>.textconv` program,
/// and run it over the blob's checked-out content.
///
/// The two halves are also useful on their own, and `diff-pairs` drives them
/// separately for `--ext-diff`: [`Textconv::driver_name`] answers the attribute
/// lookup and [`Textconv::prep_temp_blob`] materialises a side for the external
/// diff program.
pub(crate) struct Textconv<'repo> {
    repo: &'repo gix::Repository,
    /// The gitattributes stack in git's default check-in direction (worktree
    /// `.gitattributes` first, index as the fallback), which is what
    /// `userdiff_find_by_path()` queries.
    stack: gix::AttributeStack<'repo>,
    outcome: gix::attrs::search::Outcome,
    /// `prep_temp_blob()` writes the *worktree* form of the blob, so the program
    /// sees what a checkout would have produced.
    pipeline: gix::filter::Pipeline<'repo>,
    _index: gix::worktree::IndexPersistedOrInMemory,
}

/// Whether the blob was converted, and how it failed if it was not.
pub(crate) enum Converted {
    /// The driver's stdout.
    Text(Vec<u8>),
    /// No `diff` attribute, no such driver, or the driver has no `textconv`:
    /// `textconv_object()` returns 0 and git falls through to plain output.
    NoDriver,
    /// `run_textconv()` returned NULL, which `fill_textconv()` turns into
    /// `die(_("unable to read files to diff"))`.
    Failed,
}

impl<'repo> Textconv<'repo> {
    pub(crate) fn new(repo: &'repo gix::Repository) -> Result<Self> {
        let (pipeline, index) = repo.filter_pipeline(None)?;
        // The stack copies out the id-mappings it needs, so the index it was
        // built from does not have to outlive it.
        let worktree_index = repo.index_or_empty()?;
        let stack = repo.attributes_only(
            &worktree_index,
            gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
        )?;
        Ok(Self {
            repo,
            stack,
            outcome: gix::attrs::search::Outcome::default(),
            pipeline,
            _index: index,
        })
    }

    /// `userdiff_find_by_path()`: the name of the driver the path's `diff`
    /// gitattribute selects. The boolean forms (`diff` / `-diff`) pick git's
    /// built-in true/false drivers, which carry no user configuration at all, so
    /// they answer `None` here just as an unset attribute does.
    pub(crate) fn driver_name(&mut self, path: &BStr) -> Result<Option<String>> {
        // `<rev>:` names the root tree with an empty path; no pattern matches it,
        // so no driver applies.
        if path.is_empty() {
            return Ok(None);
        }
        // The stack only knows an attribute's name once a file declaring it has
        // been parsed, so descend first, then size the outcome, then match.
        let mode = Some(gix::index::entry::Mode::FILE);
        let _ = self.stack.at_entry(path, mode)?;
        self.outcome
            .initialize_with_selection(self.stack.attributes_collection(), ["diff"]);
        let platform = self.stack.at_entry(path, mode)?;
        platform.matching_attributes(&mut self.outcome);

        let Some(m) = self.outcome.iter_selected().next() else {
            return Ok(None);
        };
        let gix::attrs::StateRef::Value(value) = m.assignment.state else {
            return Ok(None);
        };
        Ok(Some(value.as_bstr().to_str_lossy().into_owned()))
    }

    /// `userdiff_find_by_path()` + `userdiff_get_textconv()`: the `diff` attribute
    /// must carry a driver *name*, and that driver must configure
    /// `diff.<name>.textconv`.
    fn program(&mut self, path: &BStr) -> Result<Option<String>> {
        let Some(name) = self.driver_name(path)? else {
            return Ok(None);
        };
        Ok(diff_driver_config(self.repo, &name, "textconv"))
    }

    /// `textconv_object()`: run the path's textconv program over `blob`, or report
    /// that no driver applies.
    pub(crate) fn convert(&mut self, path: &BStr, blob: &[u8]) -> Result<Converted> {
        let Some(program) = self.program(path)? else {
            return Ok(Converted::NoDriver);
        };
        Ok(match self.run(&program, path, blob)? {
            Some(text) => Converted::Text(text),
            None => Converted::Failed,
        })
    }

    /// `prep_temp_blob()` + `run_textconv()`: materialise the blob in a private
    /// temporary directory under its own basename — the name the program is handed
    /// — and capture the program's stdout. `None` when the program could not be
    /// started or exited non-zero, which is git's NULL return.
    fn run(&mut self, program: &str, path: &BStr, blob: &[u8]) -> Result<Option<Vec<u8>>> {
        let dir = temp_blob_dir()?;
        let file = self.prep_temp_blob(&dir, path, blob)?;

        let output =
            crate::external::prepare_shell_cmd_str(program, [&file]).output();
        let _ = std::fs::remove_dir_all(&dir);

        match output {
            Ok(o) if o.status.success() => Ok(Some(o.stdout)),
            _ => Ok(None),
        }
    }

    /// `prep_temp_blob()` (diff.c): write `blob`'s *worktree* form into `dir` under
    /// `path`'s basename — the name the program is handed — so a path with
    /// smudge/eol/ident filters reaches the program checked out.
    pub(crate) fn prep_temp_blob(
        &mut self,
        dir: &std::path::Path,
        path: &BStr,
        blob: &[u8],
    ) -> Result<std::path::PathBuf> {
        let base = path
            .rsplit(|&b| b == b'/')
            .find(|c| !c.is_empty())
            .unwrap_or(b"blob");
        let file = dir.join(std::ffi::OsString::from(
            String::from_utf8_lossy(base).into_owned(),
        ));
        let mut converted = self.pipeline.convert_to_worktree(
            blob,
            path,
            gix::filter::plumbing::driver::apply::Delay::Forbid,
        )?;
        let mut handle = std::fs::File::create(&file)?;
        std::io::copy(&mut converted, &mut handle)?;
        drop(converted);
        drop(handle);
        Ok(file)
    }
}

/// `diff.<name>.<key>` from the merged configuration, last definition winning.
/// Subsection names are compared byte for byte, as git compares them.
pub(crate) fn diff_driver_config(
    repo: &gix::Repository,
    name: &str,
    key: &str,
) -> Option<String> {
    let snapshot = repo.config_snapshot();
    let mut winner: Option<String> = None;
    for section in snapshot.sections() {
        let header = section.header();
        if !header.name().to_string().eq_ignore_ascii_case("diff") {
            continue;
        }
        if header.subsection_name() != Some(BStr::new(name.as_bytes())) {
            continue;
        }
        if let Some(v) = section.body().value(key) {
            winner = Some(v.to_str_lossy().into_owned());
        }
    }
    winner
}

/// `mks_tempfile_ts()`'s directory: a fresh `git-blob-XXXXXX` under `TMPDIR`, so
/// the blob can keep its own basename inside it.
pub(crate) fn temp_blob_dir() -> Result<std::path::PathBuf> {
    let base = std::env::temp_dir();
    for attempt in 0..64u32 {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = base.join(format!(
            "git-blob-{:06x}",
            (std::process::id() ^ stamp ^ attempt) & 0xff_ffff
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    crate::git_fatal!("could not create a temporary directory in {}", base.display())
}

/// `git cat-file --textconv (<rev>:<path> | --path=<path> <rev>)`: emit the object
/// as its path's `diff.<driver>.textconv` program renders it. git's `case 'c'`
/// falls through to `case 'p'` whenever no driver applies, so an unconfigured path
/// prints the object exactly as `-p` would.
fn run_textconv(positional: &[&str], path: Option<&str>, use_mailmap: bool) -> Result<ExitCode> {
    if positional.is_empty() {
        return die_usage("<rev> required with '--textconv'");
    }
    if positional.len() > 1 {
        return die_usage("too many arguments");
    }
    let spec = positional[0];

    let repo = crate::setup::discover()?;
    let oid = match resolve_transform_spec(&repo, spec) {
        Ok(id) => id,
        Err(code) => return Ok(code),
    };
    let rela = match required_path(spec, path) {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    let Ok(object) = repo.find_object(oid) else {
        eprintln!("fatal: Not a valid object name {spec}");
        return Ok(ExitCode::from(128));
    };

    let mut textconv = Textconv::new(&repo)?;
    match textconv.convert(rela.as_bytes().as_bstr(), &object.data)? {
        Converted::Text(text) => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            out.write_all(&text)?;
            out.flush()?;
            Ok(ExitCode::SUCCESS)
        }
        Converted::Failed => {
            eprintln!("fatal: unable to read files to diff");
            Ok(ExitCode::from(128))
        }
        Converted::NoDriver => print_object(&repo, oid, &object.data, object.kind, use_mailmap),
    }
}

// ---- batch stream ----------------------------------------------------------

/// git's `batch_options.transform_mode`: with `'w'` (`--filters`) every blob is
/// smudged through the worktree pipeline, with `'c'` (`--textconv`) it is run
/// through its path's `diff.<driver>.textconv` program. Both consume the record's
/// trailing path, which is why either one turns on whitespace splitting.
enum Transform<'repo> {
    Filters(gix::filter::Pipeline<'repo>),
    Textconv(Textconv<'repo>),
}

/// One piece of a compiled `--batch`/`--batch-check` format string.
enum Token {
    Literal(Vec<u8>),
    ObjectName,
    ObjectType,
    ObjectSize,
    ObjectSizeDisk,
    DeltaBase,
    Rest,
}

/// A compiled format plus whether it references `%(rest)` (which turns on
/// whitespace splitting of each input line).
struct Format {
    tokens: Vec<Token>,
    has_rest: bool,
    /// Whether `%(objectsize:disk)` appears, so the on-disk lookup — which costs
    /// a pack-entry decode per object — is only paid for when it is asked for.
    has_disk_size: bool,
    /// Whether `%(deltabase)` appears; same reasoning, a different lookup.
    has_delta_base: bool,
}

const DEFAULT_FORMAT: &str = "%(objectname) %(objecttype) %(objectsize)";

/// Compile a cat-file format string, matching git's `expand_format` validation
/// and its `strbuf_expand_bad_format` diagnostics. `Err` carries the exact
/// `fatal:` line git would print.
fn compile_format(fmt: &str) -> std::result::Result<Format, String> {
    let bytes = fmt.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut lit: Vec<u8> = Vec::new();
    let mut has_rest = false;
    let mut has_disk_size = false;
    let mut has_delta_base = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'%' {
            lit.push(b);
            i += 1;
            continue;
        }
        // `%%` collapses to a literal `%`; `%` before anything but `(` stays literal.
        if i + 1 >= bytes.len() || bytes[i + 1] != b'(' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'%' {
                lit.push(b'%');
                i += 2;
            } else {
                lit.push(b'%');
                i += 1;
            }
            continue;
        }
        // `%(atom)`: find the closing paren.
        let rest = &bytes[i + 1..];
        let Some(close_rel) = rest.iter().position(|&c| c == b')') else {
            // element that never ends: git echoes it starting at the `(`.
            let elem = String::from_utf8_lossy(&bytes[i + 1..]);
            return Err(format!(
                "bad cat-file format: element '{elem}' does not end in ')'"
            ));
        };
        let atom = &rest[1..close_rel];
        let token = match atom {
            b"objectname" => Token::ObjectName,
            b"objecttype" => Token::ObjectType,
            b"objectsize" => Token::ObjectSize,
            b"rest" => {
                has_rest = true;
                Token::Rest
            }
            b"objectsize:disk" => {
                has_disk_size = true;
                Token::ObjectSizeDisk
            }
            b"deltabase" => {
                has_delta_base = true;
                Token::DeltaBase
            }
            _ => {
                // git dies `bad cat-file format: %(<atom>)`.
                let a = String::from_utf8_lossy(atom);
                return Err(format!("bad cat-file format: %({a})"));
            }
        };
        if !lit.is_empty() {
            tokens.push(Token::Literal(std::mem::take(&mut lit)));
        }
        tokens.push(token);
        i += 1 + close_rel + 1;
    }
    if !lit.is_empty() {
        tokens.push(Token::Literal(lit));
    }
    Ok(Format { tokens, has_rest, has_disk_size, has_delta_base })
}

/// Render one info line into `out` (no trailing delimiter).
///
/// `disk` is only consulted when the format carries `%(objectsize:disk)`; the
/// caller passes 0 otherwise rather than paying for the lookup.
fn render_info(
    fmt: &Format,
    oid: &gix::hash::ObjectId,
    kind: Kind,
    size: u64,
    disk: u64,
    delta_base: &gix::hash::ObjectId,
    rest: &[u8],
    out: &mut Vec<u8>,
) {
    for tok in &fmt.tokens {
        match tok {
            Token::Literal(l) => out.extend_from_slice(l),
            Token::ObjectName => out.extend_from_slice(oid.to_hex().to_string().as_bytes()),
            Token::ObjectType => out.extend_from_slice(kind.to_string().as_bytes()),
            Token::ObjectSize => out.extend_from_slice(size.to_string().as_bytes()),
            Token::ObjectSizeDisk => out.extend_from_slice(disk.to_string().as_bytes()),
            Token::DeltaBase => out.extend_from_slice(delta_base.to_hex().to_string().as_bytes()),
            Token::Rest => out.extend_from_slice(rest),
        }
    }
}

/// `--unordered`'s enumeration: every object in the order the object database
/// itself yields them, de-duplicated by first sighting.
///
/// This is `batch_each_object(opt, batch_unordered_object,
/// ODB_FOR_EACH_OBJECT_PACK_ORDER, &cb)` with the `oidset seen` that
/// `batch_unordered_object()` consults, so a loose copy of a packed object is
/// reported once, from whichever source came first. `odb_for_each_object()`
/// walks **loose objects first, then packs**, and `PACK_ORDER` asks for each
/// pack in ascending pack-offset order rather than index (oid) order.
///
/// Verified against stock git 2.55.0 on a repository with both loose and packed
/// objects: its output is the loose set followed by the packed set in exactly
/// `verify-pack -v`'s offset order.
///
/// Within one loose fanout directory git uses raw `readdir` order (it sorts
/// nothing; only the `00`..`ff` directory names are visited in order), which is
/// a property of the filesystem rather than of git. `std::fs::read_dir` is the
/// same `readdir`, so the two agree on the same directory, but neither order is
/// reproducible across differently-created copies of a repository.
fn all_objects_in_odb_order(repo: &gix::Repository) -> Result<Vec<gix::hash::ObjectId>> {
    use gix::odb::store::iter::Ordering;
    let hex_len = repo.object_hash().len_in_hex();
    let mut seen: std::collections::HashSet<gix::hash::ObjectId> = std::collections::HashSet::new();
    let mut ids: Vec<gix::hash::ObjectId> = Vec::new();

    // `for_each_loose_object`: fanout `00`..`ff`, in that order, per odb.
    let mut name = String::new();
    for dir in loose_object_dirs(repo) {
        for fanout in 0u16..=0xff {
            let sub = dir.join(format!("{fanout:02x}"));
            let Ok(entries) = std::fs::read_dir(&sub) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(rest) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if rest.len() != hex_len - 2 {
                    continue;
                }
                name.clear();
                name.push_str(&format!("{fanout:02x}"));
                name.push_str(&rest);
                let Ok(oid) = gix::hash::ObjectId::from_hex(name.as_bytes()) else {
                    continue;
                };
                if seen.insert(oid) {
                    ids.push(oid);
                }
            }
        }
    }

    // `for_each_packed_object(..., FOR_EACH_OBJECT_PACK_ORDER)`. gitoxide's
    // iterator yields packs before loose, so the loose tail it appends is
    // already `seen` and drops out here.
    for id in repo
        .objects
        .iter()?
        .with_ordering(Ordering::PackAscendingOffsetThenLooseLexicographical)
    {
        let oid = id?;
        if seen.insert(oid) {
            ids.push(oid);
        }
    }
    Ok(ids)
}

/// git's `oi.disk_sizep`: how many bytes the object occupies on disk.
///
/// `packed_object_info()` reports `revidx[1].offset - obj_offset`, the entry's
/// span in the pack, which for a delta is the delta's compressed size rather
/// than the reconstructed object's; `location_by_oid` returns exactly that span
/// as `entry_size`. `loose_object_info()` reports the loose file's own length.
///
/// The order matters. `do_oid_object_info_extended()` calls `find_pack_entry()`
/// first and only falls back to `loose_object_info()` when no pack has the
/// object, so an object that exists **both** loose and packed is reported at its
/// packed size. Verified against stock git 2.55.0: a blob left loose by
/// `repack` without `-d` has a 72-byte loose file and a 63-byte pack entry, and
/// `cat-file --batch-check='%(objectsize:disk)'` prints 63.
pub(crate) fn disk_size(repo: &gix::Repository, id: gix::hash::ObjectId) -> Result<u64> {
    let mut buf = Vec::new();
    use gix::odb::pack::Find as _;
    // `location_by_oid` asserts the handle keeps packs mapped for the lifetime of
    // the returned location, so the handle has to opt out of pack unloading.
    let mut odb = repo.objects.clone();
    odb.prevent_pack_unload();
    if let Some(loc) = odb.location_by_oid(id.as_ref(), &mut buf) {
        return Ok(loc.entry_size as u64);
    }
    let hex = id.to_string();
    for dir in loose_object_dirs(repo) {
        if let Ok(meta) = std::fs::metadata(dir.join(&hex[..2]).join(&hex[2..])) {
            return Ok(meta.len());
        }
    }
    crate::git_fatal!("cannot determine on-disk size of {hex}")
}

/// git's `oi.delta_base_oid`: the object this one is stored as a delta against, or the null
/// id when it is stored whole.
///
/// ```c
/// } else if (is_atom("deltabase", atom, len)) {
///         if (data)
///                 data->info.delta_base_oid = &data->delta_base_oid;
/// ```
///
/// (`expand_atom()`, builtin/cat-file.c.) `packed_object_info()` fills it from the pack
/// entry's header: a `REF_DELTA` names its base outright, an `OFS_DELTA` names it by
/// distance backwards in the pack, and everything else — every loose object included —
/// reports the null id.
pub(crate) fn delta_base(
    repo: &gix::Repository,
    id: gix::hash::ObjectId,
) -> Result<gix::hash::ObjectId> {
    use gix::odb::pack::Find as _;
    let null = gix::ObjectId::null(repo.object_hash());
    let mut buf = Vec::new();
    let mut odb = repo.objects.clone();
    odb.prevent_pack_unload();
    let Some(location) = odb.location_by_oid(id.as_ref(), &mut buf) else {
        return Ok(null);
    };
    let Some(entry) = odb.entry_by_location(&location) else {
        return Ok(null);
    };
    let Ok(parsed) = gix::odb::pack::data::Entry::from_bytes(
        &entry.data,
        location.pack_offset,
        repo.object_hash(),
    ) else {
        return Ok(null);
    };
    use gix::odb::pack::data::entry::Header;
    match parsed.header {
        Header::RefDelta { base_id } => Ok(base_id),
        Header::OfsDelta { base_distance } => {
            let base_offset = location.pack_offset.saturating_sub(base_distance);
            let Some(pairs) = odb.pack_offsets_and_oid(location.pack_id) else {
                return Ok(null);
            };
            Ok(pairs
                .into_iter()
                .find(|(offset, _)| *offset == base_offset)
                .map(|(_, oid)| oid)
                .unwrap_or(null))
        }
        _ => Ok(null),
    }
}

/// Every `objects/` directory backing this repository — the primary one first,
/// then each alternate, in the order the odb consults them.
fn loose_object_dirs(repo: &gix::Repository) -> Vec<std::path::PathBuf> {
    use gix::odb::store::structure::Record;
    repo.objects
        .store_ref()
        .structure()
        .map(|records| {
            records
                .into_iter()
                .filter_map(|r| match r {
                    Record::LooseObjectDatabase {
                        objects_directory, ..
                    } => Some(objects_directory),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_else(|_| vec![repo.common_dir().join("objects")])
}

/// One filter-spec predicate. Only the small, unambiguous subset git shares
/// with `rev-list` is ported; anything else is rejected up front.
enum ObjFilter {
    BlobNone,
    BlobLimit(u64),
    ObjectType(Kind),
}

impl ObjFilter {
    /// `true` when the object is kept, `false` when it is filtered out.
    fn keeps(&self, kind: Kind, size: u64) -> bool {
        match self {
            ObjFilter::BlobNone => kind != Kind::Blob,
            ObjFilter::BlobLimit(limit) => !(kind == Kind::Blob && size > *limit),
            ObjFilter::ObjectType(want) => kind == *want,
        }
    }
}

/// Parse the supported `--filter` specs. `Err` carries the exact `fatal:` line.
fn parse_filter(spec: &str) -> std::result::Result<ObjFilter, String> {
    if spec == "blob:none" {
        return Ok(ObjFilter::BlobNone);
    }
    if let Some(limit) = spec.strip_prefix("blob:limit=") {
        return parse_size(limit)
            .map(ObjFilter::BlobLimit)
            .ok_or_else(|| format!("invalid filter-spec '{spec}'"));
    }
    if let Some(t) = spec.strip_prefix("object:type=") {
        return match Kind::from_bytes(t.as_bytes()) {
            Ok(k) => Ok(ObjFilter::ObjectType(k)),
            Err(_) => Err(format!("invalid filter-spec '{spec}'")),
        };
    }
    // Recognized filter families we have not ported vs. genuinely malformed:
    // both are surfaced honestly rather than silently accepted.
    if spec.starts_with("tree:")
        || spec.starts_with("sparse:")
        || spec.starts_with("combine:")
        || spec.starts_with("object:")
    {
        Err(format!("git cat-file: filter-spec '{spec}' is not yet ported"))
    } else {
        Err(format!("invalid filter-spec '{spec}'"))
    }
}

/// git's `git_parse_ulong`: decimal digits with an optional k/m/g (1024-based)
/// suffix. Returns `None` on anything else.
fn parse_size(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (digits, mult) = match bytes[bytes.len() - 1] {
        b'k' | b'K' => (&s[..s.len() - 1], 1024u64),
        b'm' | b'M' => (&s[..s.len() - 1], 1024 * 1024),
        b'g' | b'G' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    let n: u64 = digits.parse().ok()?;
    n.checked_mul(mult)
}

/// The batch driver for `--batch`, `--batch-check` and `--batch-command`.
#[allow(clippy::too_many_arguments)]
fn run_batch(
    kind: BatchKind,
    format: Option<&str>,
    all_objects: bool,
    buffer: bool,
    unordered: bool,
    input_nul: bool,
    output_nul: bool,
    filter: Option<&str>,
    use_mailmap: bool,
    transform_mode: Option<Mode>,
    follow_symlinks: bool,
) -> Result<ExitCode> {
    // `-Z` sets both; `-z` sets only the input delimiter (stdout stays newline).
    let input_delim: u8 = if input_nul { 0 } else { b'\n' };
    let output_delim: u8 = if output_nul { 0 } else { b'\n' };

    // Format compilation and validation happen before any object is touched,
    // exactly like git — a bad format fails without reading stdin.
    let fmt = match compile_format(format.unwrap_or(DEFAULT_FORMAT)) {
        Ok(f) => f,
        Err(msg) => {
            eprintln!("fatal: {msg}");
            return Ok(ExitCode::from(128));
        }
    };

    let objfilter = match filter {
        Some(spec) => match parse_filter(spec) {
            Ok(f) => Some(f),
            Err(msg) => {
                eprintln!("fatal: {msg}");
                return Ok(ExitCode::from(128));
            }
        },
        None => None,
    };

    let repo = crate::setup::discover()?;
    let mailmap = if use_mailmap {
        Some(repo.open_mailmap())
    } else {
        None
    };

    // A transform combined with a batch mode is git's `transform_mode`: every
    // blob is rewritten before it is emitted, keyed by the record's trailing
    // path. Build the state once and reuse it across every record.
    let mut xform = match transform_mode {
        Some(Mode::Filters) => Some(Transform::Filters(repo.filter_pipeline(None)?.0)),
        Some(Mode::Textconv) => Some(Transform::Textconv(Textconv::new(&repo)?)),
        _ => None,
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if all_objects {
        // Ignore stdin; enumerate the whole odb. `batch_objects()` collects into
        // an `oid_array` and walks it with `oid_array_for_each_unique()` — sorted
        // and de-duplicated — unless `--unordered`, which walks the odb directly.
        let mut ids: Vec<gix::hash::ObjectId> = if unordered {
            all_objects_in_odb_order(&repo)?
        } else {
            let mut v: Vec<gix::hash::ObjectId> = Vec::new();
            for id in repo.objects.iter()? {
                v.push(id?);
            }
            v
        };
        if !unordered {
            ids.sort();
            ids.dedup();
        }
        let want_contents = kind == BatchKind::Contents;
        for oid in ids {
            // ```c
            // ret = oid_object_info_extended(the_repository, &data->oid, &data->info, ...);
            // if (ret < 0) {
            //         strbuf_addf(scratch, "%s missing\n", ...);
            //         batch_write(opt, scratch->buf, scratch->len);
            //         return 0;
            // }
            // ```
            //
            // (`batch_object_write()`, builtin/cat-file.c.) An object the enumeration found
            // but the odb cannot read — a corrupt loose file, whose name is still a name —
            // is reported as `missing` rather than dropped from the listing.
            let Ok(header) = repo.find_header(oid) else {
                out.write_all(oid.to_hex().to_string().as_bytes())?;
                out.write_all(b" missing")?;
                out.write_all(&[output_delim])?;
                if !buffer {
                    out.flush()?;
                }
                continue;
            };
            if let Some(f) = &objfilter {
                if !f.keeps(header.kind(), header.size()) {
                    continue;
                }
            }
            match emit_object(
                &mut out,
                &repo,
                &fmt,
                oid,
                header.kind(),
                header.size(),
                b"",
                want_contents,
                output_delim,
                mailmap.as_ref(),
                xform.as_mut(),
            )? {
                EmitOutcome::Ok => {}
                EmitOutcome::Die(code) => {
                    out.flush()?;
                    return Ok(ExitCode::from(code));
                }
            }
            if !buffer {
                out.flush()?;
            }
        }
        out.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    // stdin-driven batch. `batch_objects()` clears
    // `warn_on_object_refname_ambiguity` from exactly here — after the
    // `--batch-all-objects` return above — for the reason its comment gives: the
    // names are overwhelmingly object ids already, and asking the ref store about
    // each one "just so we can warn" costs more than the object lookups do.
    let _quiet_ambiguity = crate::objname::AmbiguityWarnings::off();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut line: Vec<u8> = Vec::new();

    loop {
        line.clear();
        let n = input.read_until(input_delim, &mut line)?;
        if n == 0 {
            break;
        }
        if line.last() == Some(&input_delim) {
            line.pop();
        }

        match kind {
            BatchKind::Command => {
                match handle_command(
                    &mut out,
                    &repo,
                    &fmt,
                    &line,
                    buffer,
                    output_delim,
                    objfilter.as_ref(),
                    mailmap.as_ref(),
                    xform.as_mut(),
                    follow_symlinks,
                )? {
                    CommandResult::Ok => {}
                    CommandResult::Die(code) => {
                        out.flush()?;
                        return Ok(ExitCode::from(code));
                    }
                }
            }
            _ => {
                let want_contents = kind == BatchKind::Contents;
                match process_request(
                    &mut out,
                    &repo,
                    &fmt,
                    &line,
                    want_contents,
                    output_delim,
                    objfilter.as_ref(),
                    mailmap.as_ref(),
                    true,
                    xform.as_mut(),
                    follow_symlinks,
                )? {
                    EmitOutcome::Ok => {
                        if !buffer {
                            out.flush()?;
                        }
                    }
                    EmitOutcome::Die(code) => {
                        out.flush()?;
                        return Ok(ExitCode::from(code));
                    }
                }
            }
        }
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

enum CommandResult {
    Ok,
    /// git `die`d: flush and exit with this code.
    Die(u8),
}

/// Outcome of emitting one batch record. `Die` carries the process exit code for
/// a fatal condition that aborts the whole batch (git's `die()`), e.g. a
/// `--filters` blob request with no path.
enum EmitOutcome {
    Ok,
    Die(u8),
}

/// `--batch-command` grammar: `info <obj>`, `contents <obj>`, `flush`.
#[allow(clippy::too_many_arguments)]
fn handle_command(
    out: &mut impl Write,
    repo: &gix::Repository,
    fmt: &Format,
    line: &[u8],
    buffer: bool,
    delim: u8,
    filter: Option<&ObjFilter>,
    mailmap: Option<&gix::mailmap::Snapshot>,
    transform: Option<&mut Transform<'_>>,
    follow_symlinks: bool,
) -> Result<CommandResult> {
    // ```c
    // if (!input.len)
    //         die(_("empty command in input"));
    // if (isspace(*input.buf))
    //         die(_("whitespace before command: '%s'"), input.buf);
    //
    // for (i = 0; i < ARRAY_SIZE(commands); i++) {
    //         if (!skip_prefix(input.buf, commands[i].name, &cmd_end))
    //                 continue;
    //         cmd = &commands[i];
    //         if (cmd->takes_args) {
    //                 if (*cmd_end != ' ')
    //                         die(_("%s requires arguments"), commands[i].name);
    //                 p = cmd_end + 1;
    //         } else if (*cmd_end) {
    //                 die(_("%s takes no arguments"), commands[i].name);
    //         }
    //         break;
    // }
    // if (!cmd)
    //         die(_("unknown command: '%s'"), input.buf);
    // ```
    //
    // (`batch_objects_command()`, builtin/cat-file.c:770-795.) The match is a *prefix*
    // match against the table in its own order, so the argument requirement is what tells
    // `info` from a word that merely starts with it — splitting on the first space instead
    // read a bare `info` as a request for the empty object name and answered ` missing`.
    if line.is_empty() {
        eprintln!("fatal: empty command in input");
        return Ok(CommandResult::Die(128));
    }
    if line[0].is_ascii_whitespace() {
        eprintln!(
            "fatal: whitespace before command: '{}'",
            String::from_utf8_lossy(line)
        );
        return Ok(CommandResult::Die(128));
    }
    let mut word: &[u8] = b"";
    let mut arg: &[u8] = b"";
    for (name, takes_args) in [
        (&b"contents"[..], true),
        (&b"info"[..], true),
        (&b"flush"[..], false),
        (&b"mailmap"[..], true),
    ] {
        let Some(rest) = line.strip_prefix(name) else {
            continue;
        };
        word = name;
        if takes_args {
            if rest.first() != Some(&b' ') {
                eprintln!("fatal: {} requires arguments", String::from_utf8_lossy(name));
                return Ok(CommandResult::Die(128));
            }
            arg = &rest[1..];
        } else if !rest.is_empty() {
            eprintln!("fatal: {} takes no arguments", String::from_utf8_lossy(name));
            return Ok(CommandResult::Die(128));
        }
        break;
    }

    match word {
        b"flush" => {
            if !buffer {
                eprintln!("fatal: flush is only for --buffer mode");
                return Ok(CommandResult::Die(128));
            }
            out.flush()?;
            Ok(CommandResult::Ok)
        }
        b"contents" => {
            match process_request(out, repo, fmt, arg, true, delim, filter, mailmap, false, transform, follow_symlinks)? {
                EmitOutcome::Ok => {
                    if !buffer {
                        out.flush()?;
                    }
                    Ok(CommandResult::Ok)
                }
                EmitOutcome::Die(code) => Ok(CommandResult::Die(code)),
            }
        }
        b"info" => {
            match process_request(out, repo, fmt, arg, false, delim, filter, mailmap, false, transform, follow_symlinks)? {
                EmitOutcome::Ok => {
                    if !buffer {
                        out.flush()?;
                    }
                    Ok(CommandResult::Ok)
                }
                EmitOutcome::Die(code) => Ok(CommandResult::Die(code)),
            }
        }
        _ => {
            eprintln!(
                "fatal: unknown command: '{}'",
                String::from_utf8_lossy(line)
            );
            Ok(CommandResult::Die(128))
        }
    }
}

/// Process one object request line: resolve the name, honor `%(rest)` splitting
/// and any object filter, then emit the info line (and contents when asked).
#[allow(clippy::too_many_arguments)]
fn process_request(
    out: &mut impl Write,
    repo: &gix::Repository,
    fmt: &Format,
    line: &[u8],
    want_contents: bool,
    delim: u8,
    filter: Option<&ObjFilter>,
    mailmap: Option<&gix::mailmap::Snapshot>,
    split_rest: bool,
    transform: Option<&mut Transform<'_>>,
    follow_symlinks: bool,
) -> Result<EmitOutcome> {
    // `%(rest)` in the format splits the line at the first whitespace run: the
    // head is the object name, the tail becomes `%(rest)`. A transform mode forces
    // the same split, because the tail is then consumed as the blob's path. git
    // performs it in `batch_objects()`'s stdin loop only, so `--batch-command`
    // never splits — its whole argument is the object name (`split_rest`).
    let (name, rest): (&[u8], &[u8]) = if split_rest && (fmt.has_rest || transform.is_some()) {
        match line.iter().position(|&b| b == b' ' || b == b'\t') {
            Some(ws) => {
                let mut end = ws;
                while end < line.len() && (line[end] == b' ' || line[end] == b'\t') {
                    end += 1;
                }
                (&line[..ws], &line[end..])
            }
            None => (line, &b""[..]),
        }
    } else {
        (line, &b""[..])
    };

    // `--follow-symlinks`: git's `batch_one_object` resolves the name through
    // `get_oid_with_context(GET_OID_FOLLOW_SYMLINKS)`, following in-tree symlinks
    // during the tree walk. The resolved object (or symlink/dangling/loop/notdir
    // status line) is emitted here instead of the plain `rev_parse_single` path.
    if follow_symlinks {
        return emit_follow(
            out,
            repo,
            fmt,
            name,
            rest,
            want_contents,
            delim,
            filter,
            mailmap,
            transform,
        );
    }

    // Resolve. A non-UTF-8 or unresolvable name is reported "missing", echoing
    // the name exactly as given.
    //
    // `batch_one_object()` reaches `get_oid_with_context()` per line, so both of
    // `get_oid_basic()`'s ambiguity warnings are due. `batch_objects()`'s
    // `warn_on_object_refname_ambiguity` bracket — held around this whole loop —
    // takes the full-hex one out; the plain-name one has no such gate, and stock
    // `printf dup | git cat-file --batch-check` prints it.
    //
    // And it reaches `get_oid_with_context()`, not gitoxide's revspec parser, so
    // the rest of that call comes with it: `read_ref_at()`'s warnings, the reach
    // warning, the `die()` for a selector past the end of the log — stock
    // `printf 'HEAD@{99}\n' | git cat-file --batch-check` ends the whole batch at
    // `fatal: log for 'HEAD' only has 3 entries` rather than reporting one
    // `missing` line — and `get_oid_1()`'s narrower grammar, which has no case for
    // `HEAD^!` and reports it `missing` where gitoxide resolved it.
    let oid = std::str::from_utf8(name).ok().and_then(|s| crate::objname::resolve(repo, s));

    let Some(oid) = oid else {
        out.write_all(name)?;
        out.write_all(b" missing")?;
        out.write_all(&[delim])?;
        return Ok(EmitOutcome::Ok);
    };

    let Ok(header) = repo.find_header(oid) else {
        out.write_all(name)?;
        out.write_all(b" missing")?;
        out.write_all(&[delim])?;
        return Ok(EmitOutcome::Ok);
    };

    // On stdin, a filtered-out object reports "excluded" (keyed by its oid),
    // rather than being silently dropped as in `--batch-all-objects`.
    if let Some(f) = filter {
        if !f.keeps(header.kind(), header.size()) {
            out.write_all(oid.to_hex().to_string().as_bytes())?;
            out.write_all(b" excluded")?;
            out.write_all(&[delim])?;
            return Ok(EmitOutcome::Ok);
        }
    }

    emit_object(
        out,
        repo,
        fmt,
        oid,
        header.kind(),
        header.size(),
        rest,
        want_contents,
        delim,
        mailmap,
        transform,
    )
}

// ---- `--follow-symlinks` ---------------------------------------------------

/// The outcome of resolving one `<rev>:<path>` request with symlink following,
/// mirroring git's `get_oid_result` plus the `ctx.mode == 0` symlink-escape case
/// that `batch_one_object` splits out.
enum FollowResult {
    /// An in-tree object was reached (`FOUND`, `mode != 0`).
    Found(gix::hash::ObjectId),
    /// A symlink pointed outside the tree — an absolute target or `..` past the
    /// root (`FOUND`, `mode == 0`); the payload is the escaped path.
    Symlink(Vec<u8>),
    /// The path (or `<rev>`) does not resolve (`MISSING_OBJECT`).
    Missing,
    /// A followed symlink has no target object (`DANGLING_SYMLINK`).
    Dangling,
    /// Symlink following exceeded the 40-hop limit (`SYMLINK_LOOP`).
    Loop,
    /// A path component past a non-directory entry (`NOT_DIR`).
    NotDir,
}

/// Emit one `--follow-symlinks` batch record: resolve `name`, then either write
/// the object (info line + optional contents) or the matching status line, byte
/// for byte as git's `batch_one_object` does. The status lines use a hard `\n`
/// because git prints them with `printf(..., "\n")`, not the batch delimiter.
#[allow(clippy::too_many_arguments)]
fn emit_follow(
    out: &mut impl Write,
    repo: &gix::Repository,
    fmt: &Format,
    name: &[u8],
    rest: &[u8],
    want_contents: bool,
    delim: u8,
    filter: Option<&ObjFilter>,
    mailmap: Option<&gix::mailmap::Snapshot>,
    transform: Option<&mut Transform<'_>>,
) -> Result<EmitOutcome> {
    match resolve_follow_symlinks(repo, name) {
        FollowResult::Missing => {
            out.write_all(name)?;
            out.write_all(b" missing\n")?;
            Ok(EmitOutcome::Ok)
        }
        FollowResult::Dangling => {
            writeln!(out, "dangling {}", name.len())?;
            out.write_all(name)?;
            out.write_all(b"\n")?;
            Ok(EmitOutcome::Ok)
        }
        FollowResult::Loop => {
            writeln!(out, "loop {}", name.len())?;
            out.write_all(name)?;
            out.write_all(b"\n")?;
            Ok(EmitOutcome::Ok)
        }
        FollowResult::NotDir => {
            writeln!(out, "notdir {}", name.len())?;
            out.write_all(name)?;
            out.write_all(b"\n")?;
            Ok(EmitOutcome::Ok)
        }
        FollowResult::Symlink(path) => {
            writeln!(out, "symlink {}", path.len())?;
            out.write_all(&path)?;
            out.write_all(b"\n")?;
            Ok(EmitOutcome::Ok)
        }
        FollowResult::Found(oid) => {
            let Ok(header) = repo.find_header(oid) else {
                out.write_all(name)?;
                out.write_all(b" missing\n")?;
                return Ok(EmitOutcome::Ok);
            };
            // A filtered-out object reports "excluded" (keyed by its oid), exactly
            // as the plain-name batch path does.
            if let Some(f) = filter {
                if !f.keeps(header.kind(), header.size()) {
                    out.write_all(oid.to_hex().to_string().as_bytes())?;
                    out.write_all(b" excluded")?;
                    out.write_all(&[delim])?;
                    return Ok(EmitOutcome::Ok);
                }
            }
            emit_object(
                out,
                repo,
                fmt,
                oid,
                header.kind(),
                header.size(),
                rest,
                want_contents,
                delim,
                mailmap,
                transform,
            )
        }
    }
}

/// Port of git's `get_oid_with_context_1` symlink-following branch: split `name`
/// at the first top-level `:` into `<rev>:<path>`, resolve `<rev>` to a tree, and
/// walk `<path>` following in-tree symlinks. A name without a `:` resolves as an
/// ordinary object (following makes no difference), matching git.
fn resolve_follow_symlinks(repo: &gix::Repository, name: &[u8]) -> FollowResult {
    // Find the `:` that separates `<rev>` from `<path>`, ignoring any inside a
    // `@{...}` reflog/upstream bracket (git's `bracket_depth` scan).
    let mut colon = None;
    let mut depth = 0i32;
    for (i, &b) in name.iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' if depth > 0 => depth -= 1,
            b':' if depth == 0 => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }

    let Some(colon) = colon else {
        // No `:` — a plain object name. Symlink following is a no-op; resolve as
        // git's non-`:` path would (any failure is reported "missing").
        return match std::str::from_utf8(name)
            .ok()
            .and_then(|s| repo.rev_parse_single(s).ok())
        {
            Some(id) => FollowResult::Found(id.detach()),
            None => FollowResult::Missing,
        };
    };

    // Resolve `<rev>` to a tree (git's `GET_OID_TREEISH` sub-lookup).
    let tree_id = match std::str::from_utf8(&name[..colon])
        .ok()
        .and_then(|s| repo.rev_parse_single(s).ok())
        .and_then(|id| id.object().ok())
        .and_then(|o| o.peel_to_kind(Kind::Tree).ok())
    {
        Some(tree) => tree.id,
        None => return FollowResult::Missing,
    };

    follow_tree(repo, tree_id, name[colon + 1..].to_vec())
}

/// Port of `get_tree_entry_follow_symlinks` (tree-walk.c): resolve `path` within
/// the tree `tree_id`, following in-tree symlink blobs up to 40 hops. The `parents`
/// stack lets a symlink target's `..` ascend toward the root, exactly as git's
/// `dir_state` array does.
fn follow_tree(repo: &gix::Repository, tree_id: gix::hash::ObjectId, path: Vec<u8>) -> FollowResult {
    const MAX_LINKS: i32 = 40;
    let hash_kind = repo.object_hash();

    // Each parent holds (root oid, tree bytes); the last is the tree currently
    // being scanned. `t_loaded == false` forces a read at the loop top.
    let mut parents: Vec<(gix::hash::ObjectId, Vec<u8>)> = Vec::new();
    let mut namebuf = path;
    let mut current_tree_oid = tree_id;
    let mut t_loaded = false;
    let mut follows_remaining = MAX_LINKS;

    loop {
        if !t_loaded {
            let obj = match repo.find_object(current_tree_oid) {
                Ok(o) => o,
                Err(_) => return FollowResult::Missing,
            };
            let tree = match obj.peel_to_kind(Kind::Tree) {
                Ok(t) => t,
                Err(_) => return FollowResult::Missing,
            };
            let root = tree.id;
            let bytes = tree.data.clone();
            let empty = bytes.is_empty();
            parents.push((root, bytes));
            if namebuf.is_empty() {
                return FollowResult::Found(root);
            }
            if empty {
                return FollowResult::Missing;
            }
            t_loaded = true;
        }

        // Strip leading slashes (a symlink may point at `//a/b`).
        while namebuf.first() == Some(&b'/') {
            namebuf.remove(0);
        }

        // Split off the first path component; `remainder` is present when a `/`
        // follows it.
        let first_slash = namebuf.iter().position(|&b| b == b'/');
        let comp_end = first_slash.unwrap_or(namebuf.len());
        let component = &namebuf[..comp_end];

        if component == b".." {
            if parents.len() == 1 {
                // At the root: the `..` escapes the tree. git restores the split
                // slash and reports the whole remaining path as the symlink target.
                return FollowResult::Symlink(namebuf.clone());
            }
            parents.pop();
            // `strbuf_remove(&namebuf, 0, remainder ? 3 : 2)` — drop `../` or `..`.
            let remove = if first_slash.is_some() { 3 } else { 2 };
            namebuf.drain(..remove.min(namebuf.len()));
            // t stays loaded: scan resumes against the now-top parent tree.
            continue;
        }

        if namebuf.is_empty() {
            // Reached via a symlink to `dir/..`: the current tree is the answer.
            return FollowResult::Found(parents.last().unwrap().0);
        }

        let Some((entry_oid, kind)) = find_entry(&parents.last().unwrap().1, component, hash_kind)
        else {
            return FollowResult::Missing;
        };
        current_tree_oid = entry_oid;

        match kind {
            EntryKind::Tree => {
                if first_slash.is_none() {
                    return FollowResult::Found(current_tree_oid);
                }
                // Descend: drop `component/` and read the sub-tree next iteration.
                namebuf.drain(..first_slash.unwrap() + 1);
                t_loaded = false;
            }
            EntryKind::Link => {
                if follows_remaining == 0 {
                    return FollowResult::Loop;
                }
                follows_remaining -= 1;
                let contents = match repo.find_object(current_tree_oid) {
                    Ok(o) => o.data.clone(),
                    Err(_) => return FollowResult::Dangling,
                };
                if contents.first() == Some(&b'/') {
                    // Absolute target: escapes the tree.
                    return FollowResult::Symlink(contents);
                }
                // Replace the symlink component with its target, keeping any
                // remainder (git's `strbuf_splice`, then re-inserting the `/`).
                let len = first_slash.unwrap_or(namebuf.len());
                let link_len = contents.len();
                let mut newbuf = contents;
                newbuf.extend_from_slice(&namebuf[len..]);
                if first_slash.is_some() && link_len < newbuf.len() {
                    newbuf[link_len] = b'/';
                }
                namebuf = newbuf;
                // t stays loaded: the target is resolved against the same parent.
            }
            // Regular file (or, defensively, a gitlink): a terminal entry.
            EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Commit => {
                if first_slash.is_none() {
                    return FollowResult::Found(current_tree_oid);
                }
                return FollowResult::NotDir;
            }
        }
    }
}

/// Look up a single, slash-free path component in a tree's raw bytes, returning
/// the entry's object id and kind — git's `find_tree_entry` reduced to the
/// single-component case the symlink walk always uses.
fn find_entry(
    tree_bytes: &[u8],
    component: &[u8],
    hash_kind: gix::hash::Kind,
) -> Option<(gix::hash::ObjectId, EntryKind)> {
    for entry in TreeRefIter::from_bytes(tree_bytes, hash_kind) {
        let entry = entry.ok()?;
        let filename: &[u8] = entry.filename.as_ref();
        if filename == component {
            return Some((entry.oid.to_owned(), entry.mode.kind()));
        }
    }
    None
}

/// Emit a resolved object: the info line, then (for `--batch`/`contents`) the
/// object contents, each terminated by `delim`.
#[allow(clippy::too_many_arguments)]
fn emit_object(
    out: &mut impl Write,
    repo: &gix::Repository,
    fmt: &Format,
    oid: gix::hash::ObjectId,
    kind: Kind,
    size: u64,
    rest: &[u8],
    want_contents: bool,
    delim: u8,
    mailmap: Option<&gix::mailmap::Snapshot>,
    transform: Option<&mut Transform<'_>>,
) -> Result<EmitOutcome> {
    let mut info = Vec::new();
    let disk = if fmt.has_disk_size {
        disk_size(repo, oid)?
    } else {
        0
    };
    let delta_base = if fmt.has_delta_base {
        delta_base(repo, oid)?
    } else {
        gix::ObjectId::null(repo.object_hash())
    };
    render_info(fmt, &oid, kind, size, disk, &delta_base, rest, &mut info);
    out.write_all(&info)?;
    out.write_all(&[delim])?;

    if want_contents {
        // git's `print_object_or_die`: a transform rewrites blobs only, using
        // `rest` as the path; every other object is emitted raw either way.
        if matches!((&transform, kind), (Some(_), Kind::Blob)) {
            if rest.is_empty() {
                // git: die("missing path for '%s'", oid). The info line above was
                // already written, matching git's ordering.
                eprintln!("fatal: missing path for '{}'", oid.to_hex());
                return Ok(EmitOutcome::Die(128));
            }
            let object = repo.find_object(oid)?;
            match transform.expect("Some checked above") {
                Transform::Filters(pipeline) => {
                    let mut converted = pipeline.convert_to_worktree(
                        &object.data,
                        rest.as_bstr(),
                        gix::filter::plumbing::driver::apply::Delay::Forbid,
                    )?;
                    std::io::copy(&mut converted, out)?;
                    drop(converted);
                }
                // `textconv_object()` returning 0 leaves git with the raw blob,
                // which it then emits unchanged; only a driver that ran and failed
                // is fatal.
                Transform::Textconv(tc) => match tc.convert(rest.as_bstr(), &object.data)? {
                    Converted::Text(text) => out.write_all(&text)?,
                    Converted::NoDriver => out.write_all(&object.data)?,
                    Converted::Failed => {
                        eprintln!("fatal: unable to read files to diff");
                        return Ok(EmitOutcome::Die(128));
                    }
                },
            }
        } else {
            let object = repo.find_object(oid)?;
            // `%(objectsize)` above stays the on-disk size; mailmap only rewrites
            // the emitted bytes of commit/tag objects.
            if let (Some(mm), true) = (mailmap, matches!(kind, Kind::Commit | Kind::Tag)) {
                out.write_all(&apply_mailmap(&object.data, mm))?;
            } else {
                out.write_all(&object.data)?;
            }
        }
        out.write_all(&[delim])?;
    }
    Ok(EmitOutcome::Ok)
}

// ---- mailmap ---------------------------------------------------------------

/// Port of git's `apply_mailmap_to_header` + `rewrite_ident_line`: rewrite the
/// author/committer/tagger identities in a commit or tag object using the
/// mailmap, leaving every other byte (timestamps, message, signatures) intact.
fn apply_mailmap(buf: &[u8], mm: &gix::mailmap::Snapshot) -> Vec<u8> {
    const HEADERS: [&[u8]; 3] = [b"author ", b"committer ", b"tagger "];
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    loop {
        // End of headers: a blank line or the end of the buffer. Copy the rest.
        if i >= buf.len() || buf[i] == b'\n' {
            out.extend_from_slice(&buf[i..]);
            break;
        }
        let line_end = buf[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| i + p)
            .unwrap_or(buf.len());
        let line = &buf[i..line_end];

        let mut matched = false;
        for h in HEADERS {
            if let Some(person) = line.strip_prefix(h) {
                out.extend_from_slice(h);
                match rewrite_ident(person, mm) {
                    Some(rewritten) => out.extend_from_slice(&rewritten),
                    None => out.extend_from_slice(person),
                }
                matched = true;
                break;
            }
        }
        if !matched {
            out.extend_from_slice(line);
        }

        if line_end < buf.len() {
            out.push(b'\n');
            i = line_end + 1;
        } else {
            i = line_end;
        }
    }
    out
}

/// Rewrite a single `name <email> <time>` ident using the mailmap. Returns the
/// replacement for `person` (everything after the `author `/`committer `/
/// `tagger ` keyword), or `None` if the mailmap leaves it unchanged.
fn rewrite_ident(person: &[u8], mm: &gix::mailmap::Snapshot) -> Option<Vec<u8>> {
    // Locate `<email>` the way git's `split_ident_line` does.
    let lt = person.iter().position(|&b| b == b'<')?;
    let gt_rel = person[lt + 1..].iter().position(|&b| b == b'>')?;
    let gt = lt + 1 + gt_rel;
    let mail = &person[lt + 1..gt];

    // The name is everything before `<`, with trailing whitespace trimmed.
    let mut name_end = lt;
    while name_end > 0 && (person[name_end - 1] == b' ' || person[name_end - 1] == b'\t') {
        name_end -= 1;
    }
    let name = &person[..name_end];

    let sig = gix::actor::SignatureRef {
        name: name.as_bstr(),
        email: mail.as_bstr(),
        time: "",
    };
    let resolved = mm.resolve_cow(sig);
    let new_name = resolved.name.as_ref().to_vec();
    let new_mail = resolved.email.as_ref().to_vec();
    if new_name.as_slice() == name && new_mail.as_slice() == mail {
        return None;
    }

    // Rebuild `name <email>`, preserving the ` <time> <tz>` tail after `>`.
    let mut rebuilt = Vec::with_capacity(person.len());
    rebuilt.extend_from_slice(&new_name);
    rebuilt.extend_from_slice(b" <");
    rebuilt.extend_from_slice(&new_mail);
    rebuilt.push(b'>');
    rebuilt.extend_from_slice(&person[gt + 1..]);
    Some(rebuilt)
}

/// `ls-tree`-style listing: `<mode6> <type> <hash>\t<name>` per entry.
fn write_tree_listing(
    out: &mut impl Write,
    data: &[u8],
    hash_kind: gix::hash::Kind,
) -> Result<()> {
    for entry in TreeRefIter::from_bytes(data, hash_kind) {
        let entry = entry.map_err(|e| anyhow::anyhow!("failed to decode tree: {e}"))?;
        let typ = match entry.mode.kind() {
            EntryKind::Tree => "tree",
            EntryKind::Commit => "commit",
            _ => "blob",
        };
        write!(out, "{:06o} {} {}\t", entry.mode.value(), typ, entry.oid)?;
        let name: &[u8] = entry.filename.as_ref();
        out.write_all(name)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}
