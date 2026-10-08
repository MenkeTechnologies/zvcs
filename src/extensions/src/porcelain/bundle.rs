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
//!     `The bundle uses this hash algorithm: …` / `The bundle uses this filter: …`
//!     report on stdout and the
//!     `<file> is okay` line on stderr — the report held in the stdio
//!     buffer, so captured together the `is okay` line comes first, including all three failure paths
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
//!     first byte of the pack. A bundle whose header carries `@filter` adds
//!     `--promisor=from-bundle` to that child (bundle.c:632-634), so the
//!     installed pack gets its `.promisor` marker and the objects the filter
//!     dropped read as absent-on-purpose rather than as corruption
//!   * `-h` for `bundle` itself and for each of the four subcommands (usage to
//!     stdout, exit 0), plus `need a subcommand`, `unknown subcommand`,
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
//!     next of them consumes) all reach it, as does `--filter=<spec>`, which
//!     raises the bundle version to 3, writes the `@filter` capability and
//!     narrows the pack. So does the half of that grammar
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
//!     **The whole file is byte-identical to git's**, header and pack alike, on
//!     every shape the harness measures. It was not always, and the three
//!     reasons it was not are worth keeping because each names the piece that
//!     had to be built:
//!
//!       1. *Object order.* git's `compute_write_order()` groups the pack by
//!          type (tagged tips, then remaining commits and tags, then trees, then
//!          the rest) and keeps delta families contiguous. This module once
//!          wrote in `HashSet<ObjectId>` iteration order, so the order was
//!          neither git's nor stable between two runs. Closed by
//!          `push_proto::objects_to_send()` returning the traversal's own order
//!          and by `pack_objects::compute_write_order()`.
//!       2. *Deflate output.* zvcs compresses through `zlib-rs` where stock git
//!          links zlib, so nothing pins the two together by construction. They
//!          have agreed on every corpus measured; see the `pack_objects` module
//!          header for what was checked and what a divergence would look like.
//!       3. *Thinness.* git passes `--thin`, so its deltas may name bases the
//!          receiver already has. Closed: the prerequisites — which are the
//!          walk's edge commits, collected by
//!          `compute_and_write_prerequisites()` from `rev-list --boundary` —
//!          now go to the pack writer as its boundary, and
//!          `pack_objects::interleave_preferred_bases()` turns their trees into
//!          the delta bases git's `add_preferred_base()` makes of them. With
//!          `--all` there are no prerequisites, so this was always inert there;
//!          over a range it is what makes `bundle create - main..div` the same
//!          429 bytes as stock rather than 560.
//!
//!     Delta *base selection* is not a cause — on `branched` gitoxide picks the
//!     same base and emits a byte-identical delta payload — and the deltas are
//!     `OBJ_OFS_DELTA` wherever the base is in the pack, because
//!     `write_pack_data()` spawns `pack-objects --stdout --thin
//!     --delta-base-offset` unconditionally (`bundle.c:333-336`); a base the
//!     thinness left outside is named by id, which is git's own
//!     `DELTA(entry)->idx.offset` test.
//!
//!     `create`'s options are `PARSE_OPT_STOP_AT_NON_OPTION`, so the `<file>`
//!     operand ends option parsing: `git bundle create <file> -q` reports
//!     `error: unrecognized argument: -q` and writes nothing, exactly as stock
//!     does, including the death that follows where the allocator notices: the
//!     `goto out` at bundle.c:515 reaches `object_array_clear(&revs_copy.pending)`
//!     (:600) before `revs_copy` is initialised (:551), so git 2.54.0, 2.55.0 and
//!     2.56.0 on macOS print that line and then abort (a shell sees 134). That free is
//!     undefined behaviour: on glibc stock git exits 1 instead. This follows the host.
//!
//!     Everything after `<file>` is `setup_revisions()`'s, so rev-list options are
//!     accepted there. `claim_revision_opt()` is that function's option chain: the
//!     count-and-age arm ([`crate::revopt`]), [`REVISION_OPTS`], then
//!     `diff_opt_parse()`. A word none of them claims is remembered and reported
//!     once the line is read, so a bad revision later on it dies first (128). An
//!     option that only shapes output is dropped; one that limits the walk
//!     (`--max-count`, `--since`, `--no-merges`, a pathspec, ...) sends the revision
//!     words through `rev-list --boundary` ([`limited_walk`]), whose boundary lines
//!     are the prerequisites and whose other lines are the commits `SHOWN` — a tip
//!     missing from them is `ref '<name>' is excluded by the rev-list options`.
//!
//!     Gaps: the pseudo-options `--reflog`, `--alternate-refs`, `--indexed-objects`,
//!     `--bisect`, `--ignore-missing` and `--single-worktree` are not implemented and
//!     end as unrecognized arguments; and `--left-only` / `--right-only` / `--cherry`
//!     over a symmetric difference keep the commits they hide (`SHOWN` without being
//!     emitted) only as the set a walk without them shows, which differs from git when
//!     a limit such as `--max-count` is also given.
//!
//! One deliberate gap, so this doc claims no more than the code does: a header
//! that parses as neither a capability nor a ref line is surfaced as a plain
//! error rather than git's `unrecognized header:` text. A capability that is
//! neither `@object-format` nor `@filter` is `error: unknown capability '<cap>'`
//! at exit 1, which is what `parse_capability()` (bundle.c) reports.
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
            Ok(super::show_usage(TOP_USAGE))
        }
        "create" => create(rest),
        "verify" => verify(rest),
        "list-heads" => list_heads(rest),
        "unbundle" => unbundle(rest),
        // `parse_options_step()` consumes a lone `--` before any table lookup
        // (parse-options.c: `if (!arg[2]) { ... ctx->argc--; ctx->argv++; break; }`),
        // so it is never an unknown option. What is left is a command line with
        // no sub-command word in it, which is `PARSE_OPT_SUBCOMMAND`'s own
        // refusal — the same one an empty argv gets.
        "--" => {
            eprint!("error: need a subcommand\n{TOP_USAGE}");
            Ok(ExitCode::from(129))
        }
        s if s.starts_with("--") => Ok(bad_option(s, TOP_USAGE)),
        s if s.starts_with('-') && s.len() > 1 => Ok(bad_option(s, TOP_USAGE)),
        s => {
            eprint!("error: unknown subcommand: `{s}'\n{TOP_USAGE}");
            Ok(ExitCode::from(129))
        }
    }
}

