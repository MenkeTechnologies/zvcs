#![allow(clippy::result_large_err)]
use std::any::Any;

use crate::bstr::BStr;

impl crate::Repository {
    /// Produce configuration suitable for `url`, as differentiated by its protocol/scheme, to be passed to a transport instance via
    /// [configure()][gix_transport::client::TransportWithoutIO::configure()] (via `&**config` to pass the contained `Any` and not the `Box`).
    /// `None` is returned if there is no known configuration. If `remote_name` is not `None`, the remote's name may contribute to
    /// configuration overrides, typically for the HTTP transport.
    ///
    /// Note that the caller may cast the instance themselves to modify it before passing it on.
    ///
    /// For transports that support proxy authentication, the
    /// [default authentication method](crate::config::Snapshot::credential_helpers()) will be used with the url of the proxy
    /// if it contains a user name.
    #[cfg_attr(
        not(any(
            feature = "blocking-http-transport-reqwest",
            feature = "blocking-http-transport-curl"
        )),
        allow(unused_variables)
    )]
    pub fn transport_options<'a>(
        &self,
        url: impl Into<&'a BStr>,
        remote_name: Option<&BStr>,
    ) -> Result<Option<Box<dyn Any>>, crate::config::transport::Error> {
        let url = gix_url::parse(url.into())?;
        use gix_url::Scheme::*;

        match &url.scheme {
            Http | Https => {
                #[cfg(not(any(
                    feature = "blocking-http-transport-reqwest",
                    feature = "blocking-http-transport-curl"
                )))]
                {
                    Ok(None)
                }
                #[cfg(any(
                    feature = "blocking-http-transport-reqwest",
                    feature = "blocking-http-transport-curl"
                ))]
                {
                    use std::sync::{Arc, Mutex};

                    use gix_transport::client::blocking_io::http::{
                        self,
                        options::{ProxyAuthMethod, SslVersion, SslVersionRangeInclusive},
                    };

                    use crate::{
                        bstr::BString,
                        config,
                        config::{
                            cache::util::ApplyLeniency,
                            tree::{Key, Remote, gitoxide},
                        },
                    };
                    fn try_to_string(
                        v: BString,
                        lenient: bool,
                        key_str: impl Into<BString>,
                        key: &'static config::tree::keys::String,
                    ) -> Result<Option<String>, config::transport::Error> {
                        key.try_into_string(v)
                            .map_err(|err| config::transport::Error::IllformedUtf8 {
                                source: err,
                                key: key_str.into(),
                            })
                            .map(Some)
                            .with_leniency(lenient)
                    }

                    fn proxy_auth_method(
                        value_and_key: Option<(BString, BString, &'static config::tree::http::ProxyAuthMethod)>,
                    ) -> Result<ProxyAuthMethod, config::transport::Error> {
                        let value = value_and_key
                            .map(|(method, key, key_type)| {
                                key_type.try_into_proxy_auth_method(method).map_err(|err| {
                                    config::transport::http::Error::InvalidProxyAuthMethod { source: err, key }
                                })
                            })
                            .transpose()?
                            .unwrap_or_default();
                        Ok(value)
                    }

                    fn ssl_version(
                        value: Option<BString>,
                        key: &'static config::tree::http::SslVersion,
                        lenient: bool,
                    ) -> Result<Option<SslVersion>, config::transport::Error> {
                        value
                            .filter(|v| !v.is_empty())
                            .map(|v| {
                                key.try_into_ssl_version(v)
                                    .map_err(crate::config::transport::http::Error::from)
                            })
                            .transpose()
                            .with_leniency(lenient)
                            .map_err(Into::into)
                    }

                    fn proxy(
                        value: Option<(BString, BString, &'static config::tree::keys::String)>,
                        lenient: bool,
                    ) -> Result<Option<String>, config::transport::Error> {
                        Ok(value
                            .and_then(|(v, k, key)| try_to_string(v, lenient, k.clone(), key).transpose())
                            .transpose()?
                            .map(|mut proxy| {
                                if !proxy.trim().is_empty() && !proxy.contains("://") {
                                    proxy.insert_str(0, "http://");
                                    proxy
                                } else {
                                    proxy
                                }
                            }))
                    }

                    let mut opts = http::Options::default();
                    let config = &self.config.resolved;
                    let mut trusted_only = self.filter_config_section();
                    let lenient = self.config.lenient_config;
                    // Every `http.*` read below goes through this so that an `http.<url>.*` subsection
                    // matching the URL overrides the plain section, as `git help config` describes.
                    let hc = HttpKeys::new(config, &url, trusted_only);
                    opts.extra_headers = {
                        let key = "http.extraHeader";
                        debug_assert_eq!(key, &config::tree::Http::EXTRA_HEADER.logical_name());
                        hc.strings(config::tree::Http::EXTRA_HEADER.name)
                            .map(|values| config::tree::Http::EXTRA_HEADER.try_into_extra_header(values))
                            .transpose()
                            .map_err(|err| config::transport::Error::IllformedUtf8 {
                                source: err,
                                key: key.into(),
                            })?
                            .unwrap_or_default()
                    };

                    opts.follow_redirects = {
                        let name = config::tree::Http::FOLLOW_REDIRECTS.name;

                        config::tree::Http::FOLLOW_REDIRECTS
                            .try_into_follow_redirects(hc.string(name).unwrap_or_default(), || {
                                hc.boolean(name).transpose().with_leniency(lenient)
                            })
                            .map_err(config::transport::http::Error::InvalidFollowRedirects)?
                    };

                    opts.low_speed_time_seconds = config::tree::Http::LOW_SPEED_TIME
                        .try_into_u64(hc.integer(config::tree::Http::LOW_SPEED_TIME.name).transpose())
                        .with_leniency(lenient)
                        .map_err(config::transport::http::Error::from)?
                        .unwrap_or_default();
                    opts.low_speed_limit_bytes_per_second = config::tree::Http::LOW_SPEED_LIMIT
                        .try_into_u32(hc.integer(config::tree::Http::LOW_SPEED_LIMIT.name).transpose())
                        .with_leniency(lenient)
                        .map_err(config::transport::http::Error::from)?
                        .unwrap_or_default();
                    opts.proxy = proxy(
                        remote_name
                            .and_then(|name| {
                                config
                                    .string_filter(
                                        &format!("remote.{}.{}", name, Remote::PROXY.name),
                                        &mut trusted_only,
                                    )
                                    .map(|v| (v, format!("remote.{name}.proxy").into(), &Remote::PROXY))
                            })
                            .or_else(|| {
                                let key = "http.proxy";
                                debug_assert_eq!(key, config::tree::Http::PROXY.logical_name());
                                let http_proxy = hc
                                    .string(config::tree::Http::PROXY.name)
                                    .map(|v| (v, key.into(), &config::tree::Http::PROXY))
                                    .or_else(|| {
                                        let key = "gitoxide.http.proxy";
                                        debug_assert_eq!(key, gitoxide::Http::PROXY.logical_name());
                                        config
                                            .string_filter(key, &mut trusted_only)
                                            .map(|v| (v, key.into(), &gitoxide::Http::PROXY))
                                    });
                                if url.scheme == Https {
                                    http_proxy.or_else(|| {
                                        let key = "gitoxide.https.proxy";
                                        debug_assert_eq!(key, gitoxide::Https::PROXY.logical_name());
                                        config
                                            .string_filter(key, &mut trusted_only)
                                            .map(|v| (v, key.into(), &gitoxide::Https::PROXY))
                                    })
                                } else {
                                    http_proxy
                                }
                            })
                            .or_else(|| {
                                let key = "gitoxide.http.allProxy";
                                debug_assert_eq!(key, gitoxide::Http::ALL_PROXY.logical_name());
                                config
                                    .string_filter(key, &mut trusted_only)
                                    .map(|v| (v, key.into(), &gitoxide::Http::ALL_PROXY))
                            }),
                        lenient,
                    )?;
                    {
                        let key = "gitoxide.http.noProxy";
                        debug_assert_eq!(key, gitoxide::Http::NO_PROXY.logical_name());
                        opts.no_proxy = config
                            .string_filter(key, &mut trusted_only)
                            .and_then(|v| try_to_string(v, lenient, key, &gitoxide::Http::NO_PROXY).transpose())
                            .transpose()?;
                    }
                    opts.proxy_auth_method = proxy_auth_method({
                        let key = "gitoxide.http.proxyAuthMethod";
                        debug_assert_eq!(key, gitoxide::Http::PROXY_AUTH_METHOD.logical_name());
                        config
                            .string_filter(key, &mut trusted_only)
                            .map(|v| (v, key.into(), &gitoxide::Http::PROXY_AUTH_METHOD))
                            .or_else(|| {
                                remote_name
                                    .and_then(|name| {
                                        config
                                            .string_filter(&format!("remote.{name}.proxyAuthMethod"), &mut trusted_only)
                                            .map(|v| {
                                                (
                                                    v,
                                                    format!("remote.{name}.proxyAuthMethod").into(),
                                                    &Remote::PROXY_AUTH_METHOD,
                                                )
                                            })
                                    })
                                    .or_else(|| {
                                        let key = "http.proxyAuthMethod";
                                        debug_assert_eq!(key, config::tree::Http::PROXY_AUTH_METHOD.logical_name());
                                        hc.string(config::tree::Http::PROXY_AUTH_METHOD.name)
                                            .map(|v| (v, key.into(), &config::tree::Http::PROXY_AUTH_METHOD))
                                    })
                            })
                    })?;
                    opts.proxy_authenticate = opts
                        .proxy
                        .as_deref()
                        .filter(|url| !url.is_empty())
                        .map(|url| gix_url::parse(url.into()))
                        .transpose()?
                        .filter(|url| url.user().is_some())
                        .map(|url| -> Result<_, config::transport::http::Error> {
                            let (mut cascade, action_with_normalized_url, prompt_opts) =
                                self.config_snapshot().credential_helpers(url)?;
                            Ok((
                                action_with_normalized_url,
                                Arc::new(Mutex::new(move |action| cascade.invoke(action, prompt_opts.clone())))
                                    as Arc<Mutex<http::options::AuthenticateFn>>,
                            ))
                        })
                        .transpose()?;
                    {
                        // `http.emptyAuth`: authenticate with an empty username and password rather
                        // than asking the credential helper at all.
                        let key = "http.emptyAuth";
                        debug_assert_eq!(key, config::tree::Http::EMPTY_AUTH.logical_name());
                        let name = config::tree::Http::EMPTY_AUTH.name;
                        if hc.string(name).is_some() || hc.boolean(name).is_some() {
                            opts.empty_auth = config::tree::Http::EMPTY_AUTH
                                .try_into_empty_auth(hc.string(name).unwrap_or_default(), || {
                                    hc.boolean(name).transpose().with_leniency(lenient)
                                })
                                .map_err(config::transport::http::Error::InvalidEmptyAuth)?;
                        }
                    }

                    {
                        // `http.proactiveAuth`: authenticate before the server asks with a `401`.
                        // The credential cascade is built here, where the repository's helpers are
                        // reachable, and invoked by the transport before its first request.
                        let key = "http.proactiveAuth";
                        debug_assert_eq!(key, config::tree::Http::PROACTIVE_AUTH.logical_name());
                        opts.proactive_auth = hc
                            .string(config::tree::Http::PROACTIVE_AUTH.name)
                            .map(|v| config::tree::Http::PROACTIVE_AUTH.try_into_proactive_auth(v))
                            .unwrap_or_default();
                        if opts.proactive_auth != http::options::ProactiveAuth::None {
                            let (mut cascade, action_with_normalized_url, prompt_opts) = self
                                .config_snapshot()
                                .credential_helpers(url.clone())
                                .map_err(config::transport::http::Error::ConfigureAuthenticate)?;
                            opts.authenticate = Some((
                                action_with_normalized_url,
                                Arc::new(Mutex::new(move |action| cascade.invoke(action, prompt_opts.clone())))
                                    as Arc<Mutex<http::options::AuthenticateFn>>,
                            ));
                        }
                    }
                    opts.connect_timeout = {
                        let key = "gitoxide.http.connectTimeout";
                        debug_assert_eq!(key, gitoxide::Http::CONNECT_TIMEOUT.logical_name());
                        gitoxide::Http::CONNECT_TIMEOUT
                            .try_into_duration(config.integer_filter(key, &mut trusted_only))
                            .map_err(crate::config::transport::http::Error::from)
                            .with_leniency(lenient)?
                    };
                    {
                        let key = "http.userAgent";
                        opts.user_agent = hc
                            .string(config::tree::Http::USER_AGENT.name)
                            .and_then(|v| try_to_string(v, lenient, key, &config::tree::Http::USER_AGENT).transpose())
                            .transpose()?
                            .or_else(|| Some(crate::env::agent().into()));
                    }

                    {
                        opts.http_version = hc
                            .string(config::tree::Http::VERSION.name)
                            .map(|v| {
                                config::tree::Http::VERSION
                                    .try_into_http_version(v)
                                    .map_err(config::transport::http::Error::InvalidHttpVersion)
                            })
                            .transpose()?;
                    }

                    {
                        opts.verbose = config
                            .boolean_filter(gitoxide::Http::VERBOSE, &mut trusted_only)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                    }

                    let may_use_cainfo = {
                        config::tree::Http::SCHANNEL_USE_SSL_CA_INFO
                            .enrich_error(
                                hc.boolean(config::tree::Http::SCHANNEL_USE_SSL_CA_INFO.name)
                                    .transpose(),
                            )
                            .with_leniency(lenient)
                            .map_err(config::transport::http::Error::from)?
                            .unwrap_or(true)
                    };

                    let interpolated = |value: Option<gix_config::Path>,
                                            key: &'static str|
                     -> Result<Option<std::path::PathBuf>, config::transport::Error> {
                        value
                            .map(|p| {
                                use crate::config::cache::interpolate_context;
                                p.interpolate(interpolate_context(
                                    self.install_dir().ok().as_deref(),
                                    self.config.home_dir().as_deref(),
                                ))
                            })
                            .transpose()
                            .with_leniency(lenient)
                            .map_err(|err| config::transport::Error::InterpolatePath { source: err, key })
                    };

                    if may_use_cainfo {
                        opts.ssl_ca_info = interpolated(
                            hc.path(config::tree::Http::SSL_CA_INFO.name),
                            "http.sslCAInfo",
                        )?;
                    }

                    {
                        opts.ssl_version = ssl_version(
                            hc.string(config::tree::Http::SSL_VERSION.name),
                            &config::tree::Http::SSL_VERSION,
                            lenient,
                        )?
                        .map(|v| SslVersionRangeInclusive { min: v, max: v });
                        let min_max = ssl_version(
                            config.string_filter("gitoxide.http.sslVersionMin", &mut trusted_only),
                            &gitoxide::Http::SSL_VERSION_MIN,
                            lenient,
                        )
                        .and_then(|min| {
                            ssl_version(
                                config.string_filter("gitoxide.http.sslVersionMax", &mut trusted_only),
                                &gitoxide::Http::SSL_VERSION_MAX,
                                lenient,
                            )
                            .map(|max| min.zip(max))
                        })?;
                        if let Some((min, max)) = min_max {
                            let v = opts.ssl_version.get_or_insert(SslVersionRangeInclusive {
                                min: SslVersion::TlsV1_3,
                                max: SslVersion::TlsV1_3,
                            });
                            v.min = min;
                            v.max = max;
                        }
                    }

                    {
                        let key = "gitoxide.http.sslNoVerify";
                        let ssl_no_verify = config::tree::gitoxide::Http::SSL_NO_VERIFY
                            .enrich_error(config.boolean_filter(key, &mut trusted_only))
                            .with_leniency(lenient)
                            .map_err(config::transport::http::Error::from)?
                            .unwrap_or_default();

                        if ssl_no_verify {
                            opts.ssl_verify = false;
                        } else {
                            opts.ssl_verify = config::tree::Http::SSL_VERIFY
                                .enrich_error(hc.boolean(config::tree::Http::SSL_VERIFY.name).transpose())
                                .with_leniency(lenient)
                                .map_err(config::transport::http::Error::from)?
                                .unwrap_or(true);
                        }
                    }

                    opts.ssl_ca_path = interpolated(
                        hc.path(config::tree::Http::SSL_CA_PATH.name),
                        "http.sslCAPath",
                    )?;
                    opts.ssl_cert = interpolated(hc.path(config::tree::Http::SSL_CERT.name), "http.sslCert")?;
                    opts.ssl_key = interpolated(hc.path(config::tree::Http::SSL_KEY.name), "http.sslKey")?;

                    {
                        // An empty `http.cookieFile` means "accept new cookies but read none", which
                        // leaves nothing to load and nothing to save.
                        opts.cookie_file =
                            interpolated(hc.path(config::tree::Http::COOKIE_FILE.name), "http.cookieFile")?
                                .filter(|p| !p.as_os_str().is_empty());
                        opts.save_cookies = opts.cookie_file.is_some()
                            && config::tree::Http::SAVE_COOKIES
                                .enrich_error(hc.boolean(config::tree::Http::SAVE_COOKIES.name).transpose())
                                .with_leniency(lenient)
                                .map_err(config::transport::http::Error::from)?
                                .unwrap_or_default();
                    }

                    {
                        let key = "http.curloptResolve";
                        debug_assert_eq!(key, config::tree::Http::CURLOPT_RESOLVE.logical_name());
                        opts.resolve = hc
                            .strings(config::tree::Http::CURLOPT_RESOLVE.name)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|v| {
                                config::tree::Http::CURLOPT_RESOLVE.try_into_string(v).map_err(|err| {
                                    config::transport::Error::IllformedUtf8 {
                                        source: err,
                                        key: key.into(),
                                    }
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()
                            .map(Some)
                            .with_leniency(lenient)?
                            .unwrap_or_default();
                    }

                    {
                        let unsigned = |tree: &'static config::tree::keys::UnsignedInteger|
                         -> Result<Option<u64>, config::transport::Error> {
                            Ok(tree
                                .try_into_u64(hc.integer(tree.name).transpose())
                                .with_leniency(lenient)
                                .map_err(config::transport::http::Error::from)?)
                        };
                        opts.tcp_keepalive_idle_seconds = unsigned(&config::tree::Http::KEEP_ALIVE_IDLE)?;
                        opts.tcp_keepalive_interval_seconds = unsigned(&config::tree::Http::KEEP_ALIVE_INTERVAL)?;
                        opts.tcp_keepalive_count =
                            unsigned(&config::tree::Http::KEEP_ALIVE_COUNT)?.map(|v| v.try_into().unwrap_or(u32::MAX));
                        opts.min_sessions =
                            unsigned(&config::tree::Http::MIN_SESSIONS)?.map(|v| v.try_into().unwrap_or(usize::MAX));
                        if let Some(bytes) = unsigned(&config::tree::Http::POST_BUFFER)? {
                            opts.post_buffer_bytes = Some(bytes);
                        }
                        if let Some(retries) = unsigned(&config::tree::Http::MAX_RETRIES)? {
                            opts.max_retries = retries.try_into().unwrap_or(u32::MAX);
                        }
                        if let Some(seconds) = unsigned(&config::tree::Http::RETRY_AFTER)? {
                            opts.retry_after_seconds = seconds;
                        }
                        if let Some(seconds) = unsigned(&config::tree::Http::MAX_RETRY_TIME)? {
                            opts.max_retry_time_seconds = seconds;
                        }
                    }

                    // `git`'s `http.c` reads the config first and then lets the environment override it,
                    // so these always win over any `http.*` value.
                    apply_environment_overrides(&mut opts)?;

                    #[cfg(feature = "blocking-http-transport-curl")]
                    {
                        let schannel_check_revoke = config::tree::Http::SCHANNEL_CHECK_REVOKE
                            .enrich_error(hc.boolean(config::tree::Http::SCHANNEL_CHECK_REVOKE.name).transpose())
                            .with_leniency(lenient)
                            .map_err(config::transport::http::Error::from)?;
                        let backend =
                            gix_protocol::transport::client::blocking_io::http::curl::Options { schannel_check_revoke };
                        opts.backend =
                            Some(Arc::new(Mutex::new(backend)) as Arc<Mutex<dyn Any + Send + Sync + 'static>>);
                    }

                    Ok(Some(Box::new(opts)))
                }
            }
            File | Git | Ssh | Ext(_) => Ok(None),
        }
    }
}

/// Reads of the `http.*` keys that first consult the `http.<url>.*` subsections applying to a URL.
///
/// `git help config` documents `http.<url>.*` as "Any of the http.* options above can be applied
/// selectively to some URLs", with the strongest match winning; [`subsections`][Self::subsections] is
/// exactly that list of matching patterns in decreasing precedence, and the plain `http.*` section is
/// the last resort.
#[cfg(any(
    feature = "blocking-http-transport-reqwest",
    feature = "blocking-http-transport-curl"
))]
struct HttpKeys<'a> {
    config: &'a gix_config::File,
    subsections: Vec<crate::bstr::BString>,
    filter: fn(&gix_config::file::Metadata) -> bool,
}

