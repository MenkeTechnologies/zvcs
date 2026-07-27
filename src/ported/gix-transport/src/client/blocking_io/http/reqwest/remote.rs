use std::{
    any::Any,
    io::{Read, Write},
    net::{SocketAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use gix_features::io::pipe;
use parking_lot::Mutex;

use crate::client::blocking_io::http::{
    self,
    options::{FollowRedirects, HttpVersion, ProxyAuthMethod, SslVersion, SslVersionRangeInclusive},
    redirect::{self, Action as RedirectAction},
    reqwest::Remote,
    traits::PostBodyDataKind,
};

/// The error returned by the 'remote' helper, a purely internal construct to perform http requests.
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error("Could not finish reading all data to post to the remote")]
    ReadPostBody(#[from] std::io::Error),
    #[error("Request configuration failed")]
    ConfigureRequest(#[from] Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error(transparent)]
    Redirect(#[from] redirect::Error),
    #[error("Could not read {kind} at {path:?}")]
    ReadTlsFile {
        kind: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}")]
    Unsupported(String),
    #[error("Could not obtain credentials for the proxy")]
    ProxyAuthenticate(#[from] gix_credentials::protocol::Error),
}

impl crate::IsSpuriousError for Error {
    fn is_spurious(&self) -> bool {
        match self {
            Error::Reqwest(err) => {
                err.is_timeout() || err.is_connect() || err.status().is_some_and(|status| status.is_server_error())
            }
            _ => false,
        }
    }
}

/// The part of [`http::Options`] that `reqwest` can only apply while *building* a client, as opposed to
/// per request. The worker thread keeps one client per distinct value of this and rebuilds when it changes,
/// which is what makes `http.proxy`, the `http.ssl*` keys, `http.userAgent`, `http.version`,
/// `http.curloptResolve`, `http.keepAlive*` and `http.minSessions` take effect at all: the transport is
/// configured *after* it was constructed, so a client built up front could never see them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ClientConfig {
    proxy: Option<String>,
    no_proxy: Option<String>,
    proxy_auth_method: ProxyAuthMethod,
    user_agent: Option<String>,
    connect_timeout: Option<Duration>,
    verbose: bool,
    ssl_verify: bool,
    ssl_ca_info: Option<PathBuf>,
    ssl_ca_path: Option<PathBuf>,
    ssl_cert: Option<PathBuf>,
    ssl_key: Option<PathBuf>,
    ssl_version: Option<SslVersionRangeInclusive>,
    http_version: Option<HttpVersion>,
    resolve: Vec<String>,
    tcp_keepalive_idle_seconds: Option<u64>,
    tcp_keepalive_interval_seconds: Option<u64>,
    tcp_keepalive_count: Option<u32>,
    min_sessions: Option<usize>,
    address_family: Option<crate::AddressFamily>,
}

impl ClientConfig {
    fn from_options(opts: &http::Options) -> Self {
        ClientConfig {
            proxy: opts.proxy.clone(),
            no_proxy: opts.no_proxy.clone(),
            proxy_auth_method: opts.proxy_auth_method,
            user_agent: opts.user_agent.clone(),
            connect_timeout: opts.connect_timeout,
            verbose: opts.verbose,
            ssl_verify: opts.ssl_verify,
            ssl_ca_info: opts.ssl_ca_info.clone(),
            ssl_ca_path: opts.ssl_ca_path.clone(),
            ssl_cert: opts.ssl_cert.clone(),
            ssl_key: opts.ssl_key.clone(),
            ssl_version: opts.ssl_version,
            http_version: opts.http_version,
            resolve: opts.resolve.clone(),
            tcp_keepalive_idle_seconds: opts.tcp_keepalive_idle_seconds,
            tcp_keepalive_interval_seconds: opts.tcp_keepalive_interval_seconds,
            tcp_keepalive_count: opts.tcp_keepalive_count,
            min_sessions: opts.min_sessions,
            address_family: opts.address_family,
        }
    }
}

/// Translate `version` into `reqwest`'s TLS version, refusing what `rustls` has no notion of.
fn to_tls_version(version: SslVersion) -> Result<Option<reqwest::tls::Version>, Error> {
    Ok(Some(match version {
        SslVersion::Default => return Ok(None),
        SslVersion::TlsV1 | SslVersion::TlsV1_0 => reqwest::tls::Version::TLS_1_0,
        SslVersion::TlsV1_1 => reqwest::tls::Version::TLS_1_1,
        SslVersion::TlsV1_2 => reqwest::tls::Version::TLS_1_2,
        SslVersion::TlsV1_3 => reqwest::tls::Version::TLS_1_3,
        SslVersion::SslV2 | SslVersion::SslV3 => {
            return Err(Error::Unsupported(format!(
                "The 'http.sslVersion' value {} is not supported by the reqwest HTTP backend",
                match version {
                    SslVersion::SslV2 => "sslv2",
                    _ => "sslv3",
                }
            )));
        }
    }))
}

/// Read a PEM bundle at `path` and turn it into root certificates, like curl's `CURLOPT_CAINFO`.
fn root_certificates_from_file(path: &Path) -> Result<Vec<reqwest::Certificate>, Error> {
    let bundle = std::fs::read(path).map_err(|source| Error::ReadTlsFile {
        kind: "the CA certificate bundle",
        path: path.to_owned(),
        source,
    })?;
    Ok(reqwest::Certificate::from_pem_bundle(&bundle)?)
}

/// Read every file in the directory `path` as a PEM bundle, like curl's `CURLOPT_CAPATH`.
///
/// Unreadable or non-PEM entries are skipped: a CA directory legitimately holds OpenSSL hash symlinks
/// and other noise, and curl also just ignores what it cannot use.
fn root_certificates_from_dir(path: &Path) -> Result<Vec<reqwest::Certificate>, Error> {
    let entries = std::fs::read_dir(path).map_err(|source| Error::ReadTlsFile {
        kind: "the CA certificate directory",
        path: path.to_owned(),
        source,
    })?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|ft| ft.is_file() || ft.is_symlink()) {
            continue;
        }
        let Ok(bundle) = std::fs::read(entry.path()) else {
            continue;
        };
        if let Ok(certs) = reqwest::Certificate::from_pem_bundle(&bundle) {
            out.extend(certs);
        }
    }
    Ok(out)
}

/// Turn `http.sslCert` and `http.sslKey` into a client identity. As `rustls` reads both from one PEM blob,
/// the two files are concatenated; a lone `http.sslCert` is expected to carry the key as well, which is
/// what curl does when `CURLOPT_SSLKEY` is unset.
fn identity_from_files(cert: &Path, key: Option<&Path>) -> Result<reqwest::Identity, Error> {
    let mut pem = std::fs::read(cert).map_err(|source| Error::ReadTlsFile {
        kind: "the client certificate",
        path: cert.to_owned(),
        source,
    })?;
    if let Some(key) = key {
        if key != cert {
            if !pem.ends_with(b"\n") {
                pem.push(b'\n');
            }
            let key_pem = std::fs::read(key).map_err(|source| Error::ReadTlsFile {
                kind: "the client certificate key",
                path: key.to_owned(),
                source,
            })?;
            pem.extend_from_slice(&key_pem);
        }
    }
    Ok(reqwest::Identity::from_pem(&pem)?)
}

/// Apply `http.curloptResolve` entries in order, honoring curl's `+`-prefixed additions and
/// `-`-prefixed removals, and yield the resulting `host -> addresses` overrides.
fn resolve_overrides(entries: &[String]) -> Vec<(String, Vec<SocketAddr>)> {
    let mut out: Vec<(String, Vec<SocketAddr>)> = Vec::new();
    for entry in entries {
        let (remove, spec) = match entry.as_bytes().first() {
            Some(b'-') => (true, &entry[1..]),
            Some(b'+') => (false, &entry[1..]),
            _ => (false, entry.as_str()),
        };
        // `HOST:PORT[:ADDRESS[,ADDRESS]]`, where the host may not contain a colon.
        let mut fields = spec.splitn(3, ':');
        let (Some(host), Some(port)) = (fields.next(), fields.next()) else {
            continue;
        };
        let Ok(port) = port.parse::<u16>() else { continue };
        if remove {
            out.retain(|(existing, _)| existing != host);
            continue;
        }
        let addresses: Vec<_> = fields
            .next()
            .unwrap_or_default()
            .split(',')
            .filter_map(|addr| addr.trim().parse::<std::net::IpAddr>().ok())
            .map(|ip| SocketAddr::new(ip, port))
            .collect();
        if addresses.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(existing, _)| existing == host) {
            Some((_, existing)) => existing.extend(addresses),
            None => out.push((host.to_owned(), addresses)),
        }
    }
    out
}

