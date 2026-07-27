use std::{
    any::Any,
    borrow::Cow,
    io::{BufRead, Read},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use base64::Engine;
use bstr::BStr;
pub use traits::{Error, GetResponse, Http, PostBodyDataKind, PostResponse};

use crate::{
    Protocol, Service,
    client::{
        self, MessageKind,
        blocking_io::{
            self, ExtendedBufRead, HandleProgress, RequestWriter, SetServiceResponse,
            bufread_ext::ReadlineBufRead,
            http::options::{HttpVersion, SslVersionRangeInclusive},
        },
        capabilities::blocking_recv::Handshake,
    },
    packetline::{PacketLineRef, blocking_io::StreamingPeekableIter},
};

#[cfg(feature = "http-client-curl")]
///
pub mod curl;

/// The experimental `reqwest` backend.
///
/// It doesn't support any of the shared http options yet, but can be seen as example on how to integrate blocking `http` backends.
/// There is also nothing that would prevent it from becoming a fully-featured HTTP backend except for demand and time.
#[cfg(feature = "http-client-reqwest")]
pub mod reqwest;

mod traits;

pub mod cookies;

///
pub mod options {
    /// A function to authenticate a URL.
    pub type AuthenticateFn =
        dyn FnMut(gix_credentials::helper::Action) -> gix_credentials::protocol::Result + Send + Sync;

    /// Possible settings for the `http.followRedirects` configuration option.
    #[derive(Default, Debug, Copy, Clone, PartialEq, Eq)]
    pub enum FollowRedirects {
        /// Follow only the first redirect request, most suitable for typical git requests.
        #[default]
        Initial,
        /// Follow all redirect requests from the server unconditionally
        All,
        /// Follow no redirect request.
        None,
    }

    /// Possible settings for the `http.proactiveAuth` configuration option: whether to authenticate
    /// before the server has asked for it with a `401`, and with which scheme.
    #[derive(Default, Debug, Copy, Clone, PartialEq, Eq)]
    pub enum ProactiveAuth {
        /// `none` — make the first request unauthenticated and only authenticate after a `401`.
        #[default]
        None,
        /// `basic` — ask the credential helper for HTTP basic credentials up front.
        Basic,
        /// `auto` — let the helper pick the scheme. Without helper-provided `authtype` support this
        /// is the same as `basic`, which is also what `git` falls back to for a helper that returns
        /// only a username and password.
        Auto,
    }

    /// Possible settings for the `http.emptyAuth` configuration option: whether to authenticate with
    /// an empty username *and* password, which is how `curl` is told to drive a mechanism that needs
    /// no username of its own.
    #[derive(Default, Debug, Copy, Clone, PartialEq, Eq)]
    pub enum EmptyAuth {
        /// `auto` (the default) — send empty credentials only when the server's `401` advertises a
        /// mechanism that requires them, and otherwise ask the credential helper.
        ///
        /// This backend speaks only HTTP basic authentication, and no basic-auth server ever asks for
        /// empty credentials, so this behaves exactly like [`Never`][Self::Never] here.
        #[default]
        Auto,
        /// `true` — always send empty credentials on the very first request, before any `401`.
        Always,
        /// `false` — never send empty credentials.
        Never,
    }

    /// The way to configure a proxy for authentication if a username is present in the configured proxy.
    #[derive(Default, Debug, Copy, Clone, PartialEq, Eq)]
    pub enum ProxyAuthMethod {
        /// Automatically pick a suitable authentication method.
        #[default]
        AnyAuth,
        ///HTTP basic authentication.
        Basic,
        /// Http digest authentication to prevent a password to be passed in clear text.
        Digest,
        /// GSS negotiate authentication.
        Negotiate,
        /// NTLM authentication
        Ntlm,
    }

    /// Available SSL version numbers.
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Ord, PartialOrd)]
    #[expect(missing_docs)]
    pub enum SslVersion {
        /// The implementation default, which is unknown to this layer of abstraction.
        Default,
        TlsV1,
        SslV2,
        SslV3,
        TlsV1_0,
        TlsV1_1,
        TlsV1_2,
        TlsV1_3,
    }

    /// Available HTTP version numbers.
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Ord, PartialOrd)]
    pub enum HttpVersion {
        /// Equivalent to HTTP/1.1
        V1_1,
        /// Equivalent to HTTP/2
        V2,
    }

    /// The desired range of acceptable SSL versions, or the single version to allow if both are set to the same value.
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    pub struct SslVersionRangeInclusive {
        /// The smallest allowed ssl version to use.
        pub min: SslVersion,
        /// The highest allowed ssl version to use.
        pub max: SslVersion,
    }

    impl SslVersionRangeInclusive {
        /// Return `min` and `max` fields in the right order so `min` is smaller or equal to `max`.
        pub fn min_max(&self) -> (SslVersion, SslVersion) {
            if self.min > self.max {
                (self.max, self.min)
            } else {
                (self.min, self.max)
            }
        }
    }
}

