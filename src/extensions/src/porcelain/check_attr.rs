//! `git check-attr` — report the gitattributes in effect for a set of paths.
//!
//! Every flag stock git accepts is implemented here: `-a`/`--all`, `--cached`,
//! `--stdin`, `-z`, `--source[=]<tree-ish>` and `-h`. Both output encodings
//! (`<path>: <attr>: <info>` and the `-z` NUL form), git's C-style path quoting,
//! its argument-partitioning rules around `--`, and its `129` / `128` / `255`
//! failure exits are reproduced byte-for-byte.
//!
//! Not covered: `--help` (git execs the man page — this bails instead), and, in
//! a *bare* repository without `--cached`/`--source`, git reads no in-tree
//! `.gitattributes` at all while gitoxide's stack still probes `$GIT_DIR` for
//! them; only the repository-wide `info/attributes` and `core.attributesFile`
//! agree there. Attribute ordering under `--all` follows the order in which
//! names are first declared while parsing attribute files, which is how git
//! orders them too, but the two are derived independently.

use anyhow::{bail, Result};
use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::index::entry::Mode;
use gix::worktree::stack::state::attributes::Source;

/// git's `check_attr_usage` plus its option block, verbatim.
const USAGE: &str = "\
usage: git check-attr [--source <tree-ish>] [-a | --all | <attr>...] [--] <pathname>...
   or: git check-attr --stdin [-z] [--source <tree-ish>] [-a | --all | <attr>...]

    -a, --[no-]all        report all attributes set on file
    --[no-]cached         use .gitattributes only from the index
    --[no-]stdin          read file names from stdin
    -z                    terminate input and output records by a NUL character
    --[no-]source <tree-ish>
                          which tree-ish to check attributes at

";

/// The index the attribute stack derives its in-tree `.gitattributes` mapping
/// from: the worktree index, or one materialised from `--source`'s tree.
enum AttrIndex {
    Worktree(gix::worktree::Index),
    FromTree(gix::index::File),
}

impl AttrIndex {
    fn state(&self) -> &gix::index::State {
        match self {
            AttrIndex::Worktree(i) => i,
            AttrIndex::FromTree(f) => f,
        }
    }
}

