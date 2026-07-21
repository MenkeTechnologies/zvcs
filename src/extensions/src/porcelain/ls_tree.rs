use anyhow::Result;
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::object::tree::{EntryKind, EntryMode};
use gix::prelude::ObjectIdExt;

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
    format: Option<String>, // --format=<fmt>: custom per-entry template
    paths: Vec<String>, // path filters (empty = whole tree)
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
    };

    // The active `OPT_CMDMODE` value plus the spelling the user typed, which is
    // what git quotes back in the "cannot be used together" diagnostic.
    let mut cmdmode: Option<(CmdMode, String)> = None;
    let mut treeish: Option<&str> = None;
    let mut positionals: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        if !no_more_opts && a == "--" {
            no_more_opts = true;
            continue;
        }
        if !no_more_opts && a.len() > 1 && a.starts_with('-') {
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
                    "full-name" | "full-tree" | "no-full-name" | "no-full-tree" => {
                        // Output here is always root-relative already.
                    }
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
                    _ => return Ok(error_with_usage(&format!("unknown option `{name}'"))),
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

    match cmdmode.map(|(m, _)| m) {
        Some(CmdMode::NameOnly) | Some(CmdMode::NameStatus) => opts.name_only = true,
        Some(CmdMode::ObjectOnly) => opts.object_only = true,
        Some(CmdMode::Long) => opts.long = true,
        None => {}
    }

    let Some(spec) = treeish else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    };

    // Path filters: reject pathspec magic we don't honour, strip one trailing '/'.
    for p in &positionals {
        if p.starts_with(':') {
            return Ok(fatal(&format!("pathspec magic is not supported: {p}")));
        }
        opts.paths.push(p.trim_end_matches('/').to_string());
    }

    let repo = gix::discover(".")?;
    let Ok(id) = repo.rev_parse_single(spec) else {
        return Ok(fatal(&format!("Not a valid object name {spec}")));
    };
    let Ok(object) = id.object() else {
        return Ok(fatal(&format!("Not a valid object name {spec}")));
    };
    let Ok(tree) = object.peel_to_tree() else {
        return Ok(fatal("not a tree object"));
    };

    let mut out = String::new();
    walk(&repo, tree, "", &opts, &mut out)?;
    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

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
    let n: i64 = match v.parse() {
        Ok(n) => n,
        // git parses with strtol and clamps: an all-digit value too large for
        // the integer is not an error, it saturates to the hash width (the
        // render step caps `Len` at the hash length anyway).
        Err(_) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => i64::MAX,
        Err(_) => return None,
    };
    Some(match n {
        0 => Abbrev::Full,
        n if n < MINIMUM_ABBREV as i64 => Abbrev::Len(MINIMUM_ABBREV),
        n => Abbrev::Len(n as usize),
    })
}

/// Recursively render `tree` (rooted at `prefix`, e.g. `"dir/"`) into `out`.
fn walk(
    repo: &gix::Repository,
    tree: gix::Tree<'_>,
    prefix: &str,
    opts: &Opts,
    out: &mut String,
) -> Result<()> {
    // Materialise the entries so the borrow on the tree's data ends before we
    // recurse (child lookups need mutable access to a fresh buffer).
    let entries: Vec<(EntryMode, String, ObjectId)> = tree
        .decode()?
        .entries
        .iter()
        .map(|e| (e.mode, e.filename.to_string(), e.oid.to_owned()))
        .collect();

    for (mode, filename, oid) in entries {
        let name = format!("{prefix}{filename}");

        if mode.is_tree() {
            // A tree line is emitted when: -d (trees only), or we're not
            // recursing (so trees are the leaves shown), or -t while recursing.
            let emit = opts.dirs_only || !opts.recurse || opts.show_trees;
            if emit && path_selects(&name, opts) {
                write_entry(repo, out, mode, &oid, &name, opts)?;
            }
            if should_descend(&name, opts) {
                let child = repo.find_object(oid)?.peel_to_tree()?;
                walk(repo, child, &format!("{name}/"), opts, out)?;
            }
        } else if !opts.dirs_only && path_selects(&name, opts) {
            write_entry(repo, out, mode, &oid, &name, opts)?;
        }
    }
    Ok(())
}

