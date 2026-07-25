use anyhow::Result;
use std::io::BufRead;
use std::process::ExitCode;

/// `git credential-osxkeychain` — the macOS Keychain credential helper.
///
/// A faithful port of git's
/// `contrib/credential/osxkeychain/git-credential-osxkeychain.c`. The helper
/// reads a credential description off stdin (the `key=value` protocol) and then,
/// on the operation named by `argv[1]`:
///
///   * `get`   → `SecKeychainFindInternetPassword` for the {protocol, host,
///     port, path, username} tuple; on a hit it writes `password=<data>` and, if
///     the caller supplied no username, the stored account via
///     `SecKeychainItemCopyContent` (`kSecAccountItemAttr`) as `username=<data>`,
///     followed by upstream's `capability[]=state` / `state[]=osxkeychain:seen=…`
///     pair. A miss is silent success — git reads that as "nothing stored".
///   * `store` → `SecKeychainAddInternetPassword` for a complete credential
///     (protocol, host, username and password all present), matching upstream's
///     guard; on `errSecDuplicateItem` it overwrites the existing item's password
///     via `SecKeychainItemModifyAttributesAndData`. (git's contrib helper was
///     add-only and silently kept the old secret on a re-store — the daily-driver
///     bug where every later `get` re-prompts; modern git updates, and so do we.)
///   * `erase` → `SecKeychainFindInternetPassword` then `SecKeychainItemDelete`,
///     gated on at least a protocol and host, exactly as upstream requires.
///
/// Two write-suppression rules keep `store` from touching the keychain on every
/// authenticated operation. A keychain *write* is a separate authorization from a
/// *read*, so an item created by another program (`/usr/bin/git`'s own helper,
/// say) raises the "wants to make changes to your keychain" dialog on every
/// single push/fetch when the helper unconditionally rewrites it:
///
///   * upstream's state capability — `get` reports the credential it handed out
///     as `state[]=osxkeychain:seen=…`, and a `store` carrying a matching state is
///     a no-op, because the caller is only echoing back what came out of the
///     keychain.
///   * a compare-before-write on `errSecDuplicateItem`: the stored secret is read
///     back and the modify is skipped when it is byte-identical. `gix-credentials`
///     implements no part of git's `capability[]`/`state[]` protocol, so it never
///     round-trips the state line and upstream's rule alone never fires under
///     zvcs's own transports; this second rule covers that path. A rotated or
///     re-authenticated credential still differs, so it still lands.
///
/// `password_expiry_utc` and `oauth_refresh_token` are appended to the stored
/// secret as extra `\n`-separated `key=value` lines, exactly as upstream does —
/// `get` writes the whole blob back out raw, so those lines re-enter the caller's
/// credential as their own protocol lines and an expiring OAuth token is refreshed
/// on schedule instead of only after it starts failing.
///
/// Like upstream's `main`, the helper takes an exclusive `flock` on its own binary
/// for the duration of the run, so concurrent invocations cannot race each other
/// into duplicate keychain items.
///
/// The Security.framework symbols come from `security-framework-sys` (already in
/// the gitoxide TLS tree); the one omitted symbol, `SecKeychainItemCopyContent`,
/// is declared locally. All of it is gated on `cfg(target_os = "macos")`: git
/// only ever invokes *this* helper on macOS, so on any other target `get` /
/// `store` / `erase` are compiled to silent no-ops (exit 0) and the file still
/// builds for the Linux/x86_64 and Linux/aarch64 cross targets.
///
/// The pure-protocol front matter is byte-identical to upstream regardless of
/// platform:
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
pub fn credential_osxkeychain(args: &[String]) -> Result<ExitCode> {
    // Dispatch strips the verb, so `args[0]` is the operation (git's `argv[1]`);
    // its absence is the usage error, matching upstream's `if (!argv[1])`.
    let Some(op) = args.first() else {
        eprintln!("fatal: usage: git credential-osxkeychain <get|store|erase>");
        return Ok(ExitCode::from(128));
    };

    // Upstream locks its own binary before reading anything, serializing
    // concurrent helper runs against each other so two of them cannot both miss
    // the keychain and then both add the same item.
    let _lock = match lock_self() {
        Ok(lock) => lock,
        Err(path) => {
            eprintln!("fatal: failed to lock {path}");
            return Ok(ExitCode::from(128));
        }
    };

    // Upstream reads the credential *before* dispatching on the operation, so
    // input errors win over an unknown operand. Preserve that ordering.
    let cred = match read_credential()? {
        Parsed::UnsupportedProtocol => return Ok(ExitCode::SUCCESS),
        Parsed::BadInput(line) => {
            eprintln!("fatal: bad input: {line}");
            return Ok(ExitCode::from(128));
        }
        Parsed::Credential(cred) => cred,
    };

    match op.as_str() {
        "get" => keychain::find_internet_password(&cred),
        "store" => keychain::add_internet_password(&cred),
        "erase" => keychain::delete_internet_password(&cred),
        // Upstream ignores an unrecognized action outright and returns 0.
        _ => {}
    }
    Ok(ExitCode::SUCCESS)
}

