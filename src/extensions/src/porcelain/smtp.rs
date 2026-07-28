//! The socket transport of `git send-email` — a port of the `Net::SMTP` branch
//! of `git-send-email.perl`'s `send_message`, together with the pieces of
//! `Net::SMTP`, `Net::Cmd`, `IO::Socket::SSL` and `Authen::SASL` that branch
//! reaches.
//!
//! ### What is ported
//!
//! * `Net::SMTP->new`: connect (a `host:port` in `--smtp-server` overrides
//!   `--smtp-server-port`, as `IO::Socket::INET`'s `PeerAddr` does), read the
//!   greeting, then `hello($smtp_domain)` — `EHLO`, falling back to `HELO` on a
//!   5xx, with the ESMTP capability table built from the multi-line reply by
//!   `Net::SMTP::hello`'s `/([-\w]+)\b[= \t]*([^\n]*)/`.
//! * `$smtp_encryption`: `ssl` opens TLS from the first byte on port 465, `tls`
//!   connects in the clear, issues `STARTTLS`, hands the socket to TLS and says
//!   `EHLO` again (`Net::SMTP::starttls`). Any other value — including the empty
//!   string, which is what `git-send-email.perl:606` sets when nothing
//!   configured one — leaves the session in the clear on port 25. The script
//!   has no validation of this variable and no `else` branch (see its lines
//!   1763 and 1798, and the `--smtp-encryption` usage line: "tls or ssl;
//!   anything else disables"), so neither has this.
//! * `ssl_verify_params()`: no CA path means the platform trust store (git
//!   leaves OpenSSL on its defaults); an empty CA path disables verification
//!   (`SSL_VERIFY_NONE`); a directory becomes `SSL_ca_path` and a file
//!   `SSL_ca_file`; anything else is the `CA path "%s" does not exist` fatal.
//!   The client certificate becomes `SSL_cert_file` and the client key
//!   `SSL_key_file`, with the `Only client key "%s" specified` fatal when a key
//!   is given without a certificate. A certificate given without a key is read
//!   for its key as well, which is what `IO::Socket::SSL` does when
//!   `SSL_key_file` is unset.
//! * `Net::Cmd`'s framing: `response()` (multi-line continuations on `<code>-`,
//!   the reply text kept with the three-digit code and its separator stripped
//!   the way `Net::SMTP::parse_response` strips them in place, which is what
//!   makes `$smtp->message` the text alone), `datasend()` (LF to CRLF, and a
//!   line-leading `.` doubled), `dataend()` (a closing CRLF when the payload
//!   did not end in one, then `.` CRLF) and `debug_print`'s `>>> `/`<<< `
//!   trace under `--smtp-debug`.
//! * `smtp_auth_maybe()`: skipped when no user is known, when a previous
//!   message on this session already authenticated, or when `--smtp-auth=none`;
//!   the `^(\b[A-Z0-9-_]{1,20}\s*)*$` mechanism-name check; the credential
//!   round trip (`git credential fill`, then `approve` or `reject` on the
//!   outcome) with `protocol=smtp` and `smtp_host_string()` as the host; and
//!   `handle_smtp_error`'s classification of a transport failure by SMTP status
//!   class.
//! * `Net::SMTP::mail`/`recipient`/`data`/`quit`, including `_addr`'s
//!   `<>`-wrapping and `recipient`'s stop-at-the-first-rejection.
//! * `maildomain()`: `Net::Domain::domainname`, then a probe of `mailhost` and
//!   `localhost` for an MTA whose greeting names a domain, then `hostname -f`,
//!   then `localhost.localdomain`, each filtered through `valid_fqdn` (which
//!   rejects a `.local` name on Darwin).
//!
//! ### What is not ported
//!
//! * SASL mechanisms other than `PLAIN` and `LOGIN`. `CRAM-MD5`, `DIGEST-MD5`,
//!   `GSSAPI`, `XOAUTH2` and the rest are what `Authen::SASL` would add; asking
//!   for one through `--smtp-auth` is a hard error naming the two that work
//!   rather than a silent downgrade.
//! * `Authen::SASL`'s preference order between mechanisms of equal rank. With
//!   no `--smtp-auth` the mechanism is the first one this module implements in
//!   the order the server advertised it; with `--smtp-auth`, the first in the
//!   order the user spelled it. `Authen::SASL::Perl` ranks `PLAIN` and `LOGIN`
//!   equally and breaks the tie by list order, so this agrees with it whenever
//!   only those two are in play.
//! * The `Net::SMTP=GLOB(0x…)` prefix on the `--smtp-debug` trace: the lines
//!   are `Net::SMTP>>> ` and `Net::SMTP<<< `, since the glob address is a heap
//!   address of the Perl interpreter.
//! * `git credential`'s failure text. `Git.pm` reports a helper that exits
//!   non-zero through `command_close_pipe`; here it is `git credential <op>
//!   failed`.
//! * An `SSL_ca_path` directory is read whole rather than by OpenSSL's
//!   subject-hash lookup, so every certificate in it is trusted rather than
//!   only the ones whose `<hash>.<n>` link exists.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, Error as TlsError, RootCertStore,
    SignatureScheme, StreamOwned,
};
use rustls_pki_types::pem::PemObject;

use super::send_email::{encode_base64, port_num, self_exe};

/// `Timeout` in the `Net::SMTP->new` argument list, which defaults to 120.
const TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// TLS
// ---------------------------------------------------------------------------

/// The three `%config_path_settings` entries `ssl_verify_params()` reads.
#[derive(Default, Clone)]
pub(crate) struct Ssl {
    /// `sendemail.smtpSSLCertPath` / `--smtp-ssl-cert-path`. `None` is Perl's
    /// `undef`; `Some("")` is the explicit empty string, which turns
    /// verification off.
    pub cert_path: Option<String>,
    /// `sendemail.smtpSSLClientCert` / `--smtp-ssl-client-cert`.
    pub client_cert: Option<String>,
    /// `sendemail.smtpSSLClientKey` / `--smtp-ssl-client-key`.
    pub client_key: Option<String>,
}

/// A `ServerCertVerifier` that accepts anything — `SSL_VERIFY_NONE`, which is
/// what an empty `smtpSSLCertPath` asks for.
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

