//! `git imap-send` — upload an mbox from stdin to an IMAP folder.
//!
//! This is a port of `imap-send.c`'s **in-tree IMAP4rev1 client** — git's own
//! code, the one `--no-curl` selects — over a `TcpStream`, rustls, or a child
//! process spawned for `imap.tunnel`. Every path runs: the greeting,
//! `CAPABILITY`, `STARTTLS`, `LOGIN` or `AUTHENTICATE`
//! (PLAIN / CRAM-MD5 / OAUTHBEARER / XOAUTH2), `EXAMINE` with the `CREATE`
//! fallback, one `APPEND` literal per message, `LIST "" "*"` under `--list`,
//! and `LOGOUT`.
//!
//! ### Which of stock git's two IMAP backends this matches
//!
//! `imap-send.c` carries two implementations: git's own client (above) and a
//! delegation to libcurl, picked by `--curl`/`--no-curl` with a build-time
//! default. Which one a stock binary can reach is a property of how it was
//! compiled, not of the command line. The binary on this machine is built
//! `USE_CURL_FOR_IMAP_SEND` **and** `NO_OPENSSL`, so `imap-send.c:1820-1824`
//! forces the curl backend and `--no-curl` only warns:
//!
//! ```text
//! $ git imap-send --no-curl -c imap.folder=Drafts < mbox
//! warning: --no-curl not supported in this build
//! Sending 1 message to Drafts folder...
//! ```
//!
//! This port has a TLS stack and no libcurl, so it behaves as the other build
//! does: `--curl` warns `--curl not supported in this build`
//! (`imap-send.c:1817`) and continues with git's own client, and `--no-curl`
//! selects it silently. The user-visible output of the two backends agrees on
//! the paths that matter — `Sending N message(s) to F folder...`, the percent
//! line, and `--list`'s untagged `* LIST` lines are all shared code
//! (`imap-send.c:1584-1600` / `:1721-1749`) — but the wire dialogue does not,
//! and neither does the login: libcurl picks a SASL mechanism off `CAPABILITY`
//! by itself, while git's client uses `LOGIN` unless `imap.authMethod` names
//! one, and warns when that goes out in the clear (`imap-send.c:1322-1324`).
//!
//! ### Configuration, all of it live
//!
//! `git_imap_config()` (`imap-send.c:1521`) reads nine variables and every one
//! of them reaches the wire here:
//!
//! * `imap.host` — the server. An `imap:` or `imaps:` scheme is stripped, and
//!   `imaps:` turns implicit TLS on; a leading `//` goes too
//!   (`imap-send.c:1547-1560`).
//! * `imap.port` — the port, defaulting to 993 under TLS and 143 without
//!   (`imap-send.c:1827-1828`).
//! * `imap.user`, `imap.pass` — the credentials. Either one missing sends the
//!   pair through `git credential fill`, and a successful login `approve`s them
//!   (`server_fill_credential`, `imap-send.c:1094`).
//! * `imap.folder` — the mailbox `APPEND` targets, overridden by `--folder`.
//! * `imap.tunnel` — a shell command whose stdin/stdout are the connection,
//!   used instead of a socket (`imap-send.c:1162-1177`).
//! * `imap.authMethod` — `PLAIN`, `CRAM-MD5`, `OAUTHBEARER` or `XOAUTH2`,
//!   selecting an `AUTHENTICATE` mechanism instead of `LOGIN`. The mechanism
//!   must appear in `CAPABILITY` as `AUTH=<name>` or the command stops
//!   (`try_auth_method`, `imap-send.c:1113`).
//! * `imap.sslverify` — off makes the TLS handshake accept any certificate,
//!   which is what `SSL_CTX_set_verify` not being called leaves OpenSSL doing
//!   (`imap-send.c:313-314`, `:351-360`).
//! * `imap.preformattedHTML` — wraps each message body in `<pre>` under a
//!   `text/html` content type (`wrap_in_html`, `imap-send.c:1427`).
//!
//! ### Deliberate deviations
//!
//! * **CRAM-MD5 challenge decoding.** `cram()` decodes with
//!   `EVP_DecodeBlock` (`imap-send.c:924`), which returns a length rounded up
//!   to a multiple of three and leaves the padding bytes in place, so a
//!   challenge whose length is not a multiple of three gets NUL bytes folded
//!   into the HMAC and authentication fails. This port strips the padding.
//!   Measured against the stock binary, which authenticated to a local server
//!   with challenge `<1896.697170952@example.com>` and password `s3cret`:
//!   `hmac-md5` over the 28-byte challenge is
//!   `c1dc8f79994fb600d7d1e1cf224962c5`, which is what stock sent; the
//!   NUL-padded 30-byte variant is `9d838cf3ec327ea8f8076cf9871a0255`, which
//!   is not.
//! * **`APPENDUID`.** `parse_response_code` records it into `cb->ctx`
//!   (`imap-send.c:720-725`), which no caller in this builtin ever sets, so
//!   the branch is unreachable and is not ported.
//! * **Pipelining.** The command queue (`imap->in_progress`) exists so several
//!   commands can be outstanding, but `imap_exec` waits for each one, so at
//!   most one ever is. The queue is kept, as a `Vec`, because
//!   `literal_pending` and the `+` continuation are written against it.
//!
//! ### Command line
//!
//! `parse_options` over the builtin's five options — `-v`/`--verbose`,
//! `-q`/`--quiet`, `--curl`, `-f`/`--folder <folder>`, `--list` — including
//! `--no-` forms, `--opt=value`, `-fVALUE` and `-f VALUE`, short bundling
//! (`-vq`), unique-prefix abbreviation (`--fol`, `--l`), and `--` as a
//! terminator. `-h` prints the 408-byte usage block on **stdout**, exit 129, at
//! the point `-h` is reached. The five `parse_options` diagnostics all go to
//! stderr with exit 129: ``error: unknown option `bogus'`` and
//! ``error: unknown switch `Z'``, both followed by the usage block;
//! ``error: option `folder' requires a value``, ``error: switch `f' requires a
//! value`` and ``error: option `curl' takes no value``, none of which print
//! usage. Any positional argument prints the usage block alone, exit 129.
//! Ambiguous-abbreviation errors cannot occur: no two option names share a
//! first letter.

use anyhow::Result;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitCode, Stdio};
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, Error as TlsError, RootCertStore,
    SignatureScheme, StreamOwned,
};

use gix::config::File as ConfigFile;

use super::send_email::self_exe;

/// The usage block from `imap_send_usage[]` plus the option table
/// `parse_options` renders under it. 408 bytes.
const USAGE: &str = concat!(
    "usage: git imap-send [-v] [-q] [--[no-]curl] [(--folder|-f) <folder>] < <mbox>\n",
    "   or: git imap-send --list\n",
    "\n",
    "    -v, --[no-]verbose    be more verbose\n",
    "    -q, --[no-]quiet      be more quiet\n",
    "    --[no-]curl           use libcurl to communicate with the IMAP server\n",
    "    -f, --[no-]folder <folder>\n",
    "                          specify the IMAP folder\n",
    "    --[no-]list           list all folders on the IMAP server\n",
    "\n",
);

/// The builtin's `imap_send_options[]`, in declaration order — the order
/// `parse_options` uses when matching an abbreviation.
const LONGS: &[(&str, bool)] = &[
    ("verbose", false),
    ("quiet", false),
    ("curl", false),
    ("folder", true),
    ("list", false),
];

