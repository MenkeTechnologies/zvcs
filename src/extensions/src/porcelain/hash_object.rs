//! `git hash-object` — compute an object id, and optionally write the object.
//!
//! Covered: `-t <type>`, `-w`, `--stdin`, `--stdin-paths`, `--path=<file>`,
//! `--no-filters`/`--filters`, `--literally`, `--` and `<file>...`, in git's own
//! processing order (stdin object, then file arguments, then stdin paths), with
//! one lowercase hex id per line on stdout, flushed as each id is produced so a
//! later failure leaves the same partial stdout git would leave.
//!
//! The option parser mirrors `parse-options`: long options may be abbreviated to
//! any unambiguous prefix, `--no-` negates them, short options cluster (`-wtblob`),
//! and every parse failure exits 129 with git's usage block. Everything else that
//! fails is fatal and exits 128.
//!
//! Content filtering follows `index_mem()`: a blob with a known path (the file's own
//! name, or `--path`) is run through the checkin conversion — the `text`/`eol`/
//! `filter`/`ident` attributes and `core.autocrlf` — so the id is the one `git add`
//! would record. `--no-filters` and `--stdin` without `--path` hash the bytes as
//! they are, and a non-blob type is never converted.
//!
//! `core.safecrlf` rides on `HASH_WRITE_OBJECT` (`get_conv_flags()`): only `-w` warns
//! about a conversion that would not survive a round trip, or refuses it outright
//! when the setting is `true`.

use anyhow::Result;
use std::io::{Read, Write as _};
use std::path::Path;
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::{Kind, ObjectRef, Write as _};

/// git's own usage block for `hash-object`, byte for byte, including the blank
/// line that terminates it. Printed after every usage error and for `-h`.
const USAGE: &str = "\
usage: git hash-object [-t <type>] [-w] [--path=<file> | --no-filters]
                       [--stdin [--literally]] [--] <file>...
   or: git hash-object [-t <type>] [-w] --stdin-paths [--no-filters]

    -t <type>             object type
    -w                    write the object into the object database
    --[no-]stdin          read the object from stdin
    --[no-]stdin-paths    read file names from stdin
    --no-filters          store file as is without filters
    --filters             opposite of --no-filters
    --[no-]literally      just hash any random garbage to create corrupt objects for debugging Git
    --[no-]path <file>    process file as it were from this path

";

/// The long options git declares, with whether each takes a value. `--no-filters`
/// is the negated form of `filters`, which is why it is not listed separately.
const LONG_OPTS: &[(&str, bool)] = &[
    ("stdin", false),
    ("stdin-paths", false),
    ("literally", false),
    ("path", true),
    ("filters", false),
];

/// Parsed command line for a single `hash-object` invocation.
struct Opts {
    /// `-t <type>`, kept unparsed: git validates it only when it hashes.
    type_name: String,
    /// `--stdin` is a counter; giving it twice is a usage error.
    stdin: u32,
    write: bool,           // -w: write the object into the database
    stdin_paths: bool,     // --stdin-paths: read file names from stdin
    no_filters: bool,      // --no-filters: hash the bytes as-is
    literally: bool,       // --literally: skip object validation
    path: Option<String>,  // --path=<file>: hash as if located here
    files: Vec<String>,    // <file>... positionals
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            type_name: "blob".into(),
            stdin: 0,
            write: false,
            stdin_paths: false,
            no_filters: false,
            literally: false,
            path: None,
            files: Vec::new(),
        }
    }
}

/// A fatal error: git reports these as `fatal: <msg>` on stderr and exits 128.
struct Fatal(String);

impl Fatal {
    fn new(msg: impl Into<String>) -> Self {
        Fatal(msg.into())
    }
}

/// `git hash-object` — compute an object id from file(s) or stdin, and with `-w`
/// store the object in the repository's database.
///
/// Output is one lowercase hex object id per line, in git's order: the `--stdin`
/// object first, then each `<file>` in command-line order, then each path read
/// from `--stdin-paths`. Non-blob types are parsed before being accepted, unless
/// `--literally` is given, so malformed input fails instead of producing a
/// corrupt object.
///
/// Never returns `Err`: usage failures exit 129 and fatal failures exit 128, both
/// reported here so the caller's generic exit-1 path is not taken.
///
/// Blobs with a path are hashed through the checkin conversion, so the id matches
/// what staging the same bytes would record — see the module docs.
pub fn hash_object(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "hash-object" => &args[1..],
        _ => args,
    };

    let opts = match parse(args) {
        Ok(opts) => opts,
        Err(code) => return Ok(code),
    };

    match run(&opts) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(Fatal(msg)) => {
            eprintln!("fatal: {msg}");
            Ok(ExitCode::from(128))
        }
    }
}

