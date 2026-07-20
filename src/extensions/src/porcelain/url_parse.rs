//! `git url-parse` — parse a Git URL and extract one of its components.
//!
//! Stock git composes this builtin from three C pieces:
//!
//! * `connect.c::url_is_local_not_ssh` — deciding whether an argument is a
//!   local path, a real `scheme://` URL, or the scp-like `[user@]host:path`
//!   shorthand that gets rewritten as `ssh://`.
//! * `url.c::url_decode_mem` / `url_decode_internal` — the percent-decode pass
//!   stock git applies to `scheme://` URLs (and only to those) before
//!   normalizing, including its refusal to decode `%00`.
//! * `urlmatch.c::url_normalize_1` and `append_normalized_escapes` — the
//!   normalizer that produces the component offsets this command prints.
//!
//! The decoder and the normalizer are transcribed from the C, including the
//! escape tables and every error string, because the differential harness
//! compares bytes: the normalizer unescapes `%41` to `A` but keeps `%20`,
//! lower-cases scheme and host but not user or password, drops `:80` for
//! `http:` and `:443` for `https:` only, resolves `.`/`..` segments, and
//! truncates the path at `?` or `#`.
//!
//! The scp-to-`ssh://` rewrite is the one part reconstructed from observed
//! behaviour instead: `git url-parse` is newer than the `connect.c` code it
//! resembles and does not agree with it on where the host ends. Every rule
//! encoded in `scp_to_ssh` was pinned against stock git and is covered by a
//! test below.
//!
//! ### Covered (byte-identical stdout/stderr and exit code against stock git)
//!
//! * `git url-parse [-c <component>] [--] <url>...` for the six components git
//!   defines: `scheme`, `user`, `password`, `host`, `port`, `path`
//! * `-c`, `-c<value>`, `--component <value>`, `--component=<value>`,
//!   `--no-component`, and unambiguous long-option abbreviations
//! * no `--component`: pure validation, no output, exit 0 / 128
//! * `scheme://` URLs, the scp shorthand, IPv6 literals in both forms, and the
//!   `ssh`/`git`/`git+ssh`/`ssh+git` rule that turns a `/~user` path into
//!   `~user` (matched against the *undecased* scheme, as `get_protocol` does)
//! * every `fatal:` message git can emit here, each with exit 128; the usage
//!   block and the `error: unknown switch`/`requires a value` forms, exit 129
//!
//! ### Not covered
//!
//! * `has_dos_drive_prefix` is a no-op on POSIX in stock git as well, so
//!   `C:/x` is parsed as the scp host `c` here exactly as it is on macOS and
//!   Linux. A Windows build of git would treat it as a local path.

use anyhow::Result;
use std::io::Write;
use std::process::ExitCode;

/// Stock git's usage block, byte-for-byte. Stdout on `-h`, stderr on any
/// argument error; both exit 129.
const USAGE: &str = "usage: git url-parse [-c <component>] [--] <url>...\n\n    \
                     -c, --[no-]component <component>\n                          \
                     which URL component to extract\n\n";

/// `URL_SCHEME_CHARS` — alphanumerics plus `+.-`.
const SCHEME_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+.-";
/// `URL_HOST_CHARS` — alphanumerics plus `.-_[:]` (IPv6 literals need `[:]`).
const HOST_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789.-_[:]";
/// `URL_UNSAFE_CHARS`; the `0x00-0x1F` / `0x7F-0xFF` halves are range checks.
const UNSAFE_CHARS: &[u8] = b" <>\"%{}|\\^`";
/// `URL_RESERVED` = `URL_GEN_RESERVED URL_SUB_RESERVED` — the delimiters that
/// stay escaped when they arrived escaped.
const RESERVED: &[u8] = b":/?#[]@!$&'()*+,;=";

/// The schemes for which git strips the leading `/` of a `/~user` path
/// (`connect.c::get_protocol` mapped to `PROTO_SSH` or `PROTO_GIT`).
const TILDE_SCHEMES: [&str; 4] = ["ssh", "git", "git+ssh", "ssh+git"];

