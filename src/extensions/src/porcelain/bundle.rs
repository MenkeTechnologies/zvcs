//! `git bundle` — move objects and refs by archive.
//!
//! All four subcommands are ported (checked against git 2.55.0). The three that
//! only *read* a bundle are byte-verifiable against stock git; `create` writes a
//! bundle whose header is byte-identical and whose pack is not — see below.
//!
//! [`uri`] holds the bundle-URI client — `git clone --bundle-uri` and the
//! `fetch.bundleURI` a `git fetch` follows — which is the only place the
//! `bundle.*` key space is ever read, since those keys live in a downloaded
//! bundle *list* and not in any repository config.
//!
//! Ported, byte-for-byte:
//!   * `git bundle list-heads <file> [<refname>...]` — the bundle header's ref
//!     list, optionally filtered by exact ref name (git compares the stored ref
//!     name with `strcmp`, so `topic` does not match `refs/heads/topic`)
//!   * `git bundle verify [-q | --quiet] <file>` — prerequisite check plus the
//!     `The bundle contains …` / `The bundle requires …` /
//!     `The bundle records a complete history.` /
//!     `The bundle uses this hash algorithm: …` report on stdout and the
//!     `<file> is okay` line on stderr, including all three failure paths
//!     (`could not open`, `does not look like a v2 or v3 bundle file`,
//!     `Repository lacks these prerequisite commits:`) and the
//!     not-connected-to-history diagnostic
//!   * `git bundle unbundle [--progress] <file> [<refname>...]` — the
//!     prerequisite check, then `index-pack --fix-thin --stdin` over the pack
//!     that follows the header, then the ref list (filtered by exact name like
//!     `list-heads`). git spawns that `index-pack` as a child process
//!     (`ip.git_cmd = 1`, `ip.in = bundle_fd`, `ip.no_stdout = 1` in
//!     `bundle.c`), and so does this, which is why the header is read one byte
//!     at a time: the child inherits the very same descriptor, positioned at the
//!     first byte of the pack
//!   * `-h` for `bundle` itself and for each of the four subcommands (usage to
//!     stdout, exit 129), plus `need a subcommand`, `unknown subcommand`,
//!     `unknown option`/`unknown switch` and `need a <file> argument`
//!   * `-` as `<file>`, meaning the bundle is read from stdin
//!
//! Exit codes match git: 0 on success, 1 for a bundle that cannot be opened,
//! parsed, or verified, 129 for usage errors.
//!
//! One caveat carries over from `porcelain/index_pack.rs`: a *thin* bundle's
//! stored pack diverges from git's bytes because `gix` injects borrowed bases
//! just before their first referencing delta rather than appending them
//! (`index_pack.rs:60`). The objects and refs are identical; the pack hash on
//! disk need not be.
//!
//! Ported, with one documented divergence:
//!   * `git bundle create [-q | --quiet | --progress] [--version=<n>] <file>
//!     <git-rev-list-args>` — the signature, the `-<oid> <oneline>`
//!     prerequisites, the `<oid> <ref>` tip list, the header-terminating blank
//!     line, and the pack, in git's order and with git's ref-naming rules
//!     (`HEAD` stays `HEAD` because it is a symref; a short name is written out
//!     as the full ref it dwims to). `-` writes to stdout, any other name is
//!     written through a `.lock` and renamed, as `hold_lock_file_for_update`
//!     does. `Refusing to create empty bundle.` and `unsupported bundle
//!     version <n>` are reproduced.
//!
//!     **The pack bytes are not git's.** The header — magic line, prerequisite
//!     and tip lines, terminating blank line — is byte-identical; the pack that
//!     follows is not, for three independent reasons, each measured against
//!     stock git 2.55.0 on the `branched` fixture:
//!
//!       1. *Object order.* git's `compute_write_order()` groups the pack by
//!          type (tagged tips, then remaining commits and tags, then trees, then
//!          the rest) and keeps delta families contiguous. This port writes in
//!          the order `objects_to_send()` produces, which is
//!          `HashSet<ObjectId>` iteration order — so the order is not git's *and
//!          is not stable between two runs of this binary*. Fixing it means
//!          returning an ordered collection from
//!          `push_proto::objects_to_send()` and porting `compute_write_order()`,
//!          neither of which lives in this module.
//!       2. *Deflate output.* zvcs compresses through `zlib-rs`, which targets
//!          zlib-ng-compatible output rather than bit-identity with the zlib
//!          stock git links. On the `branched` fixture 6 of 13 objects compress
//!          to a different length at the same level (a 235-byte commit becomes
//!          149 bytes here and 152 in stock). No level setting closes this; only
//!          swapping the compressor would.
//!       3. *Thinness.* git passes `--thin`, so its deltas may name bases the
//!          receiver already has; `gix-pack`'s writer has exactly one mode,
//!          documented as "Copy base objects and deltas from packs, while
//!          non-packed objects will be treated as base objects"
//!          (`gix-pack/src/data/output/entry/iter_from_counts.rs:362`). With
//!          `--all` there are no prerequisites, so this one is inert there.
//!
//!     What is written is a self-contained superset of git's pack: every object
//!     it references is present, so `git bundle unbundle`, `git clone` and
//!     `index-pack --fix-thin` all accept it and produce the same objects and
//!     refs. Delta *base selection* is not currently a cause — on `branched`
//!     gitoxide picks the same base and emits a byte-identical delta payload —
//!     and the deltas are `OBJ_OFS_DELTA`, because `write_pack_data()` spawns
//!     `pack-objects --stdout --thin --delta-base-offset` unconditionally
//!     (`bundle.c:333-336`).
//!
//! Two further deliberate gaps, so this doc claims no more than the code does:
//! a v3 bundle carrying any capability other than `@object-format` is rejected
//! (git's `The bundle uses this filter: …` line is not reproduced from a
//! verified source), and a header that parses as neither is surfaced as a plain
//! error rather than git's `unrecognized header:` text.
//!
//! `args` excludes the `bundle` verb itself: `dispatch::run` is handed
//! `&argv[2..]` (see `lib.rs`), so `args[0]` is the subcommand.