/// Print `error: <msg>` plus the usage block and yield git's usage exit code.
fn usage_error(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Resolve a long-option name against [`LONG_OPTS`], allowing any unambiguous
/// abbreviation. Returns the matching entry, or the ambiguous candidates.
fn resolve_long(name: &str) -> std::result::Result<Option<&'static (&'static str, bool)>, Vec<&'static str>> {
    if let Some(exact) = LONG_OPTS.iter().find(|(n, _)| *n == name) {
        return Ok(Some(exact));
    }
    let candidates: Vec<&'static (&'static str, bool)> = LONG_OPTS
        .iter()
        .filter(|(n, _)| n.starts_with(name))
        .collect();
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(Some(candidates[0])),
        _ => Err(candidates.iter().map(|(n, _)| *n).collect()),
    }
}

/// Parse argv the way `parse-options` does, or produce the exit code git would.
fn parse(args: &[String]) -> std::result::Result<Opts, ExitCode> {
    let mut opts = Opts::default();
    let mut no_more_opts = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();

        // `-` on its own is a file name, as is anything after `--`.
        if no_more_opts || a == "-" || !a.starts_with('-') {
            opts.files.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            i += 1;
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`. This table has no `PARSE_OPT_HIDDEN` entry, so
        // `USAGE_FULL` renders the same block `-h` prints.
        if a == "--help-all" {
            print!("{USAGE}");
            let _ = std::io::stdout().flush();
            return Err(ExitCode::from(129));
        }

        if let Some(body) = a.strip_prefix("--") {
            // Split `--name=value` before resolving, so the `=value` part is
            // reported verbatim when the name is unknown.
            let (name, inline) = match body.find('=') {
                Some(at) => (&body[..at], Some(&body[at + 1..])),
                None => (body, None),
            };

            let (opt, negated) = match resolve_long(name) {
                Err(cands) => {
                    return Err(usage_error(format!(
                        "ambiguous option: {name} (could be --{} or --{})",
                        cands[0], cands[1]
                    )))
                }
                Ok(Some(opt)) => (opt, false),
                Ok(None) => match name.strip_prefix("no-").map(resolve_long) {
                    Some(Err(cands)) => {
                        return Err(usage_error(format!(
                            "ambiguous option: {name} (could be --{} or --{})",
                            cands[0], cands[1]
                        )))
                    }
                    Some(Ok(Some(opt))) => (opt, true),
                    _ => return Err(usage_error(format!("unknown option `{body}'"))),
                },
            };
            let (canonical, takes_value) = *opt;

            // A negated option never takes a value, and neither does a flag.
            if inline.is_some() && (negated || !takes_value) {
                return Err(usage_error(format!("option `{name}' takes no value")));
            }

            let value = if takes_value && !negated {
                match inline {
                    Some(v) => Some(v.to_string()),
                    None => {
                        i += 1;
                        match args.get(i) {
                            Some(v) => Some(v.clone()),
                            None => {
                                return Err(usage_error(format!(
                                    "option `{canonical}' requires a value"
                                )))
                            }
                        }
                    }
                }
            } else {
                None
            };

            match canonical {
                // `--stdin` counts up; `--no-stdin` resets the count to zero.
                "stdin" => opts.stdin = if negated { 0 } else { opts.stdin + 1 },
                "stdin-paths" => opts.stdin_paths = !negated,
                "literally" => opts.literally = !negated,
                "filters" => opts.no_filters = negated,
                "path" => opts.path = value,
                _ => unreachable!("resolve_long only yields names from LONG_OPTS"),
            }
            i += 1;
            continue;
        }

        // A short-option cluster: `-w`, `-tblob`, `-wtblob`, `-t blob`.
        let chars = a[1..].char_indices();
        for (at, c) in chars {
            match c {
                'w' => opts.write = true,
                'h' => {
                    print!("{USAGE}");
                    let _ = std::io::stdout().flush();
                    return Err(ExitCode::from(129));
                }
                't' => {
                    let rest = &a[1 + at + c.len_utf8()..];
                    if rest.is_empty() {
                        i += 1;
                        match args.get(i) {
                            Some(v) => opts.type_name = v.clone(),
                            None => {
                                return Err(usage_error("switch `t' requires a value"));
                            }
                        }
                    } else {
                        opts.type_name = rest.to_string();
                    }
                    break;
                }
                _ => return Err(usage_error(format!("unknown switch `{c}'"))),
            }
        }
        i += 1;
    }

    // Argument conflicts git reports as usage errors, in git's own order.
    if opts.stdin_paths {
        if opts.stdin > 0 {
            return Err(usage_error("Can't use --stdin-paths with --stdin"));
        }
        if !opts.files.is_empty() {
            return Err(usage_error("Can't specify files with --stdin-paths"));
        }
        if opts.path.is_some() {
            return Err(usage_error("Can't use --stdin-paths with --path"));
        }
    } else {
        if opts.stdin > 1 {
            return Err(usage_error("Multiple --stdin arguments are not supported"));
        }
        if opts.path.is_some() && opts.no_filters {
            return Err(usage_error("Can't use --path with --no-filters"));
        }
    }

    Ok(opts)
}

