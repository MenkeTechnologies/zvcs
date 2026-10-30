//! git's own diagnostics for a transport that never got as far as talking.
//!
//! When the `ssh` git spawns fails — a refused key, a host that does not resolve,
//! a closed connection — git says nothing of its own about *why*. The child's
//! stderr has already gone to the terminal, and git adds one fixed block
//! (`connect.c`'s `git_connect()` → `die()` in `transport.c`):
//!
//! ```text
//! git@github.com: Permission denied (publickey).
//! fatal: Could not read from remote repository.
//!
//! Please make sure you have the correct access rights
//! and the repository exists.
//! ```
//!
//! and exits 128. The vendored transport lets *most* of that stderr through
//! live and captures the handful of lines it recognises into the error chain
//! instead; the captured ones are reprinted here, so the bytes and their order
//! are what a caller sees either way. Nothing else about the failure is
//! printed — a Rust `io::Error` for a stream that ended is not a diagnostic git
//! has, and must never reach the terminal. See [`ssh_fatal`].
//!
//! This is deliberately limited to `ssh`. git's HTTP diagnostics are different
//! text (`fatal: unable to access '<url>': <curl message>` and
//! `fatal: could not read Username for '<url>': terminal prompts disabled`), and
//! the curl wording is not something this transport can reproduce, so those are
//! left alone rather than half-matched.

use std::process::ExitCode;

/// git's fixed block for a failed `ssh` transport, or `None` when `url` is not an
/// ssh remote (in which case the caller reports the error its usual way).
///
/// git itself adds **only** the block. `git_connect()` (connect.c) gives the
/// `ssh` child the caller's stderr, so whatever the child said is already on the
/// terminal by the time the protocol read fails, and `die_initial_contact()`
/// (connect.c:81-93) contributes nothing about the read:
///
/// ```c
/// static NORETURN void die_initial_contact(int unexpected)
/// {
///         if (unexpected)
///                 die(_("The remote end hung up upon initial contact"));
///         else
///                 die(_("Could not read from remote repository.\n\n"
///                       "Please make sure you have the correct access rights\n"
///                       "and the repository exists."));
/// }
/// ```
///
/// The port has to work harder for the same bytes because the vendored transport
/// does not hand the child's stderr straight through. `supervise_stderr()`
/// (gix-transport/src/client/blocking_io/file.rs:314-339) reads it line by line
/// and splits the lines two ways:
///
/// * A line `ProgramKind::line_to_err()` (ssh/program_kind.rs:101-131) recognises
///   — `Permission denied`, `resolve hostname`, `connect to host`, `Connection
///   to `, `Connection closed by ` — is **swallowed** into an
///   `io::Error::new(kind, line)` and never written out. Those are the lines this
///   function has to reprint for the stream to match git's.
/// * Every other line is echoed to this process's stderr as it arrives, exactly
///   as git's inherited stderr would have carried it. Reprinting anything for
///   those cases would duplicate output git prints once.
///
/// So the reprint is keyed on *what kind of error it is*, not on where it sits in
/// the chain. [`captured_ssh_line`] answers that, and when it answers `None` this
/// prints the block alone — which is what git does for an ssh child that said
/// nothing at all, and for one whose words were already echoed.
///
/// The previous spelling took `err.chain().last()` unconditionally. That is the
/// swallowed line only when a swallowed line exists; otherwise it is whatever
/// `std` names the failed read, and `fatal: failed to fill whole buffer` (the
/// text of `read_exact`'s `UnexpectedEof`) reached the terminal ahead of the
/// block — a Rust diagnostic in a git transcript. Reproduced headlessly with a
/// `GIT_SSH_COMMAND` that writes `ERROR: Repository not found.` and exits, which
/// is the shape of a missing repository over ssh; see
/// `tests/ssh_transport_failure_block.rs`.
pub fn ssh_fatal(url: &str, err: &anyhow::Error) -> Option<ExitCode> {
    if !is_ssh(url) {
        return None;
    }
    // OpenSSH terminates its own diagnostics with CRLF — measured on both a pipe
    // and a file, since it writes them through its log routine rather than as
    // plain stderr text — and the vendored transport strips that when it splits
    // the child's stderr into lines. Restoring it is what makes the byte stream
    // here identical to the one git's inherited stderr carries. An ssh client that
    // does not use CRLF (`plink` and friends) would differ by that one byte.
    if let Some(text) = captured_ssh_line(err) {
        use std::io::Write;
        let mut err_out = std::io::stderr();
        let _ = write!(err_out, "{text}\r\n");
        let _ = err_out.flush();
    }
    eprintln!("fatal: Could not read from remote repository.");
    eprintln!();
    eprintln!("Please make sure you have the correct access rights");
    eprintln!("and the repository exists.");
    Some(ExitCode::from(128))
}

