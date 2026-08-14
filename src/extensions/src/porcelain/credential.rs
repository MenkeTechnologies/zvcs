//! `git credential` — the scriptable front end to git's credential-helper
//! protocol, backed by the vendored `gix-credentials` cascade.
//!
//! Supported actions, with stdout byte-identical to stock git:
//! ```text
//!   * `git credential fill`       — read a credential description on stdin,
//!                                    consult the configured helpers (and, if
//!                                    they come up short, prompt), then print
//!                                    the completed description.
//!   * `git credential approve`    — send `store` to every configured helper.
//!   * `git credential reject`     — send `erase` to every configured helper.
//!   * `git credential capability` — print the fixed capability announcement.
//! ```
//!
//! Helper discovery, `credential.<url>.*` subsection matching,
//! `credential.username`, `credential.useHttpPath` and
//! `credential.protectProtocol` come from
//! [`gix::config::Snapshot::credential_helpers`], i.e. the same config engine
//! git uses; outside a repository the identical cascade is built from the
//! system/global configuration by [`standalone_credential_helpers`], because
//! `git credential` is one of git's `RUN_SETUP_GENTLY` commands.
//!
//! Prompting is *not* delegated to the cascade: git's own
//! `credential_getpass()`/`credential_ask_one()` are ported here so the prompt
//! reads exactly as git words it and both switches that govern it are honored —
//! `credential.interactive` (`false`/`never` turns a prompt into
//! `fatal: unable to get password from user`) and `credential.sanitizePrompt`
//! (on by default; percent-encodes the url in the prompt so a hostile host or
//! user name cannot forge one).
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
//! ```text
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
//!   * A helper that returns a password but no user name leaves the cascade
//!     without a complete identity, and gitoxide hands back only a redacted copy
//!     of what it collected. That password cannot be recovered, so it is asked
//!     for again rather than passed on as the literal `<redacted>`.
//! ```

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
    // Dispatch strips the verb; every element here is a real argument.
    let rest = args;
    // `show_usage_if_asked(argc, argv, usage_msg)` runs before the arity check and
    // fires only for a lone `-h` or `--help-all`. Unlike `usage()` below, it
    // prints to *stdout*.
    if rest.len() == 1 && matches!(rest[0].as_str(), "-h" | "--help-all") {
        println!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
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

    // `git credential` is `RUN_SETUP_GENTLY`: outside a repository it still runs,
    // against whatever configuration `git config` would see there.
    let repo = gix::discover(".").ok();
    let cfg: gix::config::File = match &repo {
        Some(repo) => repo.config_snapshot().plumbing().clone(),
        None => crate::config::global_config(),
    };
    // git consults this before rejecting a CR-bearing value, so read it first.
    let protect_protocol = cfg
        .boolean("credential.protectProtocol")
        .ok()
        .flatten()
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
    let (mut cascade, action, prompt) = match &repo {
        Some(repo) => match repo.config_snapshot().credential_helpers(url) {
            Ok(parts) => parts,
            Err(e) => return Ok(fatal(&format!("{e}"))),
        },
        None => match standalone_credential_helpers(&cfg, url) {
            Ok(parts) => parts,
            Err(e) => return Ok(fatal(&format!("{e}"))),
        },
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
            // git runs every helper first and only then prompts
            // (`credential_fill` → `credential_getpass`), so the cascade's own
            // prompting — which gix words differently and gates on neither
            // `credential.interactive` nor `credential.sanitizePrompt` — is
            // switched off and reproduced below.
            let helpers_only = gix::prompt::Options {
                mode: gix::prompt::Mode::Disable,
                askpass: prompt.askpass.clone(),
            };
            let partial = match cascade.invoke(Action::Get(ctx), helpers_only) {
                Ok(Some(outcome)) => match Context::try_from(&outcome.next) {
                    Ok(ctx) => ctx,
                    Err(e) => return Ok(fatal(&format!("{e}"))),
                },
                // No helper produced a complete identity. The error still carries
                // what they did produce, with any secret replaced by `<redacted>`
                // — which is unusable, so such a password is asked for again
                // rather than handed on as if it were the real one.
                Err(gix::credentials::protocol::Error::IdentityMissing { context }) => context,
                Ok(None) => return Ok(fatal("no credential could be obtained")),
                Err(e) => return Ok(fatal(&format!("{e}"))),
            };
            // Keep our own protocol/host/path — they are byte-exact copies of the
            // input, whereas the cascade's have been through url normalization.
            cred.username = partial.username.map(Into::into);
            cred.password = partial
                .password
                .filter(|p| p != "<redacted>")
                .map(Into::into);
            cred.oauth_refresh_token = partial
                .oauth_refresh_token
                .filter(|t| t != "<redacted>")
                .map(Into::into);
            cred.password_expiry_utc = partial
                .password_expiry_utc
                .map(|secs| secs.to_string().into());

            if cred.username.is_none() || cred.password.is_none() {
                match credential_getpass(&cfg, &mut cred, &prompt) {
                    Ok(()) => {}
                    Err(msg) => return Ok(fatal(&msg)),
                }
                if cred.username.is_none() && cred.password.is_none() {
                    return Ok(fatal("unable to get password from user"));
                }
            }
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

/// The helper cascade for a `git credential` run that found no repository —
/// [`gix::config::Snapshot::credential_helpers`] needs one, but git's
/// `credential_apply_config` only ever reads configuration, so system and
/// per-user files (plus `GIT_CONFIG_*` overrides) are enough to build the same
/// cascade.
///
/// Matches gix's in-repository routine key for key: `credential.helper` (an
/// empty value clears everything collected so far), `credential.username`,
/// `credential.useHttpPath` and `credential.protectProtocol`, taken both from
/// the bare `[credential]` section and from every `[credential "<url>"]`
/// subsection whose scheme, host and port match the url being filled.
fn standalone_credential_helpers(
    cfg: &gix::config::File,
    url: gix::Url,
) -> std::result::Result<
    (
        gix::credentials::helper::Cascade,
        Action,
        gix::prompt::Options,
    ),
    String,
> {
    use gix::bstr::ByteSlice;

    let mut programs = Vec::new();
    let mut context_options = gix::credentials::protocol::ContextOptions::default();
    let mut use_http_path = false;
    let mut url = url;
    let url_had_user_initially = url.user().is_some();

    if let Some(sections) = cfg.sections_by_name("credential") {
        for section in sections {
            // A subsection is a url pattern; it only contributes when it matches.
            if let Some(pattern) = section.header().subsection_name() {
                if !url_pattern_matches(pattern, &url) {
                    continue;
                }
            }
            for value in section.values("helper") {
                if value.trim().is_empty() {
                    programs.clear();
                } else {
                    programs.push(gix::credentials::Program::from_custom_definition(value.to_owned()));
                }
            }
            if !url_had_user_initially {
                if let Some(user) = section
                    .value("username")
                    .filter(|n| !n.trim().is_empty())
                    .and_then(|n| n.to_str().map(str::to_owned).ok())
                {
                    url.set_user(Some(user));
                }
            }
            if let Some(v) = section.value("useHttpPath") {
                use_http_path = gix::config::Boolean::try_from(v.as_bstr())
                    .map_err(|e| format!("credential.useHttpPath: {e}"))?
                    .0;
            }
            if let Some(v) = section.value("protectProtocol") {
                context_options.protect_protocol = gix::config::Boolean::try_from(v.as_bstr())
                    .map_err(|e| format!("credential.protectProtocol: {e}"))?
                    .0;
            }
        }
    }

    // `git_prompt`'s askpass lookup: `GIT_ASKPASS` wins over `core.askpass`,
    // which wins over `SSH_ASKPASS`.
    let prompt = gix::prompt::Options {
        askpass: cfg
            .path("core.askpass")
            .and_then(|p| p.interpolate(Default::default()).ok()),
        mode: gix::prompt::Mode::default(),
    }
    .apply_environment(true, true, false);

    let action = Action::Get(gix::credentials::protocol::Context::from_url(
        url.to_bstring(),
        context_options,
    ));
    let query_user_only = url.scheme == gix::url::Scheme::Ssh;
    Ok((
        gix::credentials::helper::Cascade {
            programs,
            use_http_path,
            context_options,
            query_user_only,
            stderr: true,
        },
        action,
        prompt,
    ))
}

/// gix's `credential_helpers` subsection test, reduced to what a `[credential
/// "<url>"]` header can express: same scheme, host (with `*` wildcards per
/// label), port, and — for non-root http(s) patterns — the same path.
fn url_pattern_matches(pattern: &gix::bstr::BStr, url: &gix::Url) -> bool {
    let Ok(pattern) = gix::url::parse(pattern) else {
        return false;
    };
    let is_http = matches!(
        pattern.scheme,
        gix::url::Scheme::Https | gix::url::Scheme::Http
    );
    if pattern.scheme != url.scheme {
        return false;
    }
    let ports = if is_http {
        (pattern.port_or_default(), url.port_or_default())
    } else {
        (pattern.port, url.port)
    };
    if ports.0 != ports.1 {
        return false;
    }
    if !(is_http && pattern.path_is_root()) && pattern.path != url.path {
        return false;
    }
    if pattern.user().is_some() && pattern.user() != url.user() {
        return false;
    }
    match (pattern.host(), url.host()) {
        (Some(p), Some(h)) => {
            let (lhs, rhs) = (p.split('.'), h.split('.'));
            lhs.clone().count() == rhs.clone().count()
                && lhs.zip(rhs).all(|(pat, value)| {
                    gix::glob::wildmatch(pat.into(), value.into(), gix::glob::wildmatch::Mode::empty())
                })
        }
        (None, None) => true,
        _ => false,
    }
}

/// Port of `credential_getpass()` (credential.c): ask for whatever the helpers
/// did not supply.
///
/// `credential.interactive` is git's off switch — `false`, or the string `never`,
/// makes this a hard failure instead of a prompt, which is how a non-interactive
/// job stops git from blocking on a terminal it does not have. Anything else
/// (including `auto`) prompts.
///
/// Returns the text of the `fatal:` message on failure.
fn credential_getpass(
    cfg: &gix::config::File,
    cred: &mut Cred,
    prompt: &gix::prompt::Options,
) -> std::result::Result<(), String> {
    // `repo_config_get_maybe_bool` first, then the literal `never`: an explicit
    // false and the `never` spelling both mean "do not ask". A non-boolean value
    // (`never`, `auto`) makes the boolean read come back empty, which is why both
    // reads are needed.
    let off = cfg.boolean("credential.interactive").ok().flatten() == Some(false)
        || cfg.string("credential.interactive").is_some_and(|v| v == "never");
    if off {
        return Err("unable to get password from user".into());
    }

    // `c->sanitize_prompt` defaults to 1 (`CREDENTIAL_INIT`).
    let sanitize = cfg
        .boolean("credential.sanitizePrompt")
        .ok()
        .flatten()
        .unwrap_or(true);

    if cred.username.is_none() {
        cred.username = Some(ask_one("Username", cred, sanitize, prompt)?.into());
    }
    if cred.password.is_none() {
        cred.password = Some(ask_one("Password", cred, sanitize, prompt)?.into());
    }
    Ok(())
}

/// Port of `credential_ask_one()`: `"<what> for '<description>': "`, or plain
/// `"<what>: "` when there is not even a protocol to describe.
fn ask_one(
    what: &str,
    cred: &Cred,
    sanitize: bool,
    prompt: &gix::prompt::Options,
) -> std::result::Result<String, String> {
    let desc = if sanitize {
        credential_format(cred)
    } else {
        credential_describe(cred)
    };
    let message = if desc.is_empty() {
        format!("{what}: ")
    } else {
        format!("{what} for '{desc}': ")
    };
    // `PROMPT_ECHO` for the username, hidden for the password; either way an
    // askpass program takes precedence, which is what `PROMPT_ASKPASS` means.
    let opts = gix::prompt::Options {
        mode: if what == "Password" {
            gix::prompt::Mode::Hidden
        } else {
            gix::prompt::Mode::Visible
        },
        askpass: prompt.askpass.clone(),
    };
    gix::prompt::ask(&message, &opts).map_err(|e| format!("could not prompt for {what}: {e}"))
}

/// `credential_describe()`: the credential as a url, verbatim.
fn credential_describe(cred: &Cred) -> String {
    build_description(cred, |part, _| String::from_utf8_lossy(part).into_owned())
}

/// `credential_format()`: the same url with every component percent-encoded, so
/// a host or user name carrying url punctuation cannot forge a different-looking
/// prompt. This is what `credential.sanitizePrompt` (default on) selects.
fn credential_format(cred: &Cred) -> String {
    build_description(cred, |part, field| {
        percent_encode(
            part,
            match field {
                Field::Username => EncodeFlags::SLASH,
                Field::Host => EncodeFlags::HOST_AND_PORT,
                Field::Path => EncodeFlags::NONE,
            },
        )
    })
}

/// Which credential component is being rendered, since each gets its own
/// `strbuf_add_percentencode` flags.
#[derive(Clone, Copy)]
enum Field {
    Username,
    Host,
    Path,
}

/// The shared shape of `credential_describe`/`credential_format`: nothing at all
/// without a protocol, then `<protocol>://[<user>@]<host>[/<path>]`. An empty
/// user name is skipped, as `*c->username` requires.
fn build_description(cred: &Cred, render: impl Fn(&[u8], Field) -> String) -> String {
    let Some(protocol) = &cred.protocol else {
        return String::new();
    };
    let mut out = format!("{}://", protocol);
    if let Some(user) = cred.username.as_ref().filter(|u| !u.is_empty()) {
        out.push_str(&render(user, Field::Username));
        out.push('@');
    }
    if let Some(host) = &cred.host {
        out.push_str(&render(host, Field::Host));
    }
    if let Some(path) = &cred.path {
        out.push('/');
        out.push_str(&render(path, Field::Path));
    }
    out
}

/// The `flags` of `strbuf_add_percentencode` (strbuf.h).
struct EncodeFlags;
impl EncodeFlags {
    const NONE: u8 = 0;
    const SLASH: u8 = 1;
    const HOST_AND_PORT: u8 = 2;
}

/// Port of `strbuf_add_percentencode()` (strbuf.c): `%XX` for every control
/// byte, every non-ASCII byte, and — depending on `flags` — the url punctuation
/// that would otherwise be read as structure.
fn percent_encode(src: &[u8], flags: u8) -> String {
    /// `URL_UNSAFE_CHARS` (strbuf.c).
    const URL_UNSAFE_CHARS: &[u8] = b" <>\"%{}|\\^`:?#[]@!$&'()*+,;=";
    let mut out = String::with_capacity(src.len());
    for &ch in src {
        let host_and_port = flags & EncodeFlags::HOST_AND_PORT != 0;
        let unsafe_here = if host_and_port {
            !ch.is_ascii_alphanumeric() && !b"-.:[]".contains(&ch)
        } else {
            URL_UNSAFE_CHARS.contains(&ch)
        };
        if ch <= 0x1F
            || ch >= 0x7F
            || (ch == b'/' && flags & EncodeFlags::SLASH != 0)
            || unsafe_here
        {
            out.push_str(&format!("%{ch:02X}"));
        } else {
            out.push(ch as char);
        }
    }
    out
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