/// State accumulated by the option scan.
#[derive(Default)]
struct Opts {
    /// `-f`/`--folder`: `None` unless the flag carried a value. `--no-folder`
    /// leaves this `None`, which is why it cannot clear `imap.folder`.
    folder: Option<String>,
    list: bool,
    /// `--curl`, which this build cannot honour; see the module docs.
    curl: bool,
    /// `OPT__VERBOSITY`: `-v` counts up, `-q` counts down, and either `--no-`
    /// form resets to zero (`parse_opt_verbosity_cb`).
    verbosity: i32,
    /// Any non-option argument. The builtin accepts none.
    positional: bool,
}

/// How the scan ended.
enum Scan {
    Ok(Opts),
    /// `-h`: usage on stdout, exit 129.
    Help,
    /// A diagnostic line, and whether the usage block follows it on stderr.
    Error(String, bool),
}

/// `usage_with_options()` — the block on stderr, exit 129.
fn usage_err() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Resolve a long option name: exact match first, then unique prefix. Returns
/// the canonical name and whether it takes a value.
fn resolve_long(name: &str) -> Option<(&'static str, bool)> {
    if name.is_empty() {
        return None;
    }
    if let Some(&(n, v)) = LONGS.iter().find(|(n, _)| *n == name) {
        return Some((n, v));
    }
    let mut hits = LONGS.iter().filter(|(n, _)| n.starts_with(name));
    let first = *hits.next()?;
    // No two names here share a first letter, so a second hit is impossible;
    // treating one as no-match would be wrong, so require uniqueness anyway.
    match hits.next() {
        None => Some(first),
        Some(_) => None,
    }
}

/// `parse_opt_verbosity_cb` — `-v` counts up unless the count is negative, `-q`
/// counts down unless it is positive, and `--no-verbose`/`--no-quiet` reset.
fn bump_verbosity(v: &mut i32, up: bool, negated: bool) {
    if negated {
        *v = 0;
    } else if up {
        *v = if *v >= 0 { *v + 1 } else { 1 };
    } else {
        *v = if *v <= 0 { *v - 1 } else { -1 };
    }
}

/// Reproduce `parse_options(..., 0)` over `imap_send_options[]`.
fn scan(args: &[String]) -> Scan {
    let mut opts = Opts::default();
    let mut it = args.iter().peekable();

    while let Some(arg) = it.next() {
        if arg == "--" {
            opts.positional |= it.next().is_some();
            break;
        }

        if let Some(body) = arg.strip_prefix("--") {
            let (spelled, value) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (body, None),
            };
            let (base, negated) = match spelled.strip_prefix("no-") {
                Some(b) => (b, true),
                None => (spelled, false),
            };
            let Some((name, takes_value)) = resolve_long(base) else {
                return Scan::Error(format!("error: unknown option `{spelled}'"), true);
            };
            // A `--no-` form never consumes a value, so `--no-folder=x` is
            // reported against the name as the user spelled it.
            if negated || !takes_value {
                if value.is_some() {
                    let shown = if negated { format!("no-{name}") } else { name.to_string() };
                    return Scan::Error(format!("error: option `{shown}' takes no value"), false);
                }
                match (name, negated) {
                    ("list", n) => opts.list = !n,
                    ("curl", n) => opts.curl = !n,
                    ("folder", true) => {}
                    ("verbose", n) => bump_verbosity(&mut opts.verbosity, true, n),
                    ("quiet", n) => bump_verbosity(&mut opts.verbosity, false, n),
                    _ => {}
                }
                continue;
            }
            let value = match value.or_else(|| it.next().cloned()) {
                Some(v) => v,
                None => {
                    return Scan::Error(format!("error: option `{name}' requires a value"), false)
                }
            };
            opts.folder = Some(value);
            continue;
        }

        // A bare `-` is an ordinary argument, as is anything not starting `-`.
        let bundle = match arg.strip_prefix('-') {
            Some(b) if !b.is_empty() => b,
            _ => {
                opts.positional = true;
                continue;
            }
        };
        let mut rest = bundle;
        while let Some(c) = rest.chars().next() {
            rest = &rest[c.len_utf8()..];
            match c {
                'h' => return Scan::Help,
                'v' => bump_verbosity(&mut opts.verbosity, true, false),
                'q' => bump_verbosity(&mut opts.verbosity, false, false),
                'f' => {
                    // The remainder of the bundle is the value, else the next
                    // argument.
                    let value = if rest.is_empty() {
                        it.next().cloned()
                    } else {
                        Some(std::mem::take(&mut rest).to_string())
                    };
                    match value {
                        Some(v) => opts.folder = Some(v),
                        None => {
                            return Scan::Error("error: switch `f' requires a value".into(), false)
                        }
                    }
                    break;
                }
                _ => return Scan::Error(format!("error: unknown switch `{c}'"), true),
            }
        }
    }

    Scan::Ok(opts)
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// `struct imap_server_conf` (`imap-send.c:81`) after `git_imap_config` has run.
#[derive(Default)]
struct ServerConf {
    /// `imap.tunnel` — a shell command that *is* the connection.
    tunnel: Option<String>,
    /// `imap.host`, scheme and `//` stripped.
    host: Option<String>,
    /// `imap.port`, zero until the 993/143 default is applied.
    port: u16,
    /// `imap.folder`, or `--folder`.
    folder: Option<String>,
    /// `imap.user` / `imap.pass`, filled from the credential helper when either
    /// is missing.
    user: Option<String>,
    pass: Option<String>,
    /// Set by an `imaps:` scheme on `imap.host`.
    use_ssl: bool,
    /// `imap.sslverify`, which defaults to true (`imap-send.c:1794-1796`).
    ssl_verify: bool,
    /// `imap.preformattedHTML`.
    use_html: bool,
    /// `imap.authMethod`.
    auth_method: Option<String>,
}

/// The config git would see: the repository in the current directory when there
/// is one, otherwise the global and system files alone. `imap-send` runs
/// `setup_git_directory_gently()`, so it works outside a repository.
fn load_config() -> Option<ConfigFile> {
    match gix::discover(".") {
        Ok(repo) => Some(repo.config_snapshot().plumbing().clone()),
        Err(_) => {
            let mut file = ConfigFile::from_globals().ok()?;
            file.append(ConfigFile::from_environment_overrides().ok()?).ok()?;
            Some(file)
        }
    }
}

/// `git_imap_config()` (`imap-send.c:1521`) — every `imap.*` variable, into the
/// struct the rest of the command runs on.
fn git_imap_config(cfg: Option<&ConfigFile>) -> ServerConf {
    // `.ssl_verify = 1` in the `imap_server_conf` initialiser.
    let mut server = ServerConf { ssl_verify: true, ..ServerConf::default() };
    let Some(cfg) = cfg else { return server };
    let string = |key: &str| cfg.string(key).map(|v| v.to_string());
    // `git_config_bool`: presence without a value is true, otherwise git's
    // boolean spelling.
    let boolean = |key: &str, default: bool| cfg.boolean(key).ok().flatten().unwrap_or(default);

    server.ssl_verify = boolean("imap.sslverify", true);
    server.use_html = boolean("imap.preformattedhtml", false);
    server.folder = string("imap.folder");
    server.user = string("imap.user");
    server.pass = string("imap.pass");
    server.tunnel = string("imap.tunnel");
    server.auth_method = string("imap.authmethod");
    // `git_config_int`; a value that will not fit a port is not one.
    server.port =
        cfg.integer("imap.port").ok().flatten().and_then(|v| u16::try_from(v).ok()).unwrap_or(0);
    if let Some(mut val) = string("imap.host") {
        // `imap:` and `imaps:` are stripped, and `imaps:` means implicit TLS.
        if let Some(rest) = val.strip_prefix("imap:") {
            val = rest.to_string();
        } else if let Some(rest) = val.strip_prefix("imaps:") {
            val = rest.to_string();
            server.use_ssl = true;
        }
        if let Some(rest) = val.strip_prefix("//") {
            val = rest.to_string();
        }
        server.host = Some(val);
    }
    server
}