/// A DNS resolver that answers like `getaddrinfo()` with `hints.ai_family` set: the system resolution,
/// filtered down to one address family.
///
/// Resolution happens synchronously inside the future because the blocking `reqwest` client owns a
/// dedicated single-client runtime and the git transport issues one request at a time, so there is no
/// concurrent work to stall.
struct FamilyRestrictedResolver {
    family: crate::AddressFamily,
}

impl reqwest::dns::Resolve for FamilyRestrictedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let family = self.family;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            // Port 0: `reqwest` substitutes the URL's port (or the scheme default) afterwards.
            let addrs: Vec<std::net::SocketAddr> = (host.as_str(), 0u16)
                .to_socket_addrs()?
                .filter(|addr| family.matches(addr))
                .collect();
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Build a `reqwest` client that honors `config`, with `redirect_action`/`redirect_tail` driving the
/// redirect policy that `http.followRedirects` selects per request.
fn build_client(
    config: &ClientConfig,
    proxy_credentials: Option<(String, String)>,
    redirect_action: Arc<Mutex<RedirectAction>>,
    redirect_tail: Arc<Mutex<String>>,
) -> Result<reqwest::blocking::Client, Error> {
    let mut builder = reqwest::blocking::ClientBuilder::new()
        .connect_timeout(config.connect_timeout.unwrap_or(Duration::from_secs(20)))
        .connection_verbose(config.verbose)
        .http1_title_case_headers()
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            match *redirect_action.lock() {
                RedirectAction::Follow => {
                    let curr_url = attempt.url();
                    let prev_urls = attempt.previous();
                    // emulate default git behaviour which relies on curl default behaviour apparently.
                    const CURL_DEFAULT_REDIRS: usize = 50;
                    if prev_urls.len() >= CURL_DEFAULT_REDIRS {
                        return attempt.error("too many redirects");
                    }

                    match prev_urls.last() {
                        Some(prev_url) if !redirect::scheme_is_safe(curr_url.as_str(), prev_url.as_str()) => {
                            // Don't follow insecure protocol redirects, particularly https-to-http downgrades.
                            attempt.stop()
                        }
                        Some(prev_url) if authority_changed(curr_url, prev_url) => {
                            // Allowed only if the tail doesn't change.
                            let redirect_tail = redirect_tail.lock();
                            if curr_url.as_str().ends_with(redirect_tail.as_str()) {
                                attempt.follow()
                            } else {
                                let curr_url = curr_url.as_str().to_owned();
                                let redirect_tail = redirect_tail.to_string();
                                attempt.error(format!(
                                    "redirect url {curr_url:?} does not end with expected request suffix {redirect_tail:?}",
                                ))
                            }
                        }
                        _ => attempt.follow(),
                    }
                }
                RedirectAction::RejectConfiguredHeaders => {
                    attempt.error("refusing to follow redirect after request headers were configured")
                }
                RedirectAction::Stop => attempt.stop(),
            }
        }));

    if let Some(user_agent) = &config.user_agent {
        builder = builder.user_agent(user_agent.clone());
    }
    match config.http_version {
        // `http.version=HTTP/1.1`.
        Some(HttpVersion::V1_1) => builder = builder.http1_only(),
        // `http.version=HTTP/2`, which for `git` means to use HTTP/2 without an upgrade dance.
        Some(HttpVersion::V2) => builder = builder.http2_prior_knowledge(),
        None => {}
    }

    // `http.proxy`, where an empty value disables proxying entirely, just like in `git`.
    match config.proxy.as_deref() {
        Some("") => builder = builder.no_proxy(),
        Some(url) => {
            let mut proxy = reqwest::Proxy::all(url)?;
            if let Some((username, password)) = proxy_credentials {
                proxy = match config.proxy_auth_method {
                    ProxyAuthMethod::AnyAuth | ProxyAuthMethod::Basic => proxy.basic_auth(&username, &password),
                    method => {
                        return Err(Error::Unsupported(format!(
                            "The 'http.proxyAuthMethod' value {} is not supported by the reqwest HTTP backend, which can only send HTTP basic proxy authentication",
                            match method {
                                ProxyAuthMethod::Digest => "digest",
                                ProxyAuthMethod::Negotiate => "negotiate",
                                _ => "ntlm",
                            }
                        )));
                    }
                };
            }
            if let Some(no_proxy) = config.no_proxy.as_deref() {
                proxy = proxy.no_proxy(reqwest::NoProxy::from_string(no_proxy));
            }
            builder = builder.proxy(proxy);
        }
        None => {}
    }

    // `http.sslVerify`, which turns off both peer and hostname verification, as `git` turns off both
    // `CURLOPT_SSL_VERIFYPEER` and `CURLOPT_SSL_VERIFYHOST`.
    if !config.ssl_verify {
        builder = builder
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true);
    }

    // `http.sslCAInfo` and `http.sslCAPath` *replace* the platform roots, like curl's CAINFO/CAPATH do.
    let mut roots = Vec::new();
    if let Some(path) = &config.ssl_ca_info {
        roots.extend(root_certificates_from_file(path)?);
    }
    if let Some(path) = &config.ssl_ca_path {
        roots.extend(root_certificates_from_dir(path)?);
    }
    if !roots.is_empty() {
        builder = builder.tls_certs_only(roots);
    }

    // `http.sslCert` and `http.sslKey`.
    if let Some(cert) = &config.ssl_cert {
        builder = builder.identity(identity_from_files(cert, config.ssl_key.as_deref())?);
    }

    // `http.sslVersion`, plus `gitoxide.http.sslVersionMin`/`Max`.
    if let Some(range) = config.ssl_version {
        let (min, max) = range.min_max();
        if let Some(min) = to_tls_version(min)? {
            builder = builder.tls_version_min(min);
        }
        if let Some(max) = to_tls_version(max)? {
            builder = builder.tls_version_max(max);
        }
    }

    // `http.curloptResolve`.
    for (host, addresses) in resolve_overrides(&config.resolve) {
        builder = builder.resolve_to_addrs(&host, &addresses);
    }

    // `http.keepAliveIdle`, `http.keepAliveInterval` and `http.keepAliveCount`.
    if let Some(seconds) = config.tcp_keepalive_idle_seconds {
        builder = builder.tcp_keepalive(Duration::from_secs(seconds));
    }
    if let Some(seconds) = config.tcp_keepalive_interval_seconds {
        builder = builder.tcp_keepalive_interval(Duration::from_secs(seconds));
    }
    if let Some(count) = config.tcp_keepalive_count {
        builder = builder.tcp_keepalive_retries(count);
    }

    // `http.minSessions` — the number of connections kept alive between requests.
    if let Some(sessions) = config.min_sessions {
        builder = builder.pool_max_idle_per_host(sessions);
    }

    // git's `--ipv4`/`--ipv6`, which curl implements with `CURLOPT_IPRESOLVE`. `reqwest` has no such
    // knob, so the equivalent is a resolver that drops every address of the other family.
    if let Some(family) = config.address_family {
        builder = builder.dns_resolver(std::sync::Arc::new(FamilyRestrictedResolver { family }));
    }

    Ok(builder.build()?)
}

