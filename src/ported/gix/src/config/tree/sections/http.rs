use crate::{
    config,
    config::tree::{Http, Key, Section, keys},
};

impl Http {
    /// The `http.sslVersion` key.
    pub const SSL_VERSION: SslVersion = SslVersion::new_ssl_version("sslVersion", &config::Tree::HTTP)
        .with_environment_override("GIT_SSL_VERSION")
        .with_deviation(
            "accepts the new 'default' value which means to use the curl default just like the empty string does",
        );
    /// The `http.sslVerify` key.
    pub const SSL_VERIFY: keys::Boolean = keys::Boolean::new_boolean("sslVerify", &config::Tree::HTTP)
        .with_note("also see the `gitoxide.http.sslNoVerify` key");
    /// The `http.proxy` key.
    pub const PROXY: keys::String =
        keys::String::new_string("proxy", &config::Tree::HTTP).with_deviation("fails on strings with illformed UTF-8");
    /// The `http.proxyAuthMethod` key.
    pub const PROXY_AUTH_METHOD: ProxyAuthMethod =
        ProxyAuthMethod::new_proxy_auth_method("proxyAuthMethod", &config::Tree::HTTP)
            .with_deviation("implemented like git, but never actually tried");
    /// The `http.version` key.
    pub const VERSION: Version = Version::new_with_validate("version", &config::Tree::HTTP, validate::Version)
        .with_deviation("fails on illformed UTF-8");
    /// The `http.userAgent` key.
    pub const USER_AGENT: keys::String = keys::String::new_string("userAgent", &config::Tree::HTTP)
        .with_environment_override("GIT_HTTP_USER_AGENT")
        .with_deviation("fails on illformed UTF-8");
    /// The `http.cookieFile` key.
    pub const COOKIE_FILE: keys::Path = keys::Path::new_path("cookieFile", &config::Tree::HTTP);
    /// The `http.saveCookies` key.
    pub const SAVE_COOKIES: keys::Boolean = keys::Boolean::new_boolean("saveCookies", &config::Tree::HTTP)
        .with_note("has no effect unless `http.cookieFile` is set to a non-empty path");
    /// The `http.sslCAPath` key.
    pub const SSL_CA_PATH: keys::Path =
        keys::Path::new_path("sslCAPath", &config::Tree::HTTP).with_environment_override("GIT_SSL_CAPATH");
    /// The `http.sslCert` key.
    pub const SSL_CERT: keys::Path =
        keys::Path::new_path("sslCert", &config::Tree::HTTP).with_environment_override("GIT_SSL_CERT");
    /// The `http.sslKey` key.
    pub const SSL_KEY: keys::Path =
        keys::Path::new_path("sslKey", &config::Tree::HTTP).with_environment_override("GIT_SSL_KEY");
    /// The `http.curloptResolve` multi-var.
    pub const CURLOPT_RESOLVE: keys::String = keys::String::new_string("curloptResolve", &config::Tree::HTTP)
        .with_deviation("fails on illformed UTF-8, and entries without a parsable IP address are ignored");
    /// The `http.keepAliveIdle` key.
    pub const KEEP_ALIVE_IDLE: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("keepAliveIdle", &config::Tree::HTTP)
            .with_environment_override("GIT_HTTP_KEEPALIVE_IDLE");
    /// The `http.keepAliveInterval` key.
    pub const KEEP_ALIVE_INTERVAL: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("keepAliveInterval", &config::Tree::HTTP)
            .with_environment_override("GIT_HTTP_KEEPALIVE_INTERVAL");
    /// The `http.keepAliveCount` key.
    pub const KEEP_ALIVE_COUNT: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("keepAliveCount", &config::Tree::HTTP)
            .with_environment_override("GIT_HTTP_KEEPALIVE_COUNT");
    /// The `http.minSessions` key.
    pub const MIN_SESSIONS: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("minSessions", &config::Tree::HTTP).with_note(
            "maps onto the number of connections kept alive between requests, as there is no notion of a curl session",
        );
    /// The `http.postBuffer` key.
    pub const POST_BUFFER: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("postBuffer", &config::Tree::HTTP);
    /// The `http.maxRetries` key.
    pub const MAX_RETRIES: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("maxRetries", &config::Tree::HTTP)
            .with_environment_override("GIT_HTTP_MAX_RETRIES");
    /// The `http.retryAfter` key.
    pub const RETRY_AFTER: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("retryAfter", &config::Tree::HTTP)
            .with_environment_override("GIT_HTTP_RETRY_AFTER");
    /// The `http.maxRetryTime` key.
    pub const MAX_RETRY_TIME: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("maxRetryTime", &config::Tree::HTTP)
            .with_environment_override("GIT_HTTP_MAX_RETRY_TIME");
    /// The `http.extraHeader` key.
    pub const EXTRA_HEADER: ExtraHeader =
        ExtraHeader::new_with_validate("extraHeader", &config::Tree::HTTP, validate::ExtraHeader)
            .with_deviation("fails on illformed UTF-8, without leniency");
    /// The `http.followRedirects` key.
    pub const FOLLOW_REDIRECTS: FollowRedirects =
        FollowRedirects::new_with_validate("followRedirects", &config::Tree::HTTP, validate::FollowRedirects);
    /// The `http.lowSpeedTime` key.
    pub const LOW_SPEED_TIME: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("lowSpeedTime", &config::Tree::HTTP)
            .with_deviation("fails on negative values");
    /// The `http.lowSpeedLimit` key.
    pub const LOW_SPEED_LIMIT: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("lowSpeedLimit", &config::Tree::HTTP)
            .with_deviation("fails on negative values");
    /// The `http.schannelUseSSLCAInfo` key.
    pub const SCHANNEL_USE_SSL_CA_INFO: keys::Boolean =
        keys::Boolean::new_boolean("schannelUseSSLCAInfo", &config::Tree::HTTP)
            .with_deviation("only used as switch internally to turn off using the sslCAInfo, unconditionally. If unset, it has no effect, whereas in `git` it defaults to false.");
    /// The `http.sslCAInfo` key.
    pub const SSL_CA_INFO: keys::Path =
        keys::Path::new_path("sslCAInfo", &config::Tree::HTTP).with_environment_override("GIT_SSL_CAINFO");
    /// The `http.schannelCheckRevoke` key.
    pub const SCHANNEL_CHECK_REVOKE: keys::Boolean =
        keys::Boolean::new_boolean("schannelCheckRevoke", &config::Tree::HTTP);
    /// The `http.proactiveAuth` key.
    pub const PROACTIVE_AUTH: ProactiveAuth =
        keys::Any::new_with_validate("proactiveAuth", &config::Tree::HTTP, validate::ProactiveAuth);
    /// The `http.emptyAuth` key.
    pub const EMPTY_AUTH: EmptyAuth =
        keys::Any::new_with_validate("emptyAuth", &config::Tree::HTTP, validate::EmptyAuth);
}