/// Options to configure http requests.
// TODO: testing most of these fields requires a lot of effort, unless special flags to introspect ongoing requests are added.
#[derive(Clone)]
pub struct Options {
    /// Headers to be added to every request.
    /// They are applied unconditionally and are expected to be valid as they occur in an HTTP request, like `header: value`, without newlines.
    ///
    /// Refers to `http.extraHeader` multi-var.
    pub extra_headers: Vec<String>,
    /// How to handle redirects.
    ///
    /// Refers to `http.followRedirects`.
    pub follow_redirects: options::FollowRedirects,
    /// Used in conjunction with `low_speed_time_seconds`, any non-0 value signals the amount of bytes per second at least to avoid
    /// aborting the connection.
    ///
    /// Refers to `http.lowSpeedLimit`.
    pub low_speed_limit_bytes_per_second: u32,
    /// Used in conjunction with `low_speed_bytes_per_second`, any non-0 value signals the amount seconds the minimal amount
    /// of bytes per second isn't reached.
    ///
    /// Refers to `http.lowSpeedTime`.
    pub low_speed_time_seconds: u64,
    /// A curl-style proxy declaration of the form `[protocol://][user[:password]@]proxyhost[:port]`.
    ///
    /// Note that an empty string means the proxy is disabled entirely.
    /// Refers to `http.proxy`.
    pub proxy: Option<String>,
    /// The comma-separated list of hosts to not send through the `proxy`, or `*` to entirely disable all proxying.
    pub no_proxy: Option<String>,
    /// The way to authenticate against the proxy if the `proxy` field contains a username.
    ///
    /// Refers to `http.proxyAuthMethod`.
    pub proxy_auth_method: options::ProxyAuthMethod,
    /// If authentication is needed for the proxy as its URL contains a username, this method must be set to provide a password
    /// for it before making the request, and to store it if the connection succeeds.
    pub proxy_authenticate: Option<(gix_credentials::helper::Action, Arc<Mutex<options::AuthenticateFn>>)>,
    /// Whether to authenticate against the *server* before it asks for it with a `401`.
    ///
    /// Refers to `http.proactiveAuth`.
    pub proactive_auth: options::ProactiveAuth,
    /// Whether to authenticate with an empty username and password instead of consulting the
    /// credential helper.
    ///
    /// Refers to `http.emptyAuth`.
    pub empty_auth: options::EmptyAuth,
    /// The credentials to authenticate the server with, used when [`proactive_auth`][Self::proactive_auth]
    /// asks for credentials up front. Without it, credentials are only obtained in reaction to a `401`.
    pub authenticate: Option<(gix_credentials::helper::Action, Arc<Mutex<options::AuthenticateFn>>)>,
    /// The `HTTP` `USER_AGENT` string presented to an `HTTP` server, notably not the user agent present to the `git` server.
    ///
    /// If not overridden, it defaults to the user agent provided by `curl`, which is a deviation from how `git` handles this.
    /// Thus it's expected from the callers to set it to their application, or use higher-level crates which make it easy to do this
    /// more correctly.
    ///
    /// Using the correct user-agent might affect how the server treats the request.
    ///
    /// Refers to `http.userAgent`.
    pub user_agent: Option<String>,
    /// The amount of time we wait until aborting a connection attempt.
    ///
    /// If `None`, this typically defaults to 2 minutes to 5 minutes.
    /// Refers to `gitoxide.http.connectTimeout`.
    pub connect_timeout: Option<std::time::Duration>,
    /// If enabled, emit additional information about connections and possibly the data received or written.
    pub verbose: bool,
    /// If set, use this path to point to a file with CA certificates to verify peers.
    pub ssl_ca_info: Option<PathBuf>,
    /// If set, use this path to point to a *directory* whose files each contain CA certificates to verify peers.
    ///
    /// Refers to `http.sslCAPath`.
    pub ssl_ca_path: Option<PathBuf>,
    /// If set, the path to a PEM file with the client certificate to authenticate with.
    ///
    /// Refers to `http.sslCert`.
    pub ssl_cert: Option<PathBuf>,
    /// If set, the path to a PEM file with the private key belonging to [`ssl_cert`][Self::ssl_cert].
    /// If unset while `ssl_cert` is set, the certificate file is expected to contain the key as well.
    ///
    /// Refers to `http.sslKey`.
    pub ssl_key: Option<PathBuf>,
    /// Pre-resolved host addresses in curl's `[+-]HOST:PORT[:ADDRESS[,ADDRESS]]` format, applied in order.
    ///
    /// Refers to `http.curloptResolve`.
    pub resolve: Vec<String>,
    /// The time an idle connection waits before TCP keepalive probes are sent, or `None` for the implementation default.
    ///
    /// Refers to `http.keepAliveIdle`.
    pub tcp_keepalive_idle_seconds: Option<u64>,
    /// The time between TCP keepalive probes, or `None` for the implementation default.
    ///
    /// Refers to `http.keepAliveInterval`.
    pub tcp_keepalive_interval_seconds: Option<u64>,
    /// The number of TCP keepalive probes to send before dropping the connection, or `None` for the implementation default.
    ///
    /// Refers to `http.keepAliveCount`.
    pub tcp_keepalive_count: Option<u32>,
    /// The number of connections to keep alive across requests, or `None` for the implementation default.
    ///
    /// Refers to `http.minSessions`.
    pub min_sessions: Option<usize>,
    /// The greatest amount of bytes a `POST` body may have before it is streamed with
    /// `Transfer-Encoding: chunked` instead of being buffered and sent with `Content-Length`.
    ///
    /// Refers to `http.postBuffer`.
    pub post_buffer_bytes: Option<u64>,
    /// How often to retry a request that was answered with `429 Too Many Requests`. `0` disables retrying.
    ///
    /// Refers to `http.maxRetries`.
    pub max_retries: u32,
    /// How long to wait before retrying a `429 Too Many Requests` response that carries no `Retry-After` header.
    ///
    /// Refers to `http.retryAfter`.
    pub retry_after_seconds: u64,
    /// The longest a single retry of a `429 Too Many Requests` response may wait. A longer requested delay fails the request.
    ///
    /// Refers to `http.maxRetryTime`.
    pub max_retry_time_seconds: u64,
    /// A file with previously stored cookie lines to send with matching requests, in the Netscape cookie
    /// file format or as plain `Set-Cookie` headers.
    ///
    /// Refers to `http.cookieFile`.
    pub cookie_file: Option<PathBuf>,
    /// Whether cookies received while requesting are stored back into [`cookie_file`][Self::cookie_file].
    ///
    /// Refers to `http.saveCookies`.
    pub save_cookies: bool,
    /// The SSL version or version range to use, or `None` to let the TLS backend determine which versions are acceptable.
    pub ssl_version: Option<SslVersionRangeInclusive>,
    /// Controls whether to perform SSL identity verification or not. Turning this off is not recommended and can lead to
    /// various security risks. An example where this may be needed is when an internal git server uses a self-signed
    /// certificate and the user accepts the associated security risks.
    pub ssl_verify: bool,
    /// The HTTP version to enforce. If unset, it is implementation defined.
    pub http_version: Option<HttpVersion>,
    /// Backend specific options, if available.
    pub backend: Option<Arc<Mutex<dyn Any + Send + Sync + 'static>>>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            extra_headers: vec![],
            follow_redirects: Default::default(),
            low_speed_limit_bytes_per_second: 0,
            low_speed_time_seconds: 0,
            proxy: None,
            no_proxy: None,
            proxy_auth_method: Default::default(),
            proxy_authenticate: None,
            proactive_auth: Default::default(),
            empty_auth: Default::default(),
            authenticate: None,
            user_agent: None,
            connect_timeout: None,
            verbose: false,
            ssl_ca_info: None,
            ssl_ca_path: None,
            ssl_cert: None,
            ssl_key: None,
            resolve: Vec::new(),
            tcp_keepalive_idle_seconds: None,
            tcp_keepalive_interval_seconds: None,
            tcp_keepalive_count: None,
            min_sessions: None,
            // `git`'s `max_request_buffer` default, see `http.postBuffer`.
            post_buffer_bytes: Some(1024 * 1024),
            max_retries: 0,
            retry_after_seconds: 0,
            // `git`'s `http_max_retry_time` default of five minutes, see `http.maxRetryTime`.
            max_retry_time_seconds: 300,
            cookie_file: None,
            save_cookies: false,
            ssl_version: None,
            ssl_verify: true,
            http_version: None,
            backend: None,
        }
    }
}

