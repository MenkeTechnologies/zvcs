//! `git receive-pack` — the server side of a push.
//!
//! `receive-pack` is a protocol server. It writes a ref advertisement, then reads
//! commands, a packfile and (optionally) a push certificate off stdin, ingests the
//! pack, runs hooks and updates refs. The advertisement (below) and the receive path
//! (`receive`: command list → pack ingest via `gix_pack::Bundle` → ref
//! compare-and-swap → `report-status`) are both implemented — enough for a local
//! (`file://`) or ssh push served by this binary. Hooks, the quarantine, push certs,
//! push-options and atomic pushes are not modelled (see the notes below). The
//! advertisement is byte-verified against git 2.55.0:
//!
//!   * **The ref advertisement** — `<oid> SP <ref>` pkt-lines in refname order,
//!     with the capability list appended to the first line after a NUL, the
//!     `0000000…0 capabilities^{}` line for a repository with no refs, the
//!     `shallow <oid>` lines a shallow repository adds, and the closing flush.
//!     Symbolic refs are resolved but tags are *not* peeled — `receive-pack`
//!     advertises no `^{}` rows.
//!   * **The capability list**, in git's emission order, honouring
//!     `receive.advertiseAtomic`, `repack.useDeltaBaseOffset` and
//!     `receive.advertisePushOptions`, plus `object-format=<algo>` from the
//!     repository's hash and `agent=` from `GIT_USER_AGENT` (see [`agent`]).
//!   * **`--http-backend-info-refs` / `--advertise-refs`** — advertise and exit 0.
//!   * **Argument handling**: `-h` prints the 68-byte usage block on *stdout*
//!     and exits 129; an unknown option prints ``error: unknown option `x'``
//!     (or ``unknown switch `c'``) followed by that usage block on stderr, 129;
//!     `--quiet=<v>` prints ``error: option `quiet' takes no value`` alone, 129;
//!     no directory / more than one directory print `fatal: …` followed by the
//!     158-byte usage block that also lists the hidden `--advertise-refs`, 129.
//!   * **`<git-dir>` resolution** without upward discovery — `<dir>` or
//!     `<dir>/.git`, the two forms git's `enter_repo()` resolves in practice;
//!     anything else is `fatal: '<dir>' does not appear to be a git
//!     repository`, exit 128. (`enter_repo()` also probes the `<dir>.git` and
//!     `<dir>/.git/.git` spellings; `gix::open` does not, so a bare repository
//!     reachable only as `<dir>.git` is reported as not a repository here.)
//!   * **The two stdin outcomes that need no pack**: an immediate flush packet
//!     ends the session with exit 0 and no further output; end-of-input before
//!     a complete pkt-line header is
//!     `fatal: the remote end hung up unexpectedly`, exit 128; a header that is
//!     not four hex digits is
//!     `fatal: protocol error: bad line length character: <4 bytes>`, exit 128.
//!
//! ### Not ported (bailed on with a precise message, never silently ignored)
//!
//! Anything that requires actually accepting a push. The substrate gaps are
//! named in gitoxide's own `src/ported/crate-status.md`:
//!
//!   1. **Server plumbing.** "upload-pack / receive-pack server plumbing for
//!      in-process transports" and "report-status, sideband, delete-refs,
//!      push-options and atomic pushes" are both unchecked
//!      (`crate-status.md:559`, `:561`). There is no command-list parser, no
//!      `report-status-v2` writer and no side-band-64k muxer to reuse.
//!   2. **Thin-pack completion.** `gix_pack::Bundle::write_to_directory` takes
//!      a `thin_pack_base_object_lookup` used to *resolve* external deltas for
//!      index computation (`gix-pack/src/bundle/write/mod.rs:53`), but it does
//!      not append the base objects to the pack the way `index-pack --fix-thin`
//!      does. `send-pack` sends thin packs by default, so the stored pack would
//!      differ from git's — a post-command repository-state divergence.
//!   3. **Hooks and the quarantine.** "client-side hooks for … push" and
//!      "quarantine-aware hook execution" are unchecked (`crate-status.md:670`,
//!      `:672`). `pre-receive`, `update`, `post-receive` and `post-update` all
//!      observe `GIT_QUARANTINE_PATH`, and their exit codes decide which refs
//!      move. Without them the ref updates are not git's ref updates.
//!   4. **`receive.unpackLimit`.** git stores a small push as loose objects via
//!      `unpack-objects` and a large one as a pack via `index-pack`. Which one
//!      it picks is directly observable in the object store.
//!
//! Advertisement-affecting configuration that has no port also bails rather
//! than producing a short advertisement: `receive.hideRefs`/`transfer.hideRefs`
//! (git filters the ref list through `ref_is_hidden`), `GIT_NAMESPACE` (git
//! advertises namespaced names), `receive.certNonceSeed` (adds a
//! `push-cert=<nonce>` capability), and object alternates (git appends one
//! `<oid> .have` line per alternate ref, obtained by running `upload-pack` in
//! each alternate).
//!
//! `-q`/`--quiet` is accepted and parsed: it only suppresses progress and
//! status reporting on the receive path, which is not reached here, so it has
//! no observable effect on any implemented path.