impl Section for Http {
    fn name(&self) -> &str {
        "http"
    }

    fn keys(&self) -> &[&dyn Key] {
        &[
            &Self::SSL_VERSION,
            &Self::SSL_VERIFY,
            &Self::PROXY,
            &Self::PROXY_AUTH_METHOD,
            &Self::VERSION,
            &Self::USER_AGENT,
            &Self::EXTRA_HEADER,
            &Self::FOLLOW_REDIRECTS,
            &Self::LOW_SPEED_TIME,
            &Self::LOW_SPEED_LIMIT,
            &Self::SCHANNEL_USE_SSL_CA_INFO,
            &Self::SSL_CA_INFO,
            &Self::SCHANNEL_CHECK_REVOKE,
            &Self::COOKIE_FILE,
            &Self::SAVE_COOKIES,
            &Self::SSL_CA_PATH,
            &Self::SSL_CERT,
            &Self::SSL_KEY,
            &Self::CURLOPT_RESOLVE,
            &Self::KEEP_ALIVE_IDLE,
            &Self::KEEP_ALIVE_INTERVAL,
            &Self::KEEP_ALIVE_COUNT,
            &Self::MIN_SESSIONS,
            &Self::POST_BUFFER,
            &Self::MAX_RETRIES,
            &Self::RETRY_AFTER,
            &Self::MAX_RETRY_TIME,
            &Self::PROACTIVE_AUTH,
            &Self::EMPTY_AUTH,
        ]
    }
}