// ---------------------------------------------------------------------------
// base64, MD5 and HMAC-MD5
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `EVP_EncodeBlock` — padded base64 on one line, which is what every
/// `*_base64()` helper in `imap-send.c` hands to the socket.
fn b64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Decode base64, dropping the padding — see the module docs for why this does
/// not reproduce `EVP_DecodeBlock`'s trailing NULs.
fn b64_decode(input: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in input.bytes() {
        let Some(v) = B64.iter().position(|&x| x == c) else { continue };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// MD5 (RFC 1321), the digest `cram()` drives through `HMAC(EVP_md5(), ...)`.
fn md5(message: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    // K[i] = floor(2^32 * abs(sin(i + 1))), the table from the RFC.
    let mut k = [0u32; 64];
    for (i, slot) in k.iter_mut().enumerate() {
        *slot = ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32;
    }

    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((message.len() as u64) * 8).to_le_bytes());

    let mut h: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for block in padded.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(k[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
    }

    let mut out = [0u8; 16];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// HMAC-MD5 (RFC 2104) — `HMAC(EVP_md5(), pass, ..., challenge, ...)`.
fn hmac_md5(key: &[u8], message: &[u8]) -> [u8; 16] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..16].copy_from_slice(&md5(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(BLOCK + message.len());
    inner.extend(k.iter().map(|b| b ^ 0x36));
    inner.extend_from_slice(message);
    let mut outer = Vec::with_capacity(BLOCK + 16);
    outer.extend(k.iter().map(|b| b ^ 0x5c));
    outer.extend_from_slice(&md5(&inner));
    md5(&outer)
}

/// `hexchar()` over a digest — the lower-case hex `cram()` puts in its response.
fn hex(digest: &[u8; 16]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// The connection
// ---------------------------------------------------------------------------

/// `Timeout` for the socket, matching the connect timeout `send-email`'s SMTP
/// transport uses; `imap-send.c` leaves the kernel default in place, which no
/// caller can observe except by waiting.
const TIMEOUT: Duration = Duration::from_secs(120);

/// A `ServerCertVerifier` that accepts anything, which is where
/// `imap.sslverify=false` leaves OpenSSL: `SSL_CTX_set_verify` is never called,
/// so the handshake result is not checked and `verify_hostname` is skipped
/// (`imap-send.c:313-314`, `:351-360`).
#[derive(Debug)]
struct NoVerify {
    schemes: Vec<SignatureScheme>,
}

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes.clone()
    }
}

/// `ssl_socket_connect()`'s `SSL_CTX` — the platform trust store, which is
/// where `SSL_CTX_set_default_verify_paths` leaves OpenSSL, unless
/// `imap.sslverify` is off.
fn tls_config(ssl_verify: bool) -> Result<Arc<ClientConfig>, String> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?;
    if !ssl_verify {
        let verifier =
            Arc::new(NoVerify { schemes: provider.signature_verification_algorithms.supported_schemes() });
        return Ok(Arc::new(
            builder.dangerous().with_custom_certificate_verifier(verifier).with_no_client_auth(),
        ));
    }
    let mut roots = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        roots.add(cert).ok();
    }
    if roots.is_empty() {
        return Err("unable to load the platform certificate store".into());
    }
    Ok(Arc::new(builder.with_root_certificates(roots).with_no_client_auth()))
}

/// `struct imap_socket` — a socket, a TLS session over one, or the two pipes of
/// an `imap.tunnel` child.
enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
    Tunnel { child: Child, stdin: ChildStdin, stdout: ChildStdout },
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
            Stream::Tunnel { stdout, .. } => stdout.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
            Stream::Tunnel { stdin, .. } => stdin.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
            Stream::Tunnel { stdin, .. } => stdin.flush(),
        }
    }
}

// ---------------------------------------------------------------------------
// The IMAP session
// ---------------------------------------------------------------------------

const RESP_OK: i32 = 0;
const RESP_NO: i32 = 1;
const RESP_BAD: i32 = 2;

/// `enum CAPABILITY` and `cap_list[]` (`imap-send.c:146-168`), as the bit each
/// name sets.
const CAP_LIST: &[&str] = &[
    "LOGINDISABLED",
    "UIDPLUS",
    "LITERAL+",
    "NAMESPACE",
    "STARTTLS",
    "AUTH=PLAIN",
    "AUTH=CRAM-MD5",
    "AUTH=OAUTHBEARER",
    "AUTH=XOAUTH2",
];
const NOLOGIN: u32 = 0;
const LITERALPLUS: u32 = 2;
const STARTTLS: u32 = 4;

/// Which `AUTHENTICATE` mechanism a `+` continuation should answer. The C code
/// stores a function pointer in `cmd->cb.cont`; an enum keeps the same
/// dispatch without borrowing the store the function would need.
#[derive(Clone, Copy, PartialEq)]
enum Cont {
    Plain,
    CramMd5,
    OAuthBearer,
    XOAuth2,
}

impl Cont {
    /// The `AUTH=<name>` capability the mechanism needs, and its spelling in
    /// the `AUTHENTICATE` command.
    fn name(self) -> &'static str {
        match self {
            Cont::Plain => "PLAIN",
            Cont::CramMd5 => "CRAM-MD5",
            Cont::OAuthBearer => "OAUTHBEARER",
            Cont::XOAuth2 => "XOAUTH2",
        }
    }

    /// The bit in `imap->caps` this mechanism is gated on.
    fn cap(self) -> u32 {
        match self {
            Cont::Plain => 5,
            Cont::CramMd5 => 6,
            Cont::OAuthBearer => 7,
            Cont::XOAuth2 => 8,
        }
    }
}

/// `struct imap_cmd` — one outstanding command.
struct Cmd {
    tag: u32,
    /// The text, kept because a failing tagged reply names it.
    cmd: String,
    /// `cb.data`: a literal to send once the server says `+`.
    data: Option<Vec<u8>>,
    /// `cb.cont`: a mechanism to answer a `+` with.
    cont: Option<Cont>,
}

/// `struct imap` plus `struct imap_store` — the whole session.
struct Store {
    stream: Option<Stream>,
    /// `imap_buffer`: bytes read but not yet consumed as a line.
    buf: Vec<u8>,
    /// How far into `buf` the CRLF scan has already looked.
    scan: usize,
    caps: u32,
    rcaps: u32,
    nexttag: u32,
    uidnext: i32,
    uidvalidity: i32,
    literal_pending: bool,
    in_progress: Vec<Cmd>,
    /// The mailbox `APPEND` targets (`ctx->name`).
    name: String,
    /// `ctx->prefix`, which this builtin always leaves empty.
    prefix: String,
    verbosity: i32,
    list_folders: bool,
    /// The credentials the `AUTHENTICATE` continuations answer with.
    user: String,
    pass: String,
}

/// `CAP(cap)`.
fn cap(caps: u32, bit: u32) -> bool {
    caps & (1 << bit) != 0
}