/// Port of upstream `main`'s `open(argv[0], O_RDONLY | O_EXLOCK)`: hold an
/// exclusive advisory lock on the helper's own binary for the whole run, so two
/// concurrent invocations cannot both miss the keychain and then both add the
/// same item. The handle must stay alive until the operation finishes; dropping
/// it closes the descriptor and releases the lock, as process exit does upstream.
/// `Err` carries the path for the fatal message.
#[cfg(target_os = "macos")]
fn lock_self() -> Result<Option<std::fs::File>, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let exe = std::env::current_exe().map_err(|_| String::from("<self>"))?;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_EXLOCK)
        .open(&exe)
        .map(Some)
        .map_err(|_| exe.display().to_string())
}

/// No lock off macOS: the keychain operations are no-ops there, and `O_EXLOCK`
/// is a BSD extension Linux does not have.
#[cfg(not(target_os = "macos"))]
fn lock_self() -> Result<Option<std::fs::File>, String> {
    Ok(None)
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

/// The prefix upstream's `read_credential` requires on a `state[]` value before
/// it belongs to this helper; other helpers in the chain get their own namespace.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const STATE_PREFIX: &str = "osxkeychain:seen=";

/// Port of upstream's `encode_state_seen`: serialize the credential the helper is
/// looking at into the opaque state token git round-trips between `get` and
/// `store`. Absent fields are omitted, each present one prefixed `__<key>=`,
/// exactly as `write_item_strbuf` writes them.
///
/// `password` is the secret as it appears on the wire — the whole stored blob on
/// `get`, the bare `password=` value on `store`, matching upstream's use of its
/// global in both paths. It is bytes rather than a `String` because upstream
/// copies the raw keychain data, which need not be UTF-8.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn encode_state_seen(cred: &Credential, password: &[u8]) -> Vec<u8> {
    let mut state = Vec::from(STATE_PREFIX);
    let mut add = |key: &str, value: &[u8]| {
        state.extend_from_slice(format!("__{key}=").as_bytes());
        state.extend_from_slice(value);
    };
    if let Some(host) = &cred.host {
        add("host", host.as_bytes());
    }
    if let Some(port) = cred.port {
        // Upstream round-trips the port through a `kCFNumberShortType` and prints
        // it with `%d`, so it is a *signed* short on the wire.
        add("port", (port as i16).to_string().as_bytes());
    }
    if let Some(path) = &cred.path {
        add("path", path.as_bytes());
    }
    if let Some(username) = &cred.username {
        add("username", username.as_bytes());
    }
    // `write_item_strbuf_cfdata` drops the item when the data is empty or starts
    // with a NUL, since it measures with `strlen` before appending `CFDataGetLength`.
    if !password.is_empty() && password[0] != 0 {
        add("password", password);
    }
    state
}