/// Whether `name` is selected by the path filters (empty filters select all).
///
/// A filter `p` selects `name` when it names the entry exactly (`name == p`) or
/// when the entry lives inside the directory `p` (`name` starts with `p/`).
fn path_selects(name: &str, opts: &Opts) -> bool {
    opts.paths.is_empty()
        || opts
            .paths
            .iter()
            .any(|p| name == p.as_str() || name.starts_with(&format!("{p}/")))
}

/// Whether the sub-tree `name` must be descended into.
///
/// Always when `-r` is set; otherwise only when a path filter points strictly
/// below this tree (so an exact `<dir>` filter shows the tree line without
/// recursing, while `<dir>/<file>` descends to reach the file).
fn should_descend(name: &str, opts: &Opts) -> bool {
    opts.recurse
        || opts
            .paths
            .iter()
            .any(|p| p.starts_with(&format!("{name}/")))
}

/// Render one entry into `out`, honouring `--format`, `--name-only`,
/// `--object-only`, `--long` and `-z`.
fn write_entry(
    repo: &gix::Repository,
    out: &mut String,
    mode: EntryMode,
    oid: &ObjectId,
    name: &str,
    opts: &Opts,
) -> Result<()> {
    let term = if opts.nul { '\0' } else { '\n' };

    if let Some(fmt) = &opts.format {
        expand_format(repo, fmt, out, mode, oid, name, opts)?;
        out.push(term);
        return Ok(());
    }

    if opts.name_only {
        out.push_str(name);
        out.push(term);
        return Ok(());
    }
    if opts.object_only {
        out.push_str(&object_id_str(repo, oid, opts)?);
        out.push(term);
        return Ok(());
    }

    let mode_str = git_mode(mode);
    let type_str = git_type(mode);
    let oid_str = object_id_str(repo, oid, opts)?;

    if opts.long {
        let size = entry_size(repo, mode, oid)?;
        out.push_str(&format!(
            "{mode_str} {type_str} {oid_str} {size:>7}\t{name}{term}"
        ));
    } else {
        out.push_str(&format!("{mode_str} {type_str} {oid_str}\t{name}{term}"));
    }
    Ok(())
}

/// Expand one `--format` template for a single entry.
///
/// Supports the atoms stock `git ls-tree` documents — `%(objectmode)`,
/// `%(objecttype)`, `%(objectname)`, `%(objectsize)`, `%(objectsize:padded)`,
/// `%(path)` — plus `%%` and `%x<hh>` byte escapes. An unrecognised `%(...)`
/// atom is copied through verbatim.
fn expand_format(
    repo: &gix::Repository,
    fmt: &str,
    out: &mut String,
    mode: EntryMode,
    oid: &ObjectId,
    name: &str,
    opts: &Opts,
) -> Result<()> {
    let bytes: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != '%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        // `%` at end of string is literal.
        let Some(&next) = bytes.get(i + 1) else {
            out.push('%');
            break;
        };
        if next == '%' {
            out.push('%');
            i += 2;
            continue;
        }
        if next == 'x' && i + 3 < bytes.len() {
            let hex: String = bytes[i + 2..i + 4].iter().collect();
            if let Ok(b) = u8::from_str_radix(&hex, 16) {
                out.push(b as char);
                i += 4;
                continue;
            }
        }
        if next == '(' {
            if let Some(close) = bytes[i + 2..].iter().position(|&c| c == ')') {
                let atom: String = bytes[i + 2..i + 2 + close].iter().collect();
                match atom.as_str() {
                    "objectmode" => out.push_str(git_mode(mode)),
                    "objecttype" => out.push_str(git_type(mode)),
                    "objectname" => out.push_str(&object_id_str(repo, oid, opts)?),
                    "objectsize" => out.push_str(&entry_size(repo, mode, oid)?),
                    "objectsize:padded" => {
                        out.push_str(&format!("{:>7}", entry_size(repo, mode, oid)?))
                    }
                    "path" => out.push_str(name),
                    other => {
                        out.push_str("%(");
                        out.push_str(other);
                        out.push(')');
                    }
                }
                i += 2 + close + 1;
                continue;
            }
        }
        out.push('%');
        i += 1;
    }
    Ok(())
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
fn object_id_str(repo: &gix::Repository, oid: &ObjectId, opts: &Opts) -> Result<String> {
    Ok(match opts.abbrev {
        Abbrev::Full => oid.to_hex().to_string(),
        Abbrev::Len(n) => oid.to_hex_with_len(n.min(oid.kind().len_in_hex())).to_string(),
        Abbrev::Auto => oid.attach(repo).shorten_or_id().to_string(),
    })
}