#[cfg(any(
    feature = "blocking-http-transport-reqwest",
    feature = "blocking-http-transport-curl"
))]
impl HttpKeys<'_> {
    /// Collect the `http.<pattern>` subsections of `config` that apply to `url`, strongest first.
    fn new<'a>(
        config: &'a gix_config::File,
        url: &gix_url::Url,
        filter: fn(&gix_config::file::Metadata) -> bool,
    ) -> HttpKeys<'a> {
        use crate::bstr::{BString, ByteSlice};

        let mut ranked: Vec<(usize, u8, BString)> = Vec::new();
        let mut filter = filter;
        if let Some(sections) = config.sections_by_name_and_filter("http", &mut filter) {
            for section in sections {
                let Some(pattern) = section.header().subsection_name() else {
                    continue;
                };
                if ranked.iter().any(|(_, _, seen)| seen == pattern) {
                    continue;
                }
                if let Some((path_len, user)) = url_match_rank(pattern.to_str().ok(), url) {
                    ranked.push((path_len, user, pattern.to_owned()));
                }
            }
        }
        // Decreasing precedence: a longer path match first, then a pattern that names a user.
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        HttpKeys {
            config,
            subsections: ranked.into_iter().map(|(_, _, name)| name).collect(),
            filter,
        }
    }

    /// Run `read` against each matching subsection in turn and finally against the plain `http` section,
    /// yielding the first value that is present.
    fn first<T>(&self, mut read: impl FnMut(Option<&crate::bstr::BStr>) -> Option<T>) -> Option<T> {
        use crate::bstr::ByteSlice;

        self.subsections
            .iter()
            .find_map(|name| read(Some(name.as_bstr())))
            .or_else(|| read(None))
    }

    fn string(&self, value_name: &str) -> Option<crate::bstr::BString> {
        self.first(|sub| self.config.string_filter_by("http", sub, value_name, self.filter))
    }

    fn strings(&self, value_name: &str) -> Option<Vec<crate::bstr::BString>> {
        self.first(|sub| self.config.strings_filter_by("http", sub, value_name, self.filter))
    }

    fn path(&self, value_name: &str) -> Option<gix_config::Path> {
        self.first(|sub| self.config.path_filter_by("http", sub, value_name, self.filter))
    }

    fn integer(&self, value_name: &str) -> Option<Result<i64, gix_config::value::Error>> {
        self.first(|sub| {
            self.config
                .integer_filter_by("http", sub, value_name, self.filter)
                .transpose()
        })
    }

    fn boolean(&self, value_name: &str) -> Option<Result<bool, gix_config::value::Error>> {
        self.first(|sub| {
            self.config
                .boolean_filter_by("http", sub, value_name, self.filter)
                .transpose()
        })
    }
}

