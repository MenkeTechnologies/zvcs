//! `git archive` — create an archive of files from a named tree.
//!
//! This is a faithful port of git's `archive.c` traversal plus the whole of
//! `archive-tar.c` (the `ustar` record layout, the pax extended headers, the
//! `tar.umask` mode mangling and the 10 KiB block padding). Output for the
//! `tar` format is byte-identical to stock `git archive`, verified against a
//! reference implementation on fixtures covering nested directories, symlinks,
//! executable bits, `--prefix`, path filters, paths over 100 bytes (both the
//! `ustar` prefix split and the `<oid>.data` + pax `path` fallback) and the
//! record-boundary padding cases.
//!
//! Covered:
//!   * `<tree-ish>` resolving to a commit, tag or raw tree. A commit (or a tag
//!     peeling to one) contributes the `pax_global_header` carrying
//!     `comment=<commit-oid>` and makes the committer date the entry mtime; a
//!     bare tree uses the current time and emits no global header.
//!   * `--format=tar` (the default, also inferred from an `-o` name ending in
//!     `.tar`).
//!   * `--format=tgz` / `--format=tar.gz`, git's in-process `gzip` filter. See
//!     [`gzip`] — it is a port of zlib's `deflate.c` + `trees.c` driven exactly
//!     the way `archive-tar.c` drives it (10 KiB input blocks into a 16 KiB
//!     output buffer, `deflateSetHeader` with `os = 3`), so the compressed
//!     bytes match stock git's at every level including `-0`.
//!   * `--prefix=<prefix>/`, including the leading directory entry git writes
//!     for a prefix that ends in `/`.
//!   * `-o <file>` / `--output=<file>`.
//!   * `-l` / `--list` (including git's refusal to take any positional
//!     alongside it), `-v` / `--verbose`.
//!   * `--mtime=<time>`, which overrides the entry *and* `pax_global_header`
//!     timestamps. git runs the value through `approxidate()`, which falls back
//!     to the current time for anything it cannot parse; see [`approxidate`] for
//!     what this port does and does not parse.
//!   * `-<digits>` compression levels, to the extent git itself honours them:
//!     `tar` rejects any with `Argument not supported for format 'tar': -<n>`,
//!     `zip` rejects one outside `0..=9` with the same message, `tgz`/`tar.gz`
//!     accept any at parse time and only fail (at `deflateInit2`, after the tree
//!     walk) on one above `9`; the last `-<digits>` on the command line is the
//!     one reported.
//!   * Trailing `[--] <path>...` filters, with git's "pathspec did not match"
//!     failure, and git's lazy directory-entry emission (a directory record is
//!     written only once a file below it is written).
//!   * Being run from a subdirectory, which narrows the tree to that
//!     subdirectory exactly as git does.
//!   * `tar.umask` (numeric, and `tar.umask=user`, which git reads from the
//!     process umask by `umask(0)`-then-restore — reproduced here).
//!   * `--add-file <path>` and `--add-virtual-file <path:content>`: the extra
//!     records git writes last of all, after the tree walk, in command-line
//!     order. `--add-file` takes the disk file's bytes and its executable bit
//!     (canonicalised and masked exactly like a tree blob), names it
//!     `<--prefix>` + `basename(path)`, and `stat()`s it at parse time so
//!     `File not found` / `Not a regular file` fire in git's order; the fake
//!     object id git assigns each added file (a 1-based counter, big-endian in
//!     the first eight bytes of an all-zero hash) is reproduced so the
//!     `<oid>.data` / `<oid>.paxheader` overflow name matches. `--add-virtual-file`
//!     writes the literal content under the literal path (git does *not* prepend
//!     `--prefix` to it), validated (missing colon / empty name) at parse time.
//!   * Unknown options: `--<opt>` → `error: unknown option '<opt>'`, `-<c>` →
//!     `error: unknown switch '<c>'`, each followed by the usage block on stderr
//!     with exit 129; `-h` / `--help` print the same usage to stdout, exit 129;
//!     the `--no-` negations of the boolean and value options.
//!
//!   * `--worktree-attributes`, to the extent this port supports attributes at
//!     all: the working directory's `.gitattributes` files are read and, like
//!     the ones in the tree, rejected if any of them assigns a
//!     content-affecting attribute. With none set the flag cannot change a byte
//!     of the archive, which is exactly what stock git produces.
//!
//! Not covered — every one of these fails loudly rather than emitting an
//! archive that would silently differ from git's:
//!   * `--format=zip`: git's `archive-zip.c` is a separate container format
//!     (local file headers, a central directory, DOS timestamps and the zip64
//!     escapes) that is not ported here. The deflate coder it would need does
//!     now exist in [`gzip`]; the container does not. The rejection is deferred
//!     to archive-writing time, exactly where git would begin emitting the
//!     container, so every diagnostic git produces first for an invalid `zip`
//!     invocation (unknown option, out-of-range level, bad tree-ish, unmatched
//!     pathspec, content-affecting attribute) still comes out with git's exact
//!     exit code; only a `zip` request git would have completed fails here.
//!   * `--remote` / `--exec`: the `git-upload-archive` protocol against another
//!     repository — a live transport handshake (or a spawned `git-upload-archive`
//!     subprocess even for a local `--remote=.`), which this port does not drive.
//!   * A regular blob larger than the 8 GiB `ustar` `size` field, which git spills
//!     into a pax `size` record; not reproduced here (no fixture can exercise it
//!     for byte comparison).
//!   * Repositories whose archived tree carries content-affecting attributes
//!     (`export-ignore`, `export-subst`, `text`, `eol`, `filter`, `ident`,
//!     `working-tree-encoding`), or a `core.autocrlf` / `core.eol` /
//!     `core.attributesFile` setting, or `$GIT_DIR/info/attributes`. git runs
//!     archived blobs through `convert_to_working_tree()`; this port writes the
//!     blob verbatim, so any of those inputs is rejected instead of producing a
//!     wrong archive.
//!   * Pathspec magic (`:(glob)`, `*`, `?`, `[`), which this port does not
//!     implement.

use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::object::tree::EntryKind;

/// git's `RECORDSIZE`: one `ustar` record.
const RECORD: usize = 512;
/// git's `BLOCKSIZE`: the unit stdout is padded up to.
const BLOCK: u64 = 10240;
/// The `ustar` `name` field width; longer paths need a prefix split or pax.
const NAME_MAX: usize = 100;
/// The `ustar` `prefix` field width.
const PREFIX_MAX: usize = 155;
/// git refuses a plain `size` field beyond this and emits a pax `size` record.
const SIZE_MAX: u64 = 0o77777777777;

const ZEROS: [u8; RECORD] = [0; RECORD];

/// The formats stock `git archive --list` reports. Only `tar` can be produced
/// here; the others exist to keep `--list` byte-identical and are rejected with
/// a precise message the moment one is actually requested.
const FORMATS: &[&str] = &["tar", "tgz", "tar.gz", "zip"];

/// The formats carrying git's `ARCHIVER_WANT_COMPRESSION_LEVELS`. A `-<digits>`
/// given for any other format is fatal, which is the only way a compression
/// level is observable from here since none of these three can be produced yet.
const LEVEL_FORMATS: &[&str] = &["tgz", "tar.gz", "zip"];

/// git's `archive_usage` followed by the option list `parse_options()` renders,
/// byte for byte, as printed on stderr for a usage error.
const USAGE: &str = "\
usage: git archive [<options>] <tree-ish> [<path>...]
   or: git archive --list
   or: git archive --remote <repo> [--exec <cmd>] [<options>] <tree-ish> [<path>...]
   or: git archive --remote <repo> [--exec <cmd>] --list

    --[no-]format <fmt>   archive format
    --[no-]prefix <prefix>
                          prepend prefix to each pathname in the archive
    --[no-]add-file <file>
                          add untracked file to archive
    --[no-]add-virtual-file <path:content>
                          add untracked file to archive
    -o, --[no-]output <file>
                          write the archive to this file
    --[no-]worktree-attributes
                          read .gitattributes in working directory
    -v, --[no-]verbose    report archived files on stderr
    --mtime <time>        set modification time of archive entries
    -NUM                  set compression level

    -l, --[no-]list       list supported archive formats

    --[no-]remote <repo>  retrieve the archive from remote repository <repo>
    --[no-]exec <command> path to the remote git-upload-archive command

";

/// Parsed command line for one `archive` invocation.
#[derive(Default)]
struct Opts {
    format: Option<String>,
    prefix: Option<String>,
    output: Option<String>,
    verbose: bool,
    /// Raw `--mtime` text, still unparsed: git resolves it only after the usage,
    /// format and compression-level diagnostics have had their chance.
    mtime: Option<String>,
    /// The last `-<digits>` seen; git keeps a single `int`, so later ones win.
    level: Option<u32>,
    worktree_attributes: bool,
    /// `--add-file` / `--add-virtual-file` records, in command-line order. git
    /// assigns each one a fake object id from a 1-based counter incremented per
    /// `add_file_cb` call, which surfaces as the `<oid>.data` / `<oid>.paxheader`
    /// name when the in-archive path overflows the `ustar` fields.
    added: Vec<Added>,
    treeish: Option<String>,
    paths: Vec<String>,
}

/// One `--add-file` / `--add-virtual-file` record, resolved at parse time so its
/// diagnostics (`File not found`, `Not a regular file`, `missing colon`, `empty
/// file name`) fire in git's command-line order, before the format, compression
/// level, tree-ish and pathspec checks that run after `parse_options()`.
enum Added {
    /// `--add-file <path>`: the archive name is `<--prefix>` + `basename(path)`
    /// (git prepends `args->base`), the mode is the disk file's executable bit
    /// canonicalised the same way tree blobs are, and the bytes are read from
    /// disk at archive-writing time.
    File {
        /// `basename(path)`; the `--prefix` is prepended when the record is written.
        name: Vec<u8>,
        /// Whether any execute bit is set on disk (`st_mode & 0111`).
        exec: bool,
        /// The path to read the bytes from, resolved against the process cwd.
        disk: std::path::PathBuf,
    },
    /// `--add-virtual-file <path:content>`: the archive name is the literal path
    /// before the first colon — git does *not* prepend `--prefix` to it — the mode
    /// is a non-executable blob's, and the content is the literal bytes after the
    /// first colon.
    Virtual { name: Vec<u8>, content: Vec<u8> },
}

/// One record git will write, in the order it writes them. Collected up front so
/// that a pathspec that matches nothing fails before a single byte reaches
/// stdout, which is what git does.
struct Item {
    /// Full in-archive path; directories and submodules carry a trailing `/`.
    path: Vec<u8>,
    kind: EntryKind,
    oid: ObjectId,
}

