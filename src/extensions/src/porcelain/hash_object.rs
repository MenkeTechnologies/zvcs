//! `git hash-object` — compute an object id, and optionally write the object.
//!
//! Covered: `-t <type>`, `-w`, `--stdin`, `--stdin-paths`, `--path=<file>`,
//! `--no-filters`/`--filters`, `--literally`, `--` and `<file>...`, in git's own
//! processing order (stdin object, then file arguments, then stdin paths), with
//! one lowercase hex id per line on stdout.
//!
//! Not covered: content filtering. Git may run the checkin conversion (the
//! `text`/`eol`/`filter`/`ident` attributes, `core.autocrlf`) over the input
//! before hashing, which changes the resulting id. Rather than silently hashing
//! unconverted bytes, this module detects the situations in which a filter could
//! apply and fails with a precise message; `--no-filters` (implied for `--stdin`
//! without `--path`) takes the check out of the picture entirely.

use anyhow::{anyhow, bail, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::objs::{Kind, ObjectRef, Write as _};

/// Parsed command line for a single `hash-object` invocation.
struct Opts {
    kind: Kind,             // -t <type> (default blob)
    write: bool,            // -w: write the object into the database
    stdin: bool,            // --stdin: hash one object read from stdin
    stdin_paths: bool,      // --stdin-paths: read file names from stdin
    no_filters: bool,       // --no-filters: hash the bytes as-is
    literally: bool,        // --literally: skip object validation
    path: Option<PathBuf>,  // --path=<file>: hash as if located here
    files: Vec<String>,     // <file>... positionals
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
/// Argument-conflict errors exit 129 like git's usage failures; everything else
/// that fails is a fatal error (exit 128 via the caller). With no input at all
/// git succeeds silently, and so does this.
///
/// Content filters are not applied — see the module docs; invocations where one
/// could apply are rejected rather than answered with a wrong id.
pub fn hash_object(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "hash-object" => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        kind: Kind::Blob,
        write: false,
        stdin: false,
        stdin_paths: false,
        no_filters: false,
        literally: false,
        path: None,
        files: Vec::new(),
    };

    let mut no_more_opts = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if no_more_opts || a == "-" || !a.starts_with('-') {
            opts.files.push(a.to_string());
            i += 1;
            continue;
        }
        match a {
            "--" => no_more_opts = true,
            "-w" => opts.write = true,
            "--stdin" => opts.stdin = true,
            "--no-stdin" => opts.stdin = false,
            "--stdin-paths" => opts.stdin_paths = true,
            "--no-stdin-paths" => opts.stdin_paths = false,
            "--no-filters" => opts.no_filters = true,
            "--filters" => opts.no_filters = false,
            "--literally" => opts.literally = true,
            "--no-literally" => opts.literally = false,
            "-t" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow!("switch `t' requires a value"))?;
                opts.kind = parse_kind(v)?;
            }
            "--path" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow!("switch `path' requires a value"))?;
                opts.path = Some(PathBuf::from(v));
            }
            _ if a.starts_with("--path=") => {
                opts.path = Some(PathBuf::from(&a["--path=".len()..]));
            }
            _ if a.starts_with("-t") => opts.kind = parse_kind(&a[2..])?,
            _ => bail!("unsupported flag {a:?} (ported: -t, -w, --stdin, --stdin-paths, --path, --no-filters, --filters, --literally)"),
        }
        i += 1;
    }

    // Argument conflicts git reports as usage errors (exit 129, nothing on stdout).
    if let Some(msg) = conflict(&opts) {
        eprintln!("error: {msg}");
        return Ok(ExitCode::from(129));
    }

    // Nothing to hash at all: git exits 0 without output.
    if !opts.stdin && !opts.stdin_paths && opts.files.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    // The repository is required only for `-w`; hashing alone works anywhere,
    // falling back to SHA-1 when there is no repository to ask.
    let repo = match gix::discover(".") {
        Ok(repo) => Some(repo),
        Err(err) => {
            if opts.write {
                return Err(err.into());
            }
            None
        }
    };
    let hash_kind = repo
        .as_ref()
        .map_or(gix::hash::Kind::Sha1, gix::Repository::object_hash);

    let mut out = String::new();

    // 1. The `--stdin` object. Without `--path` this is always filter-free.
    if opts.stdin {
        if let Some(p) = &opts.path {
            ensure_no_filters(repo.as_ref(), p, &opts)?;
        }
        let mut data = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut data)
            .map_err(|e| anyhow!("could not read from stdin: {e}"))?;
        emit(&mut out, &data, repo.as_ref(), hash_kind, &opts)?;
    }

    // 2. Each `<file>` positional, in command-line order.
    for file in &opts.files {
        hash_file(&mut out, Path::new(file), repo.as_ref(), hash_kind, &opts)?;
    }

    // 3. Paths read from stdin, one per line.
    if opts.stdin_paths {
        let mut buf = String::new();
        std::io::stdin()
            .lock()
            .read_to_string(&mut buf)
            .map_err(|e| anyhow!("could not read from stdin: {e}"))?;
        for line in buf.lines() {
            if line.is_empty() {
                continue;
            }
            hash_file(&mut out, Path::new(line), repo.as_ref(), hash_kind, &opts)?;
        }
    }

    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