/// Rank an `http.<pattern>` subsection against `url`, or return `None` when it does not apply.
///
/// The fields are compared in the order `git help config` lists them for `http.<url>.*`: the scheme
/// exactly, the host with support for a single `*` label, the port after defaulting it for the scheme,
/// the path exactly or as a slash-delimited prefix, and the user name. The returned pair is the
/// precedence of the match — the number of path elements it pinned down, then whether it named a user.
#[cfg(any(
    feature = "blocking-http-transport-reqwest",
    feature = "blocking-http-transport-curl"
))]
fn url_match_rank(pattern: Option<&str>, url: &gix_url::Url) -> Option<(usize, u8)> {
    let pattern = gix_url::parse(pattern?.into()).ok()?;
    if pattern.scheme != url.scheme {
        return None;
    }

    let (pattern_host, url_host) = (pattern.host(), url.host());
    match (pattern_host, url_host) {
        (Some(p), Some(u)) if !host_matches(p, u) => return None,
        (Some(_), None) | (None, Some(_)) => return None,
        _ => {}
    }

    if pattern.port_or_default() != url.port_or_default() {
        return None;
    }

    let path_elements = |path: &crate::bstr::BStr| -> Vec<Vec<u8>> {
        path.split(|b| *b == b'/')
            .filter(|e| !e.is_empty())
            .map(<[u8]>::to_vec)
            .collect()
    };
    let pattern_path = path_elements(pattern.path.as_ref());
    let url_path = path_elements(url.path.as_ref());
    if pattern_path.len() > url_path.len() || pattern_path[..] != url_path[..pattern_path.len()] {
        return None;
    }

    let user = match pattern.user() {
        Some(pattern_user) => {
            if url.user() != Some(pattern_user) {
                return None;
            }
            1
        }
        None => 0,
    };
    Some((pattern_path.len(), user))
}