/// The `http.proactiveAuth` key.
pub type ProactiveAuth = keys::Any<validate::ProactiveAuth>;

/// The `http.emptyAuth` key.
pub type EmptyAuth = keys::Any<validate::EmptyAuth>;

/// The `http.followRedirects` key.
pub type FollowRedirects = keys::Any<validate::FollowRedirects>;

/// The `http.extraHeader` key.
pub type ExtraHeader = keys::Any<validate::ExtraHeader>;

/// The `http.sslVersion` key, as well as others of the same type.
pub type SslVersion = keys::Any<validate::SslVersion>;

/// The `http.proxyAuthMethod` key, as well as others of the same type.
pub type ProxyAuthMethod = keys::Any<validate::ProxyAuthMethod>;

/// The `http.version` key.
pub type Version = keys::Any<validate::Version>;

mod key_impls {
    use crate::config::tree::{
        Section,
        http::{ProxyAuthMethod, SslVersion},
        keys,
    };

    impl SslVersion {
        pub const fn new_ssl_version(name: &'static str, section: &'static dyn Section) -> Self {
            keys::Any::new_with_validate(name, section, super::validate::SslVersion)
        }
    }

    impl ProxyAuthMethod {
        pub const fn new_proxy_auth_method(name: &'static str, section: &'static dyn Section) -> Self {
            keys::Any::new_with_validate(name, section, super::validate::ProxyAuthMethod)
        }
    }