/// The secret as stored in the keychain: the password with `password_expiry_utc`
/// and `oauth_refresh_token` appended as extra `\n`-separated `key=value` lines.
///
/// Upstream writes this blob out verbatim on `get`, where the embedded newlines
/// make each appended field its own line of the credential protocol — which is
/// how an expiry and a refresh token survive a round trip through an item that
/// only has one data field.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn stored_secret(cred: &Credential, password: &str) -> Vec<u8> {
    let mut data = Vec::from(password.as_bytes());
    if let Some(expiry) = &cred.password_expiry_utc {
        data.extend_from_slice(b"\npassword_expiry_utc=");
        data.extend_from_slice(expiry.as_bytes());
    }
    if let Some(token) = &cred.oauth_refresh_token {
        data.extend_from_slice(b"\noauth_refresh_token=");
        data.extend_from_slice(token.as_bytes());
    }
    data
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

/// The macOS Keychain operations, ported from git's osxkeychain helper.
///
/// git only invokes this helper on macOS, so on every other target the three
/// operations are silent no-ops — enough to keep the shadow `git` binary
/// cross-compiling for Linux/x86_64 and Linux/aarch64 without a keychain.
#[cfg(not(target_os = "macos"))]
mod keychain {
    use super::Credential;

    pub(super) fn find_internet_password(_cred: &Credential) {}
    pub(super) fn add_internet_password(_cred: &Credential) {}
    pub(super) fn delete_internet_password(_cred: &Credential) {}
}

/// The macOS Keychain operations, a 1:1 port of
/// `git-credential-osxkeychain.c`'s `find_internet_password`,
/// `add_internet_password` and `delete_internet_password` over the legacy
/// Security.framework internet-password API.
#[cfg(target_os = "macos")]
mod keychain {
    use super::Credential;
    use security_framework_sys::base::{
        SecKeychainAttribute, SecKeychainAttributeList, SecKeychainItemRef,
    };
    use security_framework_sys::keychain::{
        SecAuthenticationType, SecKeychainAddInternetPassword, SecKeychainFindInternetPassword,
        SecProtocolType,
    };
    use security_framework_sys::keychain_item::{
        SecKeychainItemDelete, SecKeychainItemFreeContent, SecKeychainItemModifyAttributesAndData,
    };
    use std::io::Write;
    use std::os::raw::{c_char, c_void};
    use std::ptr;

    // `security-framework-sys` binds the write side of the legacy attribute API
    // (`SecKeychainItemFreeContent`) but not the read side. Declare the one
    // missing symbol; it resolves against the same Security.framework the crate
    // already links. `OSStatus` is `i32`, spelled directly to avoid pulling in
    // `core_foundation_sys` by name.
    extern "C" {
        fn SecKeychainItemCopyContent(
            item_ref: SecKeychainItemRef,
            item_class: *mut u32,
            attr_list: *mut SecKeychainAttributeList,
            length: *mut u32,
            out_data: *mut *mut c_void,
        ) -> i32;
    }

    /// `kSecAccountItemAttr`, the four-char-code `'acct'`, selecting the stored
    /// account (username) attribute in `SecKeychainItemCopyContent`.
    const K_SEC_ACCOUNT_ITEM_ATTR: u32 = 0x6163_6374;

    /// `errSecDuplicateItem` — `SecKeychainAddInternetPassword` returns this when
    /// an item for the same {protocol, host, account, path, port} already exists.
    const ERR_SEC_DUPLICATE_ITEM: i32 = -25299;