/// Match a host from an `http.<url>.*` pattern against `host`, where a leading `*.` stands for exactly
/// one label, so `*.example.com` matches `foo.example.com` but not `foo.bar.example.com`.
#[cfg(any(
    feature = "blocking-http-transport-reqwest",
    feature = "blocking-http-transport-curl"
))]
fn host_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(suffix) => host
            .split_once('.')
            .is_some_and(|(label, rest)| !label.is_empty() && rest == suffix),
        None => pattern == host,
    }
}

/// Let the `GIT_*` variables that `git`'s `http.c` consults override what the `http.*` configuration
/// produced in `opts`, which is the precedence `git` documents: "Environment variable settings always
/// override any matches".
///
/// `GIT_SSL_NO_VERIFY`, `http_proxy`, `https_proxy`, `ALL_PROXY` and `NO_PROXY` are not handled here as
/// they already enter through `gitoxide.http.*` when the configuration is assembled.
#[cfg(any(
    feature = "blocking-http-transport-reqwest",
    feature = "blocking-http-transport-curl"
))]
fn apply_environment_overrides(
    opts: &mut gix_transport::client::blocking_io::http::Options,
) -> Result<(), crate::config::transport::Error> {
    use gix_transport::client::blocking_io::http::options::SslVersionRangeInclusive;

    use crate::config;

    fn var(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }
    fn var_u64(name: &str) -> Option<u64> {
        var(name).and_then(|v| v.trim().parse().ok())
    }

    if let Some(agent) = var("GIT_HTTP_USER_AGENT") {
        opts.user_agent = Some(agent);
    }
    if let Some(path) = var("GIT_SSL_CAINFO") {
        opts.ssl_ca_info = Some(path.into());
    }
    if let Some(path) = var("GIT_SSL_CAPATH") {
        opts.ssl_ca_path = Some(path.into());
    }
    if let Some(path) = var("GIT_SSL_CERT") {
        opts.ssl_cert = Some(path.into());
    }
    if let Some(path) = var("GIT_SSL_KEY") {
        opts.ssl_key = Some(path.into());
    }
    if let Some(version) = var("GIT_SSL_VERSION") {
        let version = config::tree::Http::SSL_VERSION
            .try_into_ssl_version(version.as_str())
            .map_err(config::transport::http::Error::from)?;
        opts.ssl_version = Some(SslVersionRangeInclusive {
            min: version,
            max: version,
        });
    }
    if let Some(method) = var("GIT_HTTP_PROXY_AUTHMETHOD") {
        opts.proxy_auth_method = config::tree::Http::PROXY_AUTH_METHOD
            .try_into_proxy_auth_method(method.as_str())
            .map_err(|err| config::transport::http::Error::InvalidProxyAuthMethod {
                source: err,
                key: "GIT_HTTP_PROXY_AUTHMETHOD".into(),
            })?;
    }
    if let Some(seconds) = var_u64("GIT_HTTP_KEEPALIVE_IDLE") {
        opts.tcp_keepalive_idle_seconds = Some(seconds);
    }
    if let Some(seconds) = var_u64("GIT_HTTP_KEEPALIVE_INTERVAL") {
        opts.tcp_keepalive_interval_seconds = Some(seconds);
    }
    if let Some(count) = var_u64("GIT_HTTP_KEEPALIVE_COUNT") {
        opts.tcp_keepalive_count = Some(count.try_into().unwrap_or(u32::MAX));
    }
    if let Some(retries) = var_u64("GIT_HTTP_MAX_RETRIES") {
        opts.max_retries = retries.try_into().unwrap_or(u32::MAX);
    }
    if let Some(seconds) = var_u64("GIT_HTTP_RETRY_AFTER") {
        opts.retry_after_seconds = seconds;
    }
    if let Some(seconds) = var_u64("GIT_HTTP_MAX_RETRY_TIME") {
        opts.max_retry_time_seconds = seconds;
    }
    Ok(())
}