    #[cfg(any(
        feature = "blocking-http-transport-reqwest",
        feature = "blocking-http-transport-curl"
    ))]
    impl crate::config::tree::http::FollowRedirects {
        /// Convert `value` into the redirect specification, or query the same value as `boolean`
        /// for additional possible input values.
        ///
        /// Note that `boolean` only queries the underlying key as boolean, which is a necessity to handle
        /// empty booleans correctly, that is those without a value separator.
        pub fn try_into_follow_redirects(
            &'static self,
            value: impl gix_utils::AsBStr,
            boolean: impl FnOnce() -> Result<Option<bool>, gix_config::value::Error>,
        ) -> Result<
            crate::protocol::transport::client::blocking_io::http::options::FollowRedirects,
            crate::config::key::GenericErrorWithValue,
        > {
            use crate::{bstr::ByteSlice, protocol::transport::client::blocking_io::http::options::FollowRedirects};
            let value = value.as_bstr();
            Ok(if value.as_bstr().as_bytes() == b"initial" {
                FollowRedirects::Initial
            } else if let Some(value) = boolean().map_err(|err| {
                crate::config::key::GenericErrorWithValue::from_value(self, value.into()).with_source(err)
            })? {
                if value {
                    FollowRedirects::All
                } else {
                    FollowRedirects::None
                }
            } else {
                FollowRedirects::Initial
            })
        }
    }

    impl super::ExtraHeader {
        /// Convert a list of values into extra-headers, while failing entirely on illformed UTF-8.
        pub fn try_into_extra_header(
            &'static self,
            values: Vec<impl gix_utils::AsBStr>,
        ) -> Result<Vec<String>, crate::config::string::Error> {
            let mut out = Vec::with_capacity(values.len());
            for value in values {
                let value = value.as_bstr();
                if value.is_empty() {
                    out.clear();
                } else {
                    out.push(self.try_into_string(value)?);
                }
            }
            Ok(out)
        }
    }

    #[cfg(any(
        feature = "blocking-http-transport-reqwest",
        feature = "blocking-http-transport-curl"
    ))]
    impl super::Version {
        pub fn try_into_http_version(
            &'static self,
            value: impl gix_utils::AsBStr,
        ) -> Result<
            gix_protocol::transport::client::blocking_io::http::options::HttpVersion,
            crate::config::key::GenericErrorWithValue,
        > {
            use gix_protocol::transport::client::blocking_io::http::options::HttpVersion;

            use crate::bstr::ByteSlice;
            let value = value.as_bstr();
            Ok(match value.as_bstr().as_bytes() {
                b"HTTP/1.1" => HttpVersion::V1_1,
                b"HTTP/2" => HttpVersion::V2,
                _ => {
                    return Err(crate::config::key::GenericErrorWithValue::from_value(
                        self,
                        value.into(),
                    ));
                }
            })
        }
    }

    #[cfg(any(
        feature = "blocking-http-transport-reqwest",
        feature = "blocking-http-transport-curl"
    ))]
    impl ProxyAuthMethod {
        pub fn try_into_proxy_auth_method(
            &'static self,
            value: impl gix_utils::AsBStr,
        ) -> Result<
            gix_protocol::transport::client::blocking_io::http::options::ProxyAuthMethod,
            crate::config::key::GenericErrorWithValue,
        > {
            use gix_protocol::transport::client::blocking_io::http::options::ProxyAuthMethod;

            use crate::bstr::ByteSlice;
            let value = value.as_bstr();
            Ok(match value.as_bstr().as_bytes() {
                b"anyauth" => ProxyAuthMethod::AnyAuth,
                b"basic" => ProxyAuthMethod::Basic,
                b"digest" => ProxyAuthMethod::Digest,
                b"negotiate" => ProxyAuthMethod::Negotiate,
                b"ntlm" => ProxyAuthMethod::Ntlm,
                _ => {
                    return Err(crate::config::key::GenericErrorWithValue::from_value(
                        self,
                        value.into(),
                    ));
                }
            })
        }
    }

    #[cfg(any(
        feature = "blocking-http-transport-reqwest",
        feature = "blocking-http-transport-curl"
    ))]
    impl super::ProactiveAuth {
        /// Convert `value` into the proactive-authentication mode, per `git help config`'s
        /// `basic`/`auto`/`none` for `http.proactiveAuth`.
        ///
        /// An unrecognized value leaves the setting at its default, which is what `git`'s
        /// `http_options()` does — it only `warning()`s and keeps `http_proactive_auth` as it was.
        pub fn try_into_proactive_auth(
            &'static self,
            value: impl gix_utils::AsBStr,
        ) -> gix_protocol::transport::client::blocking_io::http::options::ProactiveAuth {
            use gix_protocol::transport::client::blocking_io::http::options::ProactiveAuth;

            use crate::bstr::ByteSlice;
            match value.as_bstr().as_bytes() {
                b"basic" => ProactiveAuth::Basic,
                b"auto" => ProactiveAuth::Auto,
                _ => ProactiveAuth::None,
            }
        }
    }

    #[cfg(any(
        feature = "blocking-http-transport-reqwest",
        feature = "blocking-http-transport-curl"
    ))]
    impl super::EmptyAuth {
        /// Convert `value` into the empty-authentication mode. `git`'s `http_options()` reads
        /// `http.emptyAuth` as the literal `auto`, and anything else through `git_config_bool`, so
        /// `yes`/`on`/`1` reach the same place as `true` and a key with no value at all is true.
        pub fn try_into_empty_auth(
            &'static self,
            value: impl gix_utils::AsBStr,
            boolean: impl FnOnce() -> Result<Option<bool>, gix_config::value::Error>,
        ) -> Result<
            gix_protocol::transport::client::blocking_io::http::options::EmptyAuth,
            crate::config::key::GenericErrorWithValue,
        > {
            use gix_protocol::transport::client::blocking_io::http::options::EmptyAuth;

            use crate::bstr::ByteSlice;
            let value = value.as_bstr();
            if value.as_bytes() == b"auto" {
                return Ok(EmptyAuth::Auto);
            }
            let empty_value = value.is_empty();
            match boolean().map_err(|err| {
                crate::config::key::GenericErrorWithValue::from_value(self, value.into()).with_source(err)
            })? {
                Some(true) => Ok(EmptyAuth::Always),
                Some(false) => Ok(EmptyAuth::Never),
                // A key without a value separator (`[http] emptyAuth`) is git's implicit true; a
                // value that is not a boolean at all is one `git_config_bool()` dies on, and under
                // a lenient configuration it must not be read as an enabling one.
                None if empty_value => Ok(EmptyAuth::Always),
                None => Ok(EmptyAuth::Auto),
            }
        }
    }

    #[cfg(any(
        feature = "blocking-http-transport-reqwest",
        feature = "blocking-http-transport-curl"
    ))]
    impl SslVersion {
        pub fn try_into_ssl_version(
            &'static self,
            value: impl gix_utils::AsBStr,
        ) -> Result<
            gix_protocol::transport::client::blocking_io::http::options::SslVersion,
            crate::config::ssl_version::Error,
        > {
            use gix_protocol::transport::client::blocking_io::http::options::SslVersion::*;

            use crate::bstr::ByteSlice;
            let value = value.as_bstr();
            Ok(match value.as_bstr().as_bytes() {
                b"default" | b"" => Default,
                b"tlsv1" => TlsV1,
                b"sslv2" => SslV2,
                b"sslv3" => SslV3,
                b"tlsv1.0" => TlsV1_0,
                b"tlsv1.1" => TlsV1_1,
                b"tlsv1.2" => TlsV1_2,
                b"tlsv1.3" => TlsV1_3,
                _ => return Err(crate::config::ssl_version::Error::from_value(self, value.into())),
            })
        }
    }
}

