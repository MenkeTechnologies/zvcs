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
//!   * `--prefix=<prefix>/`, including the leading directory entry git writes
//!     for a prefix that ends in `/`.
//!   * `-o <file>` / `--output=<file>`.
//!   * `-l` / `--list`, `-v` / `--verbose`.
//!   * Trailing `[--] <path>...` filters, with git's "pathspec did not match"
//!     failure, and git's lazy directory-entry emission (a directory record is
//!     written only once a file below it is written).
//!   * Being run from a subdirectory, which narrows the tree to that
//!     subdirectory exactly as git does.
//!   * `tar.umask` (numeric).
//!
//! Not covered — every one of these fails loudly rather than emitting an
//! archive that would silently differ from git's:
//!   * `--format=zip`, `tgz`, `tar.gz`: the vendored `gix-archive` is built
//!     without its `tar`/`tar_gz`/`zip` features, so there is no deflate or
//!     zip-container substrate in the build, and git's `tgz` output depends on
//!     its internal gzip settings.
//!   * `--remote` / `--exec` (needs the `git-upload-archive` protocol),
//!     `--add-file`, `--add-virtual-file`, `--worktree-attributes`, `--mtime`,
//!     and the `-<digit>` compression levels.
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

/// Parsed command line for one `archive` invocation.
#[derive(Default)]
struct Opts {
    format: Option<String>,
    prefix: Option<String>,
    output: Option<String>,
    verbose: bool,
    treeish: Option<String>,
    paths: Vec<String>,
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
            "-l" | "--list" => list = true,
            "-v" | "--verbose" => opts.verbose = true,
            "--format" => opts.format = Some(value_of(args, &mut i, "--format")?),
            "--prefix" => opts.prefix = Some(value_of(args, &mut i, "--prefix")?),
            "-o" | "--output" => opts.output = Some(value_of(args, &mut i, "--output")?),
            _ if a.starts_with("--format=") => opts.format = Some(a[9..].to_string()),
            _ if a.starts_with("--prefix=") => opts.prefix = Some(a[9..].to_string()),
            _ if a.starts_with("--output=") => opts.output = Some(a[9..].to_string()),
            _ if a.len() > 1 && a.starts_with('-') => bail!(
                "unsupported flag {a:?} (ported: --format, --prefix, -o/--output, -l/--list, -v/--verbose, --)"
            ),
            _ if opts.treeish.is_none() => opts.treeish = Some(a.to_string()),
            _ => opts.paths.push(a.to_string()),
        }
        i += 1;
    }

    // `--list` short-circuits everything else, exactly as git's parse-options does.
    if list {
        let mut out = String::new();
        for f in FORMATS {
            out.push_str(f);
            out.push('\n');
        }
        print!("{out}");
        return Ok(ExitCode::SUCCESS);
    }

    let Some(spec) = opts.treeish.clone() else {
        eprintln!("usage: git archive [<options>] <tree-ish> [<path>...]");
        eprintln!("   or: git archive --list");
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
    match format.as_str() {
        "tar" => {}
        "tgz" | "tar.gz" | "zip" => bail!(
            "archive format {format:?} is not supported (ported: tar) \
             — the vendored gix-archive is built without its tar/tar_gz/zip features"
        ),
        _ => {
            eprintln!("fatal: Unknown archive format '{format}'");
            return Ok(ExitCode::from(128));
        }
    }

    for p in &opts.paths {
        if p.starts_with(':') || p.contains(['*', '?', '[']) {
            bail!("pathspec magic is not supported: {p:?}");
        }
    }

    let repo = gix::discover(".")?;

    if repo.config_snapshot().string("tar.tar.command").is_some() {
        bail!("tar.tar.command is configured but piping through it is not supported");
    }
    let umask = tar_umask(&repo)?;

    let Ok(id) = repo.rev_parse_single(spec.as_str()) else {
        eprintln!("fatal: not a valid object name: {spec}");
        return Ok(ExitCode::from(128));
    };

    // A commit (or a tag peeling to one) contributes the pax global header and
    // the entry mtime; anything else that peels to a tree uses the current time.
    let commit = id.object()?.peel_to_commit().ok().map(|c| (c.id, c.time()));
    let (commit_id, mtime) = match commit {
        Some((cid, time)) => (Some(cid), time?.seconds),
        None => (
            None,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or_default(),
        ),
    };
    let Ok(mut tree) = id.object()?.peel_to_tree() else {
        eprintln!("fatal: not a tree object: {}", id.detach());
        return Ok(ExitCode::from(128));
    };

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

    let base = opts.prefix.clone().unwrap_or_default();
    let sink: Box<dyn Write> = match &opts.output {
        Some(path) => Box::new(std::io::BufWriter::new(std::fs::File::create(path)?)),
        None => Box::new(std::io::BufWriter::new(std::io::stdout())),
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
    tar.finish()?;

    Ok(ExitCode::SUCCESS)
}

/// Read the value of an option given as a separate argument (`--format tar`).
fn value_of(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("option `{flag}` requires a value"))
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
        bail!("tar.umask=user is not supported (only a numeric tar.umask is ported)");
    }
    let (digits, radix) = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(rest) => (rest, 16),
        None if text.len() > 1 && text.starts_with('0') => (&text[1..], 8),
        None => (text.as_str(), 10),
    };
    u32::from_str_radix(digits, radix).map_err(|_| anyhow::anyhow!("invalid tar.umask {text:?}"))
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

    /// Pad to the next 10 KiB boundary, always adding at least one byte so the
    /// archive ends in a full zero block, then flush.
    fn finish(&mut self) -> Result<()> {
        let mut left = (self.written / BLOCK + 1) * BLOCK - self.written;
        while left > 0 {
            let n = left.min(RECORD as u64) as usize;
            self.raw(&ZEROS[..n])?;
            left -= n as u64;
        }
        self.out.flush()?;
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