/// The component selected by `--component`, in git's spelling.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Component {
    Scheme,
    User,
    Password,
    Host,
    Port,
    Path,
}

impl Component {
    /// Exact, case-sensitive match against git's six names.
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "scheme" => Component::Scheme,
            "user" => Component::User,
            "password" => Component::Password,
            "host" => Component::Host,
            "port" => Component::Port,
            "path" => Component::Path,
            _ => return None,
        })
    }
}

/// `struct url_info`: the normalized URL plus the offsets into it. An offset of
/// zero means "component absent", as in the C struct; a present-but-empty
/// component has a non-zero offset and a zero length.
struct UrlInfo {
    url: Vec<u8>,
    scheme_len: usize,
    user_off: usize,
    user_len: usize,
    passwd_off: usize,
    passwd_len: usize,
    host_off: usize,
    host_len: usize,
    port_off: usize,
    port_len: usize,
    path_off: usize,
    path_len: usize,
}

/// `git url-parse` — parse Git URLs and optionally print one component each.
///
/// URLs are processed left to right; the first one that fails to parse prints
/// git's `fatal:` line and stops with exit 128, keeping any output already
/// produced for the URLs before it.
pub fn url_parse(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the argument list without the subcommand; tolerate a
    // leading `url-parse` so either calling convention is correct.
    let argv: &[String] = match args.first() {
        Some(first) if first == "url-parse" => &args[1..],
        _ => args,
    };

    let mut component: Option<String> = None;
    let mut urls: Vec<&String> = Vec::new();
    let mut i = 0;
    let mut no_more_opts = false;

    while i < argv.len() {
        let arg = argv[i].as_str();
        if no_more_opts || arg == "-" || !arg.starts_with('-') {
            urls.push(&argv[i]);
            i += 1;
            continue;
        }
        if arg == "--" {
            no_more_opts = true;
            i += 1;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            if is_abbrev(name, "no-component") {
                component = None;
            } else if is_abbrev(name, "component") {
                match inline {
                    Some(v) => component = Some(v.to_string()),
                    None => {
                        i += 1;
                        match argv.get(i) {
                            Some(v) => component = Some(v.clone()),
                            None => return Ok(missing_value("option `component'")),
                        }
                    }
                }
            } else {
                return Ok(unknown_opt("option", name));
            }
            i += 1;
            continue;
        }

        // Short options, possibly clustered (`-c` swallows the rest as its value).
        let bytes = arg.as_bytes();
        let mut j = 1;
        while j < bytes.len() {
            if bytes[j] == b'h' {
                // parse-options answers `-h` wherever it appears, even after a
                // URL, with the usage block on stdout and exit 129.
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            if bytes[j] != b'c' {
                return Ok(unknown_opt("switch", &(bytes[j] as char).to_string()));
            }
            if j + 1 < bytes.len() {
                component = Some(arg[j + 1..].to_string());
                j = bytes.len();
            } else {
                i += 1;
                match argv.get(i) {
                    Some(v) => component = Some(v.clone()),
                    None => return Ok(missing_value("switch `c'")),
                }
                j += 1;
            }
        }
        i += 1;
    }

    if urls.is_empty() {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // The component name is validated once, before any URL is looked at.
    let selected = match &component {
        Some(name) => match Component::parse(name) {
            Some(c) => Some(c),
            None => {
                eprintln!("fatal: invalid git URL component '{name}'");
                return Ok(ExitCode::from(128));
            }
        },
        None => None,
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for url in urls {
        let info = match parse_git_url(url) {
            Ok(info) => info,
            Err(msg) => {
                out.flush()?;
                eprintln!("{msg}");
                return Ok(ExitCode::from(128));
            }
        };
        if let Some(c) = selected {
            out.write_all(extract(&info, c, url))?;
            out.write_all(b"\n")?;
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// `error: unknown switch \`x'` / `error: unknown option \`name'`, then usage.
fn unknown_opt(kind: &str, name: &str) -> ExitCode {
    eprintln!("error: unknown {kind} `{name}'");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// `error: switch \`c' requires a value` — git prints no usage block for this.
fn missing_value(what: &str) -> ExitCode {
    eprintln!("error: {what} requires a value");
    ExitCode::from(129)
}

/// parse-options accepts any unambiguous prefix of a long option name. `c` is
/// the only option here, so a non-empty prefix of `full` is unambiguous unless
/// it is also a prefix of the other spelling — which cannot happen, since
/// `component` and `no-component` differ at the first byte.
fn is_abbrev(given: &str, full: &str) -> bool {
    !given.is_empty() && full.starts_with(given)
}

/// The bytes of one component, or an empty slice when it is absent.
///
/// `path` carries the one adjustment stock git makes after normalizing: for
/// `ssh`/`git` URLs a leading `/` before `~` is dropped, so `ssh://h/~u/r`
/// yields `~u/r`. The scheme is compared against the *original* argument, not
/// the lower-cased normalized one, because `get_protocol` uses `strcmp`.
fn extract<'a>(info: &'a UrlInfo, component: Component, original: &str) -> &'a [u8] {
    let (off, len) = match component {
        Component::Scheme => (0, info.scheme_len),
        Component::User => (info.user_off, info.user_len),
        Component::Password => (info.passwd_off, info.passwd_len),
        Component::Host => (info.host_off, info.host_len),
        Component::Port => (info.port_off, info.port_len),
        Component::Path => {
            let mut off = info.path_off;
            let mut len = info.path_len;
            if tilde_scheme(original) && len >= 2 && info.url[off + 1] == b'~' {
                off += 1;
                len -= 1;
            }
            (off, len)
        }
    };
    if off == 0 && component != Component::Scheme {
        return b"";
    }
    &info.url[off..off + len]
}

/// Whether the original argument names a protocol whose paths get the `/~`
/// treatment. The scp shorthand is always `ssh`; a `scheme://` URL contributes
/// its scheme verbatim, case-sensitively.
fn tilde_scheme(original: &str) -> bool {
    match original.find("://") {
        Some(pos) => TILDE_SCHEMES.contains(&&original[..pos]),
        None => true,
    }
}

/// Turn one argument into a normalized `struct url_info`, or into the exact
/// `fatal:` line stock git would print for it.
fn parse_git_url(original: &str) -> Result<UrlInfo, String> {
    let raw = original.as_bytes();

    if url_is_local_not_ssh(raw) {
        return Err(if raw.first() == Some(&b'/') {
            format!(
                "fatal: '{original}' is not a URL; \
                 if you meant a local repository, use 'file://{original}'"
            )
        } else {
            format!(
                "fatal: '{original}' is not a URL; if you meant a local repository, \
                 use a 'file://' URL with an absolute path"
            )
        });
    }

    let input = if find(raw, b"://").is_some() {
        // A real URL: git percent-decodes it before normalizing.
        url_decode(raw)
    } else {
        scp_to_ssh(raw)
    };

    url_normalize(&input).map_err(|err| format!("fatal: invalid git URL '{original}': {err}"))
}

/// `connect.c::url_is_local_not_ssh`. The DOS-drive clause is omitted because
/// `has_dos_drive_prefix` is a constant 0 on POSIX, which is what stock git
/// does on macOS and Linux too.
fn url_is_local_not_ssh(url: &[u8]) -> bool {
    let colon = url.iter().position(|&c| c == b':');
    let slash = url.iter().position(|&c| c == b'/');
    match (colon, slash) {
        (None, _) => true,
        (Some(c), Some(s)) => s < c,
        (Some(_), None) => false,
    }
}

/// Rewrite the scp shorthand `[user[:pass]@]host[:path]` as the `ssh://` URL
/// stock git hands to the normalizer.
///
/// The host/path separator is the first `:` at or after the first `@` (git
/// looks for the userinfo before the host, so a colon on the *left* of an `@`
/// belongs to the password, not the path), skipping over a bracketed IPv6
/// literal that starts there. An unterminated `[` makes the search restart at
/// the beginning, as `host_end` returning the string start does.
///
/// A bracketed host is treated as an IPv6 literal only when its content holds
/// two or more colons. If it does, the brackets survive and the separator is
/// the first `:` after the `]`, so `[::1]F:x` keeps host `[::1]f`. Otherwise
/// the brackets are unwrapped the way `host_end(.., 1)` unwraps them — which
/// also drops whatever sat between the `]` and the separator, the separator
/// being the byte right after the `]` — so `[x:1]:b` becomes host `x` with port
/// `1`, `[x:y]:b` fails with `invalid port number`, and `[12x]F:x` keeps only
/// `12x` with the path `/:x`.
fn scp_to_ssh(url: &[u8]) -> Vec<u8> {
    let start = match url.iter().position(|&c| c == b'@') {
        Some(i) => i + 1,
        None => 0,
    };
    let bracket = if url.get(start) == Some(&b'[') {
        find_from(url, start + 1, b']').map(|close| (start, close))
    } else {
        None
    };

    let (sep, authority): (Option<usize>, Vec<u8>) = match bracket {
        Some((open, close)) if url[open + 1..close].iter().filter(|&&b| b == b':').count() >= 2 => {
            let sep = find_from(url, close, b':');
            (sep, url[start..sep.unwrap_or(url.len())].to_vec())
        }
        Some((open, close)) => {
            let sep = if close + 1 < url.len() { Some(close + 1) } else { None };
            (sep, url[open + 1..close].to_vec())
        }
        None => {
            let sep = find_from(url, start, b':');
            (sep, url[start..sep.unwrap_or(url.len())].to_vec())
        }
    };

    let mut out = b"ssh://".to_vec();
    out.extend_from_slice(&url[..start]);
    out.extend_from_slice(&authority);
    if let Some(s) = sep {
        // The normalizer supplies the leading `/` itself, so an absolute
        // scp path must not end up doubled.
        let path = &url[s + 1..];
        out.push(b'/');
        out.extend_from_slice(path.strip_prefix(b"/").unwrap_or(path));
    }
    out
}

/// `url.c::url_decode_mem`: copy the scheme (everything before the first `:`)
/// verbatim, then percent-decode the rest. `%00` is left alone because
/// `url_decode_internal` only accepts a decoded value strictly greater than 0.
fn url_decode(url: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(url.len());
    let mut i = 0;
    if let Some(colon) = url.iter().position(|&c| c == b':') {
        if colon > 0 {
            out.extend_from_slice(&url[..colon]);
            i = colon;
        }
    }
    while i < url.len() {
        let c = url[i];
        if c == 0 {
            break;
        }
        if c == b'%' && url.len() - i >= 3 {
            if let Some(v) = hex2chr(&url[i + 1..i + 3]) {
                if v > 0 {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `urlmatch.c::url_normalize_1` with `allow_globs == 0`.
///
/// Returns the normalized URL and its component offsets, or the brief error
/// string git stores in `url_info.err`.
fn url_normalize(url: &[u8]) -> Result<UrlInfo, &'static str> {
    let mut norm: Vec<u8> = Vec::with_capacity(url.len());

    // Scheme, lower-cased, followed by a literal `://`; no %-escapes allowed
    // and the first character must be alphabetic.
    let spanned = strspn(url, SCHEME_CHARS);
    if spanned == 0
        || !url[0].is_ascii_alphabetic()
        || spanned + 3 > url.len()
        || url[spanned] != b':'
        || url[spanned + 1] != b'/'
        || url[spanned + 2] != b'/'
    {
        return Err("invalid URL scheme name or missing '://' suffix");
    }
    let scheme_len = spanned;
    for &b in &url[..spanned + 3] {
        norm.push(b.to_ascii_lowercase());
    }
    let mut pos = spanned + 3;

    // Any `user[:password]@`, with %-escapes normalized. The password is found
    // by looking for a `:` in the *normalized* userinfo, so an escaped `%3A`
    // does not split it.
    let mut user_off = 0;
    let mut user_len = 0;
    let mut passwd_off = 0;
    let mut passwd_len = 0;
    let slash_ptr = pos + strcspn(&url[pos..], b"/?#");
    if let Some(rel) = url[pos..].iter().position(|&c| c == b'@') {
        let at = pos + rel;
        if at < slash_ptr {
            user_off = norm.len();
            if at > pos {
                if !append_normalized_escapes(&mut norm, &url[pos..at], b"", RESERVED) {
                    return Err("invalid %XX escape sequence");
                }
                let base = scheme_len + 3;
                match norm[base..].iter().position(|&c| c == b':') {
                    Some(ci) => {
                        let colon = base + ci;
                        passwd_off = colon + 1;
                        passwd_len = norm.len() - passwd_off;
                        user_len = (passwd_off - 1) - base;
                    }
                    None => user_len = norm.len() - base,
                }
            }
            norm.push(b'@');
            pos = at + 1;
        }
    }

    // Host, lower-cased, no %-escapes allowed. Only `file:` may omit it.
    let mut host_off = 0;
    let mut host_len = 0;
    if pos >= url.len() || matches!(url[pos], b':' | b'/' | b'?' | b'#') {
        if !norm.starts_with(b"file:") {
            return Err("missing host and scheme is not 'file:'");
        }
    } else {
        host_off = norm.len();
    }

    // Walk back from the path to the port colon, stopping at `]` so IPv6
    // literals are not scanned into.
    let mut cp = slash_ptr - 1;
    while cp > pos && url[cp] != b':' && url[cp] != b']' {
        cp -= 1;
    }
    let colon_ptr = if url[cp] != b':' {
        slash_ptr
    } else {
        if host_off == 0 && cp < slash_ptr && cp + 1 != slash_ptr {
            return Err("a 'file:' URL may not have a port number");
        }
        cp
    };

    if strspn(&url[pos..], HOST_CHARS) < colon_ptr - pos {
        return Err("invalid characters in host name");
    }
    while pos < colon_ptr {
        norm.push(url[pos].to_ascii_lowercase());
        pos += 1;
    }

    // Port: leading zeros dropped, `:80` on `http:` and `:443` on `https:`
    // dropped entirely, anything else must be a number in 1..=65535.
    let mut port_off = 0;
    let mut port_len = 0;
    if colon_ptr < slash_ptr {
        pos += 1; // the ':'
        pos += strspn(&url[pos..], b"0");
        if pos == slash_ptr && url[pos - 1] == b'0' {
            pos -= 1;
        }
        let width = slash_ptr - pos;
        if pos == slash_ptr {
            // `:` with no number is the same as the default.
        } else if width == 2 && norm.starts_with(b"http:") && &url[pos..pos + 2] == b"80" {
        } else if width == 3 && norm.starts_with(b"https:") && &url[pos..pos + 3] == b"443" {
        } else {
            if strspn(&url[pos..], b"0123456789") < width {
                return Err("invalid port number");
            }
            // Anything wider than 5 digits cannot be in range; git leaves
            // `pnum` at 0 in that case and falls into the same error.
            let pnum: u64 = if width <= 5 {
                std::str::from_utf8(&url[pos..slash_ptr])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0)
            } else {
                0
            };
            if pnum == 0 || pnum > 65535 {
                return Err("invalid port number");
            }
            norm.push(b':');
            port_off = norm.len();
            norm.extend_from_slice(&url[pos..slash_ptr]);
            port_len = width;
        }
        pos = slash_ptr;
    }
    if host_off != 0 {
        host_len = norm.len() - host_off - if port_len != 0 { port_len + 1 } else { 0 };
    }

    // Path: always starts with `/`, `.` and `..` segments resolved, %-escapes
    // normalized without unescaping delimiters.
    let path_off = norm.len();
    norm.push(b'/');
    if url.get(pos) == Some(&b'/') {
        pos += 1;
    }
    loop {
        let seg_start_off = norm.len();
        let next_slash = pos + strcspn(&url[pos..], b"/?#");
        if !append_normalized_escapes(&mut norm, &url[pos..next_slash], b"", RESERVED) {
            return Err("invalid %XX escape sequence");
        }

        let mut skip_add_slash = false;
        let seg = &norm[seg_start_off..];
        if seg == b"." {
            // Drop the segment, but never the path's initial '/'.
            if seg_start_off == path_off + 1 {
                norm.truncate(norm.len() - 1);
                skip_add_slash = true;
            } else {
                norm.truncate(norm.len() - 2);
            }
        } else if seg == b".." {
            // Drop this segment and the previous one, again keeping the
            // initial '/'; with no previous segment the URL is invalid.
            let mut prev = norm.len() - 3;
            if prev == path_off {
                return Err("invalid '..' path segment");
            }
            loop {
                prev -= 1;
                if norm[prev] == b'/' {
                    break;
                }
            }
            if prev == path_off {
                norm.truncate(prev + 1);
                skip_add_slash = true;
            } else {
                norm.truncate(prev);
            }
        }

        pos = next_slash;
        if url.get(pos) != Some(&b'/') {
            break;
        }
        pos += 1;
        if !skip_add_slash {
            norm.push(b'/');
        }
    }
    let path_len = norm.len() - path_off;

    // The trailing `?...`/`#...` is normalized too (so a bad escape there is
    // still an error) but is not part of any component.
    if pos < url.len() && !append_normalized_escapes(&mut norm, &url[pos..], b"", RESERVED) {
        return Err("invalid %XX escape sequence");
    }

    Ok(UrlInfo {
        url: norm,
        scheme_len,
        user_off,
        user_len,
        passwd_off,
        passwd_len,
        host_off,
        host_len,
        port_off,
        port_len,
        path_off,
        path_len,
    })
}

/// `urlmatch.c::append_normalized_escapes`.
///
/// Unescapes what does not need escaping, escapes what does, and upper-cases
/// every surviving `%XX`. Characters in `esc_extra` are always escaped; those
/// in `esc_ok` stay escaped when they arrived escaped but are not escaped
/// otherwise, which is how delimiters keep their meaning. Returns false on a
/// `%` that is not followed by two hex digits.
fn append_normalized_escapes(
    buf: &mut Vec<u8>,
    from: &[u8],
    esc_extra: &[u8],
    esc_ok: &[u8],
) -> bool {
    let mut i = 0;
    while i < from.len() {
        let mut ch = from[i];
        i += 1;
        let mut was_esc = false;
        if ch == b'%' {
            if from.len() - i < 2 {
                return false;
            }
            match hex2chr(&from[i..i + 2]) {
                Some(v) => ch = v,
                None => return false,
            }
            i += 2;
            was_esc = true;
        }
        if ch <= 0x1F
            || ch >= 0x7F
            || UNSAFE_CHARS.contains(&ch)
            || esc_extra.contains(&ch)
            || (was_esc && esc_ok.contains(&ch))
        {
            buf.extend_from_slice(format!("%{ch:02X}").as_bytes());
        } else {
            buf.push(ch);
        }
    }
    true
}

/// `hex2chr`: the byte spelled by two hex digits, or `None` if either is not
/// one.
fn hex2chr(s: &[u8]) -> Option<u8> {
    let hi = (s[0] as char).to_digit(16)?;
    let lo = (s[1] as char).to_digit(16)?;
    Some((hi * 16 + lo) as u8)
}

/// `strspn`: length of the leading run of bytes that are all in `accept`.
fn strspn(s: &[u8], accept: &[u8]) -> usize {
    s.iter().take_while(|b| accept.contains(b)).count()
}

/// `strcspn`: length of the leading run of bytes that are all outside `reject`.
fn strcspn(s: &[u8], reject: &[u8]) -> usize {
    s.iter().take_while(|b| !reject.contains(b)).count()
}

/// Offset of the first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Offset of the first `byte` at or after `from`.
fn find_from(haystack: &[u8], from: usize, byte: u8) -> Option<usize> {
    haystack
        .get(from..)
        .and_then(|s| s.iter().position(|&c| c == byte))
        .map(|i| from + i)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Normalize the way `parse_git_url` does and return one component as a
    /// string, so the expectations below read like the stock command's output.
    fn comp(url: &str, component: Component) -> Result<String, String> {
        let info = parse_git_url(url)?;
        Ok(String::from_utf8(extract(&info, component, url).to_vec()).unwrap())
    }

    /// The `fatal:` line git prints for a URL it refuses.
    fn err(url: &str) -> String {
        comp(url, Component::Path).unwrap_err()
    }

    #[test]
    fn scp_shorthand_matches_git() {
        // `host:path` gains a leading slash, `host:~user` does not — the two
        // cases the manual page calls out explicitly.
        assert_eq!(comp("example.com:user/repo", Component::Path).unwrap(), "/user/repo");
        assert_eq!(comp("example.com:~user/repo", Component::Path).unwrap(), "~user/repo");
        assert_eq!(comp("example.com:user/repo", Component::Scheme).unwrap(), "ssh");
        assert_eq!(comp("example.com:", Component::Path).unwrap(), "/");
        assert_eq!(comp("[::1]:path", Component::Host).unwrap(), "[::1]");
        assert_eq!(comp("[::1]:path", Component::Path).unwrap(), "/path");
    }

    #[test]
    fn escapes_are_normalized_not_merely_decoded() {
        // `%41` is unreserved so it unescapes; a space stays escaped and
        // upper-cased; `"` is unsafe so it gets escaped.
        assert_eq!(comp("https://e.com/%41", Component::Path).unwrap(), "/A");
        assert_eq!(comp("https://e.com/a b", Component::Path).unwrap(), "/a%20b");
        assert_eq!(comp("https://e.com/x\"y", Component::Path).unwrap(), "/x%22y");
        // %00 survives the pre-decode pass and is then escaped by the normalizer.
        assert_eq!(comp("https://e.com/%00x", Component::Path).unwrap(), "/%00x");
        // One decode pass only: `%252F` becomes `%2F`, which stays escaped
        // because `/` is a delimiter that arrived escaped.
        assert_eq!(comp("https://e.com/x%252Fy", Component::Path).unwrap(), "/x%2Fy");
    }

    #[test]
    fn pre_decode_happens_before_parsing_urls_but_not_shorthand() {
        // An escaped `/` in a URL splits host from path once decoded ...
        assert_eq!(comp("https://exam%2Fple.com/x", Component::Host).unwrap(), "exam");
        assert_eq!(comp("https://exam%2Fple.com/x", Component::Path).unwrap(), "/ple.com/x");
        // ... while the scp shorthand is never pre-decoded, so the escape
        // survives into the path.
        assert_eq!(comp("e.com:x%2Fy", Component::Path).unwrap(), "/x%2Fy");
        assert!(comp("e%2Ecom:x", Component::Host)
            .unwrap_err()
            .ends_with("invalid characters in host name"));
    }

    #[test]
    fn default_ports_dropped_only_for_http_and_https() {
        assert_eq!(comp("http://e.com:80/x", Component::Port).unwrap(), "");
        assert_eq!(comp("https://e.com:443/x", Component::Port).unwrap(), "");
        assert_eq!(comp("https://e.com:00080/x", Component::Port).unwrap(), "80");
        assert_eq!(comp("ssh://e.com:22/x", Component::Port).unwrap(), "22");
        assert_eq!(comp("git://e.com:9418/x", Component::Port).unwrap(), "9418");
        assert!(comp("https://e.com:0/x", Component::Port)
            .unwrap_err()
            .ends_with("invalid port number"));
    }

    #[test]
    fn dot_segments_resolve_and_underflow_is_an_error() {
        assert_eq!(comp("https://e.com/a/./b", Component::Path).unwrap(), "/a/b");
        assert_eq!(comp("https://e.com/a/b/..", Component::Path).unwrap(), "/a");
        assert!(comp("https://e.com/a/../../b", Component::Path)
            .unwrap_err()
            .ends_with("invalid '..' path segment"));
    }

    #[test]
    fn tilde_stripping_is_scheme_sensitive_and_case_sensitive() {
        assert_eq!(comp("ssh://h/~u/r", Component::Path).unwrap(), "~u/r");
        assert_eq!(comp("git+ssh://h/~u", Component::Path).unwrap(), "~u");
        assert_eq!(comp("https://h/~u", Component::Path).unwrap(), "/~u");
        assert_eq!(comp("file://h/~u", Component::Path).unwrap(), "/~u");
        // `get_protocol` compares with strcmp, so an upper-cased scheme is
        // simply not recognised as ssh.
        assert_eq!(comp("GIT+SSH://h/~u", Component::Path).unwrap(), "/~u");
        // A doubled slash is not a `/~` prefix.
        assert_eq!(comp("ssh://h//~u", Component::Path).unwrap(), "//~u");
    }

    #[test]
    fn local_paths_get_the_two_distinct_hints() {
        assert_eq!(
            err("/local/path"),
            "fatal: '/local/path' is not a URL; \
             if you meant a local repository, use 'file:///local/path'"
        );
        assert_eq!(
            err("foo/bar"),
            "fatal: 'foo/bar' is not a URL; if you meant a local repository, \
             use a 'file://' URL with an absolute path"
        );
        // A slash before the colon makes it a path, not an scp target.
        assert!(err("/a/b://c").contains("use 'file:///a/b://c'"));
    }

    #[test]
    fn scp_separator_is_the_first_colon_after_the_userinfo() {
        // Without an `@` the first colon splits host from path ...
        assert_eq!(comp("host:1234:path", Component::Path).unwrap(), "/1234:path");
        // ... but colons left of an `@` belong to the password, so the split
        // happens at the next colon instead.
        assert_eq!(comp("a:b@c:d/e", Component::Password).unwrap(), "b");
        assert_eq!(comp("a:b@c:d/e", Component::Host).unwrap(), "c");
        assert_eq!(comp("a:b@c:d/e", Component::Path).unwrap(), "/d/e");
        // With no colon after the `@`, the whole argument is the authority.
        assert_eq!(comp("host:user@x/y", Component::User).unwrap(), "host");
        assert_eq!(comp("host:user@x/y", Component::Path).unwrap(), "/y");
    }

    #[test]
    fn bracketed_scp_hosts_need_two_colons_to_stay_bracketed() {
        // Two or more colons: an IPv6 literal, brackets kept, separator is the
        // first colon after the `]`.
        assert_eq!(comp("[::1]:path", Component::Host).unwrap(), "[::1]");
        assert_eq!(comp("[::1]:22/x", Component::Path).unwrap(), "/22/x");
        assert_eq!(comp("[::1]F:x", Component::Host).unwrap(), "[::1]f");
        assert_eq!(comp("a@[::1]:p", Component::User).unwrap(), "a");
        // One colon: unwrapped and re-read as `host:port`.
        assert_eq!(comp("[x:1]:b", Component::Host).unwrap(), "x");
        assert_eq!(comp("[x:1]:b", Component::Port).unwrap(), "1");
        assert!(err("[x:y]:b").ends_with("invalid port number"));
        // Unwrapping drops whatever sat between `]` and the separator, which
        // here is simply the next byte.
        assert_eq!(comp("[12x]F:x", Component::Host).unwrap(), "12x");
        assert_eq!(comp("[12x]F:x", Component::Path).unwrap(), "/:x");
        assert!(err("[]:b").ends_with("missing host and scheme is not 'file:'"));
        // An unterminated `[` is not a bracket at all.
        assert_eq!(comp("[::1:b", Component::Host).unwrap(), "[");
        assert_eq!(comp("[::1:b", Component::Path).unwrap(), "/:1:b");
    }

    #[test]
    fn userinfo_splits_on_the_first_normalized_colon() {
        assert_eq!(comp("https://bob:pw@e.com/x", Component::User).unwrap(), "bob");
        assert_eq!(comp("https://bob:pw@e.com/x", Component::Password).unwrap(), "pw");
        assert_eq!(comp("https://u@e.com/x", Component::Password).unwrap(), "");
        // The decoded `@` re-splits the authority and pushes `@` into the host.
        assert!(comp("https://u:a%40b@e.com/x", Component::Host)
            .unwrap_err()
            .ends_with("invalid characters in host name"));
    }

    #[test]
    fn file_urls_may_omit_the_host() {
        assert_eq!(comp("file:///abs/path", Component::Host).unwrap(), "");
        assert_eq!(comp("file:///abs/path", Component::Path).unwrap(), "/abs/path");
        assert!(comp("https:///x", Component::Host)
            .unwrap_err()
            .ends_with("missing host and scheme is not 'file:'"));
    }
}
