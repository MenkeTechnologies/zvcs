//! A cookie jar for `http.cookieFile` and `http.saveCookies`.
//!
//! `git` hands both keys to curl as `CURLOPT_COOKIEFILE` and `CURLOPT_COOKIEJAR`, so the file format is
//! curl's: the Netscape/Mozilla cookie file, or a plain sequence of `Set-Cookie:` headers. Backends that
//! have no cookie engine of their own drive this type instead, which reads that same format, selects the
//! cookies that apply to a URL, and — with `http.saveCookies` — writes the jar back out.

use std::{
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// One cookie, with the fields the Netscape cookie file stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    /// The domain the cookie belongs to, without a leading dot.
    pub domain: String,
    /// Whether the cookie also applies to subdomains of `domain`.
    pub include_subdomains: bool,
    /// The path prefix the cookie applies to.
    pub path: String,
    /// Whether the cookie may only be sent over a secure transport.
    pub secure: bool,
    /// The expiry as a unix timestamp, where `0` means the cookie is a session cookie.
    pub expires: u64,
    pub(crate) name: String,
    pub(crate) value: String,
}

/// The cookies read from `http.cookieFile`, updated from responses when `http.saveCookies` is set.
#[derive(Debug, Default, Clone)]
pub struct Jar {
    cookies: Vec<Cookie>,
}

impl Jar {
    /// Read the jar at `path`, ignoring a file that does not exist as curl does.
    pub fn from_file(path: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Jar::default();
        };
        let mut jar = Jar::default();
        for line in content.lines() {
            // `#HttpOnly_` marks an http-only cookie; any other `#` line is a comment.
            let line = match line.strip_prefix("#HttpOnly_") {
                Some(rest) => rest,
                None if line.starts_with('#') => continue,
                None => line,
            };
            let line = line.trim_end_matches(['\r', '\n']);
            if line.trim().is_empty() {
                continue;
            }
            // The file may also be a plain sequence of response headers, which curl accepts too.
            if let Some(rest) = line
                .strip_prefix("Set-Cookie:")
                .or_else(|| line.strip_prefix("set-cookie:"))
            {
                if let Some(cookie) = parse_set_cookie(rest, None) {
                    jar.insert(cookie);
                }
                continue;
            }
            if let Some(cookie) = parse_netscape_line(line) {
                jar.insert(cookie);
            }
        }
        jar
    }

    /// Replace any cookie with the same name, domain and path, which is how a later definition wins.
    pub fn insert(&mut self, cookie: Cookie) {
        if let Some(existing) = self
            .cookies
            .iter_mut()
            .find(|c| c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path)
        {
            *existing = cookie;
        } else {
            self.cookies.push(cookie);
        }
    }

    /// Return the `Cookie` header value for `url`, or `None` when no cookie applies.
    pub fn header_for(&self, url: &str) -> Option<String> {
        let (secure, host, path) = split_url(url)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let pairs: Vec<_> = self
            .cookies
            .iter()
            .filter(|c| c.expires == 0 || c.expires > now)
            .filter(|c| !c.secure || secure)
            .filter(|c| domain_matches(&c.domain, c.include_subdomains, &host))
            .filter(|c| path_matches(&c.path, &path))
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }

    /// Take the `Set-Cookie` values a response carried, defaulting their domain to `url`'s host.
    pub fn absorb<'a>(&mut self, url: &str, set_cookie_values: impl IntoIterator<Item = &'a str>) {
        let host = split_url(url).map(|(_, host, _)| host);
        for value in set_cookie_values {
            if let Some(cookie) = parse_set_cookie(value, host.as_deref()) {
                self.insert(cookie);
            }
        }
    }

    /// Write the jar to `path` in the Netscape format curl produces for `CURLOPT_COOKIEJAR`.
    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        let mut out = Vec::new();
        writeln!(out, "# Netscape HTTP Cookie File")?;
        for c in &self.cookies {
            writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                c.domain,
                if c.include_subdomains { "TRUE" } else { "FALSE" },
                c.path,
                if c.secure { "TRUE" } else { "FALSE" },
                c.expires,
                c.name,
                c.value
            )?;
        }
        std::fs::write(path, out)
    }
}

/// Parse one tab-separated Netscape cookie file line.
fn parse_netscape_line(line: &str) -> Option<Cookie> {
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() < 7 {
        return None;
    }
    let domain = fields[0].trim_start_matches('.').to_owned();
    Some(Cookie {
        include_subdomains: fields[1].eq_ignore_ascii_case("TRUE") || fields[0].starts_with('.'),
        domain,
        path: fields[2].to_owned(),
        secure: fields[3].eq_ignore_ascii_case("TRUE"),
        expires: fields[4].parse().unwrap_or(0),
        name: fields[5].to_owned(),
        value: fields[6].to_owned(),
    })
}

