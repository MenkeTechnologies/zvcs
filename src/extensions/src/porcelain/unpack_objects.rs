//! `git unpack-objects` — read a pack stream from stdin and explode it into
//! loose objects in the current repository.
//!
//! Stock git streams the pack and writes each object the moment it is decoded
//! (`builtin/unpack-objects.c`). This port takes the equivalent route through
//! the vendored `gix-pack`: the stdin stream is indexed into a throwaway
//! pack+idx pair inside a scratch directory under the git dir, every object is
//! then fully resolved through that index (so `OFS_DELTA` and `REF_DELTA`
//! chains, including thin-pack bases already present in the object database,
//! are reconstructed) and written loose. The scratch directory is removed
//! before returning, so the only lasting change is the set of loose objects —
//! matching stock git's end state.
//!
//! The argument parser mirrors git's loop in `cmd_unpack_objects()` exactly:
//! the flags are tested in git's order (`-n`, `-q`, `-r`, `--strict`,
//! `--strict=`, `--pack_header=`, `--max-input-size=`) and both an unknown
//! dash-argument and any positional fall through to `usage()`. That ordering is
//! load-bearing: `git unpack-objects --strict does-not-exist` is a usage error
//! because of the *positional*, not the flag, so a parser that rejected
//! `--strict` early would answer 1 where git answers 129.
//!
//! Covered:
//!   * the default form: `git unpack-objects < pack`, exit 0, empty stdout.
//!   * `-n` — dry run: the pack is still fully decoded and verified, nothing is
//!     written.
//!   * `-q` — accepted; this port never emits progress, so it is already quiet
//!     (stock git only paints progress when stderr is a terminal).
//!   * `-r` — recover: the pack is iterated in `gix-pack`'s `Mode::Restore`,
//!     which salvages every entry up to the damage instead of failing the whole
//!     stream, and the exit status becomes 1 when fewer objects came back than
//!     the pack header declared. git reports the same loss the same way — its
//!     `cmd_unpack_objects()` ends in `return has_errors`, so a recovered-with-
//!     losses run is exit 1, not a fatal 128.
//!   * `--strict` and `--strict=<spec>` — non-blob objects are held back until
//!     every object they reference resolves, mirroring git's `write_rest()`;
//!     see the note below for the part of git's fsck that has no substrate.
//!   * `--pack_header=<version>,<objects>` — git's internal hand-off from
//!     `receive-pack`, which supplies a header its caller already consumed. The
//!     12 header bytes are reconstructed exactly as git's
//!     `parse_pack_header_option()` does and chained back in front of stdin, so
//!     the decoder sees the stream it expects. A malformed value dies with
//!     git's `bad --pack_header:` message and 128.
//!   * `--max-input-size=<n>` — `<n>` is parsed exactly as git's `strtoumax`
//!     does (leading base-10 digits only, so `1k` means 1 and `abc` means 0),
//!     and `0` means "no limit". Over the limit dies with git's message and 128.
//!   * objects already present in the repository are not written again, as git
//!     documents and does.
//!   * a lone `-h` prints the usage line on **stdout** with 129, because git.c
//!     intercepts it before the builtin runs. `-h` next to any other argument
//!     is not intercepted, so it reaches the flag loop as an unknown argument
//!     and prints on **stderr**, as does any other unknown flag or positional.
//!   * not being inside a repository: git's `fatal:` line and 128.
//!
//! Not reproduced, and documented rather than papered over:
//!   * `--strict` applies git's *structural* checks only. git runs every
//!     unpacked object through `fsck_object()` with the full message-id
//!     severity table; the vendored `gix-fsck` is 106 lines of connectivity
//!     traversal and carries no such table, so what this port enforces is the
//!     contract the manual page states — "don't write objects with broken
//!     content or links" — via `gix-object`'s decoders plus a link-existence
//!     check against the pack's own object set and the odb. Content defects
//!     that parse cleanly but that git's fsck would flag (tree entry ordering,
//!     `.git`-lookalike path names, zero-padded file modes, author/committer
//!     timestamp shapes) are not detected. The `--strict=<id>=<severity>`
//!     spec is still validated in full, because that validation happens at
//!     parse time and needs only the id table, which is reproduced below.
//!   * version 3 packs. git unpacks them; `gix-pack`'s entry iterator asserts
//!     that the version is 2 ("let's stop here if we see undocumented pack
//!     formats"), so a v3 stream would panic rather than decode. A v3
//!     `--pack_header` is therefore refused with a fatal and 128 instead of
//!     being handed through. A v3 pack arriving on stdin still reaches that
//!     assert, which is a `gix-pack` limit this module cannot route around.
//!   * a damaged pack leaves the object database untouched here, where git may
//!     already have exploded the objects that preceded the corruption: this
//!     port validates the whole pack before writing anything. The exit code
//!     agrees; the set of salvaged loose objects can differ.
//!   * the `fatal:` text for a malformed pack is `gix-pack`'s diagnostic rather
//!     than git's (`early EOF` and friends). The exit code is 128 either way.