/// Every certificate in a PEM file, for `SSL_ca_file`.
fn roots_from_file(path: &Path, roots: &mut RootCertStore) -> Result<(), String> {
    let iter = CertificateDer::pem_file_iter(path)
        .map_err(|e| format!("failed to read CA file {}: {e}\n", path.display()))?;
    for cert in iter {
        let cert = cert.map_err(|e| format!("failed to read CA file {}: {e}\n", path.display()))?;
        roots.add(cert).map_err(|e| format!("{}: {e}\n", path.display()))?;
    }
    Ok(())
}

impl Ssl {
    /// `ssl_verify_params()` — the `IO::Socket::SSL` options, as the rustls
    /// client configuration they translate into.
    pub(crate) fn client_config(&self) -> Result<Arc<ClientConfig>, String> {
        // git validates the client cert/key pair in `ssl_verify_params()` before
        // it hands anything to the TLS layer, so a key without its certificate is
        // reported as such even on a host whose trust store cannot be read. Doing
        // it after the root-store build would mask this fatal behind whatever
        // `load_native_certs()` happened to fail with.
        if self.client_cert.is_none() {
            if let Some(key) = &self.client_key {
                return Err(format!("Only client key \"{key}\" specified\n"));
            }
        }

        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("{e}\n"))?;

        let builder = match self.cert_path.as_deref() {
            // "use the OpenSSL defaults" — the platform trust store.
            None => {
                let mut roots = RootCertStore::empty();
                let native = rustls_native_certs::load_native_certs();
                for cert in native.certs {
                    roots.add(cert).ok();
                }
                if roots.is_empty() {
                    let first = native.errors.first();
                    return Err(match first {
                        Some(e) => format!("no trusted CA certificates found: {e}\n"),
                        None => "no trusted CA certificates found\n".to_string(),
                    });
                }
                builder.with_root_certificates(roots)
            }
            Some("") => builder.dangerous().with_custom_certificate_verifier(Arc::new(NoVerify {
                schemes: provider.signature_verification_algorithms.supported_schemes(),
            })),
            Some(p) => {
                let path = Path::new(p);
                let mut roots = RootCertStore::empty();
                if path.is_dir() {
                    let dir = std::fs::read_dir(path)
                        .map_err(|e| format!("failed to read CA path {p}: {e}\n"))?;
                    let mut entries: Vec<std::path::PathBuf> =
                        dir.flatten().map(|e| e.path()).collect();
                    entries.sort();
                    for entry in entries.iter().filter(|e| e.is_file()) {
                        roots_from_file(entry, &mut roots)?;
                    }
                } else if path.is_file() {
                    roots_from_file(path, &mut roots)?;
                    // `pem_file_iter` yields nothing rather than failing when the
                    // file holds no PEM blocks, so a file named explicitly as
                    // `SSL_ca_file` would otherwise build a config trusting
                    // nothing at all. `IO::Socket::SSL` treats that as fatal, and
                    // silently verifying against an empty store is worse than
                    // saying so.
                    if roots.is_empty() {
                        return Err(format!(
                            "failed to read CA file {}: no certificates found\n",
                            path.display()
                        ));
                    }
                } else {
                    return Err(format!("CA path \"{p}\" does not exist\n"));
                }
                builder.with_root_certificates(roots)
            }
        };

        match (&self.client_cert, &self.client_key) {
            (None, None) => Ok(Arc::new(builder.with_no_client_auth())),
            (None, Some(key)) => Err(format!("Only client key \"{key}\" specified\n")),
            (Some(cert), key) => {
                let cert_path = Path::new(cert);
                let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_path)
                    .and_then(|it| it.collect())
                    .map_err(|e| format!("failed to read client certificate {cert}: {e}\n"))?;
                // `SSL_key_file` defaults to `SSL_cert_file` in IO::Socket::SSL.
                let key_path = key.clone().unwrap_or_else(|| cert.clone());
                let key_der = PrivateKeyDer::from_pem_file(&key_path)
                    .map_err(|e| format!("failed to read client key {key_path}: {e}\n"))?;
                let cfg = builder
                    .with_client_auth_cert(chain, key_der)
                    .map_err(|e| format!("{e}\n"))?;
                Ok(Arc::new(cfg))
            }
        }
    }
}

/// The socket, in the clear or under TLS.
enum Transport {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Transport::Plain(s) => s.read(buf),
            Transport::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Transport::Plain(s) => s.write(buf),
            Transport::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Transport::Plain(s) => s.flush(),
            Transport::Tls(s) => s.flush(),
        }
    }
}

// ---------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------

/// Why `Net::SMTP->new` (or `starttls`) came up short.
pub(crate) enum ConnectError {
    /// `Net::SMTP->new` returned `undef`; the caller raises the script's
    /// `Unable to initialize SMTP properly` fatal, which spells out the
    /// settings itself.
    Undef,
    /// A fatal with its own message: `ssl_verify_params`'s two, and
    /// `STARTTLS failed!`.
    Die(String),
}

/// The `Net::SMTP->new` argument list, assembled by `send_message`.
pub(crate) struct Connect<'a> {
    /// `$smtp_server`, possibly `host:port`.
    pub server: &'a str,
    /// `$smtp_server_port` after the `||= 465`/`||= 25` default; a service name
    /// is resolved the way `IO::Socket::INET` resolves one.
    pub port: &'a str,
    /// `Hello => $smtp_domain`.
    pub domain: &'a str,
    /// `$smtp_encryption`.
    pub encryption: &'a str,
    /// What `ssl_verify_params()` reads.
    pub ssl: &'a Ssl,
    /// `Debug => $debug_net_smtp`.
    pub debug: bool,
}