/// git's refusal when the other end is a local path that is not a repository.
///
/// `git_connect()` runs `enter_repo()` on a local path and dies when it comes
/// back NULL:
///
/// ```c
/// die(_("'%s' does not appear to be a git repository"), path);
/// ```
///
/// The caller — `transport_get_remote_refs()` — then adds the same four-line
/// block an unreachable ssh host produces, because both are "the other end never
/// answered". Exit 128.
pub fn not_a_repository_fatal(path: &str) -> ExitCode {
    eprintln!("fatal: '{path}' does not appear to be a git repository");
    eprintln!("fatal: Could not read from remote repository.");
    eprintln!();
    eprintln!("Please make sure you have the correct access rights");
    eprintln!("and the repository exists.");
    ExitCode::from(128)
}

/// [`not_a_repository_fatal`] for the vendored transport's own spelling of the
/// same failure, or `None` when `err` is something else.
///
/// Before it spawns anything for a local path, `gix` asks whether the path is a
/// git directory and fails with `connect::Error::FileUrl` — "Could not verify
/// that "<url>" url is a valid git directory before attempting to use it" —
/// where git's `git_connect()` would have spawned `upload-pack`/`receive-pack`
/// and died in `enter_repo()`. The URL carried in the error is the path as it
/// was configured or typed, which is the string git's message quotes.
pub fn file_url_fatal(err: &gix::remote::connect::Error) -> Option<ExitCode> {
    match err {
        gix::remote::connect::Error::FileUrl { url, .. } => {
            Some(not_a_repository_fatal(&url.to_bstring().to_string()))
        }
        _ => None,
    }
}

/// [`file_url_fatal`] for an error that carries the connect failure somewhere
/// in its source chain, as one propagated through `?` does.
pub fn file_url_fatal_in(err: &anyhow::Error) -> Option<ExitCode> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<gix::remote::connect::Error>())
        .and_then(file_url_fatal)
}

/// The one line the stderr supervisor swallowed, or `None` when it swallowed
/// none and the child's words already reached the terminal on their own.
///
/// `line_to_err()` builds its errors with `io::Error::new(kind, String)`, which
/// is `std`'s *custom* representation: `get_ref()` is `Some` and
/// `raw_os_error()` is `None`. Neither of the two errors that otherwise end up
/// in this chain can look like that —
///
/// | error | representation | `get_ref()` | `raw_os_error()` |
/// |---|---|---|---|
/// | `line_to_err()` line | `Custom` | `Some` | `None` |
/// | `read_exact` at EOF (`failed to fill whole buffer`) | `SimpleMessage` | `None` | `None` |
/// | anything from a syscall (`Device not configured (os error 6)`) | `Os` | `None` | `Some` |
///
/// — so `get_ref().is_some()` is the exact test, and the kind is narrowed to the
/// three `line_to_err()` can produce so an unrelated custom `io::Error` from
/// another layer cannot be mistaken for the child's words.
///
/// The whole chain is walked rather than just its end: the swallowed error is
/// the innermost one for the transports that reach here directly, but not for a
/// chain another layer has extended.
fn captured_ssh_line(err: &anyhow::Error) -> Option<String> {
    err.chain().find_map(|cause| {
        let io = cause.downcast_ref::<std::io::Error>()?;
        let from_line_to_err = io.get_ref().is_some()
            && matches!(
                io.kind(),
                std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::NotFound
            );
        if !from_line_to_err {
            return None;
        }
        let text = io.to_string();
        (!text.is_empty()).then_some(text)
    })
}

/// git's report for an `ERR <message>` packet line, or `None` when `err` is not
/// one.
///
/// `upload-pack` answers a request it cannot serve with a single `ERR` line —
/// `ERR upload-pack: not our ref <oid>` for a want it cannot reach — and git's
/// `pkt-line.c` turns that into
///
/// ```text
/// fatal: remote error: upload-pack: not our ref <oid>
/// ```
///
/// with exit 128 (`die()` under `PACKET_READ_DIE_ON_ERR_PACKET`). The message is
/// the server's, so it is printed verbatim after the fixed prefix.
pub fn remote_error_fatal(err: &anyhow::Error) -> Option<ExitCode> {
    let message = remote_error_message(err.as_ref())?;
    eprintln!("fatal: remote error: {message}");
    Some(ExitCode::from(128))
}