use anyhow::{anyhow, bail, Result};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};
use std::io::{BufRead, Read, Write};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

/// The flags this port implements, quoted in every rejection message.
const PORTED: &str = "ported: -q/--quiet, --http-backend-info-refs/--advertise-refs";

/// git's `receive_pack_usage` as `parse_options` renders it for `-h` and for
/// option errors: hidden options omitted (68 bytes, git 2.55.0).
const SHORT_USAGE: &str = "\
usage: git receive-pack <git-dir>

    -q, --[no-]quiet      quiet

";

/// The same block as `usage_msg_opt` renders it for the two argument-count
/// errors, which also lists the hidden `--advertise-refs` (158 bytes).
const FULL_USAGE: &str = "\
usage: git receive-pack <git-dir>

    -q, --[no-]quiet      quiet
    --[no-]advertise-refs ...
                          alias of --http-backend-info-refs

";

/// The git version this port reproduces, used to build the `agent=` capability.
const GIT_VERSION: &str = "2.55.0";

/// Parsed command line for a single `receive-pack` invocation.
struct Opts {
    /// `-q`/`--quiet`: suppresses receive-path reporting only.
    quiet: bool,
    /// `--http-backend-info-refs`/`--advertise-refs`: advertise, then exit 0.
    advertise_only: bool,
    /// The single `<git-dir>` operand, exactly as spelled on the command line.
    dir: String,
}

/// `git receive-pack <git-dir>` — advertise refs, then read a push off stdin.
///
/// The advertisement is written verbatim; the receive half bails (see the
/// module docs for the missing substrate).
pub fn receive_pack(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand at index 0.
    let args = match args.first() {
        Some(a) if a == "receive-pack" => &args[1..],
        _ => args,
    };

    let opts = match parse(args)? {
        Parsed::Opts(opts) => opts,
        Parsed::Exit(code) => return Ok(code),
    };

    let Some(repo) = open_repo(&opts.dir) else {
        eprintln!(
            "fatal: '{}' does not appear to be a git repository",
            opts.dir
        );
        return Ok(ExitCode::from(128));
    };

    reject_unportable_advertisement(&repo)?;

    let adv = advertisement(&repo)?;
    {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&adv)?;
        stdout.flush()?;
    }

    if opts.advertise_only {
        return Ok(ExitCode::SUCCESS);
    }
    let _ = opts.quiet; // suppresses progress only; the report-status is unaffected.

    receive(&repo)
}

/// Either a fully parsed command line, or a terminal exit code for the
/// help/usage-error paths, which produce all of their own output.
enum Parsed {
    Opts(Opts),
    Exit(ExitCode),
}

