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
//!     as the full ref it dwims to). The revision arguments are the ones
//!     `setup_revisions()` reads — `create_bundle()` calls it directly
//!     (`bundle.c:501`) — so `<rev>`, `^<rev>`, `<a>..<b>`, `<a>...<b>` (merge
//!     bases excluded and pended first, under `oid_to_hex()`), `<rev>^@`,
//!     `<rev>^!`, `<rev>^-<n>`, `--not`, `--stdin` and the whole ref-selecting
//!     family (`--all`, `--branches`, `--tags`, `--remotes`, each optionally
//!     `=<glob>`, plus `--glob=<glob>` and the `--exclude=<glob>` patterns the
//!     next of them consumes) all reach it. So does the half of that grammar
//!     that is *not* revisions: a `--` and everything behind it, and a bare `..`
//!     — the pathspec for the parent directory rather than a range
//!     (revision.c:2164) — become `prune_data`, which `setup_revisions()` then
//!     parses, so `git bundle create <file> ..` ends at `pathspec.c`'s
//!     `'..' is outside repository`. `-` writes to stdout,
//!     any other name is
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
//!     `create`'s options are `PARSE_OPT_STOP_AT_NON_OPTION`, so the `<file>`
//!     operand ends option parsing: `git bundle create <file> -q` reports
//!     `error: unrecognized argument: -q` and writes nothing, exactly as stock
//!     does — except for the exit code, because git 2.55.0 prints that line and
//!     then aborts (a shell sees 134). This returns the 255 an `error()` return
//!     normally becomes rather than reproducing a `SIGABRT`.
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
        // `PARSE_OPT_STOP_AT_NON_OPTION`: the `<file>` operand ends option
        // parsing, so every later token is a `<refname>` filter — even one that
        // looks like a switch.
        if file.is_some() {
            filters.push(a.as_bytes());
            continue;
        }
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
            s => file = Some(s),
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
        // `PARSE_OPT_STOP_AT_NON_OPTION` (`parse_options_cmd_bundle`): the
        // `<file>` operand ends option parsing, and `cmd_bundle_verify` reads
        // `argv[0]` alone — so everything after the file is ignored, an
        // unrecognised switch included.
        if file.is_some() {
            continue;
        }
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
            s => file = Some(s),
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
    // `int version = -1`, which `create_bundle()` reads as "pick the minimum".
    let mut version: Option<i64> = None;
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
        // `PARSE_OPT_STOP_AT_NON_OPTION` (builtin/bundle.c:104): the `<file>`
        // operand ends option parsing, so everything after it is handed to
        // `setup_revisions()` — which is why `git bundle create <file> --progress`
        // reports an unrecognized argument while `--progress <file>` is accepted.
        if file.is_some() {
            rev_args.push(a);
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
                match parse_version(v) {
                    Ok(n) => version = Some(n),
                    Err(code) => return Ok(code),
                }
                i += 1;
            }
            _ if a.starts_with("--version=") => {
                match parse_version(&a["--version=".len()..]) {
                    Ok(n) => version = Some(n),
                    Err(code) => return Ok(code),
                }
            }
            _ => return Ok(bad_option(a.trim_start_matches('-'), CREATE_USAGE, !a.starts_with("--"))),
        }
        i += 1;
    }

    let Some(file) = file else {
        return Ok(need_file(CREATE_USAGE));
    };

    let repo = gix::discover(".")?;
    let (pending, pathspecs) = match resolve_revisions(&repo, &rev_args)? {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    // `setup_revisions()`'s tail: `parse_pathspec(&revs->prune_data, …)` over
    // whatever reached `prune_data`, which is where a `..` that never was a
    // range finally lands. It runs inside `setup_revisions()`, so it precedes
    // everything `create_bundle()` does afterwards — including
    // `Refusing to create empty bundle.` for the pending list it just left
    // empty.
    if let Some(msg) = crate::pathspec::parse_pathspec_fatal(&repo, &pathspecs) {
        eprintln!("fatal: {msg}");
        return Ok(ExitCode::from(128));
    }

    // `if (version == -1) version = min_version;` — 2 for sha1, and only 2 or 3
    // exist (bundle.c:525-531). The v2 header carries no `@object-format`
    // capability, so it can only describe sha1: a sha256 repository defaults to
    // 3, and an explicit `--version=2` is refused rather than written as a
    // bundle whose reader would take 64-hex ids for 40-hex ones.
    let sha1_repo = repo.object_hash() == gix::hash::Kind::Sha1;
    // `-1` is the sentinel `cmd_bundle_create` starts from, so an explicit
    // `--version=-1` takes the same default as no `--version` at all.
    let version = version.filter(|v| *v != -1).unwrap_or(if sha1_repo { 2 } else { 3 });
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
    //
    // The pending list is kept as git pends it — unpeeled — because that is what
    // `write_bundle_refs()` and `write_pack_data()` read (`revs_copy.pending`,
    // bundle.c:576-587), and an annotated tag has to reach the pack as a tag.
    // The *walk* sees `prepare_revision_walk()`'s view instead: every entry goes
    // through `handle_commit()`, whose tag loop peels down to the commit and
    // carries the flags with it (`object->flags |= flags`, revision.c). Without
    // that peel a tag tip walks nothing at all, which is why
    // `git bundle create <file> v1 ^v1^` reported a complete history where stock
    // 2.55.0 lists three prerequisites.
    let want_objects: Vec<ObjectId> =
        pending.iter().filter(|p| !p.uninteresting).map(|p| p.id).collect();
    let excluded: Vec<ObjectId> =
        pending.iter().filter(|p| p.uninteresting).map(|p| p.id).collect();
    let tips: Vec<ObjectId> = want_objects.iter().map(|id| peel_to_commit(&repo, *id)).collect();
    let hidden: Vec<ObjectId> = excluded.iter().map(|id| peel_to_commit(&repo, *id)).collect();
    // `UNINTERESTING` sits on the object, and by the time the header is written git
    // has already run the walk that spreads it to every ancestor of a `^<rev>`. Both
    // the prerequisite scan and the ref list below read it back off the objects they
    // hold, so the closure is computed once here.
    let excluded_closure = if hidden.is_empty() {
        std::collections::HashSet::new()
    } else {
        super::log::ancestor_closure(&repo, &hidden)?
    };
    let prereqs = boundary_commits(&repo, &tips, &hidden, &excluded_closure);
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
    // ```c
    // /* end header */
    // write_or_die(bundle_fd, "\n", 1);
    // return ref_count;
    // ```
    //
    // (`write_bundle_refs()`, bundle.c.) The blank line that ends the header goes out
    // *before* the count is returned, so it is there even for the bundle that turns out
    // to be empty.
    out.push(b'\n');
    if seen.is_empty() {
        // git writes the header through `write_or_die(bundle_fd, …)` as it builds it
        // (bundle.c:533-547), so by the time `create_bundle()` sees a zero ref count the
        // whole header has already reached the destination. For a file that destination
        // is a lockfile the error path rolls back and nothing survives; for `-` it is
        // stdout, and the two lines are already out.
        if file == "-" {
            io::stdout().write_all(&out)?;
            io::stdout().flush()?;
        }
        eprintln!("fatal: Refusing to create empty bundle.");
        return Ok(ExitCode::from(128));
    }

    // `write_pack_data()`: the objects reachable from the tips and not from the
    // prerequisites. git's pack is thin (its deltas may name bases the receiver
    // already has); this one is not, so it carries every object it references.
    // A non-thin pack is a strictly self-contained superset — `unbundle` and
    // `index-pack --fix-thin` accept it unchanged — so the bundle is correct,
    // but its bytes are not git's. See the module header.
    //
    // The wants are the pending entries as typed, so a tag tip is packed as a
    // tag; the haves are the peeled ones, because `pack-objects` peels a `^<tag>`
    // itself and a receiver that already has the commit is what the exclusion
    // means.
    let mut haves = prereqs.clone();
    haves.extend_from_slice(&hidden);
    let objects = crate::porcelain::push_proto::objects_to_send(&repo, &want_objects, &haves);
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

/// `--version=<n>`: git's `OPT_INTEGER` against a C `int`, so a non-numeric value
/// and one outside `[-2147483648, 2147483647]` are both parse-options' own usage
/// error (exit 129) — reported before the version range is ever looked at, and
/// with `parse-options`' two distinct texts. A value inside the `int` range but
/// outside `[2, 3]` is `create_bundle()`'s later fatal, not this one.
fn parse_version(v: &str) -> std::result::Result<i64, ExitCode> {
    crate::optint::integer(&crate::optint::long_opt("version"), v).map_err(|e| {
        eprintln!("error: {}", e.message());
        ExitCode::from(129)
    })
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

/// One element of the revision argv, after `--stdin` has been spliced in where
/// it stood.
struct Item {
    text: String,
    /// Read by `read_revisions_from_stdin()` rather than typed on the command
    /// line, which changes two things: the line is handled with
    /// `REVARG_CANNOT_BE_FILENAME`, and `warn_on_object_refname_ambiguity` is
    /// off for the whole block.
    from_stdin: bool,
}

/// `setup_revisions()` reduced to the grammar `bundle create` is documented
/// with: the ref-selecting pseudo-options (`--all`, `--branches`, `--tags`,
/// `--remotes`, `--glob`, each filtered by the `--exclude` patterns it consumes),
/// `--stdin`, `<rev>`, `^<rev>`, `<a>..<b>` and `<a>...<b>`.
///
/// `builtin/bundle.c:104` hands its whole post-`parse_options` argv to
/// `create_bundle()`, which calls `setup_revisions()` (bundle.c:501) — so the
/// pseudo-option family reaches `git bundle create` unchanged.
///
/// Returns the pending list *and* the `prune_data` git collected alongside it:
/// an operand that is not a revision at all does not disappear, it becomes a
/// pathspec, and `setup_revisions()` parses that list before it returns.
fn resolve_revisions(
    repo: &gix::Repository,
    args: &[&str],
) -> Result<std::result::Result<(Vec<Pending>, Vec<Vec<u8>>), ExitCode>> {
    let mut pending = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut negate = false;

    // `setup_revisions()`'s first act, before it looks at a single argument:
    //
    // ```c
    // for (i = 1; i < argc; i++) {
    //         const char *arg = argv[i];
    //         if (strcmp(arg, "--"))
    //                 continue;
    //         argv[i] = NULL;
    //         argc = i;
    //         if (argv[i + 1])
    //                 strvec_pushv(&prune_data, argv + i + 1);
    //         seen_dashdash = 1;
    //         break;
    // }
    // ```
    //
    // (revision.c:2836-2851). The argv is *cut* at the `--`, everything after it
    // is prune data, and every surviving argument then carries
    // `REVARG_CANNOT_BE_FILENAME` — which is what stops a `..` in front of the
    // `--` from being the parent-directory pathspec.
    let mut pathspecs: Vec<Vec<u8>> = Vec::new();
    let mut args = args;
    let mut seen_dashdash = false;
    if let Some(cut) = args.iter().position(|a| *a == "--") {
        pathspecs.extend(args[cut + 1..].iter().map(|a| a.as_bytes().to_vec()));
        args = &args[..cut];
        seen_dashdash = true;
    }

    let mut items: Vec<Item> =
        args.iter().map(|a| Item { text: (*a).to_string(), from_stdin: false }).collect();
    let mut read_from_stdin = false;

    let mut i = 0;
    while i < items.len() {
        // Cloned rather than borrowed: `--stdin` splices its lines into `items`
        // from inside this same iteration.
        let text = items[i].text.clone();
        let a = text.as_str();
        let from_stdin = items[i].from_stdin;
        // `read_revisions_from_stdin()` saves and clears
        // `warn_on_object_refname_ambiguity` around the whole block, so no line
        // it reads can warn about an ambiguous refname.
        let _quiet = from_stdin.then(crate::objname::AmbiguityWarnings::off);
        // `REVARG_CANNOT_BE_FILENAME` reaches `handle_revision_arg_1()` from two
        // places: `setup_revisions()` sets it for every argument once it has
        // found a `--` of its own, and `read_revisions_from_stdin()` passes it
        // unconditionally for each line it reads. So a `..` typed on the command
        // line is the parent-directory pathspec while the same `..` fed through
        // `--stdin` is the range `HEAD..HEAD`.
        let cant_be_filename = seen_dashdash || from_stdin;
        // `--exclude=<glob>` only accumulates; the next ref-selecting option
        // applies and clears it (`clear_ref_exclusions`).
        if let Some(v) = a.strip_prefix("--exclude=") {
            excludes.push(v.to_string());
            i += 1;
            continue;
        }
        if a == "--exclude" {
            i += 1;
            let Some(v) = items.get(i) else {
                anyhow::bail!("option 'exclude' requires a value");
            };
            excludes.push(v.text.clone());
            i += 1;
            continue;
        }
        if a == "--not" {
            negate = !negate;
            i += 1;
            continue;
        }
        // `--glob` takes its value attached or as the next argv element.
        let glob_value = if a == "--glob" {
            i += 1;
            match items.get(i) {
                Some(v) => Some(v.text.clone()),
                None => anyhow::bail!("option 'glob' requires a value"),
            }
        } else {
            None
        };
        if let Some((kind, attached)) = super::log::ref_selector(a) {
            let sel = super::log::RefSelection::new(
                0,
                kind,
                attached.or(glob_value.as_deref()),
                std::mem::take(&mut excludes),
                negate,
            );
            // `handle_refs(for_each_ref)` then `handle_refs(head_ref)`
            // (revision.c) — which is why `HEAD` lands after the ref list.
            for r in repo.references()?.all()? {
                let Ok(r) = r else { continue };
                let full = r.name().as_bstr().to_string();
                if sel.selects(&full).is_none() {
                    continue;
                }
                if let Some(id) = r.try_id() {
                    // `write_bundle_refs()` re-dwims each pending name through
                    // `repo_dwim_ref()`, so a `--branches` entry named `topic`
                    // comes back out as `refs/heads/topic`.
                    pending.push(Pending {
                        id: id.detach(),
                        display_ref: Some(full),
                        uninteresting: negate,
                    });
                }
            }
            if sel.head && !sel.excluded("HEAD") {
                if let Ok(head) = repo.head_id() {
                    pending.push(Pending {
                        id: head.detach(),
                        display_ref: Some("HEAD".into()),
                        uninteresting: negate,
                    });
                }
            }
            i += 1;
            continue;
        }
        // `--stdin`, which `setup_revisions()` reads *at the point it stands in
        // argv* (revision.c:2872-2879) — so the lines join the pending list
        // between the arguments on either side of it, and an earlier argument
        // that dies is still the one that gets reported.
        //
        // ```c
        // if (!strcmp(arg, "--stdin")) {
        //         if (revs->disable_stdin) { argv[left++] = arg; continue; }
        //         if (revs->read_from_stdin++)
        //                 die("--stdin given twice?");
        //         read_revisions_from_stdin(revs, &prune_data);
        //         continue;
        // }
        // ```
        //
        // `create_bundle()` leaves `disable_stdin` at 0, so the branch is live.
        if a == "--stdin" && !items[i].from_stdin {
            if read_from_stdin {
                eprintln!("fatal: --stdin given twice?");
                return Ok(Err(ExitCode::from(128)));
            }
            read_from_stdin = true;
            let mut lines = Vec::new();
            if let Err(code) = read_revisions_from_stdin(&mut lines, &mut pathspecs) {
                return Ok(Err(code));
            }
            items.splice(i + 1..i + 1, lines);
            i += 1;
            continue;
        }
        // `if (argc > 1) error(_("unrecognized argument: %s"), argv[1])`
        // (bundle.c:503-506): whatever `setup_revisions()` left behind. `bundle
        // create`'s own switches are exactly that once the `<file>` operand has
        // ended option parsing, which is why `git bundle create <file> -q` is an
        // error while `git bundle create -q <file>` is not.
        //
        // (git 2.55.0 aborts on this path — one `error:` line, then SIGABRT, so a
        // shell sees 134. This returns the 255 an `error()` return normally
        // becomes, and writes no bundle, which is the part that matters.)
        if matches!(a, "-q" | "--quiet" | "--progress" | "--all-progress" | "--all-progress-implied")
            || a == "--version"
            || a.starts_with("--version=")
        {
            eprintln!("error: unrecognized argument: {a}");
            return Ok(Err(ExitCode::from(255)));
        }
        // `handle_revision_arg_1()`'s very first test, ahead of everything
        // below:
        //
        // ```c
        // if (!cant_be_filename && !strcmp(arg, "..")) {
        //         /*
        //          * Just ".."?  That is not a range but the
        //          * pathspec for the parent directory.
        //          */
        //         return -1;
        // }
        // ```
        //
        // (revision.c:2164). The `-1` sends `setup_revisions()` down its
        // `verify_filename()` branch, which pushes this operand *and every one
        // after it* into `prune_data` and stops reading revisions
        // (revision.c:2896-2912) — so `git bundle create <file> ..` is the
        // pathspec layer's `'..' is outside repository`, not a revision error.
        if crate::objname::is_parent_directory_pathspec(a, cant_be_filename) {
            // ```c
            // for (j = i; j < argc; j++)
            //         verify_filename(revs->prefix, argv[j], j == i);
            // strvec_pushv(&prune_data, argv + i);
            // break;
            // ```
            //
            // `j == i` is `diagnose_misspelt_rev`, so only the operand that just
            // failed as a revision gets the ambiguous-argument wording; a later
            // one is already known to stand in path position and gets the
            // shorter `no such path in the working tree.` instead.
            for (n, item) in items[i..].iter().enumerate() {
                if let Some(msg) = crate::setup::verify_filename(&item.text, n == 0) {
                    eprintln!("fatal: {msg}");
                    return Ok(Err(ExitCode::from(128)));
                }
            }
            pathspecs.extend(items[i..].iter().map(|it| it.text.as_bytes().to_vec()));
            break;
        }
        // `handle_dotdot()`, which runs before the three-mark block below and is
        // the *whole* of the range rule: both endpoints through
        // `get_oid_with_context()`, `parse_object()` on each, and — for
        // `<a>...<b>` only — `lookup_commit_reference()` on each. Asked of
        // [`crate::objname`] rather than re-derived here, which is what brings
        // the symmetric form along: the `split_once("..")` that used to stand in
        // this spot read `<a>...<b>` as `<a>` against `.<b>` and could only fail.
        let range = crate::objname::split_range(a).map(|r| {
            // The `warning: refname … is ambiguous.` half of those two
            // `get_oid_with_context()` calls. [`crate::objname::dotdot`] is quiet
            // by design — it is a classifier every caller asks more than once —
            // so the warning is requested separately, exactly once per operand,
            // and the endpoints below are never resolved a second time.
            crate::objname::warn_dotdot_endpoints(repo, a);
            (r, crate::objname::dotdot(repo, a))
        });
        if let Some((r, crate::objname::Dotdot::Missing { .. })) = &range {
            // `dotdot_missing()`, with whatever `lookup_commit_reference()`
            // already printed ahead of it.
            eprint!(
                "{}",
                crate::objname::dotdot_fatal(repo, a).unwrap_or_else(|| format!(
                    "fatal: {}\n",
                    crate::objname::dotdot_missing_message(a, r.symmetric)
                ))
            );
            return Ok(Err(ExitCode::from(128)));
        }
        if let Some((r, crate::objname::Dotdot::Ok { a: a_oid, b: b_oid })) = range {
            // `handle_dotdot_1()`'s pending order, which is the order the header's
            // ref list comes out in. For `<a>...<b>` the merge bases go first
            // (`add_pending_commit_list(revs, exclude, flags_exclude)`,
            // revision.c:2052), then the left endpoint, then the right.
            //
            // The ids that get pended are `a_obj`/`b_obj` — what `parse_object()`
            // returned for the names, *unpeeled*. `lookup_commit_reference()`
            // runs only to feed `get_merge_bases()`, and its result is never
            // pended, which is why `git bundle create <file> v1...main` writes
            // the tag's own id under `refs/tags/v1` and not the commit's.
            // [`crate::objname::Dotdot`] hands back the peeled pair for the
            // symmetric form, so the raw ones are re-read here from the same
            // quiet resolution it used.
            let (a_raw, b_raw) = match (
                crate::objname::resolve_quiet(repo, r.a),
                crate::objname::resolve_quiet(repo, r.b),
            ) {
                (Some(a_raw), Some(b_raw)) => (a_raw, b_raw),
                _ => (a_oid, b_oid),
            };
            if r.symmetric {
                // Each base is pended under `oid_to_hex()` rather than a name, so
                // `repo_dwim_ref()` finds nothing for it and it never reaches the
                // ref list — only the prerequisite walk.
                for base in repo.merge_bases_many(a_oid, &[b_oid])? {
                    pending.push(Pending {
                        id: base.detach(),
                        display_ref: None,
                        uninteresting: !negate,
                    });
                }
                // `b_flags = flags` and `a_flags = flags | SYMMETRIC_LEFT`: both
                // ends of a symmetric difference are interesting, and only the
                // bases carry `flags_exclude`.
                pending.push(Pending {
                    id: a_raw,
                    display_ref: display_ref(repo, r.a),
                    uninteresting: negate,
                });
            } else {
                // `a_flags = flags_exclude`: the left end of `<a>..<b>` is the
                // excluded one, and a preceding `--not` flips both.
                pending.push(Pending {
                    id: a_raw,
                    display_ref: display_ref(repo, r.a),
                    uninteresting: !negate,
                });
            }
            pending.push(Pending {
                id: b_raw,
                display_ref: display_ref(repo, r.b),
                uninteresting: negate,
            });
            i += 1;
            continue;
        }
        // `handle_revision_arg_1()`'s three-mark block, which runs before the
        // operand is resolved at all — `get_oid_1()` has no case for `^@`, `^!`
        // or `^-<n>`, so a `git bundle create - HEAD^!` that skips it can only
        // fail. See [`crate::objname::parents_only`] for the C.
        //
        // The parents are recorded under the *base* name, which is what makes
        // `write_bundle_refs()` write `<parent> HEAD` for `HEAD^@`: it dwims
        // `e->name`, not the object.
        let a: &str = match crate::objname::parents_only(a) {
            // No mark, or a parent number `handle_revision_arg_1()` refused
            // before `add_parents_only()` was reached — both hand the operand on
            // exactly as typed, and a refused number then fails to resolve.
            crate::objname::ParentsOnly::Absent | crate::objname::ParentsOnly::BadParent => a,
            crate::objname::ParentsOnly::Mark { base, nth, replaces } => {
                // `^@` keeps `flags`; `^!` and `^-<n>` queue their parents under
                // `flags ^ (UNINTERESTING | BOTTOM)`, so a preceding `--not`
                // flips all three.
                let sense = if replaces { negate } else { !negate };
                let mut queue = |name: &str, parent, uninteresting| {
                    pending.push(Pending {
                        id: parent,
                        display_ref: display_ref(repo, name),
                        uninteresting,
                    });
                };
                match crate::objname::add_parents_only(repo, base, sense, nth, &mut queue) {
                    // `get_reference()`'s `die(_("bad object %s"), name)` from
                    // inside the tag-peeling loop, naming the base.
                    crate::objname::Parents::BadObject => {
                        let name = crate::objname::uninteresting_mark(base).0;
                        eprintln!("fatal: bad object {name}");
                        return Ok(Err(ExitCode::from(128)));
                    }
                    // `add_parents_only()` answered 0, so `arg` is untouched and
                    // the operand goes on carrying its mark.
                    crate::objname::Parents::None => a,
                    // `^@` alone returns from `handle_revision_arg_1()`: the
                    // named commit itself never joins the pending list.
                    crate::objname::Parents::Queued if replaces => {
                        i += 1;
                        continue;
                    }
                    // `arg = arg_minus_excl`, so `HEAD^!` goes on to pend `HEAD`
                    // beside the parents it just excluded.
                    crate::objname::Parents::Queued => base,
                }
            }
        };
        // `if (*arg == '^') { local_flags = UNINTERESTING | BOTTOM; arg++; }`,
        // then the single `get_oid_with_context()` for whatever is left.
        // `setup_revisions()` reports an unresolvable revision itself, with the
        // token as written and its own exit code — the same message `git log`
        // raises, since it is the same function.
        let (spec, uninteresting) = match a.strip_prefix('^') {
            Some(rest) => (rest, !negate),
            None => (a, negate),
        };
        match one_pending(repo, spec, uninteresting) {
            Ok(p) => pending.push(p),
            Err(_) => {
                // `read_revisions_from_stdin()` has its own refusal —
                // `die("bad revision '%s'", sb.buf)` — so a line it read never
                // reaches `setup_revisions()`' filename fallback and is named
                // whole, exclusion mark and all.
                if from_stdin {
                    eprintln!("fatal: bad revision '{a}'");
                } else {
                    eprint!("{}", super::log::bad_revision_message_in(repo, a));
                }
                return Ok(Err(ExitCode::from(128)));
            }
        }
        i += 1;
    }
    Ok(Ok((pending, pathspecs)))
}

/// git's `read_revisions_from_stdin()` (revision.c), the whole of it:
///
/// ```c
/// while (strbuf_getline(&sb, stdin) != EOF) {
///         int len = sb.len;
///         if (!len)
///                 break;
///         if (sb.buf[0] == '-') {
///                 if (len == 2 && sb.buf[1] == '-') {
///                         seen_dashdash = 1;
///                         break;
///                 }
///                 die(_("invalid option '%s' in --stdin mode"), sb.buf);
///         }
///         if (handle_revision_arg(sb.buf, revs, 0, REVARG_CANNOT_BE_FILENAME))
///                 die("bad revision '%s'", sb.buf);
/// }
/// if (seen_dashdash)
///         read_pathspec_from_stdin(&sb, prune);
/// ```
///
/// An *empty* line ends the revision list, a lone `--` ends it and hands every
/// remaining line to the pathspec list, and any other line starting with `-` is
/// fatal — `--stdin` takes no options, not even the ones the command line
/// accepts. The lines themselves are handed back for the caller to process in
/// place, because git processes them where `--stdin` stood.
fn read_revisions_from_stdin(
    lines: &mut Vec<Item>,
    pathspecs: &mut Vec<Vec<u8>>,
) -> std::result::Result<(), ExitCode> {
    use std::io::BufRead;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut seen_dashdash = false;
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // `strbuf_getline()` strips the terminator and a CR in front of it.
        let text = line.trim_end_matches('\n').trim_end_matches('\r').to_string();
        if text.is_empty() {
            break;
        }
        if text.starts_with('-') {
            if text == "--" {
                seen_dashdash = true;
                break;
            }
            // 2.55.0's wording, which names the offending line:
            // `die(_("invalid option '%s' in --stdin mode"), sb.buf)`.
            eprintln!("fatal: invalid option '{text}' in --stdin mode");
            return Err(ExitCode::from(128));
        }
        lines.push(Item { text, from_stdin: true });
    }
    if seen_dashdash {
        // `read_pathspec_from_stdin()`: every remaining line, to EOF, verbatim.
        for line in input.lines().map_while(std::result::Result::ok) {
            pathspecs.push(line.into_bytes());
        }
    }
    Ok(())
}

/// Resolve one revision argument, recording the name `write_bundle_refs` would
/// print for it.
fn one_pending(repo: &gix::Repository, spec: &str, uninteresting: bool) -> Result<Pending> {
    // `cmd_bundle_create()` hands the operands to `setup_revisions()`, so each
    // name reaches `get_oid_basic()` once — including each endpoint of a range,
    // which the caller has already split. [`crate::objname::resolve`] is that
    // call, ambiguity warning included.
    //
    // The object still has to be present: `get_reference()` `parse_object()`s
    // whatever `get_oid_basic()` decoded and dies `bad object <name>` when it is
    // not there, which is the message the caller's `bad_revision_message_in()`
    // produces for this `Err`.
    let id = crate::objname::resolve(repo, spec)
        .filter(|id| repo.find_object(*id).is_ok())
        .ok_or_else(|| anyhow::anyhow!("bad revision '{spec}'"))?;
    Ok(Pending { id, display_ref: display_ref(repo, spec), uninteresting })
}

/// `write_bundle_refs()`'s `display_ref` (bundle.c:398-403): `repo_dwim_ref()`
/// decides whether the pending entry's *name* is a ref at all, and a symref
/// keeps the name as typed — which is what makes `HEAD` print as `HEAD` rather
/// than as its target.
///
/// It is asked about the name git recorded, not about the object: the parents
/// `add_parents_only()` queues for `HEAD^@` are named `HEAD`, so they come back
/// out of the bundle header under that name even though `HEAD` points elsewhere.
fn display_ref(repo: &gix::Repository, name: &str) -> Option<String> {
    // ```c
    // if (repo_dwim_ref(revs->repo, e->name, strlen(e->name), &oid, &ref, 0) != 1)
    //         goto skip_write_ref;
    // ```
    //
    // `!= 1`, not `== 0`: a name that answers to *several* refs is skipped just
    // like one that answers to none. `git bundle create <file> dup`, with `dup`
    // both a branch and a tag, therefore writes no ref at all and refuses the
    // empty bundle — where taking gitoxide's own dwim, which simply picks one,
    // wrote `refs/tags/dup` and a bundle stock never produces.
    if crate::porcelain::rev_parse::dwim_ref_matches(repo, name).len() != 1 {
        return None;
    }
    match repo.find_reference(name) {
        Ok(r) => {
            let is_symref = matches!(r.target(), gix::refs::TargetRef::Symbolic(_));
            Some(if is_symref { name.to_string() } else { r.name().as_bstr().to_string() })
        }
        Err(_) => None,
    }
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
    // The prerequisite lines come out in the order `get_revision_1()` first met
    // each boundary parent (`revision.c:4583-4591` appends to
    // `revs->boundary_commits`; `create_boundary_commit_list()` reverses it and
    // `sort_in_topological_order()` with the default `REV_SORT_IN_GRAPH_ORDER`
    // — a LIFO priority queue — reverses it back). So the walk has to be git's
    // own commit-date order, not gitoxide's default breadth-first.
    let walk = repo
        .rev_walk(tips.to_vec())
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ))
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
    // `create_boundary_commit_list()` (revision.c:4171-4207) then drains
    // `revs->boundary_commits` into `revs->commits` with `commit_list_insert()`,
    // which *prepends*:
    //
    // ```c
    // for (i = 0; i < array->nr; i++) {
    //         c = (struct commit *)(objects[i].item);
    //         ...
    //         c->object.flags |= BOUNDARY;
    //         commit_list_insert(c, &revs->commits);
    // }
    // sort_in_topological_order(&revs->commits, revs->sort_order);
    // ```
    //
    // So the list reaches the sort in the reverse of the order the walk met each
    // parent — and then the sort runs unconditionally, with `revs->sort_order`
    // still at its `REV_SORT_IN_GRAPH_ORDER` default because nothing on this
    // path sets `--date-order`.
    boundary.reverse();
    sort_boundary_in_topological_order(repo, boundary)
}