/// A transport for supporting arbitrary http clients by abstracting interactions with them into the [Http] trait.
pub struct Transport<H: Http> {
    url: String,
    user_agent_header: &'static str,
    desired_version: Protocol,
    actual_version: Protocol,
    http: H,
    service: Option<Service>,
    line_provider: Option<StreamingPeekableIter<H::ResponseBody>>,
    identity: Option<gix_sec::identity::Account>,
    trace: bool,
    /// `http.proactiveAuth`, captured from the options passed to [`configure()`][client::TransportWithoutIO::configure()].
    /// It lives here rather than in the backend because the identity the request is signed with does.
    proactive_auth: options::ProactiveAuth,
    /// `http.emptyAuth`, captured alongside it and for the same reason.
    empty_auth: options::EmptyAuth,
    /// The credential helper cascade for the server URL, invoked once before the first request when
    /// [`proactive_auth`][Self::proactive_auth] asks for it.
    authenticate: Option<(gix_credentials::helper::Action, Arc<Mutex<options::AuthenticateFn>>)>,
}

impl<H: Http> Transport<H> {
    /// Create a new instance with `http` as implementation to communicate to `url` using the given `desired_version`.
    /// Note that we will always fallback to other versions as supported by the server.
    /// If `trace` is `true`, all packetlines received or sent will be passed to the facilities of the `gix-trace` crate.
    pub fn new_http(http: H, url: gix_url::Url, desired_version: Protocol, trace: bool) -> Self {
        let identity = url
            .user()
            .zip(url.password())
            .map(|(user, pass)| gix_sec::identity::Account {
                username: user.to_string(),
                password: pass.to_string(),
                oauth_refresh_token: None,
            });
        Transport {
            url: url.to_bstring().to_string(),
            user_agent_header: concat!("User-Agent: git/oxide-", env!("CARGO_PKG_VERSION")),
            desired_version,
            actual_version: Default::default(),
            service: None,
            http,
            line_provider: None,
            identity,
            trace,
            proactive_auth: Default::default(),
            empty_auth: Default::default(),
            authenticate: None,
        }
    }
}