    /// Upstream's `KEYCHAIN_ITEM(x)` macro: `(len, ptr)` for a present string,
    /// `(0, NULL)` for an absent one. The bytes are borrowed from `cred`, so the
    /// pointer stays valid for the duration of the enclosing FFI call.
    fn item(s: &Option<String>) -> (u32, *const c_char) {
        match s {
            Some(v) => (v.len() as u32, v.as_ptr() as *const c_char),
            None => (0, ptr::null()),
        }
    }

    /// Map the credential's protocol string to its `SecProtocolType`, mirroring
    /// upstream's `read_credential` switch. `read_credential` already rejects any
    /// value outside this set (exiting 0), so `None` only occurs when git sent no
    /// `protocol` line at all — which it never does for get/store/erase.
    fn protocol(cred: &Credential) -> Option<SecProtocolType> {
        Some(match cred.protocol.as_deref()? {
            "imap" => SecProtocolType::IMAP,
            "imaps" => SecProtocolType::IMAPS,
            "ftp" => SecProtocolType::FTP,
            "ftps" => SecProtocolType::FTPS,
            "https" => SecProtocolType::HTTPS,
            "http" => SecProtocolType::HTTP,
            "smtp" => SecProtocolType::SMTP,
            _ => return None,
        })
    }

    /// Write one `key=value` line to stdout, byte-for-byte like upstream's
    /// `write_item` (`fwrite` of the raw value, which may not be valid UTF-8).
    fn write_item(what: &str, buf: &[u8]) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(what.as_bytes());
        let _ = out.write_all(b"=");
        let _ = out.write_all(buf);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }

    /// `SecKeychainFindInternetPassword` over the {host, username, path, port,
    /// protocol} tuple, exactly like upstream's `KEYCHAIN_ARGS`. When
    /// `want_password` is false the password out-params are `NULL` (the item is
    /// located but its secret is not copied — the erase path).
    ///
    /// # Safety
    /// Calls into Security.framework; the borrowed string pointers from `item`
    /// outlive the call because `cred` is borrowed for the whole function.
    unsafe fn find(cred: &Credential, want_password: bool) -> Option<(Vec<u8>, SecKeychainItemRef)> {
        let proto = protocol(cred)?;
        let (host_len, host_ptr) = item(&cred.host);
        let (user_len, user_ptr) = item(&cred.username);
        let (path_len, path_ptr) = item(&cred.path);

        let mut item_ref: SecKeychainItemRef = ptr::null_mut();
        let mut len: u32 = 0;
        let mut buf: *mut c_void = ptr::null_mut();
        let (len_ptr, data_ptr) = if want_password {
            (&mut len as *mut u32, &mut buf as *mut *mut c_void)
        } else {
            (ptr::null_mut(), ptr::null_mut())
        };

        let status = SecKeychainFindInternetPassword(
            ptr::null(), // keychainOrArray: default keychain
            host_len,
            host_ptr,
            0,
            ptr::null(), // security domain
            user_len,
            user_ptr,
            path_len,
            path_ptr,
            cred.port.unwrap_or(0),
            proto,
            SecAuthenticationType::Default,
            len_ptr,
            data_ptr,
            &mut item_ref,
        );
        if status != 0 {
            return None;
        }

        let password = if want_password && !buf.is_null() {
            let pw = std::slice::from_raw_parts(buf as *const u8, len as usize).to_vec();
            // Free the password buffer; `attrList` NULL frees only the data,
            // exactly as upstream's `SecKeychainItemFreeContent(NULL, buf)`.
            SecKeychainItemFreeContent(ptr::null_mut(), buf);
            pw
        } else {
            Vec::new()
        };
        Some((password, item_ref))
    }

    /// Port of `find_username_in_item`: read the stored account attribute off a
    /// located item and emit it as `username=`.
    unsafe fn find_username_in_item(item_ref: SecKeychainItemRef) {
        let mut attr = SecKeychainAttribute {
            tag: K_SEC_ACCOUNT_ITEM_ATTR,
            length: 0,
            data: ptr::null_mut(),
        };
        let mut list = SecKeychainAttributeList {
            count: 1,
            attr: &mut attr,
        };
        if SecKeychainItemCopyContent(
            item_ref,
            ptr::null_mut(),
            &mut list,
            ptr::null_mut(),
            ptr::null_mut(),
        ) != 0
        {
            return;
        }
        let name = std::slice::from_raw_parts(attr.data as *const u8, attr.length as usize);
        write_item("username", name);
        SecKeychainItemFreeContent(&mut list, ptr::null_mut());
    }

    /// `get` — port of `find_internet_password`.
    pub(super) fn find_internet_password(cred: &Credential) {
        unsafe {
            let Some((password, item_ref)) = find(cred, true) else {
                return;
            };
            write_item("password", &password);
            if cred.username.is_none() {
                find_username_in_item(item_ref);
            }
            // Report what was handed out, so a `store` that merely echoes it back
            // can be recognized as redundant and skipped. Upstream emits both
            // lines unconditionally on a hit.
            write_item("capability[]", b"state");
            write_item("state[]", &super::encode_state_seen(cred, &password));
            // `item_ref` is a CoreFoundation object; the helper runs once per
            // process invocation and exits, so it is not released here — the same
            // as upstream's C, which also leaves it to process teardown.
        }
    }

    /// `store` — port of `add_internet_password`.
    pub(super) fn add_internet_password(cred: &Credential) {
        // Only store complete credentials (upstream's guard).
        let (Some(_), Some(_), Some(_), Some(password)) = (
            &cred.protocol,
            &cred.host,
            &cred.username,
            &cred.password,
        ) else {
            return;
        };
        let Some(proto) = protocol(cred) else {
            return;
        };

        let secret = super::stored_secret(cred, password);

        // Upstream's redundant-store guard: when the caller hands back the exact
        // credential this helper reported on the preceding `get`, there is nothing
        // to write. Writing anyway means a keychain *modify* authorization on every
        // authenticated operation, which is a fresh "wants to make changes" dialog
        // for any item another program created.
        let seen = cred
            .state
            .iter()
            .find(|state| state.starts_with(super::STATE_PREFIX));
        if let Some(seen) = seen {
            if seen.as_bytes() == super::encode_state_seen(cred, password.as_bytes()) {
                return;
            }
        }

        let (host_len, host_ptr) = item(&cred.host);
        let (user_len, user_ptr) = item(&cred.username);
        let (path_len, path_ptr) = item(&cred.path);
        unsafe {
            let status = SecKeychainAddInternetPassword(
                ptr::null_mut(), // default keychain
                host_len,
                host_ptr,
                0,
                ptr::null(), // security domain
                user_len,
                user_ptr,
                path_len,
                path_ptr,
                cred.port.unwrap_or(0),
                proto,
                SecAuthenticationType::Default,
                secret.len() as u32,
                secret.as_ptr() as *const c_void,
                ptr::null_mut(), // itemRef out — unused
            );

            // git's original contrib helper is add-only: it ignores the return, so
            // a store against an existing item silently keeps the OLD secret. That
            // breaks the daily driver — a re-authenticated or rotated credential
            // never lands, and every later `get` returns the stale value (or
            // nothing), so git keeps prompting. Modern git updates on duplicate;
            // match it by overwriting the existing item's password in place.
            if status == ERR_SEC_DUPLICATE_ITEM {
                let Some((existing_secret, existing)) = find(cred, true) else {
                    return;
                };
                // …but only when it differs. `gix-credentials` implements no part
                // of git's `capability[]`/`state[]` protocol, so the state guard
                // above never fires for zvcs's own transports; without this check
                // every fetch and push rewrites an unchanged item, re-raising the
                // keychain authorization dialog each time. A read is already
                // authorized here — `get` performs one on the same item.
                if existing_secret == secret {
                    return;
                }
                SecKeychainItemModifyAttributesAndData(
                    existing,
                    ptr::null(),
                    secret.len() as u32,
                    secret.as_ptr() as *const c_void,
                );
            }
        }
    }

    /// `erase` — port of `delete_internet_password`.
    pub(super) fn delete_internet_password(cred: &Credential) {
        // Require at least a protocol and host for removal (upstream's guard).
        if cred.protocol.is_none() || cred.host.is_none() {
            return;
        }
        unsafe {
            if let Some((_, item_ref)) = find(cred, false) {
                SecKeychainItemDelete(item_ref);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Credential, STATE_PREFIX, encode_state_seen, stored_secret};

    fn cred() -> Credential {
        Credential {
            protocol: Some("https".into()),
            host: Some("github.com".into()),
            username: Some("x-access-token".into()),
            password: Some("hunter2".into()),
            ..Credential::default()
        }
    }

    /// The state token is what tells a later `store` that it would be rewriting a
    /// credential this helper just handed out, so its exact bytes are the contract
    /// with git's `encode_state_seen`: `__key=value` runs, in host/port/path/
    /// username/password order, with absent fields skipped rather than emitted empty.
    #[test]
    fn state_token_matches_upstream_encoding() {
        let mut c = cred();
        assert_eq!(
            encode_state_seen(&c, b"hunter2"),
            b"osxkeychain:seen=__host=github.com__username=x-access-token__password=hunter2".to_vec(),
            "a port-less, path-less credential must omit both keys entirely"
        );

        c.port = Some(8443);
        c.path = Some("some/repo".into());
        let expected = concat!(
            "osxkeychain:seen=",
            "__host=github.com",
            "__port=8443",
            "__path=some/repo",
            "__username=x-access-token",
            "__password=hunter2",
        );
        assert_eq!(
            encode_state_seen(&c, b"hunter2"),
            expected.as_bytes(),
            "port and path take their upstream slots between host and username"
        );
    }

    /// `get` reports the state of the credential it wrote out, and a `store`
    /// echoing that same credential back must encode identically — that equality is
    /// the whole write-suppression guard. It has to survive the blob form of the
    /// secret: `get` sees password plus appended fields, git truncates the state
    /// line at the first newline, and `store` re-encodes from the bare password.
    #[test]
    fn state_from_get_blob_survives_the_round_trip_to_store() {
        let mut c = cred();
        c.password_expiry_utc = Some("1800000000".into());

        let blob = stored_secret(&c, "hunter2");
        let on_get = encode_state_seen(&c, &blob);
        // What git parses back out of the multi-line `state[]` value.
        let as_git_sees_it = &on_get[..on_get.iter().position(|b| *b == b'\n').unwrap()];

        assert_eq!(as_git_sees_it, encode_state_seen(&c, b"hunter2"));
        assert!(String::from_utf8_lossy(as_git_sees_it).starts_with(STATE_PREFIX));
    }

    /// A rotated secret must encode differently, or the guard would suppress the
    /// one write that matters.
    #[test]
    fn rotated_password_changes_the_state_token() {
        let c = cred();
        assert_ne!(encode_state_seen(&c, b"hunter2"), encode_state_seen(&c, b"rotated"));
    }

    /// The expiry and refresh token ride along in the single keychain data field as
    /// extra protocol lines; `get` writes the blob back verbatim, so the layout is
    /// what makes them reappear as their own `key=value` lines.
    #[test]
    fn secret_blob_appends_expiry_and_refresh_token_as_protocol_lines() {
        let mut c = cred();
        assert_eq!(stored_secret(&c, "hunter2"), b"hunter2".to_vec());

        c.password_expiry_utc = Some("1800000000".into());
        c.oauth_refresh_token = Some("ghr_refresh".into());
        assert_eq!(
            String::from_utf8(stored_secret(&c, "hunter2")).unwrap(),
            "hunter2\npassword_expiry_utc=1800000000\noauth_refresh_token=ghr_refresh"
        );
    }
}
