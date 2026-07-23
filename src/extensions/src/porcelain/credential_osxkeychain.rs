use anyhow::Result;
use std::io::BufRead;
use std::process::ExitCode;

/// `git credential-osxkeychain` — the macOS Keychain credential helper.
///
/// This module is a **skeleton**, not a port. The helper's entire purpose is
/// reading and writing internet-password items in the macOS login keychain via
/// Security.framework (`SecItemCopyMatching` / `SecItemAdd` / `SecItemDelete`,
/// historically `SecKeychainFindInternetPassword` and friends). That is an
/// Objective-C/CoreFoundation FFI surface, not a repository operation — nothing
/// in the vendored gitoxide crates touches it. `gix-credentials` knows the
/// string `"osxkeychain"` only as the *name* of an external helper program to
/// fork (`src/ported/gix-credentials/src/helper/cascade.rs:33`); it has no
/// keychain backend of its own, and the `zvcs` crate depends on `gix` and
/// `anyhow` alone.
///
/// What *is* implemented faithfully here is everything upstream does before it
/// reaches the keychain — the parts that are pure protocol and therefore
/// portable, with byte-identical stderr and exit codes:
///
///   * missing operand → `fatal: usage: git credential-osxkeychain
///     <get|store|erase>` on stderr, exit 128.
///   * the credential key/value input protocol on stdin: `key=value` lines
///     terminated by a blank line, a `host=<host>:<port>` split, and repeated
///     `capability[]` / `state[]` keys.
///   * a line with no `=` → `fatal: bad input: <line>` on stderr, exit 128,
///     raised mid-parse exactly where upstream raises it.
///   * a `protocol=` value outside {imap, imaps, ftp, ftps, http, https, smtp}
///     → silent success (exit 0), aborting the parse immediately, matching
///     upstream's `exit(0)` inside the read loop.
///   * an operation other than `get`/`store`/`erase` → input is consumed and
///     the operand ignored, exit 0.
///
/// `get`, `store` and `erase` themselves cannot be served without the keychain
/// substrate above, so they `bail!` rather than emit a plausible-looking empty
/// answer. That distinction matters: a silent `get` that returns nothing reads
/// to git as "no credential stored" and would make it re-prompt for a password
/// that is in fact on disk, or worse, let a `store` succeed while persisting
/// nothing.
pub fn credential_osxkeychain(args: &[String]) -> Result<ExitCode> {
    // args[0] is the subcommand name itself; the operation is the next word.
    let Some(op) = args.get(1) else {
        eprintln!("fatal: usage: git credential-osxkeychain <get|store|erase>");
        return Ok(ExitCode::from(128));
    };

    // Upstream reads the credential *before* dispatching on the operation, so
    // input errors win over an unknown operand. Preserve that ordering.
    match read_credential()? {
        Parsed::UnsupportedProtocol => return Ok(ExitCode::SUCCESS),
        Parsed::BadInput(line) => {
            eprintln!("fatal: bad input: {line}");
            return Ok(ExitCode::from(128));
        }
        Parsed::Credential(_) => {}
    }

    match op.as_str() {
        "get" | "store" | "erase" => anyhow::bail!(
            "{op} requires macOS Security.framework keychain access \
             (SecItemCopyMatching/SecItemAdd/SecItemDelete); no keychain substrate \
             exists in the vendored crates (ported: argument and stdin protocol handling only)"
        ),
        // Upstream ignores an unrecognized action outright and returns 0.
        _ => Ok(ExitCode::SUCCESS),
    }
}

/// A credential description as read off stdin. Fields are kept even though the
/// keychain calls that would consume them are unimplemented, so the parse stays
/// a faithful mirror of upstream's `read_credential` rather than a line-counter.
#[derive(Default)]
#[allow(dead_code)] // consumed only by the unimplemented keychain operations
struct Credential {
    protocol: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    path: Option<String>,
    username: Option<String>,
    password: Option<String>,
    password_expiry_utc: Option<String>,
    oauth_refresh_token: Option<String>,
    capabilities: Vec<String>,
    state: Vec<String>,
}

enum Parsed {
    Credential(Box<Credential>),
    /// A `protocol=` value the helper does not handle — upstream exits 0 here.
    UnsupportedProtocol,
    /// A line carrying no `=`; the payload is the line as upstream reports it.
    BadInput(String),
}

/// Read the `key=value` credential block from stdin, stopping at the first blank
/// line or EOF.
///
/// Mirrors upstream's loop, including the two early exits: an unhandled
/// `protocol` value aborts with success mid-stream, and a line without `=` is a
/// fatal parse error. Unknown keys are skipped silently, as upstream does.
fn read_credential() -> Result<Parsed> {
    let mut cred = Credential::default();
    let stdin = std::io::stdin();

    for line in stdin.lock().lines() {
        let line = line?;
        // A blank line terminates the block.
        if line.is_empty() {
            break;
        }
        // Upstream strips only the trailing newline, so a stray CR is data.
        let Some((key, value)) = line.split_once('=') else {
            return Ok(Parsed::BadInput(line));
        };

        match key {
            "protocol" => match value {
                "imap" | "imaps" | "ftp" | "ftps" | "http" | "https" | "smtp" => {
                    cred.protocol = Some(value.to_owned());
                }
                // Anything else (ssh, file, ...) is out of scope for the helper.
                _ => return Ok(Parsed::UnsupportedProtocol),
            },
            "host" => {
                // `host` may carry a `:<port>` suffix, which upstream splits off.
                // Its `atoi` yields 0 for a non-numeric tail rather than failing.
                match value.split_once(':') {
                    Some((h, port)) => {
                        cred.host = Some(h.to_owned());
                        cred.port = Some(port.parse().unwrap_or(0));
                    }
                    None => cred.host = Some(value.to_owned()),
                }
            }
            "path" => cred.path = Some(value.to_owned()),
            "username" => cred.username = Some(value.to_owned()),
            "password" => cred.password = Some(value.to_owned()),
            "password_expiry_utc" => cred.password_expiry_utc = Some(value.to_owned()),
            "oauth_refresh_token" => cred.oauth_refresh_token = Some(value.to_owned()),
            "capability[]" => cred.capabilities.push(value.to_owned()),
            "state[]" => cred.state.push(value.to_owned()),
            // Unrecognized keys are ignored, per the credential protocol.
            _ => {}
        }
    }

    Ok(Parsed::Credential(Box::new(cred)))
}