impl<H: Http> Transport<H> {
    /// Returns the identity that the transport uses when connecting to the remote.
    pub fn identity(&self) -> Option<&gix_sec::identity::Account> {
        self.identity.as_ref()
    }

    fn sync_redirected_base_url(&mut self) {
        Self::sync_redirected_base_url_from(&self.http, &mut self.url, &mut self.identity);
    }

    /// Update `url` with the backend's accepted redirect target, clearing credentials if the
    /// redirected authority must not reuse the original identity.
    fn sync_redirected_base_url_from(http: &H, url: &mut String, identity: &mut Option<gix_sec::identity::Account>) {
        let Some(redirected_url) = http.redirected_base_url() else {
            return;
        };
        if redirected_url == *url {
            return;
        }

        if !redirect::can_reuse_identity(&redirected_url, url) {
            *identity = None;
        }
        *url = redirected_url;
    }
}

#[cfg(any(feature = "http-client-curl", feature = "http-client-reqwest"))]
impl<H: Http + Default> Transport<H> {
    /// Create a new instance to communicate to `url` using the given `desired_version` of the `git` protocol.
    /// If `trace` is `true`, all packetlines received or sent will be passed to the facilities of the `gix-trace` crate.
    ///
    /// Note that the actual implementation depends on feature toggles.
    pub fn new(url: gix_url::Url, desired_version: Protocol, trace: bool) -> Self {
        Self::new_http(H::default(), url, desired_version, trace)
    }
}