/// Abort a transfer that receives fewer than `limit` bytes per second across a `window` of time,
/// which is what curl's `CURLOPT_LOW_SPEED_LIMIT`/`CURLOPT_LOW_SPEED_TIME` do for
/// `http.lowSpeedLimit`/`http.lowSpeedTime`.
struct LowSpeedGuard<R> {
    inner: R,
    limit: u64,
    window: Duration,
    window_start: Instant,
    window_bytes: u64,
}

impl<R: Read> Read for LowSpeedGuard<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.window_bytes += n as u64;
        let elapsed = self.window_start.elapsed();
        if elapsed >= self.window {
            if self.window_bytes < self.limit.saturating_mul(elapsed.as_secs()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "Operation too slow. Less than {} bytes/sec transferred the last {} seconds",
                        self.limit,
                        self.window.as_secs()
                    ),
                ));
            }
            self.window_start = Instant::now();
            self.window_bytes = 0;
        }
        Ok(n)
    }
}

fn authority_changed(curr_url: &reqwest::Url, prev_url: &reqwest::Url) -> bool {
    curr_url.scheme() != prev_url.scheme()
        || curr_url.host_str() != prev_url.host_str()
        || curr_url.port_or_known_default() != prev_url.port_or_known_default()
}

impl Default for Remote {
    fn default() -> Self {
        let (req_send, req_recv) = std::sync::mpsc::sync_channel(0);
        let (res_send, res_recv) = std::sync::mpsc::sync_channel(0);
        let redirected_base_url_shared = Arc::new(Mutex::new(None));
        let redirected_base_url_shared_for_field = redirected_base_url_shared.clone();
        let handle = std::thread::spawn(move || -> Result<(), Error> {
            let mut follow = None;
            let redirect_action = Arc::new(Mutex::new(RedirectAction::Stop));
            let redirect_tail = Arc::new(Mutex::new(String::new()));

            // The client is built from the first request's configuration and rebuilt whenever the
            // client-level part of it changes, because `configure()` runs *after* this thread started.
            // We may error while configuring, which is expected as part of the internal protocol. The error will be
            // received and the sender of the request might restart us.
            let mut cached_client: Option<(ClientConfig, reqwest::blocking::Client)> = None;
            let mut proxy_auth_action: Option<(
                gix_credentials::helper::NextAction,
                Arc<std::sync::Mutex<http::options::AuthenticateFn>>,
            )> = None;
            // `http.cookieFile` is read once, as curl's cookie engine does, and kept for the session.
            let mut cookies: Option<(PathBuf, http::cookies::Jar)> = None;

            for Request {
                url,
                base_url,
                headers,
                upload_body_kind,
                config,
            } in req_recv
            {
                let client_config = ClientConfig::from_options(&config);
                let client = match &cached_client {
                    Some((cached, client)) if *cached == client_config => client.clone(),
                    _ => {
                        // Only ask for proxy credentials once a proxy that wants them is configured,
                        // matching what the curl backend does.
                        let credentials = match config.proxy_authenticate.clone() {
                            Some((action, authenticate)) => {
                                let outcome = authenticate.lock().expect("no panics in other threads")(action)?
                                    .expect("action to fetch credentials");
                                proxy_auth_action = Some((outcome.next, authenticate));
                                Some((outcome.identity.username, outcome.identity.password))
                            }
                            None => None,
                        };
                        let client = build_client(
                            &client_config,
                            credentials,
                            redirect_action.clone(),
                            redirect_tail.clone(),
                        )?;
                        cached_client = Some((client_config, client.clone()));
                        client
                    }
                };
                let redirected_base_url = redirected_base_url_shared.lock().clone();
                let effective_url = redirect::swap_tails(redirected_base_url.as_deref(), &base_url, url.clone());
                let has_configured_extra_headers = !config.extra_headers.is_empty();
                let mut req_builder = if upload_body_kind.is_some() {
                    client.post(&effective_url)
                } else {
                    client.get(&effective_url)
                }
                .headers(headers);
                let (post_body_tx, mut post_body_rx) = pipe::unidirectional(0);
                let (mut response_body_tx, response_body_rx) = pipe::unidirectional(0);
                let (mut headers_tx, headers_rx) = pipe::unidirectional(0);
                if res_send
                    .send(Response {
                        headers: headers_rx,
                        body: response_body_rx,
                        upload_body: post_body_tx,
                    })
                    .is_err()
                {
                    // This means our internal protocol is violated as the one who sent the request isn't listening anymore.
                    // Shut down as something is off.
                    break;
                }
                req_builder = match upload_body_kind {
                    Some(PostBodyDataKind::BoundedAndFitsIntoMemory) => {
                        let mut buf = Vec::<u8>::with_capacity(512);
                        post_body_rx.read_to_end(&mut buf)?;
                        req_builder.body(buf)
                    }
                    // `http.postBuffer`: buffer up to that many bytes and, if the body ended within the
                    // budget, send it with `Content-Length`; only a larger body is streamed with
                    // `Transfer-Encoding: chunked`. This is `remote-curl.c`'s `post_rpc()` with
                    // `max_request_buffer`.
                    Some(PostBodyDataKind::Unbounded) => match config.post_buffer_bytes {
                        Some(limit) => {
                            let mut buf = Vec::<u8>::new();
                            (&mut post_body_rx).take(limit).read_to_end(&mut buf)?;
                            let mut probe = [0u8; 1];
                            match post_body_rx.read(&mut probe)? {
                                0 => req_builder.body(buf),
                                _ => {
                                    buf.extend_from_slice(&probe);
                                    req_builder.body(reqwest::blocking::Body::new(
                                        std::io::Cursor::new(buf).chain(post_body_rx),
                                    ))
                                }
                            }
                        }
                        None => req_builder.body(reqwest::blocking::Body::new(post_body_rx)),
                    },
                    None => req_builder,
                };
                // `http.cookieFile`: attach the stored cookies that apply to this URL.
                if let Some(path) = &config.cookie_file {
                    let jar = match &cookies {
                        Some((loaded, jar)) if loaded == path => jar,
                        _ => {
                            let jar = http::cookies::Jar::from_file(path);
                            &cookies.insert((path.clone(), jar)).1
                        }
                    };
                    if let Some(header) = jar.header_for(&effective_url) {
                        if let Ok(value) = reqwest::header::HeaderValue::try_from(header) {
                            req_builder = req_builder.header(reqwest::header::COOKIE, value);
                        }
                    }
                }

                let mut req = req_builder.build()?;
                let mut has_configure_request = false;
                if let Some(ref mut request_options) = config.backend.as_ref().and_then(|backend| backend.lock().ok()) {
                    if let Some(options) = request_options.downcast_mut::<super::Options>() {
                        if let Some(configure_request) = &mut options.configure_request {
                            has_configure_request = true;
                            configure_request(&mut req)?;
                        }
                    }
                }

                let follow = follow.get_or_insert(config.follow_redirects);
                let may_follow_redirects = matches!(*follow, FollowRedirects::Initial | FollowRedirects::All);
                let has_configured_request_headers = has_configure_request || has_configured_extra_headers;
                *redirect_action.lock() =
                    RedirectAction::from_request(may_follow_redirects, has_configured_request_headers);
                url.strip_prefix(&base_url)
                    .expect("BUG: caller assures `base_url` is subset of `url`")
                    .clone_into(&mut redirect_tail.lock());

                if *follow == FollowRedirects::Initial {
                    *follow = FollowRedirects::None;
                }

                // `http.maxRetries`, `http.retryAfter` and `http.maxRetryTime`: repeat a request that was
                // answered with `429 Too Many Requests`, waiting for what the server asks for via
                // `Retry-After` or, failing that, for `http.retryAfter` seconds. A body that is being
                // streamed cannot be replayed, so such a request is never retried.
                let mut retries_left = config.max_retries;
                let mut pending = Some(req);
                let executed = loop {
                    let req = pending.take().expect("a request to send in every iteration");
                    let replay = (retries_left > 0).then(|| req.try_clone()).flatten();
                    let res = client.execute(req);
                    let Some(replay) = replay else { break res };
                    let Ok(ref ok) = res else { break res };
                    if ok.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
                        break res;
                    }
                    // Only the delta-seconds form of `Retry-After` is understood; the HTTP-date form
                    // falls back to `http.retryAfter`, as it does with libcurl before 7.66.0.
                    let delay = ok
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .unwrap_or(config.retry_after_seconds);
                    if delay > config.max_retry_time_seconds {
                        break res;
                    }
                    std::thread::sleep(Duration::from_secs(delay));
                    retries_left -= 1;
                    pending = Some(replay);
                };

                let mut res = match executed.and_then(reqwest::blocking::Response::error_for_status) {
                    Ok(res) => res,
                    Err(err) => {
                        // `error_for_status()` preserves the final URL for HTTP error responses. Capture it here so
                        // authentication retries after redirected 401 responses use the redirected base URL.
                        if let Some(actual_url) = err.url().map(reqwest::Url::as_str) {
                            if actual_url != effective_url {
                                let new_base_url = redirect::base_url(actual_url, &base_url, url.clone())?;
                                *redirected_base_url_shared.lock() = Some(new_base_url);
                            }
                        }
                        let err = match err.status() {
                            Some(status) => {
                                let kind = if status == reqwest::StatusCode::UNAUTHORIZED {
                                    std::io::ErrorKind::PermissionDenied
                                } else if status.is_server_error() {
                                    std::io::ErrorKind::ConnectionAborted
                                } else {
                                    std::io::ErrorKind::Other
                                };
                                std::io::Error::new(kind, format!("Received HTTP status {}", status.as_str()))
                            }
                            // Preserve the `reqwest::Error` as the source so the underlying cause -- e.g. a
                            // connection or TLS failure -- isn't lost. It was previously stringified, which
                            // dead-ended `source()` and hid the real reason a request failed. See #2140.
                            None => {
                                // A transport-level failure, which is where the curl backend also gives
                                // the credential helper a chance to forget wrong proxy credentials.
                                if let Some((action, authenticate)) = proxy_auth_action.take() {
                                    authenticate.lock().expect("no panics in other threads")(action.erase()).ok();
                                    cached_client = None;
                                }
                                std::io::Error::other(err)
                            }
                        };
                        headers_tx.channel.send(Err(err)).ok();
                        continue;
                    }
                };

                let actual_url = res.url().as_str();
                if actual_url != effective_url.as_str() {
                    let new_base_url = redirect::base_url(actual_url, &base_url, url)?;
                    *redirected_base_url_shared.lock() = Some(new_base_url);
                }

                // `http.saveCookies`: store what the response set back into `http.cookieFile`.
                if config.save_cookies {
                    if let (Some(path), Some((_, jar))) = (config.cookie_file.as_ref(), cookies.as_mut()) {
                        jar.absorb(
                            res.url().as_str(),
                            res.headers()
                                .get_all(reqwest::header::SET_COOKIE)
                                .iter()
                                .filter_map(|v| v.to_str().ok()),
                        );
                        jar.write_to(path).ok();
                    }
                }

                let send_headers = {
                    let headers = res.headers();
                    move || -> std::io::Result<()> {
                        for (name, value) in headers {
                            headers_tx.write_all(name.as_str().as_bytes())?;
                            headers_tx.write_all(b":")?;
                            headers_tx.write_all(value.as_bytes())?;
                            headers_tx.write_all(b"\n")?;
                        }
                        // Make sure this is an FnOnce closure to signal the remote reader we are done.
                        drop(headers_tx);
                        Ok(())
                    }
                };

                // We don't have to care if anybody is receiving the header, as a matter of fact we cannot fail sending them.
                // Thus an error means the receiver failed somehow, but might also have decided not to read headers at all. Fine with us.
                send_headers().ok();

                // reading the response body is streaming and may fail for many reasons. If so, we send the error over the response
                // body channel and that's all we can do.
                // `http.lowSpeedLimit` together with `http.lowSpeedTime` abort a transfer that stalls,
                // which curl does with `CURLOPT_LOW_SPEED_LIMIT`/`CURLOPT_LOW_SPEED_TIME`.
                let copied = if config.low_speed_limit_bytes_per_second > 0 && config.low_speed_time_seconds > 0 {
                    std::io::copy(
                        &mut LowSpeedGuard {
                            inner: &mut res,
                            limit: u64::from(config.low_speed_limit_bytes_per_second),
                            window: Duration::from_secs(config.low_speed_time_seconds),
                            window_start: Instant::now(),
                            window_bytes: 0,
                        },
                        &mut response_body_tx,
                    )
                } else {
                    std::io::copy(&mut res, &mut response_body_tx)
                };
                if let Err(err) = copied {
                    response_body_tx.channel.send(Err(err)).ok();
                }
            }
            Ok(())
        });