/// git's parse-options diagnostic for an unrecognised option, plus the usage
/// block of the (sub)command that rejected it. Exit 129, both on stderr.
///
/// `tok` is the argument **with** its dashes, because which of the two
/// diagnostics applies — and, for a short one, how much of the token is named —
/// is [`crate::parseopt::unknown_option`]'s decision and not this module's. The
/// private copy this replaced named a whole cluster (`git bundle -7q` reported
/// ``unknown switch `7q'``) where `*ctx->opt` is a single character, and had no
/// arm at all for the non-ASCII spelling.
fn bad_option(tok: &str, usage: &str) -> ExitCode {
    crate::parseopt::unknown_option(tok, usage)
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
    /// The `@filter` capability's value, when the bundle carries one. `verify`
    /// reports it as `The bundle uses this filter: <spec>`, and `create` writes it
    /// back from the `--filter` the revision walk carried.
    pub(crate) filter: Option<String>,
}

/// The failures git reports itself, with its own wording and exit code 1.
pub(crate) enum HeaderError {
    /// `error: could not open '<file>'`
    Open,
    /// `error: '<file>' does not look like a v2 or v3 bundle file`
    NotBundle,
    /// Any other `error()` `read_bundle_header_fd()` or `parse_capability()`
    /// reports — `unrecognized header: …`, `unknown capability '<cap>'`,
    /// `unrecognized bundle hash algorithm: <name>` — after which the command
    /// exits 1.
    Error(String),
}