impl<H: Http> Transport<H> {
    fn check_content_type(service: Service, kind: &str, headers: <H as Http>::Headers) -> Result<(), client::Error> {
        let wanted_content_type = format!("application/x-{}-{}", service.as_str(), kind);
        if !headers.lines().collect::<Result<Vec<_>, _>>()?.iter().any(|l| {
            let mut tokens = l.split(':');
            tokens.next().zip(tokens.next()).is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("content-type") && value.trim() == wanted_content_type
            })
        }) {
            return Err(client::Error::Http(Error::Detail {
                description: format!(
                    "Didn't find '{wanted_content_type}' header to indicate 'smart' protocol, and 'dumb' protocol is not supported."
                ),
            }));
        }
        Ok(())
    }

    fn add_basic_auth_if_present(&self, headers: &mut Vec<Cow<'_, str>>) -> Result<(), client::Error> {
        if let Some(gix_sec::identity::Account {
            username,
            password,
            oauth_refresh_token: _,
        }) = &self.identity
        {
            #[cfg(not(feature = "http-client-insecure-credentials"))]
            if self.url.starts_with("http://") {
                return Err(client::Error::AuthenticationRefused(
                    "Will not send credentials in clear text over http",
                ));
            }
            headers.push(Cow::Owned(format!(
                "Authorization: Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
            )));
        }
        Ok(())
    }
}

fn append_url(base: &str, suffix: &str) -> String {
    let mut buf = base.to_owned();
    if base.as_bytes().last() != Some(&b'/') {
        buf.push('/');
    }
    buf.push_str(suffix);
    buf
}

impl<H: Http> client::TransportWithoutIO for Transport<H> {
    fn set_identity(&mut self, identity: gix_sec::identity::Account) -> Result<(), client::Error> {
        self.identity = Some(identity);
        Ok(())
    }

    fn to_url(&self) -> Cow<'_, BStr> {
        Cow::Borrowed(self.url.as_str().into())
    }

    fn connection_persists_across_multiple_requests(&self) -> bool {
        false
    }

    fn configure(&mut self, config: &dyn Any) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        // `http.proactiveAuth` is decided here rather than in the backend: it changes *which*
        // identity the transport signs the very first request with, and the identity lives on
        // this type. The backend still receives the same options.
        if let Some(options) = config.downcast_ref::<Options>() {
            self.proactive_auth = options.proactive_auth;
            self.empty_auth = options.empty_auth;
            self.authenticate = options.authenticate.clone();
        }
        self.http.configure(config)
    }
}

impl<H: Http> Transport<H> {
    /// `http.proactiveAuth`: obtain credentials from the helper *before* the first request, so the
    /// server never sees an unauthenticated attempt. A `401`-driven retry is what happens without it.
    ///
    /// Nothing happens when the URL already carried an identity, when the setting is `none`, or when
    /// no credential helper was wired up.
    ///
    /// `http.emptyAuth=true` wins over it, exactly as `git help config` documents ("If
    /// `http.emptyAuth` is set to true, this value has no effect"): an empty username and password
    /// go out on the first request and the credential helper is never consulted. Its other two
    /// values, `auto` and `false`, differ only for mechanisms that need no username of their own —
    /// `GSS-Negotiate` and friends, which this backend cannot speak — so both leave the `401`-driven
    /// credential-helper flow untouched.
    fn authenticate_proactively(&mut self) -> Result<(), client::Error> {
        if self.identity.is_some() {
            return Ok(());
        }
        if self.empty_auth == options::EmptyAuth::Always {
            self.identity = Some(gix_sec::identity::Account {
                username: String::new(),
                password: String::new(),
                oauth_refresh_token: None,
            });
            return Ok(());
        }
        if self.proactive_auth == options::ProactiveAuth::None {
            return Ok(());
        }
        let Some((action, authenticate)) = self.authenticate.clone() else {
            return Ok(());
        };
        let outcome = authenticate.lock().expect("no panics in other threads")(action)
            .map_err(|err| client::Error::Http(Error::Detail {
                description: format!("Could not obtain credentials for proactive authentication: {err}"),
            }))?;
        if let Some(outcome) = outcome {
            self.identity = Some(outcome.identity);
        }
        Ok(())
    }
}