/// `sort_in_topological_order(&revs->commits, REV_SORT_IN_GRAPH_ORDER)` applied
/// to the boundary list, by handing it to the port `fast-export` already
/// carries rather than writing a second one.
///
/// Reversing alone is not the whole of `create_boundary_commit_list()`: the
/// prerequisites of `bundle create <file> main~4 side^ ^main~5` are `C` and then
/// `B`, and `B` is `C`'s parent, so the sort is what puts the child in front of
/// its parent no matter which order the date-ordered walk met them in.
///
/// The `Info` values are built here because that port speaks git's commit list;
/// `commit_time` is `None` because `REV_SORT_IN_GRAPH_ORDER` is the `compare ==
/// NULL` prio-queue, which never looks at a date.
fn sort_boundary_in_topological_order(
    repo: &gix::Repository,
    ids: Vec<ObjectId>,
) -> Vec<ObjectId> {
    let mut list: Vec<gix::traverse::commit::Info> = Vec::with_capacity(ids.len());
    for id in &ids {
        // Every boundary id is a commit the walk just read a parent link from,
        // so this cannot fail; if it somehow does, the un-sorted list is still
        // the complete prerequisite set and is returned rather than truncated.
        let Ok(commit) = repo.find_commit(*id) else { return ids };
        list.push(gix::traverse::commit::Info {
            id: *id,
            parent_ids: commit.parent_ids().map(|p| p.detach()).collect(),
            commit_time: None,
        });
    }
    super::fast_export::sort_in_topological_order(list, super::fast_export::Order::Topo)
        .into_iter()
        .map(|info| info.id)
        .collect()
}