/// A live `Net::SMTP` object: the socket, the reply of the last command, and
/// the ESMTP capabilities of the last `EHLO`.
pub(crate) struct Session {
    io: Option<Transport>,
    /// Bytes read but not yet consumed as a line.
    pending: Vec<u8>,
    /// `net_cmd_code`, which starts at `DEF_REPLY_CODE` (500 for SMTP).
    code: u32,
    /// `net_cmd_resp`: the reply lines with the code and its separator
    /// stripped, each ending in `\n`.
    resp: Vec<String>,
    /// `net_smtp_esmtp`, upper-cased keyword to argument text.
    esmtp: BTreeMap<String, String>,
    /// `net_cmd_last_ch`, which `datasend`/`dataend` use to know whether the
    /// payload so far ended on a newline.
    last_ch: u8,
    /// `Debug`.
    debug: bool,
    /// `$auth` — set once a message on this session has authenticated, and
    /// cleared with the session by `--batch-size`.
    pub authenticated: bool,
}

impl Session {
    /// `Net::SMTP->new($smtp_server, Hello => …, Port => …, SSL => …)` — with
    /// the `starttls` that `send_message` runs straight afterwards for
    /// `$smtp_encryption eq 'tls'`.
    pub(crate) fn connect(cfg: &Connect<'_>) -> Result<Session, ConnectError> {
        let (host, port) = split_host_port(cfg.server, cfg.port);
        // `IO::Socket::INET` takes a service name as readily as a number.
        let port = port_num(&port).unwrap_or(port);

        // ssl_verify_params() runs before the socket does, so its two fatals
        // fire even when nothing is listening.
        let tls_config = match cfg.encryption {
            "ssl" | "tls" => Some(cfg.ssl.client_config().map_err(ConnectError::Die)?),
            _ => None,
        };

        let addr = format!("{host}:{port}");
        let mut sock = None;
        for target in addr.to_socket_addrs().map_err(|_| ConnectError::Undef)? {
            if let Ok(s) = TcpStream::connect_timeout(&target, TIMEOUT) {
                sock = Some(s);
                break;
            }
        }
        // `Net::SMTP->new` reports nothing but `undef` when the socket refuses
        // to open; the caller turns that into the script's fatal.
        let sock = sock.ok_or(ConnectError::Undef)?;
        sock.set_read_timeout(Some(TIMEOUT)).ok();
        sock.set_write_timeout(Some(TIMEOUT)).ok();

        let mut session = Session {
            io: Some(Transport::Plain(sock)),
            pending: Vec::new(),
            code: 500,
            resp: Vec::new(),
            esmtp: BTreeMap::new(),
            last_ch: b'\n',
            debug: cfg.debug,
            authenticated: false,
        };

        if cfg.encryption == "ssl" {
            let config = tls_config.clone().expect("ssl builds a config");
            session.upgrade(&host, config).map_err(|_| ConnectError::Undef)?;
        }

        // The greeting, then EHLO.
        if session.response() != 2 {
            return Err(ConnectError::Undef);
        }
        if !session.hello(cfg.domain) {
            return Err(ConnectError::Undef);
        }

        if cfg.encryption == "tls" {
            // `Net::SMTP::starttls`: the command, the handshake, then another
            // EHLO to pick up the capabilities the server offers once secured.
            if session.cmd("STARTTLS") != 2 {
                return Err(ConnectError::Die(format!(
                    "Server does not support STARTTLS! {}",
                    session.message()
                )));
            }
            let config = tls_config.expect("tls builds a config");
            if let Err(e) = session.upgrade(&host, config) {
                return Err(ConnectError::Die(format!("STARTTLS failed! {e}\n")));
            }
            if !session.hello(cfg.domain) {
                return Err(ConnectError::Undef);
            }
        }

        Ok(session)
    }

    /// Hand the socket to TLS, keeping the session's buffers.
    fn upgrade(&mut self, host: &str, config: Arc<ClientConfig>) -> Result<(), String> {
        let name = ServerName::try_from(host.to_string()).map_err(|e| format!("{e}"))?;
        let conn = ClientConnection::new(config, name).map_err(|e| format!("{e}"))?;
        let sock = match self.io.take() {
            Some(Transport::Plain(s)) => s,
            other => {
                self.io = other;
                return Err("socket is already secured".into());
            }
        };
        let mut stream = StreamOwned::new(conn, sock);
        // Drive the handshake now so a certificate failure is reported here
        // rather than on the next command.
        stream.flush().map_err(|e| format!("{e}"))?;
        while stream.conn.is_handshaking() {
            stream.conn.complete_io(&mut stream.sock).map_err(|e| format!("{e}"))?;
        }
        self.io = Some(Transport::Tls(Box::new(stream)));
        Ok(())
    }

    /// `Net::SMTP::hello` — `EHLO`, falling back to `HELO`, then the ESMTP
    /// capability table out of the reply.
    fn hello(&mut self, domain: &str) -> bool {
        let domain = if domain.is_empty() { "localhost.localdomain" } else { domain };
        let mut ok = self.cmd(&format!("EHLO {domain}")) == 2;
        if ok {
            self.esmtp.clear();
            let lines = self.resp.clone();
            for line in &lines {
                if let Some((k, v)) = esmtp_keyword(line) {
                    self.esmtp.insert(k, v);
                }
            }
        } else if self.code / 100 == 5 {
            ok = self.cmd(&format!("HELO {domain}")) == 2;
        }
        ok
    }

    /// `Net::SMTP::supports` — the argument text of an advertised keyword.
    fn supports(&self, keyword: &str) -> Option<&str> {
        self.esmtp.get(keyword).map(String::as_str)
    }

    /// `Net::SMTP::domain` — the first word of the `EHLO` reply, which
    /// `maildomain_mta` asks for.
    fn domain(&self) -> Option<String> {
        let first = self.resp.first()?;
        first.split_ascii_whitespace().next().map(str::to_string)
    }

    /// `net_cmd_code`.
    pub(crate) fn code(&self) -> u32 {
        self.code
    }

    /// `$smtp->message` in scalar context: the reply text with the codes
    /// stripped.
    pub(crate) fn message(&self) -> String {
        self.resp.concat()
    }

    /// `set_status`, which `Net::SMTP` uses to report a client-side refusal
    /// through the same accessors a server reply would.
    fn set_status(&mut self, code: u32, message: &str) {
        self.code = code;
        self.resp = vec![format!("{message}\n")];
    }

    /// `Net::Cmd::debug_print`.
    fn trace(&self, out: bool, text: &str) {
        if self.debug {
            eprint!("Net::SMTP{} {text}", if out { ">>>" } else { "<<<" });
        }
    }