impl<H: Http> blocking_io::Transport for Transport<H> {
    fn handshake<'a>(
        &mut self,
        service: Service,
        extra_parameters: &'a [(&'a str, Option<&'a str>)],
    ) -> Result<SetServiceResponse<'_>, client::Error> {
        self.authenticate_proactively()?;
        let url = append_url(self.url.as_ref(), &format!("info/refs?service={}", service.as_str()));
        let static_headers = [Cow::Borrowed(self.user_agent_header)];
        let mut dynamic_headers = Vec::<Cow<'_, str>>::new();
        if self.desired_version != Protocol::V1 || !extra_parameters.is_empty() {
            let mut parameters = if self.desired_version != Protocol::V1 {
                let mut p = format!("version={}", self.desired_version as usize);
                if !extra_parameters.is_empty() {
                    p.push(':');
                }
                p
            } else {
                String::new()
            };
            parameters.push_str(
                &extra_parameters
                    .iter()
                    .map(|(key, value)| match value {
                        Some(value) => format!("{key}={value}"),
                        None => key.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(":"),
            );
            dynamic_headers.push(format!("Git-Protocol: {parameters}").into());
        }
        self.add_basic_auth_if_present(&mut dynamic_headers)?;
        let GetResponse { headers, mut body } = self
            .http
            .get(url.as_ref(), &self.url, static_headers.iter().chain(&dynamic_headers))
            .map_err(|err| {
                self.sync_redirected_base_url();
                client::Error::from(err)
            })?;
        if let Err(err) = <Transport<H>>::check_content_type(service, "advertisement", headers) {
            const MAX_ERROR_BODY_DRAIN_BYTES: u64 = 1024 * 1024;
            std::io::copy(
                &mut body.by_ref().take(MAX_ERROR_BODY_DRAIN_BYTES),
                &mut std::io::sink(),
            )
            .ok();
            self.sync_redirected_base_url();
            return Err(err);
        }
        self.sync_redirected_base_url();

        let line_reader = self
            .line_provider
            .get_or_insert_with(|| StreamingPeekableIter::new(body, &[PacketLineRef::Flush], self.trace));

        // the service announcement is only sent sometimes depending on the exact server/protocol version/used protocol (http?)
        // eat the announcement when its there to avoid errors later (and check that the correct service was announced).
        // Ignore the announcement otherwise.
        let line_ = line_reader
            .peek_line()
            .ok_or(client::Error::ExpectedLine("capabilities, version or service"))???;
        let line = line_.as_text().ok_or(client::Error::ExpectedLine("text"))?;

        if let Some(announced_service) = line.as_bstr().strip_prefix(b"# service=") {
            if announced_service != service.as_str().as_bytes() {
                return Err(client::Error::Http(Error::Detail {
                    description: format!(
                        "Expected to see service {:?}, but got {:?}",
                        service.as_str(),
                        announced_service
                    ),
                }));
            }

            line_reader.as_read().read_to_end(&mut Vec::new())?;
        }

        let Handshake {
            capabilities,
            refs,
            protocol: actual_protocol,
        } = Handshake::from_lines_with_version_detection(line_reader)?;
        self.actual_version = actual_protocol;
        self.service = Some(service);
        Ok(SetServiceResponse {
            actual_protocol,
            capabilities,
            refs,
        })
    }

    fn request(
        &mut self,
        write_mode: client::WriteMode,
        on_into_read: MessageKind,
        trace: bool,
    ) -> Result<RequestWriter<'_>, client::Error> {
        let service = self.service.ok_or(client::Error::MissingHandshake)?;
        let url = append_url(&self.url, service.as_str());
        let static_headers = &[
            Cow::Borrowed(self.user_agent_header),
            Cow::Owned(format!("Content-Type: application/x-{}-request", service.as_str())),
            format!("Accept: application/x-{}-result", service.as_str()).into(),
        ];
        let mut dynamic_headers = Vec::new();
        self.add_basic_auth_if_present(&mut dynamic_headers)?;
        if self.actual_version != Protocol::V1 {
            dynamic_headers.push(Cow::Owned(format!(
                "Git-Protocol: version={}",
                self.actual_version as usize
            )));
        }

        let all_headers = static_headers.iter().chain(&dynamic_headers);
        let PostResponse {
            headers,
            body,
            post_body,
        } = self
            .http
            .post(&url, &self.url, all_headers, write_mode.into())
            .map_err(|err| {
                self.sync_redirected_base_url();
                client::Error::from(err)
            })?;
        self.sync_redirected_base_url();
        let line_provider = self
            .line_provider
            .as_mut()
            .expect("handshake to have been called first");
        line_provider.replace(body);
        Ok(RequestWriter::new_from_bufread(
            post_body,
            Box::new(HeadersThenBody::<H, _> {
                service,
                headers: Some(headers),
                body: line_provider.as_read_without_sidebands(),
            }),
            write_mode,
            on_into_read,
            trace,
        ))
    }
}