/// Hash everything the options ask for, in git's order.
fn run(opts: &Opts) -> std::result::Result<(), Fatal> {
    // Nothing to hash at all: git exits 0 without output, without even looking
    // at `-t`, which is why the type is still unvalidated here.
    if opts.stdin == 0 && !opts.stdin_paths && opts.files.is_empty() {
        return Ok(());
    }

    // The repository is required only for `-w`; hashing alone works anywhere,
    // falling back to SHA-1 when there is no repository to ask.
    let repo = match crate::setup::discover() {
        Ok(repo) => Some(repo),
        Err(err) => {
            if opts.write {
                return Err(Fatal::new(err.to_string()));
            }
            None
        }
    };
    let hash_kind = repo
        .as_ref()
        .map_or(gix::hash::Kind::Sha1, gix::Repository::object_hash);
    // The checkin conversion is set up on first use and reused across every input,
    // so `--stdin-paths` with a thousand paths still reads the attributes once.
    let mut filters: Option<FilterPipeline<'_>> = None;

    // 1. The `--stdin` object. Its virtual path is `--path` if given at all.
    if opts.stdin > 0 {
        let vpath = if opts.no_filters {
            None
        } else {
            opts.path.as_deref()
        };
        let mut data = Vec::new();
        if std::io::stdin().lock().read_to_end(&mut data).is_err() {
            return Err(Fatal::new(format!("Unable to hash {}", vpath.unwrap_or("(null)"))));
        }
        emit(&data, repo.as_ref(), &mut filters, hash_kind, opts, vpath)?;
    }

    // 2. Each `<file>` positional, in command-line order.
    for file in &opts.files {
        hash_file(file, repo.as_ref(), &mut filters, hash_kind, opts)?;
    }

    // 3. Paths read from stdin, one per line. git does not skip blank lines; it
    //    tries to open them and fails, so neither is skipped here.
    if opts.stdin_paths {
        let mut buf = String::new();
        if std::io::stdin().lock().read_to_string(&mut buf).is_err() {
            return Err(Fatal::new("Unable to hash (null)"));
        }
        for line in buf.lines() {
            hash_file(line, repo.as_ref(), &mut filters, hash_kind, opts)?;
        }
    }

    Ok(())
}

/// Strip the `(os error N)` tail std appends, leaving git's bare `strerror` text.
fn errno_text(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(at) => s[..at].to_string(),
        None => s,
    }
}

/// Open `file`, then hash it, following git's order of operations exactly: the
/// open failure is reported before the object type is even looked at, and a file
/// that opens but cannot be read (a directory, say) is an `Unable to hash`.
fn hash_file<'repo>(
    file: &str,
    repo: Option<&'repo gix::Repository>,
    filters: &mut Option<FilterPipeline<'repo>>,
    hash_kind: gix::hash::Kind,
    opts: &Opts,
) -> std::result::Result<(), Fatal> {
    // With `--no-filters` git passes a NULL virtual path, which its own error
    // message renders as `(null)`.
    let vpath = if opts.no_filters {
        None
    } else {
        Some(opts.path.as_deref().unwrap_or(file))
    };

    let mut handle = std::fs::File::open(file).map_err(|e| {
        Fatal::new(format!(
            "could not open '{file}' for reading: {}",
            errno_text(&e)
        ))
    })?;

    let mut data = Vec::new();
    let read = handle.read_to_end(&mut data);

    // The type is validated after the open, but before the content is used —
    // that is why `-t bogus <missing-file>` reports the missing file instead,
    // while `-t bogus <directory>` reports the type.
    let _kind = parse_kind(&opts.type_name)?;

    if read.is_err() {
        return Err(Fatal::new(format!(
            "Unable to hash {}",
            vpath.unwrap_or("(null)")
        )));
    }

    emit(&data, repo, filters, hash_kind, opts, vpath)
}