#[cfg(test)]
#[cfg(any(
    feature = "blocking-http-transport-reqwest",
    feature = "blocking-http-transport-curl"
))]
mod tests {
    use super::{host_matches, url_match_rank};

    fn rank(pattern: &str, url: &str) -> Option<(usize, u8)> {
        url_match_rank(Some(pattern), &gix_url::parse(url.into()).expect("test url parses"))
    }

    /// A `*` in an `http.<url>.*` host pattern stands for exactly one label.
    #[test]
    fn host_wildcard_spans_one_label() {
        assert!(host_matches("*.example.com", "foo.example.com"));
        assert!(!host_matches("*.example.com", "foo.bar.example.com"));
        assert!(!host_matches("*.example.com", "example.com"));
        assert!(host_matches("example.com", "example.com"));
        assert!(!host_matches("example.com", "other.com"));
    }

    /// Scheme and port must match exactly, with the port defaulted from the scheme first.
    #[test]
    fn scheme_and_port_must_match() {
        assert_eq!(rank("https://example.com", "https://example.com/r.git"), Some((0, 0)));
        assert_eq!(rank("http://example.com", "https://example.com/r.git"), None);
        assert_eq!(
            rank("https://example.com:443", "https://example.com/r.git"),
            Some((0, 0)),
            "an omitted port defaults to the scheme's before matching"
        );
        assert_eq!(rank("https://example.com:8443", "https://example.com/r.git"), None);
    }