struct HeadersThenBody<H: Http, B: Unpin> {
    service: Service,
    headers: Option<H::Headers>,
    body: B,
}

impl<H: Http, B: Unpin> HeadersThenBody<H, B> {
    fn handle_headers(&mut self) -> std::io::Result<()> {
        if let Some(headers) = self.headers.take() {
            <Transport<H>>::check_content_type(self.service, "result", headers).map_err(std::io::Error::other)?;
        }
        Ok(())
    }
}

impl<H: Http, B: Read + Unpin> Read for HeadersThenBody<H, B> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.handle_headers()?;
        self.body.read(buf)
    }
}

impl<H: Http, B: BufRead + Unpin> BufRead for HeadersThenBody<H, B> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.handle_headers()?;
        self.body.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        self.body.consume(amt);
    }
}

impl<H: Http, B: ReadlineBufRead + Unpin> ReadlineBufRead for HeadersThenBody<H, B> {
    fn readline(&mut self) -> Option<std::io::Result<Result<PacketLineRef<'_>, gix_packetline::decode::Error>>> {
        if let Err(err) = self.handle_headers() {
            return Some(Err(err));
        }
        self.body.readline()
    }

    fn readline_str(&mut self, line: &mut String) -> std::io::Result<usize> {
        self.handle_headers()?;
        self.body.readline_str(line)
    }
}

impl<'a, H: Http, B: ExtendedBufRead<'a> + Unpin> ExtendedBufRead<'a> for HeadersThenBody<H, B> {
    fn set_progress_handler(&mut self, handle_progress: Option<HandleProgress<'a>>) {
        self.body.set_progress_handler(handle_progress);
    }

    fn peek_data_line(&mut self) -> Option<std::io::Result<Result<&[u8], client::Error>>> {
        if let Err(err) = self.handle_headers() {
            return Some(Err(err));
        }
        self.body.peek_data_line()
    }

    fn reset(&mut self, version: Protocol) {
        self.body.reset(version);
    }

    fn stopped_at(&self) -> Option<MessageKind> {
        self.body.stopped_at()
    }
}

/// Connect to the given `url` via HTTP/S using the `desired_version` of the `git` protocol, with `http` as implementation.
/// If `trace` is `true`, all packetlines received or sent will be passed to the facilities of the `gix-trace` crate.
#[cfg(all(feature = "http-client", not(feature = "http-client-curl")))]
pub fn connect_http<H: Http>(http: H, url: gix_url::Url, desired_version: Protocol, trace: bool) -> Transport<H> {
    Transport::new_http(http, url, desired_version, trace)
}

/// Connect to the given `url` via HTTP/S using the `desired_version` of the `git` protocol.
/// If `trace` is `true`, all packetlines received or sent will be passed to the facilities of the `gix-trace` crate.
#[cfg(any(feature = "http-client-curl", feature = "http-client-reqwest"))]
pub fn connect<H: Http + Default>(url: gix_url::Url, desired_version: Protocol, trace: bool) -> Transport<H> {
    Transport::new(url, desired_version, trace)
}