        Remote {
            handle: Some(handle),
            request: req_send,
            response: res_recv,
            config: http::Options::default(),
            redirected_base_url: redirected_base_url_shared_for_field,
        }
    }
}

/// utilities
impl Remote {
    fn restore_thread_after_failure(&mut self) -> http::Error {
        let err_that_brought_thread_down = self
            .handle
            .take()
            .expect("thread handle present")
            .join()
            .expect("handler thread should never panic")
            .expect_err("something should have gone wrong with curl (we join on error only)");
        *self = Remote::default();
        http::Error::InitHttpClient {
            source: Box::new(err_that_brought_thread_down),
        }
    }

    fn make_request(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        upload_body_kind: Option<PostBodyDataKind>,
    ) -> Result<http::PostResponse<pipe::Reader, pipe::Reader, pipe::Writer>, http::Error> {
        let mut header_map = reqwest::header::HeaderMap::new();
        for header_line in headers {
            insert_header(&mut header_map, header_line.as_ref());
        }
        for header_line in &self.config.extra_headers {
            insert_header(&mut header_map, header_line);
        }
        // `http.userAgent` replaces the `User-Agent` the transport puts on every request. Without this
        // it would only ever be a *default* header, which `reqwest` drops as soon as the request carries
        // one of its own -- and the transport always does.
        if let Some(user_agent) = self
            .config
            .user_agent
            .as_deref()
            .and_then(|v| reqwest::header::HeaderValue::try_from(v).ok())
        {
            header_map.insert(reqwest::header::USER_AGENT, user_agent);
        }
        if self
            .request
            .send(Request {
                url: url.to_owned(),
                base_url: base_url.to_owned(),
                headers: header_map,
                upload_body_kind,
                config: self.config.clone(),
            })
            .is_err()
        {
            return Err(self.restore_thread_after_failure());
        }

        let Response {
            headers,
            body,
            upload_body,
        } = match self.response.recv() {
            Ok(res) => res,
            Err(_) => {
                return Err(self.restore_thread_after_failure());
            }
        };

        Ok(http::PostResponse {
            post_body: upload_body,
            headers,
            body,
        })
    }
}