use anyhow::Result;
use std::collections::HashSet;
use std::io::{self, BufRead, Read};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::objs::Write as _;

/// The usage line stock `git unpack-objects` prints, verbatim.
const USAGE: &str = "usage: git unpack-objects [-n] [-q] [-r] [--strict]";

/// Every fsck message id `--strict=<id>=<severity>` accepts, in the form git
/// compares against: `fsck_set_msg_types()` lowercases the whole spec, and the
/// ids it matches are the `FOREACH_FSCK_MSG_ID` names with their underscores
/// removed. So `MISSING_EMAIL` is spelled `missingemail` here, and the
/// underscore form `missing_email` is rejected by git as an unknown id.
const FSCK_MSG_IDS: &[&str] = &[
    "nulinheader",
    "unterminatedheader",
    "badheadercontinuation",
    "baddate",
    "baddateoverflow",
    "bademail",
    "badgpgsig",
    "badheadtarget",
    "badname",
    "badobjectsha1",
    "badpackedrefentry",
    "badpackedrefheader",
    "badparentsha1",
    "badreferentname",
    "badrefcontent",
    "badreffiletype",
    "badrefname",
    "badrefoid",
    "badtimezone",
    "badtree",
    "badtreesha1",
    "badtype",
    "duplicateentries",
    "gitattributesblob",
    "gitattributeslarge",
    "gitattributeslinelength",
    "gitattributesmissing",
    "gitmodulesblob",
    "gitmoduleslarge",
    "gitmodulesmissing",
    "gitmodulesname",
    "gitmodulespath",
    "gitmodulessymlink",
    "gitmodulesupdate",
    "gitmodulesurl",
    "missingauthor",
    "missingcommitter",
    "missingemail",
    "missingnamebeforeemail",
    "missingobject",
    "missingspacebeforedate",
    "missingspacebeforeemail",
    "missingtag",
    "missingtagentry",
    "missingtree",
    "missingtype",
    "missingtypeentry",
    "multipleauthors",
    "packedrefentrynotterminated",
    "packedrefunsorted",
    "treenotsorted",
    "unknowntype",
    "zeropaddeddate",
    "badreftabletablename",
    "emptyname",
    "fullpathname",
    "hasdot",
    "hasdotdot",
    "hasdotgit",
    "largepathname",
    "nullsha1",
    "nulincommit",
    "zeropaddedfilemode",
    "badfilemode",
    "badtagname",
    "emptypackedrefsfile",
    "gitattributessymlink",
    "gitignoresymlink",
    "gitmodulesparse",
    "mailmapsymlink",
    "missingtaggerentry",
    "refmissingnewline",
    "symlinkref",
    "symreftargetisnotaref",
    "trailingrefcontent",
    "extraheaderentry",
];

/// The severities `--strict=<id>=<severity>` accepts. git's internal table also
/// carries `fatal` and `info`, but neither is settable from the command line —
/// both are answered with `Unknown fsck message type`.
const FSCK_SEVERITIES: &[&str] = &["error", "warn", "ignore"];

