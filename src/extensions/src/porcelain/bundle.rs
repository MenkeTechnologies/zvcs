//! `git bundle` — move objects and refs by archive.
//!
//! Two of the four subcommands are ported in full and are byte-verifiable
//! against stock git (checked against git 2.55.0); the two that would have to
//! *write* pack data bail with the concrete substrate that is missing.
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
//!   * `-h` for `bundle` itself and for each of the four subcommands (usage to
//!     stdout, exit 129), plus `need a subcommand`, `unknown subcommand`,
//!     `unknown option`/`unknown switch` and `need a <file> argument`
//!   * `-` as `<file>`, meaning the bundle is read from stdin
//!
//! Exit codes match git: 0 on success, 1 for a bundle that cannot be opened,
//! parsed, or verified, 129 for usage errors.
//!
//! Not ported — these bail, naming the gap, rather than producing a pack that
//! only looks right:
//!   * `create` — needs a pack writer that can delta-compress and emit *thin*
//!     packs. `gix-pack`'s writer has exactly one mode, documented as "Copy
//!     base objects and deltas from packs, while non-packed objects will be
//!     treated as base objects (i.e. without trying to delta compress them)"
//!     (`gix-pack/src/data/output/entry/iter_from_counts.rs:362`). Every bundle
//!     built with a prerequisite is a thin pack, and a self-contained one would
//!     differ from git's byte-for-byte — and since `create` writes nothing to
//!     stdout and exits 0, a wrong bundle is indistinguishable from success.
//!   * `unbundle` — needs `index-pack`. `gix-pack::Bundle::write_to_directory`
//!     exists, but writes no `pack-*.rev` reverse index (grep for `rev` under
//!     `gix-pack/src/bundle/write` finds none) while git 2.55 writes one for
//!     every pack it stores, so the post-command object store diverges.
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
use std::io::{self, BufRead, BufReader, Write};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::Kind;

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
        "-h" => {
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
struct Header {
    /// The value of the `@object-format` capability, or `sha1` when absent.
    /// Printed verbatim by `verify` as the hash algorithm.
    hash: String,
    /// Prerequisite object ids (header lines starting with `-`). git prints the
    /// comment that follows them nowhere, so it is not retained.
    prereqs: Vec<ObjectId>,
    /// `(object id, ref name)` pairs. Ref names are kept as raw bytes because
    /// they are echoed verbatim and are not required to be UTF-8.
    refs: Vec<(ObjectId, Vec<u8>)>,
}

/// The failures git reports itself, with its own wording and exit code 1.
enum HeaderError {
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
        HeaderError::Malformed(why) => bail!("malformed bundle header in {path:?}: {why}"),
    }
    Ok(ExitCode::from(1))
}

/// Read one `\n`-terminated line, keeping the terminator. `Ok(None)` at EOF.
fn read_line(input: &mut dyn BufRead) -> Result<Option<Vec<u8>>, HeaderError> {
    let mut line = Vec::new();
    match input.read_until(b'\n', &mut line) {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(line)),
        Err(_) => Err(HeaderError::NotBundle),
    }
}

/// Parse the header of the bundle at `path` (`-` means stdin), stopping at the
/// blank line that separates it from the pack data.
fn read_header(path: &str) -> Result<Header, HeaderError> {
    let mut input: Box<dyn BufRead> = if path == "-" {
        Box::new(BufReader::new(io::stdin()))
    } else {
        Box::new(BufReader::new(
            File::open(path).map_err(|_| HeaderError::Open)?,
        ))
    };

    let magic = read_line(&mut *input)?.ok_or(HeaderError::NotBundle)?;
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

    let mut pending: Option<Vec<u8>> = None;
    // Capabilities (v3 only) come first, each on its own `@key[=value]` line.
    loop {
        let Some(line) = read_line(&mut *input)? else {
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
            None => read_line(&mut *input)?
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
            "-h" => {
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
        if !filters.is_empty() && !filters.iter().any(|f| *f == name.as_slice()) {
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
            "-h" => {
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

    // A prerequisite is satisfied only if the object is present *and* is a
    // commit — git adds non-commits to no pending list, so they read as missing.
    let missing: Vec<&ObjectId> = header
        .prereqs
        .iter()
        .filter(|oid| !matches!(repo.find_header(**oid).map(|h| h.kind()), Ok(Kind::Commit)))
        .collect();

    if !missing.is_empty() {
        if !quiet {
            eprintln!("error: Repository lacks these prerequisite commits:");
            for oid in missing {
                // git prints `<oid> <name>` with an empty name for prerequisites.
                eprintln!("error: {oid} ");
            }
        }
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

/// `git bundle create` is not ported; only `-h` is served.
fn create(args: &[String]) -> Result<ExitCode> {
    if args.iter().any(|a| a == "-h") {
        print!("{CREATE_USAGE}");
        return Ok(ExitCode::from(129));
    }
    bail!(
        "`bundle create` is not ported: writing a bundle needs a pack writer with delta \
         compression and thin-pack support; gix-pack's only mode is PackCopyAndBaseObjects \
         (gix-pack/src/data/output/entry/iter_from_counts.rs:362), which can produce neither \
         the thin pack a prerequisite bundle requires nor a pack matching git's bytes"
    )
}

/// `git bundle unbundle` is not ported; only `-h` is served.
fn unbundle(args: &[String]) -> Result<ExitCode> {
    if args.iter().any(|a| a == "-h") {
        print!("{UNBUNDLE_USAGE}");
        return Ok(ExitCode::from(129));
    }
    bail!(
        "`bundle unbundle` is not ported: storing the bundle's pack needs index-pack, and \
         gix-pack's Bundle::write_to_directory writes no pack-*.rev reverse index, which git \
         2.55 creates for every pack it stores — the object store would diverge"
    )
}