/// Add one `name: value` header line to `header_map`, ignoring malformed or unsupported input in `header_line`.
///
/// Git configuration may provide arbitrary extra header lines, so invalid names or values are skipped instead of
/// failing request construction. Multiple entries with the same header name are preserved to match curl behavior.
fn insert_header(header_map: &mut reqwest::header::HeaderMap, header_line: &str) {
    let Some(colon_pos) = header_line.find(':') else {
        return;
    };
    let header_name = &header_line[..colon_pos];
    let value = &header_line[colon_pos + 1..];

    if let Some((key, val)) = reqwest::header::HeaderName::from_str(header_name)
        .ok()
        .zip(reqwest::header::HeaderValue::try_from(value.trim()).ok())
    {
        header_map.append(key, val);
    }
}

impl http::Http for Remote {
    type Headers = pipe::Reader;
    type ResponseBody = pipe::Reader;
    type PostBody = pipe::Writer;

    fn get(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<http::GetResponse<Self::Headers, Self::ResponseBody>, http::Error> {
        self.make_request(url, base_url, headers, None).map(Into::into)
    }

    fn post(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        post_body_kind: PostBodyDataKind,
    ) -> Result<http::PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>, http::Error> {
        self.make_request(url, base_url, headers, Some(post_body_kind))
    }

    fn configure(&mut self, config: &dyn Any) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        if let Some(config) = config.downcast_ref::<http::Options>() {
            self.config = config.clone();
        }
        Ok(())
    }

