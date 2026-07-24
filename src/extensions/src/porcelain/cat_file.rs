use anyhow::Result;
use std::io::{BufRead, Write};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind;
use gix::objs::{Kind, TreeRefIter};

/// The usage block up to and including the `--use-mailmap` line.
const USAGE_HEAD: &str = "\
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

/// `--mailmap` renders differently depending on how the block was reached:
/// `-h` prints the alias bare, the error path prints it with parse-options'
/// `...` argument marker. The two blocks are otherwise byte-identical.
const ALIAS_HELP: &str = "    --[no-]mailmap        alias of --use-mailmap\n";
const ALIAS_ERROR: &str = "    --[no-]mailmap ...    alias of --use-mailmap\n";

/// Everything after the `--mailmap` line.
const USAGE_TAIL: &str = "
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

fn usage(alias: &str) -> String {
    format!("{USAGE_HEAD}{alias}{USAGE_TAIL}")
}

/// Print the usage block the way git's `usage_with_options()` does — on stderr,
/// with the `...` alias rendering.
fn usage_err() {
    eprint!("{}", usage(ALIAS_ERROR));
}

/// git prefixes a `fatal:` line, then a blank line, then the usage block.
fn die_usage(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    eprintln!();
    usage_err();
    Ok(ExitCode::from(129))
}

/// The four mutually exclusive query modes (`OPT_CMDMODE` in git).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Type,
    Size,
    Print,
    Exists,
}