/// `next_arg()` (`imap-send.c:489`) — the next whitespace- or quote-delimited
/// word, advancing the cursor past it. `None` for the cursor is the C `NULL`.
fn next_arg<'a>(s: &mut Option<&'a str>) -> Option<&'a str> {
    let cur = (*s)?;
    let b = cur.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= b.len() {
        *s = None;
        return None;
    }
    let (start, end) = if b[i] == b'"' {
        i += 1;
        match cur[i..].find('"') {
            Some(k) => (i, i + k),
            // `strchr` came back NULL: the word runs to the end and the cursor
            // is dropped.
            None => {
                *s = None;
                return Some(&cur[i..]);
            }
        }
    } else {
        let mut j = i;
        while j < b.len() && !b[j].is_ascii_whitespace() {
            j += 1;
        }
        (i, j)
    };
    let ret = &cur[start..end];
    // `if (**s) *(*s)++ = 0; if (!**s) *s = NULL;` — step over the delimiter,
    // and drop the cursor when nothing is left behind it.
    *s = cur.get(end + 1..).filter(|rest| !rest.is_empty());
    Some(ret)
}

/// `skip_imap_list_l()` (`imap-send.c:623`) — step the cursor over one
/// parenthesised list, sublists, quoted strings and atoms included.
fn skip_list(s: &mut Option<&str>) {
    let Some(cur) = *s else { return };
    let b = cur.as_bytes();
    let mut i = 0usize;
    let mut level = 0usize;
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return;
        }
        if level > 0 && b[i] == b')' {
            i += 1;
            level -= 1;
            if level == 0 {
                break;
            }
            continue;
        }
        if b[i] == b'(' {
            i += 1;
            level += 1;
            continue;
        }
        if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += 1;
            }
            if i >= b.len() {
                return;
            }
            i += 1;
        } else {
            while i < b.len() && !b[i].is_ascii_whitespace() {
                if level > 0 && b[i] == b')' {
                    break;
                }
                i += 1;
            }
        }
        if level == 0 {
            break;
        }
    }
    *s = if i < b.len() { Some(&cur[i..]) } else { None };
}

impl Store {
    /// `socket_write()` — the whole buffer or a failure.
    fn write_all(&mut self, buf: &[u8]) -> bool {
        let Some(stream) = self.stream.as_mut() else { return false };
        if stream.write_all(buf).is_err() || stream.flush().is_err() {
            self.stream = None;
            return false;
        }
        true
    }