/// `git archive` — write a `tar` archive of `<tree-ish>` to stdout or `-o`.
pub fn archive(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts::default();
    let mut list = false;
    let mut literal = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();
        if literal {
            // git's parse-options strips `--` and leaves the positionals, so the
            // tree-ish may still be the first thing after it.
            if opts.treeish.is_none() {
                opts.treeish = Some(a.to_string());
            } else {
                opts.paths.push(a.to_string());
            }
            i += 1;
            continue;
        }
        match a {
            "--" => literal = true,
            // git's `parse_options()` prints the full usage to *stdout* and exits
            // 129 on `-h` / `--help`.
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-l" | "--list" => list = true,
            "--no-list" => list = false,
            "-v" | "--verbose" => opts.verbose = true,
            "--no-verbose" => opts.verbose = false,
            "--worktree-attributes" => opts.worktree_attributes = true,
            "--no-worktree-attributes" => opts.worktree_attributes = false,
            "--format" => opts.format = Some(value_of(args, &mut i, "--format")?),
            "--no-format" => opts.format = None,
            "--prefix" => opts.prefix = Some(value_of(args, &mut i, "--prefix")?),
            "--no-prefix" => opts.prefix = None,
            "-o" | "--output" => opts.output = Some(value_of(args, &mut i, "--output")?),
            // git accepts `--no-output` but it is a no-op: `-o FILE --no-output`
            // (either order) still writes to FILE. Verified against git 2.55.0.
            "--no-output" => {}
            "--mtime" => opts.mtime = Some(value_of(args, &mut i, "--mtime")?),
            "--add-file" => {
                let value = value_of(args, &mut i, a)?;
                match resolve_add_file(&value) {
                    Ok(item) => opts.added.push(item),
                    Err(msg) => {
                        eprintln!("fatal: {msg}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            "--add-virtual-file" => {
                let value = value_of(args, &mut i, a)?;
                match resolve_virtual_file(&value) {
                    Ok(item) => opts.added.push(item),
                    Err(msg) => {
                        eprintln!("fatal: {msg}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            _ if a.starts_with("--format=") => opts.format = Some(a[9..].to_string()),
            _ if a.starts_with("--prefix=") => opts.prefix = Some(a[9..].to_string()),
            _ if a.starts_with("--output=") => opts.output = Some(a[9..].to_string()),
            _ if a.starts_with("--mtime=") => opts.mtime = Some(a[8..].to_string()),
            _ if a.starts_with("--add-file=") => match resolve_add_file(&a[11..]) {
                Ok(item) => opts.added.push(item),
                Err(msg) => {
                    eprintln!("fatal: {msg}");
                    return Ok(ExitCode::from(128));
                }
            },
            _ if a.starts_with("--add-virtual-file=") => match resolve_virtual_file(&a[19..]) {
                Ok(item) => opts.added.push(item),
                Err(msg) => {
                    eprintln!("fatal: {msg}");
                    return Ok(ExitCode::from(128));
                }
            },
            // `--remote` / `--exec` are real git options, so this is not an
            // "unknown option" — but honouring them means driving the
            // `git-upload-archive` protocol (a transport handshake, or a spawned
            // `git-upload-archive` even for a local `--remote=.`), which this port
            // does not do. Consume any separate value so the bail is terse.
            "--remote" | "--exec" => {
                let _ = value_of(args, &mut i, a)?;
                bail!("{a} drives the git-upload-archive protocol, which is not supported here");
            }
            _ if a.starts_with("--remote=") || a.starts_with("--exec=") => {
                bail!("{a} drives the git-upload-archive protocol, which is not supported here");
            }
            _ if compression_level(a).is_some() => opts.level = compression_level(a),
            // git's `parse_options()` rejects any other dashed token with the
            // usage block on stderr and exit 129 — `unknown option` for a `--long`
            // form (the leading `--` stripped, the rest kept verbatim) and
            // `unknown switch` for a single-dash form (just the switch character).
            _ if a.starts_with("--") => {
                eprintln!("error: unknown option `{}'", &a[2..]);
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            _ if a.len() > 1 && a.starts_with('-') => {
                let sw = a.chars().nth(1).unwrap_or('?');
                eprintln!("error: unknown switch `{sw}'");
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            _ if opts.treeish.is_none() => opts.treeish = Some(a.to_string()),
            _ => opts.paths.push(a.to_string()),
        }
        i += 1;
    }

    // `--list` short-circuits everything else, but git rejects any leftover
    // positional first — it never lists formats *and* takes a tree-ish.
    if list {
        if let Some(extra) = opts.treeish.as_deref().or(opts.paths.first().map(String::as_str)) {
            eprintln!("fatal: extra command line parameter '{extra}'");
            return Ok(ExitCode::from(128));
        }
        let mut out = String::new();
        for f in FORMATS {
            out.push_str(f);
            out.push('\n');
        }
        print!("{out}");
        return Ok(ExitCode::SUCCESS);
    }

    // git checks `argc` before it looks at the format or the compression level,
    // so a missing tree-ish outranks both `-<digits>` and an unknown format.
    let Some(spec) = opts.treeish.clone() else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    };

    // Resolve the requested format: explicit `--format`, else inferred from the
    // `-o` filename, else `tar`.
    let format = match opts.format.as_deref() {
        Some(f) => f.to_string(),
        None => opts
            .output
            .as_deref()
            .and_then(format_from_filename)
            .unwrap_or("tar")
            .to_string(),
    };
    if !FORMATS.contains(&format.as_str()) {
        eprintln!("fatal: Unknown archive format '{format}'");
        return Ok(ExitCode::from(128));
    }

    // git's `parse_archive_args()`: a compression level is fatal at parse time
    // for a format that does not declare `ARCHIVER_WANT_COMPRESSION_LEVELS`
    // (`tar`), and for `zip`, whose archiver additionally rejects a level outside
    // zlib's `0..=9` range with the very same message and exit code. `tgz` /
    // `tar.gz` accept any level here; an out-of-range one is not diagnosed until
    // `deflateInit2()` fails, which is after the tree walk (see below). The level
    // reported is the last `-<digits>` parsed, not the first one given.
    if let Some(level) = opts.level {
        let reject = !LEVEL_FORMATS.contains(&format.as_str()) || (format == "zip" && level > 9);
        if reject {
            eprintln!("fatal: Argument not supported for format '{format}': -{level}");
            return Ok(ExitCode::from(128));
        }
    }

    for p in &opts.paths {
        if p.starts_with(':') || p.contains(['*', '?', '[']) {
            bail!("pathspec magic is not supported: {p:?}");
        }
    }

    let repo = gix::discover(".")?;

    // git lets `tar.<fmt>.command` replace an archiver with an external filter.
    // The internal gzip is what this port reproduces; anything else would have
    // to be spawned, which it does not do. `tar.tgz.command` and
    // `tar.tar.gz.command` are pre-seeded with the internal name, so only a
    // value that differs from it is a problem.
    const INTERNAL_GZIP: &str = "git archive gzip";
    {
        let cfg = repo.config_snapshot();
        for (key, internal) in [
            ("tar.tar.command", None),
            ("tar.tgz.command", Some(INTERNAL_GZIP)),
            ("tar.tar.gz.command", Some(INTERNAL_GZIP)),
        ] {
            if let Some(raw) = cfg.string(key) {
                if internal != Some(raw.to_str_lossy().as_ref()) {
                    bail!("{key} is configured but piping through it is not supported");
                }
            }
        }
    }
    let umask = tar_umask(&repo)?;

    let Ok(id) = repo.rev_parse_single(spec.as_str()) else {
        eprintln!("fatal: not a valid object name: {spec}");
        return Ok(ExitCode::from(128));
    };

    // A commit (or a tag peeling to one) contributes the pax global header and
    // the entry mtime; anything else that peels to a tree uses the current time.
    let commit = id.object()?.peel_to_commit().ok().map(|c| (c.id, c.time()));
    let (commit_id, default_mtime) = match commit {
        Some((cid, time)) => (Some(cid), time?.seconds),
        None => (None, now()),
    };
    // `--mtime` replaces that for every entry and for the global header alike.
    let mtime = match opts.mtime.as_deref() {
        Some(text) => approxidate(text),
        None => default_mtime,
    };
    let Ok(mut tree) = id.object()?.peel_to_tree() else {
        eprintln!("fatal: not a tree object: {}", id.detach());
        return Ok(ExitCode::from(128));
    };

    // git does not diagnose an unsupported container, nor an out-of-range gzip
    // level, until *archive-writing* time — after the subdirectory narrowing,
    // the attribute scan and the whole path-filter walk have each had their turn
    // to fail with git's own exit code. Both checks are therefore deferred to
    // just before the first byte is written (see below); here we only compute the
    // format flags the writer needs.
    let gzipped = matches!(format.as_str(), "tgz" | "tar.gz");
    let level = opts.level.unwrap_or(6);
    // Run from a subdirectory, git narrows the tree to that subdirectory and
    // makes every archived path relative to it.
    if let Some(prefix) = repo.prefix()?.map(std::path::Path::to_path_buf) {
        for part in prefix.components() {
            let name = part.as_os_str().as_encoded_bytes().to_vec();
            let Some(sub) = subtree(&repo, &tree, &name)? else {
                eprintln!("fatal: current working directory is untracked");
                return Ok(ExitCode::from(128));
            };
            tree = sub;
        }
    }

    reject_content_attributes(&repo, &tree)?;
    if opts.worktree_attributes {
        reject_worktree_attributes(&repo)?;
    }

    // Walk first, emit second: a pathspec that matches nothing must fail with an
    // empty stdout.
    let mut matched = vec![false; opts.paths.len()];
    let mut pending: Vec<Item> = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    collect(
        &repo,
        tree.clone(),
        b"",
        &opts.paths,
        &mut matched,
        &mut pending,
        &mut items,
    )?;
    if let Some(idx) = matched.iter().position(|m| !m) {
        eprintln!(
            "fatal: pathspec '{}' did not match any files",
            opts.paths[idx]
        );
        return Ok(ExitCode::from(128));
    }

    // Now that every git diagnostic with an exit code of its own has fired, the
    // two archive-writing-time failures can be emitted in git's own order. git
    // only learns that zlib rejects a `tgz` / `tar.gz` level when
    // `deflateInit2()` fails, which it does here, after the walk — so
    // `git archive --format=tgz -10 <tree> <unmatched>` reports the pathspec
    // miss, not the deflate error.
    if gzipped && level > 9 {
        eprintln!("fatal: deflateInit2: stream consistency error (no message)");
        return Ok(ExitCode::from(128));
    }
    // The `zip` container (git's `archive-zip.c`) is not ported. git would write
    // it and exit 0; this port can only bail. Deferring the bail to here means a
    // `zip` invocation that git itself would have rejected first (unknown option,
    // an out-of-range level, a bad tree-ish, an unmatched pathspec, a
    // content-affecting attribute) still exits with git's exact code and message
    // — only a `zip` request git would actually have succeeded on fails here.
    if !gzipped && format != "tar" {
        bail!(
            "archive format {format:?} is not supported (ported: tar, tgz, tar.gz) — the zip \
             container is not ported"
        );
    }

    let base = opts.prefix.clone().unwrap_or_default();
    let raw: Box<dyn Write> = match &opts.output {
        Some(path) => Box::new(std::io::BufWriter::new(std::fs::File::create(path)?)),
        None => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };
    let sink = if gzipped {
        Sink::Gz(Box::new(gzip::GzDeflate::new(raw, level as i32)))
    } else {
        Sink::Plain(raw)
    };
    let mut tar = Tar {
        out: sink,
        written: 0,
        mtime,
        umask,
        verbose: opts.verbose,
    };

    if let Some(cid) = commit_id {
        tar.global_header(&cid)?;
    }

    // A `--prefix` ending in `/` gets its own directory record, with repeated
    // trailing slashes collapsed to one, before any tree entry.
    if base.ends_with('/') {
        let mut len = base.len();
        while len > 1 && base.as_bytes()[len - 2] == b'/' {
            len -= 1;
        }
        tar.entry(&base.as_bytes()[..len], EntryKind::Tree, &tree.id, &[])?;
    }

    for item in items {
        let mut path = base.clone().into_bytes();
        path.extend_from_slice(&item.path);
        let data = match item.kind {
            EntryKind::Tree | EntryKind::Commit => Vec::new(),
            _ => repo.find_object(item.oid)?.data.clone(),
        };
        tar.entry(&path, item.kind, &item.oid, &data)?;
    }

    // git writes the `--add-file` / `--add-virtual-file` records last of all,
    // after the whole tree walk, in command-line order. Each carries a fake object
    // id built from a 1-based counter (the Nth added file, big-endian in the first
    // eight bytes of an otherwise-zero hash), which only becomes visible when the
    // in-archive path overflows the `ustar` name field and spills into a pax
    // `<oid>.data` / `<oid>.paxheader` record.
    let hash_len = repo.object_hash().len_in_bytes();
    for (idx, added) in opts.added.iter().enumerate() {
        let mut raw = vec![0u8; hash_len];
        raw[..8].copy_from_slice(&((idx as u64) + 1).to_be_bytes());
        let oid = ObjectId::from_bytes_or_panic(&raw);
        match added {
            Added::File { name, exec, disk } => {
                let mut path = base.clone().into_bytes();
                path.extend_from_slice(name);
                let data = std::fs::read(disk)?;
                let kind = if *exec {
                    EntryKind::BlobExecutable
                } else {
                    EntryKind::Blob
                };
                tar.entry(&path, kind, &oid, &data)?;
            }
            Added::Virtual { name, content } => {
                // git does not prepend `--prefix` to a virtual file's path.
                tar.entry(name, EntryKind::Blob, &oid, content)?;
            }
        }
    }

    tar.finish()?;
    // The gzip stream's own trailer is only written once the tar is complete.
    tar.out.done()?;

    Ok(ExitCode::SUCCESS)
}

/// Where the tar bytes go: straight out, or through git's in-process gzip.
enum Sink {
    Plain(Box<dyn Write>),
    Gz(Box<gzip::GzDeflate<Box<dyn Write>>>),
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Sink::Plain(w) => w.write(buf),
            Sink::Gz(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Sink::Plain(w) => w.flush(),
            Sink::Gz(w) => w.flush(),
        }
    }
}

impl Sink {
    /// Finalise: flush the plain writer, or close out the deflate stream and
    /// then flush what it was writing to.
    fn done(self) -> Result<()> {
        match self {
            Sink::Plain(mut w) => w.flush()?,
            Sink::Gz(w) => w.finish()?.flush()?,
        }
        Ok(())
    }
}

/// Read the value of an option given as a separate argument (`--format tar`).
fn value_of(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("option `{flag}` requires a value"))
}

/// git's `add_file_cb` for `--add-virtual-file <path:content>`, which runs
/// inside `parse_options()` — so its diagnostics fire before the format,
/// compression-level and tree-ish ones, exactly like stock git. The value must
/// carry a colon, and the path before it must be non-empty; the first colon
/// splits the (literal, un-prefixed) archive path from the content. Returns the
/// `fatal:` text git would `die()` with (exit 128) on the `Err` side.
fn resolve_virtual_file(value: &str) -> std::result::Result<Added, String> {
    match value.find(':') {
        None => Err(format!("missing colon: '{value}'")),
        Some(0) => Err(format!("empty file name: '{value}'")),
        Some(idx) => Ok(Added::Virtual {
            name: value[..idx].as_bytes().to_vec(),
            content: value[idx + 1..].as_bytes().to_vec(),
        }),
    }
}

/// git's `add_file_cb` for `--add-file <path>`: it `stat()`s the file while
/// still inside `parse_options()`, so `File not found` / `Not a regular file`
/// (both `die()`, exit 128) fire in command-line order, ahead of the post-parse
/// diagnostics. The path is resolved against the process cwd exactly as git's
/// `prefix_filename()` does when run from a subdirectory; the archive name is
/// the basename, with `--prefix` prepended later at writing time.
fn resolve_add_file(path: &str) -> std::result::Result<Added, String> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Err(format!("File not found: {path}"));
    };
    if !meta.is_file() {
        return Err(format!("Not a regular file: {path}"));
    }
    // git's `canon_mode()` keys the executable bit off the *owner* execute bit
    // (0100) alone, not any of the group/other bits, before the tar writer mangles
    // it to 0777/0666 & ~umask. Verified against git 2.55.0 (mode 0641 archives as
    // non-executable, 0744 as executable).
    let exec = {
        use std::os::unix::fs::MetadataExt;
        meta.mode() & 0o100 != 0
    };
    let name = std::path::Path::new(path)
        .file_name()
        .map(|s| s.as_encoded_bytes().to_vec())
        .unwrap_or_else(|| path.as_bytes().to_vec());
    Ok(Added::File {
        name,
        exec,
        disk: std::path::PathBuf::from(path),
    })
}

/// The level in a `-<digits>` argument, or `None` if `arg` is not one.
fn compression_level(arg: &str) -> Option<u32> {
    let digits = arg.strip_prefix('-')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Seconds since the epoch, right now.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// git's `approxidate()`, as `--mtime` uses it: whatever the value parses to,
/// and the current time for anything that does not parse at all (`--mtime=@0`
/// and `--mtime=bogus-time` both land here, as stock git does).
///
/// The parse itself is `gix::date::parse`, covering ISO-8601 (with or without a
/// zone, dotted or compact), RFC-2822, git's `<seconds> <±hhmm>` raw form, a
/// bare epoch count and relative spellings like `2 days ago`. Two known
/// deviations from git: a zone-less date is read as UTC where git reads it in
/// the local zone, and a date git's approxidate accepts but `gix-date` does not
/// falls back to the current time instead of that date.
fn approxidate(spec: &str) -> i64 {
    match gix::date::parse(spec, Some(std::time::SystemTime::now())) {
        Ok(time) => time.seconds,
        Err(_) => now(),
    }
}

/// git's `filename_to_archive_format()`: the format whose name the output file's
/// extension spells, or `None` to fall back to `tar`.
fn format_from_filename(name: &str) -> Option<&'static str> {
    FORMATS
        .iter()
        .copied()
        .find(|f| name.len() > f.len() + 1 && name.ends_with(&format!(".{f}")))
}

/// `tar.umask`, parsed the way `git_config_int` does (a leading `0` means octal).
fn tar_umask(repo: &gix::Repository) -> Result<u32> {
    let Some(raw) = repo.config_snapshot().string("tar.umask") else {
        return Ok(0o002);
    };
    let text = raw.to_str()?.trim().to_string();
    if text == "user" {
        // git's `git_tar_config`: `tar_umask = umask(0); umask(tar_umask);`.
        // There is no read-only POSIX umask getter, so git reads it by setting
        // it to zero and restoring it in one breath; this does the same. The
        // result is at most 0o777, so masking guards against any garbage the C
        // ABI leaves in the high bits of the narrower `mode_t` on some targets.
        return Ok(process_umask() & 0o7777);
    }
    let (digits, radix) = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(rest) => (rest, 16),
        None if text.len() > 1 && text.starts_with('0') => (&text[1..], 8),
        None => (text.as_str(), 10),
    };
    u32::from_str_radix(digits, radix).map_err(|_| anyhow::anyhow!("invalid tar.umask {text:?}"))
}

/// The process umask, read the way git reads it for `tar.umask=user`: set it to
/// zero to learn the old value, then restore it. `mode_t` is 32-bit on Linux and
/// 16-bit on the BSD/macOS targets, so its width is selected per platform to keep
/// the C ABI correct.
#[cfg(target_os = "linux")]
type ModeT = u32;
#[cfg(not(target_os = "linux"))]
type ModeT = u16;

extern "C" {
    fn umask(mask: ModeT) -> ModeT;
}

fn process_umask() -> u32 {
    // SAFETY: `umask(2)` has no failure mode and no memory effects; the old value
    // is restored immediately, matching git's `umask(0); umask(old)` sequence.
    unsafe {
        let old = umask(0);
        umask(old);
        u32::from(old)
    }
}

/// The sub-tree named `name` directly below `tree`, if it is a tree.
fn subtree<'r>(
    repo: &'r gix::Repository,
    tree: &gix::Tree<'r>,
    name: &[u8],
) -> Result<Option<gix::Tree<'r>>> {
    for entry in tree.decode()?.entries.iter() {
        if &entry.filename[..] == name && entry.mode.is_tree() {
            return Ok(Some(repo.find_object(entry.oid.to_owned())?.peel_to_tree()?));
        }
    }
    Ok(None)
}