pub mod validate {
    use std::error::Error;

    use crate::{
        bstr::{BStr, ByteSlice},
        config::tree::keys::Validate,
    };

    pub struct SslVersion;
    impl Validate for SslVersion {
        fn validate(&self, _value: &BStr) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
            #[cfg(any(
                feature = "blocking-http-transport-reqwest",
                feature = "blocking-http-transport-curl"
            ))]
            super::Http::SSL_VERSION.try_into_ssl_version(_value)?;

            Ok(())
        }
    }

    /// `git` accepts any value here — an unknown one is a warning, not a failure — so there is
    /// nothing to reject.
    pub struct ProactiveAuth;
    impl Validate for ProactiveAuth {
        fn validate(&self, _value: &BStr) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
            Ok(())
        }
    }

    pub struct EmptyAuth;
    impl Validate for EmptyAuth {
        fn validate(&self, _value: &BStr) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
            #[cfg(any(
                feature = "blocking-http-transport-reqwest",
                feature = "blocking-http-transport-curl"
            ))]
            super::Http::EMPTY_AUTH.try_into_empty_auth(_value, || {
                gix_config::Boolean::try_from(_value).map(|b| Some(b.0)).map_err(Into::into)
            })?;

            Ok(())
        }
    }

    pub struct ProxyAuthMethod;
    impl Validate for ProxyAuthMethod {
        fn validate(&self, _value: &BStr) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
            #[cfg(any(
                feature = "blocking-http-transport-reqwest",
                feature = "blocking-http-transport-curl"
            ))]
            super::Http::PROXY_AUTH_METHOD.try_into_proxy_auth_method(_value)?;

            Ok(())
        }
    }

    pub struct Version;
    impl Validate for Version {
        fn validate(&self, _value: &BStr) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
            #[cfg(any(
                feature = "blocking-http-transport-reqwest",
                feature = "blocking-http-transport-curl"
            ))]
            super::Http::VERSION.try_into_http_version(_value)?;

            Ok(())
        }
    }

    pub struct ExtraHeader;
    impl Validate for ExtraHeader {
        fn validate(&self, value: &BStr) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
            value.to_str()?;
            Ok(())
        }
    }

    pub struct FollowRedirects;
    impl Validate for FollowRedirects {
        fn validate(&self, _value: &BStr) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
            #[cfg(any(
                feature = "blocking-http-transport-reqwest",
                feature = "blocking-http-transport-curl"
            ))]
            super::Http::FOLLOW_REDIRECTS
                .try_into_follow_redirects(_value, || gix_config::Boolean::try_from(_value).map(|b| Some(b.0)))?;
            Ok(())
        }
    }
}