    /// The path matches exactly or on a slash boundary, and a longer match ranks higher.
    #[test]
    fn path_matches_on_slash_boundaries_and_longer_wins() {
        assert_eq!(rank("https://example.com/foo", "https://example.com/foo/bar"), Some((1, 0)));
        assert_eq!(
            rank("https://example.com/foo/bar", "https://example.com/foo/bar"),
            Some((2, 0))
        );
        assert_eq!(
            rank("https://example.com/foo", "https://example.com/foobar"),
            None,
            "a prefix that is not on a slash boundary does not match"
        );
        assert_eq!(rank("https://example.com/foo/bar", "https://example.com/foo"), None);
    }

    /// A pattern with a user name must match it, and outranks one without.
    #[test]
    fn user_name_is_matched_and_ranks_above_a_pattern_without_one() {
        assert_eq!(
            rank("https://user@example.com", "https://user@example.com/r.git"),
            Some((0, 1))
        );
        assert_eq!(rank("https://user@example.com", "https://example.com/r.git"), None);
        assert_eq!(
            rank("https://other@example.com", "https://user@example.com/r.git"),
            None
        );
        assert_eq!(
            rank("https://example.com", "https://user@example.com/r.git"),
            Some((0, 0)),
            "a pattern without a user still matches a URL that has one"
        );
    }
}