/// Report a [`HeaderError`] the way git does and yield its exit code.
pub(crate) fn report(path: &str, err: HeaderError) -> Result<ExitCode> {
    match err {
        HeaderError::Open => eprintln!("error: could not open '{path}'"),
        HeaderError::NotBundle => {
            eprintln!("error: '{path}' does not look like a v2 or v3 bundle file");
        }
        HeaderError::Error(message) => eprintln!("error: {message}"),
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

/// Read one `\n`-terminated line, keeping the terminator. `Ok(None)` at EOF —
/// including EOF partway through a line, which `strbuf_getwholeline_fd()`
/// (strbuf.c) reports as `EOF` too, dropping the unterminated tail.
///
/// One byte per `read`, as `strbuf_getwholeline_fd` does, so the stream stops on
/// the terminator and not a byte later.
fn read_line(input: &mut dyn Read) -> Result<Option<Vec<u8>>, HeaderError> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => return Ok(None),
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

/// git's `read_bundle_header_fd()` (bundle.c:77-151).
///
/// The signature line must be one of the two `bundle_sigs`; after it, every
/// line up to the first empty one — or up to EOF, which ends the header just as
/// quietly — is a v3 `@capability`, a `-<oid>[ <subject>]` prerequisite or an
/// `<oid> <refname>` tip. A line that is none of those is `unrecognized header:
/// <line> (<len>)`, quoted after `strbuf_rtrim()` and without its `-`, which
/// the message puts back.
fn read_header_from(input: &mut BundleSource) -> Result<Header, HeaderError> {
    let magic = read_line(input)?.ok_or(HeaderError::NotBundle)?;
    let version = match magic.as_slice() {
        b"# v2 git bundle\n" => 2u8,
        b"# v3 git bundle\n" => 3u8,
        _ => return Err(HeaderError::NotBundle),
    };

    // "The default hash format for bundles is SHA1, unless told otherwise by
    // an "object-format=" capability".
    let mut header = Header {
        hash: "sha1".into(),
        prereqs: Vec::new(),
        refs: Vec::new(),
        filter: None,
    };
    let mut hexsz = 40usize;

    while let Some(line) = read_line(input)? {
        if line.first().is_none_or(|&b| b == b'\n') {
            break;
        }
        // `strbuf_rtrim()`
        let end = line.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(0, |i| i + 1);
        let line = &line[..end];

        if version == 3 && line.first() == Some(&b'@') {
            // `parse_capability()` (bundle.c:47-62).
            let cap = String::from_utf8_lossy(&line[1..]).into_owned();
            if let Some(name) = cap.strip_prefix("object-format=") {
                match name {
                    "sha1" => hexsz = 40,
                    "sha256" => hexsz = 64,
                    _ => {
                        return Err(HeaderError::Error(format!(
                            "unrecognized bundle hash algorithm: {name}"
                        )))
                    }
                }
                header.hash = name.to_string();
            } else if let Some(spec) = cap.strip_prefix("filter=") {
                // Recorded verbatim: `verify` echoes it and nothing else reads it.
                header.filter = Some(spec.to_string());
            } else {
                return Err(HeaderError::Error(format!("unknown capability '{cap}'")));
            }
            continue;
        }

        let (is_prereq, body) = match line.strip_prefix(b"-") {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        // ```c
        // if (parse_oid_hex_algop(buf.buf, &oid, &p, header->hash_algo) ||
        //     (*p && !isspace(*p)) ||
        //     (!is_prereq && !*p)) {
        // ```
        let oid = body
            .get(..hexsz)
            .filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
            .and_then(|hex| ObjectId::from_hex(hex).ok());
        let rest = body.get(hexsz..).unwrap_or_default();
        let unrecognized = match oid {
            None => true,
            Some(_) => {
                rest.first().is_some_and(|b| !b.is_ascii_whitespace()) || (!is_prereq && rest.is_empty())
            }
        };
        if unrecognized {
            return Err(HeaderError::Error(format!(
                "unrecognized header: {}{} ({})",
                if is_prereq { "-" } else { "" },
                String::from_utf8_lossy(body),
                body.len()
            )));
        }
        let oid = oid.expect("checked above");
        if is_prereq {
            header.prereqs.push(oid);
        } else {
            // `p + 1`: the ref name follows the one whitespace byte.
            header.refs.push((oid, rest[1..].to_vec()));
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
                return Ok(super::show_usage(LIST_HEADS_USAGE));
            }
            s if s.starts_with("--") && s.len() > 2 => {
                return Ok(bad_option(s, LIST_HEADS_USAGE));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                return Ok(bad_option(s, LIST_HEADS_USAGE));
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
                return Ok(super::show_usage(VERIFY_USAGE));
            }
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            s if s.starts_with("--") && s.len() > 2 => {
                return Ok(bad_option(s, VERIFY_USAGE));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                return Ok(bad_option(s, VERIFY_USAGE));
            }
            s => file = Some(s),
        }
    }

    let Some(file) = file else {
        return Ok(need_file(VERIFY_USAGE));
    };
    // `verify_bundle()` prints its verbose block with `printf_ln()` into stdio's
    // stdout buffer (bundle.c:265-286) and `cmd_bundle_verify()` then writes
    // `<file> is okay` with `fprintf(stderr, …)` (builtin/bundle.c:161), so off
    // a terminal the listing reaches the fd at `exit()`, after that line.
    crate::cstdio::defer();
    // `if (!startup_info->have_repository)` (builtin/bundle.c) is asked before
    // `open_bundle()`, and is an `error()`, not a `die()`: exit 1.
    let Ok(repo) = crate::setup::discover() else {
        eprintln!("error: need a repository to verify a bundle");
        return Ok(ExitCode::from(1));
    };
    let header = match read_header(file) {
        Ok(h) => h,
        Err(e) => return report(file, e),
    };


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
        // ```c
        // if (header->filter.choice)
        //         printf_ln(_("The bundle uses this filter: %s"),
        //                   list_objects_filter_spec(&header->filter));
        // ```
        //
        // (`verify_bundle()`, bundle.c.) The last line of the verbose block, and
        // the only one a v2 bundle never has: `@filter=<spec>` exists in v3 alone.
        // `list_objects_filter_spec()` re-renders the parsed filter, which for
        // every spelling the corpus carries is the text as written.
        if let Some(filter) = &header.filter {
            out.extend_from_slice(
                format!("The bundle uses this filter: {filter}\n").as_bytes(),
            );
        }
        crate::cstdio::write_bytes_io(&out)?;
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
    // `builtin_bundle_create_options`: the progress switches are
    // `OPT_PASSTHRU_ARGV` into `pack_opts`, after the `--progress` that
    // `isatty(STDERR_FILENO)` pushes and before the unconditional
    // `--all-progress-implied` (builtin/bundle.c:94-96). The `pack-objects` child
    // reads them in that order, last one wins, and `--all-progress-implied` lifts
    // an enabled meter to `progress = 2` so `Writing objects` shows despite
    // `--stdout` (builtin/pack-objects.c:5352-5353). `--version` is the one
    // option that changes the bytes.
    // `int version = -1`, which `create_bundle()` reads as "pick the minimum".
    let mut progress = crate::progress::enabled(false);
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
            // `-h` and `--help-all` are read by `parse_options()` only while it is still
            // looking at options; after the `<file>` operand they are `setup_revisions()`
            // leftovers like any other. `--help-all` is its own `strcmp()` inside
            // `parse_options_step()` and prints `USAGE_FULL`, which is this same block —
            // no entry here is `PARSE_OPT_HIDDEN`.
            "-h" | "--help-all" => return Ok(super::show_usage(CREATE_USAGE)),
            "-q" | "--quiet" => progress = false,
            "--progress" | "--all-progress" => progress = true,
            "--all-progress-implied" => {}
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
            _ => return Ok(bad_option(a, CREATE_USAGE)),
        }
        i += 1;
    }

    let Some(file) = file else {
        return Ok(need_file(CREATE_USAGE));
    };

    // ```c
    // static int cmd_bundle_create(...)
    // {
    //         [...]
    //         argc = parse_options(argc, argv, prefix, options, builtin_bundle_create_usage,
    //                              PARSE_OPT_STOP_AT_NON_OPTION | PARSE_OPT_KEEP_ARGV0);
    // ```
    //
    // `--filter` is not one of `bundle create`'s own options — before `<file>` it
    // is `error: unknown option 'filter=…'` at 129 — it is a **rev-list** option,
    // reaching `revs.filter` through `setup_revisions()`. That is why it may only
    // appear after the file operand, and why the last spelling wins with
    // `--no-filter` clearing it, as it does for `git rev-list`.
    let mut filter: Option<String> = None;
    {
        let mut kept: Vec<&str> = Vec::with_capacity(rev_args.len());
        let mut i = 0;
        while i < rev_args.len() {
            let a = rev_args[i];
            match a {
                "--no-filter" => filter = None,
                "--filter" => {
                    if let Some(v) = rev_args.get(i + 1) {
                        filter = Some((*v).to_string());
                        i += 1;
                    }
                }
                _ if a.starts_with("--filter=") => {
                    filter = Some(a["--filter=".len()..].to_string());
                }
                _ => kept.push(a),
            }
            i += 1;
        }
        rev_args = kept;
    }

    // git's `if (!startup_info->have_repository) die(...)`, which exits 128.
    let Ok(repo) = crate::setup::discover() else {
        eprintln!("fatal: Need a repository to create a bundle.");
        return Ok(ExitCode::from(128));
    };
    let Setup { pending, pathspecs, walk_args, unrecognized, diff, max_age, min_age } =
        match resolve_revisions(&repo, &rev_args)? {
            Ok(setup) => setup,
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
    // `diff_setup_done(&revs->diffopt)` (revision.c:3212), still inside
    // `setup_revisions()`: `--follow` without exactly one pathspec, two pickaxes,
    // and the other option combinations the diff machinery refuses.
    if let Err(message) = diff.setup_done(&pathspecs) {
        eprintln!("fatal: {message}");
        return Ok(ExitCode::from(128));
    }
    // `if (argc > 1) error(_("unrecognized argument: %s"), argv[1])`
    // (bundle.c:513-516): whatever `setup_revisions()` left behind. `bundle
    // create`'s own switches are exactly that once the `<file>` operand has
    // ended option parsing, which is why `git bundle create <file> -q` is an
    // error while `git bundle create -q <file>` is not.
    //
    // Stock dies on this path where the allocator notices: one `error:` line, then
    // SIGABRT from freeing the uninitialised `revs_copy` at bundle.c:600 (a shell
    // sees 134; measured on macOS with git 2.54.0, 2.55.0 and 2.56.0). That free is
    // undefined behaviour, so the outcome is the host allocator's: macOS's aborts,
    // glibc's lets it pass and git exits 1 (measured on the ubuntu CI runner, where
    // `git bundle create <file> --foo` prints the same line and returns 1). This
    // follows the host the same way. The `error:` line is written unbuffered above,
    // and no bundle is written either way.
    if let Some(word) = unrecognized {
        eprintln!("error: unrecognized argument: {word}");
        if cfg!(target_vendor = "apple") {
            std::process::abort();
        }
        return Ok(ExitCode::from(1));
    }

    // `if (version == -1) version = min_version;` — 2 for sha1, and only 2 or 3
    // exist (bundle.c:525-531). The v2 header carries no `@object-format`
    // capability, so it can only describe sha1: a sha256 repository defaults to
    // 3, and an explicit `--version=2` is refused rather than written as a
    // bundle whose reader would take 64-hex ids for 40-hex ones.
    let sha1_repo = repo.object_hash() == gix::hash::Kind::Sha1;
    // A filter raises the minimum the same way a non-sha1 algorithm does: only a
    // v3 header has an `@filter` capability to carry it. The refusal that follows
    // is one message for both reasons and names the *algorithm* whichever raised
    // the floor — measured against stock 2.55.0, `git bundle create --version=2
    // <file> --all --filter=blob:none` in a sha1 repository answers
    // `fatal: cannot write bundle version 2 with algorithm sha1`.
    let min_version = if sha1_repo && filter.is_none() { 2 } else { 3 };
    // `-1` is the sentinel `cmd_bundle_create` starts from, so an explicit
    // `--version=-1` takes the same default as no `--version` at all.
    let version = version.filter(|v| *v != -1).unwrap_or(min_version);
    if !(2..=3).contains(&version) {
        eprintln!("fatal: unsupported bundle version {version}");
        return Ok(ExitCode::from(128));
    }
    if version < min_version {
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
        // `@filter=<spec>` follows `@object-format`, in the order
        // `write_bundle_header()` writes them, and is what `bundle verify` reads
        // back as `The bundle uses this filter: …`.
        if let Some(spec) = &filter {
            out.extend_from_slice(format!("@filter={spec}\n").as_bytes());
        }
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
    // A limiting option or a pathspec makes `get_revision()` show fewer commits than
    // the closure of the tips, so the boundary and the shown set come from the walk
    // itself; without one the closure computed above is the whole answer.
    let limited = match &walk_args {
        // Nothing to walk from: the pack is empty and the bundle is refused below, and
        // `rev-list` would only answer with its usage.
        Some(walk_args) if pending.iter().any(|p| !p.uninteresting) => match limited_walk(walk_args)? {
            Ok(limited) => Some(limited),
            Err(code) => return Ok(code),
        },
        _ => None,
    };
    let prereqs = match &limited {
        Some(limited) => limited.boundary.clone(),
        None => boundary_commits(&repo, &tips, &hidden, &excluded_closure),
    };
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
        // ```c
        // if (!(e->item->flags & SHOWN) && e->item->type == OBJ_COMMIT) {
        //         warning(_("ref '%s' is excluded by the rev-list options"), e->name);
        //         goto skip_write_ref;
        // }
        // ```
        //
        // `--max-count` and the other limiting options can keep a tip out of the
        // walk's output; a tag or blob is not a commit and is never affected.
        if let Some(limited) = &limited {
            // A boundary commit is `UNINTERESTING`, which the loop skips before it dwims.
            if limited.boundary.contains(&entry.id) {
                continue;
            }
            let entry_kind = repo.find_object(entry.id).ok().map(|o| o.kind);
            let is_commit = entry_kind == Some(Kind::Commit);
            if is_commit && !limited.shown.contains(&entry.id) {
                eprintln!("warning: ref '{}' is excluded by the rev-list options", entry.name);
                continue;
            }
            // A tag is never warned about, but `--since` and `--until` still drop one whose
            // own date is outside the window (its tagger line, 0 without one). Measured on
            // git 2.56.0: `bundle create <file> v1 --since=<after the tag was made>` and
            // `--until=<before it>` both refuse an empty bundle, while a cut that falls
            // between the tagged commit and the tag keeps the ref.
            if entry_kind == Some(Kind::Tag)
                && !tag_in_age_window(&repo, entry.id, max_age, min_age)
            {
                continue;
            }
        }
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
    // prerequisites. The pack is thin: `pack-objects --thin` turns the walk's
    // edge commits into preferred bases, so a delta may name a base the pack
    // does not carry because the receiver already reached it. The prerequisites
    // *are* those edge commits — `compute_and_write_prerequisites()` collects
    // them from `rev-list --boundary` and hands them to `pack-objects` as
    // `^<oid>` (bundle.c) — so they go to the pack writer as the boundary.
    //
    // The wants are the pending entries as typed, so a tag tip is packed as a
    // tag; the haves are the peeled ones, because `pack-objects` peels a `^<tag>`
    // itself and a receiver that already has the commit is what the exclusion
    // means.
    let mut haves = prereqs.clone();
    haves.extend_from_slice(&hidden);
    let mut objects = crate::porcelain::push_proto::objects_to_send(&repo, &want_objects, &haves);
    // `pack-objects --filter=<spec>` is what `write_pack_data()` spawns when the
    // revision walk carried one, so the same exemption applies here as there: the
    // tips the user named are never filtered out, only the objects the walk
    // reached through them.
    super::pack_objects::apply_filter(&repo, filter.as_deref(), &want_objects, &mut objects);
    // The child's `get_object_list()` shows each object it adds as
    // `Enumerating objects` (`add_object_entry()`, builtin/pack-objects.c:1875).
    {
        let mut enumerating = crate::progress::Meter::unknown("Enumerating objects", progress);
        for _ in 0..objects.len() {
            enumerating.tick();
        }
        enumerating.stop("done");
    }
    // `write_pack_data()` spawns `pack-objects --stdout --thin --delta-base-offset`
    // (bundle.c:333-336) — both flags are unconditional there, so a bundle's
    // deltas are always `OBJ_OFS_DELTA` where the base is in the pack and
    // `OBJ_REF_DELTA` where `--thin` put it outside. Passing `false` for the
    // first wrote `OBJ_REF_DELTA` throughout, which is 18 bytes larger per delta
    // and is not what any git bundle contains. `--stdout` is why the writing
    // phase closes with a byte count and rate.
    out.extend_from_slice(
        &crate::porcelain::pack_objects::packed_for_thin(
            &repo,
            &objects,
            crate::porcelain::pack_objects::WriteOptions {
                allow_ofs_delta: true,
                progress,
                // The unconditional `--all-progress-implied` (builtin/bundle.c:94-96).
                all_progress: true,
                to_stdout: true,
                ..Default::default()
            },
            &prereqs,
        )?
        .bytes,
    );

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
    /// `e->name`: the entry as typed (a full ref name for `--all`, `--branches` and
    /// friends), which is what `write_bundle_refs()` quotes in its
    /// `ref '%s' is excluded by the rev-list options` warning.
    name: String,
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

/// How `handle_revision_opt()` / `handle_revision_pseudo_opt()` take a value.
#[derive(Clone, Copy)]
enum Take {
    /// `--name` and nothing else: `--name=x` is not this option.
    Flag,
    /// `--name` or `--name=<v>` (`skip_prefix(arg, "--name=")` / `starts_with`).
    Opt,
    /// `--name=<v>` only; the bare word is not this option.
    Attached,
    /// `parse_long_opt()`: `--name=<v>`, or `--name` plus the next argv word,
    /// which `die()`s `Option '--name' requires a value` when there is none.
    Detached,
}

/// The options `setup_revisions()` consumes that are neither the count-and-age
/// arm ([`crate::revopt`]) nor `diff_opt_parse()`'s table, with whether the
/// option changes which commits `get_revision()` hands back. Derived by running
/// each spelling through stock `git bundle create <file> main <opt>` and keeping
/// those that do not answer `unrecognized argument`.
///
/// `bundle create` runs no diff and prints no log, so every option that only
/// shapes output (`--oneline`, `--parents`, `--left-right`, `--graph`, …) is
/// accepted and ignored, exactly as stock ignores it. The `walk` ones are handed
/// to the limited walk in [`limited_walk`].
const REVISION_OPTS: &[(&str, Take, bool)] = &[
    // Commit selection.
    ("--merges", Take::Flag, true),
    ("--no-merges", Take::Flag, true),
    ("--min-parents", Take::Attached, true),
    ("--max-parents", Take::Attached, true),
    ("--no-min-parents", Take::Flag, true),
    ("--no-max-parents", Take::Flag, true),
    ("--first-parent", Take::Flag, true),
    ("--exclude-first-parent-only", Take::Flag, true),
    ("--ancestry-path", Take::Opt, true),
    ("--full-history", Take::Flag, true),
    ("--sparse", Take::Flag, true),
    ("--dense", Take::Flag, true),
    ("--simplify-merges", Take::Flag, true),
    ("--simplify-by-decoration", Take::Flag, true),
    ("--remove-empty", Take::Flag, true),
    ("--show-pulls", Take::Flag, true),
    ("--maximal-only", Take::Flag, true),
    ("--cherry-pick", Take::Flag, true),
    ("--cherry", Take::Flag, true),
    // See [`limited_walk`]: these hide commits by setting `SHOWN`, not by dropping them.
    ("--left-only", Take::Flag, true),
    ("--right-only", Take::Flag, true),
    ("--unpacked", Take::Opt, true),
    ("--no-kept-objects", Take::Opt, true),
    ("--merge", Take::Flag, true),
    ("-g", Take::Flag, true),
    ("--walk-reflogs", Take::Flag, true),
    // Order and direction.
    ("--reverse", Take::Flag, true),
    ("--no-walk", Take::Opt, true),
    ("--do-walk", Take::Flag, true),
    ("--topo-order", Take::Flag, true),
    ("--date-order", Take::Flag, true),
    ("--author-date-order", Take::Flag, true),
    // The commit-message predicates and the dialect flags that compile them.
    ("--grep", Take::Detached, true),
    ("--author", Take::Detached, true),
    ("--committer", Take::Detached, true),
    ("--grep-reflog", Take::Detached, true),
    ("-i", Take::Flag, true),
    ("--regexp-ignore-case", Take::Flag, true),
    ("-E", Take::Flag, true),
    ("--extended-regexp", Take::Flag, true),
    ("-F", Take::Flag, true),
    ("--fixed-strings", Take::Flag, true),
    ("-P", Take::Flag, true),
    ("--perl-regexp", Take::Flag, true),
    ("--basic-regexp", Take::Flag, true),
    ("--all-match", Take::Flag, true),
    ("--invert-grep", Take::Flag, true),
    // Output shape only.
    ("--boundary", Take::Flag, false),
    ("--children", Take::Flag, false),
    ("--parents", Take::Flag, false),
    ("--left-right", Take::Flag, false),
    ("--cherry-mark", Take::Flag, false),
    ("--count", Take::Flag, false),
    ("--objects", Take::Flag, false),
    ("--objects-edge", Take::Flag, false),
    ("--objects-edge-aggressive", Take::Flag, false),
    ("--verify-objects", Take::Flag, false),
    ("--in-commit-order", Take::Flag, false),
    ("--graph", Take::Flag, false),
    ("--no-graph", Take::Flag, false),
    ("--oneline", Take::Flag, false),
    ("--abbrev-commit", Take::Flag, false),
    ("--no-abbrev-commit", Take::Flag, false),
    ("--relative-date", Take::Flag, false),
    ("--log-size", Take::Flag, false),
    ("--always", Take::Flag, false),
    ("--root", Take::Flag, false),
    ("--cc", Take::Flag, false),
    ("--dd", Take::Flag, false),
    ("--remerge-diff", Take::Flag, false),
    ("--full-diff", Take::Flag, false),
    ("--no-commit-id", Take::Flag, false),
    ("--no-diff-merges", Take::Flag, false),
    ("-c", Take::Flag, false),
    ("-m", Take::Flag, false),
    ("-r", Take::Flag, false),
    ("-t", Take::Flag, false),
    ("-v", Take::Flag, false),
    ("--show-signature", Take::Flag, false),
    ("--no-show-signature", Take::Flag, false),
    ("--no-notes", Take::Flag, false),
    ("--standard-notes", Take::Flag, false),
    ("--no-standard-notes", Take::Flag, false),
    ("--show-notes-by-default", Take::Flag, false),
    ("--encode-email-headers", Take::Flag, false),
    ("--no-encode-email-headers", Take::Flag, false),
    ("--no-expand-tabs", Take::Flag, false),
    ("--git-completion-helper", Take::Flag, false),
    ("--git-completion-helper-all", Take::Flag, false),
    ("--pretty", Take::Opt, false),
    ("--format", Take::Attached, false),
    ("--notes", Take::Opt, false),
    ("--show-notes", Take::Opt, false),
    ("--show-linear-break", Take::Opt, false),
    ("--expand-tabs", Take::Opt, false),
    ("--date", Take::Detached, false),
    ("--encoding", Take::Detached, false),
    ("--diff-merges", Take::Detached, false),
];

/// What `setup_revisions()` made of one `-`-prefixed word.
enum Claimed {
    /// Left in `argv` for the caller; `bundle create` reports it unrecognized.
    No,
    /// Consumed, this many argv words; `walk` says whether it limits the walk.
    Yes { words: usize, walk: bool },
}

/// The `handle_revision_opt()` chain for one word: the count-and-age arm, then the
/// table above, then `diff_opt_parse()` (revision.c:2758-2762). `Err` is the exit
/// status of a `die()` / parse-options refusal whose message is already out.
fn claim_revision_opt(
    repo: &gix::Repository,
    args: &[String],
    i: usize,
    counts: &mut crate::revopt::Counts,
    diff: &mut super::diff_opt_parse::DiffOpts,
) -> std::result::Result<Claimed, ExitCode> {
    let a = args[i].as_str();
    match counts.parse(args, i) {
        Some(Ok(hit)) => return Ok(Claimed::Yes { words: hit.consumed, walk: true }),
        Some(Err(msg)) => {
            // `-n` with nothing after it is an `error()`, the rest are `die()`s.
            let level = if msg == "-n requires an argument" { "error" } else { "fatal" };
            eprintln!("{level}: {msg}");
            return Err(ExitCode::from(128));
        }
        None => {}
    }
    // `--no-walk[=sorted|unsorted]`: any other value is an `error()` and the word is left
    // in `argv`.
    if a.strip_prefix("--no-walk=").is_some_and(|v| !matches!(v, "sorted" | "unsorted")) {
        eprintln!("error: invalid argument to --no-walk");
        return Ok(Claimed::No);
    }
    // `--default <rev>` (revision.c:2429-2433): the revision to use when none was given.
    if a == "--default" {
        if args.get(i + 1).is_none() {
            eprintln!("error: bad --default argument");
            return Err(ExitCode::from(128));
        }
        return Ok(Claimed::Yes { words: 2, walk: false });
    }
    for &(name, take, walk) in REVISION_OPTS {
        let attached = a.strip_prefix(name).and_then(|rest| rest.strip_prefix('='));
        let words = match take {
            Take::Flag if a == name => 1,
            Take::Opt if a == name || attached.is_some() => 1,
            Take::Attached if attached.is_some() => 1,
            Take::Detached if attached.is_some() => 1,
            Take::Detached if a == name => {
                if args.get(i + 1).is_none() {
                    eprintln!("fatal: Option '{name}' requires a value");
                    return Err(ExitCode::from(128));
                }
                2
            }
            _ => continue,
        };
        // `--unpacked=<packfile>` is a `die()` since the pack-list form was removed.
        if name == "--unpacked" && attached.is_some() {
            eprintln!("fatal: --unpacked=<packfile> no longer supported");
            return Err(ExitCode::from(128));
        }
        return Ok(Claimed::Yes { words, walk });
    }
    match super::diff_opt_parse::diff_opt_parse(repo, &args[i..], diff) {
        super::diff_opt_parse::Step::Unknown => Ok(Claimed::No),
        super::diff_opt_parse::Step::Took(words) => Ok(Claimed::Yes { words, walk: false }),
        super::diff_opt_parse::Step::Exit(code) => Err(code),
    }
}

/// Everything `setup_revisions()` hands `create_bundle()`.
struct Setup {
    pending: Vec<Pending>,
    /// `revs->prune_data`.
    pathspecs: Vec<Vec<u8>>,
    /// The revision words with the output-only options removed, for the walk
    /// that decides which commits are shown. `None` when nothing limits it.
    walk_args: Option<Vec<String>>,
    /// `argv[1]` once `setup_revisions()` returns: the first word nothing claimed.
    unrecognized: Option<String>,
    /// `revs->diffopt`, for the `diff_setup_done()` checks.
    diff: super::diff_opt_parse::DiffOpts,
    /// `revs->max_age` and `revs->min_age`: `--since` / `--until` and their spellings.
    max_age: Option<i64>,
    min_age: Option<i64>,
}

/// Whether a tag's own date, its tagger line or 0, lies inside the `--since` /
/// `--until` window.
fn tag_in_age_window(
    repo: &gix::Repository,
    id: ObjectId,
    max_age: Option<i64>,
    min_age: Option<i64>,
) -> bool {
    let date = repo
        .find_object(id)
        .ok()
        .and_then(|o| o.try_into_tag().ok())
        .and_then(|tag| tag.tagger().ok().flatten().map(|t| t.seconds()))
        .unwrap_or(0);
    max_age.is_none_or(|min| date >= min) && min_age.is_none_or(|max| date <= max)
}

/// What the limited revision walk showed.
struct Limited {
    /// The `BOUNDARY` commits, in the order the walk lists them: the bundle's
    /// prerequisites.
    boundary: Vec<ObjectId>,
    /// Every commit the walk showed (`SHOWN`). A boundary commit is `UNINTERESTING`
    /// instead, which is why a ref on one is dropped without a warning.
    shown: std::collections::HashSet<ObjectId>,
}

/// `revs.boundary = 1` and `traverse_commit_list()` (bundle.c:564-575) for a walk
/// the closure of the tips cannot describe — `--max-count`, `--since`, `--skip`,
/// the parent-count and message filters, `--first-parent`, a pathspec, `--reverse`
/// and the rest of `walk` options in [`REVISION_OPTS`]. `rev-list --boundary` is
/// that same traversal, so it is asked instead of re-deriving every filter here:
/// a plain line is a shown commit, a `-`-prefixed one a boundary commit.
///
/// `Err` carries the exit status of a walk that died; its diagnostic is already
/// on stderr.
fn limited_walk(walk_args: &[String]) -> Result<std::result::Result<Limited, ExitCode>> {
    let mut limited = match rev_list_boundary(walk_args)? {
        Ok(limited) => limited,
        Err(code) => return Ok(Err(code)),
    };
    // `limit_left_right()` (revision.c) hides the commits of the other side of a
    // symmetric difference by setting `SHOWN` on them instead of removing them from
    // the list, so they never reach `show_commit()` — no prerequisite comes of them —
    // yet `write_bundle_refs()` finds the flag set and keeps their refs without the
    // "excluded" warning. `rev-list` prints only what was not hidden, so the commits
    // that carry the flag are the ones a walk without the option shows. `--cherry` is
    // `--left-only` plus `--cherry-pick` and hides the same way.
    const HIDING: [&str; 3] = ["--left-only", "--right-only", "--cherry"];
    if walk_args.iter().any(|a| HIDING.contains(&a.as_str())) {
        let unhidden: Vec<String> = walk_args
            .iter()
            .filter(|a| !HIDING.contains(&a.as_str()))
            .cloned()
            .collect();
        match rev_list_boundary(&unhidden)? {
            Ok(all) => limited.shown.extend(all.shown),
            Err(code) => return Ok(Err(code)),
        }
    }
    Ok(Ok(limited))
}

/// One `git rev-list --boundary <args>` run, read the way `bundle.c` reads the
/// traversal: a plain line is a shown commit, a `-`-prefixed one a boundary commit.
fn rev_list_boundary(walk_args: &[String]) -> Result<std::result::Result<Limited, ExitCode>> {
    let out = std::process::Command::new(std::env::current_exe()?)
        .arg("rev-list")
        .arg("--boundary")
        .args(walk_args)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()?;
    if !out.status.success() {
        return Ok(Err(ExitCode::from(out.status.code().unwrap_or(128) as u8)));
    }
    let mut limited = Limited { boundary: Vec::new(), shown: Default::default() };
    for line in out.stdout.split(|b| *b == b'\n') {
        let (boundary, rest) = match line.strip_prefix(b"-") {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        // `--cherry` implies `--cherry-mark`, which puts `+` or `=` in front of the id.
        let hex = rest.strip_prefix(b"+").or_else(|| rest.strip_prefix(b"=")).unwrap_or(rest);
        let Ok(id) = ObjectId::from_hex(hex) else { continue };
        if boundary {
            limited.boundary.push(id);
        } else {
            limited.shown.insert(id);
        }
    }
    Ok(Ok(limited))
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
) -> Result<std::result::Result<Setup, ExitCode>> {
    let mut pending = Vec::new();
    let mut counts = crate::revopt::Counts::default();
    let mut diff = super::diff_opt_parse::DiffOpts::default();
    let mut unrecognized: Option<String> = None;
    // Indices into `items` of the output-only options, which the limited walk must not see.
    let mut inert: Vec<usize> = Vec::new();
    let mut limiting = false;
    let mut seen_end_of_options = false;
    let mut default_rev: Option<String> = None;
    let mut used_default: Option<String> = None;
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
                eprintln!("fatal: Option '--exclude' requires a value");
                return Ok(Err(ExitCode::from(128)));
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
                None => {
                    eprintln!("fatal: Option '--glob' requires a value");
                    return Ok(Err(ExitCode::from(128)));
                }
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
                let r = match r {
                    Ok(r) => r,
                    Err(e) => {
                        // `handle_one_ref()` hands every entry to
                        // `get_reference()`, which dies on one whose object the
                        // repository does not have. A ref file that will not
                        // parse arrives with a null id, so it dies there too —
                        // and it dies *before* `create_bundle()` writes the
                        // signature, which is why stock leaves stdout empty
                        // rather than a header with no pack behind it.
                        use gix::refs::file::iter::loose_then_packed::Error as IterError;
                        match e.downcast_ref::<IterError>() {
                            Some(IterError::ReferenceCreation { relative_path, .. }) => {
                                crate::git_fatal!(
                                    "bad object {}",
                                    relative_path.to_string_lossy().replace('\\', "/")
                                );
                            }
                            _ => continue,
                        }
                    }
                };
                let full = r.name().as_bstr().to_string();
                if sel.selects(&full).is_none() {
                    continue;
                }
                // `for_each_ref()` resolves a symbolic ref before handing the
                // callback an object id, so `refs/remotes/origin/HEAD` is a
                // pending object like any other — and a bundle names it in its
                // header. `try_id()` answers only for a direct ref, so following
                // is this port's half of that resolution. The follow stops at the
                // object: a tag tip stays the tag's own id, which is what the
                // header has to carry.
                let mut r = r;
                let resolved = r.try_id().map(|id| id.detach()).or_else(|| {
                    r.follow_to_object().ok().map(|id| id.detach())
                });
                if let Some(id) = resolved {
                    // `write_bundle_refs()` re-dwims each pending name through
                    // `repo_dwim_ref()`, so a `--branches` entry named `topic`
                    // comes back out as `refs/heads/topic`.
                    // `--branches`, `--tags` and `--remotes` iterate with the namespace
                    // trimmed (`trim_prefix`), so `e->name` is the short name; `--all` and
                    // `--glob` keep the whole ref name.
                    let name = match kind {
                        super::log::RefSelector::Branches => full.strip_prefix("refs/heads/"),
                        super::log::RefSelector::Tags => full.strip_prefix("refs/tags/"),
                        super::log::RefSelector::Remotes => full.strip_prefix("refs/remotes/"),
                        _ => None,
                    }
                    .unwrap_or(&full)
                    .to_string();
                    pending.push(Pending {
                        id,
                        name,
                        display_ref: Some(full),
                        uninteresting: negate,
                    });
                }
            }
            if sel.head && !sel.excluded("HEAD") {
                if let Ok(head) = repo.head_id() {
                    pending.push(Pending {
                        id: head.detach(),
                        name: "HEAD".into(),
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
        // `--end-of-options` and then `handle_revision_opt()` (revision.c:3062-3090),
        // which run for every `-`-prefixed word until `--end-of-options` is seen.
        // What neither claims stays in `argv`; `create()` reports the first such
        // word once `setup_revisions()` has finished — a bad revision later on the
        // line therefore still dies first, as in stock.
        if !from_stdin && !seen_end_of_options && a.starts_with('-') {
            if a == "--end-of-options" {
                seen_end_of_options = true;
                i += 1;
                continue;
            }
            let tail: Vec<String> = items[i..].iter().map(|it| it.text.clone()).collect();
            if a == "--default" {
                default_rev = tail.get(1).cloned();
            }
            match claim_revision_opt(repo, &tail, 0, &mut counts, &mut diff) {
                Err(code) => return Ok(Err(code)),
                Ok(Claimed::Yes { words, walk }) => {
                    if walk {
                        limiting = true;
                    } else {
                        inert.extend(i..i + words);
                    }
                    i += words;
                }
                Ok(Claimed::No) => {
                    unrecognized.get_or_insert_with(|| a.to_string());
                    i += 1;
                }
            }
            continue;
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
            items.truncate(i);
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
                        name: base.to_string(),
                        display_ref: None,
                        uninteresting: !negate,
                    });
                }
                // `b_flags = flags` and `a_flags = flags | SYMMETRIC_LEFT`: both
                // ends of a symmetric difference are interesting, and only the
                // bases carry `flags_exclude`.
                pending.push(Pending {
                    id: a_raw,
                    name: r.a.to_string(),
                    display_ref: display_ref(repo, r.a),
                    uninteresting: negate,
                });
            } else {
                // `a_flags = flags_exclude`: the left end of `<a>..<b>` is the
                // excluded one, and a preceding `--not` flips both.
                pending.push(Pending {
                    id: a_raw,
                    name: r.a.to_string(),
                    display_ref: display_ref(repo, r.a),
                    uninteresting: !negate,
                });
            }
            pending.push(Pending {
                id: b_raw,
                name: r.b.to_string(),
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
                        name: name.to_string(),
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
                    return Ok(Err(ExitCode::from(128)));
                }
                let message = super::log::bad_revision_message_in(repo, a);
                // ```c
                // if (handle_revision_arg(arg, revs, flags, revarg_opt)) {
                //         if (seen_dashdash || *arg == '^')
                //                 die(_("bad revision '%s'"), arg);
                //         for (j = i; j < argc; j++)
                //                 verify_filename(revs->prefix, argv[j], j == i);
                //         append_prune_data(&prune_data, argv + i);
                //         break;
                // }
                // ```
                //
                // (revision.c:3080-3097.) An operand that is not a revision but is, or
                // may be, a path ends the revision list: it and everything after it
                // become pathspecs — and a later word that looks like an option is
                // `option '%s' must come before non-option arguments`. A failure that
                // `handle_revision_arg()` raised itself keeps its own text.
                let may_be_path = !seen_dashdash
                    && !text.starts_with('^')
                    && message.starts_with("fatal: ambiguous argument")
                    && (text.starts_with('-') || super::log::spec_is_path(repo, &text));
                if may_be_path {
                    for (n, item) in items[i..].iter().enumerate() {
                        if let Some(msg) = crate::setup::verify_filename(&item.text, n == 0) {
                            eprintln!("fatal: {msg}");
                            return Ok(Err(ExitCode::from(128)));
                        }
                    }
                    pathspecs.extend(items[i..].iter().map(|it| it.text.as_bytes().to_vec()));
                    items.truncate(i);
                    break;
                }
                eprint!("{message}");
                return Ok(Err(ExitCode::from(128)));
            }
        }
        i += 1;
    }
    // `--follow` clears `revs->prune` (revision.c: "Can't prune commits with rename
    // following"), so its single pathspec does not limit the walk at all.
    // `if (revs->def && !revs->pending.nr && !got_rev_arg)`: `--default` stands in for a
    // command line that named no revision.
    if let (true, Some(def)) = (pending.is_empty(), default_rev.as_deref()) {
        match one_pending(repo, def, false) {
            Ok(p) => {
                pending.push(p);
                used_default = Some(def.to_string());
            }
            Err(_) => {
                eprint!("{}", super::log::bad_revision_message_in(repo, def));
                return Ok(Err(ExitCode::from(128)));
            }
        }
    }
    let follow = inert
        .iter()
        .map(|n| items[*n].text.as_str())
        .rfind(|t| matches!(*t, "--follow" | "--no-follow"))
        == Some("--follow");
    let limiting_paths = !pathspecs.is_empty() && !follow;
    let walk_args = (limiting || limiting_paths).then(|| {
        let mut walk: Vec<String> = items
            .iter()
            .enumerate()
            .filter(|(n, it)| !inert.contains(n) && (it.from_stdin || it.text != "--stdin"))
            .map(|(_, it)| it.text.clone())
            .collect();
        walk.extend(used_default);
        if limiting_paths {
            walk.push("--".to_string());
            walk.extend(pathspecs.iter().map(|p| String::from_utf8_lossy(p).into_owned()));
        }
        walk
    });
    Ok(Ok(Setup { pending, pathspecs, walk_args, unrecognized, diff, max_age: counts.max_age, min_age: counts.min_age }))
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
    Ok(Pending { id, name: spec.to_string(), display_ref: display_ref(repo, spec), uninteresting })
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
                return Ok(super::show_usage(UNBUNDLE_USAGE));
            }
            "--progress" => progress = true,
            "--no-progress" => progress = false,
            s if s.starts_with("--") && s.len() > 2 => {
                return Ok(bad_option(s, UNBUNDLE_USAGE));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                return Ok(bad_option(s, UNBUNDLE_USAGE));
            }
            s => file = Some(s),
        }
    }

    let Some(file) = file else {
        return Ok(need_file(UNBUNDLE_USAGE));
    };

    // git's `if (!startup_info->have_repository) die(...)`, which exits 128.
    let Ok(repo) = crate::setup::discover() else {
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

    let mut extra: Vec<&str> = Vec::new();
    // ```c
    // /* If there is a filter, then we need to create the promisor pack. */
    // if (header->filter.choice)
    //         strvec_push(&ip.args, "--promisor=from-bundle");
    // ```
    //
    // (bundle.c:632-634, in `unbundle()`.) A filtered bundle's pack is missing
    // the objects the filter dropped, so it has to be installed as a promisor
    // pack — otherwise every later reader treats those absences as corruption
    // rather than as objects to be fetched from the promisor remote.
    if header.filter.is_some() {
        extra.push("--promisor=from-bundle");
    }
    if progress {
        extra.extend(["-v", "--progress-title", "Unbundling objects"]);
    }
    if !index_pack(source, &repo, &extra)? {
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
    // `index-pack` resolves the repository with `crate::setup::discover()`
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