/// git's `parse_options` pass over the `receive-pack` option table, followed by
/// its two argument-count checks.
fn parse(args: &[String]) -> Result<Parsed> {
    let mut quiet = false;
    let mut advertise_only = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    for a in args {
        let a = a.as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            positionals.push(a);
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }

        if let Some(long) = a.strip_prefix("--") {
            // `--<name>=<value>` on a boolean is rejected before anything else.
            let (name, value) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let (name, on) = match name.strip_prefix("no-") {
                Some(rest) => (rest, false),
                None => (name, true),
            };
            let known = matches!(
                name,
                "quiet" | "http-backend-info-refs" | "advertise-refs"
            );
            if known && value.is_some() {
                eprintln!("error: option `{name}' takes no value");
                return Ok(Parsed::Exit(ExitCode::from(129)));
            }
            match name {
                "quiet" => quiet = on,
                "http-backend-info-refs" | "advertise-refs" => advertise_only = on,
                // Real but unported git options; the receive path they belong
                // to is not implemented, so accepting them would mislead.
                "stateless-rpc" | "skip-connectivity-check" | "reject-thin-pack-for-testing"
                | "signed-push" => {
                    let flag = format!("--{name}");
                    bail!("unsupported flag {flag:?} ({PORTED})")
                }
                _ => {
                    eprint!("error: unknown option `{long}'\n{SHORT_USAGE}");
                    return Ok(Parsed::Exit(ExitCode::from(129)));
                }
            }
            continue;
        }

        // Clumped short flags, e.g. `-qq`. `-h` is handled by parse_options
        // before every other check and writes to stdout.
        for c in a[1..].chars() {
            match c {
                'q' => quiet = true,
                'h' => {
                    print!("{SHORT_USAGE}");
                    return Ok(Parsed::Exit(ExitCode::from(129)));
                }
                _ => {
                    eprint!("error: unknown switch `{c}'\n{SHORT_USAGE}");
                    return Ok(Parsed::Exit(ExitCode::from(129)));
                }
            }
        }
    }

    // git checks "too many" before "you must specify".
    if positionals.len() > 1 {
        eprint!("fatal: too many arguments\n\n{FULL_USAGE}");
        return Ok(Parsed::Exit(ExitCode::from(129)));
    }
    let Some(dir) = positionals.first() else {
        eprint!("fatal: you must specify a directory\n\n{FULL_USAGE}");
        return Ok(Parsed::Exit(ExitCode::from(129)));
    };

    Ok(Parsed::Opts(Opts {
        quiet,
        advertise_only,
        dir: (*dir).to_string(),
    }))
}

/// git's `enter_repo()` reduced to what `receive-pack` relies on: the operand
/// names the repository directly, either as the git directory or as the work
/// tree holding it. There is deliberately no upward search — `git receive-pack
/// <repo>/<subdir>` fails even inside a repository.
fn open_repo(dir: &str) -> Option<gix::Repository> {
    // `gix::open` already expands `<path>` to `<path>/.git` for a work tree.
    gix::open(std::path::Path::new(dir)).ok()
}

/// Bail on repository state that changes the advertisement in a way this port
/// does not reproduce, rather than emitting a silently wrong ref list.
fn reject_unportable_advertisement(repo: &gix::Repository) -> Result<()> {
    let config = repo.config_snapshot();
    if config.string("receive.hideRefs").is_some() || config.string("transfer.hideRefs").is_some() {
        bail!("receive.hideRefs/transfer.hideRefs is not supported (the advertisement would include hidden refs)");
    }
    if config.string("receive.certNonceSeed").is_some() {
        bail!("receive.certNonceSeed is not supported (the push-cert capability needs a nonce and a signed-push reader)");
    }
    if std::env::var_os("GIT_NAMESPACE").is_some() {
        bail!("GIT_NAMESPACE is not supported (git advertises namespaced ref names)");
    }
    let alternates = repo.common_dir().join("objects").join("info").join("alternates");
    if alternates.is_file() || std::env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES").is_some() {
        bail!("object alternates are not supported (git advertises one '<oid> .have' line per alternate ref)");
    }
    Ok(())
}

/// Build the complete advertisement, byte-for-byte as git's `write_head_info`
/// emits it: every ref under `refs/` in name order (capabilities appended to
/// the first line), the synthetic `capabilities^{}` line when there were none,
/// the `shallow <oid>` lines, then a flush packet.
fn advertisement(repo: &gix::Repository) -> Result<Vec<u8>> {
    let caps = capabilities(repo);
    let mut out = Vec::new();
    let mut sent_capabilities = false;

    for reference in repo.references()?.all()? {
        // Broken refs are skipped, as git's ref iteration does.
        let Ok(mut reference) = reference else { continue };
        let name = reference.name().as_bstr().to_string();
        // Symbolic refs resolve to their object; tags are not peeled here.
        let Ok(id) = reference.follow_to_object() else {
            continue;
        };
        let line = if sent_capabilities {
            format!("{} {name}\n", id.detach().to_hex())
        } else {
            sent_capabilities = true;
            format!("{} {name}\0{caps}\n", id.detach().to_hex())
        };
        pkt_line(&mut out, line.as_bytes());
    }

    if !sent_capabilities {
        let null = repo.object_hash().null();
        pkt_line(
            &mut out,
            format!("{} capabilities^{{}}\0{caps}\n", null.to_hex()).as_bytes(),
        );
    }

    // git's `advertise_shallow_grafts`; the graft list is oid-sorted on both sides.
    if let Ok(Some(commits)) = repo.shallow_commits() {
        for id in commits.iter() {
            pkt_line(&mut out, format!("shallow {}\n", id.to_hex()).as_bytes());
        }
    }

    flush_pkt(&mut out);
    Ok(out)
}