    /// `buffer_gets()` (`imap-send.c:415`) — one CRLF-terminated line, echoed
    /// under `-v` and, for `--list`, whenever it is an untagged `* LIST`.
    fn gets(&mut self) -> Option<String> {
        loop {
            if let Some(i) = find_crlf(&self.buf, self.scan) {
                let line = String::from_utf8_lossy(&self.buf[..i]).into_owned();
                self.buf.drain(..i + 2);
                self.scan = 0;
                if self.verbosity > 0 || (self.list_folders && line.contains("* LIST")) {
                    println!("{line}");
                }
                return Some(line);
            }
            self.scan = self.buf.len().saturating_sub(1);
            let mut chunk = [0u8; 1024];
            let stream = self.stream.as_mut()?;
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => {
                    self.stream = None;
                    return None;
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    /// `parse_capability()` (`imap-send.c:670`).
    fn parse_capability(&mut self, cmd: Option<&str>) {
        self.caps = 0x8000_0000;
        let mut cur = cmd;
        while let Some(arg) = next_arg(&mut cur) {
            if let Some(i) = CAP_LIST.iter().position(|c| *c == arg) {
                self.caps |= 1 << i;
            }
        }
        self.rcaps = self.caps;
    }

    /// `parse_response_code()` (`imap-send.c:683`) — the `[...]` a status line
    /// may carry. `APPENDUID` is not handled; see the module docs.
    fn parse_response_code(&mut self, s: Option<&str>) -> i32 {
        let Some(s) = s else { return RESP_OK };
        let Some(body) = s.strip_prefix('[') else { return RESP_OK };
        let Some(close) = body.find(']') else {
            eprintln!("IMAP error: malformed response code");
            return RESP_BAD;
        };
        let (inside, after) = (&body[..close], &body[close + 1..]);
        let mut cur = Some(inside);
        let Some(arg) = next_arg(&mut cur) else {
            eprintln!("IMAP error: empty response code");
            return RESP_BAD;
        };
        match arg {
            "UIDVALIDITY" => {
                match next_arg(&mut cur).and_then(|v| v.parse::<i32>().ok()).filter(|v| *v != 0) {
                    Some(v) => self.uidvalidity = v,
                    None => {
                        eprintln!("IMAP error: malformed UIDVALIDITY status");
                        return RESP_BAD;
                    }
                }
            }
            "UIDNEXT" => {
                match next_arg(&mut cur).and_then(|v| v.parse::<i32>().ok()).filter(|v| *v != 0) {
                    Some(v) => self.uidnext = v,
                    None => {
                        eprintln!("IMAP error: malformed NEXTUID status");
                        return RESP_BAD;
                    }
                }
            }
            "CAPABILITY" => self.parse_capability(cur),
            "ALERT" => eprintln!("*** IMAP ALERT *** {}", after.trim_start()),
            _ => {}
        }
        RESP_OK
    }

    /// `issue_imap_cmd()` (`imap-send.c:519`) — write one tagged command and
    /// queue it. `data` becomes a literal, `cont` a mechanism to answer `+`.
    fn issue(&mut self, cmd: &str, data: Option<Vec<u8>>, cont: Option<Cont>) -> Option<u32> {
        self.nexttag += 1;
        let tag = self.nexttag;

        while self.literal_pending {
            self.get_cmd_result(None);
        }

        let line = match &data {
            None => format!("{tag} {cmd}\r\n"),
            Some(d) => {
                let plus = if cap(self.caps, LITERALPLUS) { "+" } else { "" };
                format!("{tag} {cmd}{{{}{plus}}}\r\n", d.len())
            }
        };
        if self.verbosity > 0 {
            if !self.in_progress.is_empty() {
                print!("({} in progress) ", self.in_progress.len());
            }
            if cmd.starts_with("LOGIN") {
                println!(">>> {tag} LOGIN <user> <pass>");
            } else {
                print!(">>> {line}");
            }
        }
        if !self.write_all(line.as_bytes()) {
            return None;
        }

        let mut queued = Cmd { tag, cmd: cmd.to_string(), data, cont };
        if queued.data.is_some() {
            if cap(self.caps, LITERALPLUS) {
                let payload = queued.data.take().unwrap_or_default();
                if !self.write_all(&payload) || !self.write_all(b"\r\n") {
                    return None;
                }
            } else {
                self.literal_pending = true;
            }
        } else if queued.cont.is_some() {
            self.literal_pending = true;
        }
        self.in_progress.push(queued);
        Some(tag)
    }

    /// `imap_exec()` (`imap-send.c:588`).
    fn exec(&mut self, cmd: &str, data: Option<Vec<u8>>, cont: Option<Cont>) -> i32 {
        match self.issue(cmd, data, cont) {
            None => RESP_BAD,
            Some(tag) => self.get_cmd_result(Some(tag)),
        }
    }

    /// The `AUTHENTICATE` continuations (`imap-send.c:1007-1083`) — each writes
    /// one base64 line, and `get_cmd_result` adds the CRLF.
    fn run_cont(&mut self, cont: Cont, prompt: Option<&str>) -> bool {
        let payload = match cont {
            // "\0user\0pass", RFC 4616.
            Cont::Plain => {
                let mut raw = vec![0u8];
                raw.extend_from_slice(self.user.as_bytes());
                raw.push(0);
                raw.extend_from_slice(self.pass.as_bytes());
                b64_encode(&raw)
            }
            Cont::CramMd5 => {
                let challenge = b64_decode(prompt.unwrap_or_default());
                let digest = hmac_md5(self.pass.as_bytes(), &challenge);
                b64_encode(format!("{} {}", self.user, hex(&digest)).as_bytes())
            }
            // The gs2 header of RFC 5801 plus the RFC 7628 key/value pairs.
            Cont::OAuthBearer => b64_encode(
                format!("n,a={},\x01auth=Bearer {}\x01\x01", self.user, self.pass).as_bytes(),
            ),
            Cont::XOAuth2 => {
                b64_encode(format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.pass).as_bytes())
            }
        };
        if !self.write_all(payload.as_bytes()) {
            eprintln!("error: IMAP error: sending {} response failed", cont.name());
            return false;
        }
        true
    }

    /// `get_cmd_result()` (`imap-send.c:730`) — read responses until the tagged
    /// reply for `tcmd` arrives, or until the first `+` when `tcmd` is `None`.
    fn get_cmd_result(&mut self, tcmd: Option<u32>) -> i32 {
        loop {
            let Some(line) = self.gets() else { return RESP_BAD };
            let mut cur = Some(line.as_str());
            let Some(arg) = next_arg(&mut cur) else {
                eprintln!("IMAP error: empty response");
                return RESP_BAD;
            };

            if arg.starts_with('*') {
                let Some(arg) = next_arg(&mut cur) else {
                    eprintln!("IMAP error: unable to parse untagged response");
                    return RESP_BAD;
                };
                match arg {
                    "NAMESPACE" => {
                        // Personal, others' and shared mailboxes.
                        skip_list(&mut cur);
                        skip_list(&mut cur);
                        skip_list(&mut cur);
                    }
                    "OK" | "BAD" | "NO" | "BYE" => {
                        let resp = self.parse_response_code(cur);
                        if resp != RESP_OK {
                            return resp;
                        }
                    }
                    "CAPABILITY" => self.parse_capability(cur),
                    _ => {
                        // Unhandled response-data with at least two words is
                        // ignored; one word is a parse failure.
                        if next_arg(&mut cur).is_none() {
                            eprintln!("IMAP error: unable to parse untagged response");
                            return RESP_BAD;
                        }
                    }
                }
            } else if self.in_progress.is_empty() {
                eprintln!("IMAP error: unexpected reply: {arg} {}", cur.unwrap_or(""));
                return RESP_BAD;
            } else if arg.starts_with('+') {
                // Only the last command can be underway: a continuation
                // enforces a round trip.
                let last = self.in_progress.len() - 1;
                let data = self.in_progress[last].data.take();
                let cont = self.in_progress[last].cont;
                if let Some(payload) = data {
                    if !self.write_all(&payload) {
                        return RESP_BAD;
                    }
                } else if let Some(cont) = cont {
                    if !self.run_cont(cont, cur) {
                        return RESP_BAD;
                    }
                } else {
                    eprintln!("IMAP error: unexpected command continuation request");
                    return RESP_BAD;
                }
                if !self.write_all(b"\r\n") {
                    return RESP_BAD;
                }
                if cont.is_none() {
                    self.literal_pending = false;
                }
                if tcmd.is_none() {
                    return RESP_OK;
                }
            } else {
                let Ok(tag) = arg.parse::<u32>() else {
                    eprintln!("IMAP error: malformed tag {arg}");
                    return RESP_BAD;
                };
                let Some(at) = self.in_progress.iter().position(|c| c.tag == tag) else {
                    eprintln!("IMAP error: unexpected tag {arg}");
                    return RESP_BAD;
                };
                let done = self.in_progress.remove(at);
                if done.cont.is_some() || done.data.is_some() {
                    self.literal_pending = false;
                }
                let status = next_arg(&mut cur).unwrap_or("");
                let mut resp = if status == "OK" {
                    RESP_OK
                } else {
                    let resp = if status == "NO" { RESP_NO } else { RESP_BAD };
                    let shown = if done.cmd.starts_with("LOGIN") {
                        "LOGIN <user> <pass>"
                    } else {
                        done.cmd.as_str()
                    };
                    eprintln!(
                        "IMAP command '{shown}' returned response ({status}) - {}",
                        cur.unwrap_or("")
                    );
                    resp
                };
                let resp2 = self.parse_response_code(cur);
                if resp2 > resp {
                    resp = resp2;
                }
                if tcmd.is_none_or(|t| t == done.tag) {
                    return resp;
                }
            }
        }
    }

    /// `imap_close_server()` (`imap-send.c:850`) — `LOGOUT`, then drop the
    /// socket or reap the tunnel.
    fn close(&mut self) {
        if self.stream.is_some() {
            self.exec("LOGOUT", None, None);
        }
        if let Some(Stream::Tunnel { mut child, stdin, stdout }) = self.stream.take() {
            drop(stdin);
            drop(stdout);
            child.wait().ok();
        }
    }
}

/// The index of the next `\r\n` at or after `from`.
fn find_crlf(buf: &[u8], from: usize) -> Option<usize> {
    if buf.len() < 2 {
        return None;
    }
    (from.min(buf.len().saturating_sub(1))..buf.len() - 1).find(|&i| buf[i] == b'\r' && buf[i + 1] == b'\n')
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// `server_fill_credential()` (`imap-send.c:1094`) — one `git credential`
/// round trip, filling whichever of user and password is missing.
struct Credential {
    protocol: String,
    host: String,
    username: Option<String>,
    password: Option<String>,
}

impl Credential {
    fn encode(&self) -> String {
        let mut out = String::new();
        let mut push = |key: &str, value: Option<&str>| {
            if let Some(v) = value {
                if !v.contains(['\n', '\0']) {
                    out.push_str(&format!("{key}={v}\n"));
                }
            }
        };
        push("protocol", Some(&self.protocol));
        push("host", Some(&self.host));
        push("username", self.username.as_deref());
        push("password", self.password.as_deref());
        out.push('\n');
        out
    }

    fn absorb(&mut self, reply: &str) {
        for line in reply.lines() {
            if line.is_empty() {
                break;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            match key {
                "protocol" => self.protocol = value.to_string(),
                "host" => self.host = value.to_string(),
                "username" => self.username = Some(value.to_string()),
                "password" => self.password = Some(value.to_string()),
                _ => {}
            }
        }
    }

    /// `credential_fill` / `credential_approve` / `credential_reject`.
    fn run(&mut self, op: &str) -> bool {
        let Ok(mut child) = Command::new(self_exe())
            .args(["credential", op])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        else {
            return false;
        };
        if let Some(stdin) = child.stdin.as_mut() {
            if stdin.write_all(self.encode().as_bytes()).is_err() {
                return false;
            }
        }
        drop(child.stdin.take());
        let Ok(out) = child.wait_with_output() else { return false };
        if !out.status.success() {
            return false;
        }
        if op == "fill" {
            self.absorb(&String::from_utf8_lossy(&out.stdout));
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Opening the store
// ---------------------------------------------------------------------------

/// `imap_info()` (`imap-send.c:465`) — progress chatter on stdout, which the
/// default verbosity of zero already prints.
fn imap_info(verbosity: i32, msg: &str) {
    if verbosity >= 0 {
        print!("{msg}");
        std::io::stdout().flush().ok();
    }
}

/// `imap_warn()` (`imap-send.c:477`).
fn imap_warn(verbosity: i32, msg: &str) {
    if verbosity > -2 {
        eprint!("{msg}");
    }
}

/// `imap_open_store()` (`imap-send.c:1145`) — connect, greet, authenticate, and
/// make sure `folder` exists.
fn imap_open_store(cfg: &mut ServerConf, folder: &str, verbosity: i32, list: bool) -> Option<Store> {
    let host = cfg.host.clone().unwrap_or_default();
    let mut store = Store {
        stream: None,
        buf: Vec::new(),
        scan: 0,
        caps: 0,
        rcaps: 0,
        nexttag: 0,
        uidnext: 0,
        uidvalidity: 0,
        literal_pending: false,
        in_progress: Vec::new(),
        name: folder.to_string(),
        prefix: String::new(),
        verbosity,
        list_folders: list,
        user: String::new(),
        pass: String::new(),
    };

    if let Some(tunnel) = cfg.tunnel.clone() {
        imap_info(verbosity, &format!("Starting tunnel '{tunnel}'... "));
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&tunnel)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| {
                eprintln!("fatal: cannot start proxy {tunnel}: {e}");
                std::process::exit(128);
            });
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        store.stream = Some(Stream::Tunnel { child, stdin, stdout });
        imap_info(verbosity, "OK\n");
    } else {
        imap_info(verbosity, &format!("Resolving {host}... "));
        let targets = match format!("{host}:{}", cfg.port).to_socket_addrs() {
            Ok(t) => t.collect::<Vec<_>>(),
            Err(e) => {
                eprintln!("getaddrinfo: {e}");
                return None;
            }
        };
        imap_info(verbosity, "OK\n");

        let mut sock = None;
        for target in targets {
            imap_info(verbosity, &format!("Connecting to [{}]:{}... ", target.ip(), cfg.port));
            match TcpStream::connect_timeout(&target, TIMEOUT) {
                Ok(s) => {
                    sock = Some(s);
                    break;
                }
                Err(e) => eprintln!("connect: {e}"),
            }
        }
        let Some(sock) = sock else {
            eprintln!("error: unable to connect to server");
            return None;
        };
        sock.set_read_timeout(Some(TIMEOUT)).ok();
        sock.set_write_timeout(Some(TIMEOUT)).ok();

        store.stream = Some(if cfg.use_ssl {
            match tls_connect(sock, &host, cfg.ssl_verify) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("SSL_connect: {e}");
                    return None;
                }
            }
        } else {
            Stream::Plain(sock)
        });
        imap_info(verbosity, "OK\n");
    }

    // The greeting.
    let Some(greeting) = store.gets() else {
        eprintln!("IMAP error: no greeting response");
        return None;
    };
    let mut cur = Some(greeting.as_str());
    let star = next_arg(&mut cur);
    let status = next_arg(&mut cur);
    if star.is_none_or(|a| !a.starts_with('*')) || status.is_none() {
        eprintln!("IMAP error: invalid greeting response");
        return None;
    }
    let status = status.unwrap_or("");
    let preauth = status == "PREAUTH";
    if !preauth && status != "OK" {
        eprintln!("IMAP error: unknown greeting response");
        return None;
    }
    store.parse_response_code(cur);
    if store.caps == 0 && store.exec("CAPABILITY", None, None) != RESP_OK {
        return None;
    }

    let mut cred = None;
    if !preauth {
        if !cfg.use_ssl && cap(store.caps, STARTTLS) {
            if store.exec("STARTTLS", None, None) != RESP_OK {
                return None;
            }
            let Some(Stream::Plain(sock)) = store.stream.take() else {
                eprintln!("IMAP error: STARTTLS on a connection that cannot be upgraded");
                return None;
            };
            store.stream = Some(match tls_connect(sock, &host, cfg.ssl_verify) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("SSL_connect: {e}");
                    return None;
                }
            });
            // Capabilities may have changed.
            if store.exec("CAPABILITY", None, None) != RESP_OK {
                return None;
            }
        }

        imap_info(verbosity, "Logging in...\n");
        // `server_fill_credential`: only when something is missing.
        if cfg.user.is_none() || cfg.pass.is_none() {
            let mut c = Credential {
                protocol: if cfg.use_ssl { "imaps".into() } else { "imap".into() },
                host: format!("{host}:{}", cfg.port),
                username: cfg.user.clone(),
                password: cfg.pass.clone(),
            };
            c.run("fill");
            if cfg.user.is_none() {
                cfg.user.clone_from(&c.username);
            }
            if cfg.pass.is_none() {
                cfg.pass.clone_from(&c.password);
            }
            // `if (cred.username)` gates the later approve/reject.
            cred = if c.username.is_some() { Some(c) } else { None };
        }
        store.user = cfg.user.clone().unwrap_or_default();
        store.pass = cfg.pass.clone().unwrap_or_default();

        let ok = match cfg.auth_method.as_deref() {
            None => {
                if cap(store.caps, NOLOGIN) {
                    eprintln!(
                        "skipping account {}@{host}, server forbids LOGIN",
                        store.user
                    );
                    false
                } else {
                    if !matches!(store.stream, Some(Stream::Tls(_))) {
                        imap_warn(
                            verbosity,
                            "*** IMAP Warning *** Password is being sent in the clear\n",
                        );
                    }
                    let cmd = format!("LOGIN \"{}\" \"{}\"", store.user, store.pass);
                    if store.exec(&cmd, None, None) != RESP_OK {
                        eprintln!("IMAP error: LOGIN failed");
                        false
                    } else {
                        true
                    }
                }
            }
            Some(method) => {
                let cont = match method {
                    "PLAIN" => Some(Cont::Plain),
                    "CRAM-MD5" => Some(Cont::CramMd5),
                    "OAUTHBEARER" => Some(Cont::OAuthBearer),
                    "XOAUTH2" => Some(Cont::XOAuth2),
                    _ => None,
                };
                match cont {
                    None => {
                        eprintln!("unknown authentication mechanism: {method}");
                        false
                    }
                    // `try_auth_method` (`imap-send.c:1113`).
                    Some(cont) if !cap(store.caps, cont.cap()) => {
                        eprintln!(
                            "You specified {method} as authentication method, but {host} doesn't support it."
                        );
                        false
                    }
                    Some(cont) => {
                        let cmd = format!("AUTHENTICATE {}", cont.name());
                        if store.exec(&cmd, None, Some(cont)) != RESP_OK {
                            eprintln!("IMAP error: AUTHENTICATE {method} failed");
                            false
                        } else {
                            true
                        }
                    }
                }
            }
        };
        if !ok {
            if let Some(mut c) = cred {
                c.run("reject");
            }
            store.close();
            return None;
        }
    }

    if let Some(mut c) = cred {
        c.run("approve");
    }

    // Check the target mailbox exists.
    let examine = format!("EXAMINE \"{}\"", store.name);
    match store.exec(&examine, None, None) {
        RESP_OK => {}
        RESP_NO => {
            let create = format!("CREATE \"{}\"", store.name);
            if store.exec(&create, None, None) == RESP_OK {
                imap_info(verbosity, "Created missing mailbox\n");
            } else {
                eprintln!("IMAP error: could not create missing mailbox");
                store.close();
                return None;
            }
        }
        _ => {
            eprintln!("IMAP error: could not check mailbox");
            store.close();
            return None;
        }
    }

    Some(store)
}

/// `ssl_socket_connect()` (`imap-send.c:282`), as a rustls handshake with SNI.
///
/// `SSL_connect` is a distinct step in the C, and a certificate it rejects is
/// reported there rather than as a missing greeting later, so the handshake is
/// driven to completion here instead of being left to the first read.
fn tls_connect(sock: TcpStream, host: &str, ssl_verify: bool) -> Result<Stream, String> {
    let config = tls_config(ssl_verify)?;
    let name = ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
    let conn = ClientConnection::new(config, name).map_err(|e| e.to_string())?;
    let mut stream = StreamOwned::new(conn, sock);
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock).map_err(|e| e.to_string())?;
    }
    Ok(Stream::Tls(Box::new(stream)))
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// `lf_to_crlf()` (`imap-send.c:1372`) — every LF gains a CR unless it has one.
fn lf_to_crlf(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(msg.len());
    let mut last = 0u8;
    for &b in msg {
        if b == b'\n' && last != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        last = b;
    }
    out
}