/// The server's message from an `ERR` line anywhere in `err`'s source chain.
///
/// Both spellings the vendored protocol produces are checked: the packetline
/// reader's own error (transports whose reader keeps `fail_on_err_lines`, i.e.
/// `git://` and `file://`) and the fetch-response parser's wrapper around it
/// (HTTP, whose reader starts fresh for every request).
fn remote_error_message(err: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut source = Some(err);
    while let Some(err) = source {
        if let Some(err) = err.downcast_ref::<gix::protocol::transport::packetline::read::Error>() {
            return Some(err.message.to_string());
        }
        if let Some(gix::protocol::fetch::response::Error::UploadPack(err)) =
            err.downcast_ref::<gix::protocol::fetch::response::Error>()
        {
            return Some(err.message.to_string());
        }
        source = err.source();
    }
    None
}

/// Whether `url` names an ssh remote — the `ssh://` scheme or the scp-like
/// `[user@]host:path` shorthand, both of which `gix_url` resolves to `Ssh`.
fn is_ssh(url: &str) -> bool {
    gix::url::parse(url.into()).is_ok_and(|u| u.scheme == gix::url::Scheme::Ssh)
}

/// `die_initial_contact(0)` (connect.c):
///
/// ```c
/// die(_("Could not read from remote repository.\n\n"
///       "Please make sure you have the correct access rights\n"
///       "and the repository exists."));
/// ```
///
/// The local transport reaches this whenever the program on the other end — the
/// one `--receive-pack`/`--upload-pack` named — produced no advertisement, and it
/// names neither the program nor the path: git has nothing to report but that the
/// conversation never started. Exit 128.
pub fn initial_contact_fatal() -> ExitCode {
    eprintln!("fatal: Could not read from remote repository.");
    eprintln!();
    eprintln!("Please make sure you have the correct access rights");
    eprintln!("and the repository exists.");
    ExitCode::from(128)
}

/// Whether `url` names a path on this machine, which is the transport
/// `git_connect()` serves by spawning the service program directly.
pub fn is_local(url: &str) -> bool {
    gix::url::parse(url.into()).is_ok_and(|u| u.scheme == gix::url::Scheme::File)
}

/// git's `die()` for an advertisement the other end cut short, or `None` when
/// `err` is something else: `die_initial_contact(0)` for a stream that closed
/// before its first packet line (connect.c:151-153) and `expected flush after
/// capabilities` for a v2 capability list without its flush
/// (connect.c:134-140). Exit 128.
pub fn hang_up_fatal(err: &anyhow::Error) -> Option<ExitCode> {
    let hang_up = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<gix::protocol::transport::client::HangUp>())?;
    eprintln!("fatal: {hang_up}");
    Some(ExitCode::from(128))
}

/// `discover_refs()`'s `HTTP_NOAUTH` case (remote-curl.c:524-527), or `None` when
/// `err` is not a refusal of the credentials the handshake retried with:
///
/// ```c
/// case HTTP_NOAUTH:
///         show_http_message(&type, &charset, &buffer);
///         die(_("Authentication failed for '%s'"),
///             transport_anonymize_url(url.buf));
/// ```
///
/// `handle_curl_result()` only returns `HTTP_NOAUTH` for a `401` to a request that
/// carried a username and password, after `credential_reject()` has erased them
/// (http.c:1945-1955); the vendored handshake has done the same erase by the time
/// it reports `InvalidCredentials`. `url` is the remote's URL as configured, which
/// remote-curl ends with a slash (`end_url_with_slash()`, remote-curl.c:1587-1589).
/// Exit 128.
pub fn http_auth_fatal(
    url: &str,
    repo: Option<&gix::Repository>,
    err: &gix::protocol::handshake::Error,
) -> Option<ExitCode> {
    let gix::protocol::handshake::Error::InvalidCredentials { source, .. } = err else {
        return None;
    };
    if let Some(response) = source
        .get_ref()
        .and_then(|e| e.downcast_ref::<gix::protocol::transport::client::blocking_io::http::ErrorResponse>())
    {
        show_http_message(response, repo);
    }
    let mut url = url.to_owned();
    if !url.is_empty() && !url.ends_with('/') {
        url.push('/');
    }
    eprintln!("fatal: Authentication failed for '{}'", transport_anonymize_url(&url));
    Some(ExitCode::from(128))
}