/// Read `path`, check it against the filter rules, then hash/write it.
fn hash_file(
    out: &mut String,
    path: &Path,
    repo: Option<&gix::Repository>,
    hash_kind: gix::hash::Kind,
    opts: &Opts,
) -> Result<()> {
    let attr_path = opts.path.as_deref().unwrap_or(path);
    ensure_no_filters(repo, attr_path, opts)?;

    let data = std::fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow!(
                "could not open '{}' for reading: No such file or directory",
                path.display()
            )
        } else {
            anyhow!("Unable to hash {}", path.display())
        }
    })?;
    emit(out, &data, repo, hash_kind, opts)
}

/// Validate (unless `--literally`), hash, optionally write, and append the id.
fn emit(
    out: &mut String,
    data: &[u8],
    repo: Option<&gix::Repository>,
    hash_kind: gix::hash::Kind,
    opts: &Opts,
) -> Result<()> {
    if !opts.literally && opts.kind != Kind::Blob {
        ObjectRef::from_bytes(data, opts.kind, hash_kind)
            .map_err(|e| anyhow!("object fails check: {e}\nrefusing to create malformed object"))?;
    }

    let id: ObjectId = if opts.write {
        let repo = repo.expect("repository is discovered before any -w write");
        repo.objects
            .write_buf(opts.kind, data)
            .map_err(|e| anyhow!("unable to write {} object: {e}", opts.kind))?
    } else {
        gix::objs::compute_hash(hash_kind, opts.kind, data)
            .map_err(|e| anyhow!("unable to hash {} object: {e}", opts.kind))?
    };

    out.push_str(&id.to_hex().to_string());
    out.push('\n');
    Ok(())
}

/// `-t <type>` accepts exactly git's four object types.
fn parse_kind(s: &str) -> Result<Kind> {
    Kind::from_bytes(s.as_bytes()).map_err(|_| anyhow!("invalid object type \"{s}\""))
}

/// The argument conflicts git rejects with its usage message, verbatim.
fn conflict(opts: &Opts) -> Option<&'static str> {
    if opts.stdin_paths {
        if opts.stdin {
            return Some("Can't use --stdin-paths with --stdin");
        }
        if !opts.files.is_empty() {
            return Some("Can't specify files with --stdin-paths");
        }
        if opts.path.is_some() {
            return Some("Can't use --stdin-paths with --path");
        }
    }
    if opts.path.is_some() && opts.no_filters {
        return Some("Can't use --path with --no-filters");
    }
    None
}

/// Refuse to hash when the checkin conversion could change the bytes.
///
/// Filtering is decided by `core.autocrlf` and by the `text`/`eol`/`filter`/
/// `ident` attributes, which are read from `.gitattributes` along `rela_path`,
/// from `$GIT_DIR/info/attributes`, and from `core.attributesFile`. None of that
/// conversion is implemented here, so the presence of any of those inputs is a
/// hard error instead of a silently-unconverted (and therefore wrong) id.
fn ensure_no_filters(repo: Option<&gix::Repository>, rela_path: &Path, opts: &Opts) -> Result<()> {
    if opts.no_filters {
        return Ok(());
    }
    let Some(repo) = repo else { return Ok(()) };

    let snapshot = repo.config_snapshot();
    if let Some(v) = snapshot.string("core.autocrlf") {
        let v = v.to_str_lossy().into_owned();
        if v != "false" {
            bail!("core.autocrlf={v} would filter the input; pass --no-filters");
        }
    }
    if let Ok(Some(p)) = snapshot.trusted_path("core.attributesFile") {
        if p.exists() {
            bail!(
                "attributes in {} may filter the input; pass --no-filters",
                p.display()
            );
        }
    }
    drop(snapshot);

    let info = repo.git_dir().join("info").join("attributes");
    if info.exists() {
        bail!(
            "attributes in {} may filter the input; pass --no-filters",
            info.display()
        );
    }

    // `.gitattributes` from the file's own directory up to the worktree root.
    let Some(workdir) = repo.workdir() else {
        return Ok(());
    };
    let root = workdir.canonicalize().unwrap_or_else(|_| workdir.to_owned());
    let start = rela_path
        .canonicalize()
        .unwrap_or_else(|_| root.join(rela_path));
    let mut dir = start.parent();
    while let Some(d) = dir {
        let candidate = d.join(".gitattributes");
        if candidate.exists() {
            bail!(
                "attributes in {} may filter the input; pass --no-filters",
                candidate.display()
            );
        }
        if d == root {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}
