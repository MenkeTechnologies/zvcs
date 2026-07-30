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
//! and exits 128. The vendored transport captures that stderr into the error
//! chain instead of letting it through live, so it is reprinted here — the bytes
//! and their order are what a caller sees either way.
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
/// The first line is the innermost error in the chain, which for both an auth
/// refusal and a connect failure is the text the `ssh` child wrote.
pub fn ssh_fatal(url: &str, err: &anyhow::Error) -> Option<ExitCode> {
    if !is_ssh(url) {
        return None;
    }
    // `err.chain()` ends at the `std::io::Error` holding the child's stderr.
    //
    // OpenSSH terminates its own diagnostics with CRLF — measured on both a pipe
    // and a file, since it writes them through its log routine rather than as
    // plain stderr text — and the vendored transport strips that when it splits
    // the child's stderr into lines. Restoring it is what makes the byte stream
    // here identical to the one git's inherited stderr carries. An ssh client that
    // does not use CRLF (`plink` and friends) would differ by that one byte.
    if let Some(cause) = err.chain().last() {
        let text = cause.to_string();
        if !text.is_empty() {
            use std::io::Write;
            let mut err_out = std::io::stderr();
            let _ = write!(err_out, "{text}\r\n");
            let _ = err_out.flush();
        }
    }
    eprintln!("fatal: Could not read from remote repository.");
    eprintln!();
    eprintln!("Please make sure you have the correct access rights");
    eprintln!("and the repository exists.");
    Some(ExitCode::from(128))
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