/// Attributes that make git rewrite or drop blob content on the way into the
/// archive. This port writes blobs verbatim, so their presence is a hard error.
const CONTENT_ATTRS: &[&str] = &[
    "export-ignore",
    "export-subst",
    "text",
    "eol",
    "crlf",
    "ident",
    "filter",
    "working-tree-encoding",
];

/// Fail if anything in the archived tree, or in the configuration, would make
/// git's `convert_to_working_tree()` change what lands in the archive.
fn reject_content_attributes(repo: &gix::Repository, tree: &gix::Tree<'_>) -> Result<()> {
    let cfg = repo.config_snapshot();
    if cfg.string("core.attributesFile").is_some() {
        bail!("core.attributesFile is set but attribute-driven content conversion is not supported");
    }
    if let Some(raw) = cfg.string("core.autocrlf") {
        let value = raw.to_str_lossy().trim().to_ascii_lowercase();
        if !matches!(value.as_str(), "false" | "0" | "no" | "off" | "") {
            bail!("core.autocrlf={value} is set but content conversion is not supported");
        }
    }
    if cfg.string("core.eol").is_some() {
        bail!("core.eol is set but content conversion is not supported");
    }
    if repo.common_dir().join("info").join("attributes").exists() {
        bail!("$GIT_DIR/info/attributes exists but attribute-driven content conversion is not supported");
    }
    scan_attributes(repo, tree.clone(), b"")
}