/// Parse a `Set-Cookie` value, taking the domain from `default_host` when the header omits one.
fn parse_set_cookie(value: &str, default_host: Option<&str>) -> Option<Cookie> {
    let mut parts = value.split(';');
    let (name, val) = parts.next()?.trim().split_once('=')?;
    let mut cookie = Cookie {
        domain: default_host.unwrap_or_default().to_owned(),
        include_subdomains: false,
        path: "/".into(),
        secure: false,
        expires: 0,
        name: name.trim().to_owned(),
        value: val.trim().to_owned(),
    };
    for attribute in parts {
        let attribute = attribute.trim();
        let (key, val) = match attribute.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (attribute, ""),
        };
        if key.eq_ignore_ascii_case("domain") {
            cookie.domain = val.trim_start_matches('.').to_owned();
            cookie.include_subdomains = true;
        } else if key.eq_ignore_ascii_case("path") {
            cookie.path = val.to_owned();
        } else if key.eq_ignore_ascii_case("secure") {
            cookie.secure = true;
        } else if key.eq_ignore_ascii_case("max-age") {
            if let Ok(seconds) = val.parse::<u64>() {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or_default();
                cookie.expires = now + seconds;
            }
        }
    }
    (!cookie.name.is_empty()).then_some(cookie)
}

/// Split `url` into whether it is secure, its host and its path.
fn split_url(url: &str) -> Option<(bool, String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let secure = scheme.eq_ignore_ascii_case("https");
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = authority.split(':').next().unwrap_or(authority);
    Some((secure, host.to_ascii_lowercase(), path.to_owned()))
}

/// Domain matching as the Netscape format defines it: exact, or any subdomain when the cookie says so.
fn domain_matches(cookie_domain: &str, include_subdomains: bool, host: &str) -> bool {
    let cookie_domain = cookie_domain.to_ascii_lowercase();
    if cookie_domain == host {
        return true;
    }
    include_subdomains && host.ends_with(&format!(".{cookie_domain}"))
}

/// Path matching: the cookie path must be the request path or a prefix of it on a `/` boundary.
fn path_matches(cookie_path: &str, path: &str) -> bool {
    if cookie_path == "/" || cookie_path == path {
        return true;
    }
    let Some(rest) = path.strip_prefix(cookie_path) else {
        return false;
    };
    rest.starts_with('/') || cookie_path.ends_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netscape_lines_are_read_and_selected_by_domain_path_and_secure() {
        let dir = std::env::temp_dir().join(format!("gix-cookies-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cookies.txt");
        std::fs::write(
            &file,
            "# Netscape HTTP Cookie File\n\
             example.com\tFALSE\t/\tFALSE\t0\tplain\t1\n\
             .example.com\tTRUE\t/repo\tFALSE\t0\tscoped\t2\n\
             example.com\tFALSE\t/\tTRUE\t0\tsecureonly\t3\n\
             other.com\tFALSE\t/\tFALSE\t0\telsewhere\t4\n",
        )
        .unwrap();
        let jar = Jar::from_file(&file);

        assert_eq!(jar.header_for("http://example.com/x").as_deref(), Some("plain=1"));
        assert_eq!(
            jar.header_for("http://example.com/repo/info/refs").as_deref(),
            Some("plain=1; scoped=2")
        );
        assert_eq!(
            jar.header_for("https://sub.example.com/repo").as_deref(),
            Some("scoped=2"),
            "only the subdomain-enabled cookie crosses a label, and the secure one needs a matching host"
        );
        assert_eq!(
            jar.header_for("https://example.com/x").as_deref(),
            Some("plain=1; secureonly=3")
        );
        assert_eq!(jar.header_for("http://unrelated.test/x"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn set_cookie_headers_are_absorbed_and_written_back() {
        let mut jar = Jar::default();
        jar.absorb(
            "https://example.com/repo.git/info/refs",
            ["session=abc; Path=/; Secure", "tracking=xyz; Domain=.example.com"],
        );
        assert_eq!(
            jar.header_for("https://sub.example.com/anything").as_deref(),
            Some("tracking=xyz"),
            "the domain attribute widens the cookie to subdomains while the host-only one stays put"
        );

        let dir = std::env::temp_dir().join(format!("gix-cookies-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("jar.txt");
        jar.write_to(&file).unwrap();
        let reread = Jar::from_file(&file);
        assert_eq!(
            reread.header_for("https://example.com/repo.git").as_deref(),
            Some("session=abc; tracking=xyz"),
            "a written jar reads back to the same cookies"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expired_cookies_are_not_sent() {
        let mut jar = Jar::default();
        jar.insert(Cookie {
            domain: "example.com".into(),
            include_subdomains: false,
            path: "/".into(),
            secure: false,
            expires: 1,
            name: "stale".into(),
            value: "1".into(),
        });
        assert_eq!(jar.header_for("http://example.com/"), None);
    }
}