/// `show_http_message()` (remote-curl.c:370-395): a `text/plain` body is reencoded
/// from its charset to the log output encoding, trimmed, and printed one
/// `remote: ` line per line; any other type, or an empty body, prints nothing.
fn show_http_message(
    response: &gix::protocol::transport::client::blocking_io::http::ErrorResponse,
    repo: Option<&gix::Repository>,
) {
    let (kind, charset) = extract_content_type(response.content_type.as_deref().unwrap_or(""));
    if kind != "text/plain" {
        return;
    }
    let mut msg = response.body.clone();
    if !charset.is_empty() {
        let to = match repo {
            Some(repo) => crate::revfilter::log_output_encoding(repo, None),
            None => "UTF-8".to_owned(),
        };
        // `strbuf_reencode()` leaves the buffer alone when the conversion fails.
        if let Some(converted) = crate::porcelain::mailinfo::reencode(&msg, &charset, &to) {
            msg = converted;
        }
    }
    let msg = msg.trim_ascii();
    if msg.is_empty() {
        return;
    }
    for line in msg.split(|&b| b == b'\n') {
        let mut out = b"remote: ".to_vec();
        out.extend_from_slice(line);
        out.push(b'\n');
        use std::io::Write;
        std::io::stderr().write_all(&out).ok();
    }
}

/// `extract_content_type()` (http.c:2071-2101): the media type with whitespace
/// dropped and lower-cased up to the first `;`, then the `charset` parameter,
/// which defaults to `ISO-8859-1` for a `text/` type that names none.
fn extract_content_type(raw: &str) -> (String, String) {
    let raw = raw.as_bytes();
    let mut kind = String::new();
    let mut p = 0;
    while p < raw.len() {
        let c = raw[p];
        p += 1;
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b';' {
            break;
        }
        kind.push(c.to_ascii_lowercase() as char);
    }
    let mut charset = String::new();
    while p < raw.len() {
        while p < raw.len() && (raw[p].is_ascii_whitespace() || raw[p] == b';') {
            p += 1;
        }
        // `extract_param(p, "charset", charset)`
        let rest = &raw[p..];
        if rest.len() > 7 && rest[..7].eq_ignore_ascii_case(b"charset") && rest[7] == b'=' {
            charset = rest[8..]
                .iter()
                .take_while(|&&b| !b.is_ascii_whitespace() && b != b';')
                .map(|&b| b as char)
                .collect();
            return (kind, charset);
        }
        while p < raw.len() && !raw[p].is_ascii_whitespace() {
            p += 1;
        }
    }
    if charset.is_empty() && kind.starts_with("text/") {
        charset.push_str("ISO-8859-1");
    }
    (kind, charset)
}

/// `transport_anonymize_url()` (transport.c:1744-1784): drop the userinfo of a
/// `<scheme>://<userinfo>@<host>…` or `<userinfo>@<host>:<path>` URL; anything
/// else, a local path included, is returned unchanged.
pub fn transport_anonymize_url(url: &str) -> String {
    let Some(at) = url.find('@') else {
        return url.to_owned();
    };
    if url_is_local_not_ssh(url) {
        return url.to_owned();
    }
    let anon_part = &url[at + 1..];
    let prefix_len = match url.find("://") {
        None => {
            // cannot be "me@there:/path/name"
            if !anon_part.contains(':') {
                return url.to_owned();
            }
            0
        }
        Some(scheme_end) => {
            // make sure scheme is reasonable (RFC 1738 2.1)
            if !url[..scheme_end]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-'))
            {
                return url.to_owned();
            }
            // @ past the first slash does not count
            if url[scheme_end + 3..].find('/').is_some_and(|slash| scheme_end + 3 + slash < at) {
                return url.to_owned();
            }
            scheme_end + 3
        }
    };
    format!("{}{}", &url[..prefix_len], anon_part)
}

/// `url_is_local_not_ssh()` (url.c:136-142) off Windows, where
/// `has_dos_drive_prefix()` is always false.
fn url_is_local_not_ssh(url: &str) -> bool {
    let colon = url.find(':');
    let slash = url.find('/');
    match colon {
        None => true,
        Some(colon) => slash.is_some_and(|slash| slash < colon),
    }
}
