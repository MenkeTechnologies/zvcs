use anyhow::Result;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// `git credential-store` — the plaintext-file credential helper.
///
/// A full port of upstream's `builtin/credential-store.c` plus the pieces of
/// `credential.c` / `url.c` it leans on. Nothing here touches a repository:
/// stock `git credential-store` never opens one either (it runs fine outside a
/// worktree), so there is no gitoxide substrate to route through. The vendored
/// `gix-credentials` crate models the same protocol but with UTF-8 `String`
/// fields and a stricter parser, which would silently diverge on the byte-exact
/// cases below; the parse is therefore done here over raw bytes, as upstream
/// does.
///
/// Implemented, byte-identical to stock git:
///   * `--file=<path>` / `--file <path>` (and the `--no-file` reset, plus
///     `parse_options`-style unambiguous long-option abbreviation).
///   * `get` — first matching entry wins across the file list; prints
///     `username=<u>\n` / `password=<p>\n` and nothing else.
///   * `store` — rewrites the first existing file (creating the first candidate
///     when none exist) with the new entry on line one followed by every
///     non-matching line preserved verbatim.
///   * `erase` — drops matching entries from *every* existing file, matching on
///     the password too, so a stale password never erases a fresh entry.
///   * The default file list `~/.git-credentials` then
///     `$XDG_CONFIG_HOME/git/credentials` (falling back to `~/.config/git`).
///   * The on-disk URL format: percent-encoding with git's RFC-3986 tables
///     (unreserved for user/password/host, reserved-or-unreserved for the path)
///     and lowercase hex, and the matching decoder including its passthrough of
///     malformed `%` escapes.
///   * `credential_match` semantics: an absent field in the request matches
///     anything, an absent field in the entry matches only an absent request.
///   * The stdin protocol: `key=value` until a blank line or EOF, one trailing
///     CR stripped, unknown keys ignored, `url=` clearing all prior fields.
///   * Exit codes and stderr text: usage on stdout for `-h` and on stderr for a
///     bad/absent action (both 129), `fatal: unable to read credential` (128),
///     `fatal: credential url cannot be parsed: <url>` (128), and 0 for every
///     successful operation including "no match" and an unrecognized action.
///
/// Not implemented: the `credentialstore.locktimeoutms` config knob — the lock
/// timeout is upstream's 1000 ms default, since reading it would mean opening a
/// repository this command otherwise never needs.
pub fn credential_store(args: &[String]) -> Result<ExitCode> {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(UsageError::Help) => {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        Err(UsageError::Bad(msg)) => {
            if let Some(msg) = msg {
                eprintln!("{msg}");
            }
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
    };

    let files = match parsed.file {
        Some(f) => vec![PathBuf::from(f)],
        None => default_files(),
    };
    if files.is_empty() {
        eprintln!("fatal: unable to set up default path; use --file");
        return Ok(ExitCode::from(128));
    }

    // Upstream reads the credential before dispatching, so input errors win
    // over an unknown action.
    let want = match read_credential()? {
        Ok(c) => c,
        Err(fatal) => return Ok(fatal),
    };

    match parsed.op.as_str() {
        "get" => lookup_credential(&files, &want)?,
        "store" => store_credential(&files, &want)?,
        "erase" => remove_credential(&files, &want)?,
        // Upstream ignores an unrecognized operation outright and returns 0.
        _ => {}
    }

    Ok(ExitCode::SUCCESS)
}

const USAGE: &str = "usage: git credential-store [<options>] <action>\n\n    --[no-]file <path>    fetch and store credentials in <path>\n\n";

struct Parsed {
    file: Option<String>,
    op: String,
}

enum UsageError {
    /// `-h`: usage goes to stdout.
    Help,
    /// Usage goes to stderr, optionally preceded by an `error:` line.
    Bad(Option<String>),
}

/// Mirror `parse_options` for this command's single option, including argument
/// permutation (options may follow the action) and long-option abbreviation.
fn parse_args(args: &[String]) -> std::result::Result<Parsed, UsageError> {
    let mut file: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut no_more_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if no_more_opts || a == "-" || !a.starts_with('-') {
            positional.push(a.to_owned());
            i += 1;
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            i += 1;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            if is_abbrev(name, "no-file") {
                file = None;
            } else if is_abbrev(name, "file") {
                match inline {
                    Some(v) => file = Some(v.to_owned()),
                    None => {
                        i += 1;
                        match args.get(i) {
                            Some(v) => file = Some(v.clone()),
                            None => {
                                return Err(UsageError::Bad(Some(
                                    "error: option `file' requires a value".to_owned(),
                                )))
                            }
                        }
                    }
                }
            } else {
                return Err(UsageError::Bad(Some(format!(
                    "error: unknown option `{long}'"
                ))));
            }
            i += 1;
            continue;
        }
        // Short switches; only `-h` is known.
        for c in a[1..].chars() {
            if c == 'h' {
                return Err(UsageError::Help);
            }
            return Err(UsageError::Bad(Some(format!("error: unknown switch `{c}'"))));
        }
        i += 1;
    }

    if positional.len() != 1 {
        return Err(UsageError::Bad(None));
    }
    Ok(Parsed {
        file,
        op: positional.remove(0),
    })
}

