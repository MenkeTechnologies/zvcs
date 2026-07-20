//! `git credential` — the scriptable front end to git's credential-helper
//! protocol, backed by the vendored `gix-credentials` cascade.
//!
//! Supported actions, with stdout byte-identical to stock git:
//!   * `git credential fill`       — read a credential description on stdin,
//!                                    consult the configured helpers (and, if
//!                                    they come up short, prompt), then print
//!                                    the completed description.
//!   * `git credential approve`    — send `store` to every configured helper.
//!   * `git credential reject`     — send `erase` to every configured helper.
//!   * `git credential capability` — print the fixed capability announcement.
//!
//! Helper discovery, `credential.<url>.*` subsection matching, `credential.
//! username`, `credential.useHttpPath`, `credential.protectProtocol` and the
//! askpass/terminal prompt policy all come from
//! [`gix::config::Snapshot::credential_helpers`], i.e. the same config engine
//! git uses.
//!
//! Exit codes match stock git: `0` on success, `129` for a usage error, `128`
//! for a fatal error.
//!
//! Deliberately **not** ported, because `gix_credentials::protocol::Context`
//! has no field to carry them and silently dropping them would hand a caller a
//! credential that looks right but is not: the `authtype`, `credential`,
//! `ephemeral` and `continue` attributes, and every multi-valued `key[]`
//! attribute (`capability[]`, `state[]`, `wwwauth[]`). Those `bail!` on input.
//!
//! Note that `git credential capability` still prints git's fixed
//! `authtype`/`state` announcement verbatim, because that string is the
//! protocol-version handshake callers match on. A caller that takes it up on
//! the offer and sends `capability[]=authtype` gets the hard error above
//! rather than a silently degraded credential.
//!
//! Known divergences from stock git:
//!   * Helper *stdin* is written by `gix-credentials`, which leads with a
//!     `url=` line and orders the remaining keys differently than git. Helpers
//!     that parse `key=value` see the same credential; a helper that echoes its
//!     raw input verbatim would differ. Additionally, `gix_url` drops a port
//!     that equals the scheme default, so `https://host:443` reaches a helper
//!     as `host=host` where git sends `host=host:443`. Our own stdout keeps the
//!     port verbatim either way.
//!   * `fatal:` message text for errors originating inside `gix-credentials`
//!     (helper I/O, prompt failure, `quit=1`) is gitoxide's, not git's. The
//!     exit code is still 128.
//!   * A complete credential (`username` + `password`) that is missing `host`
//!     makes stock git hit a `BUG:` assertion and abort with 134; we report a
//!     fatal and exit 128.
//!   * `git credential` outside of a repository is not supported — the
//!     credential config engine is reached through a `gix::Repository`.

use anyhow::Result;
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice, ByteVec};
use gix::credentials::helper::Action;
use gix::credentials::protocol::Context;

/// Stock git's usage line, verbatim (it omits `capability`, as git's does).
const USAGE: &str = "usage: git credential (fill|approve|reject)";

/// The action requested on the command line.
#[derive(Clone, Copy)]
enum Op {
    Fill,
    Approve,
    Reject,
}

