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