/// Whether `given` is a non-empty prefix of `full`, as `parse_options` allows.
fn is_abbrev(given: &str, full: &str) -> bool {
    !given.is_empty() && full.starts_with(given)
}

/// `~/.git-credentials`, then `$XDG_CONFIG_HOME/git/credentials` (or
/// `$HOME/.config/git/credentials`). Candidates whose base directory cannot be
/// determined are dropped, exactly as upstream's NULL returns are.
fn default_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty());
    if let Some(home) = &home {
        out.push(Path::new(home).join(".git-credentials"));
    }
    match std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        Some(xdg) => out.push(Path::new(&xdg).join("git").join("credentials")),
        None => {
            if let Some(home) = &home {
                out.push(Path::new(home).join(".config").join("git").join("credentials"));
            }
        }
    }
    out
}

/// A credential description. Every field is optional and byte-valued; the
/// present-but-empty case is meaningful (upstream distinguishes a NULL field
/// from an empty string, and matching depends on it).
#[derive(Default)]
struct Credential {
    protocol: Option<Vec<u8>>,
    host: Option<Vec<u8>>,
    path: Option<Vec<u8>>,
    username: Option<Vec<u8>>,
    password: Option<Vec<u8>>,
}

/// Read the `key=value` block from stdin, stopping at the first blank line or
/// EOF. The outer `Result` is I/O; the inner `Err` carries the exit code of a
/// fatal protocol error whose message has already been reported.
fn read_credential() -> Result<std::result::Result<Credential, ExitCode>> {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;

    let mut c = Credential::default();
    for raw in lines_lf(&input) {
        // `strbuf_getline` drops a single trailing CR, so CRLF input works.
        let line = match raw.split_last() {
            Some((b'\r', rest)) => rest,
            _ => raw,
        };
        if line.is_empty() {
            break;
        }
        let Some(eq) = line.iter().position(|&b| b == b'=') else {
            let mut err = std::io::stderr();
            err.write_all(b"warning: invalid credential line: ")?;
            err.write_all(line)?;
            err.write_all(b"\n")?;
            eprintln!("fatal: unable to read credential");
            return Ok(Err(ExitCode::from(128)));
        };
        let (key, value) = (&line[..eq], &line[eq + 1..]);
        match key {
            b"protocol" => c.protocol = Some(value.to_vec()),
            b"host" => c.host = Some(value.to_vec()),
            b"path" => c.path = Some(value.to_vec()),
            b"username" => c.username = Some(value.to_vec()),
            b"password" => c.password = Some(value.to_vec()),
            // `url=` replaces the whole credential, discarding earlier keys.
            b"url" => match credential_from_url(value) {
                Some(from_url) => c = from_url,
                None => {
                    let mut err = std::io::stderr();
                    err.write_all(b"warning: url has no scheme: ")?;
                    err.write_all(value)?;
                    err.write_all(b"\nfatal: credential url cannot be parsed: ")?;
                    err.write_all(value)?;
                    err.write_all(b"\n")?;
                    return Ok(Err(ExitCode::from(128)));
                }
            },
            // Unrecognized keys are ignored, per the credential protocol.
            _ => {}
        }
    }
    Ok(Ok(c))
}