pub fn check_attr(args: &[String]) -> Result<ExitCode> {
    let mut all = false;
    let mut cached = false;
    let mut stdin_paths = false;
    let mut nul = false;
    let mut source: Option<String> = None;

    // What git's `parse_options` leaves in `argv`: the non-option arguments in
    // order, with `--` retained (PARSE_OPT_KEEP_DASHDASH). Options are only
    // recognised before `--`, but may be interleaved with non-options.
    let mut rest: Vec<&str> = Vec::new();
    let mut after_dashdash = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if after_dashdash {
            rest.push(a);
            i += 1;
            continue;
        }
        match a {
            "--" => {
                after_dashdash = true;
                rest.push(a);
            }
            "--all" => all = true,
            "--no-all" => all = false,
            "--cached" => cached = true,
            "--no-cached" => cached = false,
            "--stdin" => stdin_paths = true,
            "--no-stdin" => stdin_paths = false,
            "--no-source" => source = None,
            "--source" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprint!("error: option `source' requires a value\n{USAGE}");
                    return Ok(ExitCode::from(129));
                };
                source = Some(v.clone());
            }
            s if s.starts_with("--source=") => {
                source = Some(s["--source=".len()..].to_owned());
            }
            "--help" => bail!("--help (man page display) is not supported; use -h for usage"),
            s if s.starts_with("--") => {
                eprint!("error: unknown option `{}'\n{USAGE}", &s[2..]);
                return Ok(ExitCode::from(129));
            }
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // Grouped short flags, e.g. `-az`.
            s if s.len() > 1 && s.starts_with('-') => {
                for c in s[1..].chars() {
                    match c {
                        'a' => all = true,
                        'z' => nul = true,
                        'h' => {
                            print!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => {
                            eprint!("error: unknown switch `{c}'\n{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                    }
                }
            }
            s => rest.push(s),
        }
        i += 1;
    }

    // git's split of `argv` into `<attr>...` and `<pathname>...`, verbatim from
    // `cmd_check_attr`: `cnt` attribute names live at `argv[..cnt]`, paths start
    // at `filei`. A lone `-` before `--` never reaches here as it is a path.
    let argc = rest.len() as isize;
    let doubledash = rest
        .iter()
        .position(|a| *a == "--")
        .map_or(-1, |p| p as isize);

    let (cnt, filei);
    if all {
        if doubledash >= 1 {
            return Ok(usage_error("Attributes and --all both specified"));
        }
        cnt = 0;
        filei = doubledash + 1;
    } else if doubledash == 0 {
        return Ok(usage_error("No attribute specified"));
    } else if doubledash < 0 {
        if argc == 0 {
            return Ok(usage_error("No attribute specified"));
        }
        if stdin_paths {
            // Without `--`, `--stdin` makes every argument an attribute name.
            cnt = argc;
            filei = argc;
        } else {
            cnt = 1;
            filei = 1;
        }
    } else {
        cnt = doubledash;
        filei = doubledash + 1;
    }

    if stdin_paths {
        if filei < argc {
            return Ok(usage_error("Can't specify files with --stdin"));
        }
    } else if filei >= argc {
        return Ok(usage_error("No file specified"));
    }

    // Attribute names are validated only after the argument-count checks, as git does.
    let names: Vec<&str> = if all {
        Vec::new()
    } else {
        rest[..cnt as usize].to_vec()
    };
    for n in &names {
        if !attr_name_valid(n) {
            eprintln!("error: {n}: not a valid attribute name");
            return Ok(ExitCode::from(255));
        }
    }

    // Paths, exactly as the user wrote them — the output echoes these, not the
    // worktree-relative form they are looked up by.
    let given: Vec<BString> = if stdin_paths {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        let sep = if nul { b'\0' } else { b'\n' };
        let mut records: Vec<&[u8]> = if buf.is_empty() {
            Vec::new()
        } else {
            buf.split(|b| *b == sep).collect()
        };
        // A trailing separator terminates the last record, it does not start one.
        if buf.last() == Some(&sep) {
            records.pop();
        }
        let mut paths = Vec::with_capacity(records.len());
        for r in records {
            // git only unquotes in the line-oriented mode; `-z` records are literal.
            if !nul && r.first() == Some(&b'"') {
                let Some(unquoted) = unquote_c_style(r) else {
                    eprintln!("fatal: line is badly quoted");
                    return Ok(ExitCode::from(128));
                };
                paths.push(unquoted);
            } else {
                paths.push(BString::from(r));
            }
        }
        paths
    } else {
        rest[filei as usize..]
            .iter()
            .map(|s| BString::from(s.as_bytes()))
            .collect()
    };

    let repo = gix::discover(".")?;

    // git compares user-supplied absolute paths against the resolved worktree
    // root, and names it in the "outside repository" message.
    let workdir = repo.workdir().map(gix::path::realpath).transpose()?;
    let prefix: BString = match repo.prefix()? {
        Some(p) if !p.as_os_str().is_empty() => {
            let mut b = BString::from(gix::path::into_bstr(p).into_owned());
            b.push(b'/');
            b
        }
        _ => BString::default(),
    };

    // git resolves `--source` to a tree-ish whenever it is given and dies if it
    // does not name one — *before* reading any attribute and regardless of
    // `--cached`, which merely overrides the read direction. So validate here
    // unconditionally, then let `--cached` win when both are set.
    let source_tree = if let Some(spec) = &source {
        let tree = repo
            .rev_parse_single(spec.as_str())
            .ok()
            .and_then(|id| id.object().ok())
            .and_then(|obj| obj.peel_to_tree().ok())
            .map(|tree| tree.id);
        let Some(tree) = tree else {
            eprintln!("fatal: {spec}: not a valid tree-ish source");
            return Ok(ExitCode::from(128));
        };
        Some(tree)
    } else {
        None
    };

    // Where `.gitattributes` blobs come from. git's default direction is
    // `GIT_ATTR_CHECKIN` (worktree first, index as fallback); `--cached` reads
    // the index only — and takes precedence over `--source`; `--source` alone
    // reads the named tree only.
    let (index, attr_source) = if cached {
        (
            AttrIndex::Worktree(repo.index_or_empty()?),
            Source::IdMapping,
        )
    } else if let Some(tree) = source_tree {
        (
            AttrIndex::FromTree(repo.index_from_tree(&tree)?),
            Source::IdMapping,
        )
    } else {
        (
            AttrIndex::Worktree(repo.index_or_empty()?),
            Source::WorktreeThenIdMapping,
        )
    };

    let mut stack = repo.attributes_only(index.state(), attr_source)?;
    let mut outcome = gix::attrs::search::Outcome::default();
    let mut out: Vec<u8> = Vec::new();

    for path in &given {
        let Some(rel) = worktree_relative(workdir.as_deref(), prefix.as_bstr(), path.as_bstr())
        else {
            // git dies mid-stream, so whatever was already produced is kept.
            std::io::stdout().write_all(&out)?;
            eprintln!(
                "fatal: '{path}' is outside repository at '{}'",
                workdir.as_deref().unwrap_or(Path::new("")).display()
            );
            return Ok(ExitCode::from(128));
        };

        // A trailing slash is git's marker for "this is a directory", which is
        // what `<dir>/` patterns match against.
        let mode = if rel.ends_with(b"/") {
            Some(Mode::DIR)
        } else {
            Some(Mode::FILE)
        };

        // Descending loads the `.gitattributes` files along the way, and only
        // then does the metadata collection know every attribute name — so the
        // outcome has to be sized against it afterwards. The second descent is
        // a cache hit on the same stack.
        stack.at_entry(rel.as_bstr(), mode)?;
        if all {
            outcome.initialize(stack.attributes_collection());
        } else {
            outcome.initialize_with_selection(stack.attributes_collection(), names.iter().copied());
        }
        let platform = stack.at_entry(rel.as_bstr(), mode)?;
        platform.matching_attributes(&mut outcome);

        if all {
            // `--all` reports only attributes that actually have a state.
            for m in outcome.iter() {
                if matches!(m.assignment.state, gix::attrs::StateRef::Unspecified) {
                    continue;
                }
                emit(&mut out, path.as_bstr(), m.assignment, nul);
            }
        } else {
            // Selected attributes are reported in the order they were requested.
            for m in outcome.iter_selected() {
                emit(&mut out, path.as_bstr(), m.assignment, nul);
            }
        }
    }

    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// git's `error_with_usage()`: the message on stderr, then the full usage block.
fn usage_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE}");
    ExitCode::from(129)
}