    /// Raw bytes onto the socket, with no framing of any kind.
    fn put(&mut self, bytes: &[u8]) -> bool {
        let Some(io) = self.io.as_mut() else { return false };
        if io.write_all(bytes).is_err() || io.flush().is_err() {
            self.io = None;
            return false;
        }
        true
    }

    /// `Net::Cmd::getline` — one reply line, CRLF normalised to `\n`.
    fn getline(&mut self) -> Option<String> {
        loop {
            if let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.pending.drain(..=pos).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let mut s = String::from_utf8_lossy(&line).into_owned();
                s.push('\n');
                return Some(s);
            }
            let mut buf = [0u8; 1024];
            let n = match self.io.as_mut()?.read(&mut buf) {
                Ok(0) | Err(_) => {
                    self.io = None;
                    return None;
                }
                Ok(n) => n,
            };
            self.pending.extend_from_slice(&buf[..n]);
        }
    }

    /// `Net::Cmd::response` — read one reply, following `<code>-` continuation
    /// lines, and return its leading digit (0 when the socket failed).
    fn response(&mut self) -> u32 {
        self.code = 500;
        self.resp.clear();
        loop {
            let Some(line) = self.getline() else {
                self.code = 599;
                return 0;
            };
            self.trace(false, &line);
            // `Net::SMTP::parse_response` strips the code and the separator
            // from the line it was handed, which is the line that gets kept.
            let bytes = line.as_bytes();
            if bytes.len() < 3 || !bytes[..3].iter().all(u8::is_ascii_digit) {
                self.code = 599;
                return 0;
            }
            let code: u32 = line[..3].parse().unwrap_or(500);
            let more = bytes.get(3) == Some(&b'-');
            let rest = if bytes.len() > 3 { &line[4..] } else { &line[3..] };
            self.code = code;
            self.resp.push(rest.to_string());
            if !more {
                return code / 100;
            }
        }
    }

    /// `Net::Cmd::command` + `response`: send one line, read one reply, return
    /// the reply's leading digit.
    fn cmd(&mut self, line: &str) -> u32 {
        let text = format!("{line}\r\n");
        self.trace(true, &format!("{line}\n"));
        if !self.put(text.as_bytes()) {
            self.code = 599;
            self.resp = vec!["Connection closed\n".into()];
            return 0;
        }
        self.response()
    }

    /// `Net::SMTP::mail` — `MAIL FROM:<addr>`.
    pub(crate) fn mail(&mut self, addr: &str) -> bool {
        let line = format!("MAIL FROM:{}", wrap_addr(addr));
        self.cmd(&line) == 2
    }

    /// `Net::SMTP::recipient` — one `RCPT TO:<addr>` per recipient, stopping at
    /// the first the server refuses.
    pub(crate) fn recipients(&mut self, addrs: &[String]) -> bool {
        for addr in addrs {
            let line = format!("RCPT TO:{}", wrap_addr(addr));
            if self.cmd(&line) != 2 {
                return false;
            }
        }
        true
    }

    /// `Net::SMTP::data` with no payload — the `DATA` command alone, which the
    /// server answers with 354.
    pub(crate) fn data(&mut self) -> bool {
        self.cmd("DATA") == 3
    }

    /// `Net::Cmd::datasend` — LF becomes CRLF and a line-leading `.` is
    /// doubled.
    pub(crate) fn datasend(&mut self, payload: &[u8]) -> bool {
        if payload.is_empty() {
            return true;
        }
        if self.debug {
            for line in payload.split(|&b| b == b'\n') {
                if !line.is_empty() {
                    self.trace(true, &format!("{}\n", String::from_utf8_lossy(line)));
                }
            }
        }
        // `net_cmd_last_ch eq "\012"` means the previous chunk ended a line, so
        // a `.` opening this one is at the start of a line too.
        let out = dot_stuff(payload, self.last_ch == b'\n');
        self.last_ch = *payload.last().expect("non-empty");
        self.put(&out)
    }

    /// `Net::Cmd::dataend` — close the last line if it is open, then the lone
    /// dot, then the reply.
    pub(crate) fn dataend(&mut self) -> bool {
        let mut tosend: Vec<u8> = Vec::new();
        if self.last_ch != b'\n' {
            tosend.extend_from_slice(b"\r\n");
        }
        tosend.extend_from_slice(b".\r\n");
        self.trace(true, ".\n");
        if !self.put(&tosend) {
            self.code = 599;
            self.resp = vec!["Connection closed\n".into()];
            return false;
        }
        self.last_ch = b'\n';
        self.response() == 2
    }

    /// `$smtp->quit` — `QUIT`, then drop the socket.
    pub(crate) fn quit(mut self) {
        if self.io.is_some() {
            self.cmd("QUIT");
        }
        self.io = None;
    }
}

/// `Net::Cmd::datasend`'s two substitutions: `s/\015?\012/\015\012/` and the
/// `.` doubling at the start of every line.
fn dot_stuff(payload: &[u8], mut at_line_start: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + payload.len() / 8);
    let mut i = 0;
    while i < payload.len() {
        let b = payload[i];
        // Only a CR that precedes LF is folded into the line ending; a lone CR
        // is data.
        if b == b'\r' && payload.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        match b {
            b'\n' => {
                out.extend_from_slice(b"\r\n");
                at_line_start = true;
            }
            b'.' if at_line_start => {
                out.extend_from_slice(b"..");
                at_line_start = false;
            }
            _ => {
                out.push(b);
                at_line_start = false;
            }
        }
        i += 1;
    }
    out
}

/// `Net::SMTP::hello`'s `/([-\w]+)\b[= \t]*([^\n]*)/` over one reply line.
fn esmtp_keyword(line: &str) -> Option<(String, String)> {
    let line = line.trim_end_matches('\n');
    let bytes = line.as_bytes();
    let start = bytes.iter().position(|&b| b == b'-' || b.is_ascii_alphanumeric() || b == b'_')?;
    let end = start
        + bytes[start..]
            .iter()
            .position(|&b| !(b == b'-' || b.is_ascii_alphanumeric() || b == b'_'))
            .unwrap_or(bytes.len() - start);
    let keyword = line[start..end].to_ascii_uppercase();
    let rest = line[end..].trim_start_matches([' ', '\t', '=']);
    Some((keyword, rest.to_string()))
}