/// `handle_commit()`'s tag loop (revision.c), which is how every pending entry
/// reaches the walk:
///
/// ```c
/// while (object->type == OBJ_TAG) {
///         struct tag *tag = (struct tag *) object;
///         ...
///         object = parse_object(revs->repo, get_tagged_oid(tag));
///         ...
///         object->flags |= flags;
/// }
/// ```
///
/// The flags ride down to the commit, so an annotated tag named with `^` excludes
/// its commit's history and one named as a tip walks it. Only the *walk* sees
/// this: `revs_copy.pending` keeps the tag object, which is what puts the tag
/// itself in the pack and its ref in the header.
fn peel_to_commit(repo: &gix::Repository, id: ObjectId) -> ObjectId {
    let Ok(object) = repo.find_object(id) else { return id };
    object.peel_to_kind(gix::object::Kind::Commit).map_or(id, |commit| commit.id)
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
        // `PARSE_OPT_STOP_AT_NON_OPTION`: the `<file>` operand ends option
        // parsing, so every later token is a `<refname>` filter — even one that
        // looks like a switch.
        if file.is_some() {
            filters.push(a.as_bytes());
            continue;
        }
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
            s => file = Some(s),
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
    let status = std::process::Command::new(crate::hosted::git_exe()?)
        .current_dir(cwd)
        .args(["index-pack", "--fix-thin", "--stdin"])
        .args(extra_args)
        .stdin(source.into_stdio())
        .stdout(Stdio::null())
        .status()?;
    Ok(status.success())
}