/// `git credential (fill|approve|reject|capability)`.
///
/// `args[0]` is the subcommand name itself; exactly one further argument — the
/// action — is accepted, matching git's own arity check.
pub fn credential(args: &[String]) -> Result<ExitCode> {
    let rest = &args[1..];
    if rest.len() != 1 {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
    match rest[0].as_str() {
        // A fixed string: the protocol version, then one line per capability
        // this side of the protocol understands.
        "capability" => {
            print!("version 0\ncapability authtype\ncapability state\n");
            Ok(ExitCode::SUCCESS)
        }
        "fill" => run(Op::Fill),
        "approve" => run(Op::Approve),
        "reject" => run(Op::Reject),
        _ => {
            eprintln!("{USAGE}");
            Ok(ExitCode::from(129))
        }
    }
}

/// A credential description: git's attribute set, kept as raw bytes so values
/// round-trip to stdout unchanged.
#[derive(Default)]
struct Cred {
    protocol: Option<BString>,
    host: Option<BString>,
    path: Option<BString>,
    username: Option<BString>,
    password: Option<BString>,
    oauth_refresh_token: Option<BString>,
    password_expiry_utc: Option<BString>,
}

fn run(op: Op) -> Result<ExitCode> {
    let mut stdin_bytes = Vec::new();
    std::io::stdin().read_to_end(&mut stdin_bytes)?;

    let repo = match gix::discover(".") {
        Ok(repo) => repo,
        Err(e) => return Ok(fatal(&format!("not in a git repository: {e}"))),
    };
    // git consults this before rejecting a CR-bearing value, so read it first.
    let protect_protocol = repo
        .config_snapshot()
        .boolean("credential.protectProtocol")
        .unwrap_or(true);

    let mut cred = Cred::default();
    if let Err(msg) = parse(&stdin_bytes, protect_protocol, &mut cred) {
        return Ok(fatal(&msg));
    }

    // git's `credential_fill` returns before applying any config when the
    // credential is already complete, so neither the helpers nor the http path
    // rule are reached — the description is echoed exactly as it came in.
    if matches!(op, Op::Fill) && cred.username.is_some() && cred.password.is_some() {
        if let Err(msg) = require_url_fields(&cred) {
            return Ok(fatal(&msg));
        }
        return emit(&cred);
    }
    // Likewise, `credential_approve` is a no-op for an incomplete credential.
    if matches!(op, Op::Approve) && (cred.username.is_none() || cred.password.is_none()) {
        return Ok(ExitCode::SUCCESS);
    }

    if let Err(msg) = require_url_fields(&cred) {
        return Ok(fatal(&msg));
    }

    let lookup_url = cred_url(&cred);
    let url = match gix::url::parse(lookup_url.as_bstr()) {
        Ok(url) => url,
        Err(e) => return Ok(fatal(&format!("credential url cannot be parsed: {e}"))),
    };
    let (mut cascade, action, prompt) = match repo.config_snapshot().credential_helpers(url) {
        Ok(parts) => parts,
        Err(e) => return Ok(fatal(&format!("{e}"))),
    };

    // `credential_apply_config`: an http(s) path is not part of the credential
    // unless `credential.useHttpPath` says so. This governs both what helpers
    // are told and what `fill` prints.
    if !cascade.use_http_path && is_http(cred.protocol.as_ref()) {
        cred.path = None;
    }

    match op {
        Op::Fill => {
            // The cascade's own context carries the url with `credential.username`
            // already folded in; secrets come back through the next-action handle.
            let ctx = action.context().cloned().unwrap_or_default();
            let outcome = match cascade.invoke(Action::Get(ctx), prompt) {
                Ok(Some(outcome)) => outcome,
                Ok(None) => return Ok(fatal("no credential could be obtained")),
                Err(e) => return Ok(fatal(&format!("{e}"))),
            };
            let filled = match Context::try_from(&outcome.next) {
                Ok(ctx) => ctx,
                Err(e) => return Ok(fatal(&format!("{e}"))),
            };
            // Keep our own protocol/host/path — they are byte-exact copies of the
            // input, whereas the cascade's have been through url normalization.
            cred.username = filled.username.map(Into::into);
            cred.password = filled.password.map(Into::into);
            cred.oauth_refresh_token = filled.oauth_refresh_token.map(Into::into);
            cred.password_expiry_utc = filled
                .password_expiry_utc
                .map(|secs| secs.to_string().into());
            emit(&cred)
        }
        Op::Approve | Op::Reject => {
            // Encoded here rather than via `Context::write_to` so helpers receive
            // git's key order with no synthetic `url=` line.
            let payload = encode(&cred);
            let action = if matches!(op, Op::Approve) {
                Action::Store(payload)
            } else {
                Action::Erase(payload)
            };
            // Store/erase never report failure: git ignores a helper that cannot
            // record the outcome, and so does the cascade.
            match cascade.invoke(action, prompt) {
                Ok(_) => Ok(ExitCode::SUCCESS),
                Err(e) => Ok(fatal(&format!("{e}"))),
            }
        }
    }
}

/// git's `credential_apply_config` presence checks, in git's order.
fn require_url_fields(cred: &Cred) -> std::result::Result<(), String> {
    if cred.host.is_none() {
        return Err("refusing to work with credential missing host field".into());
    }
    if cred.protocol.is_none() {
        return Err("refusing to work with credential missing protocol field".into());
    }
    Ok(())
}

fn is_http(protocol: Option<&BString>) -> bool {
    protocol.is_some_and(|p| matches!(p.to_str(), Ok("http") | Ok("https")))
}

/// Read the `key=value` credential description, terminated by a blank line or
/// EOF. Returns the text of a `fatal:` message on a rejected input.
fn parse(
    input: &[u8],
    protect_protocol: bool,
    cred: &mut Cred,
) -> std::result::Result<(), String> {
    for raw in input.split(|&b| b == b'\n') {
        // `strbuf_getline` strips a trailing CR; an interior one is a protocol
        // smuggling attempt and is rejected below.
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if line.is_empty() {
            break;
        }
        let Some(eq) = line.iter().position(|&b| b == b'=') else {
            eprintln!("warning: invalid credential line: {}", line.as_bstr());
            return Err("unable to read credential from stdin".into());
        };
        let (key, value) = (&line[..eq], &line[eq + 1..]);
        let key = key.to_str().map_err(|_| {
            format!("invalid credential key: {}", key.as_bstr())
        })?;

        if value.contains(&0) {
            return Err(format!("credential value for {key} contains null byte"));
        }
        if protect_protocol && value.contains(&b'\r') {
            return Err(format!(
                "credential value for {key} contains carriage return\nIf this is intended, set `credential.protectProtocol=false`"
            ));
        }
        let value: BString = value.into();

        match key {
            "protocol" => cred.protocol = Some(value),
            "host" => cred.host = Some(value),
            "path" => cred.path = Some(value),
            "username" => cred.username = Some(value),
            "password" => cred.password = Some(value),
            "oauth_refresh_token" => cred.oauth_refresh_token = Some(value),
            "password_expiry_utc" => cred.password_expiry_utc = Some(value),
            "url" => apply_url(cred, value.as_bstr())?,
            // Recognised by git but with no representation in the vendored
            // credential context — erroring beats returning a wrong credential.
            "authtype" | "credential" | "ephemeral" | "continue" => {
                return Err(format!(
                    "the {key:?} credential attribute is not supported (needs authtype/state protocol support in gix-credentials)"
                ))
            }
            _ if key.ends_with("[]") => {
                return Err(format!(
                    "the multi-valued {key:?} credential attribute is not supported (needs authtype/state protocol support in gix-credentials)"
                ))
            }
            // git silently discards attributes it does not know, including `quit`
            // on input (it is meaningful only coming back from a helper).
            _ => {}
        }
    }
    Ok(())
}

/// Expand a `url=` attribute into its constituent fields, exactly as git's
/// `credential_from_url` does: every component is overwritten, including with
/// `None` when the url does not carry it.
fn apply_url(cred: &mut Cred, value: &BStr) -> std::result::Result<(), String> {
    if !value.contains_str("://") {
        return Err(format!("credential url cannot be parsed: {value}"));
    }
    let url = gix::url::parse(value)
        .map_err(|_| format!("credential url cannot be parsed: {value}"))?;

    cred.protocol = Some(url.scheme.as_str().into());
    // git keeps the port verbatim, including when it is the scheme default; a
    // url with no host at all yields an empty host attribute.
    cred.host = Some(match (url.host(), url.port) {
        (Some(h), Some(port)) => format!("{h}:{port}").into(),
        (Some(h), None) => h.into(),
        (None, _) => BString::default(),
    });
    cred.username = url.user().map(Into::into);
    cred.password = url.password().map(Into::into);
    let path = url.path.trim_with(|b| b == '/');
    cred.path = (!path.is_empty()).then(|| path.into());
    Ok(())
}

/// Rebuild a url from the credential fields, for helper/config lookup.
fn cred_url(cred: &Cred) -> BString {
    let mut url = BString::default();
    if let Some(protocol) = &cred.protocol {
        url.push_str(protocol);
    }
    url.push_str(b"://");
    if let Some(user) = &cred.username {
        url.push_str(user);
        url.push(b'@');
    }
    if let Some(host) = &cred.host {
        url.push_str(host);
    }
    if let Some(path) = &cred.path {
        if !path.starts_with_str("/") {
            url.push(b'/');
        }
        url.push_str(path);
    }
    url
}

/// Serialize the credential in git's `credential_write` field order.
fn encode(cred: &Cred) -> BString {
    let mut out = BString::default();
    let mut item = |key: &str, value: &BString| {
        out.push_str(key);
        out.push(b'=');
        out.push_str(value);
        out.push(b'\n');
    };
    // protocol and host are mandatory here; presence was checked by the caller.
    if let Some(v) = &cred.protocol {
        item("protocol", v);
    }
    if let Some(v) = &cred.host {
        item("host", v);
    }
    for (key, value) in [
        ("path", &cred.path),
        ("username", &cred.username),
        ("password", &cred.password),
        ("oauth_refresh_token", &cred.oauth_refresh_token),
        ("password_expiry_utc", &cred.password_expiry_utc),
    ] {
        if let Some(v) = value {
            item(key, v);
        }
    }
    out
}

fn emit(cred: &Cred) -> Result<ExitCode> {
    let bytes = encode(cred);
    let mut out = std::io::stdout().lock();
    out.write_all(&bytes)?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Report a fatal error the way git does and yield git's fatal exit code.
fn fatal(message: &str) -> ExitCode {
    eprintln!("fatal: {message}");
    ExitCode::from(128)
}
