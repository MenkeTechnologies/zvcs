use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

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

/// `git cat-file` — inspect a single object in the database.
///
/// Implemented modes:
///   * `git cat-file -t <object>` → object type
///   * `git cat-file -s <object>` → object size in bytes
///   * `git cat-file -p <object>` → pretty-printed content
///   * `git cat-file -e <object>` → exit 0 if the object exists, 1 if it does not
///   * `git cat-file <type> <object>` → raw content, after peeling to `<type>`
///
/// `<object>` is any revision spec gix can resolve (a full/abbreviated hash,
/// `HEAD`, `HEAD:path`, `<rev>^{tree}`, …). `-t`/`-s` read only the object
/// header, avoiding a full decode.
///
/// Batch modes (`--batch`, `--batch-check`, `--batch-command`), content filters
/// (`--textconv`, `--filters`, `--path`, `--filter`), mailmap application and
/// the batch output-shaping flags are not ported and fail with a terse message
/// rather than being accepted and ignored.
pub fn cat_file(args: &[String]) -> Result<ExitCode> {
    let mut mode: Option<Mode> = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut end_of_options = false;

    for arg in args {
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
            let name = long.split('=').next().unwrap_or(long);
            match name {
                "batch" | "batch-check" | "batch-command" | "batch-all-objects" => {
                    bail!("batch mode (--{name}) is not yet ported")
                }
                "textconv" | "filters" | "path" | "filter" => {
                    bail!("content filters (--{name}) are not yet ported")
                }
                "buffer" | "no-buffer" | "follow-symlinks" | "no-follow-symlinks" | "unordered"
                | "no-unordered" => bail!("batch output shaping (--{name}) is not yet ported"),
                "use-mailmap" | "no-use-mailmap" | "mailmap" | "no-mailmap" => {
                    bail!("mailmap application (--{name}) is not yet ported")
                }
                _ => {
                    eprintln!("error: unknown option `{long}'");
                    usage_err();
                    return Ok(ExitCode::from(129));
                }
            }
        }

        // A lone `-` is a positional; anything else starting with `-` is a
        // (possibly bundled) run of short options, exactly as parse-options
        // treats it.
        if arg.len() > 1 {
            if let Some(shorts) = arg.strip_prefix('-') {
                for c in shorts.chars() {
                    let next = match c {
                        't' => Mode::Type,
                        's' => Mode::Size,
                        'p' => Mode::Print,
                        'e' => Mode::Exists,
                        'h' => {
                            print!("{}", usage(ALIAS_HELP));
                            return Ok(ExitCode::from(129));
                        }
                        'Z' => bail!("NUL-terminated batch I/O (-Z) is not yet ported"),
                        _ => {
                            eprintln!("error: unknown switch `{c}'");
                            usage_err();
                            return Ok(ExitCode::from(129));
                        }
                    };
                    // git rejects the first conflicting pair it meets and names
                    // the newcomer before the option already in effect.
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
                continue;
            }
        }

        positional.push(arg);
    }

    // Argument-count checks, in git's order: mode-less `<type> <object>` form
    // first, then the arity rules for the cmdmode form.
    let Some(mode) = mode else {
        return match positional.len() {
            // Bare `git cat-file` prints the usage block with no `fatal:` line.
            0 => {
                usage_err();
                Ok(ExitCode::from(129))
            }
            2 => type_mode(positional[0], positional[1]),
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

    let repo = gix::discover(".")?;

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
fn type_mode(type_name: &str, spec: &str) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

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
    out.write_all(&peeled.data)?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
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