use anyhow::{bail, Result};
use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::process::{ExitCode, Stdio};

use gix::hash::ObjectId;
use gix::objs::Kind;

pub(crate) mod uri;

/// The top-level usage block, byte-for-byte as git 2.55 emits it.
const TOP_USAGE: &str = "\
usage: git bundle create [-q | --quiet | --progress]
                         [--version=<version>] <file> <git-rev-list-args>
   or: git bundle verify [-q | --quiet] <file>
   or: git bundle list-heads <file> [<refname>...]
   or: git bundle unbundle [--progress] <file> [<refname>...]

";

const CREATE_USAGE: &str = "\
usage: git bundle create [-q | --quiet | --progress]
                         [--version=<version>] <file> <git-rev-list-args>

    -q, --[no-]quiet      do not show progress meter
    --[no-]progress       show progress meter
    --[no-]version <n>    specify bundle format version

";

const VERIFY_USAGE: &str = "\
usage: git bundle verify [-q | --quiet] <file>

    -q, --[no-]quiet      do not show bundle details

";

const LIST_HEADS_USAGE: &str = "\
usage: git bundle list-heads <file> [<refname>...]

";

const UNBUNDLE_USAGE: &str = "\
usage: git bundle unbundle [--progress] <file> [<refname>...]

    --[no-]progress       show progress meter

";

pub fn bundle(args: &[String]) -> Result<ExitCode> {
    let Some(sub) = args.first() else {
        eprint!("error: need a subcommand\n{TOP_USAGE}");
        return Ok(ExitCode::from(129));
    };
    let rest = &args[1..];

    match sub.as_str() {
        // `--help-all` is a `strcmp()` of its own inside `parse_options_step()`,
        // rendering `USAGE_FULL` — the same block as `-h` here, since no entry of
        // this table is `PARSE_OPT_HIDDEN`.
        "-h" | "--help-all" => {
            print!("{TOP_USAGE}");
            Ok(ExitCode::from(129))
        }
        "create" => create(rest),
        "verify" => verify(rest),
        "list-heads" => list_heads(rest),
        "unbundle" => unbundle(rest),
        s if s.starts_with("--") => Ok(bad_option(&s[2..], TOP_USAGE, false)),
        s if s.starts_with('-') && s.len() > 1 => Ok(bad_option(&s[1..], TOP_USAGE, true)),
        s => {
            eprint!("error: unknown subcommand: `{s}'\n{TOP_USAGE}");
            Ok(ExitCode::from(129))
        }
    }
}

/// git's parse-options diagnostic for an unrecognised option, plus the usage
/// block of the (sub)command that rejected it. Exit 129, both on stderr.
fn bad_option(name: &str, usage: &str, short: bool) -> ExitCode {
    let kind = if short { "switch" } else { "option" };
    eprint!("error: unknown {kind} `{name}'\n{usage}");
    ExitCode::from(129)
}

/// git's `fatal: need a <file> argument`, followed by a blank line and usage.
fn need_file(usage: &str) -> ExitCode {
    eprint!("fatal: need a <file> argument\n\n{usage}");
    ExitCode::from(129)
}

// ---------------------------------------------------------------- header ----

/// A parsed bundle header: everything before the pack data.
pub(crate) struct Header {
    /// The value of the `@object-format` capability, or `sha1` when absent.
    /// Printed verbatim by `verify` as the hash algorithm.
    hash: String,
    /// Prerequisite object ids (header lines starting with `-`). git prints the
    /// comment that follows them nowhere, so it is not retained.
    prereqs: Vec<ObjectId>,
    /// `(object id, ref name)` pairs. Ref names are kept as raw bytes because
    /// they are echoed verbatim and are not required to be UTF-8.
    pub(crate) refs: Vec<(ObjectId, Vec<u8>)>,
}