///
pub mod redirect;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::TransportWithoutIO;

    /// An [`Http`] that never performs a request; enough to drive the parts of [`Transport`] that
    /// decide *what* to send before anything is sent.
    #[derive(Default)]
    struct NoHttp;

    impl Http for NoHttp {
        type Headers = std::io::Cursor<Vec<u8>>;
        type ResponseBody = std::io::Cursor<Vec<u8>>;
        type PostBody = Vec<u8>;

        fn get(
            &mut self,
            _url: &str,
            _base_url: &str,
            _headers: impl IntoIterator<Item = impl AsRef<str>>,
        ) -> Result<GetResponse<Self::Headers, Self::ResponseBody>, Error> {
            unreachable!("no request is made in these tests")
        }

        fn post(
            &mut self,
            _url: &str,
            _base_url: &str,
            _headers: impl IntoIterator<Item = impl AsRef<str>>,
            _body: PostBodyDataKind,
        ) -> Result<PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>, Error> {
            unreachable!("no request is made in these tests")
        }

        fn configure(&mut self, _config: &dyn Any) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
            Ok(())
        }
    }

    fn transport() -> Transport<NoHttp> {
        Transport::new_http(
            NoHttp,
            gix_url::parse("https://example.com/repo.git".into()).expect("valid url"),
            Protocol::V2,
            false,
        )
    }

    /// A credential helper that hands out one fixed identity, and counts how often it ran.
    fn fixed_identity(
        calls: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Option<(gix_credentials::helper::Action, Arc<Mutex<options::AuthenticateFn>>)> {
        let action = gix_credentials::helper::Action::get_for_url("https://example.com/repo.git");
        let authenticate = move |action: gix_credentials::helper::Action| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match action {
                gix_credentials::helper::Action::Get(ctx) => Ok(Some(gix_credentials::protocol::Outcome {
                    identity: gix_sec::identity::Account {
                        username: "user".into(),
                        password: "pass".into(),
                        oauth_refresh_token: None,
                    },
                    next: gix_credentials::helper::NextAction::from(ctx),
                })),
                _ => Ok(None),
            }
        };
        Some((action, Arc::new(Mutex::new(authenticate)) as Arc<Mutex<options::AuthenticateFn>>))
    }

    /// Without `http.proactiveAuth` the transport stays anonymous until a `401` forces the issue,
    /// and the credential helper is never even asked.
    #[test]
    fn no_proactive_auth_leaves_the_first_request_anonymous() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut opts = Options::default();
        opts.authenticate = fixed_identity(calls.clone());
        let mut transport = transport();
        transport.configure(&opts).expect("options are accepted");
        transport.authenticate_proactively().expect("nothing to do");
        assert!(transport.identity().is_none());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// `http.proactiveAuth=basic` asks the helper before the first request, so the very first
    /// request already carries an `Authorization` header.
    #[test]
    fn proactive_auth_obtains_credentials_up_front() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut opts = Options::default();
        opts.proactive_auth = options::ProactiveAuth::Basic;
        opts.authenticate = fixed_identity(calls.clone());
        let mut transport = transport();
        transport.configure(&opts).expect("options are accepted");
        transport.authenticate_proactively().expect("credentials are available");
        assert_eq!(transport.identity().map(|i| i.username.clone()), Some("user".into()));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let mut headers = Vec::new();
        transport.add_basic_auth_if_present(&mut headers).expect("https");
        assert_eq!(headers, vec!["Authorization: Basic dXNlcjpwYXNz"]);
    }

    /// `http.emptyAuth=true` sends an empty username *and* password and never consults the helper,
    /// and it wins over `http.proactiveAuth`, which `git help config` says has no effect then.
    #[test]
    fn empty_auth_wins_over_proactive_auth_and_skips_the_helper() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut opts = Options::default();
        opts.empty_auth = options::EmptyAuth::Always;
        opts.proactive_auth = options::ProactiveAuth::Basic;
        opts.authenticate = fixed_identity(calls.clone());
        let mut transport = transport();
        transport.configure(&opts).expect("options are accepted");
        transport.authenticate_proactively().expect("no helper needed");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        let mut headers = Vec::new();
        transport.add_basic_auth_if_present(&mut headers).expect("https");
        // base64(":"), curl's `CURLOPT_USERPWD=":"`.
        assert_eq!(headers, vec!["Authorization: Basic Og=="]);
    }

    /// The two values of `http.emptyAuth` that only matter for mechanisms this backend cannot speak
    /// leave the `401`-driven flow exactly as it was.
    #[test]
    fn empty_auth_auto_and_false_change_nothing() {
        for mode in [options::EmptyAuth::Auto, options::EmptyAuth::Never] {
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut opts = Options::default();
            opts.empty_auth = mode;
            opts.authenticate = fixed_identity(calls.clone());
            let mut transport = transport();
            transport.configure(&opts).expect("options are accepted");
            transport.authenticate_proactively().expect("nothing to do");
            assert!(transport.identity().is_none(), "{mode:?} stays anonymous");
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    }
}