/// Split on LF, keeping a final unterminated line and dropping the empty tail
/// a trailing LF would otherwise produce — the behaviour of repeated
/// `strbuf_getline_lf` calls until EOF.
fn lines_lf(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            out.push(&data[start..i]);
            start = i + 1;
        }
    }
    if start < data.len() {
        out.push(&data[start..]);
    }
    out
}

/// Port of `credential_from_url_1` with `allow_partial_url = 0`: parse
/// `proto://[user[:pass]@]host[/path]`. Returns `None` when there is no scheme,
/// which callers treat as "not a credential line".
fn credential_from_url(url: &[u8]) -> Option<Credential> {
    let proto_end = find(url, b"://")?;
    if proto_end == 0 {
        return None;
    }
    let cp = proto_end + 3;

    let at = find(&url[cp..], b"@").map(|i| i + cp);
    let colon = find(&url[cp..], b":").map(|i| i + cp);
    // A query or fragment marker before the slash also ends the host portion.
    let slash = cp + url[cp..]
        .iter()
        .position(|b| matches!(b, b'/' | b'?' | b'#'))
        .unwrap_or(url.len() - cp);

    let mut c = Credential::default();
    let host_start = match at {
        // Case (1): no userinfo at all.
        None => cp,
        Some(at) if slash <= at => cp,
        // Case (2): user only.
        Some(at) if colon.is_none_or(|colon| at <= colon) => {
            c.username = Some(url_decode(&url[cp..at]));
            at + 1
        }
        // Case (3): user and password.
        Some(at) => {
            let colon = colon.expect("guarded by the arm above");
            c.username = Some(url_decode(&url[cp..colon]));
            c.password = Some(url_decode(&url[colon + 1..at]));
            at + 1
        }
    };

    c.protocol = Some(url[..proto_end].to_vec());
    c.host = Some(url_decode(&url[host_start..slash]));

    // Trim leading slashes, then trailing ones off the decoded path.
    let rest = &url[slash..];
    let rest = &rest[rest.iter().take_while(|&&b| b == b'/').count()..];
    if !rest.is_empty() {
        let mut path = url_decode(rest);
        while path.len() > 1 && path.last() == Some(&b'/') {
            path.pop();
        }
        c.path = Some(path);
    }
    Some(c)
}

/// Render a credential back into the on-disk URL form, as `store_credential_file`
/// builds it.
fn credential_to_url(c: &Credential) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(c.protocol.as_deref().unwrap_or_default());
    out.extend_from_slice(b"://");
    urlencode(&mut out, c.username.as_deref().unwrap_or_default(), false);
    out.push(b':');
    urlencode(&mut out, c.password.as_deref().unwrap_or_default(), false);
    out.push(b'@');
    if let Some(host) = &c.host {
        urlencode(&mut out, host, false);
    }
    if let Some(path) = &c.path {
        out.push(b'/');
        urlencode(&mut out, path, true);
    }
    out
}

/// Percent-encode with git's RFC-3986 tables and lowercase hex. `keep_reserved`
/// selects `is_rfc3986_reserved_or_unreserved` over `is_rfc3986_unreserved`.
fn urlencode(out: &mut Vec<u8>, src: &[u8], keep_reserved: bool) {
    const RESERVED: &[u8] = b"!*'();:@&=+$,/?#[]";
    for &b in src {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if unreserved || (keep_reserved && RESERVED.contains(&b)) {
            out.push(b);
        } else {
            out.extend_from_slice(format!("%{b:02x}").as_bytes());
        }
    }
}