/// `--worktree-attributes`: git additionally consults the `.gitattributes`
/// files present in the working directory. This port cannot apply any of them,
/// so it reads them all and fails on the first content-affecting assignment;
/// with none set the flag is a no-op, which is also true of stock git.
fn reject_worktree_attributes(repo: &gix::Repository) -> Result<()> {
    let Some(root) = repo.workdir() else {
        return Ok(());
    };
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == std::ffi::OsStr::new(".git") {
                continue;
            }
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => dirs.push(path),
                Ok(t) if t.is_file() && name == std::ffi::OsStr::new(".gitattributes") => {
                    let text = std::fs::read(&path)?;
                    if let Some(attr) = first_content_attribute(&text) {
                        let shown = path.strip_prefix(root).unwrap_or(path.as_path()).display();
                        bail!(
                            "{shown} assigns {attr:?}, which changes archived content; \
                             --worktree-attributes cannot be honoured"
                        );
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Recursively read every `.gitattributes` in the tree and reject the ones that
/// assign a content-affecting attribute.
fn scan_attributes(repo: &gix::Repository, tree: gix::Tree<'_>, base: &[u8]) -> Result<()> {
    let entries: Vec<(bool, Vec<u8>, ObjectId)> = tree
        .decode()?
        .entries
        .iter()
        .map(|e| (e.mode.is_tree(), e.filename.to_vec(), e.oid.to_owned()))
        .collect();

    for (is_tree, filename, oid) in entries {
        let mut path = base.to_vec();
        path.extend_from_slice(&filename);
        if is_tree {
            path.push(b'/');
            let child = repo.find_object(oid)?.peel_to_tree()?;
            scan_attributes(repo, child, &path)?;
        } else if filename.as_slice() == &b".gitattributes"[..] {
            let blob = repo.find_object(oid)?;
            if let Some(attr) = first_content_attribute(&blob.data) {
                let shown = String::from_utf8_lossy(&path).into_owned();
                bail!(
                    "{shown} assigns {attr:?}, which changes archived content; \
                     attribute-driven conversion is not supported"
                );
            }
        }
    }
    Ok(())
}

/// The first content-affecting attribute assigned anywhere in a `.gitattributes`.
fn first_content_attribute(text: &[u8]) -> Option<String> {
    for line in text.split(|b| *b == b'\n') {
        let line = String::from_utf8_lossy(line);
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for token in line.split_whitespace().skip(1) {
            let token = token.trim_start_matches(['-', '!']);
            let name = token.split('=').next().unwrap_or(token).to_ascii_lowercase();
            if CONTENT_ATTRS.contains(&name.as_str()) {
                return Some(name);
            }
        }
    }
    None
}

/// Depth-first walk producing the exact record sequence git writes.
///
/// Directories are *queued* rather than written: git only materialises a
/// directory record once a file beneath it survives filtering, so a sub-tree
/// that contributes nothing leaves no trace in the archive.
fn collect(
    repo: &gix::Repository,
    tree: gix::Tree<'_>,
    base: &[u8],
    specs: &[String],
    matched: &mut [bool],
    pending: &mut Vec<Item>,
    out: &mut Vec<Item>,
) -> Result<()> {
    let entries: Vec<(EntryKind, Vec<u8>, ObjectId)> = tree
        .decode()?
        .entries
        .iter()
        .map(|e| (e.mode.kind(), e.filename.to_vec(), e.oid.to_owned()))
        .collect();

    for (kind, filename, oid) in entries {
        let mut path = base.to_vec();
        path.extend_from_slice(&filename);

        if kind == EntryKind::Tree {
            if !descend(&path, specs) {
                continue;
            }
            let mut dir = path.clone();
            dir.push(b'/');
            pending.push(Item {
                path: dir.clone(),
                kind,
                oid,
            });
            let child = repo.find_object(oid)?.peel_to_tree()?;
            collect(repo, child, &dir, specs, matched, pending, out)?;
            continue;
        }

        if !selects(&path, specs, matched) {
            continue;
        }
        // Submodules become directory records, so they carry a trailing slash.
        if kind == EntryKind::Commit {
            path.push(b'/');
        }
        flush_pending(pending, &path, out);
        out.push(Item { path, kind, oid });
    }
    Ok(())
}

/// Write out the queued directories that are ancestors of `path`, dropping the
/// ones left behind by a sub-tree that turned out to be empty. This mirrors the
/// `c->bottom` stack unwinding in git's `queue_or_write_archive_entry()`.
fn flush_pending(pending: &mut Vec<Item>, path: &[u8], out: &mut Vec<Item>) {
    let ancestors: Vec<Item> = pending
        .drain(..)
        .filter(|dir| path.starts_with(&dir.path))
        .collect();
    out.extend(ancestors);
}

/// Whether a file at `path` passes the path filters, marking the ones it matches.
fn selects(path: &[u8], specs: &[String], matched: &mut [bool]) -> bool {
    if specs.is_empty() {
        return true;
    }
    let mut any = false;
    for (idx, spec) in specs.iter().enumerate() {
        let spec = spec.trim_end_matches('/').as_bytes();
        if path == spec || (path.starts_with(spec) && path.get(spec.len()) == Some(&b'/')) {
            matched[idx] = true;
            any = true;
        }
    }
    any
}

/// Whether the sub-tree at `path` can still contain a filtered-in file.
fn descend(path: &[u8], specs: &[String]) -> bool {
    if specs.is_empty() {
        return true;
    }
    specs.iter().any(|spec| {
        let spec = spec.trim_end_matches('/').as_bytes();
        path == spec
            || (path.starts_with(spec) && path.get(spec.len()) == Some(&b'/'))
            || (spec.starts_with(path) && spec.get(path.len()) == Some(&b'/'))
    })
}

/// The `ustar` writer: a direct port of git's `archive-tar.c`.
struct Tar<W: Write> {
    out: W,
    written: u64,
    mtime: i64,
    umask: u32,
    verbose: bool,
}

impl<W: Write> Tar<W> {
    fn raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.out.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    /// `data` followed by NUL padding up to the next 512-byte record boundary.
    fn payload(&mut self, data: &[u8]) -> Result<()> {
        self.raw(data)?;
        let rem = data.len() % RECORD;
        if rem != 0 {
            self.raw(&ZEROS[..RECORD - rem])?;
        }
        Ok(())
    }

    /// The `pax_global_header` recording the commit the archive was made from.
    fn global_header(&mut self, commit: &ObjectId) -> Result<()> {
        let record = ext_record(b"comment", commit.to_hex().to_string().as_bytes());
        let header = build_header(
            b"pax_global_header",
            b"",
            b"",
            0o100666,
            record.len() as u64,
            self.mtime,
            b'g',
        );
        self.raw(&header)?;
        self.payload(&record)
    }

    /// One archive entry, including the `<oid>.paxheader` record when the path
    /// or symlink target does not fit the `ustar` fields.
    fn entry(&mut self, path: &[u8], kind: EntryKind, oid: &ObjectId, data: &[u8]) -> Result<()> {
        if self.verbose {
            eprintln!("{}", String::from_utf8_lossy(path));
        }

        // git's mode mangling: directories and submodules get 0777 masked by
        // tar.umask, symlinks an unmasked 0777, regular files 0777 or 0666
        // depending on the executable bit, masked.
        let (typeflag, mode) = match kind {
            EntryKind::Tree => (b'5', (0o040000 | 0o777) & !self.umask),
            EntryKind::Commit => (b'5', (0o160000 | 0o777) & !self.umask),
            EntryKind::Link => (b'2', 0o120000 | 0o777),
            EntryKind::BlobExecutable => (b'0', (0o100755 | 0o777) & !self.umask),
            EntryKind::Blob => (b'0', (0o100644 | 0o666) & !self.umask),
        };
        let is_regular = typeflag == b'0';

        let mut ext: Vec<u8> = Vec::new();
        let mut name: Vec<u8> = Vec::new();
        let mut prefix: Vec<u8> = Vec::new();
        if path.len() > NAME_MAX {
            // Split on the last `/` that leaves a short-enough remainder; when no
            // such split exists the real path moves into a pax `path` record.
            let split = ustar_prefix_len(path);
            let rest = path.len() - split - 1;
            if split > 0 && rest <= NAME_MAX {
                prefix.extend_from_slice(&path[..split]);
                name.extend_from_slice(&path[split + 1..]);
            } else {
                name.extend_from_slice(format!("{oid}.data").as_bytes());
                ext.extend_from_slice(&ext_record(b"path", path));
            }
        } else {
            name.extend_from_slice(path);
        }

        let mut link: Vec<u8> = Vec::new();
        if kind == EntryKind::Link {
            if data.len() > NAME_MAX {
                link.extend_from_slice(format!("see {oid}.paxheader").as_bytes());
                ext.extend_from_slice(&ext_record(b"linkpath", data));
            } else {
                link.extend_from_slice(data);
            }
        }

        let size = if is_regular { data.len() as u64 } else { 0 };
        if size > SIZE_MAX {
            bail!("blob {oid} exceeds the ustar size field and the pax `size` record is not ported");
        }

        if !ext.is_empty() {
            let header = build_header(
                format!("{oid}.paxheader").as_bytes(),
                b"",
                b"",
                0o100666,
                ext.len() as u64,
                self.mtime,
                b'x',
            );
            self.raw(&header)?;
            self.payload(&ext)?;
        }

        let header = build_header(&name, &prefix, &link, mode, size, self.mtime, typeflag);
        self.raw(&header)?;
        if is_regular && !data.is_empty() {
            self.payload(data)?;
        }
        Ok(())
    }

    /// git's `write_trailer()`: zero-fill the rest of the current 10 KiB block
    /// and emit it, then emit one more zero block when that fill was shorter
    /// than the two 512-byte records the tar format ends with.
    fn finish(&mut self) -> Result<()> {
        let offset = self.written % BLOCK;
        let tail = BLOCK - offset;
        self.zeros(tail)?;
        if tail < 2 * RECORD as u64 {
            self.zeros(BLOCK)?;
        }
        self.out.flush()?;
        Ok(())
    }

    /// `count` NUL bytes.
    fn zeros(&mut self, count: u64) -> Result<()> {
        let mut left = count;
        while left > 0 {
            let n = left.min(RECORD as u64) as usize;
            self.raw(&ZEROS[..n])?;
            left -= n as u64;
        }
        Ok(())
    }
}

/// git's `get_path_prefix()`: the length of the `ustar` `prefix` field, i.e. the
/// offset of the last `/` at or before byte 155 (ignoring a trailing `/`).
fn ustar_prefix_len(path: &[u8]) -> usize {
    let mut i = path.len();
    if i > 1 && path[i - 1] == b'/' {
        i -= 1;
    }
    if i > PREFIX_MAX {
        i = PREFIX_MAX;
    }
    loop {
        i -= 1;
        if i == 0 || path[i] == b'/' {
            return i;
        }
    }
}

/// git's `strbuf_append_ext_header()`: a pax record `"<len> <keyword>=<value>\n"`
/// where `<len>` counts the record including its own decimal digits.
fn ext_record(keyword: &[u8], value: &[u8]) -> Vec<u8> {
    let mut len = 1 + 1 + keyword.len() + 1 + value.len() + 1;
    let mut tmp = 1usize;
    while len / 10 >= tmp {
        len += 1;
        tmp *= 10;
    }
    let mut out = format!("{len} ").into_bytes();
    out.extend_from_slice(keyword);
    out.push(b'=');
    out.extend_from_slice(value);
    out.push(b'\n');
    out
}

/// One 512-byte `ustar` header, laid out and checksummed exactly as git's
/// `prepare_header()` does: uid/gid 0, uname/gname `root`, `ustar\0` + `00`, and
/// a 7-digit checksum written over a field otherwise read as spaces.
fn build_header(
    name: &[u8],
    prefix: &[u8],
    link: &[u8],
    mode: u32,
    size: u64,
    mtime: i64,
    typeflag: u8,
) -> [u8; RECORD] {
    fn put(header: &mut [u8; RECORD], offset: usize, width: usize, value: &[u8]) {
        let n = value.len().min(width);
        header[offset..offset + n].copy_from_slice(&value[..n]);
    }

    let mut header = [0u8; RECORD];
    put(&mut header, 0, 100, name);
    put(&mut header, 100, 8, format!("{:07o}", mode & 0o7777).as_bytes());
    put(&mut header, 108, 8, b"0000000");
    put(&mut header, 116, 8, b"0000000");
    put(&mut header, 124, 12, format!("{size:011o}").as_bytes());
    put(&mut header, 136, 12, format!("{:011o}", mtime as u64).as_bytes());
    header[156] = typeflag;
    put(&mut header, 157, 100, link);
    put(&mut header, 257, 6, b"ustar\0");
    put(&mut header, 263, 2, b"00");
    put(&mut header, 265, 32, b"root");
    put(&mut header, 297, 32, b"root");
    put(&mut header, 329, 8, b"0000000");
    put(&mut header, 337, 8, b"0000000");
    put(&mut header, 345, 155, prefix);

    let checksum: u32 = header
        .iter()
        .enumerate()
        .map(|(i, b)| if (148..156).contains(&i) { 0x20 } else { u32::from(*b) })
        .sum();
    put(&mut header, 148, 8, format!("{checksum:07o}").as_bytes());
    header
}

/// git's in-process `gzip` filter for `--format=tgz` / `--format=tar.gz`: a
/// port of zlib's `deflate.c` and `trees.c`, driven the way `archive-tar.c`
/// drives it.
///
/// Byte-identical output is the whole point, so this is a transcription rather
/// than a reimplementation. Everything that is observable in the output is
/// reproduced: the `configuration_table` per level, `deflate_stored` /
/// `deflate_fast` / `deflate_slow`, `longest_match`'s unrolled eight-way
/// compare and its chain limits, the lazy-match `TOO_FAR` rule, the
/// dynamic-versus-static-versus-stored block decision, the bit-length overflow
/// repair in `gen_bitlen`, and the `high_water` window zeroing that makes
/// matches past the end of the data deterministic.
///
/// Two buffer sizes are load-bearing rather than incidental: `deflate_stored`
/// sizes its blocks from `avail_in` and `avail_out`, so the 10 KiB input blocks
/// and 16 KiB output buffer `archive-tar.c` uses are part of the format at
/// `-0`. The gzip wrapper matches `deflateSetHeader(&gzhead)` with git's
/// `{ .os = 3 }`: `MTIME` zero and `XFL` derived from the level.
///
/// The vendored `gix-zlib` cannot stand in for this. It is backed by `zlib-rs`,
/// a zlib-ng-lineage encoder whose deflate output differs from zlib's at every
/// level (measured: level 1 is larger, levels 2-8 smaller), and it exposes no
/// raw-deflate mode.
mod gzip {
    #![allow(clippy::needless_range_loop)]

    use std::io::{self, Write};

    const MAX_MATCH: usize = 258;
    const MIN_MATCH: usize = 3;
    const LENGTH_CODES: usize = 29;
    const LITERALS: usize = 256;
    const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
    const D_CODES: usize = 30;
    const BL_CODES: usize = 19;
    const HEAP_SIZE: usize = 2 * L_CODES + 1;
    const MAX_BITS: usize = 15;
    const MAX_BL_BITS: usize = 7;
    const BUF_SIZE: i32 = 16;
    const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
    const WIN_INIT: usize = MAX_MATCH;
    const END_BLOCK: usize = 256;
    const REP_3_6: usize = 16;
    const REPZ_3_10: usize = 17;
    const REPZ_11_138: usize = 18;
    const TOO_FAR: usize = 4096;
    const MAX_STORED: usize = 65535;

    /// `deflateInit2(..., windowBits = 15, memLevel = 8, ...)`, as git uses.
    const W_BITS: usize = 15;
    const W_SIZE: usize = 1 << W_BITS;
    const W_MASK: usize = W_SIZE - 1;
    const HASH_BITS: usize = 8 + 7;
    const HASH_SIZE: usize = 1 << HASH_BITS;
    const HASH_MASK: usize = HASH_SIZE - 1;
    const HASH_SHIFT: usize = (HASH_BITS + MIN_MATCH - 1) / MIN_MATCH;
    const LIT_BUFSIZE: usize = 1 << (8 + 6);
    const PENDING_BUF_SIZE: usize = LIT_BUFSIZE * 4;
    const SYM_END: usize = (LIT_BUFSIZE - 1) * 3;
    const WINDOW_SIZE: usize = 2 * W_SIZE;
    const MAX_DIST: usize = W_SIZE - MIN_LOOKAHEAD;

    /// git feeds the tar to zlib one 10 KiB `BLOCKSIZE` block at a time and drains
    /// into a 16 KiB `outbuf`. Both sizes are observable in the output at level 0,
    /// where `deflate_stored()` sizes its blocks from `avail_in`/`avail_out`.
    const IN_BLOCK: usize = 10240;
    const OUT_BUF: usize = 16384;

    const Z_NO_FLUSH: i32 = 0;
    const Z_FINISH: i32 = 4;
    const Z_OK: i32 = 0;
    const Z_STREAM_END: i32 = 1;
    const Z_BUF_ERROR: i32 = -5;

    static EXTRA_LBITS: [i32; LENGTH_CODES] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    static EXTRA_DBITS: [i32; D_CODES] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
        13,
    ];
    static EXTRA_BLBITS: [i32; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];
    const BL_ORDER: [usize; BL_CODES] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];

    /// `configuration_table`: good_length, max_lazy, nice_length, max_chain.
    const CONFIG: [(u16, u16, u16, u16); 10] = [
        (0, 0, 0, 0),
        (4, 4, 8, 4),
        (4, 5, 16, 8),
        (4, 6, 32, 32),
        (4, 4, 16, 16),
        (8, 16, 32, 32),
        (8, 16, 128, 128),
        (8, 32, 128, 256),
        (32, 128, 258, 1024),
        (32, 258, 258, 4096),
    ];

    /// zlib's `ct_data`: a union of (`freq`, `code`) and (`dad`, `len`).
    #[derive(Clone, Copy, Default)]
    struct Ct {
        fc: u16,
        dl: u16,
    }

    /// The three tree kinds, indexed the way `deflate_state` names them.
    const TREE_L: usize = 0;
    const TREE_D: usize = 1;
    const TREE_BL: usize = 2;

    /// `tr_static_init()`: the tables zlib computes once per process.
    struct Tables {
        static_ltree: Vec<Ct>,
        static_dtree: Vec<Ct>,
        dist_code: [u8; 512],
        length_code: [u8; 256],
        base_length: [i32; LENGTH_CODES],
        base_dist: [i32; D_CODES],
    }

    fn bi_reverse(mut code: u32, mut len: i32) -> u32 {
        let mut res = 0u32;
        loop {
            res |= code & 1;
            code >>= 1;
            res <<= 1;
            len -= 1;
            if len <= 0 {
                break;
            }
        }
        res >> 1
    }

    fn gen_codes(tree: &mut [Ct], max_code: i32, bl_count: &[u16; MAX_BITS + 1]) {
        let mut next_code = [0u16; MAX_BITS + 1];
        let mut code: u32 = 0;
        for bits in 1..=MAX_BITS {
            code = (code + u32::from(bl_count[bits - 1])) << 1;
            next_code[bits] = code as u16;
        }
        for n in 0..=max_code.max(-1) {
            let n = n as usize;
            let len = tree[n].dl as usize;
            if len == 0 {
                continue;
            }
            tree[n].fc = bi_reverse(u32::from(next_code[len]), len as i32) as u16;
            next_code[len] += 1;
        }
    }

    impl Tables {
        fn new() -> Self {
            let mut length_code = [0u8; 256];
            let mut base_length = [0i32; LENGTH_CODES];
            let mut length = 0usize;
            for code in 0..LENGTH_CODES - 1 {
                base_length[code] = length as i32;
                for _ in 0..(1 << EXTRA_LBITS[code]) {
                    length_code[length] = code as u8;
                    length += 1;
                }
            }
            // Match length 258 is cheaper as code 285 than as 284 + 5 extra bits.
            length_code[length - 1] = (LENGTH_CODES - 1) as u8;

            let mut dist_code = [0u8; 512];
            let mut base_dist = [0i32; D_CODES];
            let mut dist = 0usize;
            for code in 0..16 {
                base_dist[code] = dist as i32;
                for _ in 0..(1 << EXTRA_DBITS[code]) {
                    dist_code[dist] = code as u8;
                    dist += 1;
                }
            }
            dist >>= 7;
            for code in 16..D_CODES {
                base_dist[code] = (dist as i32) << 7;
                for _ in 0..(1 << (EXTRA_DBITS[code] - 7)) {
                    dist_code[256 + dist] = code as u8;
                    dist += 1;
                }
            }

            let mut bl_count = [0u16; MAX_BITS + 1];
            let mut static_ltree = vec![Ct::default(); L_CODES + 2];
            for n in 0..=143 {
                static_ltree[n].dl = 8;
                bl_count[8] += 1;
            }
            for n in 144..=255 {
                static_ltree[n].dl = 9;
                bl_count[9] += 1;
            }
            for n in 256..=279 {
                static_ltree[n].dl = 7;
                bl_count[7] += 1;
            }
            for n in 280..=287 {
                static_ltree[n].dl = 8;
                bl_count[8] += 1;
            }
            gen_codes(&mut static_ltree, (L_CODES + 1) as i32, &bl_count);

            let mut static_dtree = vec![Ct::default(); D_CODES];
            for n in 0..D_CODES {
                static_dtree[n].dl = 5;
                static_dtree[n].fc = bi_reverse(n as u32, 5) as u16;
            }

            Tables {
                static_ltree,
                static_dtree,
                dist_code,
                length_code,
                base_length,
                base_dist,
            }
        }

        /// zlib's `d_code()`.
        fn d_code(&self, dist: usize) -> usize {
            if dist < 256 {
                self.dist_code[dist] as usize
            } else {
                self.dist_code[256 + (dist >> 7)] as usize
            }
        }
    }

    /// `deflate_state` plus the parts of `z_stream` deflate actually reads.
    struct State {
        level: i32,
        status_gzip: bool,
        status_finish: bool,
        last_flush: i32,
        wrap: i32,

        window: Vec<u8>,
        prev: Vec<u16>,
        head: Vec<u16>,
        pending_buf: Vec<u8>,
        pending: usize,
        pending_out: usize,

        ins_h: usize,
        block_start: i64,
        match_length: usize,
        prev_match: usize,
        match_available: bool,
        strstart: usize,
        match_start: usize,
        lookahead: usize,
        prev_length: usize,
        max_chain_length: usize,
        max_lazy_match: usize,
        good_match: usize,
        nice_match: i32,
        insert: usize,
        matches: u32,
        high_water: usize,

        trees: [Vec<Ct>; 3],
        max_code: [i32; 3],
        bl_count: [u16; MAX_BITS + 1],
        heap: [i32; HEAP_SIZE],
        heap_len: usize,
        heap_max: usize,
        depth: [u8; HEAP_SIZE],
        sym_next: usize,
        opt_len: u64,
        static_len: u64,
        bi_buf: u16,
        bi_valid: i32,

        // z_stream
        avail_in: usize,
        next_in: usize,
        total_in: u64,
        avail_out: usize,
        next_out: usize,
        total_out: u64,
        crc: u32,
    }

    fn crc32_update(crc: u32, data: &[u8]) -> u32 {
        // Table-driven CRC-32 (IEEE), the polynomial zlib's crc32() uses.
        static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
        let table = TABLE.get_or_init(|| {
            let mut t = [0u32; 256];
            for n in 0..256usize {
                let mut c = n as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                }
                t[n] = c;
            }
            t
        });
        let mut c = !crc;
        for b in data {
            c = table[((c ^ u32::from(*b)) & 0xff) as usize] ^ (c >> 8);
        }
        !c
    }

    impl State {
        fn new(level: i32) -> Self {
            let level = if level == -1 { 6 } else { level };
            let cfg = CONFIG[level as usize];
            State {
                level,
                status_gzip: true,
                status_finish: false,
                last_flush: -2,
                wrap: 2,
                // The extra MAX_MATCH bytes keep `longest_match`'s scan in bounds;
                // zlib relies on the same slack past the window.
                window: vec![0; WINDOW_SIZE + MAX_MATCH],
                prev: vec![0; W_SIZE],
                head: vec![0; HASH_SIZE],
                pending_buf: vec![0; PENDING_BUF_SIZE],
                pending: 0,
                pending_out: 0,
                ins_h: 0,
                block_start: 0,
                match_length: MIN_MATCH - 1,
                prev_match: 0,
                match_available: false,
                strstart: 0,
                match_start: 0,
                lookahead: 0,
                prev_length: MIN_MATCH - 1,
                max_chain_length: cfg.3 as usize,
                max_lazy_match: cfg.1 as usize,
                good_match: cfg.0 as usize,
                nice_match: i32::from(cfg.2),
                insert: 0,
                matches: 0,
                high_water: 0,
                trees: [
                    vec![Ct::default(); HEAP_SIZE],
                    vec![Ct::default(); 2 * D_CODES + 1],
                    vec![Ct::default(); 2 * BL_CODES + 1],
                ],
                max_code: [0; 3],
                bl_count: [0; MAX_BITS + 1],
                heap: [0; HEAP_SIZE],
                heap_len: 0,
                heap_max: 0,
                depth: [0; HEAP_SIZE],
                sym_next: 0,
                opt_len: 0,
                static_len: 0,
                bi_buf: 0,
                bi_valid: 0,
                avail_in: 0,
                next_in: 0,
                total_in: 0,
                avail_out: OUT_BUF,
                next_out: 0,
                total_out: 0,
                crc: 0,
            }
        }

        fn init_block(&mut self) {
            for n in 0..L_CODES {
                self.trees[TREE_L][n].fc = 0;
            }
            for n in 0..D_CODES {
                self.trees[TREE_D][n].fc = 0;
            }
            for n in 0..BL_CODES {
                self.trees[TREE_BL][n].fc = 0;
            }
            self.trees[TREE_L][END_BLOCK].fc = 1;
            self.opt_len = 0;
            self.static_len = 0;
            self.sym_next = 0;
            self.matches = 0;
        }

        fn put_byte(&mut self, b: u8) {
            self.pending_buf[self.pending] = b;
            self.pending += 1;
        }

        fn put_short(&mut self, w: u16) {
            self.put_byte((w & 0xff) as u8);
            self.put_byte((w >> 8) as u8);
        }

        fn send_bits(&mut self, value: i32, len: i32) {
            if self.bi_valid > BUF_SIZE - len {
                let val = value as u16 as u32;
                self.bi_buf |= (val << self.bi_valid) as u16;
                let b = self.bi_buf;
                self.put_short(b);
                self.bi_buf = (val >> (BUF_SIZE - self.bi_valid)) as u16;
                self.bi_valid += len - BUF_SIZE;
            } else {
                self.bi_buf |= ((value as u16 as u32) << self.bi_valid) as u16;
                self.bi_valid += len;
            }
        }

        fn send_code(&mut self, c: usize, tree: usize) {
            let t = self.trees[tree][c];
            self.send_bits(i32::from(t.fc), i32::from(t.dl));
        }

        fn bi_flush(&mut self) {
            if self.bi_valid == 16 {
                let b = self.bi_buf;
                self.put_short(b);
                self.bi_buf = 0;
                self.bi_valid = 0;
            } else if self.bi_valid >= 8 {
                let b = self.bi_buf as u8;
                self.put_byte(b);
                self.bi_buf >>= 8;
                self.bi_valid -= 8;
            }
        }

        fn bi_windup(&mut self) {
            if self.bi_valid > 8 {
                let b = self.bi_buf;
                self.put_short(b);
            } else if self.bi_valid > 0 {
                let b = self.bi_buf as u8;
                self.put_byte(b);
            }
            self.bi_buf = 0;
            self.bi_valid = 0;
        }

        fn smaller(&self, k: usize, n: i32, m: i32) -> bool {
            let t = &self.trees[k];
            let (n, m) = (n as usize, m as usize);
            t[n].fc < t[m].fc || (t[n].fc == t[m].fc && self.depth[n] <= self.depth[m])
        }

        fn pqdownheap(&mut self, k: usize, mut node: usize) {
            let v = self.heap[node];
            let mut j = node << 1;
            while j <= self.heap_len {
                if j < self.heap_len && self.smaller(k, self.heap[j + 1], self.heap[j]) {
                    j += 1;
                }
                if self.smaller(k, v, self.heap[j]) {
                    break;
                }
                self.heap[node] = self.heap[j];
                node = j;
                j <<= 1;
            }
            self.heap[node] = v;
        }

        /// The `static_tree`, `extra_bits`, `extra_base` and `max_length` of a tree.
        fn desc<'t>(&self, t: &'t Tables, k: usize) -> (Option<&'t [Ct]>, &'static [i32], usize, usize) {
            match k {
                TREE_L => (
                    Some(&t.static_ltree),
                    &EXTRA_LBITS,
                    LITERALS + 1,
                    MAX_BITS,
                ),
                TREE_D => (Some(&t.static_dtree), &EXTRA_DBITS, 0, MAX_BITS),
                _ => (None, &EXTRA_BLBITS, 0, MAX_BL_BITS),
            }
        }

        fn elems(k: usize) -> usize {
            match k {
                TREE_L => L_CODES,
                TREE_D => D_CODES,
                _ => BL_CODES,
            }
        }

        fn gen_bitlen(&mut self, t: &Tables, k: usize) {
            let (stree, extra, base, max_length) = self.desc(t, k);
            let max_code = self.max_code[k];
            let mut overflow = 0i32;

            self.bl_count = [0; MAX_BITS + 1];

            let root = self.heap[self.heap_max] as usize;
            self.trees[k][root].dl = 0;

            for h in self.heap_max + 1..HEAP_SIZE {
                let n = self.heap[h] as usize;
                let dad = self.trees[k][n].dl as usize;
                let mut bits = self.trees[k][dad].dl as usize + 1;
                if bits > max_length {
                    bits = max_length;
                    overflow += 1;
                }
                self.trees[k][n].dl = bits as u16;

                if n as i32 > max_code {
                    continue;
                }
                self.bl_count[bits] += 1;
                let mut xbits = 0i32;
                if n >= base {
                    xbits = extra[n - base];
                }
                let f = u64::from(self.trees[k][n].fc);
                self.opt_len = self.opt_len.wrapping_add(f * (bits as u64 + xbits as u64));
                if let Some(s) = stree {
                    self.static_len = self
                        .static_len
                        .wrapping_add(f * (u64::from(s[n].dl) + xbits as u64));
                }
            }
            if overflow == 0 {
                return;
            }

            loop {
                let mut bits = max_length - 1;
                while self.bl_count[bits] == 0 {
                    bits -= 1;
                }
                self.bl_count[bits] -= 1;
                self.bl_count[bits + 1] += 2;
                self.bl_count[max_length] -= 1;
                overflow -= 2;
                if overflow <= 0 {
                    break;
                }
            }

            let mut h = HEAP_SIZE;
            for bits in (1..=max_length).rev() {
                let mut n = self.bl_count[bits];
                while n != 0 {
                    h -= 1;
                    let m = self.heap[h] as usize;
                    if m as i32 > max_code {
                        continue;
                    }
                    if usize::from(self.trees[k][m].dl) != bits {
                        self.opt_len = self.opt_len.wrapping_add(
                            (bits as u64)
                                .wrapping_sub(u64::from(self.trees[k][m].dl))
                                .wrapping_mul(u64::from(self.trees[k][m].fc)),
                        );
                        self.trees[k][m].dl = bits as u16;
                    }
                    n -= 1;
                }
            }
        }

        fn build_tree(&mut self, t: &Tables, k: usize) {
            let (stree, _, _, _) = self.desc(t, k);
            let elems = Self::elems(k);
            let mut max_code: i32 = -1;

            self.heap_len = 0;
            self.heap_max = HEAP_SIZE;

            for n in 0..elems {
                if self.trees[k][n].fc != 0 {
                    self.heap_len += 1;
                    self.heap[self.heap_len] = n as i32;
                    max_code = n as i32;
                    self.depth[n] = 0;
                } else {
                    self.trees[k][n].dl = 0;
                }
            }

            while self.heap_len < 2 {
                self.heap_len += 1;
                let node = if max_code < 2 {
                    max_code += 1;
                    max_code as usize
                } else {
                    0
                };
                self.heap[self.heap_len] = node as i32;
                self.trees[k][node].fc = 1;
                self.depth[node] = 0;
                self.opt_len = self.opt_len.wrapping_sub(1);
                if let Some(s) = stree {
                    self.static_len = self.static_len.wrapping_sub(u64::from(s[node].dl));
                }
            }
            self.max_code[k] = max_code;

            for n in (1..=self.heap_len / 2).rev() {
                self.pqdownheap(k, n);
            }

            let mut node = elems;
            loop {
                // pqremove
                let n = self.heap[1];
                self.heap[1] = self.heap[self.heap_len];
                self.heap_len -= 1;
                self.pqdownheap(k, 1);

                let m = self.heap[1];

                self.heap_max -= 1;
                self.heap[self.heap_max] = n;
                self.heap_max -= 1;
                self.heap[self.heap_max] = m;

                let (nu, mu) = (n as usize, m as usize);
                self.trees[k][node].fc = self.trees[k][nu].fc + self.trees[k][mu].fc;
                self.depth[node] = self.depth[nu].max(self.depth[mu]) + 1;
                self.trees[k][nu].dl = node as u16;
                self.trees[k][mu].dl = node as u16;

                self.heap[1] = node as i32;
                node += 1;
                self.pqdownheap(k, 1);

                if self.heap_len < 2 {
                    break;
                }
            }

            self.heap_max -= 1;
            self.heap[self.heap_max] = self.heap[1];

            self.gen_bitlen(t, k);
            let max_code = self.max_code[k];
            let bl = self.bl_count;
            gen_codes(&mut self.trees[k], max_code, &bl);
        }

        fn scan_tree(&mut self, k: usize) {
            let max_code = self.max_code[k];
            let mut prevlen: i32 = -1;
            let mut nextlen = self.trees[k][0].dl as i32;
            let mut count = 0i32;
            let mut max_count = 7i32;
            let mut min_count = 4i32;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            }
            self.trees[k][(max_code + 1) as usize].dl = 0xffff;

            for n in 0..=max_code {
                let curlen = nextlen;
                nextlen = self.trees[k][(n + 1) as usize].dl as i32;
                count += 1;
                if count < max_count && curlen == nextlen {
                    continue;
                } else if count < min_count {
                    self.trees[TREE_BL][curlen as usize].fc += count as u16;
                } else if curlen != 0 {
                    if curlen != prevlen {
                        self.trees[TREE_BL][curlen as usize].fc += 1;
                    }
                    self.trees[TREE_BL][REP_3_6].fc += 1;
                } else if count <= 10 {
                    self.trees[TREE_BL][REPZ_3_10].fc += 1;
                } else {
                    self.trees[TREE_BL][REPZ_11_138].fc += 1;
                }
                count = 0;
                prevlen = curlen;
                if nextlen == 0 {
                    max_count = 138;
                    min_count = 3;
                } else if curlen == nextlen {
                    max_count = 6;
                    min_count = 3;
                } else {
                    max_count = 7;
                    min_count = 4;
                }
            }
        }

        fn send_tree(&mut self, k: usize) {
            let max_code = self.max_code[k];
            let mut prevlen: i32 = -1;
            let mut nextlen = self.trees[k][0].dl as i32;
            let mut count = 0i32;
            let mut max_count = 7i32;
            let mut min_count = 4i32;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            }

            for n in 0..=max_code {
                let curlen = nextlen;
                nextlen = self.trees[k][(n + 1) as usize].dl as i32;
                count += 1;
                if count < max_count && curlen == nextlen {
                    continue;
                } else if count < min_count {
                    loop {
                        self.send_code(curlen as usize, TREE_BL);
                        count -= 1;
                        if count == 0 {
                            break;
                        }
                    }
                } else if curlen != 0 {
                    if curlen != prevlen {
                        self.send_code(curlen as usize, TREE_BL);
                        count -= 1;
                    }
                    self.send_code(REP_3_6, TREE_BL);
                    self.send_bits(count - 3, 2);
                } else if count <= 10 {
                    self.send_code(REPZ_3_10, TREE_BL);
                    self.send_bits(count - 3, 3);
                } else {
                    self.send_code(REPZ_11_138, TREE_BL);
                    self.send_bits(count - 11, 7);
                }
                count = 0;
                prevlen = curlen;
                if nextlen == 0 {
                    max_count = 138;
                    min_count = 3;
                } else if curlen == nextlen {
                    max_count = 6;
                    min_count = 3;
                } else {
                    max_count = 7;
                    min_count = 4;
                }
            }
        }

        fn build_bl_tree(&mut self, t: &Tables) -> usize {
            self.scan_tree(TREE_L);
            self.scan_tree(TREE_D);
            self.build_tree(t, TREE_BL);

            let mut max_blindex = BL_CODES - 1;
            while max_blindex >= 3 {
                if self.trees[TREE_BL][BL_ORDER[max_blindex]].dl != 0 {
                    break;
                }
                max_blindex -= 1;
            }
            self.opt_len = self
                .opt_len
                .wrapping_add(3 * (max_blindex as u64 + 1) + 5 + 5 + 4);
            max_blindex
        }

        fn send_all_trees(&mut self, lcodes: usize, dcodes: usize, blcodes: usize) {
            self.send_bits(lcodes as i32 - 257, 5);
            self.send_bits(dcodes as i32 - 1, 5);
            self.send_bits(blcodes as i32 - 4, 4);
            for rank in 0..blcodes {
                let len = self.trees[TREE_BL][BL_ORDER[rank]].dl;
                self.send_bits(i32::from(len), 3);
            }
            self.max_code[TREE_L] = lcodes as i32 - 1;
            self.send_tree(TREE_L);
            self.max_code[TREE_D] = dcodes as i32 - 1;
            self.send_tree(TREE_D);
        }

        fn tr_stored_block(&mut self, buf: Option<usize>, stored_len: usize, last: bool) {
            self.send_bits(((0) << 1) + i32::from(last), 3);
            self.bi_windup();
            self.put_short(stored_len as u16);
            self.put_short(!(stored_len as u16));
            if stored_len != 0 {
                let start = buf.expect("stored block without a buffer");
                let (dst, src) = (self.pending, start);
                for i in 0..stored_len {
                    self.pending_buf[dst + i] = self.window[src + i];
                }
            }
            self.pending += stored_len;
        }

        /// One entry of the literal/length or the distance tree, taken from the
        /// dynamic trees or from the static ones depending on what the block header
        /// announced.
        fn send_sym(&mut self, t: &Tables, c: usize, dynamic: bool, dist_tree: bool) {
            let cd = if dynamic {
                self.trees[if dist_tree { TREE_D } else { TREE_L }][c]
            } else if dist_tree {
                t.static_dtree[c]
            } else {
                t.static_ltree[c]
            };
            self.send_bits(i32::from(cd.fc), i32::from(cd.dl));
        }

        fn compress_block(&mut self, t: &Tables, dynamic: bool) {
            let mut sx = 0usize;
            if self.sym_next != 0 {
                loop {
                    let mut dist = usize::from(self.pending_buf[LIT_BUFSIZE + sx]);
                    sx += 1;
                    dist += usize::from(self.pending_buf[LIT_BUFSIZE + sx]) << 8;
                    sx += 1;
                    let lc = self.pending_buf[LIT_BUFSIZE + sx];
                    sx += 1;
                    if dist == 0 {
                        self.send_sym(t, lc as usize, dynamic, false);
                    } else {
                        let mut lc = i32::from(lc);
                        let code = usize::from(t.length_code[lc as usize]);
                        self.send_sym(t, code + LITERALS + 1, dynamic, false);
                        let extra = EXTRA_LBITS[code];
                        if extra != 0 {
                            lc -= t.base_length[code];
                            self.send_bits(lc, extra);
                        }
                        let mut dist = dist - 1;
                        let code = t.d_code(dist);
                        self.send_sym(t, code, dynamic, true);
                        let extra = EXTRA_DBITS[code];
                        if extra != 0 {
                            dist -= t.base_dist[code] as usize;
                            self.send_bits(dist as i32, extra);
                        }
                    }
                    if sx >= self.sym_next {
                        break;
                    }
                }
            }
            self.send_sym(t, END_BLOCK, dynamic, false);
        }

        fn tr_flush_block(&mut self, t: &Tables, buf: Option<usize>, stored_len: usize, last: bool) {
            let opt_lenb;
            let static_lenb;
            let mut max_blindex = 0usize;

            if self.level > 0 {
                self.build_tree(t, TREE_L);
                self.build_tree(t, TREE_D);
                max_blindex = self.build_bl_tree(t);

                let mut o = (self.opt_len.wrapping_add(3 + 7)) >> 3;
                let s = (self.static_len.wrapping_add(3 + 7)) >> 3;
                if s <= o {
                    o = s;
                }
                opt_lenb = o;
                static_lenb = s;
            } else {
                opt_lenb = stored_len as u64 + 5;
                static_lenb = opt_lenb;
            }

            if stored_len as u64 + 4 <= opt_lenb && buf.is_some() {
                self.tr_stored_block(buf, stored_len, last);
            } else if static_lenb == opt_lenb {
                self.send_bits((1 << 1) + i32::from(last), 3);
                self.compress_block(t, false);
            } else {
                self.send_bits((2 << 1) + i32::from(last), 3);
                let (lmax, dmax) = (self.max_code[TREE_L], self.max_code[TREE_D]);
                self.send_all_trees((lmax + 1) as usize, (dmax + 1) as usize, max_blindex + 1);
                self.compress_block(t, true);
            }

            self.init_block();
            if last {
                self.bi_windup();
            }
        }

        /// `_tr_tally()`: record one symbol, returning true when the block is full.
        fn tr_tally(&mut self, t: &Tables, dist: usize, lc: usize) -> bool {
            self.pending_buf[LIT_BUFSIZE + self.sym_next] = dist as u8;
            self.sym_next += 1;
            self.pending_buf[LIT_BUFSIZE + self.sym_next] = (dist >> 8) as u8;
            self.sym_next += 1;
            self.pending_buf[LIT_BUFSIZE + self.sym_next] = lc as u8;
            self.sym_next += 1;
            if dist == 0 {
                self.trees[TREE_L][lc].fc += 1;
            } else {
                self.matches += 1;
                let d = dist - 1;
                let li = usize::from(t.length_code[lc]) + LITERALS + 1;
                self.trees[TREE_L][li].fc += 1;
                let di = t.d_code(d);
                self.trees[TREE_D][di].fc += 1;
            }
            self.sym_next == SYM_END
        }

        fn slide_hash(&mut self) {
            for p in self.head.iter_mut() {
                let m = *p as usize;
                *p = if m >= W_SIZE { (m - W_SIZE) as u16 } else { 0 };
            }
            for p in self.prev.iter_mut() {
                let m = *p as usize;
                *p = if m >= W_SIZE { (m - W_SIZE) as u16 } else { 0 };
            }
        }

        fn update_hash(&mut self, c: u8) {
            self.ins_h = ((self.ins_h << HASH_SHIFT) ^ usize::from(c)) & HASH_MASK;
        }

        /// `INSERT_STRING`, returning the previous head of the chain.
        fn insert_string(&mut self, str_: usize) -> usize {
            let c = self.window[str_ + MIN_MATCH - 1];
            self.update_hash(c);
            let head = self.head[self.ins_h];
            self.prev[str_ & W_MASK] = head;
            self.head[self.ins_h] = str_ as u16;
            head as usize
        }

        fn read_buf_into_window(&mut self, input: &[u8], at: usize, size: usize) -> usize {
            let mut len = self.avail_in;
            if len > size {
                len = size;
            }
            if len == 0 {
                return 0;
            }
            self.avail_in -= len;
            let src = &input[self.next_in..self.next_in + len];
            self.window[at..at + len].copy_from_slice(src);
            self.crc = crc32_update(self.crc, src);
            self.next_in += len;
            self.total_in += len as u64;
            len
        }

        fn read_buf_into_out(&mut self, input: &[u8], out: &mut [u8], size: usize) -> usize {
            let mut len = self.avail_in;
            if len > size {
                len = size;
            }
            if len == 0 {
                return 0;
            }
            self.avail_in -= len;
            let src = &input[self.next_in..self.next_in + len];
            out[self.next_out..self.next_out + len].copy_from_slice(src);
            self.crc = crc32_update(self.crc, src);
            self.next_in += len;
            self.total_in += len as u64;
            len
        }

        fn flush_pending(&mut self, out: &mut [u8]) {
            self.bi_flush();
            let mut len = self.pending;
            if len > self.avail_out {
                len = self.avail_out;
            }
            if len == 0 {
                return;
            }
            out[self.next_out..self.next_out + len]
                .copy_from_slice(&self.pending_buf[self.pending_out..self.pending_out + len]);
            self.next_out += len;
            self.pending_out += len;
            self.total_out += len as u64;
            self.avail_out -= len;
            self.pending -= len;
            if self.pending == 0 {
                self.pending_out = 0;
            }
        }

        fn fill_window(&mut self, input: &[u8]) {
            loop {
                let mut more = WINDOW_SIZE - self.lookahead - self.strstart;

                if self.strstart >= W_SIZE + MAX_DIST {
                    self.window.copy_within(W_SIZE..2 * W_SIZE - more, 0);
                    self.match_start -= W_SIZE;
                    self.strstart -= W_SIZE;
                    self.block_start -= W_SIZE as i64;
                    if self.insert > self.strstart {
                        self.insert = self.strstart;
                    }
                    self.slide_hash();
                    more += W_SIZE;
                }
                if self.avail_in == 0 {
                    break;
                }

                let at = self.strstart + self.lookahead;
                let n = self.read_buf_into_window(input, at, more);
                self.lookahead += n;

                if self.lookahead + self.insert >= MIN_MATCH {
                    let mut str_ = self.strstart - self.insert;
                    self.ins_h = usize::from(self.window[str_]);
                    let c = self.window[str_ + 1];
                    self.update_hash(c);
                    while self.insert != 0 {
                        let c = self.window[str_ + MIN_MATCH - 1];
                        self.update_hash(c);
                        self.prev[str_ & W_MASK] = self.head[self.ins_h];
                        self.head[self.ins_h] = str_ as u16;
                        str_ += 1;
                        self.insert -= 1;
                        if self.lookahead + self.insert < MIN_MATCH {
                            break;
                        }
                    }
                }

                if !(self.lookahead < MIN_LOOKAHEAD && self.avail_in != 0) {
                    break;
                }
            }

            if self.high_water < WINDOW_SIZE {
                let curr = self.strstart + self.lookahead;
                if self.high_water < curr {
                    let mut init = WINDOW_SIZE - curr;
                    if init > WIN_INIT {
                        init = WIN_INIT;
                    }
                    self.window[curr..curr + init].fill(0);
                    self.high_water = curr + init;
                } else if self.high_water < curr + WIN_INIT {
                    let mut init = curr + WIN_INIT - self.high_water;
                    if init > WINDOW_SIZE - self.high_water {
                        init = WINDOW_SIZE - self.high_water;
                    }
                    let hw = self.high_water;
                    self.window[hw..hw + init].fill(0);
                    self.high_water += init;
                }
            }
        }

        fn longest_match(&mut self, mut cur_match: usize) -> usize {
            let mut chain_length = self.max_chain_length;
            let scan = self.strstart;
            let mut best_len = self.prev_length;
            let mut nice_match = self.nice_match as usize;
            let limit = if self.strstart > MAX_DIST {
                self.strstart - MAX_DIST
            } else {
                0
            };

            let strend = self.strstart + MAX_MATCH;
            let mut scan_end1 = self.window[scan + best_len - 1];
            let mut scan_end = self.window[scan + best_len];

            if self.prev_length >= self.good_match {
                chain_length >>= 2;
            }
            if nice_match > self.lookahead {
                nice_match = self.lookahead;
            }

            loop {
                let m = cur_match;
                if self.window[m + best_len] == scan_end
                    && self.window[m + best_len - 1] == scan_end1
                    && self.window[m] == self.window[scan]
                    && self.window[m + 1] == self.window[scan + 1]
                {
                    // zlib compares in unrolled groups of eight and only then
                    // rechecks `scan < strend`; MAX_MATCH-2 is a multiple of 8, so
                    // the scan lands exactly on `strend` when everything matches.
                    let mut sp = scan + 2;
                    let mut mp = m + 2;
                    'outer: loop {
                        for _ in 0..8 {
                            sp += 1;
                            mp += 1;
                            if self.window[sp] != self.window[mp] {
                                break 'outer;
                            }
                        }
                        if sp >= strend {
                            break;
                        }
                    }
                    let len = MAX_MATCH - (strend - sp);

                    if len > best_len {
                        self.match_start = cur_match;
                        best_len = len;
                        if len >= nice_match {
                            break;
                        }
                        scan_end1 = self.window[scan + best_len - 1];
                        scan_end = self.window[scan + best_len];
                    }
                }
                cur_match = self.prev[cur_match & W_MASK] as usize;
                chain_length -= 1;
                if cur_match <= limit || chain_length == 0 {
                    break;
                }
            }

            if best_len <= self.lookahead {
                best_len
            } else {
                self.lookahead
            }
        }
    }

    /// `deflate()`'s block-function return codes.
    #[derive(PartialEq, Clone, Copy)]
    enum BState {
        NeedMore,
        BlockDone,
        FinishStarted,
        FinishDone,
    }

    pub struct GzDeflate<W: Write> {
        sink: W,
        state: State,
        tables: Tables,
        out: Vec<u8>,
        block: Vec<u8>,
    }

    impl<W: Write> GzDeflate<W> {
        pub fn new(sink: W, level: i32) -> Self {
            let mut state = State::new(level);
            state.init_block();
            GzDeflate {
                sink,
                state,
                tables: Tables::new(),
                out: vec![0; OUT_BUF],
                block: Vec::with_capacity(IN_BLOCK),
            }
        }

        /// git's `tgz_deflate()`: run `deflate()` until the input is drained,
        /// draining the 16 KiB output buffer to the sink whenever it fills.
        fn run(&mut self, input: &[u8], flush: i32) -> io::Result<()> {
            self.state.avail_in = input.len();
            self.state.next_in = 0;
            loop {
                if self.state.avail_in == 0 && flush != Z_FINISH {
                    break;
                }
                let status = self.deflate(input, flush);
                if self.state.avail_out == 0 || status == Z_STREAM_END {
                    let n = self.state.next_out;
                    self.sink.write_all(&self.out[..n])?;
                    self.state.next_out = 0;
                    self.state.avail_out = OUT_BUF;
                    if status == Z_STREAM_END {
                        break;
                    }
                }
                if status != Z_OK && status != Z_BUF_ERROR {
                    return Err(io::Error::other(format!("deflate error ({status})")));
                }
            }
            Ok(())
        }

        fn deflate(&mut self, input: &[u8], flush: i32) -> i32 {
            let s = &mut self.state;
            if s.avail_out == 0 {
                return Z_BUF_ERROR;
            }
            let old_flush = s.last_flush;
            s.last_flush = flush;

            if s.pending != 0 {
                let out = &mut self.out;
                s.flush_pending(out);
                if s.avail_out == 0 {
                    s.last_flush = -1;
                    return Z_OK;
                }
            } else if s.avail_in == 0 && flush <= old_flush && flush != Z_FINISH {
                return Z_BUF_ERROR;
            }

            if s.status_finish && s.avail_in != 0 {
                return Z_BUF_ERROR;
            }

            if s.status_gzip {
                s.crc = 0;
                s.put_byte(31);
                s.put_byte(139);
                s.put_byte(8);
                s.put_byte(0); // no extra, name, comment or header CRC
                for _ in 0..4 {
                    s.put_byte(0); // gzhead.time == 0
                }
                let xfl = if s.level == 9 {
                    2
                } else if s.level < 2 {
                    4
                } else {
                    0
                };
                s.put_byte(xfl);
                s.put_byte(3); // gzhead.os == 3 (Unix), as git sets it
                s.status_gzip = false;
                let out = &mut self.out;
                s.flush_pending(out);
                if s.pending != 0 {
                    s.last_flush = -1;
                    return Z_OK;
                }
            }

            if s.avail_in != 0 || s.lookahead != 0 || (flush != Z_NO_FLUSH && !s.status_finish) {
                let bstate = if s.level == 0 {
                    deflate_stored(s, &self.tables, input, &mut self.out, flush)
                } else if s.level <= 3 {
                    deflate_fast(s, &self.tables, input, &mut self.out, flush)
                } else {
                    deflate_slow(s, &self.tables, input, &mut self.out, flush)
                };

                if bstate == BState::FinishStarted || bstate == BState::FinishDone {
                    s.status_finish = true;
                }
                if bstate == BState::NeedMore || bstate == BState::FinishStarted {
                    if s.avail_out == 0 {
                        s.last_flush = -1;
                    }
                    return Z_OK;
                }
                if bstate == BState::BlockDone {
                    // Only Z_NO_FLUSH and Z_FINISH reach this port.
                    let out = &mut self.out;
                    s.flush_pending(out);
                    if s.avail_out == 0 {
                        s.last_flush = -1;
                        return Z_OK;
                    }
                }
            }

            if flush != Z_FINISH {
                return Z_OK;
            }
            if s.wrap <= 0 {
                return Z_STREAM_END;
            }

            let crc = s.crc;
            let total = s.total_in as u32;
            for shift in [0, 8, 16, 24] {
                s.put_byte((crc >> shift) as u8);
            }
            for shift in [0, 8, 16, 24] {
                s.put_byte((total >> shift) as u8);
            }
            let out = &mut self.out;
            s.flush_pending(out);
            s.wrap = -s.wrap;
            if s.pending != 0 {
                Z_OK
            } else {
                Z_STREAM_END
            }
        }

        /// Finish the stream and return the sink.
        pub fn finish(mut self) -> io::Result<W> {
            if !self.block.is_empty() {
                let block = std::mem::take(&mut self.block);
                self.run(&block, Z_NO_FLUSH)?;
            }
            self.run(&[], Z_FINISH)?;
            self.sink.flush()?;
            Ok(self.sink)
        }

    }

    impl<W: Write> Write for GzDeflate<W> {
        fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
            let total = buf.len();
            while !buf.is_empty() {
                let want = IN_BLOCK - self.block.len();
                let take = want.min(buf.len());
                self.block.extend_from_slice(&buf[..take]);
                buf = &buf[take..];
                if self.block.len() == IN_BLOCK {
                    let block = std::mem::take(&mut self.block);
                    self.run(&block, Z_NO_FLUSH)?;
                    self.block = block;
                    self.block.clear();
                }
            }
            Ok(total)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn flush_block_only(
        s: &mut State,
        t: &Tables,
        out: &mut [u8],
        last: bool,
    ) {
        let buf = if s.block_start >= 0 {
            Some(s.block_start as usize)
        } else {
            None
        };
        let len = (s.strstart as i64 - s.block_start) as usize;
        s.tr_flush_block(t, buf, len, last);
        s.block_start = s.strstart as i64;
        s.flush_pending(out);
    }

    fn deflate_fast(
        s: &mut State,
        t: &Tables,
        input: &[u8],
        out: &mut [u8],
        flush: i32,
    ) -> BState {
        loop {
            if s.lookahead < MIN_LOOKAHEAD {
                s.fill_window(input);
                if s.lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH {
                    return BState::NeedMore;
                }
                if s.lookahead == 0 {
                    break;
                }
            }

            let mut hash_head = 0usize;
            if s.lookahead >= MIN_MATCH {
                hash_head = s.insert_string(s.strstart);
            }

            if hash_head != 0 && s.strstart - hash_head <= MAX_DIST {
                s.match_length = s.longest_match(hash_head);
            }

            let bflush;
            if s.match_length >= MIN_MATCH {
                let dist = s.strstart - s.match_start;
                let lc = s.match_length - MIN_MATCH;
                bflush = s.tr_tally(t, dist, lc);
                s.lookahead -= s.match_length;

                if s.match_length <= s.max_lazy_match && s.lookahead >= MIN_MATCH {
                    s.match_length -= 1;
                    loop {
                        s.strstart += 1;
                        s.insert_string(s.strstart);
                        s.match_length -= 1;
                        if s.match_length == 0 {
                            break;
                        }
                    }
                    s.strstart += 1;
                } else {
                    s.strstart += s.match_length;
                    s.match_length = 0;
                    s.ins_h = usize::from(s.window[s.strstart]);
                    let c = s.window[s.strstart + 1];
                    s.update_hash(c);
                }
            } else {
                let lit = usize::from(s.window[s.strstart]);
                bflush = s.tr_tally(t, 0, lit);
                s.lookahead -= 1;
                s.strstart += 1;
            }
            if bflush {
                flush_block_only(s, t, out, false);
                if s.avail_out == 0 {
                    return BState::NeedMore;
                }
            }
        }
        s.insert = if s.strstart < MIN_MATCH - 1 {
            s.strstart
        } else {
            MIN_MATCH - 1
        };
        if flush == Z_FINISH {
            flush_block_only(s, t, out, true);
            if s.avail_out == 0 {
                return BState::FinishStarted;
            }
            return BState::FinishDone;
        }
        if s.sym_next != 0 {
            flush_block_only(s, t, out, false);
            if s.avail_out == 0 {
                return BState::NeedMore;
            }
        }
        BState::BlockDone
    }

    fn deflate_slow(
        s: &mut State,
        t: &Tables,
        input: &[u8],
        out: &mut [u8],
        flush: i32,
    ) -> BState {
        loop {
            if s.lookahead < MIN_LOOKAHEAD {
                s.fill_window(input);
                if s.lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH {
                    return BState::NeedMore;
                }
                if s.lookahead == 0 {
                    break;
                }
            }

            let mut hash_head = 0usize;
            if s.lookahead >= MIN_MATCH {
                hash_head = s.insert_string(s.strstart);
            }

            s.prev_length = s.match_length;
            s.prev_match = s.match_start;
            s.match_length = MIN_MATCH - 1;

            if hash_head != 0 && s.prev_length < s.max_lazy_match && s.strstart - hash_head <= MAX_DIST
            {
                s.match_length = s.longest_match(hash_head);
                if s.match_length <= 5
                    && s.match_length == MIN_MATCH
                    && s.strstart - s.match_start > TOO_FAR
                {
                    s.match_length = MIN_MATCH - 1;
                }
            }

            if s.prev_length >= MIN_MATCH && s.match_length <= s.prev_length {
                let max_insert = s.strstart + s.lookahead - MIN_MATCH;
                let dist = s.strstart - 1 - s.prev_match;
                let lc = s.prev_length - MIN_MATCH;
                let bflush = s.tr_tally(t, dist, lc);

                s.lookahead -= s.prev_length - 1;
                s.prev_length -= 2;
                loop {
                    s.strstart += 1;
                    if s.strstart <= max_insert {
                        s.insert_string(s.strstart);
                    }
                    s.prev_length -= 1;
                    if s.prev_length == 0 {
                        break;
                    }
                }
                s.match_available = false;
                s.match_length = MIN_MATCH - 1;
                s.strstart += 1;

                if bflush {
                    flush_block_only(s, t, out, false);
                    if s.avail_out == 0 {
                        return BState::NeedMore;
                    }
                }
            } else if s.match_available {
                let lit = usize::from(s.window[s.strstart - 1]);
                let bflush = s.tr_tally(t, 0, lit);
                if bflush {
                    flush_block_only(s, t, out, false);
                }
                s.strstart += 1;
                s.lookahead -= 1;
                if s.avail_out == 0 {
                    return BState::NeedMore;
                }
            } else {
                s.match_available = true;
                s.strstart += 1;
                s.lookahead -= 1;
            }
        }

        if s.match_available {
            let lit = usize::from(s.window[s.strstart - 1]);
            s.tr_tally(t, 0, lit);
            s.match_available = false;
        }
        s.insert = if s.strstart < MIN_MATCH - 1 {
            s.strstart
        } else {
            MIN_MATCH - 1
        };
        if flush == Z_FINISH {
            flush_block_only(s, t, out, true);
            if s.avail_out == 0 {
                return BState::FinishStarted;
            }
            return BState::FinishDone;
        }
        if s.sym_next != 0 {
            flush_block_only(s, t, out, false);
            if s.avail_out == 0 {
                return BState::NeedMore;
            }
        }
        BState::BlockDone
    }

    fn deflate_stored(
        s: &mut State,
        _t: &Tables,
        input: &[u8],
        out: &mut [u8],
        flush: i32,
    ) -> BState {
        let mut min_block = (PENDING_BUF_SIZE - 5).min(W_SIZE);

        let mut last = false;
        let used = s.avail_in;
        loop {
            let mut len = MAX_STORED;
            let mut have = ((s.bi_valid + 42) >> 3) as usize;
            if s.avail_out < have {
                break;
            }
            have = s.avail_out - have;
            let mut left = (s.strstart as i64 - s.block_start) as usize;
            if len > left + s.avail_in {
                len = left + s.avail_in;
            }
            if len > have {
                len = have;
            }

            if len < min_block
                && ((len == 0 && flush != Z_FINISH)
                    || flush == Z_NO_FLUSH
                    || len != left + s.avail_in)
            {
                break;
            }

            last = flush == Z_FINISH && len == left + s.avail_in;
            s.tr_stored_block(None, 0, last);

            s.pending_buf[s.pending - 4] = len as u8;
            s.pending_buf[s.pending - 3] = (len >> 8) as u8;
            s.pending_buf[s.pending - 2] = !(len as u8);
            s.pending_buf[s.pending - 1] = !((len >> 8) as u8);

            s.flush_pending(out);

            if left != 0 {
                if left > len {
                    left = len;
                }
                let from = s.block_start as usize;
                out[s.next_out..s.next_out + left].copy_from_slice(&s.window[from..from + left]);
                s.next_out += left;
                s.avail_out -= left;
                s.total_out += left as u64;
                s.block_start += left as i64;
                len -= left;
            }
            if len != 0 {
                let n = s.read_buf_into_out(input, out, len);
                s.next_out += n;
                s.avail_out -= n;
                s.total_out += n as u64;
            }
            if last {
                break;
            }
        }

        let used = used - s.avail_in;
        if used != 0 {
            if used >= W_SIZE {
                s.matches = 2;
                let start = s.next_in - W_SIZE;
                s.window[..W_SIZE].copy_from_slice(&input[start..start + W_SIZE]);
                s.strstart = W_SIZE;
                s.insert = s.strstart;
            } else {
                if WINDOW_SIZE - s.strstart <= used {
                    s.strstart -= W_SIZE;
                    s.window.copy_within(W_SIZE..W_SIZE + s.strstart, 0);
                    if s.matches < 2 {
                        s.matches += 1;
                    }
                    if s.insert > s.strstart {
                        s.insert = s.strstart;
                    }
                }
                let start = s.next_in - used;
                let at = s.strstart;
                s.window[at..at + used].copy_from_slice(&input[start..start + used]);
                s.strstart += used;
                s.insert += used.min(W_SIZE - s.insert);
            }
            s.block_start = s.strstart as i64;
        }
        if s.high_water < s.strstart {
            s.high_water = s.strstart;
        }

        if last {
            return BState::FinishDone;
        }
        if flush != Z_NO_FLUSH && flush != Z_FINISH && s.avail_in == 0 && s.strstart as i64 == s.block_start
        {
            return BState::BlockDone;
        }

        let mut have = WINDOW_SIZE - s.strstart;
        if s.avail_in > have && s.block_start >= W_SIZE as i64 {
            s.block_start -= W_SIZE as i64;
            s.strstart -= W_SIZE;
            s.window.copy_within(W_SIZE..W_SIZE + s.strstart, 0);
            if s.matches < 2 {
                s.matches += 1;
            }
            have += W_SIZE;
            if s.insert > s.strstart {
                s.insert = s.strstart;
            }
        }
        if have > s.avail_in {
            have = s.avail_in;
        }
        if have != 0 {
            let at = s.strstart;
            let n = s.read_buf_into_window(input, at, have);
            s.strstart += n;
            s.insert += n.min(W_SIZE - s.insert);
        }
        if s.high_water < s.strstart {
            s.high_water = s.strstart;
        }

        have = ((s.bi_valid + 42) >> 3) as usize;
        have = (PENDING_BUF_SIZE - have).min(MAX_STORED);
        min_block = have.min(W_SIZE);
        let left = (s.strstart as i64 - s.block_start) as usize;
        if left >= min_block
            || ((left != 0 || flush == Z_FINISH)
                && flush != Z_NO_FLUSH
                && s.avail_in == 0
                && left <= have)
        {
            let len = left.min(have);
            let last2 = flush == Z_FINISH && s.avail_in == 0 && len == left;
            let from = s.block_start as usize;
            s.tr_stored_block(Some(from), len, last2);
            s.block_start += len as i64;
            s.flush_pending(out);
            if last2 {
                return BState::FinishStarted;
            }
        }

        BState::NeedMore
    }
}