/// The failures git reports itself, with its own wording and exit code 1.
pub(crate) enum HeaderError {
    /// `error: could not open '<file>'`
    Open,
    /// `error: '<file>' does not look like a v2 or v3 bundle file`
    NotBundle,
    /// A header that starts correctly but does not parse. git has its own
    /// `unrecognized header:` text for this; it is not reproduced here, so the
    /// reason is surfaced as a plain error instead of a wrong-looking match.
    Malformed(String),
}

/// Report a [`HeaderError`] the way git does and yield its exit code, except
/// for [`HeaderError::Malformed`] which becomes an ordinary error.
fn report(path: &str, err: HeaderError) -> Result<ExitCode> {
    match err {
        HeaderError::Open => eprintln!("error: could not open '{path}'"),
        HeaderError::NotBundle => {
            eprintln!("error: '{path}' does not look like a v2 or v3 bundle file");
        }
        HeaderError::Malformed(why) => crate::git_fatal!("malformed bundle header in {path:?}: {why}"),
    }
    Ok(ExitCode::from(1))
}

/// The bundle byte stream, left at exactly the position the header parser
/// stopped at — the first byte of the pack.
///
/// git reads bundle headers one byte at a time (`strbuf_getwholeline_fd`,
/// `strbuf.c`, called from `read_bundle_header_fd` in `bundle.c`) for precisely
/// this reason: `unbundle()` then hands the very same descriptor to
/// `index-pack --stdin` as its `ip.in`. Any read-ahead buffer would swallow the
/// leading pack bytes, so this type never buffers.
pub(crate) enum BundleSource {
    File(File),
    /// `-`: the stream is descriptor 0 itself, which the child inherits in place.
    Stdin,
}

impl Read for BundleSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            BundleSource::File(f) => f.read(buf),
            // `io::Stdin` wraps a `BufReader` that would read past the header, so
            // go to the descriptor directly. `ManuallyDrop` keeps fd 0 open.
            BundleSource::Stdin => {
                let mut fd = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
                fd.read(buf)
            }
        }
    }
}

impl BundleSource {
    /// The stream as a child's stdin, still positioned at the pack. This is
    /// git's `ip.in = bundle_fd`.
    fn into_stdio(self) -> Stdio {
        match self {
            BundleSource::File(f) => Stdio::from(f),
            BundleSource::Stdin => Stdio::inherit(),
        }
    }
}

/// Read one `\n`-terminated line, keeping the terminator. `Ok(None)` at EOF.
///
/// One byte per `read`, as `strbuf_getwholeline_fd` does, so the stream stops on
/// the terminator and not a byte later.
fn read_line(input: &mut dyn Read) -> Result<Option<Vec<u8>>, HeaderError> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => return Ok(if line.is_empty() { None } else { Some(line) }),
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    return Ok(Some(line));
                }
            }
            Err(_) => return Err(HeaderError::NotBundle),
        }
    }
}

/// Parse the header of the bundle at `path` (`-` means stdin), stopping at the
/// blank line that separates it from the pack data.
fn read_header(path: &str) -> Result<Header, HeaderError> {
    open_bundle(path).map(|(header, _)| header)
}

/// git's `open_bundle()` (`builtin/bundle.c`): the parsed header plus the stream
/// positioned at the pack, ready to be handed to `index-pack --stdin`.
pub(crate) fn open_bundle(path: &str) -> Result<(Header, BundleSource), HeaderError> {
    let mut input = if path == "-" {
        BundleSource::Stdin
    } else {
        BundleSource::File(File::open(path).map_err(|_| HeaderError::Open)?)
    };
    let header = read_header_from(&mut input)?;
    Ok((header, input))
}