/// `Net::SMTP::_addr` — an address already in angle brackets is taken as it is,
/// anything else is trimmed and wrapped.
fn wrap_addr(addr: &str) -> String {
    if let Some(open) = addr.find('<') {
        if let Some(close) = addr[open..].find('>') {
            return addr[open..open + close + 1].to_string();
        }
    }
    format!("<{}>", addr.trim_matches(|c: char| c.is_ascii_whitespace()))
}

/// `IO::Socket::INET`'s `PeerAddr`: a `host:port` names its own port, and the
/// `Port` argument is only the default. An IPv6 literal (more than one colon)
/// is left alone.
fn split_host_port(server: &str, default_port: &str) -> (String, String) {
    if server.matches(':').count() == 1 {
        if let Some((host, port)) = server.split_once(':') {
            if !host.is_empty() && !port.is_empty() {
                return (host.to_string(), port.to_string());
            }
        }
    }
    (server.to_string(), default_port.to_string())
}

// ---------------------------------------------------------------------------
// maildomain
// ---------------------------------------------------------------------------

/// `valid_fqdn` — dotted labels of 1..=63 characters that neither open nor
/// close on a hyphen, and never a `.local` name on Darwin.
fn valid_fqdn(domain: &str) -> bool {
    if domain.is_empty() {
        return false;
    }
    if cfg!(target_os = "macos") && domain.ends_with(".local") {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// `maildomain_net` — `Net::Domain::domainname()`, i.e. the canonical name the
/// resolver has for this host.
fn maildomain_net() -> Option<String> {
    let host = super::send_email::hostname();
    let canonical = canonical_name(&host)?;
    valid_fqdn(&canonical).then_some(canonical)
}

/// `getaddrinfo(host, NULL, {ai_flags: AI_CANONNAME})`, which is what
/// `Net::Domain` ends up asking the resolver for.
fn canonical_name(host: &str) -> Option<String> {
    let c_host = std::ffi::CString::new(host).ok()?;
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_flags = libc::AI_CANONNAME;
    hints.ai_family = libc::AF_UNSPEC;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let rc = unsafe { libc::getaddrinfo(c_host.as_ptr(), std::ptr::null(), &hints, &mut res) };
    if rc != 0 || res.is_null() {
        return None;
    }
    let name = unsafe {
        let canon = (*res).ai_canonname;
        let out = if canon.is_null() {
            None
        } else {
            std::ffi::CStr::from_ptr(canon).to_str().ok().map(str::to_string)
        };
        libc::freeaddrinfo(res);
        out
    };
    name.filter(|n| !n.is_empty())
}

/// `maildomain_mta` — ask a local MTA what it calls itself.
fn maildomain_mta() -> Option<String> {
    for host in ["mailhost", "localhost"] {
        let ssl = Ssl::default();
        let cfg = Connect {
            server: host,
            port: "25",
            domain: "localhost.localdomain",
            encryption: "",
            ssl: &ssl,
            debug: false,
        };
        if let Ok(session) = Session::connect(&cfg) {
            let domain = session.domain();
            session.quit();
            if let Some(d) = domain {
                if valid_fqdn(&d) {
                    return Some(d);
                }
            }
        }
    }
    None
}

/// `maildomain_hostname_command` — `(hostname -f) 2>/dev/null`, on Linux and
/// Darwin only.
fn maildomain_hostname_command() -> Option<String> {
    if !(cfg!(target_os = "linux") || cfg!(target_os = "macos")) {
        return None;
    }
    let out = std::process::Command::new("hostname").arg("-f").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let domain = String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string();
    valid_fqdn(&domain).then_some(domain)
}

/// `maildomain()` — the name `EHLO` announces when nothing configured one.
pub(crate) fn maildomain() -> String {
    maildomain_net()
        .or_else(maildomain_mta)
        .or_else(maildomain_hostname_command)
        .unwrap_or_else(|| "localhost.localdomain".to_string())
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

/// The SASL mechanisms this module speaks, in no particular order — the
/// candidate list decides which one is used.
const MECHANISMS: &[&str] = &["PLAIN", "LOGIN"];

/// A credential description, in the four fields `smtp_auth_maybe` fills in.
struct Credential {
    protocol: String,
    host: String,
    username: Option<String>,
    password: Option<String>,
}

impl Credential {
    /// `Git::_credential_write` — `key=value` lines in the order `Git.pm` sorts
    /// them (`url` first, then alphabetically), terminated by a blank line.
    fn encode(&self) -> String {
        let mut out = String::new();
        let mut push = |key: &str, value: Option<&str>| {
            if let Some(v) = value {
                if !v.contains(['\n', '\0']) {
                    out.push_str(&format!("{key}={v}\n"));
                }
            }
        };
        push("host", Some(&self.host));
        push("password", self.password.as_deref());
        push("protocol", Some(&self.protocol));
        push("username", self.username.as_deref());
        out.push('\n');
        out
    }

    /// `Git::_credential_read` — every `key=value` line of the reply updates
    /// the description.
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

    /// `Git::_credential_run` — one `git credential <op>` round trip.
    fn run(&mut self, op: &str) -> Result<(), String> {
        let mut child = std::process::Command::new(self_exe())
            .args(["credential", op])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("git credential {op} failed: {e}\n"))?;
        {
            let stdin = child.stdin.as_mut().ok_or("git credential: no stdin\n")?;
            stdin
                .write_all(self.encode().as_bytes())
                .map_err(|e| format!("git credential {op} failed: {e}\n"))?;
        }
        drop(child.stdin.take());
        let out =
            child.wait_with_output().map_err(|e| format!("git credential {op} failed: {e}\n"))?;
        if !out.status.success() {
            return Err(format!("git credential {op} failed\n"));
        }
        if op == "fill" {
            self.absorb(&String::from_utf8_lossy(&out.stdout));
        }
        Ok(())
    }
}

/// `handle_smtp_error` — classify a transport failure by its SMTP status class,
/// so that a server having a bad day does not get the credential rejected.
fn handle_smtp_error(error: &str) -> bool {
    let status = error
        .split(|c: char| !c.is_ascii_digit())
        .find(|w| w.len() == 3)
        .and_then(|w| w.parse::<u32>().ok());
    match status {
        Some(code) if (400..500).contains(&code) => {
            eprintln!("SMTP transient error (status code {code}): {error}");
            true
        }
        Some(code) if (500..600).contains(&code) => {
            eprintln!("SMTP permanent error (status code {code}): {error}");
            false
        }
        Some(_) => {
            eprintln!("SMTP unknown error: {error}. Treating as transient failure.");
            true
        }
        None => {
            eprintln!("SMTP generic error: {error}");
            true
        }
    }
}

/// The `%config_settings` entries `smtp_auth_maybe` reads.
pub(crate) struct Auth<'a> {
    /// `$smtp_authuser`.
    pub user: Option<&'a str>,
    /// `$smtp_authpass`.
    pub pass: Option<&'a str>,
    /// `$smtp_auth` — the allowed mechanisms, or `none`.
    pub mechanisms: Option<&'a str>,
    /// `smtp_host_string()`.
    pub host: String,
}

impl Session {
    /// `smtp_auth_maybe` — 1 if authentication succeeded or was unnecessary, 0
    /// otherwise. A `Err` is one of the script's own fatals.
    pub(crate) fn auth_maybe(&mut self, auth: &Auth<'_>) -> Result<bool, String> {
        if auth.user.is_none() || self.authenticated || auth.mechanisms == Some("none") {
            return Ok(true);
        }
        // "Check mechanism naming as defined in RFC 4422".
        if let Some(m) = auth.mechanisms.filter(|m| !m.is_empty()) {
            if !valid_mechanism_list(m) {
                return Err(format!("invalid smtp auth: '{m}'\n"));
            }
        }

        let mut cred = Credential {
            protocol: "smtp".into(),
            host: auth.host.clone(),
            username: auth.user.map(str::to_string),
            // "if there's no password, `git credential fill` will give us one,
            // otherwise it'll just pass this one".
            password: auth.pass.map(str::to_string),
        };
        cred.run("fill")?;

        let result = self.sasl_auth(&cred, auth.mechanisms)?;
        cred.run(if result { "approve" } else { "reject" })?;
        self.authenticated = result;
        Ok(result)
    }

    /// `Net::SMTP::auth` with the `Authen::SASL` object git builds: pick a
    /// mechanism, then run it.
    fn sasl_auth(&mut self, cred: &Credential, requested: Option<&str>) -> Result<bool, String> {
        let Some(server_list) = self.supports("AUTH").map(str::to_string) else {
            self.set_status(500, "Command unknown: 'AUTH'");
            return Ok(false);
        };
        let server: Vec<String> =
            server_list.split_ascii_whitespace().map(str::to_ascii_uppercase).collect();
        let candidates: Vec<String> = match requested.filter(|m| !m.is_empty()) {
            Some(m) => m.split_ascii_whitespace().map(str::to_ascii_uppercase).collect(),
            None => server.clone(),
        };
        let Some(mechanism) =
            candidates.iter().find(|m| MECHANISMS.contains(&m.as_str())).cloned()
        else {
            if requested.is_some() {
                return Err(format!(
                    "unsupported: --smtp-auth={} names no mechanism this build implements — \
                     {} are the ones it speaks\n",
                    candidates.join(" "),
                    MECHANISMS.join(" and ")
                ));
            }
            self.set_status(
                500,
                &format!(
                    "Client SASL mechanisms ({}) do not match server ones ({})",
                    MECHANISMS.join(" "),
                    server.join(" ")
                ),
            );
            return Ok(false);
        };

        let user = cred.username.clone().unwrap_or_default();
        let pass = cred.password.clone().unwrap_or_default();
        let outcome = match mechanism.as_str() {
            "PLAIN" => {
                // RFC 4616: authzid NUL authcid NUL passwd, with `authname` the
                // same as `user`, which is how git fills the SASL callback.
                let mut token = Vec::new();
                token.extend_from_slice(user.as_bytes());
                token.push(0);
                token.extend_from_slice(user.as_bytes());
                token.push(0);
                token.extend_from_slice(pass.as_bytes());
                self.cmd(&format!("AUTH PLAIN {}", b64(&token)))
            }
            _ => {
                // LOGIN: the two challenges are answered in order, and their
                // text is not inspected — `Authen::SASL::Perl::LOGIN` does not
                // look at it either.
                let mut class = self.cmd("AUTH LOGIN");
                if class == 3 {
                    class = self.cmd(&b64(user.as_bytes()));
                }
                if class == 3 {
                    class = self.cmd(&b64(pass.as_bytes()));
                }
                class
            }
        };

        match outcome {
            2 => Ok(true),
            0 => {
                // The socket failed rather than the credential: `Net::SMTP`
                // dies out of `$smtp->auth` and git runs `handle_smtp_error`.
                let message = self.message();
                Ok(handle_smtp_error(message.trim_end_matches('\n')))
            }
            _ => Ok(false),
        }
    }
}

/// `MIME::Base64::encode_base64($token, '')` — no line breaks.
fn b64(bytes: &[u8]) -> String {
    let mut s = String::from_utf8_lossy(&encode_base64(bytes)).into_owned();
    s.retain(|c| c != '\n');
    s
}

/// `$smtp_auth !~ /^(\b[A-Z0-9-_]{1,20}\s*)*$/` — mechanism names, in capitals,
/// separated by whitespace.
fn valid_mechanism_list(list: &str) -> bool {
    let mut rest = list;
    loop {
        let name_len = rest
            .bytes()
            .position(|b| !(b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-' || b == b'_'))
            .unwrap_or(rest.len());
        if name_len == 0 {
            return rest.is_empty();
        }
        if name_len > 20 {
            return false;
        }
        rest = &rest[name_len..];
        let ws = rest.bytes().position(|b| !b.is_ascii_whitespace()).unwrap_or(rest.len());
        if ws == 0 && !rest.is_empty() {
            return false;
        }
        rest = &rest[ws..];
        if rest.is_empty() {
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-signed P-256 certificate, for the `SSL_ca_file`/`SSL_ca_path`
    /// cases. Nothing signs anything with it; it only has to parse as a trust
    /// anchor.
    const CA_PEM: &str = concat!(
        "-----BEGIN CERTIFICATE-----\n",
        "MIIBhDCCASugAwIBAgIUYD2hSHARda8vH3Ezo+S2eE2Ibg4wCgYIKoZIzj0EAwIw\n",
        "FzEVMBMGA1UEAwwMenZjcy10ZXN0LWNhMCAXDTI2MDcyNzAwMTgwNFoYDzIxMjYw\n",
        "NzAzMDAxODA0WjAXMRUwEwYDVQQDDAx6dmNzLXRlc3QtY2EwWTATBgcqhkjOPQIB\n",
        "BggqhkjOPQMBBwNCAAShYdXc4bZG+YgPrmpCjZUPjcnCnqnU0oimk7c15QvvfMql\n",
        "mli5W9IyWsjC0qS0EpCwvB8DevpYO/b5mDH7bBBjo1MwUTAdBgNVHQ4EFgQUSDZ6\n",
        "s2EGTjko3tvF31iTLyhyBJUwHwYDVR0jBBgwFoAUSDZ6s2EGTjko3tvF31iTLyhy\n",
        "BJUwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNHADBEAiAgWLiFDMDyuuI1\n",
        "9qgIiBy+Ue4Dot+hYk42Ocu4MpIulAIgY3pT0gu9x1sdJR1FWK3HHucuAmJ+ozN5\n",
        "vPe3Jxfz1GI=\n",
        "-----END CERTIFICATE-----\n",
    );

    /// A second self-signed P-256 certificate with its key, for the client
    /// certificate cases.
    const CLIENT_PEM: &str = concat!(
        "-----BEGIN CERTIFICATE-----\n",
        "MIIBjjCCATOgAwIBAgIUEbcRInHktuXlFtQewgjIwttl4bowCgYIKoZIzj0EAwIw\n",
        "GzEZMBcGA1UEAwwQenZjcy10ZXN0LWNsaWVudDAgFw0yNjA3MjcwMDE4MDRaGA8y\n",
        "MTI2MDcwMzAwMTgwNFowGzEZMBcGA1UEAwwQenZjcy10ZXN0LWNsaWVudDBZMBMG\n",
        "ByqGSM49AgEGCCqGSM49AwEHA0IABDti+IC8o5gljJjlct0ec0stvDd4xfNRNYPm\n",
        "Ravsli51qCJL5V1latxZaUfRpSs0kbVQD07pViqSy5PW8cW6H06jUzBRMB0GA1Ud\n",
        "DgQWBBRlZg5rNMN1kcV7CPETsL8G+eyzTTAfBgNVHSMEGDAWgBRlZg5rNMN1kcV7\n",
        "CPETsL8G+eyzTTAPBgNVHRMBAf8EBTADAQH/MAoGCCqGSM49BAMCA0kAMEYCIQCV\n",
        "88AFoBGdftIKBUIyJwv8jf1rcH9xgF3TV4w4goqLlwIhAJnyXqnPRSB7iP8L2Aam\n",
        "pCnraxt/MSf52j7WQkCeTpU8\n",
        "-----END CERTIFICATE-----\n",
    );

    /// The key matching `CLIENT_PEM`.
    const CLIENT_KEY_PEM: &str = concat!(
        "-----BEGIN PRIVATE KEY-----\n",
        "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg7vbvE5OXn1QTiTeE\n",
        "Q8dmUkPOkiOca0EDVzv5mA1zi8uhRANCAAQ7YviAvKOYJYyY5XLdHnNLLbw3eMXz\n",
        "UTWD5kWr7JYudagiS+VdZWrcWWlH0aUrNJG1UA9O6VYqksuT1vHFuh9O\n",
        "-----END PRIVATE KEY-----\n",
    );

    /// A scratch directory under the target dir, so the tests need no writable
    /// `/tmp` policy and clean up after themselves.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zvcs-smtp-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// `ssl_verify_params()` with a CA *file*: it is opened, parsed and its
    /// certificates become the trust anchors of the configuration the
    /// handshake runs with. A file that is not PEM is reported against its
    /// name rather than silently ignored.
    #[test]
    fn ca_file_is_read_into_the_trust_anchors() {
        let dir = scratch("cafile");
        let good = dir.join("ca.pem");
        std::fs::write(&good, CA_PEM).expect("write ca");
        let ssl = Ssl { cert_path: Some(good.to_string_lossy().into_owned()), ..Ssl::default() };
        assert!(ssl.client_config().is_ok(), "a PEM CA file builds a verifying config");

        let bad = dir.join("not-a-ca.pem");
        std::fs::write(&bad, "this is not a certificate\n").expect("write junk");
        let ssl = Ssl { cert_path: Some(bad.to_string_lossy().into_owned()), ..Ssl::default() };
        let err = ssl.client_config().expect_err("junk CA file");
        assert!(err.contains("failed to read CA file"), "{err}");
        assert!(err.contains("not-a-ca.pem"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A CA *directory* is `SSL_ca_path`: every certificate in it is read.
    #[test]
    fn ca_directory_is_read_into_the_trust_anchors() {
        let dir = scratch("capath");
        std::fs::write(dir.join("root.pem"), CA_PEM).expect("write ca");
        let ssl = Ssl { cert_path: Some(dir.to_string_lossy().into_owned()), ..Ssl::default() };
        assert!(ssl.client_config().is_ok(), "a directory of PEM files builds a verifying config");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An empty `smtpSSLCertPath` is `SSL_VERIFY_NONE`: the configuration is
    /// built with the accept-anything verifier, which still hands the provider
    /// its full signature-scheme list.
    #[test]
    fn empty_ca_path_disables_verification() {
        let ssl = Ssl { cert_path: Some(String::new()), ..Ssl::default() };
        let cfg = ssl.client_config().expect("no-verify config builds");
        let verifier = NoVerify {
            schemes: cfg.crypto_provider().signature_verification_algorithms.supported_schemes(),
        };
        assert!(!verifier.supported_verify_schemes().is_empty());
        let name = ServerName::try_from("nowhere.invalid").expect("name");
        assert!(
            verifier
                .verify_server_cert(
                    &CertificateDer::from(vec![0u8; 4]),
                    &[],
                    &name,
                    &[],
                    UnixTime::now()
                )
                .is_ok(),
            "SSL_VERIFY_NONE accepts a certificate that is not even parseable"
        );
        // With no client certificate configured, nothing is offered.
        assert!(!cfg.client_auth_cert_resolver.has_certs());
    }

    /// A CA path that is neither a file nor a directory is the script's fatal.
    #[test]
    fn missing_ca_path_is_fatal() {
        let ssl = Ssl { cert_path: Some("/nonexistent/ca".into()), ..Ssl::default() };
        let err = ssl.client_config().expect_err("missing path");
        assert_eq!(err, "CA path \"/nonexistent/ca\" does not exist\n");
    }

    /// `SSL_cert_file`/`SSL_key_file`: the pair reaches the handshake as the
    /// certificate the client is ready to present.
    #[test]
    fn client_certificate_reaches_the_handshake() {
        let dir = scratch("clientcert");
        let cert = dir.join("client.pem");
        let key = dir.join("client-key.pem");
        std::fs::write(&cert, CLIENT_PEM).expect("write cert");
        std::fs::write(&key, CLIENT_KEY_PEM).expect("write key");

        let ssl = Ssl {
            cert_path: Some(String::new()),
            client_cert: Some(cert.to_string_lossy().into_owned()),
            client_key: Some(key.to_string_lossy().into_owned()),
        };
        let cfg = ssl.client_config().expect("client auth config builds");
        assert!(cfg.client_auth_cert_resolver.has_certs(), "the certificate is offered");

        // `SSL_key_file` defaults to `SSL_cert_file`, so a combined PEM works
        // with the certificate alone.
        let both = dir.join("both.pem");
        std::fs::write(&both, format!("{CLIENT_PEM}{CLIENT_KEY_PEM}")).expect("write both");
        let ssl = Ssl {
            cert_path: Some(String::new()),
            client_cert: Some(both.to_string_lossy().into_owned()),
            client_key: None,
        };
        let cfg = ssl.client_config().expect("combined PEM builds");
        assert!(cfg.client_auth_cert_resolver.has_certs());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A client key without a client certificate is the script's other fatal.
    #[test]
    fn client_key_without_cert_is_fatal() {
        let ssl = Ssl { client_key: Some("/tmp/k.pem".into()), ..Ssl::default() };
        let err = ssl.client_config().expect_err("key alone");
        assert_eq!(err, "Only client key \"/tmp/k.pem\" specified\n");
    }

    /// `Net::Cmd::datasend`'s framing.
    #[test]
    fn payloads_are_crlf_terminated_and_dot_stuffed() {
        assert_eq!(dot_stuff(b"a\nb\n", true), b"a\r\nb\r\n".to_vec());
        // RFC 5321 4.5.2 inserts exactly one additional period, so a line that
        // already starts with one dot goes out with two — not three.
        assert_eq!(dot_stuff(b".hidden\n", true), b"..hidden\r\n".to_vec());
        assert_eq!(dot_stuff(b"x\n.y\n", true), b"x\r\n..y\r\n".to_vec());
        // A `.` that opens the chunk is only stuffed when the previous chunk
        // ended a line.
        assert_eq!(dot_stuff(b".y\n", false), b".y\r\n".to_vec());
        // An already-CRLF payload is not doubled up.
        assert_eq!(dot_stuff(b"a\r\nb\r\n", true), b"a\r\nb\r\n".to_vec());
        // A lone CR is data.
        assert_eq!(dot_stuff(b"a\rb\n", true), b"a\rb\r\n".to_vec());
    }

    /// `Net::SMTP::_addr`.
    #[test]
    fn addresses_are_wrapped_once() {
        assert_eq!(wrap_addr("a@b.c"), "<a@b.c>");
        assert_eq!(wrap_addr("  a@b.c "), "<a@b.c>");
        assert_eq!(wrap_addr("A B <a@b.c>"), "<a@b.c>");
    }

    /// `IO::Socket::INET`'s `PeerAddr` beats the `Port` argument.
    #[test]
    fn host_port_in_the_server_wins() {
        assert_eq!(split_host_port("mail.example:2525", "25"), ("mail.example".into(), "2525".into()));
        assert_eq!(split_host_port("mail.example", "465"), ("mail.example".into(), "465".into()));
        assert_eq!(split_host_port("::1", "25"), ("::1".into(), "25".into()));
    }

    /// `Net::SMTP::hello`'s capability scan.
    #[test]
    fn esmtp_capabilities_are_upper_cased_and_split() {
        assert_eq!(
            esmtp_keyword("AUTH PLAIN LOGIN\n"),
            Some(("AUTH".into(), "PLAIN LOGIN".into()))
        );
        assert_eq!(esmtp_keyword("SIZE 35882577\n"), Some(("SIZE".into(), "35882577".into())));
        assert_eq!(esmtp_keyword("8BITMIME\n"), Some(("8BITMIME".into(), String::new())));
    }

    /// The RFC 4422 name check behind `invalid smtp auth`.
    #[test]
    fn mechanism_lists_are_checked() {
        assert!(valid_mechanism_list("PLAIN"));
        assert!(valid_mechanism_list("PLAIN LOGIN"));
        assert!(valid_mechanism_list("CRAM-MD5 DIGEST-MD5"));
        assert!(!valid_mechanism_list("plain"));
        assert!(!valid_mechanism_list("PLAIN!"));
        assert!(!valid_mechanism_list("ABCDEFGHIJKLMNOPQRSTUVWXYZ"));
    }

    /// `valid_fqdn`, including its Darwin-only `.local` refusal.
    #[test]
    fn fqdns_are_validated() {
        assert!(valid_fqdn("mail.example.com"));
        assert!(!valid_fqdn("-bad.example.com"));
        assert!(!valid_fqdn(""));
        assert_eq!(valid_fqdn("host.local"), !cfg!(target_os = "macos"));
    }
}