/// `git unpack-objects` — explode a pack read from stdin into loose objects.
///
/// See the module docs for the supported flag set and the documented gaps.
pub fn unpack_objects(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands over the arguments after the subcommand; tolerate a
    // leading `unpack-objects` in case the caller passes argv unsliced. The
    // token is never a legal argument here (git answers any positional with
    // the usage line), so dropping it costs no fidelity.
    let args = match args.split_first() {
        Some((first, rest)) if first == "unpack-objects" => rest,
        _ => args,
    };

    // `git.c` intercepts a lone `-h` before the builtin ever runs and prints the
    // usage line on stdout. It is not part of the builtin's own flag loop, so
    // `-h` alongside anything else is just an unknown argument and lands on
    // stderr through the catch-all below.
    if args.len() == 1 && args[0] == "-h" {
        println!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let mut dry_run = false;
    let mut recover = false;
    let mut strict = false;
    let mut max_input_size: u64 = 0; // git: 0 means "unlimited"
    let mut pack_header: Option<[u8; 12]> = None;

    // git's own order, arm for arm. Anything that falls off the end is a usage
    // error, whether it started with a dash or not.
    for a in args {
        let a = a.as_str();
        match a {
            "-n" => dry_run = true,
            // Progress is never painted by this port, so `-q` is already true.
            "-q" => {}
            "-r" => recover = true,
            "--strict" => strict = true,
            _ if a.starts_with("--strict=") => {
                strict = true;
                // git validates the spec while parsing, before it reads a byte
                // of the pack, and dies at 128 on a bad one.
                if let Err(msg) = check_fsck_msg_types(&a["--strict=".len()..]) {
                    eprintln!("fatal: {msg}");
                    return Ok(ExitCode::from(128));
                }
            }
            _ if a.starts_with("--pack_header=") => {
                let value = &a["--pack_header=".len()..];
                let Some(hdr) = parse_pack_header_option(value) else {
                    eprintln!("fatal: bad --pack_header: {value}");
                    return Ok(ExitCode::from(128));
                };
                // A version the decoder cannot handle has to be refused here.
                // `gix-pack`'s entry iterator asserts on anything but v2 —
                // `data::header::decode` admits v3 and the assert immediately
                // behind it panics — so handing the synthesized header straight
                // through would turn a bad value into a crash.
                let version = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
                match version {
                    2 => pack_header = Some(hdr),
                    // git accepts v3 and unpacks it; this port cannot, and says
                    // so rather than pretending the version was unrecognized.
                    3 => {
                        eprintln!("fatal: pack version 3 is not supported");
                        return Ok(ExitCode::from(128));
                    }
                    // git's own wording, for the versions it also rejects.
                    v => {
                        eprintln!("fatal: unknown pack file version {v}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            _ if a.starts_with("--max-input-size=") => {
                max_input_size = parse_magnitude(&a["--max-input-size=".len()..]);
            }
            // Any other flag, and any positional, is a usage error for git.
            _ => {
                eprintln!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
    }

    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    // Scratch space for the intermediate pack+idx. It lives under the git dir
    // so the tempfile rename `gix-pack` performs stays on one filesystem. A dry
    // run never needs it: the writer keeps its temporaries elsewhere and
    // discards them.
    let scratch = if dry_run {
        None
    } else {
        Some(Scratch::new(&repo)?)
    };

    let mut stdin = io::stdin().lock();

    // Whatever has to sit in front of the real stdin bytes, plus the object
    // count the pack header declares when it is knowable up front.
    //
    // `--pack_header` supplies both directly. Otherwise the count is only
    // needed to tell a complete `-r` run from a lossy one, so the header is
    // read off the stream — and handed straight back — only in that mode; every
    // other invocation keeps the untouched stdin it has always had.
    let (prefix, declared_objects) = if let Some(hdr) = pack_header {
        (hdr.to_vec(), Some(u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]])))
    } else if recover {
        match peek_pack_header(&mut stdin) {
            Ok(peeked) => peeked,
            Err(e) => {
                eprintln!("fatal: {e}");
                return Ok(ExitCode::from(128));
            }
        }
    } else {
        (Vec::new(), None)
    };

    let source: Box<dyn BufRead> = if prefix.is_empty() {
        Box::new(stdin)
    } else {
        Box::new(io::Cursor::new(prefix).chain(stdin))
    };

    // The limit wraps the chained stream rather than bare stdin so a
    // reconstructed `--pack_header` counts against `--max-input-size`, which is
    // what git does: `unpack_all()` runs the synthesized header through the same
    // `use()` that advances `consumed_bytes`.
    let mut input = Limited {
        inner: source,
        limit: max_input_size,
        consumed: 0,
    };

    let options = gix::odb::pack::bundle::write::Options {
        // `Restore` returns everything decoded up to the damage instead of
        // failing the stream, which is the salvage `-r` asks for.
        iteration_mode: if recover {
            gix::odb::pack::data::input::Mode::Restore
        } else {
            gix::odb::pack::data::input::Mode::Verify
        },
        object_hash: repo.object_hash(),
        ..Default::default()
    };
    let should_interrupt = AtomicBool::new(false);
    let mut progress = gix::features::progress::Discard;

    let written = gix::odb::pack::Bundle::write_to_directory(
        &mut input,
        // A dry run still decodes and verifies every entry; it just discards
        // the index and pack instead of keeping them around to read back.
        scratch.as_ref().map(|s| s.path.as_path()),
        &mut progress,
        &should_interrupt,
        // Thin packs reference bases by id that only exist in the odb; letting
        // the writer look them up completes the pack the way git resolves them.
        Some(repo.objects.clone()),
        options,
    );

    // git checks the running byte count as it fills its input buffer, so a pack
    // over the limit dies whether or not it is otherwise well formed. Checking
    // the drained total covers both the error and the success path.
    if max_input_size != 0 && input.consumed > max_input_size {
        eprintln!("fatal: pack exceeds maximum allowed size");
        return Ok(ExitCode::from(128));
    }

    let outcome = match written {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };

    // git's `-r` swallows the per-object failure and reports the loss through
    // its exit status alone (`return has_errors`), so this is 1 rather than the
    // 128 a non-recovering run would have produced.
    let has_errors = recover
        && declared_objects.is_some_and(|declared| outcome.index.num_objects < declared);

    if dry_run {
        return Ok(done(has_errors));
    }

    // `to_bundle` is `None` only when nothing was written to disk, which for a
    // non-dry run means an empty pack — a valid input carrying zero objects.
    let Some(bundle) = outcome.to_bundle() else {
        return Ok(done(has_errors));
    };
    let bundle = match bundle {
        Ok(bundle) => bundle,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };

    let mut buf = Vec::with_capacity(64 * 1024);
    let mut inflate = gix::zlib::Inflate::default();
    let mut cache = gix::odb::pack::cache::Never;

    // Non-strict is git's streaming path: decode an object, write it, move on.
    if !strict {
        for idx in 0..bundle.index.num_objects() {
            let id = bundle.index.oid_at_index(idx).to_owned();
            let object = match bundle.get_object_by_index(idx, &mut buf, &mut inflate, &mut cache) {
                Ok((object, _location)) => object,
                Err(e) => {
                    eprintln!("fatal: {e}");
                    return Ok(ExitCode::from(128));
                }
            };
            // `Repository::write_buf_with_known_id` skips ids the odb already
            // has, which is exactly git's "objects that already exist are not
            // unpacked".
            if let Err(e) = repo.write_buf_with_known_id(object.kind, object.data, id) {
                eprintln!("fatal: {e}");
                return Ok(ExitCode::from(128));
            }
        }
        return Ok(done(has_errors));
    }

    // Strict is git's deferred path. Blobs go out as they are decoded; every
    // other object is parsed for structure now, held back, and only written
    // once `write_rest()`'s equivalent has confirmed that everything it points
    // at resolves. Nothing in the pack is written if any link dangles.
    let pack_ids: HashSet<gix::ObjectId> = (0..bundle.index.num_objects())
        .map(|idx| bundle.index.oid_at_index(idx).to_owned())
        .collect();

    let mut deferred: Vec<u32> = Vec::new();
    let mut referenced: Vec<gix::ObjectId> = Vec::new();

    for idx in 0..bundle.index.num_objects() {
        let id = bundle.index.oid_at_index(idx).to_owned();
        let object = match bundle.get_object_by_index(idx, &mut buf, &mut inflate, &mut cache) {
            Ok((object, _location)) => object,
            Err(e) => {
                eprintln!("fatal: {e}");
                return Ok(ExitCode::from(128));
            }
        };

        if object.kind == gix::object::Kind::Blob {
            if let Err(e) = repo.write_buf_with_known_id(object.kind, object.data, id) {
                eprintln!("fatal: {e}");
                return Ok(ExitCode::from(128));
            }
            continue;
        }

        // git: `parse_object_buffer()` failing here is `die("invalid %s", ...)`.
        let parsed = match gix::objs::ObjectRef::from_bytes(
            object.data,
            object.kind,
            repo.object_hash(),
        ) {
            Ok(parsed) => parsed,
            Err(_) => {
                eprintln!("fatal: invalid {}", object.kind);
                return Ok(ExitCode::from(128));
            }
        };
        collect_links(&parsed, &mut referenced);
        deferred.push(idx);
    }

    // A link resolves if the pack carries it or the odb already had it — the
    // same two places git looks before deciding an object is unwritable.
    if let Some(missing) = referenced
        .iter()
        .find(|id| !pack_ids.contains(*id) && !repo.has_object(*id))
    {
        eprintln!("fatal: missing object referenced by the pack: {missing}");
        eprintln!("fatal: fsck error in pack objects");
        return Ok(ExitCode::from(128));
    }

    for idx in deferred {
        let id = bundle.index.oid_at_index(idx).to_owned();
        let object = match bundle.get_object_by_index(idx, &mut buf, &mut inflate, &mut cache) {
            Ok((object, _location)) => object,
            Err(e) => {
                eprintln!("fatal: {e}");
                return Ok(ExitCode::from(128));
            }
        };
        if let Err(e) = repo.write_buf_with_known_id(object.kind, object.data, id) {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    }

    Ok(done(has_errors))
}

/// git's `cmd_unpack_objects()` ends in `return has_errors`, so a run that lost
/// objects to `-r`'s salvage exits 1 — the ordinary "expected negative" code,
/// not the 128 a fatal would have produced.
fn done(has_errors: bool) -> ExitCode {
    if has_errors {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Record every object id `object` points at, which is what decides whether it
/// is safe to write under `--strict`.
fn collect_links(object: &gix::objs::ObjectRef<'_>, out: &mut Vec<gix::ObjectId>) {
    match object {
        gix::objs::ObjectRef::Tree(tree) => out.extend(tree.entries.iter().map(|e| e.oid.to_owned())),
        gix::objs::ObjectRef::Commit(commit) => {
            out.push(commit.tree());
            out.extend(commit.parents());
        }
        gix::objs::ObjectRef::Tag(tag) => out.push(tag.target()),
        gix::objs::ObjectRef::Blob(_) => {}
    }
}

/// Validate a `--strict=<spec>` argument the way git's `fsck_set_msg_types()`
/// does, returning the `fatal:` body it would die with.
///
/// git lowercases the whole spec first, then walks the comma-separated
/// elements: each needs an `=`, the id must be known, and the severity must be
/// one it accepts from the command line. Empty elements are skipped, so
/// `--strict=,` is valid, and `skiplist=<path>` takes a path rather than a
/// severity. The id is checked before the severity, so `nosuchid=bogus` is
/// reported as an unknown id.
fn check_fsck_msg_types(spec: &str) -> Result<(), String> {
    let lowered = spec.to_ascii_lowercase();
    for element in lowered.split(',') {
        if element.is_empty() {
            continue;
        }
        let Some((id, severity)) = element.split_once('=') else {
            return Err(format!("Missing '=': '{element}'"));
        };
        if id == "skiplist" {
            continue;
        }
        if !FSCK_MSG_IDS.contains(&id) {
            return Err(format!("Unhandled message id: {id}"));
        }
        if !FSCK_SEVERITIES.contains(&severity) {
            return Err(format!("Unknown fsck message type: '{severity}'"));
        }
    }
    Ok(())
}

/// Rebuild the 12-byte pack header `--pack_header=<version>,<objects>`
/// describes, exactly as git's `parse_pack_header_option()` does: a `strtoul`
/// for the version, a literal comma, a `strtoul` for the entry count, and
/// nothing after it. `None` is git's `-1`, which it turns into
/// `die("bad --pack_header: %s")`.
///
/// Both numbers go through `strtoul`, so ` 2 ,0` is malformed (the space stops
/// the scan before the comma) while `+2,+0` is not, and `,` parses as two
/// zeroes — git accepts that and dies later on the version instead.
fn parse_pack_header_option(value: &str) -> Option<[u8; 12]> {
    let (version, rest) = strtoul(value);
    let rest = rest.strip_prefix(',')?;
    let (entries, rest) = strtoul(rest);
    if !rest.is_empty() {
        return None;
    }

    let mut header = [0u8; 12];
    header[0..4].copy_from_slice(b"PACK");
    header[4..8].copy_from_slice(&version.to_be_bytes());
    header[8..12].copy_from_slice(&entries.to_be_bytes());
    Some(header)
}

/// C's `strtoul` over a base-10 prefix, returning the value and the unconsumed
/// tail. Leading whitespace and a sign are skipped; when no digit follows, the
/// tail is the whole input, matching `strtoul`'s "no conversion performed"
/// contract of leaving `endptr` at the start.
///
/// The result is narrowed to 32 bits because every caller feeds it to git's
/// `store_be32`, which truncates the same way.
fn strtoul(s: &str) -> (u32, &str) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let negated = i < bytes.len() && bytes[i] == b'-';
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start {
        return (0, s);
    }
    // Out of range saturates the way `strtoul` does, at the maximum.
    let value: u64 = s[digits_start..i].parse().unwrap_or(u64::MAX);
    let value = value as u32;
    (if negated { value.wrapping_neg() } else { value }, &s[i..])
}

/// git parses `--max-input-size=` with `strtoumax(arg, NULL, 10)`: it consumes
/// the leading run of base-10 digits and ignores the rest, so `1k` is 1 and a
/// value with no leading digit is 0 (which then means "no limit").
fn parse_magnitude(s: &str) -> u64 {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return 0;
    }
    // Out of range saturates the way `strtoumax` does, at the maximum.
    digits.parse().unwrap_or(u64::MAX)
}

/// Take the 12-byte pack header off `stream`, returning the bytes read so they
/// can be chained back in front of it along with the object count they declare.
///
/// A short read is handed back verbatim and reported as no count at all: the
/// pack decoder downstream then produces the same truncation error it would
/// have without the peek, which keeps an empty stdin answering `early EOF`.
fn peek_pack_header(stream: &mut impl Read) -> io::Result<(Vec<u8>, Option<u32>)> {
    let mut header = [0u8; 12];
    let mut filled = 0;
    while filled < header.len() {
        match stream.read(&mut header[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }

    let bytes = header[..filled].to_vec();
    let declared = (filled == header.len() && &bytes[0..4] == b"PACK")
        .then(|| u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]));
    Ok((bytes, declared))
}

/// A scratch directory under the git dir, removed when this value is dropped so
/// the intermediate pack never survives an early return.
struct Scratch {
    path: std::path::PathBuf,
}

impl Scratch {
    fn new(repo: &gix::Repository) -> Result<Self> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = repo
            .git_dir()
            .join(format!("zvcs-unpack-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        Ok(Scratch { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Stdin wrapper that counts the pack bytes handed downstream and refuses to
/// serve more once `--max-input-size` has been passed, mirroring the check git
/// performs in its `fill()`.
struct Limited<R> {
    inner: R,
    /// The byte budget; `0` means unlimited, as in git.
    limit: u64,
    /// How many bytes the pack reader has taken so far.
    consumed: u64,
}

impl<R> Limited<R> {
    fn over_budget(&self) -> bool {
        self.limit != 0 && self.consumed > self.limit
    }

    fn check(&self) -> io::Result<()> {
        if self.over_budget() {
            return Err(io::Error::other("pack exceeds maximum allowed size"));
        }
        Ok(())
    }
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        let n = self.inner.read(buf)?;
        self.consumed += n as u64;
        Ok(n)
    }
}

impl<R: BufRead> BufRead for Limited<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.check()?;
        self.inner.fill_buf()
    }

    // Only `consume` advances the count for the buffered path; `read` accounts
    // for its own bytes above, and the two paths never overlap.
    fn consume(&mut self, amt: usize) {
        self.consumed += amt as u64;
        self.inner.consume(amt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two spellings that decide the failing parity cases: git accepts
    /// `--strict` and `-r` outright, so a usage error next to them has to come
    /// from the other argument, not the flag.
    #[test]
    fn strict_spec_validation_matches_git() {
        // Verified against git 2.55.0: `--strict=missingEmail` dies with the
        // lowercased element, `=ignore` is accepted, an unknown id beats an
        // unknown severity, and `skiplist` takes a path rather than a severity.
        assert_eq!(
            check_fsck_msg_types("missingEmail"),
            Err("Missing '=': 'missingemail'".into())
        );
        assert_eq!(check_fsck_msg_types("missingEmail=ignore"), Ok(()));
        assert_eq!(
            check_fsck_msg_types("nosuchid=bogus"),
            Err("Unhandled message id: nosuchid".into())
        );
        assert_eq!(
            check_fsck_msg_types("missingEmail=bogus"),
            Err("Unknown fsck message type: 'bogus'".into())
        );
        assert_eq!(
            check_fsck_msg_types("a=b,c"),
            Err("Unhandled message id: a".into())
        );
        assert_eq!(check_fsck_msg_types("skiplist=/dev/null"), Ok(()));
        assert_eq!(check_fsck_msg_types(","), Ok(()));
        assert_eq!(check_fsck_msg_types(""), Ok(()));
        // git rejects the underscore spelling: it compares against the
        // underscore-stripped name.
        assert_eq!(
            check_fsck_msg_types("missing_email=ignore"),
            Err("Unhandled message id: missing_email".into())
        );
        // Only these three severities are settable from the command line.
        assert_eq!(check_fsck_msg_types("badtree=error"), Ok(()));
        assert_eq!(check_fsck_msg_types("badtree=warn"), Ok(()));
        assert_eq!(
            check_fsck_msg_types("badtree=fatal"),
            Err("Unknown fsck message type: 'fatal'".into())
        );
        assert_eq!(
            check_fsck_msg_types("badtree=info"),
            Err("Unknown fsck message type: 'info'".into())
        );
    }

    #[test]
    fn pack_header_option_matches_git() {
        // Verified against git 2.55.0: `2,0` reconstructs a v2 header, a
        // missing or trailing component is `bad --pack_header`, whitespace
        // stops the scan before the comma, and `,` is two zeroes.
        let hdr = parse_pack_header_option("2,0").expect("2,0 is well formed");
        assert_eq!(&hdr[0..4], b"PACK");
        assert_eq!(u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]), 2);
        assert_eq!(u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]), 0);

        let hdr = parse_pack_header_option("2,17").expect("2,17 is well formed");
        assert_eq!(u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]), 17);

        // The version is carried through verbatim; the caller is what refuses
        // the ones the decoder cannot take, so parsing must not filter them.
        let hdr = parse_pack_header_option("3,0").expect("3,0 parses");
        assert_eq!(u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]), 3);
        let hdr = parse_pack_header_option("0,0").expect("0,0 parses");
        assert_eq!(u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]), 0);

        assert!(parse_pack_header_option("+2,+0").is_some());
        assert!(parse_pack_header_option(",").is_some());
        assert!(parse_pack_header_option("bad").is_none());
        assert!(parse_pack_header_option("2").is_none());
        assert!(parse_pack_header_option("2,3x").is_none());
        assert!(parse_pack_header_option("2,0,").is_none());
        assert!(parse_pack_header_option(" 2 ,0").is_none());
    }

    #[test]
    fn max_input_size_parses_like_strtoumax() {
        // git's `strtoumax(arg, NULL, 10)`: leading digits only, and a value
        // with no leading digit is 0, which then means "no limit".
        assert_eq!(parse_magnitude("1048576"), 1048576);
        assert_eq!(parse_magnitude("1k"), 1);
        assert_eq!(parse_magnitude("abc"), 0);
        assert_eq!(parse_magnitude(""), 0);
        assert_eq!(parse_magnitude("0"), 0);
    }

    /// A short stream has to come back untouched so the decoder still reports
    /// the truncation; an empty stdin is the common case for this.
    #[test]
    fn peeking_a_short_header_returns_every_byte() {
        let (bytes, declared) = peek_pack_header(&mut &b""[..]).expect("empty read succeeds");
        assert!(bytes.is_empty());
        assert_eq!(declared, None);

        let (bytes, declared) = peek_pack_header(&mut &b"PACK"[..]).expect("short read succeeds");
        assert_eq!(bytes, b"PACK");
        assert_eq!(declared, None);

        let mut full = Vec::from(*b"PACK");
        full.extend_from_slice(&2u32.to_be_bytes());
        full.extend_from_slice(&3u32.to_be_bytes());
        full.extend_from_slice(b"trailing");
        let (bytes, declared) = peek_pack_header(&mut full.as_slice()).expect("full read succeeds");
        assert_eq!(bytes.len(), 12);
        assert_eq!(declared, Some(3));
    }
}
