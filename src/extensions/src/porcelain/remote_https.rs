//! `git remote-https` — the smart-HTTP remote helper (stock git ships this as
//! `git-remote-curl`, hard-linked to `git-remote-http`/`-https`/`-ftp`/`-ftps`).
//!
//! This is not a porcelain command: git invokes it as
//! `git remote-https <remote> [<url>]` and then speaks the remote-helper line
//! protocol over its stdin/stdout. Every diagnostic here therefore quotes
//! `remote-curl`, matching the binary stock git actually execs.
//!
//! Covered, byte-for-byte against stock git:
//!   * argument validation — the `error: remote-curl: usage: ...` line on
//!     stderr with exit 1.
//!   * the command loop itself, including its two termination paths: a blank
//!     command line exits 0, EOF without one exits 1.
//!   * `capabilities` — the fixed advertisement block.
//!   * `option <name> [<value>]` — the full `set_option` table, reproducing
//!     git's `ok` / `unsupported` / `error invalid value` replies and the
//!     `fatal: unknown value for object-format: <v>` die (exit 128).
//!   * `list` — the v0/v1 ref advertisement (`@<target> HEAD` for a symref,
//!     `<oid> <ref>` otherwise, terminated by a blank line), the leading
//!     `:object-format <algo>` line when `option object-format true` was set,
//!     and the empty advertisement stock emits under `protocol.version=2`,
//!     where the real ref exchange moves into `stateless-connect`.
//!   * unknown commands — `error: remote-curl: unknown command '<cmd>' from git`
//!     with exit 1.
//!
//! Not covered, and bailing rather than emitting plausible-looking wire data:
//!   * `push` and `list for-push` — the vendored gitoxide crates implement no
//!     send-pack/receive-pack client at all; `gix_transport::Service::ReceivePack`
//!     exists only as a service-name string, with no protocol behind it.
//!   * `fetch` — gitoxide's fetch is driven through `gix::remote`, not through
//!     the helper's batched `fetch <sha1> <name>` contract with its own
//!     shallow/depth/`check-connectivity` state, and there is no way to hand it
//!     the helper's negotiated option set.
//!   * `stateless-connect` — a raw pkt-line proxy between git and the server
//!     across multiple stateless HTTP round trips; the vendored transport
//!     exposes no such passthrough.
//!   * `get` — bundle-URI download, absent from the vendored crates.

use anyhow::{bail, Result};
use std::io::{BufRead, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::protocol::handshake::Ref;

/// The exact advertisement stock `remote-curl` prints for `capabilities`,
/// including the blank line that terminates it.
const CAPABILITIES: &str =
    "stateless-connect\nfetch\nget\noption\npush\ncheck-connectivity\nobject-format\n\n";

/// Helper commands this port implements, quoted in every rejection message.
const PORTED: &str = "ported: capabilities, option, list";

/// State accumulated by `option` that later commands read back.
#[derive(Default)]
struct Options {
    /// `option object-format true` — makes `list` prefix a `:object-format` line.
    object_format: bool,
}

/// The four replies stock `set_option` can produce for one `option` line.
enum Reply {
    /// `ok` — recognized and accepted.
    Accepted,
    /// `unsupported` — not a `remote-curl` option.
    Unsupported,
    /// `error invalid value` — recognized, but the value did not parse.
    InvalidValue,
    /// `die()` — printed to stderr, terminating the helper with exit 128.
    Fatal(String),
}

/// `git remote-https` — smart-HTTP remote helper.
///
/// Reads the remote-helper protocol from stdin and answers on stdout until a
/// blank command line (exit 0) or EOF (exit 1), exactly as stock `remote-curl`
/// does.
pub fn remote_https(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand at index 0; tolerate its absence so the
    // function can also be driven directly with bare helper arguments.
    let args = match args.first().map(String::as_str) {
        Some("remote-https" | "remote-http") => &args[1..],
        _ => args,
    };

    // Stock accepts `<remote>` or `<remote> <url>` and nothing else.
    if args.is_empty() || args.len() > 2 {
        eprintln!("error: remote-curl: usage: git remote-curl <remote> [<url>]");
        return Ok(ExitCode::from(1));
    }
    // With both present the URL is authoritative (git has already applied
    // `url.<base>.insteadOf`); with one, it names a configured remote.
    let spec = args[args.len() - 1].as_str();

    let mut opts = Options::default();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        // A blank command line ends the batch; the helper exits successfully.
        if line.is_empty() {
            return Ok(ExitCode::SUCCESS);
        }

        if line == "capabilities" {
            stdout.write_all(CAPABILITIES.as_bytes())?;
            stdout.flush()?;
        } else if line == "option" || line.starts_with("option ") {
            let rest = line.strip_prefix("option ").unwrap_or("");
            match set_option(rest, &mut opts) {
                Reply::Accepted => writeln!(stdout, "ok")?,
                Reply::Unsupported => writeln!(stdout, "unsupported")?,
                Reply::InvalidValue => writeln!(stdout, "error invalid value")?,
                Reply::Fatal(msg) => {
                    eprintln!("fatal: {msg}");
                    return Ok(ExitCode::from(128));
                }
            }
            stdout.flush()?;
        } else if line == "list" || line.starts_with("list ") {
            // git's own test is a substring search over the argument tail.
            if line["list".len()..].contains("for-push") {
                bail!(
                    "'list for-push' needs a git-receive-pack advertisement, \
                     which the vendored gitoxide crates do not implement ({PORTED})"
                );
            }
            let out = list(spec, &opts)?;
            stdout.write_all(out.as_bytes())?;
            stdout.flush()?;
        } else if line == "fetch" || line.starts_with("fetch ") {
            bail!(
                "'fetch' needs the helper's batched fetch contract wired to pack \
                 negotiation, which gitoxide only exposes through gix::remote ({PORTED})"
            );
        } else if line == "push" || line.starts_with("push ") {
            bail!("'push' needs a send-pack client, absent from the vendored gitoxide crates ({PORTED})");
        } else if line == "stateless-connect" || line.starts_with("stateless-connect ") {
            bail!(
                "'stateless-connect' needs a raw pkt-line passthrough over the HTTP \
                 transport, which gix-transport does not expose ({PORTED})"
            );
        } else if line == "get" || line.starts_with("get ") {
            bail!("'get' needs bundle-URI download support, absent from the vendored gitoxide crates ({PORTED})");
        } else {
            eprintln!("error: remote-curl: unknown command '{line}' from git");
            return Ok(ExitCode::from(1));
        }
    }

    // EOF without the terminating blank line is an error for stock git too.
    Ok(ExitCode::from(1))
}