/// Validate (unless `--literally`), hash, optionally write, and print the id.
fn emit<'repo>(
    data: &[u8],
    repo: Option<&'repo gix::Repository>,
    filters: &mut Option<FilterPipeline<'repo>>,
    hash_kind: gix::hash::Kind,
    opts: &Opts,
    vpath: Option<&str>,
) -> std::result::Result<(), Fatal> {
    let kind = parse_kind(&opts.type_name)?;

    // `index_mem()` converts only blobs, and only when it has a path to look up
    // attributes with.
    let converted = match (kind, vpath) {
        (Kind::Blob, Some(p)) => convert_for_checkin(filters, repo, p, data, opts.write)?,
        _ => None,
    };
    let data = converted.as_deref().unwrap_or(data);

    if !opts.literally && kind != Kind::Blob {
        if let Err(e) = ObjectRef::from_bytes(data, kind, hash_kind) {
            eprintln!("error: object fails check: {e}");
            return Err(Fatal::new("refusing to create malformed object"));
        }
    }

    let id: ObjectId = if opts.write {
        let Some(repo) = repo else {
            return Err(Fatal::new("not a git repository"));
        };
        repo.objects
            .write_buf(kind, data)
            .map_err(|e| Fatal::new(format!("unable to write {kind} object: {e}")))?
    } else {
        gix::objs::compute_hash(hash_kind, kind, data)
            .map_err(|e| Fatal::new(format!("unable to hash {kind} object: {e}")))?
    };

    // git writes each id as it is produced, so a later failure still leaves the
    // ids computed so far on stdout.
    println!("{}", id.to_hex());
    let _ = std::io::stdout().flush();
    Ok(())
}

/// `-t <type>` accepts exactly git's four object types.
fn parse_kind(s: &str) -> std::result::Result<Kind, Fatal> {
    Kind::from_bytes(s.as_bytes())
        .map_err(|_| Fatal::new(format!("invalid object type \"{s}\"")))
}

/// The checkin conversion, as `index_mem()` runs it: for a blob with a known path,
/// the `text`/`eol`/`filter`/`ident` attributes and `core.autocrlf` decide the bytes
/// that get hashed, so the id matches the one `git add` would record.
///
/// `get_conv_flags()` ties the `core.safecrlf` round-trip check to
/// `HASH_WRITE_OBJECT`, so only `-w` can warn about — or refuse — a lossy
/// conversion; a plain `git hash-object --path x x` converts silently.
///
/// Returns the bytes to hash. A path the attribute stack cannot place (a `--path`
/// pointing outside the worktree, say) has no attributes to apply and is hashed
/// verbatim, which is what git's own lookup ends up doing.
fn convert_for_checkin<'repo>(
    filters: &mut Option<FilterPipeline<'repo>>,
    repo: Option<&'repo gix::Repository>,
    rela_path: &str,
    data: &[u8],
    write: bool,
) -> std::result::Result<Option<Vec<u8>>, Fatal> {
    let Some(repo) = repo else { return Ok(None) };
    if filters.is_none() {
        let (mut pipeline, index) = repo
            .filter_pipeline(None)
            .map_err(|err| Fatal::new(format!("unable to setup filters: {err}")))?;
        if !write {
            pipeline.options_mut().crlf_roundtrip_check =
                gix::filter::plumbing::pipeline::CrlfRoundTripCheck::Skip;
        }
        *filters = Some(FilterPipeline { pipeline, index });
    }
    let state = filters.as_mut().expect("just set");
    let mut converted = Vec::with_capacity(data.len());
    match state
        .pipeline
        .convert_to_git(data, Path::new(rela_path), &state.index)
    {
        Ok(mut outcome) => {
            outcome
                .read_to_end(&mut converted)
                .map_err(|err| Fatal::new(format!("unable to convert '{rela_path}': {err}")))?;
            Ok(Some(converted))
        }
        // A path the attribute stack cannot walk to — `--path=../outside`, or a name
        // that leaves the worktree — has no attributes to apply. git looks them up and
        // finds none; here the lookup fails outright, and the answer is the same: hash
        // the bytes as they are.
        Err(gix::filter::pipeline::convert_to_git::Error::WorktreeCacheAtPath(_)) => Ok(None),
        // `core.safecrlf=true` turns a lossy conversion into a refusal, naming the path.
        Err(err) => Err(Fatal::new(err.to_string())),
    }
}

/// The filter pipeline plus the index it consults, built at most once per run.
struct FilterPipeline<'repo> {
    pipeline: gix::filter::Pipeline<'repo>,
    index: gix::worktree::IndexPersistedOrInMemory,
}