    fn redirected_base_url(&self) -> Option<String> {
        self.redirected_base_url.lock().clone()
    }
}

pub(crate) struct Request {
    pub url: String,
    pub base_url: String,
    pub headers: reqwest::header::HeaderMap,
    pub upload_body_kind: Option<PostBodyDataKind>,
    pub config: http::Options,
}

/// A link to a thread who provides data for the contained readers.
/// The expected order is:
/// - write `upload_body`
/// - read `headers` to end
/// - read `body` to hend
pub(crate) struct Response {
    pub headers: pipe::Reader,
    pub body: pipe::Reader,
    pub upload_body: pipe::Writer,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `http.curloptResolve` accepts curl's `[+-]HOST:PORT[:ADDRESS[,ADDRESS]]`, and a `-` entry drops a
    /// mapping an earlier entry added, which is the only way `reqwest` can express curl's removal form.
    #[test]
    fn resolve_entries_are_parsed_added_and_removed_in_order() {
        let overrides = resolve_overrides(&[
            "example.com:443:127.0.0.1,127.0.0.2".into(),
            "+other.com:80:10.0.0.1".into(),
            "malformed".into(),
            "bad.com:notaport:10.0.0.1".into(),
            "noaddress.com:80".into(),
            "-other.com:80".into(),
        ]);
        assert_eq!(
            overrides,
            vec![(
                "example.com".to_string(),
                vec![
                    SocketAddr::from(([127, 0, 0, 1], 443)),
                    SocketAddr::from(([127, 0, 0, 2], 443)),
                ]
            )],
            "both addresses of the first entry survive, the added `other.com` is removed again, \
             and unparsable entries are skipped rather than failing the request"
        );
    }