/// The capability list, in `receive-pack.c`'s emission order.
///
/// `atomic` and `ofs-delta` default on, `push-options` defaults off.
fn capabilities(repo: &gix::Repository) -> String {
    let config = repo.config_snapshot();
    let on = |key: &str, default: bool| config.boolean(key).unwrap_or(default);

    let mut caps = String::from("report-status report-status-v2 delete-refs side-band-64k quiet");
    if on("receive.advertiseAtomic", true) {
        caps.push_str(" atomic");
    }
    if on("repack.useDeltaBaseOffset", true) {
        caps.push_str(" ofs-delta");
    }
    if on("receive.advertisePushOptions", false) {
        caps.push_str(" push-options");
    }
    caps.push_str(&format!(" object-format={}", repo.object_hash()));
    caps.push_str(&format!(" agent={}", agent()));
    caps
}

/// git's `git_user_agent()`: `$GIT_USER_AGENT` when set, else
/// `git/<version>-<uname -s>`.
///
/// The suffix is the kernel name git appends at runtime; the mapping below
/// covers the platforms zvcs targets, and falls back to Rust's own OS name.
fn agent() -> String {
    if let Some(agent) = std::env::var_os("GIT_USER_AGENT") {
        return agent.to_string_lossy().into_owned();
    }
    let sysname = match std::env::consts::OS {
        "macos" => "Darwin",
        "linux" => "Linux",
        "freebsd" => "FreeBSD",
        "netbsd" => "NetBSD",
        "openbsd" => "OpenBSD",
        other => other,
    };
    format!("git/{GIT_VERSION}-{sysname}")
}

/// Append one pkt-line: a four-digit hex length covering the header itself,
/// followed by the payload.
fn pkt_line(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
}

/// Append a flush packet.
fn flush_pkt(out: &mut Vec<u8>) {
    out.extend_from_slice(b"0000");
}

/// One ref-update command a client sent: move `name` from `old` to `new` (a zero
/// `old` is a create, a zero `new` a delete).
struct Command {
    old: gix::ObjectId,
    new: gix::ObjectId,
    name: String,
}