/// git's `attr_name_valid()`: non-empty, not starting with `-`, and built only
/// from `[-A-Za-z0-9_.]`.
fn attr_name_valid(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| matches!(b, b'-' | b'.' | b'_' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z'))
}

/// Render one `<path> <attr> <info>` record, in either output encoding.
fn emit(out: &mut Vec<u8>, path: &BStr, assignment: gix::attrs::AssignmentRef<'_>, nul: bool) {
    use gix::attrs::StateRef;
    let info: &[u8] = match assignment.state {
        StateRef::Set => b"set",
        StateRef::Unset => b"unset",
        StateRef::Unspecified => b"unspecified",
        StateRef::Value(v) => v.as_bstr().as_bytes(),
    };
    let name = assignment.name.as_str().as_bytes();

    if nul {
        out.extend_from_slice(path.as_bytes());
        out.push(0);
        out.extend_from_slice(name);
        out.push(0);
        out.extend_from_slice(info);
        out.push(0);
    } else {
        // Only the path is quoted; git prints the attribute value verbatim.
        out.extend_from_slice(quote_c_style(path.as_bytes()).as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(name);
        out.extend_from_slice(b": ");
        out.extend_from_slice(info);
        out.push(b'\n');
    }
}

/// git's `prefix_path()`: turn a user-supplied path into the worktree-relative
/// slash path it is looked up by. Relative paths resolve against `prefix` (the
/// worktree-relative position of the current directory, `""` or `<dir>/`);
/// absolute ones must lie inside `workdir`. `None` means the path escapes the
/// worktree, which git reports as a fatal error.
fn worktree_relative(workdir: Option<&Path>, prefix: &BStr, file: &BStr) -> Option<BString> {
    let file = file.as_bytes();
    let trailing_slash = file.len() > 1 && file.ends_with(b"/");
    let mut comps: Vec<&[u8]> = Vec::new();

    if file.starts_with(b"/") {
        let root = gix::path::into_bstr(workdir?);
        let root = root.as_bytes();
        let root = root.strip_suffix(b"/").unwrap_or(root);
        let rest = file.strip_prefix(root)?;
        if !(rest.is_empty() || rest.starts_with(b"/")) {
            return None;
        }
        push_components(&mut comps, rest)?;
    } else {
        push_components(&mut comps, prefix.as_bytes())?;
        push_components(&mut comps, file)?;
    }

    let mut rel = BString::from(comps.join(&b'/'));
    if trailing_slash && !rel.is_empty() {
        rel.push(b'/');
    }
    Some(rel)
}

/// Append the `/`-separated components of `path` to `comps`, dropping empty and
/// `.` components and popping on `..`. `None` when `..` pops past the root.
fn push_components<'a>(comps: &mut Vec<&'a [u8]>, path: &'a [u8]) -> Option<()> {
    for c in path.split(|b| *b == b'/') {
        match c {
            b"" | b"." => {}
            b".." => {
                comps.pop()?;
            }
            c => comps.push(c),
        }
    }
    Some(())
}

/// git's `quote_c_style()` with `nodq = 0`: a path is wrapped in double quotes
/// and escaped when it contains a control byte, a quote, a backslash, or any
/// byte >= 0x80; otherwise it is emitted verbatim.
fn quote_c_style(bytes: &[u8]) -> String {
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
            b if b < 0x20 || b == 0x7f || b >= 0x80 => {
                out.push_str(&format!("\\{b:03o}"));
            }
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// git's `unquote_c_style()`: decode a `"`-delimited, backslash-escaped record
/// read from stdin. `None` when the input is not a well-formed quoted string,
/// which git reports as `fatal: line is badly quoted`.
fn unquote_c_style(input: &[u8]) -> Option<BString> {
    if input.first() != Some(&b'"') {
        return None;
    }
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    let mut i = 1;
    loop {
        let b = *input.get(i)?;
        i += 1;
        match b {
            b'"' => return Some(BString::from(out)),
            b'\\' => {
                let e = *input.get(i)?;
                i += 1;
                match e {
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b't' => out.push(b'\t'),
                    b'n' => out.push(b'\n'),
                    b'v' => out.push(0x0b),
                    b'f' => out.push(0x0c),
                    b'r' => out.push(b'\r'),
                    b'"' | b'\\' => out.push(e),
                    b'0'..=b'7' => {
                        // One to three octal digits, truncated to a byte — git
                        // always emits three, but accepts fewer.
                        let mut v = u32::from(e - b'0');
                        for _ in 0..2 {
                            match input.get(i) {
                                Some(&d @ b'0'..=b'7') => {
                                    v = (v << 3) + u32::from(d - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push(v as u8);
                    }
                    _ => return None,
                }
            }
            b => out.push(b),
        }
    }
}