    /// `http.sslVersion` maps onto what the TLS backend understands, and the SSL-era values that
    /// `rustls` has no notion of are refused instead of being silently ignored.
    #[test]
    fn ssl_versions_map_or_are_refused() {
        assert_eq!(to_tls_version(SslVersion::Default).unwrap(), None);
        assert_eq!(
            to_tls_version(SslVersion::TlsV1_2).unwrap(),
            Some(reqwest::tls::Version::TLS_1_2)
        );
        assert_eq!(
            to_tls_version(SslVersion::TlsV1).unwrap(),
            Some(reqwest::tls::Version::TLS_1_0)
        );
        assert!(matches!(to_tls_version(SslVersion::SslV3), Err(Error::Unsupported(_))));
    }

    /// The client is rebuilt only when the client-level part of the options changes: `http.extraHeader`
    /// is applied per request, while `http.userAgent` needs a new client.
    #[test]
    fn only_client_level_options_invalidate_the_client() {
        let base = http::Options::default();
        let mut per_request = base.clone();
        per_request.extra_headers = vec!["X-Extra: 1".into()];
        assert_eq!(
            ClientConfig::from_options(&base),
            ClientConfig::from_options(&per_request)
        );

        let mut client_level = base.clone();
        client_level.user_agent = Some("agent".into());
        assert_ne!(
            ClientConfig::from_options(&base),
            ClientConfig::from_options(&client_level)
        );
    }