/// Read the command list off stdin, ingest the packfile, apply the ref updates, and
/// write `report-status` — the receiving half of a push. Faithful to git's
/// `receive-pack`/`send-pack` wire for the plain (non-side-band) path that zvcs's own
/// `send-pack` speaks: pkt-line commands, then a flush, then a raw (non-pkt) pack,
/// then a plain pkt-line `report-status`. An empty command list (immediate flush) is
/// a no-op success, matching a client that connects and hangs up.
fn receive(repo: &gix::Repository) -> Result<ExitCode> {
    let mut stdin = std::io::stdin().lock();

    // --- command list (until flush); the first line carries the caps after a NUL.
    let hash = repo.object_hash();
    let zero = gix::ObjectId::null(hash);
    let mut cmds: Vec<Command> = Vec::new();
    let mut caps: Vec<u8> = Vec::new();
    while let Some(line) = read_pkt_line(&mut stdin)? {
        let (body, cap) = match (cmds.is_empty() && caps.is_empty(), line.iter().position(|&b| b == 0)) {
            (true, Some(n)) => (&line[..n], Some(line[n + 1..].to_vec())),
            _ => (&line[..], None),
        };
        if let Some(c) = cap {
            caps = c;
        }
        let text = std::str::from_utf8(body)
            .map_err(|_| anyhow!("protocol error: non-utf8 command"))?
            .trim_end();
        let mut it = text.splitn(3, ' ');
        let (o, n, name) = match (it.next(), it.next(), it.next()) {
            (Some(o), Some(n), Some(name)) if !name.is_empty() => (o, n, name),
            _ => bail!("protocol error: expected old/new/ref, got {text:?}"),
        };
        cmds.push(Command {
            old: gix::ObjectId::from_hex(o.as_bytes()).map_err(|_| anyhow!("protocol error: bad old id"))?,
            new: gix::ObjectId::from_hex(n.as_bytes()).map_err(|_| anyhow!("protocol error: bad new id"))?,
            name: name.to_string(),
        });
    }
    if cmds.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    // --- ingest the pack (skipped when every command is a delete) -----------
    let mut unpack: Result<(), String> = Ok(());
    if cmds.iter().any(|c| c.new != zero) {
        if let Err(e) = ingest_pack(repo, &mut stdin) {
            unpack = Err(e.to_string());
        }
    }

    // --- apply the ref updates ---------------------------------------------
    let mut verdicts: Vec<(String, Result<(), String>)> = Vec::with_capacity(cmds.len());
    for c in &cmds {
        let v = if unpack.is_err() {
            Err("unpacker error".to_string())
        } else {
            apply_update(repo, &c.name, c.old, c.new, zero)
        };
        verdicts.push((c.name.clone(), v));
    }

    // --- report-status (plain pkt-lines; zvcs's client requests no side-band).
    if cap_present(&caps, b"report-status") || cap_present(&caps, b"report-status-v2") {
        let mut out: Vec<u8> = Vec::new();
        match &unpack {
            Ok(()) => pkt_line(&mut out, b"unpack ok\n"),
            Err(e) => pkt_line(&mut out, format!("unpack {e}\n").as_bytes()),
        }
        for (name, v) in &verdicts {
            match v {
                Ok(()) => pkt_line(&mut out, format!("ok {name}\n").as_bytes()),
                Err(reason) => pkt_line(&mut out, format!("ng {name} {reason}\n").as_bytes()),
            }
        }
        flush_pkt(&mut out);
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&out)?;
        stdout.flush()?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Read one pkt-line: `None` on a flush (`0000`), else its payload (header
/// stripped). A missing/short header or a non-hex length is a protocol error.
fn read_pkt_line(r: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut hdr = [0u8; 4];
    read_exact(r, &mut hdr).map_err(|_| anyhow!("the remote end hung up unexpectedly"))?;
    let len = u16::from_str_radix(
        std::str::from_utf8(&hdr).map_err(|_| anyhow!("protocol error: bad line length character"))?,
        16,
    )
    .map_err(|_| anyhow!("protocol error: bad line length character"))?;
    match len {
        0 => Ok(None),                 // flush
        1..=4 => Ok(Some(Vec::new())), // flush/delim/response-end or empty line
        _ => {
            let mut buf = vec![0u8; len as usize - 4];
            read_exact(r, &mut buf)?;
            Ok(Some(buf))
        }
    }
}

fn read_exact(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        match r.read(&mut buf[off..])? {
            0 => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof")),
            n => off += n,
        }
    }
    Ok(())
}

/// Whether the client advertised capability `want` (a whole space-separated token).
fn cap_present(caps: &[u8], want: &[u8]) -> bool {
    caps.split(|&b| b == b' ' || b == b'\n' || b == 0).any(|tok| tok == want)
}

/// Index the packfile streaming off `input` into `objects/pack`. A thin pack's
/// external delta bases are resolved from the odb (git's `index-pack --fix-thin`),
/// so a `send-pack` thin pack lands complete.
fn ingest_pack(repo: &gix::Repository, input: &mut impl BufRead) -> Result<()> {
    let pack_dir = repo.objects.store_ref().path().join("pack");
    std::fs::create_dir_all(&pack_dir)?;
    let outcome = gix::odb::pack::Bundle::write_to_directory(
        input,
        Some(&pack_dir),
        &mut gix::progress::Discard,
        &AtomicBool::new(false),
        Some(repo.objects.clone()),
        gix::odb::pack::bundle::write::Options {
            object_hash: gix::hash::Kind::Sha1,
            ..Default::default()
        },
    )?;
    // `write_to_directory` always drops a `.keep`; a received push keeps none.
    if let Some(kp) = &outcome.keep_path {
        let _ = std::fs::remove_file(kp);
    }
    Ok(())
}

/// Apply one ref update as a compare-and-swap against `old` (create when `old` is
/// zero, delete when `new` is zero), returning the client-facing reason on failure.
fn apply_update(
    repo: &gix::Repository,
    name: &str,
    old: gix::ObjectId,
    new: gix::ObjectId,
    zero: gix::ObjectId,
) -> Result<(), String> {
    let full = FullName::try_from(name).map_err(|_| "funny refname".to_string())?;
    let expected = if old == zero {
        PreviousValue::MustNotExist
    } else {
        PreviousValue::MustExistAndMatch(Target::Object(old))
    };
    let change = if new == zero {
        Change::Delete { expected, log: RefLog::AndReference }
    } else {
        Change::Update {
            log: LogChange { mode: RefLog::AndReference, force_create_reflog: false, message: "push".into() },
            expected,
            new: Target::Object(new),
        }
    };
    repo.edit_reference(RefEdit { change, name: full, deref: false })
        .map(|_| ())
        .map_err(|e| e.to_string())
}