/// Stock `remote-curl`'s `set_option`, for one `<name> [<value>]` pair.
///
/// The recognized set and each option's value discipline were read off stock
/// `git remote-https`; anything outside it answers `unsupported` rather than
/// being silently swallowed.
fn set_option(rest: &str, opts: &mut Options) -> Reply {
    let (name, value) = match rest.split_once(' ') {
        Some((n, v)) => (n, v),
        None => (rest, ""),
    };

    match name {
        // Integer-valued.
        "verbosity" | "depth" => match value.trim().parse::<i64>() {
            Ok(_) => Reply::Accepted,
            Err(_) => Reply::InvalidValue,
        },

        // Boolean-valued.
        "progress" | "dry-run" | "followtags" | "update-shallow" | "deepen-relative"
        | "atomic" | "force" | "check-connectivity" => match parse_bool(value) {
            Some(_) => Reply::Accepted,
            None => Reply::InvalidValue,
        },

        // Boolean, but `if-asked` is also a legal value.
        "pushcert" => {
            if value == "if-asked" || parse_bool(value).is_some() {
                Reply::Accepted
            } else {
                Reply::InvalidValue
            }
        }

        // Only the literal `true` is accepted; anything else is fatal.
        "object-format" => {
            if value == "true" {
                opts.object_format = true;
                Reply::Accepted
            } else {
                Reply::Fatal(format!("unknown value for object-format: {value}"))
            }
        }

        // Opaque strings, accepted verbatim (including empty).
        "cas" | "filter" | "deepen-since" | "deepen-not" | "push-option" | "from-promisor" => {
            Reply::Accepted
        }

        _ => Reply::Unsupported,
    }
}

/// git's `git_parse_maybe_bool`: `None` when the value is not a boolean.
///
/// An empty value is false, matching `git_parse_maybe_bool_text`'s `!*value`
/// branch (the "option present without argument" case).
fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "" | "false" | "no" | "off" | "0" => Some(false),
        "true" | "yes" | "on" | "1" => Some(true),
        _ => None,
    }
}

/// Render the `list` reply for `spec` (a remote name or URL).
///
/// Under `protocol.version=2` stock git advertises nothing here — v2 carries no
/// ref advertisement in the initial request, and git reaches the refs through
/// `stateless-connect` instead — so only the terminating blank line is emitted.
/// The handshake still runs so that an unreachable remote fails the same way it
/// does for stock git.
fn list(spec: &str, opts: &Options) -> Result<String> {
    // gitoxide resolves transport, credential, `http.*` and `insteadOf`
    // configuration through a Repository; there is no repository-less remote.
    let Ok(repo) = gix::discover(".") else {
        bail!("remote-curl outside a repository is not supported (no repository found)")
    };

    let remote = repo.find_fetch_remote(Some(BStr::new(spec)))?;
    let connection = remote.connect(gix::remote::Direction::Fetch)?;
    // `prefix_from_spec_as_filter_on_remote` must be off: the helper reports
    // every advertised ref, not just the ones the remote's refspecs would fetch.
    let (map, handshake) = connection.ref_map(
        gix::progress::Discard,
        gix::remote::ref_map::Options {
            prefix_from_spec_as_filter_on_remote: false,
            ..Default::default()
        },
    )?;

    let mut out = String::new();
    if handshake.server_protocol_version != gix::protocol::transport::Protocol::V2 {
        if opts.object_format {
            // The advertised algorithm, defaulting to git's own fallback.
            let algo = handshake
                .capabilities
                .capability("object-format")
                .and_then(|c| c.value().map(|v| v.to_str_lossy().into_owned()))
                .unwrap_or_else(|| "sha1".to_owned());
            out.push_str(&format!(":object-format {algo}\n"));
        }
        for r in &map.remote_refs {
            match r {
                // A symref is reported by target, never by object id, and the
                // peeled `^{}` rows of `ls-remote` are not part of this protocol.
                Ref::Symbolic {
                    full_ref_name,
                    target,
                    ..
                } => out.push_str(&format!("@{target} {full_ref_name}\n")),
                Ref::Peeled {
                    full_ref_name, tag, ..
                } => out.push_str(&format!("{} {full_ref_name}\n", tag.to_hex())),
                Ref::Direct {
                    full_ref_name,
                    object,
                } => out.push_str(&format!("{} {full_ref_name}\n", object.to_hex())),
                // v1 advertises nothing at all for an unborn HEAD.
                Ref::Unborn { .. } => {}
            }
        }
    }
    out.push('\n');
    Ok(out)
}