/// Decode `%XX` escapes. A malformed escape is passed through verbatim, as
/// `url_decode_internal` does; `+` is *not* treated as a space here.
fn url_decode(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%' && i + 2 < src.len() {
            if let (Some(hi), Some(lo)) = (hex_val(src[i + 1]), hex_val(src[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Port of `credential_match`: an absent field in `want` matches anything; a
/// present one requires an equal, present field in `have`.
fn credential_match(want: &Credential, have: &Credential, match_password: bool) -> bool {
    fn check(want: &Option<Vec<u8>>, have: &Option<Vec<u8>>) -> bool {
        match want {
            None => true,
            Some(w) => have.as_deref() == Some(w.as_slice()),
        }
    }
    check(&want.protocol, &have.protocol)
        && check(&want.host, &have.host)
        && check(&want.path, &have.path)
        && check(&want.username, &have.username)
        && (!match_password || check(&want.password, &have.password))
}

/// Read a credentials file. A missing or unreadable file yields no lines, as
/// upstream tolerates `ENOENT`/`EACCES` and dies on anything else.
fn read_credential_file(path: &Path) -> Result<Vec<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(data) => Ok(lines_lf(&data).into_iter().map(|l| l.to_vec()).collect()),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(Vec::new())
        }
        Err(e) => anyhow::bail!("unable to open {}: {e}", path.display()),
    }
}

/// A stored line is a credential candidate only when it parses as a URL and
/// carries both a username and a password.
fn entry_of(line: &[u8]) -> Option<Credential> {
    let c = credential_from_url(line)?;
    (c.username.is_some() && c.password.is_some()).then_some(c)
}

/// `get`: print the first matching entry, scanning the files in order.
fn lookup_credential(files: &[PathBuf], want: &Credential) -> Result<()> {
    for file in files {
        for line in read_credential_file(file)? {
            let Some(entry) = entry_of(&line) else { continue };
            if credential_match(want, &entry, false) {
                let mut out = std::io::stdout().lock();
                out.write_all(b"username=")?;
                out.write_all(entry.username.as_deref().unwrap_or_default())?;
                out.write_all(b"\npassword=")?;
                out.write_all(entry.password.as_deref().unwrap_or_default())?;
                out.write_all(b"\n")?;
                out.flush()?;
                return Ok(());
            }
        }
    }
    Ok(())
}

/// `store`: write to the first existing file, or create the first candidate.
fn store_credential(files: &[PathBuf], c: &Credential) -> Result<()> {
    // Refuse to store anything that could not form a usable primary key. An
    // empty-but-present field still counts as present, as it does upstream.
    if c.protocol.is_none()
        || (c.host.is_none() && c.path.is_none())
        || c.username.is_none()
        || c.password.is_none()
    {
        return Ok(());
    }

    let extra = credential_to_url(c);
    let target = files.iter().find(|f| f.exists()).unwrap_or(&files[0]);
    rewrite_credential_file(target, c, Some(&extra), false)
}

/// `erase`: drop matching entries from every existing file. The password is part
/// of the match, so an outdated password never erases a current entry.
fn remove_credential(files: &[PathBuf], c: &Credential) -> Result<()> {
    for file in files {
        if file.exists() {
            rewrite_credential_file(file, c, None, true)?;
        }
    }
    Ok(())
}

/// Rewrite `path` under a `.lock` file: `extra` first (when storing), then every
/// existing line that does not match `c`, each LF-terminated.
fn rewrite_credential_file(
    path: &Path,
    c: &Credential,
    extra: Option<&[u8]>,
    match_password: bool,
) -> Result<()> {
    let mut buf = Vec::new();
    if let Some(extra) = extra {
        buf.extend_from_slice(extra);
        buf.push(b'\n');
    }
    for line in read_credential_file(path)? {
        let drop = entry_of(&line).is_some_and(|e| credential_match(c, &e, match_password));
        if !drop {
            buf.extend_from_slice(&line);
            buf.push(b'\n');
        }
    }

    let lock = lock_path(path);
    let mut file = acquire_lock(&lock)?;
    let mut write = file.write_all(&buf);
    if write.is_ok() {
        write = file.sync_all();
    }
    drop(file);
    if let Err(e) = write {
        let _ = std::fs::remove_file(&lock);
        anyhow::bail!("unable to write credential store: {e}");
    }
    if let Err(e) = std::fs::rename(&lock, path) {
        let _ = std::fs::remove_file(&lock);
        anyhow::bail!("unable to write credential store: {e}");
    }
    Ok(())
}

fn lock_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// Take the `<file>.lock` exclusively, retrying for upstream's default 1000 ms.
/// The file is created 0600 — upstream gets the same via its `umask(077)`.
fn acquire_lock(lock: &Path) -> Result<std::fs::File> {
    const TIMEOUT_MS: u64 = 1000;
    let mut waited = 0;
    loop {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(lock) {
            Ok(f) => return Ok(f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && waited < TIMEOUT_MS => {
                std::thread::sleep(std::time::Duration::from_millis(10));
                waited += 10;
            }
            Err(e) => anyhow::bail!("unable to get credential storage lock in {TIMEOUT_MS} ms: {e}"),
        }
    }
}