impl Mode {
    /// The spelling git uses when naming the option in diagnostics.
    fn flag(self) -> &'static str {
        match self {
            Mode::Type => "-t",
            Mode::Size => "-s",
            Mode::Print => "-p",
            Mode::Exists => "-e",
        }
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
/// Not ported: `--textconv` (needs the configured external `diff.*.textconv`
/// program; gix-filter has no textconv driver, standalone or in batch),
/// the `%(objectsize:disk)` / `%(deltabase)` format atoms (require pack-entry
/// internals gix's header lookup does not expose), and `--filter` specs beyond
/// `blob:none` / `blob:limit=<n>` / `object:type=<t>`.
pub fn cat_file(args: &[String]) -> Result<ExitCode> {
    let mut mode: Option<Mode> = None;
    let mut batch: Option<BatchKind> = None;
    let mut batch_dup = false;
    let mut batch_format: Option<String> = None;
    let mut all_objects = false;
    let mut buffer = false;
    let mut unordered = false;
    let mut nul_in = false;
    let mut nul_out = false;
    let mut nul_flag: Option<&'static str> = None;
    let mut textconv = false;
    let mut filters = false;
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
                "batch-all-objects" => all_objects = true,
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
                "textconv" => textconv = true,
                "filters" => filters = true,
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
                            None => {
                                eprintln!("error: option `{name}' requires a value");
                                usage_err();
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
                    usage_err();
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
                            usage_err();
                            return Ok(ExitCode::from(129));
                        }
                    };
                    if let Some(next) = next {
                        // git rejects the first conflicting pair it meets and
                        // names the newcomer before the option already in effect.
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
                    }
                }
                continue;
            }
        }

        positional.push(arg);
    }

    // ---- cross-option validation, in git's order ---------------------------

    if batch_dup {
        eprintln!("error: only one batch option may be specified");
        return Ok(ExitCode::from(129));
    }

    if let (Some(m), Some(_)) = (mode, batch) {
        return die_usage(&format!("'{}' is incompatible with batch mode", m.flag()));
    }

    if all_objects && batch.is_none() {
        return die_usage("'--batch-all-objects' requires a batch mode");
    }

    if path.is_some() && !(textconv || filters) {
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

    if textconv && batch.is_none() {
        // Not ported: textconv runs the configured external `diff.*.textconv`
        // program, which has no gix-filter equivalent.
        eprintln!("fatal: git cat-file: --textconv is not yet ported");
        return Ok(ExitCode::from(128));
    }

    if textconv && batch.is_some() {
        // Not ported: textconv inside a batch runs the external `diff.*.textconv`
        // program per record, which gix-filter has no driver for.
        eprintln!("fatal: git cat-file: --textconv with batch is not yet ported");
        return Ok(ExitCode::from(128));
    }
    // `--filters` inside a batch (git's transform_mode 'w') is ported: each blob is
    // smudged through the worktree pipeline using its per-line path (see run_batch).

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
            filters,
            follow_symlinks,
        );
    }

    if filters {
        return run_filters(&positional, path.as_deref());
    }

    let repo = gix::discover(".")?;

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

    let Ok(id) = repo.rev_parse_single(spec) else {
        eprintln!("fatal: Not a valid object name {spec}");
        return Ok(ExitCode::from(128));
    };
    let oid = id.detach();

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
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            if object.kind == Kind::Tree {
                write_tree_listing(&mut out, &object.data, oid.kind())?;
            } else if use_mailmap && matches!(object.kind, Kind::Commit | Kind::Tag) {
                let mm = repo.open_mailmap();
                out.write_all(&apply_mailmap(&object.data, &mm))?;
            } else {
                // blob / commit / tag: raw content, no added newline.
                out.write_all(&object.data)?;
            }
            out.flush()?;
            Ok(ExitCode::SUCCESS)
        }
    }
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
    let Ok(id) = repo.rev_parse_single(spec) else {
        eprintln!("fatal: Not a valid object name {spec}");
        return Ok(ExitCode::from(128));
    };

    let Ok(want) = Kind::from_bytes(type_name.as_bytes()) else {
        eprintln!("fatal: invalid object type \"{type_name}\"");
        return Ok(ExitCode::from(128));
    };

    let Ok(object) = repo.find_object(id.detach()) else {
        eprintln!("fatal: Not a valid object name {spec}");
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

    // git resolves the path either from `--path` or by splitting `<rev>:<path>`.
    let rela = match path {
        Some(p) => p.to_string(),
        None => match spec.split_once(':') {
            Some((_, p)) if !p.is_empty() => p.to_string(),
            _ => {
                return die_usage(&format!(
                    "<object>:<path> required, only <object> '{spec}' given"
                ));
            }
        },
    };

    let repo = gix::discover(".")?;
    let Ok(id) = repo.rev_parse_single(spec) else {
        eprintln!("fatal: Not a valid object name {spec}");
        return Ok(ExitCode::from(128));
    };
    let Ok(object) = repo.find_object(id.detach()) else {
        eprintln!("fatal: Not a valid object name {spec}");
        return Ok(ExitCode::from(128));
    };
    if object.kind != Kind::Blob {
        eprintln!("fatal: git cat-file {spec}: bad file");
        return Ok(ExitCode::from(128));
    }
    let blob = object.data.clone();

    let (mut pipeline, _index) = repo.filter_pipeline(None)?;
    let mut converted = pipeline.convert_to_worktree(
        &blob,
        rela.as_bytes().as_bstr(),
        gix::filter::plumbing::driver::apply::Delay::Forbid,
    )?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    std::io::copy(&mut converted, &mut out)?;
    drop(converted);
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

// ---- batch stream ----------------------------------------------------------

/// One piece of a compiled `--batch`/`--batch-check` format string.
enum Token {
    Literal(Vec<u8>),
    ObjectName,
    ObjectType,
    ObjectSize,
    Rest,
}

/// A compiled format plus whether it references `%(rest)` (which turns on
/// whitespace splitting of each input line).
struct Format {
    tokens: Vec<Token>,
    has_rest: bool,
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
            b"objectsize:disk" | b"deltabase" => {
                // Valid in git, but the values require pack-entry introspection
                // gix's header lookup does not surface. Reject rather than fake.
                let a = String::from_utf8_lossy(atom);
                return Err(format!("git cat-file: format atom %({a}) is not yet ported"));
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
    Ok(Format { tokens, has_rest })
}

/// Render one info line into `out` (no trailing delimiter).
fn render_info(fmt: &Format, oid: &gix::hash::ObjectId, kind: Kind, size: u64, rest: &[u8], out: &mut Vec<u8>) {
    for tok in &fmt.tokens {
        match tok {
            Token::Literal(l) => out.extend_from_slice(l),
            Token::ObjectName => out.extend_from_slice(oid.to_hex().to_string().as_bytes()),
            Token::ObjectType => out.extend_from_slice(kind.to_string().as_bytes()),
            Token::ObjectSize => out.extend_from_slice(size.to_string().as_bytes()),
            Token::Rest => out.extend_from_slice(rest),
        }
    }
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
    filters: bool,
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

    let repo = gix::discover(".")?;
    let mailmap = if use_mailmap {
        Some(repo.open_mailmap())
    } else {
        None
    };

    // When `--filters` is combined with a batch mode, git sets transform_mode 'w'
    // and smudges each blob through the worktree pipeline, keyed by the per-line
    // path. Build the pipeline once and reuse it across every record.
    let mut fpipe = if filters {
        Some(repo.filter_pipeline(None)?.0)
    } else {
        None
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if all_objects {
        // Ignore stdin; enumerate the whole odb. Default output is oid-sorted
        // and de-duplicated; `--unordered` streams in enumeration order.
        let mut ids: Vec<gix::hash::ObjectId> = Vec::new();
        for id in repo.objects.iter()? {
            ids.push(id?);
        }
        if !unordered {
            ids.sort();
            ids.dedup();
        }
        let want_contents = kind == BatchKind::Contents;
        for oid in ids {
            let Ok(header) = repo.find_header(oid) else {
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
                fpipe.as_mut(),
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

    // stdin-driven batch.
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
                    fpipe.as_mut(),
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
                    fpipe.as_mut(),
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
    filters_pipeline: Option<&mut gix::filter::Pipeline<'_>>,
    follow_symlinks: bool,
) -> Result<CommandResult> {
    // Split the command word from its argument on the first ASCII space.
    let (word, arg) = match line.iter().position(|&b| b == b' ') {
        Some(sp) => (&line[..sp], &line[sp + 1..]),
        None => (line, &b""[..]),
    };

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
            match process_request(out, repo, fmt, arg, true, delim, filter, mailmap, filters_pipeline, follow_symlinks)? {
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
            match process_request(out, repo, fmt, arg, false, delim, filter, mailmap, filters_pipeline, follow_symlinks)? {
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
    filters_pipeline: Option<&mut gix::filter::Pipeline<'_>>,
    follow_symlinks: bool,
) -> Result<EmitOutcome> {
    // `%(rest)` in the format splits the line at the first whitespace run: the
    // head is the object name, the tail becomes `%(rest)`. git also forces this
    // split whenever a transform mode is active (`--filters`), because the tail
    // is then consumed as the blob's path.
    let (name, rest): (&[u8], &[u8]) = if fmt.has_rest || filters_pipeline.is_some() {
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
            filters_pipeline,
        );
    }

    // Resolve. A non-UTF-8 or unresolvable name is reported "missing", echoing
    // the name exactly as given.
    let oid = std::str::from_utf8(name)
        .ok()
        .and_then(|s| repo.rev_parse_single(s).ok())
        .map(|id| id.detach());

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
        filters_pipeline,
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
    filters_pipeline: Option<&mut gix::filter::Pipeline<'_>>,
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
                filters_pipeline,
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
    filters_pipeline: Option<&mut gix::filter::Pipeline<'_>>,
) -> Result<EmitOutcome> {
    let mut info = Vec::new();
    render_info(fmt, &oid, kind, size, rest, &mut info);
    out.write_all(&info)?;
    out.write_all(&[delim])?;

    if want_contents {
        // git's `print_object_or_die`: under `--filters` (transform_mode 'w') a
        // blob is smudged through the worktree pipeline using `rest` as its path;
        // every other object is emitted raw.
        if matches!((&filters_pipeline, kind), (Some(_), Kind::Blob)) {
            if rest.is_empty() {
                // git: die("missing path for '%s'", oid). The info line above was
                // already written, matching git's ordering.
                eprintln!("fatal: missing path for '{}'", oid.to_hex());
                return Ok(EmitOutcome::Die(128));
            }
            let pipeline = filters_pipeline.expect("Some checked above");
            let object = repo.find_object(oid)?;
            let mut converted = pipeline.convert_to_worktree(
                &object.data,
                rest.as_bstr(),
                gix::filter::plumbing::driver::apply::Delay::Forbid,
            )?;
            std::io::copy(&mut converted, out)?;
            drop(converted);
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