/// `strbuf_addstr_xml_quoted()` (`strbuf.c:804`).
fn xml_quoted(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for &b in s {
        match b {
            b'"' => out.extend_from_slice(b"&quot;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'&' => out.extend_from_slice(b"&amp;"),
            _ => out.push(b),
        }
    }
    out
}

/// `wrap_in_html()` (`imap-send.c:1427`) — the body in a `<pre>` block under a
/// `text/html` content type. A message with no blank line has no body, and is
/// left alone.
fn wrap_in_html(msg: &[u8]) -> Vec<u8> {
    let Some(at) = msg.windows(2).position(|w| w == b"\n\n") else { return msg.to_vec() };
    let body = at + 2;
    let mut out = Vec::with_capacity(msg.len() + 64);
    // The headers plus the first of the two newlines.
    out.extend_from_slice(&msg[..body - 1]);
    out.extend_from_slice(b"Content-Type: text/html;\n");
    out.push(b'\n');
    out.extend_from_slice(b"<pre>\n");
    out.extend_from_slice(&xml_quoted(&msg[body..]));
    out.extend_from_slice(b"</pre>\n");
    out
}

/// `count_messages()` — how many `From `-delimited messages the buffer holds.
///
/// The scan has two shapes. A `From git-send-email` line — the one
/// `git send-email` writes for its `sendemail.imapSentFolder` copy — needs only
/// a `From: ` and then a `To: ` header after it; any other `From ` line needs
/// `From: `, `Date: ` and `Subject: `, in that order. Positions advance the way
/// the C `strstr` chain does, so a header block that appears before its `From `
/// line, or a `From ` line that is not at the start of the buffer and not
/// reachable from five bytes past the previous match, does not count.
fn count_messages(buf: &[u8]) -> usize {
    // The C code scans a NUL-terminated `strbuf`, so an embedded NUL ends it.
    let buf = match buf.iter().position(|&b| b == 0) {
        Some(n) => &buf[..n],
        None => buf,
    };
    let find = |from: usize, needle: &[u8]| -> Option<usize> {
        if from > buf.len() {
            return None;
        }
        buf[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
    };
    let at = |p: usize, needle: &[u8]| p <= buf.len() && buf[p..].starts_with(needle);

    let mut count = 0;
    let mut p = 0usize;
    loop {
        if at(p, b"From ") {
            if at(p, b"From git-send-email") {
                let Some(i) = find(p + 5, b"\nFrom: ") else { break };
                let Some(j) = find(i + 7, b"\nTo: ") else { break };
                p = j + 5;
            } else {
                let Some(i) = find(p + 5, b"\nFrom: ") else { break };
                let Some(j) = find(i + 7, b"\nDate: ") else { break };
                let Some(k) = find(j + 7, b"\nSubject: ") else { break };
                p = k + 10;
            }
            count += 1;
        }
        let Some(n) = find(p + 5, b"\nFrom ") else { break };
        p = n + 1;
    }
    count
}

/// `split_msg()` (`imap-send.c:1490`) — the next message, minus its `From `
/// line, and where the one after it starts.
fn split_msg(all: &[u8], ofs: &mut usize) -> Option<Vec<u8>> {
    if *ofs >= all.len() {
        return None;
    }
    let mut data = *ofs;
    let mut len = all.len() - data;
    if len < 5 || !all[data..].starts_with(b"From ") {
        return None;
    }
    // Skip the `From ` line itself.
    if let Some(nl) = all[data..].iter().position(|&b| b == b'\n') {
        let step = nl + 1;
        len -= step;
        *ofs += step;
        data += step;
    }
    // The message ends just after the newline that precedes the next `From `.
    if let Some(next) = all[data..].windows(6).position(|w| w == b"\nFrom ") {
        len = next + 1;
    }
    *ofs += len;
    Some(all[data..data + len].to_vec())
}

/// `imap_store_msg()` (`imap-send.c:1404`) — one `APPEND` with the message as a
/// literal.
fn imap_store_msg(store: &mut Store, msg: &[u8]) -> i32 {
    let payload = lf_to_crlf(msg);
    let prefix = if store.name == "INBOX" { "" } else { store.prefix.as_str() };
    let cmd = format!("APPEND \"{prefix}{}\" ", store.name);
    let ret = store.exec(&cmd, Some(payload), None);
    // `imap->caps = imap->rcaps` — an APPEND may have changed them.
    store.caps = store.rcaps;
    ret
}

/// `append_msgs_to_imap()` (`imap-send.c:1568`).
fn append_msgs_to_imap(cfg: &mut ServerConf, all: &[u8], total: usize, verbosity: i32) -> u8 {
    let folder = cfg.folder.clone().unwrap_or_default();
    let Some(mut store) = imap_open_store(cfg, &folder, verbosity, false) else {
        eprintln!("failed to open store");
        return 1;
    };

    eprintln!(
        "Sending {total} message{} to {folder} folder...",
        if total != 1 { "s" } else { "" }
    );
    let mut ofs = 0usize;
    let mut n = 0usize;
    loop {
        eprint!("{:>4}% ({n}/{total}) done\r", n * 100 / total);
        let Some(msg) = split_msg(all, &mut ofs) else { break };
        let msg = if cfg.use_html { wrap_in_html(&msg) } else { msg };
        if imap_store_msg(&mut store, &msg) != RESP_OK {
            break;
        }
        n += 1;
    }
    eprintln!();

    store.close();
    0
}

/// `list_imap_folders()` (`imap-send.c:1607`) — `LIST "" "*"`, whose untagged
/// replies `buffer_gets` has already printed.
fn list_imap_folders(cfg: &mut ServerConf, verbosity: i32) -> u8 {
    let Some(mut store) = imap_open_store(cfg, "INBOX", verbosity, true) else {
        eprintln!("failed to connect to IMAP server");
        return 1;
    };
    eprintln!("Fetching the list of available folders...");
    let rc = if store.exec("LIST \"\" \"*\"", None, None) != RESP_OK {
        eprintln!("failed to list folders");
        1
    } else {
        0
    };
    store.close();
    rc
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

/// `git imap-send` — send a collection of patches from stdin to an IMAP folder.
///
/// `cmd_main()` (`imap-send.c:1792`): read the configuration, parse the command
/// line, apply the port default, check that a host and a folder are known, read
/// and count the messages, then hand them to the IMAP client.
pub fn imap_send(args: &[String]) -> Result<ExitCode> {
    let cfg_file = load_config();
    let mut server = git_imap_config(cfg_file.as_ref());

    let opts = match scan(args) {
        Scan::Help => {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        Scan::Error(msg, with_usage) => {
            eprintln!("{msg}");
            if with_usage {
                eprint!("{USAGE}");
            }
            return Ok(ExitCode::from(129));
        }
        Scan::Ok(opts) => opts,
    };

    // `--folder` overwrites the config value only when it carried one.
    if opts.folder.is_some() {
        server.folder = opts.folder.clone();
    }

    // `if (argc) usage_with_options(...)`, checked after the whole scan.
    if opts.positional {
        return Ok(usage_err());
    }

    // This build has git's own IMAP client and no libcurl, which is the
    // `#ifndef USE_CURL_FOR_IMAP_SEND` arm (`imap-send.c:1815-1819`).
    if opts.curl {
        eprintln!("warning: --curl not supported in this build");
    }

    if server.port == 0 {
        server.port = if server.use_ssl { 993 } else { 143 };
    }

    // Presence, not emptiness: `-c imap.host=` satisfies this.
    if server.host.is_none() {
        if server.tunnel.is_none() {
            eprintln!("error: no IMAP host specified");
            eprintln!("hint: set the IMAP host with 'git config imap.host <host>'.");
            eprintln!("hint: (e.g., 'git config imap.host imaps://imap.example.com')");
            return Ok(ExitCode::from(1));
        }
        server.host = Some("tunnel".into());
    }

    if opts.list {
        return Ok(ExitCode::from(list_imap_folders(&mut server, opts.verbosity)));
    }

    if server.folder.is_none() {
        eprintln!("error: no IMAP folder specified");
        eprintln!("hint: set the target folder with 'git config imap.folder <folder>'.");
        eprintln!("hint: (e.g., 'git config imap.folder Drafts')");
        return Ok(ExitCode::from(1));
    }

    let mut mbox = Vec::new();
    std::io::stdin().lock().read_to_end(&mut mbox)?;

    if mbox.is_empty() {
        eprintln!("nothing to send");
        return Ok(ExitCode::from(1));
    }
    let total = count_messages(&mbox);
    if total == 0 {
        eprintln!("no messages found to send");
        return Ok(ExitCode::from(1));
    }

    Ok(ExitCode::from(append_msgs_to_imap(&mut server, &mbox, total, opts.verbosity)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest the stock binary sent when it authenticated to a local server
    /// with challenge `<1896.697170952@example.com>` and password `s3cret`.
    #[test]
    fn cram_md5_matches_the_digest_stock_git_sent() {
        let challenge = "<1896.697170952@example.com>";
        assert_eq!(
            hex(&hmac_md5(b"s3cret", challenge.as_bytes())),
            "c1dc8f79994fb600d7d1e1cf224962c5"
        );
    }

    /// RFC 1321's own test vectors, so the digest is not just self-consistent.
    #[test]
    fn md5_matches_rfc1321_test_vectors() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&md5(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890")),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    /// RFC 2202's HMAC-MD5 vectors, including the key longer than the block
    /// size that has to be hashed down first.
    #[test]
    fn hmac_md5_matches_rfc2202_test_vectors() {
        assert_eq!(hex(&hmac_md5(&[0x0b; 16], b"Hi There")), "9294727a3638bb1c13f48ef8158bfc9d");
        assert_eq!(
            hex(&hmac_md5(b"Jefe", b"what do ya want for nothing?")),
            "750c783e6ab0b503eaa86e310a5db738"
        );
        assert_eq!(
            hex(&hmac_md5(&[0xaa; 80], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "6b1ab7fe4bd7bf8f0b62e6ce61b9d0cd"
        );
    }

    /// `imap.host` carries a scheme, and `imaps:` is what turns TLS on and
    /// moves the default port.
    #[test]
    fn host_scheme_selects_tls_and_is_stripped() {
        let cfg = ConfigFile::try_from("[imap]\nhost = imaps://mail.example.com\n").unwrap();
        let server = git_imap_config(Some(&cfg));
        assert_eq!(server.host.as_deref(), Some("mail.example.com"));
        assert!(server.use_ssl);

        let cfg = ConfigFile::try_from("[imap]\nhost = imap://mail.example.com\n").unwrap();
        let server = git_imap_config(Some(&cfg));
        assert_eq!(server.host.as_deref(), Some("mail.example.com"));
        assert!(!server.use_ssl);
    }

    /// The three booleans and their defaults.
    #[test]
    fn booleans_and_their_defaults() {
        let server = git_imap_config(Some(&ConfigFile::try_from("[imap]\nhost = h\n").unwrap()));
        assert!(server.ssl_verify, "imap.sslverify defaults to true");
        assert!(!server.use_html, "imap.preformattedHTML defaults to false");

        let cfg = ConfigFile::try_from("[imap]\nsslverify = false\npreformattedHTML = true\n").unwrap();
        let server = git_imap_config(Some(&cfg));
        assert!(!server.ssl_verify);
        assert!(server.use_html);
    }

    /// Every remaining string key, and the port.
    #[test]
    fn strings_and_port_reach_the_server_config() {
        let cfg = ConfigFile::try_from(
            "[imap]\nuser = alice\npass = s3cret\nport = 1143\nfolder = Drafts\ntunnel = ssh h imapd\nauthMethod = CRAM-MD5\n",
        )
        .unwrap();
        let server = git_imap_config(Some(&cfg));
        assert_eq!(server.user.as_deref(), Some("alice"));
        assert_eq!(server.pass.as_deref(), Some("s3cret"));
        assert_eq!(server.port, 1143);
        assert_eq!(server.folder.as_deref(), Some("Drafts"));
        assert_eq!(server.tunnel.as_deref(), Some("ssh h imapd"));
        assert_eq!(server.auth_method.as_deref(), Some("CRAM-MD5"));
    }

    /// `next_arg` over a status line: the tag, the status, and the quoted word
    /// that keeps its spaces.
    #[test]
    fn next_arg_splits_on_whitespace_and_quotes() {
        let line = "* LIST (\\HasNoChildren) \"/\" \"My Folder\"";
        let mut cur = Some(line);
        assert_eq!(next_arg(&mut cur), Some("*"));
        assert_eq!(next_arg(&mut cur), Some("LIST"));
        assert_eq!(next_arg(&mut cur), Some("(\\HasNoChildren)"));
        assert_eq!(next_arg(&mut cur), Some("/"));
        assert_eq!(next_arg(&mut cur), Some("My Folder"));
        assert_eq!(next_arg(&mut cur), None);
    }

    /// `lf_to_crlf` is what makes the literal byte count what the server is
    /// told: the sample mbox body is 138 bytes with six newlines, and stock
    /// git announced `{144}` for it.
    #[test]
    fn lf_to_crlf_only_adds_missing_carriage_returns() {
        assert_eq!(lf_to_crlf(b"a\nb\r\nc\n"), b"a\r\nb\r\nc\r\n".to_vec());
        let body = b"From: a\nDate: b\nSubject: c\n\nline\n";
        assert_eq!(lf_to_crlf(body).len(), body.len() + 5);
    }

    /// `wrap_in_html` quotes the body and leaves a header-only message alone.
    #[test]
    fn wrap_in_html_quotes_the_body_only() {
        let msg = b"From: a\n\n<b> & \"c\"\n";
        let out = String::from_utf8(wrap_in_html(msg)).unwrap();
        assert_eq!(
            out,
            "From: a\nContent-Type: text/html;\n\n<pre>\n&lt;b&gt; &amp; &quot;c&quot;\n</pre>\n"
        );
        assert_eq!(wrap_in_html(b"From: a\n"), b"From: a\n".to_vec());
    }

    /// `split_msg` drops the `From ` line and stops at the next one.
    #[test]
    fn split_msg_splits_on_from_lines() {
        let all = b"From x\nFrom: a\nbody\nFrom y\nFrom: b\nmore\n";
        let mut ofs = 0;
        assert_eq!(split_msg(all, &mut ofs).unwrap(), b"From: a\nbody\n".to_vec());
        assert_eq!(split_msg(all, &mut ofs).unwrap(), b"From: b\nmore\n".to_vec());
        assert!(split_msg(all, &mut ofs).is_none());
    }
}