fn read_header_from(input: &mut BundleSource) -> Result<Header, HeaderError> {
    let magic = read_line(input)?.ok_or(HeaderError::NotBundle)?;
    let version = match magic.as_slice() {
        b"# v2 git bundle\n" => 2u8,
        b"# v3 git bundle\n" => 3u8,
        _ => return Err(HeaderError::NotBundle),
    };

    let mut header = Header {
        hash: "sha1".into(),
        prereqs: Vec::new(),
        refs: Vec::new(),
    };
    let mut hexsz = 40usize;

    let mut pending: Option<Vec<u8>>;
    // Capabilities (v3 only) come first, each on its own `@key[=value]` line.
    loop {
        let Some(line) = read_line(input)? else {
            return Err(HeaderError::Malformed("truncated before the pack".into()));
        };
        if !line.starts_with(b"@") {
            pending = Some(line);
            break;
        }
        if version < 3 {
            return Err(HeaderError::Malformed(
                "capability line in a v2 bundle".into(),
            ));
        }
        let cap = String::from_utf8_lossy(&line[1..]).trim_end().to_string();
        match cap.strip_prefix("object-format=") {
            Some("sha1") => {}
            Some("sha256") => {
                header.hash = "sha256".into();
                hexsz = 64;
            }
            Some(other) => {
                return Err(HeaderError::Malformed(format!(
                    "unknown object format {other:?}"
                )))
            }
            None => {
                return Err(HeaderError::Malformed(format!(
                    "capability {cap:?} is not supported"
                )))
            }
        }
    }

    // Ref lines, terminated by an empty line.
    loop {
        let line = match pending.take() {
            Some(line) => line,
            None => read_line(input)?
                .ok_or_else(|| HeaderError::Malformed("truncated before the pack".into()))?,
        };
        let line = line.strip_suffix(b"\n").unwrap_or(&line);
        if line.is_empty() {
            break;
        }
        let (is_prereq, body) = match line.strip_prefix(b"-") {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        if body.len() < hexsz {
            return Err(HeaderError::Malformed("short object id".into()));
        }
        let oid = ObjectId::from_hex(&body[..hexsz])
            .map_err(|e| HeaderError::Malformed(format!("bad object id: {e}")))?;
        if is_prereq {
            header.prereqs.push(oid);
        } else {
            // Exactly one space separates the id from the ref name.
            let name = body[hexsz..].strip_prefix(b" ").unwrap_or(&body[hexsz..]);
            header.refs.push((oid, name.to_vec()));
        }
    }

    Ok(header)
}

// ------------------------------------------------------------ list-heads ----

fn list_heads(args: &[String]) -> Result<ExitCode> {
    let mut file: Option<&str> = None;
    let mut filters: Vec<&[u8]> = Vec::new();

    for a in args {
        match a.as_str() {
            // `--help-all` renders `USAGE_FULL`, identical to the `-h` block:
            // no entry of this subcommand's table is `PARSE_OPT_HIDDEN`.
            "-h" | "--help-all" => {
                print!("{LIST_HEADS_USAGE}");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with("--") && s.len() > 2 => {
                return Ok(bad_option(&s[2..], LIST_HEADS_USAGE, false));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                return Ok(bad_option(&s[1..], LIST_HEADS_USAGE, true));
            }
            s if file.is_none() => file = Some(s),
            s => filters.push(s.as_bytes()),
        }
    }

    let Some(file) = file else {
        return Ok(need_file(LIST_HEADS_USAGE));
    };
    let header = match read_header(file) {
        Ok(h) => h,
        Err(e) => return report(file, e),
    };

    let mut out = Vec::new();
    write_refs(&mut out, &header.refs, &filters);
    io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// Render `<oid> <name>` lines, keeping only the refs named in `filters`
/// (an empty filter list keeps everything). git matches ref names exactly.
fn write_refs(out: &mut Vec<u8>, refs: &[(ObjectId, Vec<u8>)], filters: &[&[u8]]) {
    for (oid, name) in refs {
        if !filters.is_empty() && !filters.contains(&name.as_slice()) {
            continue;
        }
        out.extend_from_slice(oid.to_hex().to_string().as_bytes());
        out.push(b' ');
        out.extend_from_slice(name);
        out.push(b'\n');
    }
}

// ---------------------------------------------------------------- verify ----

fn verify(args: &[String]) -> Result<ExitCode> {
    let mut quiet = false;
    let mut file: Option<&str> = None;

    for a in args {
        match a.as_str() {
            // `--help-all` renders `USAGE_FULL`, identical to the `-h` block:
            // no entry of this subcommand's table is `PARSE_OPT_HIDDEN`.
            "-h" | "--help-all" => {
                print!("{VERIFY_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            s if s.starts_with("--") && s.len() > 2 => {
                return Ok(bad_option(&s[2..], VERIFY_USAGE, false));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                return Ok(bad_option(&s[1..], VERIFY_USAGE, true));
            }
            // git's verify takes a single <file>; further operands are ignored.
            s if file.is_none() => file = Some(s),
            _ => {}
        }
    }

    let Some(file) = file else {
        return Ok(need_file(VERIFY_USAGE));
    };
    let header = match read_header(file) {
        Ok(h) => h,
        Err(e) => return report(file, e),
    };

    let repo = gix::discover(".")?;

    if !report_missing_prereqs(&repo, &header, quiet) {
        return Ok(ExitCode::from(1));
    }

    // Every prerequisite is present; its whole ancestry must be too.
    let mut ok = true;
    if !header.prereqs.is_empty() && !history_is_complete(&repo, &header.prereqs) {
        if !quiet {
            eprintln!(
                "error: some prerequisite commits exist in the object store, but are not connected to the repository's history"
            );
        }
        ok = false;
    }

    if !quiet {
        let mut out = Vec::new();
        let n = header.refs.len();
        if n == 1 {
            out.extend_from_slice(b"The bundle contains this ref:\n");
        } else {
            out.extend_from_slice(format!("The bundle contains these {n} refs:\n").as_bytes());
        }
        write_refs(&mut out, &header.refs, &[]);

        let p = header.prereqs.len();
        if p == 0 {
            out.extend_from_slice(b"The bundle records a complete history.\n");
        } else {
            if p == 1 {
                out.extend_from_slice(b"The bundle requires this ref:\n");
            } else {
                out.extend_from_slice(format!("The bundle requires these {p} refs:\n").as_bytes());
            }
            for oid in &header.prereqs {
                out.extend_from_slice(format!("{oid} \n").as_bytes());
            }
        }
        out.extend_from_slice(
            format!("The bundle uses this hash algorithm: {}\n", header.hash).as_bytes(),
        );
        io::stdout().write_all(&out)?;
    }

    if !ok {
        return Ok(ExitCode::from(1));
    }
    eprintln!("{file} is okay");
    Ok(ExitCode::SUCCESS)
}

/// The prerequisite half of git's `verify_bundle()` (`bundle.c`): report every
/// prerequisite commit the repository lacks and answer whether none were
/// missing. `quiet` is git's `VERIFY_BUNDLE_QUIET`, which suppresses the report
/// but not the verdict.
///
/// A prerequisite is satisfied only if the object is present *and* is a commit —
/// git's `parse_object()` yields nothing else to its pending list, so a present
/// blob or tree reads as missing.
fn report_missing_prereqs(repo: &gix::Repository, header: &Header, quiet: bool) -> bool {
    let missing: Vec<&ObjectId> = header
        .prereqs
        .iter()
        .filter(|oid| !matches!(repo.find_header(**oid).map(|h| h.kind()), Ok(Kind::Commit)))
        .collect();
    if missing.is_empty() {
        return true;
    }
    if !quiet {
        eprintln!("error: Repository lacks these prerequisite commits:");
        for oid in missing {
            // git prints `<oid> <name>` with an empty name for prerequisites.
            eprintln!("error: {oid} ");
        }
    }
    false
}

/// git's `verify_bundle()` with neither `VERIFY_BUNDLE_VERBOSE` nor a
/// reachability shortcut: the prerequisite presence check followed by the
/// connectivity check. Answers whether the bundle may be applied.
pub(crate) fn verify_bundle(repo: &gix::Repository, header: &Header, quiet: bool) -> bool {
    if !report_missing_prereqs(repo, header, quiet) {
        return false;
    }
    if !header.prereqs.is_empty() && !history_is_complete(repo, &header.prereqs) {
        if !quiet {
            eprintln!(
                "error: some prerequisite commits exist in the object store, but are not connected to the repository's history"
            );
        }
        return false;
    }
    true
}

/// Whether every commit reachable from `tips` is present in the object store.
/// A traversal error means a parent (or one of its ancestors) is missing, which
/// is exactly the "exists but is not connected" case git reports.
fn history_is_complete(repo: &gix::Repository, tips: &[ObjectId]) -> bool {
    let Ok(walk) = repo.rev_walk(tips.to_vec()).all() else {
        return false;
    };
    for info in walk {
        if info.is_err() {
            return false;
        }
    }
    true
}

// -------------------------------------------------- create / unbundle -------

/// `git bundle create [-q | --quiet | --progress] [--version=<n>] <file>
/// <git-rev-list-args>`
///
/// Port of `cmd_bundle_create` (`builtin/bundle.c`) plus the `create_bundle()`
/// it calls (`bundle.c:478-604`), in git's order: the signature, the
/// prerequisite lines, the ref lines, the blank line that ends the header, and
/// the pack.
fn create(args: &[String]) -> Result<ExitCode> {
    // `--help-all` is its own `strcmp()` inside `parse_options_step()` and prints
    // `USAGE_FULL`, which is this same block — no entry here is
    // `PARSE_OPT_HIDDEN`.
    if args.iter().any(|a| a == "-h" || a == "--help-all") {
        print!("{CREATE_USAGE}");
        return Ok(ExitCode::from(129));
    }

    // `builtin_bundle_create_options`: the three progress switches all feed the
    // same `progress` int, which only decides what `pack-objects` narrates on
    // stderr. Nothing here narrates, so all four spellings are accepted and
    // dropped. `--version` is the one option that changes the bytes.
    let mut version: Option<u8> = None;
    let mut rev_args: Vec<&str> = Vec::new();
    let mut file: Option<&str> = None;
    let mut end_of_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts || !a.starts_with('-') || a == "-" {
            if file.is_none() {
                file = Some(a);
            } else {
                rev_args.push(a);
            }
            i += 1;
            continue;
        }
        match a {
            "--" => end_of_opts = true,
            "-q" | "--quiet" | "--progress" | "--all-progress" | "--all-progress-implied" => {}
            "--version" => {
                let Some(v) = args.get(i + 1) else {
                    eprintln!("error: option `version' requires a value");
                    return Ok(ExitCode::from(129));
                };
                version = Some(parse_version(v)?);
                i += 1;
            }
            _ if a.starts_with("--version=") => {
                version = Some(parse_version(&a["--version=".len()..])?)
            }
            // Everything else is a rev-list argument: `--all`, `^<rev>`,
            // `--not`, and the rest of the revision grammar.
            _ if file.is_some() => rev_args.push(a),
            _ => return Ok(bad_option(a.trim_start_matches('-'), CREATE_USAGE, !a.starts_with("--"))),
        }
        i += 1;
    }

    let Some(file) = file else {
        return Ok(need_file(CREATE_USAGE));
    };

    let repo = gix::discover(".")?;
    let pending = resolve_revisions(&repo, &rev_args)?;

    // `if (version == -1) version = min_version;` — 2 for sha1, and only 2 or 3
    // exist (bundle.c:525-531). The v2 header carries no `@object-format`
    // capability, so it can only describe sha1: a sha256 repository defaults to
    // 3, and an explicit `--version=2` is refused rather than written as a
    // bundle whose reader would take 64-hex ids for 40-hex ones.
    let sha1_repo = repo.object_hash() == gix::hash::Kind::Sha1;
    let version = version.unwrap_or(if sha1_repo { 2 } else { 3 });
    if !(2..=3).contains(&version) {
        eprintln!("fatal: unsupported bundle version {version}");
        return Ok(ExitCode::from(128));
    }
    if version < 3 && !sha1_repo {
        eprintln!(
            "fatal: cannot write bundle version {version} with algorithm {}",
            repo.object_hash()
        );
        return Ok(ExitCode::from(128));
    }

    let mut out: Vec<u8> = Vec::new();
    if version == 2 {
        out.extend_from_slice(b"# v2 git bundle\n");
    } else {
        out.extend_from_slice(b"# v3 git bundle\n");
        out.extend_from_slice(format!("@object-format={}\n", repo.object_hash()).as_bytes());
    }

    // `revs.boundary = 1` then `traverse_commit_list(..., write_bundle_prerequisites, ...)`
    // (bundle.c:564-575): each BOUNDARY commit is written as `-<oid> <oneline>`.
    let tips: Vec<ObjectId> = pending.iter().filter(|p| !p.uninteresting).map(|p| p.id).collect();
    let excluded: Vec<ObjectId> =
        pending.iter().filter(|p| p.uninteresting).map(|p| p.id).collect();
    // `UNINTERESTING` sits on the object, and by the time the header is written git
    // has already run the walk that spreads it to every ancestor of a `^<rev>`. Both
    // the prerequisite scan and the ref list below read it back off the objects they
    // hold, so the closure is computed once here.
    let excluded_closure = if excluded.is_empty() {
        std::collections::HashSet::new()
    } else {
        super::log::ancestor_closure(&repo, &excluded)?
    };
    let prereqs = boundary_commits(&repo, &tips, &excluded, &excluded_closure);
    for id in &prereqs {
        let subject = commit_oneline(&repo, *id);
        out.extend_from_slice(format!("-{id} {subject}\n").as_bytes());
    }

    // `write_bundle_refs()` (bundle.c:383-444): the interesting pending entries
    // that dwim to a ref, deduplicated by the name that gets written.
    //
    // "Interesting" is `e->item->flags & UNINTERESTING`, a property of the object the
    // entry points at rather than of the entry, so a ref is dropped as soon as
    // *anything* excluded reaches its commit — not only when the same name was also
    // written with a `^`. `git bundle create <file> --all ^main` therefore keeps only
    // the refs outside main's history, and `... main ^main` keeps none at all and
    // refuses to write an empty bundle.
    let mut seen: Vec<String> = Vec::new();
    for entry in pending
        .iter()
        .filter(|p| !p.uninteresting && !excluded_closure.contains(&p.id))
    {
        let Some(display) = &entry.display_ref else { continue };
        if seen.iter().any(|s| s == display) {
            continue;
        }
        seen.push(display.clone());
        out.extend_from_slice(format!("{} {display}\n", entry.id).as_bytes());
    }
    if seen.is_empty() {
        eprintln!("fatal: Refusing to create empty bundle.");
        return Ok(ExitCode::from(128));
    }
    // `write_or_die(bundle_fd, "\n", 1)` — the blank line that ends the header.
    out.push(b'\n');

    // `write_pack_data()`: the objects reachable from the tips and not from the
    // prerequisites. git's pack is thin (its deltas may name bases the receiver
    // already has); this one is not, so it carries every object it references.
    // A non-thin pack is a strictly self-contained superset — `unbundle` and
    // `index-pack --fix-thin` accept it unchanged — so the bundle is correct,
    // but its bytes are not git's. See the module header.
    let mut haves = prereqs.clone();
    haves.extend_from_slice(&excluded);
    let objects = crate::porcelain::push_proto::objects_to_send(&repo, &tips, &haves);
    // `write_pack_data()` spawns `pack-objects --stdout --thin --delta-base-offset`
    // (bundle.c:333-336) — the flag is unconditional there, so a bundle's deltas
    // are always `OBJ_OFS_DELTA`. Passing `false` here wrote `OBJ_REF_DELTA`
    // instead, which is 18 bytes larger per delta and is not what any git bundle
    // contains.
    out.extend_from_slice(&crate::porcelain::pack_objects::pack_bytes_with(
        &repo, &objects, true,
    )?);

    if file == "-" {
        io::stdout().write_all(&out)?;
        io::stdout().flush()?;
    } else {
        // `hold_lock_file_for_update` + `commit_lock_file`: the bundle appears
        // whole or not at all, so a reader never sees a half-written header.
        let tmp = format!("{file}.lock");
        std::fs::write(&tmp, &out)?;
        std::fs::rename(&tmp, file)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `--version=<n>`: git's `OPT_INTEGER`, so a non-numeric value is a usage
/// error before the version range is ever looked at.
fn parse_version(v: &str) -> Result<u8> {
    v.parse::<u8>()
        .map_err(|_| anyhow::anyhow!("option `version' expects a numerical value"))
}

/// One entry of git's `revs.pending`: the object a revision argument named, the
/// ref name `write_bundle_refs` would print for it, and whether it arrived
/// negated.
struct Pending {
    id: ObjectId,
    /// `display_ref` in `write_bundle_refs`: the dwim-resolved full ref name,
    /// or the name as typed when that name is a symref (which is what keeps
    /// `HEAD` printing as `HEAD` rather than as its target). `None` for an
    /// argument that does not name a ref at all, which git skips.
    display_ref: Option<String>,
    uninteresting: bool,
}

/// `setup_revisions()` reduced to the grammar `bundle create` is documented
/// with: `--all`, `<rev>`, `^<rev>`, and `<a>..<b>`.
fn resolve_revisions(repo: &gix::Repository, args: &[&str]) -> Result<Vec<Pending>> {
    let mut pending = Vec::new();
    for arg in args {
        match *arg {
            // `handle_refs(for_each_ref)` then `handle_refs(head_ref)`
            // (revision.c) — which is why `HEAD` lands after the ref list.
            "--all" => {
                for r in repo.references()?.all()? {
                    let Ok(r) = r else { continue };
                    let name = r.name().as_bstr().to_string();
                    if let Some(id) = r.try_id() {
                        pending.push(Pending {
                            id: id.detach(),
                            display_ref: Some(name),
                            uninteresting: false,
                        });
                    }
                }
                if let Ok(head) = repo.head_id() {
                    pending.push(Pending {
                        id: head.detach(),
                        display_ref: Some("HEAD".into()),
                        uninteresting: false,
                    });
                }
            }
            a => {
                if let Some(rest) = a.strip_prefix('^') {
                    pending.push(one_pending(repo, rest, true)?);
                } else if let Some((left, right)) = a.split_once("..") {
                    pending.push(one_pending(repo, left, true)?);
                    pending.push(one_pending(repo, right, false)?);
                } else {
                    pending.push(one_pending(repo, a, false)?);
                }
            }
        }
    }
    Ok(pending)
}

/// Resolve one revision argument, recording the name `write_bundle_refs` would
/// print for it.
fn one_pending(repo: &gix::Repository, spec: &str, uninteresting: bool) -> Result<Pending> {
    let id = repo
        .rev_parse_single(spec)
        .map_err(|_| anyhow::anyhow!("bad revision '{spec}'"))?
        .detach();
    // `repo_dwim_ref()` decides whether this is a ref at all; a symref keeps the
    // name as typed (bundle.c:398-403).
    let display_ref = match repo.find_reference(spec) {
        Ok(r) => {
            let is_symref = matches!(r.target(), gix::refs::TargetRef::Symbolic(_));
            Some(if is_symref { spec.to_string() } else { r.name().as_bstr().to_string() })
        }
        Err(_) => None,
    };
    Ok(Pending { id, display_ref, uninteresting })
}

/// The `BOUNDARY` commits of the walk from `tips` with `excluded` marked
/// uninteresting: the excluded commits that are directly reachable from a
/// commit the pack will carry. These are exactly the prerequisites a receiver
/// must already have.
fn boundary_commits(
    repo: &gix::Repository,
    tips: &[ObjectId],
    excluded: &[ObjectId],
    hidden: &std::collections::HashSet<ObjectId>,
) -> Vec<ObjectId> {
    if excluded.is_empty() {
        return Vec::new();
    }
    let mut boundary = Vec::new();
    let walk = repo
        .rev_walk(tips.to_vec())
        .with_hidden(excluded.to_vec())
        .all();
    for info in walk.into_iter().flatten() {
        let Ok(info) = info else { continue };
        let Ok(commit) = repo.find_commit(info.id) else { continue };
        for parent in commit.parent_ids() {
            let parent = parent.detach();
            if hidden.contains(&parent) && !boundary.contains(&parent) {
                boundary.push(parent);
            }
        }
    }
    boundary
}

/// `CMIT_FMT_ONELINE`: the commit's subject, i.e. its message up to the first
/// blank line, with surrounding whitespace trimmed.
fn commit_oneline(repo: &gix::Repository, id: ObjectId) -> String {
    let Ok(commit) = repo.find_commit(id) else { return String::new() };
    let Ok(message) = commit.message() else { return String::new() };
    message.summary().to_string()
}

/// `git bundle unbundle [--progress] <file> [<refname>...]`
///
/// Port of `cmd_bundle_unbundle` (`builtin/bundle.c`) plus the `unbundle()` it
/// calls (`bundle.c`): open the bundle, verify its prerequisites, then run
/// `index-pack --fix-thin --stdin` over the pack that follows the header and
/// list the bundle's refs. git spawns that `index-pack` as a child process with
/// `ip.git_cmd = 1` and `ip.in = bundle_fd`; this does the same with the running
/// binary, so `porcelain/index_pack.rs` is reused exactly as git reuses its own
/// builtin rather than being duplicated here.
fn unbundle(args: &[String]) -> Result<ExitCode> {
    // git's `int progress = isatty(2);`, overridable with `--progress`.
    let mut progress = io::stderr().is_terminal();
    let mut file: Option<&str> = None;
    let mut filters: Vec<&[u8]> = Vec::new();

    for a in args {
        match a.as_str() {
            // `--help-all` renders `USAGE_FULL`, identical to the `-h` block:
            // no entry of this subcommand's table is `PARSE_OPT_HIDDEN`.
            "-h" | "--help-all" => {
                print!("{UNBUNDLE_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--progress" => progress = true,
            "--no-progress" => progress = false,
            s if s.starts_with("--") && s.len() > 2 => {
                return Ok(bad_option(&s[2..], UNBUNDLE_USAGE, false));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                return Ok(bad_option(&s[1..], UNBUNDLE_USAGE, true));
            }
            s if file.is_none() => file = Some(s),
            s => filters.push(s.as_bytes()),
        }
    }

    let Some(file) = file else {
        return Ok(need_file(UNBUNDLE_USAGE));
    };

    // git's `if (!startup_info->have_repository) die(...)`, which exits 128.
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: Need a repository to unbundle.");
        return Ok(ExitCode::from(128));
    };

    let (header, source) = match open_bundle(file) {
        Ok(pair) => pair,
        Err(e) => return report(file, e),
    };

    // `unbundle()` runs `verify_bundle()` first and gives up if it fails.
    if !verify_bundle(&repo, &header, false) {
        return Ok(ExitCode::from(1));
    }

    let extra: &[&str] = if progress {
        &["-v", "--progress-title", "Unbundling objects"]
    } else {
        &[]
    };
    if !index_pack(source, &repo, extra)? {
        eprintln!("error: index-pack died");
        return Ok(ExitCode::from(1));
    }

    // `list_bundle_refs()`, which is `list_refs()` over the header's references —
    // the same rendering `list-heads` uses.
    let mut out = Vec::new();
    write_refs(&mut out, &header.refs, &filters);
    io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// git's `strvec_pushl(&ip.args, "index-pack", "--fix-thin", "--stdin", NULL)`
/// child, fed the bundle stream as its stdin. `ip.no_stdout = 1` in git, so the
/// `pack\t<hash>` line `index-pack` writes is discarded here too.
///
/// Answers whether the child succeeded.
pub(crate) fn index_pack(
    source: BundleSource,
    repo: &gix::Repository,
    extra_args: &[&str],
) -> Result<bool> {
    // The child must index into *this* repository even when the caller was
    // invoked from elsewhere — the bundle-URI client runs from the directory
    // `git clone` was started in, not from inside the new repository.
    // `index-pack` resolves the repository with `gix::discover(".")`
    // (`index_pack.rs:286`), which walks *upwards* and so does not recognise a
    // `.git` directory it is standing inside; the work tree is the directory to
    // hand it, falling back to the git dir itself for a bare repository.
    let cwd = repo.workdir().unwrap_or_else(|| repo.git_dir());
    let status = std::process::Command::new(std::env::current_exe()?)
        .current_dir(cwd)
        .args(["index-pack", "--fix-thin", "--stdin"])
        .args(extra_args)
        .stdin(source.into_stdio())
        .stdout(Stdio::null())
        .status()?;
    Ok(status.success())
}