    /// Every option that has to reach `reqwest::ClientBuilder` builds a client, so a value read from
    /// `http.*` cannot be silently dropped on the way in.
    #[test]
    fn client_level_options_reach_the_client_builder() {
        let mut opts = http::Options::default();
        opts.user_agent = Some("zvcs-test".into());
        opts.ssl_verify = false;
        opts.http_version = Some(HttpVersion::V2);
        opts.ssl_version = Some(SslVersionRangeInclusive {
            min: SslVersion::TlsV1_2,
            max: SslVersion::TlsV1_3,
        });
        opts.resolve = vec!["example.invalid:80:127.0.0.1".into()];
        opts.tcp_keepalive_idle_seconds = Some(30);
        opts.tcp_keepalive_interval_seconds = Some(5);
        opts.tcp_keepalive_count = Some(3);
        opts.min_sessions = Some(4);
        opts.proxy = Some("http://127.0.0.1:1".into());
        opts.no_proxy = Some("localhost".into());
        opts.connect_timeout = Some(Duration::from_secs(7));
        build_client(
            &ClientConfig::from_options(&opts),
            Some(("user".into(), "pass".into())),
            Arc::new(Mutex::new(RedirectAction::Stop)),
            Arc::new(Mutex::new(String::new())),
        )
        .expect("every option above is expressible with reqwest");
    }

    /// A proxy authentication scheme the backend cannot actually speak is an error, never a silent
    /// downgrade to HTTP basic.
    #[test]
    fn unsupported_proxy_auth_method_is_refused() {
        let mut opts = http::Options::default();
        opts.proxy = Some("http://127.0.0.1:1".into());
        opts.proxy_auth_method = ProxyAuthMethod::Ntlm;
        let err = build_client(
            &ClientConfig::from_options(&opts),
            Some(("user".into(), "pass".into())),
            Arc::new(Mutex::new(RedirectAction::Stop)),
            Arc::new(Mutex::new(String::new())),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Unsupported(msg) if msg.contains("ntlm")));
    }

    /// `http.sslCAInfo` and `http.sslCert` are read from disk while the client is built, so a path that
    /// does not exist fails the connection instead of being ignored.
    #[test]
    fn missing_tls_files_fail_the_client() {
        let mut opts = http::Options::default();
        opts.ssl_ca_info = Some("/definitely/not/a/ca/bundle.pem".into());
        let err = build_client(
            &ClientConfig::from_options(&opts),
            None,
            Arc::new(Mutex::new(RedirectAction::Stop)),
            Arc::new(Mutex::new(String::new())),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ReadTlsFile { kind, .. } if kind == "the CA certificate bundle"));

        let mut opts = http::Options::default();
        opts.ssl_cert = Some("/definitely/not/a/client.pem".into());
        let err = build_client(
            &ClientConfig::from_options(&opts),
            None,
            Arc::new(Mutex::new(RedirectAction::Stop)),
            Arc::new(Mutex::new(String::new())),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ReadTlsFile { kind, .. } if kind == "the client certificate"));
    }

    /// `http.lowSpeedLimit`/`http.lowSpeedTime` abort a transfer that stays under the configured rate for
    /// the configured window, and leave a fast one alone.
    #[test]
    fn low_speed_guard_aborts_a_stalled_transfer() {
        let payload = vec![0u8; 64];
        let mut guard = LowSpeedGuard {
            inner: payload.as_slice(),
            limit: 1024,
            window: Duration::from_millis(20),
            // Pretend the window already elapsed so the very first read closes it.
            window_start: Instant::now() - Duration::from_secs(5),
            window_bytes: 0,
        };
        let err = guard.read(&mut [0u8; 64]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(err.to_string().contains("Operation too slow"));

        let mut guard = LowSpeedGuard {
            inner: payload.as_slice(),
            limit: 1,
            window: Duration::from_secs(3600),
            window_start: Instant::now(),
            window_bytes: 0,
        };
        assert_eq!(guard.read(&mut [0u8; 64]).unwrap(), 64, "a window that has not elapsed never aborts");
    }
}
